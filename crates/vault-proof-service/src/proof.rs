use {
    crate::{
        PrivateOrderOpening, TransferRequest, VaultAeKey, VaultElGamalKeypair, VaultProofError,
        WithdrawalStatementContext,
    },
    ark_bn254::Bn254,
    ark_ed_on_bn254::Fq,
    ark_ff::{BigInteger, PrimeField},
    ark_groth16::ProvingKey,
    beatex_babyjubjub_groth16_spike::{
        claim::{PrivateWithdrawalKind, PrivateWithdrawalWitness},
        ristretto_bridge::RistrettoBridgeWitness,
    },
    curve25519_dalek::scalar::Scalar,
    rand::{CryptoRng, RngCore},
    solana_zk_sdk_v4::{
        encryption::{
            auth_encryption::AeCiphertext,
            elgamal::{ElGamalCiphertext, ElGamalPubkey},
            grouped_elgamal::{GroupedElGamal, GroupedElGamalCiphertext},
            pedersen::{Pedersen, PedersenOpening},
            pod::pedersen::PodPedersenCommitment,
        },
        zk_elgamal_proof_program::proof_data::{
            batched_range_proof::batched_range_proof_u128::BatchedRangeProofU128Data,
            ciphertext_commitment_equality::CiphertextCommitmentEqualityProofData,
            BatchedGroupedCiphertext3HandlesValidityProofData, ZkProofData,
        },
    },
    spl_token_confidential_transfer_proof_generation::{
        try_combine_lo_hi_ciphertexts, try_combine_lo_hi_commitments, try_combine_lo_hi_openings,
        try_split_u64, REMAINING_BALANCE_BIT_LENGTH, TRANSFER_AMOUNT_HI_BITS,
        TRANSFER_AMOUNT_LO_BITS,
    },
};

const RANGE_PROOF_PADDING_BIT_LENGTH: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferCommitments {
    pub amount_lo: [u8; 32],
    pub amount_hi: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimBridgeWitness {
    pub base_amount_commitment: [u8; 32],
    pub quote_amount_commitment: [u8; 32],
    pub batched_amount_commitment: [u8; 32],
    pub batched_opening: [u8; 32],
}

/// A host-verified proof bundle ready to be written to three proof-context
/// accounts and consumed by Token-2022's confidential `Transfer` instruction.
pub struct TransferProofBundle {
    pub equality_proof: CiphertextCommitmentEqualityProofData,
    pub ciphertext_validity_proof: BatchedGroupedCiphertext3HandlesValidityProofData,
    pub range_proof: BatchedRangeProofU128Data,
    pub new_source_decryptable_available_balance: [u8; 36],
    amount: u64,
    source_token_account: [u8; 32],
    mint: [u8; 32],
    destination_token_account: [u8; 32],
    auditor_ciphertext_lo: ElGamalCiphertext,
    auditor_ciphertext_hi: ElGamalCiphertext,
    /// Opening of `C_lo + 2^16*C_hi`. It is consumed by the private claim
    /// prover and never serialized into a transaction.
    combined_amount_opening: PedersenOpening,
}

pub struct WithdrawalProofBundle {
    pub base: TransferProofBundle,
    pub quote: TransferProofBundle,
    pub bridge: ClaimBridgeWitness,
}

impl WithdrawalProofBundle {
    pub fn transfer_args(
        &self,
    ) -> (
        beatex::ConfidentialTransferArgs,
        beatex::ConfidentialTransferArgs,
    ) {
        (self.base.transfer_args(), self.quote.transfer_args())
    }
}

impl TransferProofBundle {
    pub const fn amount(&self) -> u64 {
        self.amount
    }

    pub fn auditor_ciphertext_lo(&self) -> [u8; 64] {
        self.auditor_ciphertext_lo.to_bytes()
    }

    pub fn auditor_ciphertext_hi(&self) -> [u8; 64] {
        self.auditor_ciphertext_hi.to_bytes()
    }

    pub fn commitments(&self) -> Result<TransferCommitments, VaultProofError> {
        let lo: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_lo
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        let hi: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_hi
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        Ok(TransferCommitments {
            amount_lo: lo.commitment.to_bytes(),
            amount_hi: hi.commitment.to_bytes(),
        })
    }

    pub fn combined_amount_opening(&self) -> [u8; 32] {
        self.combined_amount_opening.to_bytes()
    }

    pub fn combined_amount_commitment(&self) -> Result<[u8; 32], VaultProofError> {
        let commitments = self.commitments()?;
        let lo = solana_zk_sdk_v4::encryption::pedersen::PedersenCommitment::from_bytes(
            &commitments.amount_lo,
        )
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
        let hi = solana_zk_sdk_v4::encryption::pedersen::PedersenCommitment::from_bytes(
            &commitments.amount_hi,
        )
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
        try_combine_lo_hi_commitments(&lo, &hi, TRANSFER_AMOUNT_LO_BITS)
            .map(|commitment| commitment.to_bytes())
            .ok_or(VaultProofError::InvalidGeneratedContext)
    }

    /// Return the exact public proof material independently reconstructed by
    /// the BeatEx program before it verifies the claim/refund Groth16 proof.
    pub fn proof_binding(
        &self,
    ) -> Result<beatex::token2022::TransferProofBinding, VaultProofError> {
        let lo: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_lo
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        let hi: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_hi
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        let ciphertexts = |ciphertext: &GroupedElGamalCiphertext<3>| {
            Ok::<_, VaultProofError>([
                ciphertext
                    .to_elgamal_ciphertext(0)
                    .map_err(|_| VaultProofError::InvalidGeneratedContext)?
                    .to_bytes(),
                ciphertext
                    .to_elgamal_ciphertext(1)
                    .map_err(|_| VaultProofError::InvalidGeneratedContext)?
                    .to_bytes(),
                ciphertext
                    .to_elgamal_ciphertext(2)
                    .map_err(|_| VaultProofError::InvalidGeneratedContext)?
                    .to_bytes(),
            ])
        };
        let pubkey_bytes = |value| {
            ElGamalPubkey::try_from(value)
                .map(Into::into)
                .map_err(|_| VaultProofError::InvalidGeneratedContext)
        };
        let new_source_ciphertext =
            ElGamalCiphertext::try_from(self.equality_proof.context.ciphertext)
                .map_err(|_| VaultProofError::InvalidGeneratedContext)?
                .to_bytes();
        Ok(beatex::token2022::TransferProofBinding {
            source_pubkey: pubkey_bytes(self.ciphertext_validity_proof.context.first_pubkey)?,
            destination_pubkey: pubkey_bytes(self.ciphertext_validity_proof.context.second_pubkey)?,
            auditor_pubkey: pubkey_bytes(self.ciphertext_validity_proof.context.third_pubkey)?,
            new_source_ciphertext,
            ciphertexts_lo: ciphertexts(&lo)?,
            ciphertexts_hi: ciphertexts(&hi)?,
        })
    }

    pub fn transfer_args(&self) -> beatex::ConfidentialTransferArgs {
        beatex::ConfidentialTransferArgs {
            new_source_decryptable_available_balance: self.new_source_decryptable_available_balance,
            auditor_ciphertext_lo: self.auditor_ciphertext_lo(),
            auditor_ciphertext_hi: self.auditor_ciphertext_hi(),
        }
    }

    /// Verify all three proofs and the exact cross-context relationships that
    /// Token-2022 v11 checks before mutating balances.
    pub fn verify_host(&self) -> Result<(), VaultProofError> {
        self.equality_proof
            .verify_proof()
            .map_err(|_| VaultProofError::ProofVerification("ciphertext-commitment equality"))?;
        self.ciphertext_validity_proof
            .verify_proof()
            .map_err(|_| VaultProofError::ProofVerification("grouped-ciphertext validity"))?;
        self.range_proof
            .verify_proof()
            .map_err(|_| VaultProofError::ProofVerification("batched range"))?;

        if self.equality_proof.context.pubkey != self.ciphertext_validity_proof.context.first_pubkey
        {
            return Err(VaultProofError::InconsistentProofContexts);
        }

        let lo: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_lo
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        let hi: GroupedElGamalCiphertext<3> = self
            .ciphertext_validity_proof
            .context
            .grouped_ciphertext_hi
            .try_into()
            .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
        let expected_commitments = [
            self.equality_proof.context.commitment,
            PodPedersenCommitment::from(lo.commitment),
            PodPedersenCommitment::from(hi.commitment),
        ];
        if self.range_proof.context.commitments[..3] != expected_commitments
            || self.range_proof.context.bit_lengths[..4] != [64, 16, 32, 16]
        {
            return Err(VaultProofError::InconsistentProofContexts);
        }
        Ok(())
    }
}

/// Build the single private opening consumed by the Groth16 bridge after both
/// standard Token-2022 proof bundles have fixed their amount commitments.
pub fn claim_bridge_witness(
    base: &TransferProofBundle,
    quote: &TransferProofBundle,
) -> Result<ClaimBridgeWitness, VaultProofError> {
    let base_amount_commitment = base.combined_amount_commitment()?;
    let quote_amount_commitment = quote.combined_amount_commitment()?;
    let rho_bytes = beatex::queue_hash::ristretto_batching_scalar(
        base_amount_commitment,
        quote_amount_commitment,
    )
    .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
    let rho = Scalar::from_canonical_bytes(rho_bytes)
        .into_option()
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let base_opening = PedersenOpening::from_bytes(&base.combined_amount_opening())
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let quote_opening = PedersenOpening::from_bytes(&quote.combined_amount_opening())
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let batched_opening = (base_opening + quote_opening * rho).to_bytes();
    let base_commitment = solana_zk_sdk_v4::encryption::pedersen::PedersenCommitment::from_bytes(
        &base_amount_commitment,
    )
    .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let quote_commitment = solana_zk_sdk_v4::encryption::pedersen::PedersenCommitment::from_bytes(
        &quote_amount_commitment,
    )
    .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let batched_amount_commitment = (base_commitment + quote_commitment * rho).to_bytes();
    Ok(ClaimBridgeWitness {
        base_amount_commitment,
        quote_amount_commitment,
        batched_amount_commitment,
        batched_opening,
    })
}

fn field_bytes(value: Fq) -> [u8; 32] {
    let encoded = value.into_bigint().to_bytes_le();
    let mut bytes = [0u8; 32];
    bytes[..encoded.len()].copy_from_slice(&encoded);
    bytes
}

fn pubkey_fields(key: &solana_pubkey::Pubkey) -> [Fq; 2] {
    let bytes = key.to_bytes();
    [
        Fq::from_le_bytes_mod_order(&bytes[..16]),
        Fq::from_le_bytes_mod_order(&bytes[16..]),
    ]
}

pub fn derive_private_order_identifiers(
    opening: &PrivateOrderOpening,
    auction: &solana_pubkey::Pubkey,
    owner: &solana_pubkey::Pubkey,
    client_order_nonce: u64,
) -> ([u8; 32], [u8; 32]) {
    let witness = PrivateWithdrawalWitness {
        kind: PrivateWithdrawalKind::Claim,
        market: [Fq::from(0u64); 2],
        auction: pubkey_fields(auction),
        order: [Fq::from(0u64); 2],
        owner: pubkey_fields(owner),
        key_epoch: [Fq::from(0u64); 2],
        client_order_nonce,
        sequence: 0,
        allocation_rule_version: 1,
        order_circuit_version: 1,
        claim_circuit_version: 1,
        buy: opening.buy,
        sell: opening.sell,
        base_reserve: opening.base_reserve,
        quote_reserve: opening.quote_reserve,
        order_salt: Fq::from_le_bytes_mod_order(&opening.order_salt),
        clearing_index: 0,
        clearing_price: 0,
        demand: 0,
        supply: 0,
        clearing_volume: 0,
        opaque_withdrawal_fields: [Fq::from(0u64);
            beatex::queue_hash::PRIVATE_WITHDRAWAL_OPAQUE_FIELDS],
        bridge: RistrettoBridgeWitness::for_amounts(0, 0),
    };
    (
        field_bytes(witness.order_commitment()),
        field_bytes(witness.nullifier()),
    )
}

/// Assemble the exact claim/refund witness from an opened private order, the
/// public on-chain statement, and the two host-verified Token-2022 bundles.
/// The function independently checks order commitment, terminal nullifier and
/// the final Poseidon digest before any expensive Groth16 proving begins.
pub fn prepare_private_withdrawal_witness(
    opening: &PrivateOrderOpening,
    context: &WithdrawalStatementContext,
    bundle: &WithdrawalProofBundle,
) -> Result<PrivateWithdrawalWitness, VaultProofError> {
    if context.base_vault.to_bytes() != bundle.base.source_token_account
        || context.base_mint.to_bytes() != bundle.base.mint
        || context.base_destination.to_bytes() != bundle.base.destination_token_account
        || context.quote_vault.to_bytes() != bundle.quote.source_token_account
        || context.quote_mint.to_bytes() != bundle.quote.mint
        || context.quote_destination.to_bytes() != bundle.quote.destination_token_account
    {
        return Err(VaultProofError::StatementAccountMismatch);
    }
    let base_binding = bundle.base.proof_binding()?;
    let quote_binding = bundle.quote.proof_binding()?;
    let base_args = bundle.base.transfer_args();
    let quote_args = bundle.quote.transfer_args();
    let statement = beatex::queue_hash::PrivateWithdrawalStatement {
        kind: context.kind,
        market: &context.market,
        auction: &context.auction,
        order: &context.order,
        owner: &context.owner,
        key_epoch: &context.key_epoch,
        client_order_nonce: context.client_order_nonce,
        sequence: context.sequence,
        allocation_rule_version: context.allocation_rule_version,
        order_circuit_version: context.order_circuit_version,
        claim_circuit_version: context.claim_circuit_version,
        order_commitment: context.order_commitment,
        clearing_index: context.clearing_index,
        clearing_price: context.clearing_price,
        demand_at_clearing: context.demand_at_clearing,
        supply_at_clearing: context.supply_at_clearing,
        clearing_volume: context.clearing_volume,
        terminal_nullifier: context.terminal_nullifier,
        base_mint: &context.base_mint,
        quote_mint: &context.quote_mint,
        base_vault: &context.base_vault,
        quote_vault: &context.quote_vault,
        base_destination: &context.base_destination,
        quote_destination: &context.quote_destination,
        base_transfer: &base_binding,
        quote_transfer: &quote_binding,
        base_new_decryptable_balance: &base_args.new_source_decryptable_available_balance,
        quote_new_decryptable_balance: &quote_args.new_source_decryptable_available_balance,
        base_amount_commitment: bundle.bridge.base_amount_commitment,
        quote_amount_commitment: bundle.bridge.quote_amount_commitment,
        batched_amount_commitment: bundle.bridge.batched_amount_commitment,
    };
    let opaque = beatex::queue_hash::private_withdrawal_opaque_fields(&statement)
        .map_err(|_| VaultProofError::TranscriptMismatch)?
        .into_iter()
        .map(|field| Fq::from_le_bytes_mod_order(&field))
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| VaultProofError::TranscriptMismatch)?;
    let kind = match context.kind {
        beatex::queue::WithdrawalKind::Claim => PrivateWithdrawalKind::Claim,
        beatex::queue::WithdrawalKind::Refund => PrivateWithdrawalKind::Refund,
    };
    let witness = PrivateWithdrawalWitness {
        kind,
        market: pubkey_fields(&context.market),
        auction: pubkey_fields(&context.auction),
        order: pubkey_fields(&context.order),
        owner: pubkey_fields(&context.owner),
        key_epoch: pubkey_fields(&context.key_epoch),
        client_order_nonce: context.client_order_nonce,
        sequence: context.sequence,
        allocation_rule_version: context.allocation_rule_version,
        order_circuit_version: context.order_circuit_version,
        claim_circuit_version: context.claim_circuit_version,
        buy: opening.buy,
        sell: opening.sell,
        base_reserve: opening.base_reserve,
        quote_reserve: opening.quote_reserve,
        order_salt: Fq::from_le_bytes_mod_order(&opening.order_salt),
        clearing_index: context.clearing_index,
        clearing_price: context.clearing_price,
        demand: context.demand_at_clearing,
        supply: context.supply_at_clearing,
        clearing_volume: context.clearing_volume,
        opaque_withdrawal_fields: opaque,
        bridge: RistrettoBridgeWitness {
            base_amount: bundle.base.amount(),
            quote_amount: bundle.quote.amount(),
            batched_opening: bundle.bridge.batched_opening,
            base_commitment: bundle.bridge.base_amount_commitment,
            quote_commitment: bundle.bridge.quote_amount_commitment,
            batched_commitment: bundle.bridge.batched_amount_commitment,
        },
    };
    if field_bytes(witness.order_commitment()) != context.order_commitment {
        return Err(VaultProofError::OrderCommitmentMismatch);
    }
    if field_bytes(witness.nullifier()) != context.terminal_nullifier {
        return Err(VaultProofError::NullifierMismatch);
    }
    let program_digest = beatex::queue_hash::private_withdrawal_statement_digest(&statement)
        .map_err(|_| VaultProofError::TranscriptMismatch)?;
    if field_bytes(witness.digest()) != program_digest {
        return Err(VaultProofError::TranscriptMismatch);
    }
    Ok(witness)
}

pub fn generate_private_withdrawal_groth16_proof<R: RngCore + CryptoRng>(
    proving_key: &ProvingKey<Bn254>,
    witness: PrivateWithdrawalWitness,
    rng: &mut R,
) -> Result<beatex::Groth16ProofInput, VaultProofError> {
    let proof =
        beatex_babyjubjub_groth16_spike::claim::prove_private_withdrawal(proving_key, witness, rng)
            .map_err(|error| {
                VaultProofError::PrivateWithdrawalProofGeneration(error.to_string())
            })?;
    Ok(beatex::Groth16ProofInput {
        proof_a: proof.proof_a,
        proof_b: proof.proof_b,
        proof_c: proof.proof_c,
        public_input: proof.public_input,
    })
}

impl core::fmt::Debug for TransferProofBundle {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("TransferProofBundle")
            .field("commitments", &self.commitments())
            .field(
                "new_source_decryptable_available_balance",
                &"[REDACTED AE CIPHERTEXT]",
            )
            .finish_non_exhaustive()
    }
}

pub fn generate_transfer_proof(
    request: &TransferRequest,
    source_key: &VaultElGamalKeypair,
    source_ae_key: &VaultAeKey,
) -> Result<TransferProofBundle, VaultProofError> {
    request.validate()?;
    if source_key.pubkey_bytes() != request.source.elgamal_pubkey {
        return Err(VaultProofError::SourceKeyMismatch);
    }

    let current_available_balance =
        ElGamalCiphertext::from_bytes(&request.source.available_balance)
            .ok_or(VaultProofError::InvalidAvailableBalance)?;
    let current_decryptable_available_balance =
        AeCiphertext::from_bytes(&request.source.decryptable_available_balance)
            .ok_or(VaultProofError::InvalidDecryptableBalance)?;
    let clear_balance = current_decryptable_available_balance
        .decrypt(&source_ae_key.inner)
        .ok_or(VaultProofError::InvalidDecryptableBalance)?;
    let new_clear_balance = clear_balance
        .checked_sub(request.amount)
        .ok_or(VaultProofError::InsufficientFunds)?;

    let destination_pubkey = ElGamalPubkey::try_from(request.destination.elgamal_pubkey.as_slice())
        .map_err(|_| VaultProofError::InvalidDestinationKey)?;
    let auditor_pubkey = request
        .auditor_elgamal_pubkey
        .as_ref()
        .map(|bytes| ElGamalPubkey::try_from(bytes.as_slice()))
        .transpose()
        .map_err(|_| VaultProofError::InvalidAuditorKey)?;

    let default_auditor_pubkey = ElGamalPubkey::default();
    let auditor_pubkey = auditor_pubkey.as_ref().unwrap_or(&default_auditor_pubkey);
    let (amount_lo, amount_hi) = try_split_u64(request.amount, TRANSFER_AMOUNT_LO_BITS)
        .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let opening_lo = PedersenOpening::new_rand();
    let opening_hi = PedersenOpening::new_rand();
    let grouped_lo = GroupedElGamal::<3>::encrypt_with(
        [
            source_key.inner.pubkey(),
            &destination_pubkey,
            auditor_pubkey,
        ],
        amount_lo,
        &opening_lo,
    );
    let grouped_hi = GroupedElGamal::<3>::encrypt_with(
        [
            source_key.inner.pubkey(),
            &destination_pubkey,
            auditor_pubkey,
        ],
        amount_hi,
        &opening_hi,
    );
    let source_ciphertext_lo = grouped_lo
        .to_elgamal_ciphertext(0)
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
    let source_ciphertext_hi = grouped_hi
        .to_elgamal_ciphertext(0)
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
    let transfer_ciphertext = try_combine_lo_hi_ciphertexts(
        &source_ciphertext_lo,
        &source_ciphertext_hi,
        TRANSFER_AMOUNT_LO_BITS,
    )
    .ok_or(VaultProofError::InvalidGeneratedContext)?;
    #[allow(clippy::arithmetic_side_effects)]
    let new_available_balance_ciphertext = current_available_balance - transfer_ciphertext;
    let (new_available_balance_commitment, new_source_opening) = Pedersen::new(new_clear_balance);
    let equality_proof_data = CiphertextCommitmentEqualityProofData::new(
        &source_key.inner,
        &new_available_balance_ciphertext,
        &new_available_balance_commitment,
        &new_source_opening,
        new_clear_balance,
    )
    .map_err(|error| VaultProofError::ProofGeneration(error.to_string()))?;
    let ciphertext_validity_proof_data = BatchedGroupedCiphertext3HandlesValidityProofData::new(
        source_key.inner.pubkey(),
        &destination_pubkey,
        auditor_pubkey,
        &grouped_lo,
        &grouped_hi,
        amount_lo,
        amount_hi,
        &opening_lo,
        &opening_hi,
    )
    .map_err(|error| VaultProofError::ProofGeneration(error.to_string()))?;
    let auditor_ciphertext_lo = ciphertext_validity_proof_data
        .context_data()
        .grouped_ciphertext_lo
        .try_extract_ciphertext(2)
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?
        .try_into()
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
    let auditor_ciphertext_hi = ciphertext_validity_proof_data
        .context_data()
        .grouped_ciphertext_hi
        .try_extract_ciphertext(2)
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?
        .try_into()
        .map_err(|_| VaultProofError::InvalidGeneratedContext)?;
    let (padding_commitment, padding_opening) = Pedersen::new(0_u64);
    let range_proof_data = BatchedRangeProofU128Data::new(
        vec![
            &new_available_balance_commitment,
            &grouped_lo.commitment,
            &grouped_hi.commitment,
            &padding_commitment,
        ],
        vec![new_clear_balance, amount_lo, amount_hi, 0],
        vec![
            REMAINING_BALANCE_BIT_LENGTH,
            TRANSFER_AMOUNT_LO_BITS,
            TRANSFER_AMOUNT_HI_BITS,
            RANGE_PROOF_PADDING_BIT_LENGTH,
        ],
        vec![
            &new_source_opening,
            &opening_lo,
            &opening_hi,
            &padding_opening,
        ],
    )
    .map_err(|error| VaultProofError::ProofGeneration(error.to_string()))?;
    let combined_amount_opening =
        try_combine_lo_hi_openings(&opening_lo, &opening_hi, TRANSFER_AMOUNT_LO_BITS)
            .ok_or(VaultProofError::InvalidGeneratedContext)?;
    let bundle = TransferProofBundle {
        equality_proof: equality_proof_data,
        ciphertext_validity_proof: ciphertext_validity_proof_data,
        range_proof: range_proof_data,
        new_source_decryptable_available_balance: source_ae_key
            .inner
            .encrypt(new_clear_balance)
            .to_bytes(),
        amount: request.amount,
        source_token_account: request.source.token_account,
        mint: request.source.mint,
        destination_token_account: request.destination.token_account,
        auditor_ciphertext_lo,
        auditor_ciphertext_hi,
        combined_amount_opening,
    };
    bundle.verify_host()?;
    Ok(bundle)
}

/// Generate both standard Token-2022 proof bundles and the retained secret
/// material required by one atomic BeatEx claim/refund proof.
#[allow(clippy::too_many_arguments)]
pub fn generate_withdrawal_proof_bundle(
    base_request: &TransferRequest,
    quote_request: &TransferRequest,
    base_source_key: &VaultElGamalKeypair,
    quote_source_key: &VaultElGamalKeypair,
    base_ae_key: &VaultAeKey,
    quote_ae_key: &VaultAeKey,
) -> Result<WithdrawalProofBundle, VaultProofError> {
    let base = generate_transfer_proof(base_request, base_source_key, base_ae_key)?;
    let quote = generate_transfer_proof(quote_request, quote_source_key, quote_ae_key)?;
    let bridge = claim_bridge_witness(&base, &quote)?;
    Ok(WithdrawalProofBundle {
        base,
        quote,
        bridge,
    })
}

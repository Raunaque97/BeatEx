//! Token-2022 confidential-vault CPI helpers.
//!
//! Proof generation stays offchain. These helpers consume already-verified ZK
//! proof-context accounts and sign only with the market vault-authority PDA.

use {
    anchor_lang::{
        prelude::*,
        solana_program::program::{invoke, invoke_signed},
    },
    solana_curve25519::{
        ristretto::{add_ristretto, multiply_ristretto, PodRistrettoPoint},
        scalar::PodScalar,
    },
    solana_zk_elgamal_proof_interface::proof_data::{
        BatchedGroupedCiphertext3HandlesValidityProofContext, BatchedRangeProofContext,
        CiphertextCommitmentEqualityProofContext,
    },
    solana_zk_sdk_pod::encryption::{
        auth_encryption::PodAeCiphertext,
        elgamal::{PodElGamalCiphertext, PodElGamalPubkey},
    },
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::{
                instruction::{
                    deposit as confidential_deposit, inner_apply_pending_balance, inner_transfer,
                    BatchedGroupedCiphertext3HandlesValidityProofData, BatchedRangeProofU128Data,
                    CiphertextCommitmentEqualityProofData,
                },
                ConfidentialTransferAccount,
            },
            BaseStateWithExtensions, StateWithExtensions,
        },
        instruction::transfer_checked,
        state::{Account as TokenAccount, Mint},
    },
    spl_token_confidential_transfer_proof_extraction::{
        instruction::{verify_and_extract_context, ProofLocation},
        transfer::TransferProofContext,
    },
};

use crate::{queue_hash, BeatExError};

pub const AE_CIPHERTEXT_LEN: usize = 36;

/// Canonical public material extracted from one Token-2022 confidential
/// transfer proof bundle. The claim/refund statement hashes this data before
/// Groth16 verification, so a relayer cannot substitute destinations, keys,
/// ciphertext handles or the post-transfer source balance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferProofBinding {
    pub source_pubkey: [u8; 32],
    pub destination_pubkey: [u8; 32],
    pub auditor_pubkey: [u8; 32],
    pub new_source_ciphertext: [u8; 64],
    pub ciphertexts_lo: [[u8; 64]; 3],
    pub ciphertexts_hi: [[u8; 64]; 3],
}

impl TransferProofBinding {
    pub fn commitment_lo(&self) -> [u8; 32] {
        self.ciphertexts_lo[0][..32]
            .try_into()
            .expect("fixed ciphertext prefix")
    }

    pub fn commitment_hi(&self) -> [u8; 32] {
        self.ciphertexts_hi[0][..32]
            .try_into()
            .expect("fixed ciphertext prefix")
    }

    /// Combine Token-2022's 16-bit low and 32-bit high commitments into the
    /// commitment to the private 48-bit transfer amount:
    /// `C = C_lo + 2^16 * C_hi`.
    pub fn combined_amount_commitment(&self) -> Result<[u8; 32]> {
        combine_transfer_amount_commitments(self.commitment_lo(), self.commitment_hi())
    }
}

fn extract_transfer_ciphertexts(
    context: &spl_token_confidential_transfer_proof_extraction::encryption::PodTransferAmountCiphertext,
) -> Result<[[u8; 64]; 3]> {
    Ok([
        context
            .try_extract_ciphertext(0)
            .map_err(|_| error!(BeatExError::InvalidCurveInput))?
            .0,
        context
            .try_extract_ciphertext(1)
            .map_err(|_| error!(BeatExError::InvalidCurveInput))?
            .0,
        context
            .try_extract_ciphertext(2)
            .map_err(|_| error!(BeatExError::InvalidCurveInput))?
            .0,
    ])
}

/// Decode and cross-check the three proof-context accounts exactly as
/// Token-2022 does before a confidential transfer. This does not replace the
/// Token-2022 CPI; it gives the FBA claim transcript the same canonical public
/// statement that the CPI will consume.
pub fn extract_transfer_proof_binding<'info>(
    equality_proof_context: &AccountInfo<'info>,
    ciphertext_validity_proof_context: &AccountInfo<'info>,
    range_proof_context: &AccountInfo<'info>,
) -> Result<TransferProofBinding> {
    let equality_accounts = [equality_proof_context.clone()];
    let validity_accounts = [ciphertext_validity_proof_context.clone()];
    let range_accounts = [range_proof_context.clone()];
    let equality = verify_and_extract_context::<
        CiphertextCommitmentEqualityProofData,
        CiphertextCommitmentEqualityProofContext,
    >(&mut equality_accounts.iter(), 0, None)
    .map_err(|_| error!(BeatExError::InvalidCurveInput))?;
    let validity = verify_and_extract_context::<
        BatchedGroupedCiphertext3HandlesValidityProofData,
        BatchedGroupedCiphertext3HandlesValidityProofContext,
    >(&mut validity_accounts.iter(), 0, None)
    .map_err(|_| error!(BeatExError::InvalidCurveInput))?;
    let range = verify_and_extract_context::<BatchedRangeProofU128Data, BatchedRangeProofContext>(
        &mut range_accounts.iter(),
        0,
        None,
    )
    .map_err(|_| error!(BeatExError::InvalidCurveInput))?;
    let transfer = TransferProofContext::verify_and_extract(&equality, &validity, &range)
        .map_err(|_| error!(BeatExError::InvalidCurveInput))?;

    Ok(TransferProofBinding {
        source_pubkey: transfer.transfer_pubkeys.source.0,
        destination_pubkey: transfer.transfer_pubkeys.destination.0,
        auditor_pubkey: transfer.transfer_pubkeys.auditor.0,
        new_source_ciphertext: transfer.new_source_ciphertext.0,
        ciphertexts_lo: extract_transfer_ciphertexts(&transfer.ciphertext_lo)?,
        ciphertexts_hi: extract_transfer_ciphertexts(&transfer.ciphertext_hi)?,
    })
}

pub fn combine_transfer_amount_commitments(
    commitment_lo: [u8; 32],
    commitment_hi: [u8; 32],
) -> Result<[u8; 32]> {
    let mut radix_bytes = [0u8; 32];
    radix_bytes[2] = 1; // 2^16, little endian
    let scaled_hi = multiply_ristretto(&PodScalar(radix_bytes), &PodRistrettoPoint(commitment_hi))
        .ok_or_else(|| error!(BeatExError::InvalidRistrettoPoint))?;
    add_ristretto(&PodRistrettoPoint(commitment_lo), &scaled_hi)
        .map(|point| point.0)
        .ok_or_else(|| error!(BeatExError::InvalidRistrettoPoint))
}

/// Combine the two asset commitments with the claim circuit's Fiat-Shamir
/// coefficient. The returned point is the only Ristretto relation checked in
/// Groth16; this native precheck proves it was derived from the exact two
/// Token-2022 transfer commitments.
pub fn batch_transfer_amount_commitments(base: [u8; 32], quote: [u8; 32]) -> Result<[u8; 32]> {
    let scalar = queue_hash::ristretto_batching_scalar(base, quote)?;
    let scaled_quote = multiply_ristretto(&PodScalar(scalar), &PodRistrettoPoint(quote))
        .ok_or_else(|| error!(BeatExError::InvalidRistrettoPoint))?;
    add_ristretto(&PodRistrettoPoint(base), &scaled_quote)
        .map(|point| point.0)
        .ok_or_else(|| error!(BeatExError::InvalidRistrettoPoint))
}

pub fn validate_transfer_args(
    binding: &TransferProofBinding,
    new_source_decryptable_available_balance: &[u8; AE_CIPHERTEXT_LEN],
    auditor_ciphertext_lo: &[u8; 64],
    auditor_ciphertext_hi: &[u8; 64],
) -> Result<()> {
    let _ = new_source_decryptable_available_balance;
    require!(
        binding.ciphertexts_lo[2] == *auditor_ciphertext_lo
            && binding.ciphertexts_hi[2] == *auditor_ciphertext_hi,
        BeatExError::InvalidCurveInput
    );
    Ok(())
}

pub fn mint_decimals(mint_data: &[u8]) -> Result<u8> {
    let mint = StateWithExtensions::<Mint>::unpack(mint_data)
        .map_err(|_| error!(BeatExError::InvalidDepositAccount))?;
    require!(mint.base.is_initialized, BeatExError::InvalidDepositAccount);
    Ok(mint.base.decimals)
}

pub fn validate_public_token_account(
    token_account_data: &[u8],
    expected_mint: &Pubkey,
    expected_owner: &Pubkey,
) -> Result<u64> {
    let token_account = StateWithExtensions::<TokenAccount>::unpack(token_account_data)
        .map_err(|_| error!(BeatExError::InvalidDepositAccount))?;
    require_keys_eq!(
        token_account.base.mint,
        *expected_mint,
        BeatExError::InvalidDepositAccount
    );
    require_keys_eq!(
        token_account.base.owner,
        *expected_owner,
        BeatExError::InvalidDepositAccount
    );
    Ok(token_account.base.amount)
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_public_transfer_checked<'info>(
    token_program: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = transfer_checked(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        owner.key,
        &[],
        amount,
        decimals,
    )?;
    invoke(
        &instruction,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            owner.clone(),
            token_program.clone(),
        ],
    )?;
    Ok(())
}

pub fn invoke_confidential_deposit<'info>(
    token_program: &AccountInfo<'info>,
    token_account: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = confidential_deposit(
        token_program.key,
        token_account.key,
        mint.key,
        amount,
        decimals,
        authority.key,
        &[],
    )?;
    invoke_signed(
        &instruction,
        &[
            token_account.clone(),
            mint.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn validate_confidential_deposit_deltas(
    source_before: u64,
    source_after: u64,
    vault_before: u64,
    vault_after: u64,
    amount: u64,
) -> Result<()> {
    // The first CPI moves `amount` into the vault's public balance and the
    // second immediately deposits the same amount into confidential pending.
    // Therefore the vault's net public balance must remain unchanged.
    require!(
        source_before.checked_sub(source_after) == Some(amount) && vault_after == vault_before,
        BeatExError::NonExactDeposit
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_confidential_transfer<'info>(
    token_program: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    equality_proof_context: &AccountInfo<'info>,
    ciphertext_validity_proof_context: &AccountInfo<'info>,
    range_proof_context: &AccountInfo<'info>,
    new_source_decryptable_available_balance: [u8; AE_CIPHERTEXT_LEN],
    auditor_ciphertext_lo: [u8; 64],
    auditor_ciphertext_hi: [u8; 64],
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = inner_transfer(
        token_program.key,
        source.key,
        mint.key,
        destination.key,
        &PodAeCiphertext(new_source_decryptable_available_balance),
        &PodElGamalCiphertext(auditor_ciphertext_lo),
        &PodElGamalCiphertext(auditor_ciphertext_hi),
        authority.key,
        &[],
        ProofLocation::<CiphertextCommitmentEqualityProofData>::ContextStateAccount(
            equality_proof_context.key,
        ),
        ProofLocation::<BatchedGroupedCiphertext3HandlesValidityProofData>::ContextStateAccount(
            ciphertext_validity_proof_context.key,
        ),
        ProofLocation::<BatchedRangeProofU128Data>::ContextStateAccount(range_proof_context.key),
    )?;

    invoke_signed(
        &instruction,
        &[
            source.clone(),
            mint.clone(),
            destination.clone(),
            equality_proof_context.clone(),
            ciphertext_validity_proof_context.clone(),
            range_proof_context.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn invoke_apply_pending_balance<'info>(
    token_program: &AccountInfo<'info>,
    token_account: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    expected_pending_balance_credit_counter: u64,
    new_decryptable_available_balance: [u8; AE_CIPHERTEXT_LEN],
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_keys_eq!(
        *token_program.key,
        spl_token_2022_interface::id(),
        BeatExError::InvalidTokenProgram
    );
    let instruction = inner_apply_pending_balance(
        token_program.key,
        token_account.key,
        expected_pending_balance_credit_counter,
        &PodAeCiphertext(new_decryptable_available_balance),
        authority.key,
        &[],
    )?;
    invoke_signed(
        &instruction,
        &[
            token_account.clone(),
            authority.clone(),
            token_program.clone(),
        ],
        signer_seeds,
    )?;
    Ok(())
}

pub fn validate_confidential_vault(
    token_account_data: &[u8],
    expected_mint: &Pubkey,
    expected_authority: &Pubkey,
    expected_elgamal_pubkey: &[u8; 32],
) -> Result<()> {
    let token_account = StateWithExtensions::<TokenAccount>::unpack(token_account_data)
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    require_keys_eq!(
        token_account.base.mint,
        *expected_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        token_account.base.owner,
        *expected_authority,
        BeatExError::InvalidConfidentialVault
    );
    let confidential = token_account
        .get_extension::<ConfidentialTransferAccount>()
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    confidential
        .approved()
        .map_err(|_| error!(BeatExError::InvalidConfidentialVault))?;
    require!(
        confidential.elgamal_pubkey == PodElGamalPubkey(*expected_elgamal_pubkey),
        BeatExError::InvalidConfidentialVault
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::{program_option::COption, program_pack::Pack};
    use beatex_babyjubjub_groth16_spike::ristretto_bridge::RistrettoBridgeWitness;
    use solana_zk_sdk::encryption::pedersen::Pedersen;
    use spl_token_2022_interface::state::AccountState;

    fn token_account_data(mint: Pubkey, owner: Pubkey, amount: u64) -> Vec<u8> {
        let account = TokenAccount {
            mint,
            owner,
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut data = vec![0; TokenAccount::LEN];
        TokenAccount::pack(account, &mut data).unwrap();
        data
    }

    #[test]
    fn public_deposit_account_validation_binds_mint_owner_and_amount() {
        let mint = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let data = token_account_data(mint, owner, 123);
        assert_eq!(
            validate_public_token_account(&data, &mint, &owner).unwrap(),
            123
        );
        assert!(validate_public_token_account(&data, &Pubkey::new_unique(), &owner).is_err());
        assert!(validate_public_token_account(&data, &mint, &Pubkey::new_unique()).is_err());
    }

    #[test]
    fn confidential_deposit_requires_exact_source_debit_and_zero_public_vault_delta() {
        validate_confidential_deposit_deltas(100, 93, 20, 20, 7).unwrap();
        assert!(validate_confidential_deposit_deltas(100, 94, 20, 20, 7).is_err());
        assert!(validate_confidential_deposit_deltas(100, 93, 20, 27, 7).is_err());
        assert!(validate_confidential_deposit_deltas(100, 93, 20, 19, 7).is_err());
    }

    #[test]
    fn low_and_high_transfer_commitments_combine_to_the_48_bit_amount_commitment() {
        let amount = (0x1234_5678u64 << 16) | 0xabcd;
        let (commitment_lo, _) = Pedersen::new(amount & 0xffff);
        let (commitment_hi, _) = Pedersen::new(amount >> 16);
        let expected = commitment_lo + commitment_hi * (1u64 << 16);
        assert_eq!(
            combine_transfer_amount_commitments(commitment_lo.to_bytes(), commitment_hi.to_bytes())
                .unwrap(),
            expected.to_bytes()
        );
    }

    #[test]
    fn program_batching_matches_the_exact_groth16_bridge() {
        let witness = RistrettoBridgeWitness::deterministic();
        assert_eq!(
            batch_transfer_amount_commitments(witness.base_commitment, witness.quote_commitment,)
                .unwrap(),
            witness.batched_commitment
        );
    }

    #[test]
    fn transfer_commitment_combination_rejects_invalid_points() {
        assert!(combine_transfer_amount_commitments([0xff; 32], [0xff; 32]).is_err());
    }
}

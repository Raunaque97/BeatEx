//! Settles a private order directly into the trader's Baby Jubjub encrypted
//! balance. A rollover transaction composes this proof with the ordinary next
//! order proof, avoiding Token-2022 withdrawal and redeposit proof contexts.

use super::*;
use crate::{
    claim::{
        enforce_claim_allocation, expected_payouts, PrivateWithdrawalKind,
        PrivateWithdrawalWitness, NULLIFIER_DOMAIN, OPAQUE_WITHDRAWAL_FIELDS,
        ORDER_COMMITMENT_DOMAIN,
    },
    ristretto_bridge::RistrettoBridgeWitness,
    transition::{
        ciphertext_bytes, context_pubkey, decode_wire_point, field_bytes, field_bytes_le,
        field_from_bytes_le, g1_bytes, g2_bytes, pubkey_context, scalar_bytes, scalar_from_bytes,
        wire_point_bytes, OrderProofOutput, OrderProofRequest, OrderProver, PrivateBalanceOpening,
        SolanaVerificationKey,
    },
};
use ark_bn254::Bn254;
use ark_ec::AffineRepr;
use ark_groth16::{Groth16, ProvingKey, VerifyingKey};
use ark_r1cs_std::prelude::ToBitsGadget;
use ark_snark::SNARK;
use rand::{CryptoRng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::ops::Neg;

const ROLLOVER_DOMAIN: &[u8] = b"BeatEx rollover credit v2";
const BALANCE_BITS: usize = 64;

#[derive(Clone, Debug)]
pub struct RolloverSettlementWitness {
    pub claim: PrivateWithdrawalWitness,
    pub active_order: [Fq; 2],
    pub balance_revision: u64,
    pub old_base_balance: u64,
    pub old_quote_balance: u64,
    pub committee_key: EdwardsProjective,
    /// old base, old quote, credited base, credited quote.
    pub balance_randomness: [JubjubScalar; 4],
}

impl RolloverSettlementWitness {
    pub fn deterministic() -> Self {
        Self {
            claim: PrivateWithdrawalWitness::deterministic(PrivateWithdrawalKind::Claim),
            active_order: [Fq::from(61u64), Fq::from(62u64)],
            balance_revision: 9,
            old_base_balance: 80,
            old_quote_balance: 8_000_000,
            committee_key: OrderWitness::deterministic().committee_key,
            balance_randomness: [
                JubjubScalar::from(0xf001u64),
                JubjubScalar::from(0xf002u64),
                JubjubScalar::from(0xf003u64),
                JubjubScalar::from(0xf004u64),
            ],
        }
    }

    pub fn payouts(&self) -> Result<(u64, u64), SynthesisError> {
        expected_payouts(&self.claim)
    }

    pub fn order_commitment(&self) -> Fq {
        self.claim.order_commitment()
    }

    pub fn nullifier(&self) -> Fq {
        self.claim.nullifier()
    }

    fn balance_ciphertexts(&self) -> Result<[Ciphertext; 4], SynthesisError> {
        let (base_payout, quote_payout) = self.payouts()?;
        let credited_base = self
            .old_base_balance
            .checked_add(base_payout)
            .ok_or(SynthesisError::Unsatisfiable)?;
        let credited_quote = self
            .old_quote_balance
            .checked_add(quote_payout)
            .ok_or(SynthesisError::Unsatisfiable)?;
        Ok([
            encrypt(
                self.old_base_balance.into(),
                self.balance_randomness[0],
                self.committee_key,
            ),
            encrypt(
                self.old_quote_balance.into(),
                self.balance_randomness[1],
                self.committee_key,
            ),
            encrypt(
                credited_base.into(),
                self.balance_randomness[2],
                self.committee_key,
            ),
            encrypt(
                credited_quote.into(),
                self.balance_randomness[3],
                self.committee_key,
            ),
        ])
    }

    pub fn digest(&self) -> Result<Fq, SynthesisError> {
        let claim = &self.claim;
        let mut payload = vec![Fq::from_le_bytes_mod_order(ROLLOVER_DOMAIN), Fq::from(2u64)];
        payload.extend_from_slice(&claim.market);
        payload.extend_from_slice(&claim.auction);
        payload.extend_from_slice(&claim.order);
        payload.extend_from_slice(&self.active_order);
        payload.extend_from_slice(&claim.owner);
        payload.extend_from_slice(&claim.key_epoch);
        payload.push(Fq::from(claim.client_order_nonce));
        payload.push(Fq::from(claim.sequence));
        payload.push(Fq::from(claim.allocation_rule_version));
        payload.push(Fq::from(claim.order_circuit_version));
        payload.push(Fq::from(claim.claim_circuit_version));
        payload.push(self.order_commitment());
        payload.push(Fq::from(claim.clearing_index));
        payload.push(Fq::from(claim.clearing_price));
        payload.push(Fq::from(claim.demand));
        payload.push(Fq::from(claim.supply));
        payload.push(Fq::from(claim.clearing_volume));
        payload.push(self.nullifier());
        payload.push(Fq::from(self.balance_revision));
        append_wire_point_native(&mut payload, self.committee_key.into_affine());
        for ciphertext in self.balance_ciphertexts()? {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
        Ok(transcript_hash_native(&payload))
    }
}

#[derive(Clone)]
pub struct RolloverSettlementCircuit {
    pub witness: Option<RolloverSettlementWitness>,
    pub digest: Option<Fq>,
}

impl RolloverSettlementCircuit {
    pub const fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
        }
    }

    pub fn from_witness(witness: RolloverSettlementWitness) -> Result<Self, SynthesisError> {
        let digest = Some(witness.digest()?);
        Ok(Self {
            witness: Some(witness),
            digest,
        })
    }
}

impl ConstraintSynthesizer<Fq> for RolloverSettlementCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        if witness.is_some_and(|value| value.claim.kind != PrivateWithdrawalKind::Claim) {
            return Err(SynthesisError::Unsatisfiable);
        }
        let claim = witness.map(|value| &value.claim);
        let alloc_field = |value: Option<Fq>| {
            FpVar::new_witness(cs.clone(), || {
                value.ok_or(SynthesisError::AssignmentMissing)
            })
        };
        let alloc_context = |value: Option<[Fq; 2]>| {
            (0..2)
                .map(|index| alloc_field(value.map(|fields| fields[index])))
                .collect::<Result<Vec<_>, _>>()
        };

        let market = alloc_context(claim.map(|value| value.market))?;
        let auction = alloc_context(claim.map(|value| value.auction))?;
        let order = alloc_context(claim.map(|value| value.order))?;
        let active_order = alloc_context(witness.map(|value| value.active_order))?;
        let owner = alloc_context(claim.map(|value| value.owner))?;
        let key_epoch = alloc_context(claim.map(|value| value.key_epoch))?;
        let (_, nonce) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.client_order_nonce)),
            64,
        )?;
        let (_, sequence) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.sequence)),
            64,
        )?;
        let (_, allocation_version) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.allocation_rule_version)),
            16,
        )?;
        let (_, order_version) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.order_circuit_version)),
            16,
        )?;
        let (_, claim_version) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.claim_circuit_version)),
            16,
        )?;
        allocation_version.enforce_equal(&FpVar::constant(Fq::from(2u64)))?;
        order_version.enforce_equal(&FpVar::constant(Fq::from(2u64)))?;
        claim_version.enforce_equal(&FpVar::constant(Fq::from(2u64)))?;

        let mut buy = Vec::with_capacity(LEVELS);
        let mut sell = Vec::with_capacity(LEVELS);
        for index in 0..LEVELS {
            let (_, buy_value) = alloc_uint(
                cs.clone(),
                claim.map(|value| u128::from(value.buy[index])),
                QUANTITY_BITS,
            )?;
            let (_, sell_value) = alloc_uint(
                cs.clone(),
                claim.map(|value| u128::from(value.sell[index])),
                QUANTITY_BITS,
            )?;
            buy.push(buy_value);
            sell.push(sell_value);
        }
        let (_, base_reserve) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.base_reserve)),
            BALANCE_BITS,
        )?;
        let (_, quote_reserve) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.quote_reserve)),
            BALANCE_BITS,
        )?;
        let salt = alloc_field(claim.map(|value| value.order_salt))?;
        let mut order_payload = vec![FpVar::constant(Fq::from_le_bytes_mod_order(
            ORDER_COMMITMENT_DOMAIN,
        ))];
        order_payload.extend(auction.iter().cloned());
        order_payload.extend(owner.iter().cloned());
        order_payload.push(nonce.clone());
        for index in 0..LEVELS {
            order_payload.push(buy[index].clone());
            order_payload.push(sell[index].clone());
        }
        order_payload.push(base_reserve.clone());
        order_payload.push(quote_reserve.clone());
        order_payload.push(salt.clone());
        let order_commitment = transcript_hash_gadget(&order_payload)?;

        let (_, clearing_index) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.clearing_index)),
            5,
        )?;
        let (_, price) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.clearing_price)),
            64,
        )?;
        let (_, demand) = alloc_uint(cs.clone(), claim.map(|value| u128::from(value.demand)), 64)?;
        let (_, supply) = alloc_uint(cs.clone(), claim.map(|value| u128::from(value.supply)), 64)?;
        let (_, volume) = alloc_uint(
            cs.clone(),
            claim.map(|value| u128::from(value.clearing_volume)),
            64,
        )?;
        let selectors = (0..LEVELS)
            .map(|index| {
                Boolean::new_witness(cs.clone(), || {
                    claim
                        .map(|value| usize::from(value.clearing_index) == index)
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut selected_buy = FpVar::zero();
        let mut selected_sell = FpVar::zero();
        let mut selected_index = FpVar::zero();
        let mut selector_sum = FpVar::zero();
        for index in 0..LEVELS {
            let selector = FpVar::from(selectors[index].clone());
            selector_sum += &selector;
            selected_index += &selector * Fq::from(index as u64);
            selected_buy += &selector * &buy[index];
            selected_sell += &selector * &sell[index];
        }
        selector_sum.enforce_equal(&FpVar::one())?;
        selected_index.enforce_equal(&clearing_index)?;

        let (base_payout_value, quote_payout_value) =
            claim.map(expected_payouts).transpose()?.unwrap_or((0, 0));
        let (_, base_payout) = alloc_uint(
            cs.clone(),
            claim.map(|_| u128::from(base_payout_value)),
            BALANCE_BITS,
        )?;
        let (_, quote_payout) = alloc_uint(
            cs.clone(),
            claim.map(|_| u128::from(quote_payout_value)),
            BALANCE_BITS,
        )?;
        enforce_claim_allocation(
            cs.clone(),
            claim,
            &selected_buy,
            &selected_sell,
            &demand,
            &supply,
            &price,
            &base_reserve,
            &quote_reserve,
            &base_payout,
            &quote_payout,
        )?;

        let mut nullifier_payload = vec![FpVar::constant(Fq::from_le_bytes_mod_order(
            NULLIFIER_DOMAIN,
        ))];
        nullifier_payload.extend(auction.iter().cloned());
        nullifier_payload.extend(owner.iter().cloned());
        nullifier_payload.push(nonce.clone());
        nullifier_payload.push(order_commitment.clone());
        nullifier_payload.push(salt);
        let nullifier = transcript_hash_gadget(&nullifier_payload)?;

        let (_, revision) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.balance_revision)),
            64,
        )?;
        let old_base_value = witness.map(|value| u128::from(value.old_base_balance));
        let old_quote_value = witness.map(|value| u128::from(value.old_quote_balance));
        let credited_base_value = witness.and_then(|value| {
            value
                .old_base_balance
                .checked_add(base_payout_value)
                .map(u128::from)
        });
        let credited_quote_value = witness.and_then(|value| {
            value
                .old_quote_balance
                .checked_add(quote_payout_value)
                .map(u128::from)
        });
        let (old_base_bits, old_base) = alloc_uint(cs.clone(), old_base_value, BALANCE_BITS)?;
        let (old_quote_bits, old_quote) = alloc_uint(cs.clone(), old_quote_value, BALANCE_BITS)?;
        let (credited_base_bits, credited_base) =
            alloc_uint(cs.clone(), credited_base_value, BALANCE_BITS)?;
        let (credited_quote_bits, credited_quote) =
            alloc_uint(cs.clone(), credited_quote_value, BALANCE_BITS)?;
        (&old_base + &base_payout).enforce_equal(&credited_base)?;
        (&old_quote + &quote_payout).enforce_equal(&credited_quote)?;

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            witness
                .map(|value| value.committee_key)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let randomness = witness.map(|value| &value.balance_randomness);
        let messages = [
            old_base_bits,
            old_quote_bits,
            credited_base_bits,
            credited_quote_bits,
        ];
        let mut ciphertexts = Vec::with_capacity(4);
        for (index, message) in messages.iter().enumerate() {
            let randomness_bits = randomness.map(|values| {
                let bigint = values[index].into_bigint();
                (0..RANDOMNESS_BITS)
                    .map(|bit| bigint.get_bit(bit))
                    .collect::<Vec<_>>()
            });
            ciphertexts.push(encrypt_gadget(
                cs.clone(),
                message,
                randomness_bits,
                &committee_key,
            )?);
        }

        let mut statement = vec![
            FpVar::constant(Fq::from_le_bytes_mod_order(ROLLOVER_DOMAIN)),
            FpVar::constant(Fq::from(2u64)),
        ];
        statement.extend(market);
        statement.extend(auction);
        statement.extend(order);
        statement.extend(active_order);
        statement.extend(owner);
        statement.extend(key_epoch);
        statement.extend([
            nonce,
            sequence,
            allocation_version,
            order_version,
            claim_version,
            order_commitment,
            clearing_index,
            price,
            demand,
            supply,
            volume,
            nullifier,
            revision,
        ]);
        append_wire_point_gadget(&mut statement, &committee_key)?;
        for (ephemeral, body) in ciphertexts {
            append_wire_point_gadget(&mut statement, &ephemeral)?;
            append_wire_point_gadget(&mut statement, &body)?;
        }
        let expected =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&statement)?.enforce_equal(&expected)
    }
}

fn append_wire_point_native(payload: &mut Vec<Fq>, point: EdwardsAffine) {
    payload.push(point.y);
    payload.push(Fq::from(u64::from(point.x.into_bigint().get_bit(0))));
}

fn append_wire_point_gadget(
    payload: &mut Vec<FpVar<Fq>>,
    point: &EdwardsVar,
) -> Result<(), SynthesisError> {
    let x_bits = point.x.to_bits_le()?;
    payload.push(point.y.clone());
    payload.push(x_bits[0].clone().into());
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SolanaRolloverFixture {
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub market: [u8; 32],
    pub auction: [u8; 32],
    pub order: [u8; 32],
    pub active_order: [u8; 32],
    pub owner: [u8; 32],
    pub key_epoch: [u8; 32],
    pub client_order_nonce: u64,
    pub sequence: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub order_commitment: [u8; 32],
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub demand: u64,
    pub supply: u64,
    pub clearing_volume: u64,
    pub nullifier: [u8; 32],
    pub balance_revision: u64,
    pub committee_key: [u8; 32],
    pub balance_ciphertexts: [[u8; 64]; 4],
}

#[derive(Clone, Debug)]
pub struct RolloverProofRequest {
    pub market: [u8; 32],
    pub auction: [u8; 32],
    pub order: [u8; 32],
    pub active_order: [u8; 32],
    pub owner: [u8; 32],
    pub key_epoch: [u8; 32],
    pub client_order_nonce: u64,
    pub sequence: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub buy: [u64; LEVELS],
    pub sell: [u64; LEVELS],
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub order_salt: [u8; 32],
    pub expected_order_commitment: [u8; 32],
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub demand: u64,
    pub supply: u64,
    pub clearing_volume: u64,
    pub balance_revision: u64,
    pub committee_key: [u8; 32],
    pub old_balance: PrivateBalanceOpening,
}

#[derive(Clone, Debug)]
pub struct RolloverProofOutput {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub terminal_nullifier: [u8; 32],
    pub credited_encrypted_base: [u8; 64],
    pub credited_encrypted_quote: [u8; 64],
    pub credited_balance: PrivateBalanceOpening,
    pub base_payout: u64,
    pub quote_payout: u64,
}

pub struct RolloverProver {
    proving_key: ProvingKey<Bn254>,
}

impl RolloverProver {
    /// Load ceremony or environment-specific parameters outside the browser
    /// setup path. Compressed Arkworks serialization is validated strictly.
    pub fn from_compressed_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_proving_key(bytes)?,
        })
    }

    pub fn to_compressed_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        serialize_proving_key(&self.proving_key)
    }

    pub fn from_uncompressed_bytes(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_uncompressed_proving_key(bytes)?,
        })
    }

    /// Loads an uncompressed proving key whose bytes were authenticated by the caller.
    pub fn from_trusted_uncompressed_bytes(
        bytes: &[u8],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            proving_key: deserialize_trusted_uncompressed_proving_key(bytes)?,
        })
    }

    pub fn to_uncompressed_bytes(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        serialize_uncompressed_proving_key(&self.proving_key)
    }

    /// Deterministic setup for Devnet testing only. Production must replace
    /// this with ceremony-generated parameters loaded from an artifact.
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        let mut setup_rng = ChaCha20Rng::from_seed([0xf1; 32]);
        Ok(Self {
            proving_key: setup(&mut setup_rng)?,
        })
    }

    pub fn verification_key(&self) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        let key = &self.proving_key.vk;
        let ic = key
            .gamma_abc_g1
            .iter()
            .copied()
            .map(g1_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| std::io::Error::other("rollover key must have two IC points"))?;
        Ok(SolanaVerificationKey {
            alpha_g1: g1_bytes(key.alpha_g1),
            beta_g2: g2_bytes(key.beta_g2),
            gamma_g2: g2_bytes(key.gamma_g2),
            delta_g2: g2_bytes(key.delta_g2),
            ic,
        })
    }

    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        request: RolloverProofRequest,
        rng: &mut R,
    ) -> Result<RolloverProofOutput, Box<dyn std::error::Error>> {
        let mut claim = PrivateWithdrawalWitness {
            kind: PrivateWithdrawalKind::Claim,
            market: pubkey_context(request.market),
            auction: pubkey_context(request.auction),
            order: pubkey_context(request.order),
            owner: pubkey_context(request.owner),
            key_epoch: pubkey_context(request.key_epoch),
            client_order_nonce: request.client_order_nonce,
            sequence: request.sequence,
            allocation_rule_version: request.allocation_rule_version,
            order_circuit_version: request.order_circuit_version,
            claim_circuit_version: request.claim_circuit_version,
            buy: request.buy,
            sell: request.sell,
            base_reserve: request.base_reserve,
            quote_reserve: request.quote_reserve,
            order_salt: field_from_bytes_le(request.order_salt)?,
            clearing_index: request.clearing_index,
            clearing_price: request.clearing_price,
            demand: request.demand,
            supply: request.supply,
            clearing_volume: request.clearing_volume,
            opaque_withdrawal_fields: [Fq::zero(); OPAQUE_WITHDRAWAL_FIELDS],
            bridge: RistrettoBridgeWitness::for_amounts(0, 0),
        };
        if field_bytes_le(claim.order_commitment()) != request.expected_order_commitment {
            return Err(
                std::io::Error::other("private order does not match its commitment").into(),
            );
        }
        let (base_payout, quote_payout) = expected_payouts(&claim)
            .map_err(|_| std::io::Error::other("invalid private allocation"))?;
        claim.bridge = RistrettoBridgeWitness::for_amounts(base_payout, quote_payout);
        let credited_base_value = request
            .old_balance
            .base_value
            .checked_add(base_payout)
            .ok_or_else(|| std::io::Error::other("credited base balance overflow"))?;
        let credited_quote_value = request
            .old_balance
            .quote_value
            .checked_add(quote_payout)
            .ok_or_else(|| std::io::Error::other("credited quote balance overflow"))?;
        let credited_base_randomness = JubjubScalar::rand(rng);
        let credited_quote_randomness = JubjubScalar::rand(rng);
        let witness = RolloverSettlementWitness {
            claim,
            active_order: pubkey_context(request.active_order),
            balance_revision: request.balance_revision,
            old_base_balance: request.old_balance.base_value,
            old_quote_balance: request.old_balance.quote_value,
            committee_key: decode_wire_point(request.committee_key)?.into_group(),
            balance_randomness: [
                scalar_from_bytes(request.old_balance.base_randomness)?,
                scalar_from_bytes(request.old_balance.quote_randomness)?,
                credited_base_randomness,
                credited_quote_randomness,
            ],
        };
        let digest = witness
            .digest()
            .map_err(|_| std::io::Error::other("invalid rollover statement"))?;
        let terminal_nullifier = field_bytes_le(witness.nullifier());
        let balance_ciphertexts = witness
            .balance_ciphertexts()
            .map_err(|_| std::io::Error::other("invalid rollover balances"))?
            .map(ciphertext_bytes);
        let proof = Groth16::<Bn254>::prove(
            &self.proving_key,
            RolloverSettlementCircuit::from_witness(witness)?,
            rng,
        )?;
        let prepared_key = Groth16::<Bn254>::process_vk(&self.proving_key.vk)?;
        if !Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)? {
            return Err(std::io::Error::other("generated rollover proof did not verify").into());
        }
        Ok(RolloverProofOutput {
            proof_a: g1_bytes(proof.a.neg()),
            proof_b: g2_bytes(proof.b),
            proof_c: g1_bytes(proof.c),
            public_input: field_bytes(digest),
            terminal_nullifier,
            credited_encrypted_base: balance_ciphertexts[2],
            credited_encrypted_quote: balance_ciphertexts[3],
            credited_balance: PrivateBalanceOpening {
                base_value: credited_base_value,
                quote_value: credited_quote_value,
                base_randomness: scalar_bytes(credited_base_randomness),
                quote_randomness: scalar_bytes(credited_quote_randomness),
            },
            base_payout,
            quote_payout,
        })
    }
}

pub struct MarketMakerRolloverProver {
    rollover: RolloverProver,
    order: OrderProver,
}

#[derive(Clone, Debug)]
pub struct MarketMakerRolloverProofOutput {
    pub settlement: RolloverProofOutput,
    pub next_order: OrderProofOutput,
}

impl MarketMakerRolloverProver {
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            rollover: RolloverProver::deterministic_devnet()?,
            order: OrderProver::deterministic_devnet()?,
        })
    }

    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        settlement_request: RolloverProofRequest,
        mut next_order_request: OrderProofRequest,
        rng: &mut R,
    ) -> Result<MarketMakerRolloverProofOutput, Box<dyn std::error::Error>> {
        if settlement_request.owner != next_order_request.owner
            || settlement_request.committee_key != next_order_request.committee_key
        {
            return Err(std::io::Error::other(
                "settlement and next order must share owner and committee key",
            )
            .into());
        }
        let settlement = self.rollover.prove(settlement_request, rng)?;
        next_order_request.old_balance = settlement.credited_balance.clone();
        let next_order = self.order.prove(next_order_request, rng)?;
        Ok(MarketMakerRolloverProofOutput {
            settlement,
            next_order,
        })
    }

    pub fn rollover_verification_key(
        &self,
    ) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        self.rollover.verification_key()
    }

    pub fn order_verification_key(
        &self,
    ) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        self.order.verification_key()
    }
}

pub fn setup<R: RngCore + CryptoRng>(
    rng: &mut R,
) -> Result<ProvingKey<Bn254>, Box<dyn std::error::Error>> {
    let (proving_key, _) =
        Groth16::<Bn254>::circuit_specific_setup(RolloverSettlementCircuit::blank(), rng)?;
    Ok(proving_key)
}

pub fn prove<R: RngCore + CryptoRng>(
    proving_key: &ProvingKey<Bn254>,
    witness: RolloverSettlementWitness,
    rng: &mut R,
) -> Result<SolanaProofBytes, Box<dyn std::error::Error>> {
    let digest = witness.digest()?;
    let proof = Groth16::<Bn254>::prove(
        proving_key,
        RolloverSettlementCircuit::from_witness(witness)?,
        rng,
    )?;
    Ok((
        g1_bytes(proof.a.neg()),
        g2_bytes(proof.b),
        g1_bytes(proof.c),
        field_bytes(digest),
    ))
}

pub fn deterministic_solana_fixture() -> Result<SolanaRolloverFixture, Box<dyn std::error::Error>> {
    let witness = RolloverSettlementWitness::deterministic();
    let order_commitment = field_bytes_le(witness.order_commitment());
    let nullifier = field_bytes_le(witness.nullifier());
    let balance_ciphertexts = witness.balance_ciphertexts()?.map(ciphertext_bytes);
    let claim = &witness.claim;
    let market = context_pubkey(claim.market);
    let auction = context_pubkey(claim.auction);
    let order = context_pubkey(claim.order);
    let active_order = context_pubkey(witness.active_order);
    let owner = context_pubkey(claim.owner);
    let key_epoch = context_pubkey(claim.key_epoch);
    let client_order_nonce = claim.client_order_nonce;
    let sequence = claim.sequence;
    let allocation_rule_version = claim.allocation_rule_version;
    let order_circuit_version = claim.order_circuit_version;
    let claim_circuit_version = claim.claim_circuit_version;
    let clearing_index = claim.clearing_index;
    let clearing_price = claim.clearing_price;
    let demand = claim.demand;
    let supply = claim.supply;
    let clearing_volume = claim.clearing_volume;
    let balance_revision = witness.balance_revision;
    let committee_key = wire_point_bytes(witness.committee_key.into_affine());
    let mut setup_rng = ChaCha20Rng::from_seed([0xf1; 32]);
    let proving_key = setup(&mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0xf2; 32]);
    let (proof_a, proof_b, proof_c, public_input) = prove(&proving_key, witness, &mut proof_rng)?;
    let VerifyingKey {
        alpha_g1,
        beta_g2,
        gamma_g2,
        delta_g2,
        gamma_abc_g1,
    } = proving_key.vk;
    let vk_ic = gamma_abc_g1
        .into_iter()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "rollover key must have two IC points")?;
    Ok(SolanaRolloverFixture {
        vk_alpha_g1: g1_bytes(alpha_g1),
        vk_beta_g2: g2_bytes(beta_g2),
        vk_gamma_g2: g2_bytes(gamma_g2),
        vk_delta_g2: g2_bytes(delta_g2),
        vk_ic,
        proof_a,
        proof_b,
        proof_c,
        public_input,
        market,
        auction,
        order,
        active_order,
        owner,
        key_epoch,
        client_order_nonce,
        sequence,
        allocation_rule_version,
        order_circuit_version,
        claim_circuit_version,
        order_commitment,
        clearing_index,
        clearing_price,
        demand,
        supply,
        clearing_volume,
        nullifier,
        balance_revision,
        committee_key,
        balance_ciphertexts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn settled_payout_credits_encrypted_balance() {
        let witness = RolloverSettlementWitness::deterministic();
        let cs = ConstraintSystem::<Fq>::new_ref();
        RolloverSettlementCircuit::from_witness(witness)
            .unwrap()
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a full rollover Groth16 proof; run in release mode"]
    fn arbitrary_rollover_prover_generates_a_verified_transition() {
        let witness = RolloverSettlementWitness::deterministic();
        let claim = &witness.claim;
        let request = RolloverProofRequest {
            market: context_pubkey(claim.market),
            auction: context_pubkey(claim.auction),
            order: context_pubkey(claim.order),
            active_order: context_pubkey(witness.active_order),
            owner: context_pubkey(claim.owner),
            key_epoch: context_pubkey(claim.key_epoch),
            client_order_nonce: claim.client_order_nonce,
            sequence: claim.sequence,
            allocation_rule_version: claim.allocation_rule_version,
            order_circuit_version: claim.order_circuit_version,
            claim_circuit_version: claim.claim_circuit_version,
            buy: claim.buy,
            sell: claim.sell,
            base_reserve: claim.base_reserve,
            quote_reserve: claim.quote_reserve,
            order_salt: field_bytes_le(claim.order_salt),
            expected_order_commitment: field_bytes_le(claim.order_commitment()),
            clearing_index: claim.clearing_index,
            clearing_price: claim.clearing_price,
            demand: claim.demand,
            supply: claim.supply,
            clearing_volume: claim.clearing_volume,
            balance_revision: witness.balance_revision,
            committee_key: wire_point_bytes(witness.committee_key.into_affine()),
            old_balance: PrivateBalanceOpening {
                base_value: witness.old_base_balance,
                quote_value: witness.old_quote_balance,
                base_randomness: scalar_bytes(witness.balance_randomness[0]),
                quote_randomness: scalar_bytes(witness.balance_randomness[1]),
            },
        };
        let (base_payout, quote_payout) = witness.payouts().unwrap();
        let prover = RolloverProver::deterministic_devnet().unwrap();
        let mut rng = ChaCha20Rng::from_seed([0xfc; 32]);
        let output = prover.prove(request, &mut rng).unwrap();
        assert_eq!(output.base_payout, base_payout);
        assert_eq!(output.quote_payout, quote_payout);
        assert_eq!(
            output.credited_balance.base_value,
            witness.old_base_balance + base_payout
        );
        assert_eq!(
            output.credited_balance.quote_value,
            witness.old_quote_balance + quote_payout
        );
        assert_ne!(output.terminal_nullifier, [0; 32]);
    }

    #[test]
    fn changed_clearing_result_invalidates_original_statement() {
        let witness = RolloverSettlementWitness::deterministic();
        let original_digest = witness.digest().unwrap();
        let mut changed = witness;
        changed.claim.clearing_price += 1;
        let cs = ConstraintSystem::<Fq>::new_ref();
        RolloverSettlementCircuit {
            witness: Some(changed),
            digest: Some(original_digest),
        }
        .generate_constraints(cs.clone())
        .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    #[ignore = "generates a circuit-specific setup and measures release proving"]
    fn benchmark_recurring_rollover_proof() {
        let witness = RolloverSettlementWitness::deterministic();
        let cs = ConstraintSystem::<Fq>::new_ref();
        RolloverSettlementCircuit::from_witness(witness.clone())
            .unwrap()
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
        let constraints = cs.num_constraints();

        let mut setup_rng = ChaCha20Rng::from_seed([0xfa; 32]);
        let setup_started = Instant::now();
        let proving_key = setup(&mut setup_rng).unwrap();
        let setup_elapsed = setup_started.elapsed();

        let mut proof_rng = ChaCha20Rng::from_seed([0xfb; 32]);
        let proof_started = Instant::now();
        let _proof = prove(&proving_key, witness, &mut proof_rng).unwrap();
        let proof_elapsed = proof_started.elapsed();
        eprintln!(
            "rollover constraints={constraints}, setup_ms={}, recurring_proof_ms={}",
            setup_elapsed.as_millis(),
            proof_elapsed.as_millis()
        );
    }
}

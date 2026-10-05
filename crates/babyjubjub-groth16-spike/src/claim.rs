//! Private queued-order claim and cancellation-refund circuit.
//!
//! The circuit reopens the salted order commitment created by the submission
//! circuit, selects only the quantities at the public clearing index, applies
//! exact-index pro-rata allocation with adverse rounding on the long side, and
//! binds both private payouts to Token-2022 Ristretto commitments through one
//! batched Pedersen relation.

use super::{
    alloc_uint,
    ristretto_bridge::{enforce_batched_pedersen_relation, RistrettoBridgeWitness},
    transcript_hash_gadget, transcript_hash_native,
    transition::{field_bytes, g1_bytes, g2_bytes},
    Fq, LEVELS, QUANTITY_BITS,
};
use ark_bn254::Bn254;
use ark_ff::PrimeField;
use ark_groth16::Groth16;
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    prelude::ToBitsGadget,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use ark_snark::SNARK;
use rand::{CryptoRng, RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::ops::Neg;

const ORDER_COMMITMENT_DOMAIN: &[u8] = b"BeatEx private contingent order v1";
const CLAIM_DOMAIN: &[u8] = b"BeatEx private claim v1";
const REFUND_DOMAIN: &[u8] = b"BeatEx cancellation refund v1";
const NULLIFIER_DOMAIN: &[u8] = b"BeatEx withdrawal nullifier v1";
const OPAQUE_WITHDRAWAL_FIELDS: usize = 86;
const AMOUNT_BITS: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateWithdrawalKind {
    Claim,
    Refund,
}

#[derive(Clone, Debug)]
pub struct PrivateWithdrawalWitness {
    pub kind: PrivateWithdrawalKind,
    pub market: [Fq; 2],
    pub auction: [Fq; 2],
    pub order: [Fq; 2],
    pub owner: [Fq; 2],
    pub key_epoch: [Fq; 2],
    pub client_order_nonce: u64,
    pub sequence: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub buy: [u64; LEVELS],
    pub sell: [u64; LEVELS],
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub order_salt: Fq,
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub demand: u64,
    pub supply: u64,
    pub clearing_volume: u64,
    /// Six account keys, both extracted Token proof bindings and both 36-byte
    /// decryptable balances, already encoded as 128-bit transcript limbs.
    pub opaque_withdrawal_fields: [Fq; OPAQUE_WITHDRAWAL_FIELDS],
    pub bridge: RistrettoBridgeWitness,
}

impl PrivateWithdrawalWitness {
    pub fn deterministic(kind: PrivateWithdrawalKind) -> Self {
        let mut buy = [0u64; LEVELS];
        buy[LEVELS / 2] = 3;
        let sell = [0u64; LEVELS];
        let (base_payout, quote_payout) = match kind {
            // D=5, S=3: floor(3*3/5)=1 base and ceil(3*3*100/5)=180 quote.
            PrivateWithdrawalKind::Claim => (1, 120),
            PrivateWithdrawalKind::Refund => (0, 300),
        };
        Self {
            kind,
            market: [Fq::from(11u64), Fq::from(12u64)],
            auction: [Fq::from(21u64), Fq::from(22u64)],
            order: [Fq::from(31u64), Fq::from(32u64)],
            owner: [Fq::from(41u64), Fq::from(42u64)],
            key_epoch: [Fq::from(51u64), Fq::from(52u64)],
            client_order_nonce: 7,
            sequence: 3,
            allocation_rule_version: 1,
            order_circuit_version: 1,
            claim_circuit_version: 1,
            buy,
            sell,
            base_reserve: 0,
            quote_reserve: 300,
            order_salt: Fq::from(0x9137u64),
            clearing_index: (LEVELS / 2) as u8,
            clearing_price: 100,
            demand: 5,
            supply: 3,
            clearing_volume: 3,
            opaque_withdrawal_fields: std::array::from_fn(|index| Fq::from(index as u64 + 100)),
            bridge: RistrettoBridgeWitness::for_amounts(base_payout, quote_payout),
        }
    }

    pub fn order_commitment(&self) -> Fq {
        let mut payload = vec![Fq::from_le_bytes_mod_order(ORDER_COMMITMENT_DOMAIN)];
        payload.extend_from_slice(&self.auction);
        payload.extend_from_slice(&self.owner);
        payload.push(Fq::from(self.client_order_nonce));
        for index in 0..LEVELS {
            payload.push(Fq::from(self.buy[index]));
            payload.push(Fq::from(self.sell[index]));
        }
        payload.push(Fq::from(self.base_reserve));
        payload.push(Fq::from(self.quote_reserve));
        payload.push(self.order_salt);
        transcript_hash_native(&payload)
    }

    pub fn nullifier(&self) -> Fq {
        let mut payload = vec![Fq::from_le_bytes_mod_order(NULLIFIER_DOMAIN)];
        payload.extend_from_slice(&self.auction);
        payload.extend_from_slice(&self.owner);
        payload.push(Fq::from(self.client_order_nonce));
        payload.push(self.order_commitment());
        payload.push(self.order_salt);
        transcript_hash_native(&payload)
    }

    pub fn digest(&self) -> Fq {
        let mut payload = vec![
            Fq::from_le_bytes_mod_order(match self.kind {
                PrivateWithdrawalKind::Claim => CLAIM_DOMAIN,
                PrivateWithdrawalKind::Refund => REFUND_DOMAIN,
            }),
            Fq::from(1u64),
        ];
        payload.extend_from_slice(&self.market);
        payload.extend_from_slice(&self.auction);
        payload.extend_from_slice(&self.order);
        payload.extend_from_slice(&self.owner);
        payload.extend_from_slice(&self.key_epoch);
        payload.push(Fq::from(self.client_order_nonce));
        payload.push(Fq::from(self.sequence));
        payload.push(Fq::from(self.allocation_rule_version));
        payload.push(Fq::from(self.order_circuit_version));
        payload.push(Fq::from(self.claim_circuit_version));
        payload.push(self.order_commitment());
        payload.push(Fq::from(self.clearing_index));
        payload.push(Fq::from(self.clearing_price));
        payload.push(Fq::from(self.demand));
        payload.push(Fq::from(self.supply));
        payload.push(Fq::from(self.clearing_volume));
        payload.push(self.nullifier());
        payload.extend_from_slice(&self.opaque_withdrawal_fields);
        append_packed(&mut payload, self.bridge.base_commitment);
        append_packed(&mut payload, self.bridge.quote_commitment);
        append_packed(&mut payload, self.bridge.batched_commitment);
        transcript_hash_native(&payload)
    }
}

#[derive(Clone)]
pub struct PrivateWithdrawalCircuit {
    pub witness: Option<PrivateWithdrawalWitness>,
    pub kind: PrivateWithdrawalKind,
    pub digest: Option<Fq>,
}

#[derive(Clone, Debug)]
pub struct SolanaPrivateWithdrawalFixture {
    pub kind: PrivateWithdrawalKind,
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct SolanaPrivateWithdrawalProof {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct SolanaPrivateWithdrawalVerifyingKey {
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: [[u8; 64]; 2],
}

pub fn export_private_withdrawal_verifying_key(
    verifying_key: &ark_groth16::VerifyingKey<Bn254>,
) -> Result<SolanaPrivateWithdrawalVerifyingKey, Box<dyn std::error::Error>> {
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "private withdrawal key must have two IC points")?;
    Ok(SolanaPrivateWithdrawalVerifyingKey {
        alpha_g1: g1_bytes(verifying_key.alpha_g1),
        beta_g2: g2_bytes(verifying_key.beta_g2),
        gamma_g2: g2_bytes(verifying_key.gamma_g2),
        delta_g2: g2_bytes(verifying_key.delta_g2),
        ic,
    })
}

pub fn prove_private_withdrawal<R: RngCore + CryptoRng>(
    proving_key: &ark_groth16::ProvingKey<Bn254>,
    witness: PrivateWithdrawalWitness,
    rng: &mut R,
) -> Result<SolanaPrivateWithdrawalProof, Box<dyn std::error::Error>> {
    let digest = witness.digest();
    let proof = Groth16::<Bn254>::prove(
        proving_key,
        PrivateWithdrawalCircuit::from_witness(witness),
        rng,
    )?;
    if !Groth16::<Bn254>::verify(&proving_key.vk, &[digest], &proof)? {
        return Err("generated private-withdrawal proof failed host verification".into());
    }
    Ok(SolanaPrivateWithdrawalProof {
        proof_a: g1_bytes(proof.a.neg()),
        proof_b: g2_bytes(proof.b),
        proof_c: g1_bytes(proof.c),
        public_input: field_bytes(digest),
    })
}

/// Deterministic development-only setup and proof for the exact production
/// relation. Production deployments must install a separately generated and
/// audited ceremony key under the versioned verification-key PDA.
pub fn deterministic_private_withdrawal_solana_fixture(
    kind: PrivateWithdrawalKind,
) -> Result<SolanaPrivateWithdrawalFixture, Box<dyn std::error::Error>> {
    let witness = PrivateWithdrawalWitness::deterministic(kind);
    let digest = witness.digest();
    let setup_seed = match kind {
        PrivateWithdrawalKind::Claim => [0xc1; 32],
        PrivateWithdrawalKind::Refund => [0xc2; 32],
    };
    let proof_seed = match kind {
        PrivateWithdrawalKind::Claim => [0xd1; 32],
        PrivateWithdrawalKind::Refund => [0xd2; 32],
    };
    let mut setup_rng = ChaCha20Rng::from_seed(setup_seed);
    #[cfg(test)]
    let setup_start = std::time::Instant::now();
    let (proving_key, verifying_key) = Groth16::<Bn254>::circuit_specific_setup(
        PrivateWithdrawalCircuit::blank(kind),
        &mut setup_rng,
    )?;
    #[cfg(test)]
    let setup_time = setup_start.elapsed();
    let mut proof_rng = ChaCha20Rng::from_seed(proof_seed);
    #[cfg(test)]
    let prove_start = std::time::Instant::now();
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        PrivateWithdrawalCircuit::from_witness(witness),
        &mut proof_rng,
    )?;
    #[cfg(test)]
    let prove_time = prove_start.elapsed();
    #[cfg(test)]
    let verify_start = std::time::Instant::now();
    if !Groth16::<Bn254>::verify(&verifying_key, &[digest], &proof)? {
        return Err("private withdrawal fixture failed host verification".into());
    }
    #[cfg(test)]
    eprintln!(
        "private withdrawal setup_ms={:.3} prove_ms={:.3} verify_ms={:.3}",
        setup_time.as_secs_f64() * 1_000.0,
        prove_time.as_secs_f64() * 1_000.0,
        verify_start.elapsed().as_secs_f64() * 1_000.0,
    );
    let vk_ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "private withdrawal key must have two IC points")?;
    Ok(SolanaPrivateWithdrawalFixture {
        kind,
        vk_alpha_g1: g1_bytes(verifying_key.alpha_g1),
        vk_beta_g2: g2_bytes(verifying_key.beta_g2),
        vk_gamma_g2: g2_bytes(verifying_key.gamma_g2),
        vk_delta_g2: g2_bytes(verifying_key.delta_g2),
        vk_ic,
        proof_a: g1_bytes(proof.a.neg()),
        proof_b: g2_bytes(proof.b),
        proof_c: g1_bytes(proof.c),
        public_input: field_bytes(digest),
    })
}

impl PrivateWithdrawalCircuit {
    pub const fn blank(kind: PrivateWithdrawalKind) -> Self {
        Self {
            witness: None,
            kind,
            digest: None,
        }
    }

    pub fn from_witness(witness: PrivateWithdrawalWitness) -> Self {
        let kind = witness.kind;
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            kind,
            digest,
        }
    }
}

impl ConstraintSynthesizer<Fq> for PrivateWithdrawalCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        if witness.is_some_and(|value| value.kind != self.kind) {
            return Err(SynthesisError::Unsatisfiable);
        }
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
        let market = alloc_context(witness.map(|value| value.market))?;
        let auction = alloc_context(witness.map(|value| value.auction))?;
        let order = alloc_context(witness.map(|value| value.order))?;
        let owner = alloc_context(witness.map(|value| value.owner))?;
        let key_epoch = alloc_context(witness.map(|value| value.key_epoch))?;
        let (_, nonce) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.client_order_nonce)),
            64,
        )?;
        let (_, sequence) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.sequence)),
            64,
        )?;
        let (_, allocation_version) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.allocation_rule_version)),
            16,
        )?;
        let (_, order_version) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.order_circuit_version)),
            16,
        )?;
        let (_, claim_version) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.claim_circuit_version)),
            16,
        )?;
        allocation_version.enforce_equal(&FpVar::constant(Fq::from(1u64)))?;
        order_version.enforce_equal(&FpVar::constant(Fq::from(1u64)))?;
        claim_version.enforce_equal(&FpVar::constant(Fq::from(1u64)))?;

        let mut buy = Vec::with_capacity(LEVELS);
        let mut sell = Vec::with_capacity(LEVELS);
        for index in 0..LEVELS {
            let (_, buy_value) = alloc_uint(
                cs.clone(),
                witness.map(|value| u128::from(value.buy[index])),
                QUANTITY_BITS,
            )?;
            let (_, sell_value) = alloc_uint(
                cs.clone(),
                witness.map(|value| u128::from(value.sell[index])),
                QUANTITY_BITS,
            )?;
            buy.push(buy_value);
            sell.push(sell_value);
        }
        let (_, base_reserve) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.base_reserve)),
            64,
        )?;
        let (_, quote_reserve) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.quote_reserve)),
            64,
        )?;
        let salt = alloc_field(witness.map(|value| value.order_salt))?;
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
            witness.map(|value| u128::from(value.clearing_index)),
            5,
        )?;
        let (_, price) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.clearing_price)),
            64,
        )?;
        let (_, demand) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.demand)),
            64,
        )?;
        let (_, supply) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.supply)),
            64,
        )?;
        let (_, volume) = alloc_uint(
            cs.clone(),
            witness.map(|value| u128::from(value.clearing_volume)),
            64,
        )?;
        let selectors = (0..LEVELS)
            .map(|index| {
                Boolean::new_witness(cs.clone(), || {
                    witness
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
            witness.map(expected_payouts).transpose()?.unwrap_or((0, 0));
        let (_, base_payout) = alloc_uint(
            cs.clone(),
            witness.map(|_| u128::from(base_payout_value)),
            AMOUNT_BITS,
        )?;
        let (_, quote_payout) = alloc_uint(
            cs.clone(),
            witness.map(|_| u128::from(quote_payout_value)),
            AMOUNT_BITS,
        )?;
        match self.kind {
            PrivateWithdrawalKind::Claim => enforce_claim_allocation(
                cs.clone(),
                witness,
                &selected_buy,
                &selected_sell,
                &demand,
                &supply,
                &price,
                &base_reserve,
                &quote_reserve,
                &base_payout,
                &quote_payout,
            )?,
            PrivateWithdrawalKind::Refund => {
                base_payout.enforce_equal(&base_reserve)?;
                quote_payout.enforce_equal(&quote_reserve)?;
            }
        }

        let mut nullifier_payload = vec![FpVar::constant(Fq::from_le_bytes_mod_order(
            NULLIFIER_DOMAIN,
        ))];
        nullifier_payload.extend(auction.iter().cloned());
        nullifier_payload.extend(owner.iter().cloned());
        nullifier_payload.push(nonce.clone());
        nullifier_payload.push(order_commitment.clone());
        nullifier_payload.push(salt);
        let nullifier = transcript_hash_gadget(&nullifier_payload)?;

        let opaque = (0..OPAQUE_WITHDRAWAL_FIELDS)
            .map(|index| alloc_field(witness.map(|value| value.opaque_withdrawal_fields[index])))
            .collect::<Result<Vec<_>, _>>()?;
        let base_fields = alloc_packed_point(
            cs.clone(),
            witness.map(|value| value.bridge.base_commitment),
        )?;
        let quote_fields = alloc_packed_point(
            cs.clone(),
            witness.map(|value| value.bridge.quote_commitment),
        )?;
        let batched_fields = alloc_packed_point(
            cs.clone(),
            witness.map(|value| value.bridge.batched_commitment),
        )?;
        enforce_batched_pedersen_relation(
            cs.clone(),
            witness.map(|_| base_payout_value),
            witness.map(|_| quote_payout_value),
            witness.map(|value| value.bridge.batched_opening),
            witness.map(|value| value.bridge.batched_commitment),
            &base_fields,
            &quote_fields,
            &batched_fields,
        )?;

        let mut statement = vec![
            FpVar::constant(Fq::from_le_bytes_mod_order(match self.kind {
                PrivateWithdrawalKind::Claim => CLAIM_DOMAIN,
                PrivateWithdrawalKind::Refund => REFUND_DOMAIN,
            })),
            FpVar::constant(Fq::from(1u64)),
        ];
        statement.extend(market);
        statement.extend(auction);
        statement.extend(order);
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
        ]);
        statement.extend(opaque);
        statement.extend(base_fields);
        statement.extend(quote_fields);
        statement.extend(batched_fields);
        let expected =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&statement)?.enforce_equal(&expected)
    }
}

#[allow(clippy::too_many_arguments)]
fn enforce_claim_allocation(
    cs: ConstraintSystemRef<Fq>,
    witness: Option<&PrivateWithdrawalWitness>,
    buy: &FpVar<Fq>,
    sell: &FpVar<Fq>,
    demand: &FpVar<Fq>,
    supply: &FpVar<Fq>,
    price: &FpVar<Fq>,
    base_reserve: &FpVar<Fq>,
    quote_reserve: &FpVar<Fq>,
    base_payout: &FpVar<Fq>,
    quote_payout: &FpVar<Fq>,
) -> Result<(), SynthesisError> {
    let relation = witness.map(|value| value.demand.cmp(&value.supply));
    let demand_gt = Boolean::new_witness(cs.clone(), || {
        relation
            .map(|v| v.is_gt())
            .ok_or(SynthesisError::AssignmentMissing)
    })?;
    let supply_gt = Boolean::new_witness(cs.clone(), || {
        relation
            .map(|v| v.is_lt())
            .ok_or(SynthesisError::AssignmentMissing)
    })?;
    let equal = Boolean::new_witness(cs.clone(), || {
        relation
            .map(|v| v.is_eq())
            .ok_or(SynthesisError::AssignmentMissing)
    })?;
    let dg = FpVar::from(demand_gt);
    let sg = FpVar::from(supply_gt);
    let eq = FpVar::from(equal);
    (&dg + &sg + &eq).enforce_equal(&FpVar::one())?;
    let delta_value = witness.map(|value| value.demand.abs_diff(value.supply));
    let (_, delta) = alloc_uint(cs.clone(), delta_value.map(u128::from), 64)?;
    (demand - supply).enforce_equal(&((&dg - &sg) * &delta))?;
    (&eq * &delta).enforce_equal(&FpVar::zero())?;
    let delta_minus_one_value = delta_value.map(|value| value.saturating_sub(1));
    let (_, delta_minus_one) = alloc_uint(cs.clone(), delta_minus_one_value.map(u128::from), 64)?;
    gated_eq(&(&dg + &sg), &delta, &(&delta_minus_one + Fq::from(1u64)))?;

    let demand_slack_value = witness.and_then(|value| {
        let index = usize::from(value.clearing_index);
        value
            .buy
            .get(index)
            .and_then(|buy| value.demand.checked_sub(*buy))
    });
    let supply_slack_value = witness.and_then(|value| {
        let index = usize::from(value.clearing_index);
        value
            .sell
            .get(index)
            .and_then(|sell| value.supply.checked_sub(*sell))
    });
    let (_, demand_slack) = alloc_uint(cs.clone(), demand_slack_value.map(u128::from), 64)?;
    let (_, supply_slack) = alloc_uint(cs.clone(), supply_slack_value.map(u128::from), 64)?;
    demand.enforce_equal(&(buy + demand_slack))?;
    supply.enforce_equal(&(sell + supply_slack))?;

    let outputs = witness
        .map(allocation_terms)
        .transpose()?
        .unwrap_or_default();
    let (_, base_debit) = alloc_uint(cs.clone(), Some(u128::from(outputs[0])), 64)?;
    let (_, base_credit) = alloc_uint(cs.clone(), Some(u128::from(outputs[1])), 64)?;
    let (_, quote_debit) = alloc_uint(cs.clone(), Some(u128::from(outputs[2])), 64)?;
    let (_, quote_credit) = alloc_uint(cs.clone(), Some(u128::from(outputs[3])), 64)?;
    let (_, rem_a) = alloc_uint(cs.clone(), Some(u128::from(outputs[4])), 64)?;
    let (_, slack_a) = alloc_uint(cs.clone(), Some(u128::from(outputs[5])), 64)?;
    let (_, rem_b) = alloc_uint(cs.clone(), Some(u128::from(outputs[6])), 64)?;
    let (_, slack_b) = alloc_uint(cs, Some(u128::from(outputs[7])), 64)?;

    let buy_num = buy * supply;
    gated_eq(&dg, &buy_num, &(&base_credit * demand + &rem_a))?;
    gated_eq(&dg, demand, &(&rem_a + &slack_a + Fq::from(1u64)))?;
    gated_eq(&dg, &(&quote_debit * demand), &(&buy_num * price + &rem_b))?;
    gated_eq(&dg, demand, &(&rem_b + &slack_b + Fq::from(1u64)))?;
    gated_eq(&dg, &base_debit, sell)?;
    gated_eq(&dg, &quote_credit, &(sell * price))?;

    let sell_num = sell * demand;
    gated_eq(&sg, &(&base_debit * supply), &(&sell_num + &rem_a))?;
    gated_eq(&sg, supply, &(&rem_a + &slack_a + Fq::from(1u64)))?;
    gated_eq(&sg, &base_credit, buy)?;
    gated_eq(&sg, &quote_debit, &(buy * price))?;
    gated_eq(
        &sg,
        &(&sell_num * price),
        &(&quote_credit * supply + &rem_b),
    )?;
    gated_eq(&sg, supply, &(&rem_b + &slack_b + Fq::from(1u64)))?;

    gated_eq(&eq, &base_debit, sell)?;
    gated_eq(&eq, &base_credit, buy)?;
    gated_eq(&eq, &quote_debit, &(buy * price))?;
    gated_eq(&eq, &quote_credit, &(sell * price))?;
    (base_reserve + &base_credit).enforce_equal(&(base_payout + &base_debit))?;
    (quote_reserve + &quote_credit).enforce_equal(&(quote_payout + &quote_debit))
}

fn gated_eq(gate: &FpVar<Fq>, left: &FpVar<Fq>, right: &FpVar<Fq>) -> Result<(), SynthesisError> {
    (gate * (left - right)).enforce_equal(&FpVar::zero())
}

fn expected_payouts(witness: &PrivateWithdrawalWitness) -> Result<(u64, u64), SynthesisError> {
    if witness.kind == PrivateWithdrawalKind::Refund {
        return Ok((witness.base_reserve, witness.quote_reserve));
    }
    let terms = allocation_terms(witness)?;
    let base = witness
        .base_reserve
        .checked_sub(terms[0])
        .and_then(|value| value.checked_add(terms[1]))
        .ok_or(SynthesisError::Unsatisfiable)?;
    let quote = witness
        .quote_reserve
        .checked_sub(terms[2])
        .and_then(|value| value.checked_add(terms[3]))
        .ok_or(SynthesisError::Unsatisfiable)?;
    if base >= 1_u64 << AMOUNT_BITS || quote >= 1_u64 << AMOUNT_BITS {
        return Err(SynthesisError::Unsatisfiable);
    }
    Ok((base, quote))
}

/// debit/credit values followed by the two division remainder/slack pairs.
fn allocation_terms(witness: &PrivateWithdrawalWitness) -> Result<[u64; 8], SynthesisError> {
    let index = usize::from(witness.clearing_index);
    if index >= LEVELS {
        return Err(SynthesisError::Unsatisfiable);
    }
    let buy = u128::from(witness.buy[index]);
    let sell = u128::from(witness.sell[index]);
    let demand = u128::from(witness.demand);
    let supply = u128::from(witness.supply);
    let price = u128::from(witness.clearing_price);
    if buy > demand || sell > supply {
        return Err(SynthesisError::Unsatisfiable);
    }
    let values = if demand > supply {
        let n = buy * supply;
        let base_credit = n / demand;
        let rem_a = n % demand;
        let quote_n = n * price;
        let quote_debit = quote_n.div_ceil(demand);
        let pad_b = quote_debit * demand - quote_n;
        [
            sell,
            base_credit,
            quote_debit,
            sell * price,
            rem_a,
            demand - rem_a - 1,
            pad_b,
            demand - pad_b - 1,
        ]
    } else if supply > demand {
        let n = sell * demand;
        let base_debit = n.div_ceil(supply);
        let pad_a = base_debit * supply - n;
        let quote_n = n * price;
        let quote_credit = quote_n / supply;
        let rem_b = quote_n % supply;
        [
            base_debit,
            buy,
            buy * price,
            quote_credit,
            pad_a,
            supply - pad_a - 1,
            rem_b,
            supply - rem_b - 1,
        ]
    } else {
        [sell, buy, buy * price, sell * price, 0, 0, 0, 0]
    };
    let mut narrowed = [0u64; 8];
    for (destination, value) in narrowed.iter_mut().zip(values) {
        *destination = u64::try_from(value).map_err(|_| SynthesisError::Unsatisfiable)?;
    }
    Ok(narrowed)
}

fn alloc_packed_point(
    cs: ConstraintSystemRef<Fq>,
    bytes: Option<[u8; 32]>,
) -> Result<Vec<FpVar<Fq>>, SynthesisError> {
    let values = bytes.map(|bytes| {
        [
            Fq::from_le_bytes_mod_order(&bytes[..16]),
            Fq::from_le_bytes_mod_order(&bytes[16..]),
        ]
    });
    (0..2)
        .map(|index| {
            let field = FpVar::new_witness(cs.clone(), || {
                values
                    .map(|values| values[index])
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            for bit in field.to_bits_le()?.iter().skip(128) {
                bit.enforce_equal(&Boolean::FALSE)?;
            }
            Ok(field)
        })
        .collect()
}

fn append_packed(payload: &mut Vec<Fq>, bytes: [u8; 32]) {
    payload.push(Fq::from_le_bytes_mod_order(&bytes[..16]));
    payload.push(Fq::from_le_bytes_mod_order(&bytes[16..]));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;
    use std::time::Instant;

    fn satisfied(witness: PrivateWithdrawalWitness) -> bool {
        let cs = ConstraintSystem::<Fq>::new_ref();
        PrivateWithdrawalCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        eprintln!(
            "private withdrawal shape: constraints={} instances={} witnesses={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
        cs.is_satisfied().unwrap()
    }

    #[test]
    #[ignore = "multi-million-constraint exact Token-2022 bridge"]
    fn exact_claim_is_satisfied() {
        assert!(satisfied(PrivateWithdrawalWitness::deterministic(
            PrivateWithdrawalKind::Claim
        )));
    }

    #[test]
    #[ignore = "multi-million-constraint exact Token-2022 bridge"]
    fn exact_refund_is_satisfied() {
        assert!(satisfied(PrivateWithdrawalWitness::deterministic(
            PrivateWithdrawalKind::Refund
        )));
    }

    #[test]
    #[ignore = "multi-million-constraint deterministic setup and proof benchmark"]
    fn exact_claim_setup_and_proof_verifies() {
        let start = Instant::now();
        let fixture =
            deterministic_private_withdrawal_solana_fixture(PrivateWithdrawalKind::Claim).unwrap();
        eprintln!(
            "claim setup+proof fixture ms={:.3}",
            start.elapsed().as_secs_f64() * 1_000.0
        );
        assert_ne!(fixture.proof_a, [0; 64]);
    }
}

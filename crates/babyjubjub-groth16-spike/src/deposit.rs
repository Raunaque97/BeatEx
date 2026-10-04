//! Public Token-2022 deposit to encrypted-balance transition.
//!
//! This circuit matches `queue_hash::BalanceDepositStatement`. It opens the
//! old and new private balances and proves that the two public token amounts
//! transferred by the Anchor instruction are exactly the two plaintext
//! increments. All four ciphertexts use the registered committee key.

use super::*;
use std::ops::Neg;

const BALANCE_DEPOSIT_DOMAIN: &[u8] = b"BeatEx balance deposit v1";
const BALANCE_BITS: usize = 64;

#[derive(Clone, Debug)]
struct DepositWitness {
    market_context: [Fq; 2],
    owner_context: [Fq; 2],
    key_epoch_context: [Fq; 2],
    committee_key: EdwardsProjective,
    old_revision: u64,
    base_amount: u64,
    quote_amount: u64,
    old_base: u64,
    old_quote: u64,
    randomness: [JubjubScalar; 4],
}

impl DepositWitness {
    fn deterministic() -> Self {
        let mut rng = ChaCha20Rng::from_seed([0x6d; 32]);
        let secret = JubjubScalar::rand(&mut rng);
        Self {
            market_context: [Fq::from(0x4d41524bu64), Fq::from(0x4554u64)],
            owner_context: [Fq::from(0x4f574e45u64), Fq::from(0x52u64)],
            key_epoch_context: [Fq::from(0x4b455945u64), Fq::from(0x504f4348u64)],
            committee_key: EdwardsProjective::generator().mul_bigint(secret.into_bigint()),
            old_revision: 4,
            base_amount: 7,
            quote_amount: 250_000,
            old_base: 19,
            old_quote: 1_500_000,
            randomness: std::array::from_fn(|_| JubjubScalar::rand(&mut rng)),
        }
    }

    fn new_base(&self) -> Option<u64> {
        self.old_base.checked_add(self.base_amount)
    }

    fn new_quote(&self) -> Option<u64> {
        self.old_quote.checked_add(self.quote_amount)
    }

    fn ciphertexts(&self) -> Option<[Ciphertext; 4]> {
        Some([
            encrypt(self.old_base.into(), self.randomness[0], self.committee_key),
            encrypt(
                self.old_quote.into(),
                self.randomness[1],
                self.committee_key,
            ),
            encrypt(
                self.new_base()?.into(),
                self.randomness[2],
                self.committee_key,
            ),
            encrypt(
                self.new_quote()?.into(),
                self.randomness[3],
                self.committee_key,
            ),
        ])
    }

    fn digest(&self) -> Option<Fq> {
        let mut payload = Vec::with_capacity(24);
        payload.push(Fq::from_le_bytes_mod_order(BALANCE_DEPOSIT_DOMAIN));
        payload.push(Fq::from(1u64));
        payload.extend_from_slice(&self.market_context);
        payload.extend_from_slice(&self.owner_context);
        payload.extend_from_slice(&self.key_epoch_context);
        append_wire_point_native(&mut payload, self.committee_key.into_affine());
        payload.push(Fq::from(self.old_revision));
        payload.push(Fq::from(self.old_revision.checked_add(1)?));
        payload.push(Fq::from(self.base_amount));
        payload.push(Fq::from(self.quote_amount));
        for ciphertext in self.ciphertexts()? {
            append_wire_point_native(&mut payload, ciphertext.ephemeral);
            append_wire_point_native(&mut payload, ciphertext.body);
        }
        Some(transcript_hash_native(&payload))
    }
}

#[derive(Clone)]
struct DepositCircuit {
    witness: Option<DepositWitness>,
    digest: Option<Fq>,
}

impl DepositCircuit {
    fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
        }
    }

    fn from_witness(witness: DepositWitness) -> Self {
        let digest = witness.digest();
        Self {
            witness: Some(witness),
            digest,
        }
    }
}

impl ConstraintSynthesizer<Fq> for DepositCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let (_, old_revision) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.old_revision)),
            64,
        )?;
        let (_, new_revision) = alloc_uint(
            cs.clone(),
            witness.and_then(|witness| witness.old_revision.checked_add(1).map(u128::from)),
            64,
        )?;
        (&old_revision + Fq::from(1u64)).enforce_equal(&new_revision)?;

        let (_, base_amount) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.base_amount)),
            BALANCE_BITS,
        )?;
        let (_, quote_amount) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.quote_amount)),
            BALANCE_BITS,
        )?;
        let (old_base_bits, old_base) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.old_base)),
            BALANCE_BITS,
        )?;
        let (old_quote_bits, old_quote) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.old_quote)),
            BALANCE_BITS,
        )?;
        let (new_base_bits, new_base) = alloc_uint(
            cs.clone(),
            witness.and_then(|witness| witness.new_base().map(u128::from)),
            BALANCE_BITS,
        )?;
        let (new_quote_bits, new_quote) = alloc_uint(
            cs.clone(),
            witness.and_then(|witness| witness.new_quote().map(u128::from)),
            BALANCE_BITS,
        )?;
        (&old_base + &base_amount).enforce_equal(&new_base)?;
        (&old_quote + &quote_amount).enforce_equal(&new_quote)?;

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            witness
                .map(|witness| witness.committee_key)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        committee_key.is_zero()?.enforce_equal(&Boolean::FALSE)?;
        let values = [old_base_bits, old_quote_bits, new_base_bits, new_quote_bits];
        let mut ciphertext_vars = Vec::with_capacity(4);
        for (index, value_bits) in values.iter().enumerate() {
            let randomness = witness.map(|witness| {
                let value = witness.randomness[index].into_bigint();
                (0..RANDOMNESS_BITS)
                    .map(|bit| value.get_bit(bit))
                    .collect::<Vec<_>>()
            });
            ciphertext_vars.push(encrypt_gadget(
                cs.clone(),
                value_bits,
                randomness,
                &committee_key,
            )?);
        }

        let mut payload = Vec::with_capacity(24);
        payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(
            BALANCE_DEPOSIT_DOMAIN,
        )));
        payload.push(FpVar::constant(Fq::from(1u64)));
        for contexts in [
            witness.map(|witness| witness.market_context),
            witness.map(|witness| witness.owner_context),
            witness.map(|witness| witness.key_epoch_context),
        ] {
            for part in 0..2 {
                payload.push(FpVar::new_witness(cs.clone(), || {
                    contexts
                        .map(|context| context[part])
                        .ok_or(SynthesisError::AssignmentMissing)
                })?);
            }
        }
        append_wire_point_gadget(&mut payload, &committee_key)?;
        payload.push(old_revision);
        payload.push(new_revision);
        payload.push(base_amount);
        payload.push(quote_amount);
        for (ephemeral, body) in &ciphertext_vars {
            append_wire_point_gadget(&mut payload, ephemeral)?;
            append_wire_point_gadget(&mut payload, body)?;
        }
        let expected_digest =
            FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
        transcript_hash_gadget(&payload)?.enforce_equal(&expected_digest)
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
    use ark_r1cs_std::prelude::ToBitsGadget;
    let x_bits = point.x.to_bits_le()?;
    payload.push(point.y.clone());
    payload.push(x_bits[0].clone().into());
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SolanaBalanceDepositFixture {
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
    pub owner: [u8; 32],
    pub key_epoch: [u8; 32],
    pub committee_key: [u8; 32],
    pub old_revision: u64,
    pub base_amount: u64,
    pub quote_amount: u64,
    pub old_base: [u8; 64],
    pub old_quote: [u8; 64],
    pub new_base: [u8; 64],
    pub new_quote: [u8; 64],
}

/// Deterministic development fixture for host/SBF parity tests. Both setup
/// and proof seeds are intentionally non-production.
pub fn deterministic_solana_fixture(
) -> Result<SolanaBalanceDepositFixture, Box<dyn std::error::Error>> {
    build_solana_fixture(DepositWitness::deterministic())
}

/// Deterministic fixture bound to caller-provided Solana account addresses.
/// This is used by LiteSVM to exercise the complete signed deposit instruction.
pub fn deterministic_solana_fixture_for_context(
    market: [u8; 32],
    owner: [u8; 32],
    key_epoch: [u8; 32],
) -> Result<SolanaBalanceDepositFixture, Box<dyn std::error::Error>> {
    let mut witness = DepositWitness::deterministic();
    witness.market_context = pubkey_context(market);
    witness.owner_context = pubkey_context(owner);
    witness.key_epoch_context = pubkey_context(key_epoch);
    build_solana_fixture(witness)
}

fn pubkey_context(bytes: [u8; 32]) -> [Fq; 2] {
    [
        Fq::from_le_bytes_mod_order(&bytes[..16]),
        Fq::from_le_bytes_mod_order(&bytes[16..]),
    ]
}

fn build_solana_fixture(
    witness: DepositWitness,
) -> Result<SolanaBalanceDepositFixture, Box<dyn std::error::Error>> {
    let digest = witness.digest().ok_or("valid deterministic deposit")?;
    let ciphertexts = witness
        .ciphertexts()
        .ok_or("valid deterministic balances")?;
    let mut setup_rng = ChaCha20Rng::from_seed([0x81; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(DepositCircuit::blank(), &mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0x91; 32]);
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        DepositCircuit::from_witness(witness.clone()),
        &mut proof_rng,
    )?;
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(transition::g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "deposit verifying key must have two IC points")?;
    let encrypted = ciphertexts.map(transition::ciphertext_bytes);
    Ok(SolanaBalanceDepositFixture {
        vk_alpha_g1: transition::g1_bytes(verifying_key.alpha_g1),
        vk_beta_g2: transition::g2_bytes(verifying_key.beta_g2),
        vk_gamma_g2: transition::g2_bytes(verifying_key.gamma_g2),
        vk_delta_g2: transition::g2_bytes(verifying_key.delta_g2),
        vk_ic: ic,
        proof_a: transition::g1_bytes(proof.a.neg()),
        proof_b: transition::g2_bytes(proof.b),
        proof_c: transition::g1_bytes(proof.c),
        public_input: transition::field_bytes(digest),
        market: transition::context_pubkey(witness.market_context),
        owner: transition::context_pubkey(witness.owner_context),
        key_epoch: transition::context_pubkey(witness.key_epoch_context),
        committee_key: transition::wire_point_bytes(witness.committee_key.into_affine()),
        old_revision: witness.old_revision,
        base_amount: witness.base_amount,
        quote_amount: witness.quote_amount,
        old_base: encrypted[0],
        old_quote: encrypted[1],
        new_base: encrypted[2],
        new_quote: encrypted[3],
    })
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cs = ConstraintSystem::<Fq>::new_ref();
    cs.set_mode(ark_relations::r1cs::SynthesisMode::Setup);
    DepositCircuit::blank().generate_constraints(cs.clone())?;
    cs.finalize();
    println!("BeatEx public-token deposit/encrypted-balance circuit");
    println!(
        "deposit_constraints={} instance_variables={} witness_variables={}",
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables()
    );

    let witness = DepositWitness::deterministic();
    let digest = witness.digest().ok_or("valid deterministic deposit")?;
    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([0x81; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(DepositCircuit::blank(), &mut setup_rng)?;
    let setup_time = setup_started.elapsed();
    let mut proving_key_bytes = Vec::new();
    proving_key.serialize_compressed(&mut proving_key_bytes)?;
    let prepared_key = Groth16::<Bn254>::process_vk(&verifying_key)?;
    let mut prove_times = Vec::new();
    let mut verify_times = Vec::new();
    for round in 0..3u8 {
        let mut proof_rng = ChaCha20Rng::from_seed([0x91 + round; 32]);
        let prove_started = Instant::now();
        let proof = Groth16::<Bn254>::prove(
            &proving_key,
            DepositCircuit::from_witness(witness.clone()),
            &mut proof_rng,
        )?;
        prove_times.push(prove_started.elapsed());
        let verify_started = Instant::now();
        assert!(Groth16::<Bn254>::verify_with_processed_vk(
            &prepared_key,
            &[digest],
            &proof,
        )?);
        verify_times.push(verify_started.elapsed());
    }
    println!(
        "deposit_setup_ms={:.3}, deposit_prove_median_ms={:.3}, deposit_verify_median_ms={:.3}, deposit_pk_bytes={}",
        setup_time.as_secs_f64() * 1_000.0,
        median(prove_times).as_secs_f64() * 1_000.0,
        median(verify_times).as_secs_f64() * 1_000.0,
        proving_key_bytes.len(),
    );
    println!(
        "deposit_wire_bytes={} (two new ciphertexts + public amounts + proof + digest)",
        2 * 64 + 2 * 8 + 256 + 32
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_satisfied(witness: DepositWitness, digest: Fq) -> bool {
        let circuit = DepositCircuit {
            witness: Some(witness),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        cs.is_satisfied().unwrap()
    }

    #[test]
    fn valid_deposit_is_satisfied() {
        let witness = DepositWitness::deterministic();
        assert!(is_satisfied(witness.clone(), witness.digest().unwrap()));
    }

    #[test]
    fn changed_public_amount_is_rejected() {
        let witness = DepositWitness::deterministic();
        let digest = witness.digest().unwrap();
        let mut changed = witness;
        changed.base_amount += 1;
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn changed_new_ciphertext_is_rejected() {
        let witness = DepositWitness::deterministic();
        let digest = witness.digest().unwrap();
        let mut changed = witness;
        changed.randomness[2] += JubjubScalar::from(1u64);
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn cross_owner_replay_is_rejected() {
        let witness = DepositWitness::deterministic();
        let digest = witness.digest().unwrap();
        let mut changed = witness;
        changed.owner_context[0] += Fq::from(1u64);
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn overflowing_balance_has_no_witness() {
        let witness = DepositWitness::deterministic();
        let digest = witness.digest().unwrap();
        let mut changed = witness;
        changed.old_base = u64::MAX;
        changed.base_amount = 1;
        let circuit = DepositCircuit {
            witness: Some(changed),
            digest: Some(digest),
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        assert!(circuit.generate_constraints(cs).is_err());
    }
}

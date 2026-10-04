//! Host-only Groth16 serialization and verification feasibility spike.
//!
//! This is intentionally not a production order-validity circuit. In
//! particular, it does not bind its private BN254 witnesses to BeatEx's
//! Ristretto twisted-ElGamal ciphertexts.

use {
    ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine},
    ark_ec::AffineRepr,
    ark_ff::{BigInteger, PrimeField},
    ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey},
    ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError, Variable},
    ark_snark::SNARK,
    groth16_solana::groth16::{Groth16Verifier, Groth16Verifyingkey},
    rand::SeedableRng,
    rand_chacha::ChaCha20Rng,
};

pub const GRID_LEVELS: usize = 3;
pub const MAX_QUANTITY: u64 = 15;
pub const PUBLIC_INPUT_COUNT: usize = 2;

const SETUP_SEED: [u8; 32] = [0x42; 32];
const PROOF_SEED: [u8; 32] = [0x24; 32];

/// Private order witnesses for the three-level feasibility circuit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderWitness {
    pub buy: [u64; GRID_LEVELS],
    pub sell: [u64; GRID_LEVELS],
}

impl OrderWitness {
    pub fn public_inputs(&self) -> [Fr; PUBLIC_INPUT_COUNT] {
        [
            self.buy
                .iter()
                .fold(Fr::from(0u64), |sum, quantity| sum + Fr::from(*quantity)),
            self.sell
                .iter()
                .fold(Fr::from(0u64), |sum, quantity| sum + Fr::from(*quantity)),
        ]
    }
}

/// Circuit used only to validate the Groth16 and Solana byte pipeline.
///
/// The polynomial `(q - 0) * ... * (q - MAX_QUANTITY) == 0` constrains each
/// field witness to one of the allowed integer quantities. This is compact
/// enough for a small feasibility spike, but it is not the range-check design
/// intended for a production-sized circuit.
#[derive(Clone, Debug)]
pub struct OrderValidityCircuit {
    witness: Option<OrderWitness>,
    total_buy: Option<Fr>,
    total_sell: Option<Fr>,
}

impl OrderValidityCircuit {
    pub fn blank() -> Self {
        Self {
            witness: None,
            total_buy: None,
            total_sell: None,
        }
    }

    pub fn from_witness(witness: OrderWitness) -> Self {
        let [total_buy, total_sell] = witness.public_inputs();
        Self {
            witness: Some(witness),
            total_buy: Some(total_buy),
            total_sell: Some(total_sell),
        }
    }
}

impl ConstraintSynthesizer<Fr> for OrderValidityCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let total_buy =
            cs.new_input_variable(|| self.total_buy.ok_or(SynthesisError::AssignmentMissing))?;
        let total_sell =
            cs.new_input_variable(|| self.total_sell.ok_or(SynthesisError::AssignmentMissing))?;

        let mut buy_sum = ark_relations::lc!();
        let mut sell_sum = ark_relations::lc!();

        for index in 0..GRID_LEVELS {
            let buy_value = self.witness.map(|witness| Fr::from(witness.buy[index]));
            let sell_value = self.witness.map(|witness| Fr::from(witness.sell[index]));

            let buy =
                cs.new_witness_variable(|| buy_value.ok_or(SynthesisError::AssignmentMissing))?;
            let sell =
                cs.new_witness_variable(|| sell_value.ok_or(SynthesisError::AssignmentMissing))?;

            // A level may quote buy or sell liquidity, but never both.
            cs.enforce_constraint(
                ark_relations::lc!() + buy,
                ark_relations::lc!() + sell,
                ark_relations::lc!(),
            )?;

            enforce_small_inclusive_range(&cs, buy, buy_value)?;
            enforce_small_inclusive_range(&cs, sell, sell_value)?;

            buy_sum += (Fr::from(1u64), buy);
            sell_sum += (Fr::from(1u64), sell);
        }

        cs.enforce_constraint(
            buy_sum - total_buy,
            ark_relations::lc!() + Variable::One,
            ark_relations::lc!(),
        )?;
        cs.enforce_constraint(
            sell_sum - total_sell,
            ark_relations::lc!() + Variable::One,
            ark_relations::lc!(),
        )?;

        Ok(())
    }
}

fn enforce_small_inclusive_range(
    cs: &ConstraintSystemRef<Fr>,
    quantity: Variable,
    quantity_value: Option<Fr>,
) -> Result<(), SynthesisError> {
    let mut accumulator = quantity;
    let mut accumulator_value = quantity_value;

    for allowed in 1..=MAX_QUANTITY {
        let next_value = quantity_value.map(|value| value - Fr::from(allowed));
        let next =
            cs.new_witness_variable(|| next_value.ok_or(SynthesisError::AssignmentMissing))?;
        cs.enforce_constraint(
            ark_relations::lc!() + quantity - (Fr::from(allowed), Variable::One),
            ark_relations::lc!() + Variable::One,
            ark_relations::lc!() + next,
        )?;

        if allowed == MAX_QUANTITY {
            cs.enforce_constraint(
                ark_relations::lc!() + accumulator,
                ark_relations::lc!() + next,
                ark_relations::lc!(),
            )?;
        } else {
            let product_value = accumulator_value
                .zip(next_value)
                .map(|(left, right)| left * right);
            let product =
                cs.new_witness_variable(|| product_value.ok_or(SynthesisError::AssignmentMissing))?;
            cs.enforce_constraint(
                ark_relations::lc!() + accumulator,
                ark_relations::lc!() + next,
                ark_relations::lc!() + product,
            )?;
            accumulator = product;
            accumulator_value = product_value;
        }
    }

    Ok(())
}

/// Uncompressed proof layout accepted by Light Protocol's verifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolanaProof {
    /// Negated A in big-endian G1 `x || y` form.
    pub a: [u8; 64],
    /// B in big-endian G2 `x.c1 || x.c0 || y.c1 || y.c0` form.
    pub b: [u8; 128],
    /// C in big-endian G1 `x || y` form.
    pub c: [u8; 64],
}

impl SolanaProof {
    pub fn to_bytes(&self) -> [u8; 256] {
        let mut bytes = [0u8; 256];
        bytes[..64].copy_from_slice(&self.a);
        bytes[64..192].copy_from_slice(&self.b);
        bytes[192..].copy_from_slice(&self.c);
        bytes
    }
}

/// Verifying-key layout accepted by `groth16-solana` 0.2.0.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SolanaVerifyingKey {
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: Vec<[u8; 64]>,
}

impl SolanaVerifyingKey {
    fn as_light_key(&self) -> Groth16Verifyingkey<'_> {
        Groth16Verifyingkey {
            nr_pubinputs: self.ic.len(),
            vk_alpha_g1: self.alpha_g1,
            vk_beta_g2: self.beta_g2,
            // Field spelling is retained by groth16-solana 0.2.0.
            vk_gamme_g2: self.gamma_g2,
            vk_delta_g2: self.delta_g2,
            vk_ic: &self.ic,
        }
    }
}

/// Deterministically generated native and exported artifacts.
pub struct TestArtifacts {
    pub proving_key: ProvingKey<Bn254>,
    pub proof: Proof<Bn254>,
    pub public_inputs: [Fr; PUBLIC_INPUT_COUNT],
    pub solana_proof: SolanaProof,
    pub solana_verifying_key: SolanaVerifyingKey,
    pub solana_public_inputs: [[u8; 32]; PUBLIC_INPUT_COUNT],
}

/// Generate a deterministic test setup and proof.
///
/// Fixed seeds make tests reproducible. They provide no ceremony security and
/// must never be used for a deployed circuit.
pub fn generate_test_artifacts(witness: OrderWitness) -> Result<TestArtifacts, SynthesisError> {
    let mut setup_rng = ChaCha20Rng::from_seed(SETUP_SEED);
    let proving_key = Groth16::<Bn254>::generate_random_parameters_with_reduction(
        OrderValidityCircuit::blank(),
        &mut setup_rng,
    )?;

    let public_inputs = witness.public_inputs();
    let mut proof_rng = ChaCha20Rng::from_seed(PROOF_SEED);
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        OrderValidityCircuit::from_witness(witness),
        &mut proof_rng,
    )?;

    let solana_proof = export_proof(&proof);
    let solana_verifying_key = export_verifying_key(&proving_key.vk);
    let solana_public_inputs = public_inputs.map(fr_to_be_bytes);

    Ok(TestArtifacts {
        proving_key,
        proof,
        public_inputs,
        solana_proof,
        solana_verifying_key,
        solana_public_inputs,
    })
}

pub fn export_proof(proof: &Proof<Bn254>) -> SolanaProof {
    SolanaProof {
        a: g1_to_be_bytes(-proof.a),
        b: g2_to_be_bytes(proof.b),
        c: g1_to_be_bytes(proof.c),
    }
}

pub fn export_verifying_key(verifying_key: &VerifyingKey<Bn254>) -> SolanaVerifyingKey {
    SolanaVerifyingKey {
        alpha_g1: g1_to_be_bytes(verifying_key.alpha_g1),
        beta_g2: g2_to_be_bytes(verifying_key.beta_g2),
        gamma_g2: g2_to_be_bytes(verifying_key.gamma_g2),
        delta_g2: g2_to_be_bytes(verifying_key.delta_g2),
        ic: verifying_key
            .gamma_abc_g1
            .iter()
            .copied()
            .map(g1_to_be_bytes)
            .collect(),
    }
}

pub fn verify_with_arkworks(artifacts: &TestArtifacts) -> Result<bool, SynthesisError> {
    Groth16::<Bn254>::verify_proof(
        &prepare_verifying_key(&artifacts.proving_key.vk),
        &artifacts.proof,
        &artifacts.public_inputs,
    )
}

/// Verify the exported bytes through the same host implementation used by
/// `groth16-solana`, including Solana's alt_bn128 serialization conventions.
pub fn verify_with_light_protocol(artifacts: &TestArtifacts) -> bool {
    let light_key = artifacts.solana_verifying_key.as_light_key();
    Groth16Verifier::<PUBLIC_INPUT_COUNT>::new(
        &artifacts.solana_proof.a,
        &artifacts.solana_proof.b,
        &artifacts.solana_proof.c,
        &artifacts.solana_public_inputs,
        &light_key,
    )
    .and_then(|mut verifier| verifier.verify())
    .is_ok()
}

fn g1_to_be_bytes(point: G1Affine) -> [u8; 64] {
    if point.is_zero() {
        return [0u8; 64];
    }
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(&fq_to_be_bytes(point.x));
    bytes[32..].copy_from_slice(&fq_to_be_bytes(point.y));
    bytes
}

fn g2_to_be_bytes(point: G2Affine) -> [u8; 128] {
    if point.is_zero() {
        return [0u8; 128];
    }
    let mut bytes = [0u8; 128];
    write_fq2_for_solana(&mut bytes[..64], point.x);
    write_fq2_for_solana(&mut bytes[64..], point.y);
    bytes
}

fn write_fq2_for_solana(destination: &mut [u8], value: Fq2) {
    destination[..32].copy_from_slice(&fq_to_be_bytes(value.c1));
    destination[32..].copy_from_slice(&fq_to_be_bytes(value.c0));
}

fn fq_to_be_bytes(value: Fq) -> [u8; 32] {
    bigint_to_32_be(value.into_bigint().to_bytes_be())
}

fn fr_to_be_bytes(value: Fr) -> [u8; 32] {
    bigint_to_32_be(value.into_bigint().to_bytes_be())
}

fn bigint_to_32_be(encoded: Vec<u8>) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let start = 32 - encoded.len();
    bytes[start..].copy_from_slice(&encoded);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;

    const VALID_WITNESS: OrderWitness = OrderWitness {
        buy: [9, 4, 0],
        sell: [0, 0, 7],
    };

    #[test]
    fn valid_circuit_is_satisfied() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        OrderValidityCircuit::from_witness(VALID_WITNESS)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn rejects_buy_and_sell_at_the_same_level() {
        let witness = OrderWitness {
            buy: [9, 4, 0],
            sell: [0, 1, 7],
        };
        let cs = ConstraintSystem::<Fr>::new_ref();
        OrderValidityCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn rejects_quantity_above_the_bound() {
        let witness = OrderWitness {
            buy: [MAX_QUANTITY + 1, 0, 0],
            sell: [0, 0, 1],
        };
        let cs = ConstraintSystem::<Fr>::new_ref();
        OrderValidityCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn accepts_quantities_at_both_inclusive_bounds() {
        let witness = OrderWitness {
            buy: [MAX_QUANTITY, 0, 0],
            sell: [0, 0, MAX_QUANTITY],
        };
        let cs = ConstraintSystem::<Fr>::new_ref();
        OrderValidityCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn exported_proof_verifies_in_arkworks_and_light_protocol() {
        let artifacts = generate_test_artifacts(VALID_WITNESS).unwrap();
        assert!(verify_with_arkworks(&artifacts).unwrap());
        assert!(verify_with_light_protocol(&artifacts));
        assert_eq!(artifacts.solana_proof.to_bytes().len(), 256);
        assert_eq!(
            artifacts.solana_verifying_key.ic.len(),
            PUBLIC_INPUT_COUNT + 1
        );
    }

    #[test]
    fn setup_and_proof_export_are_deterministic() {
        let first = generate_test_artifacts(VALID_WITNESS).unwrap();
        let second = generate_test_artifacts(VALID_WITNESS).unwrap();
        assert_eq!(first.solana_proof, second.solana_proof);
        assert_eq!(first.solana_verifying_key, second.solana_verifying_key);
        assert_eq!(first.solana_public_inputs, second.solana_public_inputs);
    }

    #[test]
    fn public_inputs_are_fixed_width_big_endian() {
        let inputs = VALID_WITNESS.public_inputs().map(fr_to_be_bytes);
        assert_eq!(inputs[0][31], 13);
        assert_eq!(inputs[1][31], 7);
        assert!(inputs[0][..31].iter().all(|byte| *byte == 0));
        assert!(inputs[1][..31].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn changed_public_input_fails_light_verification() {
        let mut artifacts = generate_test_artifacts(VALID_WITNESS).unwrap();
        artifacts.solana_public_inputs[0][31] ^= 1;
        assert!(!verify_with_light_protocol(&artifacts));
    }

    #[test]
    fn changed_proof_fails_light_verification() {
        let mut artifacts = generate_test_artifacts(VALID_WITNESS).unwrap();
        artifacts.solana_proof.c[63] ^= 1;
        assert!(!verify_with_light_protocol(&artifacts));
    }
}

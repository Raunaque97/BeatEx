//! Entirely-WebAssembly Arkworks Groth16 experiments for BeatEx orders.
//!
//! The original benchmark copies the arithmetic semantics of `crates/groth16-spike`:
//! every private buy/sell quantity is in `0..=15`, buy and sell cannot both be
//! non-zero at one price level, and the two public inputs are the buy and sell
//! totals. The `ristretto` module separately implements the complete
//! Solana-compatible twisted-ElGamal ciphertext-opening relation so its real
//! non-native constraint cost can be measured rather than estimated.

mod ristretto;

pub use ristretto::{BindingWitness, RistrettoBindingCircuit};

use {
    ark_bn254::{Bn254, Fq, Fq2, Fr, G1Affine, G2Affine},
    ark_ec::AffineRepr,
    ark_ff::{BigInteger, PrimeField},
    ark_groth16::{prepare_verifying_key, Groth16, PreparedVerifyingKey, ProvingKey},
    ark_relations::r1cs::{
        ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError, Variable,
    },
    ark_serialize::CanonicalSerialize,
    ark_snark::SNARK,
    rand::SeedableRng,
    rand_chacha::ChaCha20Rng,
    serde::Serialize,
    wasm_bindgen::prelude::*,
};

const MAX_QUANTITY: u64 = 15;
const PUBLIC_INPUT_COUNT: usize = 2;
const SOLANA_PROOF_BYTES: usize = 256;

#[derive(Clone, Debug)]
struct OrderWitness {
    buy: Vec<u64>,
    sell: Vec<u64>,
}

impl OrderWitness {
    fn deterministic(levels: usize) -> Self {
        let mut buy = vec![0; levels];
        let mut sell = vec![0; levels];
        let spread = levels / 2;

        for (index, quantity) in buy.iter_mut().enumerate().take(spread) {
            *quantity = ((index * 7 + 3) % MAX_QUANTITY as usize + 1) as u64;
        }
        for (index, quantity) in sell.iter_mut().enumerate().skip(spread + 1) {
            *quantity = ((index * 11 + 5) % MAX_QUANTITY as usize + 1) as u64;
        }

        Self { buy, sell }
    }

    fn public_inputs(&self) -> [Fr; PUBLIC_INPUT_COUNT] {
        [
            self.buy
                .iter()
                .fold(Fr::from(0u64), |sum, value| sum + Fr::from(*value)),
            self.sell
                .iter()
                .fold(Fr::from(0u64), |sum, value| sum + Fr::from(*value)),
        ]
    }
}

#[derive(Clone, Debug)]
struct OrderValidityCircuit {
    levels: usize,
    witness: Option<OrderWitness>,
    total_buy: Option<Fr>,
    total_sell: Option<Fr>,
}

impl OrderValidityCircuit {
    fn blank(levels: usize) -> Self {
        Self {
            levels,
            witness: None,
            total_buy: None,
            total_sell: None,
        }
    }

    fn from_witness(witness: OrderWitness) -> Self {
        let levels = witness.buy.len();
        let [total_buy, total_sell] = witness.public_inputs();
        Self {
            levels,
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

        for index in 0..self.levels {
            let buy_value = self
                .witness
                .as_ref()
                .map(|witness| Fr::from(witness.buy[index]));
            let sell_value = self
                .witness
                .as_ref()
                .map(|witness| Fr::from(witness.sell[index]));
            let buy =
                cs.new_witness_variable(|| buy_value.ok_or(SynthesisError::AssignmentMissing))?;
            let sell =
                cs.new_witness_variable(|| sell_value.ok_or(SynthesisError::AssignmentMissing))?;

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupReport {
    levels: usize,
    setup_ms: f64,
    constraints: usize,
    instance_variables: usize,
    witness_variables: usize,
    proving_key_compressed_bytes: usize,
    proving_key_uncompressed_bytes: usize,
    warning: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    levels: usize,
    rounds: usize,
    proof_ms: Vec<f64>,
    export_ms: Vec<f64>,
    verify_ms: Vec<f64>,
    proof_bytes: usize,
    export_checksum: u8,
    all_verified: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RistrettoConstraintReport {
    bindings: usize,
    constraints: usize,
    instance_variables: usize,
    witness_variables: usize,
    shared_key_constraints: usize,
    constraints_per_binding: usize,
    relation: &'static str,
}

/// Synthesize the real Ristretto relation once, then report the exact linear
/// shape for `bindings` ciphertexts. A normal order has two bindings per price
/// level (buy and sell) until the signed-quantity design is adopted.
#[wasm_bindgen(js_name = ristrettoConstraintReport)]
pub fn ristretto_constraint_report(bindings: usize) -> Result<JsValue, JsValue> {
    if bindings == 0 || bindings > 510 {
        return Err(JsValue::from_str("bindings must be in 1..=510"));
    }
    let shared = ristretto::circuit_shape(0).map_err(synthesis_error)?;
    let one = ristretto::circuit_shape(1).map_err(synthesis_error)?;
    let per_binding = (
        one.0 - shared.0,
        one.1 - shared.1,
        one.2 - shared.2,
    );
    serde_wasm_bindgen::to_value(&RistrettoConstraintReport {
        bindings,
        constraints: shared.0 + bindings * per_binding.0,
        instance_variables: shared.1 + bindings * per_binding.1,
        witness_variables: shared.2 + bindings * per_binding.2,
        shared_key_constraints: shared.0,
        constraints_per_binding: per_binding.0,
        relation: "C = q*G + r*H; D = r*P; canonical compressed Ristretto255",
    })
    .map_err(js_error)
}

/// Holds one deterministic proving key in WebAssembly memory.
///
/// Construct this once, outside the timed per-order proof path.
#[wasm_bindgen]
pub struct BrowserProver {
    levels: usize,
    proving_key: ProvingKey<Bn254>,
    prepared_verifying_key: PreparedVerifyingKey<Bn254>,
    setup_report: SetupReport,
}

#[wasm_bindgen]
impl BrowserProver {
    #[wasm_bindgen(constructor)]
    pub fn new(levels: usize) -> Result<BrowserProver, JsValue> {
        if levels == 0 || levels > 255 {
            return Err(JsValue::from_str("levels must be in 1..=255"));
        }

        let (constraints, instance_variables, witness_variables) = circuit_shape(levels)?;
        let setup_started = now_ms();
        let mut setup_rng = ChaCha20Rng::from_seed(seed_for(0x42, levels, 0));
        let proving_key = Groth16::<Bn254>::generate_random_parameters_with_reduction(
            OrderValidityCircuit::blank(levels),
            &mut setup_rng,
        )
        .map_err(synthesis_error)?;
        let setup_ms = now_ms() - setup_started;
        let prepared_verifying_key = prepare_verifying_key(&proving_key.vk);
        let setup_report = SetupReport {
            levels,
            setup_ms,
            constraints,
            instance_variables,
            witness_variables,
            proving_key_compressed_bytes: proving_key.compressed_size(),
            proving_key_uncompressed_bytes: proving_key.uncompressed_size(),
            warning: "Arithmetic-only feasibility circuit; no Ristretto ciphertext binding",
        };

        Ok(Self {
            levels,
            proving_key,
            prepared_verifying_key,
            setup_report,
        })
    }

    #[wasm_bindgen(js_name = setupReport)]
    pub fn setup_report(&self) -> Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(&self.setup_report).map_err(js_error)
    }

    /// Generate, export, and verify `rounds` proofs entirely inside WebAssembly.
    #[wasm_bindgen]
    pub fn benchmark(&self, rounds: usize) -> Result<JsValue, JsValue> {
        if rounds == 0 || rounds > 100 {
            return Err(JsValue::from_str("rounds must be in 1..=100"));
        }

        let witness = OrderWitness::deterministic(self.levels);
        let public_inputs = witness.public_inputs();
        let mut proof_ms = Vec::with_capacity(rounds);
        let mut export_ms = Vec::with_capacity(rounds);
        let mut verify_ms = Vec::with_capacity(rounds);
        let mut all_verified = true;
        let mut export_checksum = 0u8;

        for round in 0..rounds {
            let mut proof_rng = ChaCha20Rng::from_seed(seed_for(0x24, self.levels, round as u64));
            let proof_started = now_ms();
            let proof = Groth16::<Bn254>::prove(
                &self.proving_key,
                OrderValidityCircuit::from_witness(witness.clone()),
                &mut proof_rng,
            )
            .map_err(synthesis_error)?;
            proof_ms.push(now_ms() - proof_started);

            let export_started = now_ms();
            let exported = std::hint::black_box(export_solana_proof(&proof));
            for byte in exported {
                export_checksum ^= byte;
            }
            export_ms.push(now_ms() - export_started);

            let verify_started = now_ms();
            let verified = Groth16::<Bn254>::verify_proof(
                &self.prepared_verifying_key,
                &proof,
                &public_inputs,
            )
            .map_err(synthesis_error)?;
            verify_ms.push(now_ms() - verify_started);

            all_verified &= verified && exported.len() == SOLANA_PROOF_BYTES;
        }

        serde_wasm_bindgen::to_value(&BenchmarkReport {
            levels: self.levels,
            rounds,
            proof_ms,
            export_ms,
            verify_ms,
            proof_bytes: SOLANA_PROOF_BYTES,
            export_checksum,
            all_verified,
        })
        .map_err(js_error)
    }
}

fn circuit_shape(levels: usize) -> Result<(usize, usize, usize), JsValue> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    OrderValidityCircuit::from_witness(OrderWitness::deterministic(levels))
        .generate_constraints(cs.clone())
        .map_err(synthesis_error)?;
    Ok((
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables(),
    ))
}

fn seed_for(tag: u8, levels: usize, round: u64) -> [u8; 32] {
    let mut seed = [tag; 32];
    seed[1..9].copy_from_slice(&(levels as u64).to_le_bytes());
    seed[9..17].copy_from_slice(&round.to_le_bytes());
    seed
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or_else(js_sys::Date::now)
}

fn synthesis_error(error: SynthesisError) -> JsValue {
    JsValue::from_str(&format!("Groth16 error: {error}"))
}

fn js_error(error: serde_wasm_bindgen::Error) -> JsValue {
    JsValue::from_str(&format!("result serialization error: {error}"))
}

fn export_solana_proof(proof: &ark_groth16::Proof<Bn254>) -> [u8; SOLANA_PROOF_BYTES] {
    let mut bytes = [0u8; SOLANA_PROOF_BYTES];
    bytes[..64].copy_from_slice(&g1_to_be_bytes(-proof.a));
    bytes[64..192].copy_from_slice(&g2_to_be_bytes(proof.b));
    bytes[192..].copy_from_slice(&g1_to_be_bytes(proof.c));
    bytes
}

fn g1_to_be_bytes(point: G1Affine) -> [u8; 64] {
    if point.is_zero() {
        return [0; 64];
    }
    let mut bytes = [0u8; 64];
    bytes[..32].copy_from_slice(&fq_to_be_bytes(point.x));
    bytes[32..].copy_from_slice(&fq_to_be_bytes(point.y));
    bytes
}

fn g2_to_be_bytes(point: G2Affine) -> [u8; 128] {
    if point.is_zero() {
        return [0; 128];
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
    let encoded = value.into_bigint().to_bytes_be();
    let mut bytes = [0u8; 32];
    bytes[32 - encoded.len()..].copy_from_slice(&encoded);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_shapes_are_satisfied_and_scale_linearly() {
        for levels in [3, 17] {
            let cs = ConstraintSystem::<Fr>::new_ref();
            OrderValidityCircuit::from_witness(OrderWitness::deterministic(levels))
                .generate_constraints(cs.clone())
                .unwrap();
            assert!(cs.is_satisfied().unwrap());
            assert_eq!(cs.num_constraints(), 61 * levels + 2);
        }
    }

    #[test]
    fn same_level_buy_and_sell_is_rejected() {
        let mut witness = OrderWitness::deterministic(3);
        witness.buy[0] = 1;
        witness.sell[0] = 1;
        let cs = ConstraintSystem::<Fr>::new_ref();
        OrderValidityCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}

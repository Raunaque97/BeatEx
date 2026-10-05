//! Feasibility circuit for privately binding two Token-2022 transfer amounts.
//!
//! Token-2022 represents each 48-bit transfer with low-16 and high-32
//! Ristretto Pedersen commitments. The Solana program can combine those halves
//! natively, then batch the base- and quote-asset commitments:
//!
//! ```text
//! C_base  = m_base  * G + r_base  * H
//! C_quote = m_quote * G + r_quote * H
//! rho     = low_128(Poseidon(domain, C_base, C_quote))
//! C_star  = C_base + rho * C_quote
//! ```
//!
//! This circuit proves the single relation
//! `C_star = (m_base + rho*m_quote)*G + r_star*H`. The source commitments and
//! `C_star` are public. Computing `C_star` from the source commitments remains
//! an inexpensive native Ristretto precheck performed by the Solana program.

use {
    super::{transcript_hash_gadget, transcript_hash_native},
    ark_bn254::{Bn254, Fr},
    ark_ff::{BigInteger, Field, Fp256, MontBackend, MontConfig, One, PrimeField, Zero},
    ark_groth16::Groth16,
    ark_r1cs_std::{
        alloc::AllocVar,
        boolean::Boolean,
        eq::EqGadget,
        fields::{emulated_fp::EmulatedFpVar, fp::FpVar, FieldVar},
        prelude::{CondSelectGadget, ToBitsGadget},
    },
    ark_relations::r1cs::{
        ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError,
    },
    ark_snark::SNARK,
    curve25519_dalek::{
        constants::RISTRETTO_BASEPOINT_POINT,
        ristretto::{CompressedRistretto, RistrettoPoint},
        scalar::Scalar,
    },
    rand::SeedableRng,
    rand_chacha::ChaCha20Rng,
    sha3::Sha3_512,
    std::time::Instant,
};

#[derive(MontConfig)]
#[modulus = "57896044618658097711785492504343953926634992332820282019728792003956564819949"]
#[generator = "2"]
pub struct F25519Config;

type F25519 = Fp256<MontBackend<F25519Config, 4>>;
type F25519Var = EmulatedFpVar<F25519, Fr>;

const AMOUNT_BITS: usize = 48;
const RHO_BITS: usize = 128;
const BATCHED_AMOUNT_BITS: usize = AMOUNT_BITS + RHO_BITS;
const OPENING_BITS: usize = 253;
const BRIDGE_DOMAIN: &[u8] = b"BeatEx Token-2022 Pedersen bridge v1";

#[derive(Clone, Debug)]
pub struct RistrettoBridgeWitness {
    pub base_amount: u64,
    pub quote_amount: u64,
    pub batched_opening: [u8; 32],
    pub base_commitment: [u8; 32],
    pub quote_commitment: [u8; 32],
    pub batched_commitment: [u8; 32],
}

impl RistrettoBridgeWitness {
    pub fn deterministic() -> Self {
        Self::for_amounts(42_007, 7_654_321)
    }

    pub fn for_amounts(base_amount: u64, quote_amount: u64) -> Self {
        let base_opening = Scalar::from(0x8f31_5a77_1020u64);
        let quote_opening = Scalar::from(0x5ee1_d00d_9988u64);
        let h = ristretto_h();
        let base = Scalar::from(base_amount) * RISTRETTO_BASEPOINT_POINT + base_opening * h;
        let quote = Scalar::from(quote_amount) * RISTRETTO_BASEPOINT_POINT + quote_opening * h;
        let base_commitment = base.compress().to_bytes();
        let quote_commitment = quote.compress().to_bytes();
        let rho = batching_scalar(base_commitment, quote_commitment);
        let batched = base + rho * quote;
        let batched_opening = base_opening + rho * quote_opening;

        Self {
            base_amount,
            quote_amount,
            batched_opening: batched_opening.to_bytes(),
            base_commitment,
            quote_commitment,
            batched_commitment: batched.compress().to_bytes(),
        }
    }

    pub fn public_inputs(&self) -> Vec<Fr> {
        let mut inputs = Vec::with_capacity(6);
        append_packed_point(&mut inputs, self.base_commitment);
        append_packed_point(&mut inputs, self.quote_commitment);
        append_packed_point(&mut inputs, self.batched_commitment);
        inputs
    }

    /// Models the native program precheck which is deliberately outside R1CS.
    pub fn native_batch_precheck(&self) -> bool {
        let Some(base) = CompressedRistretto(self.base_commitment).decompress() else {
            return false;
        };
        let Some(quote) = CompressedRistretto(self.quote_commitment).decompress() else {
            return false;
        };
        let rho = batching_scalar(self.base_commitment, self.quote_commitment);
        (base + rho * quote).compress().to_bytes() == self.batched_commitment
    }
}

#[derive(Clone, Debug)]
pub struct RistrettoBridgeCircuit {
    pub witness: Option<RistrettoBridgeWitness>,
}

impl RistrettoBridgeCircuit {
    pub const fn blank() -> Self {
        Self { witness: None }
    }

    pub fn from_witness(witness: RistrettoBridgeWitness) -> Self {
        Self {
            witness: Some(witness),
        }
    }
}

impl ConstraintSynthesizer<Fr> for RistrettoBridgeCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        let base_commitment = witness.map(|value| value.base_commitment);
        let quote_commitment = witness.map(|value| value.quote_commitment);

        // These four public limbs bind rho to the two commitments which were
        // fixed before the batching challenge. Their Ristretto validity and
        // native combination are checked by the Solana program.
        let base_fields = allocate_public_packed_point(cs.clone(), base_commitment)?;
        let quote_fields = allocate_public_packed_point(cs.clone(), quote_commitment)?;
        let batched_fields = allocate_public_packed_point(
            cs.clone(),
            witness.map(|value| value.batched_commitment),
        )?;
        enforce_batched_pedersen_relation(
            cs,
            witness.map(|value| value.base_amount),
            witness.map(|value| value.quote_amount),
            witness.map(|value| value.batched_opening),
            witness.map(|value| value.batched_commitment),
            &base_fields,
            &quote_fields,
            &batched_fields,
        )
    }
}

/// Enforce the exact batched Ristretto Pedersen relation. Callers may allocate
/// the packed commitment limbs as private transcript witnesses, allowing a
/// larger circuit to expose only its final Poseidon digest.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enforce_batched_pedersen_relation(
    cs: ConstraintSystemRef<Fr>,
    base_amount_value: Option<u64>,
    quote_amount_value: Option<u64>,
    batched_opening_value: Option<[u8; 32]>,
    batched_commitment_value: Option<[u8; 32]>,
    base_fields: &[FpVar<Fr>],
    quote_fields: &[FpVar<Fr>],
    batched_fields: &[FpVar<Fr>],
) -> Result<(), SynthesisError> {
    let mut challenge_payload = vec![
        FpVar::constant(Fr::from_le_bytes_mod_order(BRIDGE_DOMAIN)),
        FpVar::constant(Fr::from(1u64)),
    ];
    challenge_payload.extend_from_slice(base_fields);
    challenge_payload.extend_from_slice(quote_fields);
    let challenge = transcript_hash_gadget(&challenge_payload)?;
    let challenge_bits = challenge.to_bits_le()?;
    let rho_bits = &challenge_bits[..RHO_BITS];
    rho_bits
        .iter()
        .fold(Boolean::FALSE, |nonzero, bit| nonzero | bit)
        .enforce_equal(&Boolean::TRUE)?;
    let rho = bits_to_fp(rho_bits);

    let (_, base_amount) = allocate_u64_bits(cs.clone(), base_amount_value, AMOUNT_BITS)?;
    let (_, quote_amount) = allocate_u64_bits(cs.clone(), quote_amount_value, AMOUNT_BITS)?;
    let batched_amount = &base_amount + &rho * &quote_amount;
    let batched_amount_bits = batched_amount.to_bits_le()?;
    for bit in batched_amount_bits.iter().skip(BATCHED_AMOUNT_BITS) {
        bit.enforce_equal(&Boolean::FALSE)?;
    }

    let opening_bits = allocate_scalar_bits(cs.clone(), batched_opening_value)?;
    let batched_commitment =
        decode_point_with_fields(cs, batched_commitment_value, batched_fields)?;
    let amount_term = fixed_base_scalar_mul_le(
        RISTRETTO_BASEPOINT_POINT,
        &batched_amount_bits[..BATCHED_AMOUNT_BITS],
    )?;
    let opening_term = fixed_base_scalar_mul_le(ristretto_h(), &opening_bits)?;
    amount_term
        .add(&opening_term)?
        .enforce_ristretto_equal(&batched_commitment)
}

pub fn circuit_shape() -> Result<(usize, usize, usize), SynthesisError> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    RistrettoBridgeCircuit::from_witness(RistrettoBridgeWitness::deterministic())
        .generate_constraints(cs.clone())?;
    if !cs.is_satisfied()? {
        return Err(SynthesisError::Unsatisfiable);
    }
    Ok((
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables(),
    ))
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = RistrettoBridgeWitness::deterministic();
    assert!(fixture.native_batch_precheck());
    let (constraints, instances, witnesses) = circuit_shape()?;
    println!("BeatEx batched Ristretto Pedersen bridge circuit");
    println!(
        "ristretto_bridge_constraints={constraints} instance_variables={instances} witness_variables={witnesses}"
    );

    let mut setup_rng = ChaCha20Rng::from_seed([0x71; 32]);
    let setup_start = Instant::now();
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(RistrettoBridgeCircuit::blank(), &mut setup_rng)?;
    let setup_time = setup_start.elapsed();

    let mut proof_rng = ChaCha20Rng::from_seed([0x72; 32]);
    let prove_start = Instant::now();
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        RistrettoBridgeCircuit::from_witness(fixture.clone()),
        &mut proof_rng,
    )?;
    let prove_time = prove_start.elapsed();
    let verify_start = Instant::now();
    let verified = Groth16::<Bn254>::verify(&verifying_key, &fixture.public_inputs(), &proof)?;
    let verify_time = verify_start.elapsed();
    assert!(verified);
    println!(
        "ristretto_bridge_setup_ms={:.3}, ristretto_bridge_prove_ms={:.3}, ristretto_bridge_verify_ms={:.3}",
        setup_time.as_secs_f64() * 1_000.0,
        prove_time.as_secs_f64() * 1_000.0,
        verify_time.as_secs_f64() * 1_000.0,
    );
    Ok(())
}

#[derive(Clone, Debug)]
struct AffinePoint {
    x: F25519,
    y: F25519,
}

#[derive(Clone)]
struct EdwardsPointVar {
    x: F25519Var,
    y: F25519Var,
    z: F25519Var,
    t: F25519Var,
}

impl EdwardsPointVar {
    fn constant(point: AffinePoint) -> Self {
        Self {
            x: F25519Var::constant(point.x),
            y: F25519Var::constant(point.y),
            z: F25519Var::one(),
            t: F25519Var::constant(point.x * point.y),
        }
    }

    fn identity() -> Self {
        Self {
            x: F25519Var::zero(),
            y: F25519Var::one(),
            z: F25519Var::one(),
            t: F25519Var::zero(),
        }
    }

    fn add(&self, other: &Self) -> Result<Self, SynthesisError> {
        let d = F25519Var::constant(edwards_d());
        let two = F25519Var::constant(F25519::from(2u64));
        let a = (&self.y - &self.x) * (&other.y - &other.x);
        let b = (&self.y + &self.x) * (&other.y + &other.x);
        let c = &two * &d * &self.t * &other.t;
        let dd = &two * &self.z * &other.z;
        let e = &b - &a;
        let f = &dd - &c;
        let g = &dd + &c;
        let h = &b + &a;
        Ok(Self {
            x: &e * &f,
            y: &g * &h,
            z: &f * &g,
            t: &e * &h,
        })
    }

    fn select(
        bit: &Boolean<Fr>,
        when_true: &Self,
        when_false: &Self,
    ) -> Result<Self, SynthesisError> {
        Ok(Self {
            x: F25519Var::conditionally_select(bit, &when_true.x, &when_false.x)?,
            y: F25519Var::conditionally_select(bit, &when_true.y, &when_false.y)?,
            z: F25519Var::conditionally_select(bit, &when_true.z, &when_false.z)?,
            t: F25519Var::conditionally_select(bit, &when_true.t, &when_false.t)?,
        })
    }

    fn enforce_ristretto_equal(&self, other: &Self) -> Result<(), SynthesisError> {
        let xy_equal = (&self.x * &other.y).is_eq(&(&self.y * &other.x))?;
        let xx_equal = (&self.x * &other.x).is_eq(&(&self.y * &other.y))?;
        (&xy_equal | &xx_equal).enforce_equal(&Boolean::TRUE)
    }
}

fn fixed_base_scalar_mul_le(
    base: RistrettoPoint,
    bits: &[Boolean<Fr>],
) -> Result<EdwardsPointVar, SynthesisError> {
    let identity = EdwardsPointVar::identity();
    let mut accumulator = EdwardsPointVar::identity();
    let mut power = base;
    for bit in bits {
        let constant = EdwardsPointVar::constant(decode_point(power.compress().to_bytes())?);
        let selected = EdwardsPointVar::select(bit, &constant, &identity)?;
        accumulator = accumulator.add(&selected)?;
        power += power;
    }
    Ok(accumulator)
}

fn decode_point_with_fields(
    cs: ConstraintSystemRef<Fr>,
    compressed: Option<[u8; 32]>,
    fields: &[FpVar<Fr>],
) -> Result<EdwardsPointVar, SynthesisError> {
    let point = compressed.map(decode_point).transpose()?;
    let s_value = compressed.map(|bytes| F25519::from_le_bytes_mod_order(&bytes));
    let s = F25519Var::new_witness(cs.clone(), || {
        s_value.ok_or(SynthesisError::AssignmentMissing)
    })?;
    let x = F25519Var::new_witness(cs.clone(), || {
        point
            .as_ref()
            .map(|point| point.x)
            .ok_or(SynthesisError::AssignmentMissing)
    })?;
    let y = F25519Var::new_witness(cs.clone(), || {
        point
            .as_ref()
            .map(|point| point.y)
            .ok_or(SynthesisError::AssignmentMissing)
    })?;

    bind_emulated_element_to_fields(fields, &s)?;
    let one = F25519Var::one();
    let four = F25519Var::constant(F25519::from(4u64));
    let ss = &s * &s;
    let u1 = &one - &ss;
    let u2 = &one + &ss;
    let v = F25519Var::constant(-edwards_d()) * &u1 * &u1 - &u2 * &u2;

    (&x * &x * &v).enforce_equal(&(&four * &ss))?;
    (&u2 * &y).enforce_equal(&u1)?;
    (&y * &y.inverse()?).enforce_equal(&one)?;

    let s_bits = s.to_bits_le()?;
    let x_bits = x.to_bits_le()?;
    let t = &x * &y;
    let t_bits = t.to_bits_le()?;
    s_bits[0].enforce_equal(&Boolean::FALSE)?;
    x_bits[0].enforce_equal(&Boolean::FALSE)?;
    t_bits[0].enforce_equal(&Boolean::FALSE)?;

    Ok(EdwardsPointVar { x, y, z: one, t })
}

fn allocate_public_packed_point(
    cs: ConstraintSystemRef<Fr>,
    compressed: Option<[u8; 32]>,
) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let low_value = compressed.map(|bytes| Fr::from_le_bytes_mod_order(&bytes[..16]));
    let high_value = compressed.map(|bytes| Fr::from_le_bytes_mod_order(&bytes[16..]));
    let low = FpVar::new_input(cs.clone(), || {
        low_value.ok_or(SynthesisError::AssignmentMissing)
    })?;
    let high = FpVar::new_input(cs, || high_value.ok_or(SynthesisError::AssignmentMissing))?;
    for bit in low
        .to_bits_le()?
        .iter()
        .skip(128)
        .chain(high.to_bits_le()?.iter().skip(128))
    {
        bit.enforce_equal(&Boolean::FALSE)?;
    }
    Ok(vec![low, high])
}

fn bind_emulated_element_to_fields(
    fields: &[FpVar<Fr>],
    element: &F25519Var,
) -> Result<(), SynthesisError> {
    if fields.len() != 2 {
        return Err(SynthesisError::Unsatisfiable);
    }
    let low_bits = fields[0].to_bits_le()?;
    let high_bits = fields[1].to_bits_le()?;
    let element_bits = element.to_bits_le()?;
    for index in 0..128 {
        element_bits[index].enforce_equal(&low_bits[index])?;
    }
    for index in 0..127 {
        element_bits[index + 128].enforce_equal(&high_bits[index])?;
    }
    high_bits[127].enforce_equal(&Boolean::FALSE)
}

fn allocate_u64_bits(
    cs: ConstraintSystemRef<Fr>,
    value: Option<u64>,
    bit_count: usize,
) -> Result<(Vec<Boolean<Fr>>, FpVar<Fr>), SynthesisError> {
    let bits = (0..bit_count)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                value
                    .map(|value| ((value >> index) & 1) == 1)
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let variable = bits_to_fp(&bits);
    Ok((bits, variable))
}

fn allocate_scalar_bits(
    cs: ConstraintSystemRef<Fr>,
    scalar: Option<[u8; 32]>,
) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    (0..OPENING_BITS)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                scalar
                    .map(|bytes| ((bytes[index / 8] >> (index % 8)) & 1) == 1)
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect()
}

fn bits_to_fp(bits: &[Boolean<Fr>]) -> FpVar<Fr> {
    let mut coefficient = Fr::one();
    let mut value = FpVar::zero();
    for bit in bits {
        value += FpVar::from(bit.clone()) * coefficient;
        coefficient *= Fr::from(2u64);
    }
    value
}

fn batching_scalar(base: [u8; 32], quote: [u8; 32]) -> Scalar {
    let digest = batching_digest(base, quote);
    let mut bytes = [0u8; 32];
    let digest_bytes = digest.into_bigint().to_bytes_le();
    bytes[..16].copy_from_slice(&digest_bytes[..16]);
    Scalar::from_bytes_mod_order(bytes)
}

fn batching_digest(base: [u8; 32], quote: [u8; 32]) -> Fr {
    let mut payload = vec![Fr::from_le_bytes_mod_order(BRIDGE_DOMAIN), Fr::from(1u64)];
    append_packed_point(&mut payload, base);
    append_packed_point(&mut payload, quote);
    transcript_hash_native(&payload)
}

fn decode_point(compressed: [u8; 32]) -> Result<AffinePoint, SynthesisError> {
    if CompressedRistretto(compressed).decompress().is_none() {
        return Err(SynthesisError::Unsatisfiable);
    }
    let s = F25519::from_le_bytes_mod_order(&compressed);
    if is_negative(s) {
        return Err(SynthesisError::Unsatisfiable);
    }
    let one = F25519::one();
    let ss = s.square();
    let u1 = one - ss;
    let u2 = one + ss;
    let v = -edwards_d() * u1.square() - u2.square();
    let mut x = (F25519::from(4u64) * ss * v.inverse().ok_or(SynthesisError::Unsatisfiable)?)
        .sqrt()
        .ok_or(SynthesisError::Unsatisfiable)?;
    if is_negative(x) {
        x = -x;
    }
    let y = u1 * u2.inverse().ok_or(SynthesisError::Unsatisfiable)?;
    if y.is_zero() || is_negative(x * y) {
        return Err(SynthesisError::Unsatisfiable);
    }
    Ok(AffinePoint { x, y })
}

fn edwards_d() -> F25519 {
    -F25519::from(121665u64) * F25519::from(121666u64).inverse().unwrap()
}

fn ristretto_h() -> RistrettoPoint {
    RistrettoPoint::hash_from_bytes::<Sha3_512>(RISTRETTO_BASEPOINT_POINT.compress().as_bytes())
}

fn is_negative(value: F25519) -> bool {
    value.into_bigint().is_odd()
}

fn append_packed_point(inputs: &mut Vec<Fr>, point: [u8; 32]) {
    inputs.push(Fr::from_le_bytes_mod_order(&point[..16]));
    inputs.push(Fr::from_le_bytes_mod_order(&point[16..]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn satisfied(witness: RistrettoBridgeWitness) -> bool {
        let cs = ConstraintSystem::<Fr>::new_ref();
        RistrettoBridgeCircuit::from_witness(witness)
            .generate_constraints(cs.clone())
            .unwrap();
        cs.is_satisfied().unwrap()
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn batched_bridge_is_satisfied() {
        let witness = RistrettoBridgeWitness::deterministic();
        assert!(witness.native_batch_precheck());
        assert!(satisfied(witness));
        eprintln!("bridge shape: {:?}", circuit_shape().unwrap());
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn wrong_base_amount_is_rejected() {
        let mut witness = RistrettoBridgeWitness::deterministic();
        witness.base_amount += 1;
        assert!(!satisfied(witness));
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn wrong_quote_amount_is_rejected() {
        let mut witness = RistrettoBridgeWitness::deterministic();
        witness.quote_amount += 1;
        assert!(!satisfied(witness));
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn wrong_batched_opening_is_rejected() {
        let mut witness = RistrettoBridgeWitness::deterministic();
        let opening = Scalar::from_canonical_bytes(witness.batched_opening).unwrap();
        witness.batched_opening = (opening + Scalar::ONE).to_bytes();
        assert!(!satisfied(witness));
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn source_commitment_change_invalidates_native_batch_and_proof() {
        let mut witness = RistrettoBridgeWitness::deterministic();
        let base = CompressedRistretto(witness.base_commitment)
            .decompress()
            .unwrap();
        witness.base_commitment = (base + RISTRETTO_BASEPOINT_POINT).compress().to_bytes();
        assert!(!witness.native_batch_precheck());
        assert!(!satisfied(witness));
    }

    #[test]
    #[ignore = "multi-million-constraint bridge test; run in release mode"]
    fn wrong_batched_commitment_is_rejected() {
        let mut witness = RistrettoBridgeWitness::deterministic();
        let point = CompressedRistretto(witness.batched_commitment)
            .decompress()
            .unwrap();
        witness.batched_commitment = (point + RISTRETTO_BASEPOINT_POINT).compress().to_bytes();
        assert!(!witness.native_batch_precheck());
        assert!(!satisfied(witness));
    }
}

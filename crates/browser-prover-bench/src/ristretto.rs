//! R1CS implementation of Solana's Ristretto255 twisted-ElGamal relation.
//!
//! The circuit field is BN254 Fr while the encrypted-order arithmetic lives in
//! F_(2^255-19), so all Ristretto field operations use Arkworks' emulated-field
//! gadget. Public points remain the canonical 32-byte Ristretto encodings used
//! by `solana-zk-sdk`; each encoding is exposed as two 128-bit BN254 inputs.

use {
    ark_bn254::Fr,
    ark_ff::{BigInteger, Field, Fp256, MontBackend, MontConfig, One, PrimeField, Zero},
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
    curve25519_dalek::{
        constants::RISTRETTO_BASEPOINT_POINT,
        ristretto::{CompressedRistretto, RistrettoPoint},
        scalar::Scalar,
    },
    sha3::Sha3_512,
};

#[derive(MontConfig)]
#[modulus = "57896044618658097711785492504343953926634992332820282019728792003956564819949"]
#[generator = "2"]
pub struct F25519Config;

type F25519 = Fp256<MontBackend<F25519Config, 4>>;
type F25519Var = EmulatedFpVar<F25519, Fr>;

const OPENING_BITS: usize = 253;
const QUANTITY_BITS: usize = 4;

#[derive(Clone, Debug)]
pub struct BindingWitness {
    pub quantity: u8,
    pub opening: [u8; 32],
    pub ciphertext: [u8; 64],
}

#[derive(Clone, Debug)]
pub struct RistrettoBindingCircuit {
    pub public_key: Option<[u8; 32]>,
    pub bindings: Vec<Option<BindingWitness>>,
}

impl RistrettoBindingCircuit {
    pub fn blank(binding_count: usize) -> Self {
        Self {
            public_key: None,
            bindings: vec![None; binding_count],
        }
    }

    pub fn deterministic(binding_count: usize) -> Self {
        let h = ristretto_h();
        let secret = Scalar::from(7u64);
        let public_key = secret.invert() * h;
        let mut bindings = Vec::with_capacity(binding_count);

        for index in 0..binding_count {
            let quantity = ((index * 7 + 3) % 16) as u8;
            let mut opening_bytes = [0u8; 32];
            opening_bytes[..8]
                .copy_from_slice(&(0x8f31_5a77_u64 + index as u64 * 7919).to_le_bytes());
            let opening = Scalar::from_bytes_mod_order(opening_bytes);
            let commitment =
                Scalar::from(quantity as u64) * RISTRETTO_BASEPOINT_POINT + opening * h;
            let handle = opening * public_key;
            let mut ciphertext = [0u8; 64];
            ciphertext[..32].copy_from_slice(commitment.compress().as_bytes());
            ciphertext[32..].copy_from_slice(handle.compress().as_bytes());
            bindings.push(Some(BindingWitness {
                quantity,
                opening: opening.to_bytes(),
                ciphertext,
            }));
        }

        Self {
            public_key: Some(public_key.compress().to_bytes()),
            bindings,
        }
    }

    pub fn public_inputs(&self) -> Vec<Fr> {
        let mut inputs = Vec::with_capacity(2 + self.bindings.len() * 4);
        append_packed_point(
            &mut inputs,
            self.public_key.expect("witness circuit has a key"),
        );
        for binding in &self.bindings {
            let binding = binding.as_ref().expect("witness circuit has ciphertexts");
            append_packed_point(
                &mut inputs,
                binding.ciphertext[..32].try_into().expect("fixed slice"),
            );
            append_packed_point(
                &mut inputs,
                binding.ciphertext[32..].try_into().expect("fixed slice"),
            );
        }
        inputs
    }
}

impl ConstraintSynthesizer<Fr> for RistrettoBindingCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let key = decode_public_point(cs.clone(), self.public_key)?;
        let generator = EdwardsPointVar::constant(decode_point(
            RISTRETTO_BASEPOINT_POINT.compress().to_bytes(),
        )?);
        let blinding_base =
            EdwardsPointVar::constant(decode_point(ristretto_h().compress().to_bytes())?);

        for binding in self.bindings {
            let commitment_bytes = binding.as_ref().map(|value| {
                value.ciphertext[..32]
                    .try_into()
                    .expect("ciphertext half is fixed width")
            });
            let handle_bytes = binding.as_ref().map(|value| {
                value.ciphertext[32..]
                    .try_into()
                    .expect("ciphertext half is fixed width")
            });
            let commitment = decode_public_point(cs.clone(), commitment_bytes)?;
            let handle = decode_public_point(cs.clone(), handle_bytes)?;
            let quantity_bits =
                allocate_quantity_bits(cs.clone(), binding.as_ref().map(|value| value.quantity))?;
            let opening_bits =
                allocate_opening_bits(cs.clone(), binding.as_ref().map(|value| value.opening))?;

            let expected_commitment = generator
                .scalar_mul_le(&quantity_bits)?
                .add(&blinding_base.scalar_mul_le(&opening_bits)?)?;
            let expected_handle = key.scalar_mul_le(&opening_bits)?;
            expected_commitment.enforce_ristretto_equal(&commitment)?;
            expected_handle.enforce_ristretto_equal(&handle)?;
        }
        Ok(())
    }
}

pub fn circuit_shape(binding_count: usize) -> Result<(usize, usize, usize), SynthesisError> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    RistrettoBindingCircuit::deterministic(binding_count).generate_constraints(cs.clone())?;
    if !cs.is_satisfied()? {
        return Err(SynthesisError::Unsatisfiable);
    }
    Ok((
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables(),
    ))
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

    fn scalar_mul_le(&self, bits: &[Boolean<Fr>]) -> Result<Self, SynthesisError> {
        let mut accumulator = Self::identity();
        let mut power = self.clone();
        for bit in bits {
            let candidate = accumulator.add(&power)?;
            accumulator = Self::select(bit, &candidate, &accumulator)?;
            power = power.add(&power)?;
        }
        Ok(accumulator)
    }

    /// Ristretto points are equivalence classes of Edwards points. This is the
    /// equality test used by curve25519-dalek, written without affine inversions.
    fn enforce_ristretto_equal(&self, other: &Self) -> Result<(), SynthesisError> {
        let xy_equal = (&self.x * &other.y).is_eq(&(&self.y * &other.x))?;
        let xx_equal = (&self.x * &other.x).is_eq(&(&self.y * &other.y))?;
        (&xy_equal | &xx_equal).enforce_equal(&Boolean::TRUE)
    }
}

fn decode_public_point(
    cs: ConstraintSystemRef<Fr>,
    compressed: Option<[u8; 32]>,
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

    bind_emulated_element_to_public_bytes(cs, compressed, &s)?;
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

fn bind_emulated_element_to_public_bytes(
    cs: ConstraintSystemRef<Fr>,
    compressed: Option<[u8; 32]>,
    element: &F25519Var,
) -> Result<(), SynthesisError> {
    let low_value = compressed.map(|bytes| Fr::from_le_bytes_mod_order(&bytes[..16]));
    let high_value = compressed.map(|bytes| Fr::from_le_bytes_mod_order(&bytes[16..]));
    let low = FpVar::new_input(cs.clone(), || {
        low_value.ok_or(SynthesisError::AssignmentMissing)
    })?;
    let high = FpVar::new_input(cs, || high_value.ok_or(SynthesisError::AssignmentMissing))?;
    let low_bits = low.to_bits_le()?;
    let high_bits = high.to_bits_le()?;
    for bit in low_bits.iter().skip(128).chain(high_bits.iter().skip(128)) {
        bit.enforce_equal(&Boolean::FALSE)?;
    }
    let element_bits = element.to_bits_le()?;
    for index in 0..128 {
        element_bits[index].enforce_equal(&low_bits[index])?;
    }
    for index in 0..127 {
        element_bits[index + 128].enforce_equal(&high_bits[index])?;
    }
    high_bits[127].enforce_equal(&Boolean::FALSE)
}

fn allocate_quantity_bits(
    cs: ConstraintSystemRef<Fr>,
    quantity: Option<u8>,
) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    (0..QUANTITY_BITS)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                quantity
                    .map(|quantity| ((quantity >> index) & 1) == 1)
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect()
}

fn allocate_opening_bits(
    cs: ConstraintSystemRef<Fr>,
    opening: Option<[u8; 32]>,
) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    (0..OPENING_BITS)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                opening
                    .map(|opening| ((opening[index / 8] >> (index % 8)) & 1) == 1)
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect()
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

    #[test]
    fn solana_compatible_binding_is_satisfied() {
        let circuit = RistrettoBindingCircuit::deterministic(1);
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        eprintln!(
            "constraints={} instance_variables={} witness_variables={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
        assert_eq!(cs.num_constraints(), 5_176_032);
        assert_eq!(cs.num_instance_variables(), 7);
        assert_eq!(cs.num_witness_variables(), 5_150_283);
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn shared_key_decode_shape() {
        let circuit = RistrettoBindingCircuit::deterministic(0);
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        eprintln!(
            "constraints={} instance_variables={} witness_variables={}",
            cs.num_constraints(),
            cs.num_instance_variables(),
            cs.num_witness_variables()
        );
        assert_eq!(cs.num_constraints(), 12_730);
        assert_eq!(cs.num_instance_variables(), 3);
        assert_eq!(cs.num_witness_variables(), 11_780);
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn modified_ciphertext_is_rejected() {
        let mut circuit = RistrettoBindingCircuit::deterministic(1);
        let binding = circuit.bindings[0].as_mut().unwrap();
        let point = CompressedRistretto(binding.ciphertext[..32].try_into().unwrap())
            .decompress()
            .unwrap();
        binding.ciphertext[..32]
            .copy_from_slice((point + RISTRETTO_BASEPOINT_POINT).compress().as_bytes());
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}

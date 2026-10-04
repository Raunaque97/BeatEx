use ark_bn254::{Fq, Fr, G1Projective};
use ark_ec::{AdditiveGroup, CurveGroup, PrimeGroup};
use ark_ff::{BigInteger, PrimeField, Zero};
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{emulated_fp::EmulatedFpVar, FieldVar},
    prelude::CondSelectGadget,
};
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError, SynthesisMode,
};

type FqVar = EmulatedFpVar<Fq, Fr>;

const RANDOMNESS_BITS: usize = Fr::MODULUS_BIT_SIZE as usize;
const QUANTITY_BITS: usize = 4;

#[derive(Clone)]
struct G1Var {
    // Complete RCB formulas use homogeneous coordinates (x / z, y / z).
    x: FqVar,
    y: FqVar,
    z: FqVar,
}

impl G1Var {
    fn identity() -> Self {
        Self {
            x: FqVar::zero(),
            y: FqVar::one(),
            z: FqVar::zero(),
        }
    }

    fn constant(point: G1Projective) -> Self {
        if point.is_zero() {
            return Self::identity();
        }
        let affine = point.into_affine();
        Self {
            x: FqVar::constant(affine.x),
            y: FqVar::constant(affine.y),
            z: FqVar::one(),
        }
    }

    fn witness(
        cs: ConstraintSystemRef<Fr>,
        point: Option<G1Projective>,
    ) -> Result<Self, SynthesisError> {
        let affine = point.map(CurveGroup::into_affine);
        let x = FqVar::new_witness(cs.clone(), || {
            affine
                .map(|point| point.x)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let y = FqVar::new_witness(cs, || {
            affine
                .map(|point| point.y)
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let result = Self {
            x,
            y,
            z: FqVar::one(),
        };
        result.enforce_on_curve()?;
        Ok(result)
    }

    fn enforce_on_curve(&self) -> Result<(), SynthesisError> {
        // Homogeneous y^2*z = x^3 + 3*z^3 for alt_bn128 G1.
        let yy = &self.y * &self.y;
        let lhs = &yy * &self.z;
        let xx = &self.x * &self.x;
        let xxx = &xx * &self.x;
        let zz = &self.z * &self.z;
        let zzz = &zz * &self.z;
        lhs.enforce_equal(&(xxx + zzz * Fq::from(3u64)))
    }

    fn add(&self, other: &Self) -> Self {
        // Complete addition for a=0 short-Weierstrass curves, RCB 2015,
        // matching the generic Arkworks ProjectiveVar formula.
        let xx = &self.x * &other.x;
        let yy = &self.y * &other.y;
        let zz = &self.z * &other.z;
        let xy_pairs = (&self.x + &self.y) * (&other.x + &other.y) - (&xx + &yy);
        let xz_pairs = (&self.x + &self.z) * (&other.x + &other.z) - (&xx + &zz);
        let yz_pairs = (&self.y + &self.z) * (&other.y + &other.z) - (&yy + &zz);
        let bzz3 = &zz * Fq::from(9u64);
        let yy_m_bzz3 = &yy - &bzz3;
        let yy_p_bzz3 = &yy + &bzz3;
        let xx3 = &xx + &xx + &xx;
        let b3_xz_pairs = &xz_pairs * Fq::from(9u64);
        let x = &yy_m_bzz3 * &xy_pairs - &yz_pairs * &b3_xz_pairs;
        let y = &yy_p_bzz3 * &yy_m_bzz3 + &xx3 * &b3_xz_pairs;
        let z = &yy_p_bzz3 * &yz_pairs + &xy_pairs * &xx3;
        Self { x, y, z }
    }

    fn select(
        bit: &Boolean<Fr>,
        when_true: &Self,
        when_false: &Self,
    ) -> Result<Self, SynthesisError> {
        Ok(Self {
            x: FqVar::conditionally_select(bit, &when_true.x, &when_false.x)?,
            y: FqVar::conditionally_select(bit, &when_true.y, &when_false.y)?,
            z: FqVar::conditionally_select(bit, &when_true.z, &when_false.z)?,
        })
    }

    fn fixed_base_mul(
        bits: &[Boolean<Fr>],
        base: G1Projective,
    ) -> Result<Self, SynthesisError> {
        let mut result = Self::identity();
        let mut power = base;
        for bit in bits {
            let candidate = result.add(&Self::constant(power));
            result = Self::select(bit, &candidate, &result)?;
            power.double_in_place();
        }
        Ok(result)
    }

    fn variable_base_mul(&self, bits: &[Boolean<Fr>]) -> Result<Self, SynthesisError> {
        let mut result = Self::identity();
        let mut power = self.clone();
        for bit in bits {
            let candidate = result.add(&power);
            result = Self::select(bit, &candidate, &result)?;
            power = power.add(&power);
        }
        Ok(result)
    }

    fn enforce_equal(&self, other: &Self) -> Result<(), SynthesisError> {
        (&self.x * &other.z).enforce_equal(&(&other.x * &self.z))?;
        (&self.y * &other.z).enforce_equal(&(&other.y * &self.z))
    }
}

#[derive(Clone)]
struct OneCipherCircuit {
    key: Option<G1Projective>,
    randomness: Option<Fr>,
    quantity: Option<u8>,
    expected_ephemeral: Option<G1Projective>,
    expected_body: Option<G1Projective>,
}

impl OneCipherCircuit {
    fn deterministic() -> Self {
        let generator = G1Projective::generator();
        let secret = Fr::from(7u64);
        let key = generator.mul_bigint(secret.into_bigint());
        let randomness = Fr::from(0x8f31_5a77_u64);
        let quantity = 11u8;
        let expected_ephemeral = generator.mul_bigint(randomness.into_bigint());
        let expected_body = key.mul_bigint(randomness.into_bigint())
            + generator.mul_bigint(Fr::from(quantity).into_bigint());
        Self {
            key: Some(key),
            randomness: Some(randomness),
            quantity: Some(quantity),
            expected_ephemeral: Some(expected_ephemeral),
            expected_body: Some(expected_body),
        }
    }
}

impl ConstraintSynthesizer<Fr> for OneCipherCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let key = G1Var::witness(cs.clone(), self.key)?;
        let randomness = self.randomness.map(|value| {
            let bigint = value.into_bigint();
            (0..RANDOMNESS_BITS)
                .map(|index| bigint.get_bit(index))
                .collect::<Vec<_>>()
        });
        let randomness_bits = (0..RANDOMNESS_BITS)
            .map(|index| {
                Boolean::new_witness(cs.clone(), || {
                    randomness
                        .as_ref()
                        .map(|bits| bits[index])
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let quantity_bits = (0..QUANTITY_BITS)
            .map(|index| {
                Boolean::new_witness(cs.clone(), || {
                    self.quantity
                        .map(|quantity| ((quantity >> index) & 1) == 1)
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let generator = G1Projective::generator();
        let ephemeral = G1Var::fixed_base_mul(&randomness_bits, generator)?;
        let shared = key.variable_base_mul(&randomness_bits)?;
        let message = G1Var::fixed_base_mul(&quantity_bits, generator)?;
        let body = shared.add(&message);

        let expected_ephemeral = G1Var::witness(cs.clone(), self.expected_ephemeral)?;
        let expected_body = G1Var::witness(cs, self.expected_body)?;
        ephemeral.enforce_equal(&expected_ephemeral)?;
        body.enforce_equal(&expected_body)
    }
}

fn main() -> Result<(), SynthesisError> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    cs.set_optimization_goal(ark_relations::r1cs::OptimizationGoal::Constraints);
    cs.set_mode(SynthesisMode::Setup);
    OneCipherCircuit::deterministic().generate_constraints(cs.clone())?;
    println!(
        "constraints={} instance_variables={} witness_variables={}",
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ciphertext_is_satisfied() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        OneCipherCircuit::deterministic()
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(cs.is_satisfied().unwrap());
    }

    #[test]
    fn mutated_body_is_rejected() {
        let mut circuit = OneCipherCircuit::deterministic();
        circuit.expected_body = circuit.expected_body.map(|point| point + G1Projective::generator());
        let cs = ConstraintSystem::<Fr>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}

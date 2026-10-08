//! Versioned Groth16 verification pipeline.
//!
//! The current verifying key belongs to a deliberately tiny square circuit.
//! It validates BN254 serialization, proof-A negation, native syscall routing,
//! and public-input rejection before the final order/claim circuits are frozen.
//! It does **not** validate an order or authorize token movement.

use {
    crate::{queue::QueueVerificationKey, BeatExError},
    anchor_lang::prelude::*,
};

#[cfg(not(target_os = "solana"))]
use solana_bn254::prelude::{alt_bn128_addition, alt_bn128_multiplication, alt_bn128_pairing};
#[cfg(target_os = "solana")]
use solana_define_syscall::definitions as syscalls;

#[allow(dead_code)]
mod generated_constants {
    include!(concat!(env!("OUT_DIR"), "/crypto_constants.rs"));
}
#[cfg(test)]
use generated_constants::{
    GROTH16_PIPELINE_VK_ALPHA_G1, GROTH16_PIPELINE_VK_BETA_G2, GROTH16_PIPELINE_VK_DELTA_G2,
    GROTH16_PIPELINE_VK_GAMMA_G2, GROTH16_PIPELINE_VK_IC,
};

// Scalar-field modulus for BN254, encoded big-endian. Public inputs must be
// canonical before they are passed to the multiplication syscall. Keeping the
// comparison byte-oriented avoids the heap allocation performed by BigUint.
const BN254_SCALAR_MODULUS_BE: [u8; 32] = [
    48, 100, 78, 114, 225, 49, 160, 41, 184, 80, 69, 182, 129, 129, 88, 93, 40, 51, 232, 72, 121,
    185, 112, 145, 67, 225, 245, 147, 240, 0, 0, 1,
];

#[cfg(target_os = "solana")]
const ALT_BN128_ADD: u64 = 0;
#[cfg(target_os = "solana")]
const ALT_BN128_MUL: u64 = 2;
#[cfg(target_os = "solana")]
const ALT_BN128_PAIRING: u64 = 3;

struct VerificationKeyRef<'a> {
    alpha_g1: &'a [u8; 64],
    beta_g2: &'a [u8; 128],
    gamma_g2: &'a [u8; 128],
    delta_g2: &'a [u8; 128],
    ic: &'a [[u8; 64]; 2],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct Groth16ProofInput {
    /// Proof A is already negated and encoded as two 32-byte big-endian limbs.
    pub proof_a: [u8; 64],
    /// Proof B uses the Solana alt_bn128 G2 big-endian layout.
    pub proof_b: [u8; 128],
    /// Proof C uses the Solana alt_bn128 G1 big-endian layout.
    pub proof_c: [u8; 64],
    /// A canonical BN254 scalar encoded big-endian.
    pub public_input: [u8; 32],
}

#[cfg(test)]
pub fn verify_pipeline_proof(proof: &Groth16ProofInput) -> Result<()> {
    verify_one_public_input(
        proof,
        VerificationKeyRef {
            alpha_g1: &GROTH16_PIPELINE_VK_ALPHA_G1,
            beta_g2: &GROTH16_PIPELINE_VK_BETA_G2,
            gamma_g2: &GROTH16_PIPELINE_VK_GAMMA_G2,
            delta_g2: &GROTH16_PIPELINE_VK_DELTA_G2,
            ic: &GROTH16_PIPELINE_VK_IC,
        },
    )
}

/// Verify a one-public-input queue circuit with an authority-installed,
/// circuit-versioned key PDA.
pub fn verify_queue_proof(
    proof: &Groth16ProofInput,
    expected_public_input_le: &[u8; 32],
    key: &QueueVerificationKey,
) -> Result<()> {
    let mut expected_be = *expected_public_input_le;
    expected_be.reverse();
    require!(
        proof.public_input == expected_be,
        BeatExError::Groth16PublicInputMismatch
    );
    verify_one_public_input(
        proof,
        VerificationKeyRef {
            alpha_g1: &key.alpha_g1,
            beta_g2: &key.beta_g2,
            gamma_g2: &key.gamma_g2,
            delta_g2: &key.delta_g2,
            ic: &key.ic,
        },
    )
}

/// Allocation-lean Groth16 verifier for the one-public-input circuits used by
/// the program. `groth16-solana` builds every syscall input with `concat()` and
/// checks scalar canonicality through `BigUint`. Solana's bump allocator never
/// reclaims those temporary allocations during an instruction, which is
/// especially costly for replacement and rollover instructions that verify two
/// proofs atomically. Fixed-size syscall inputs keep those paths within the
/// 256-KiB transaction heap.
fn verify_one_public_input(proof: &Groth16ProofInput, key: VerificationKeyRef<'_>) -> Result<()> {
    let prepared = prepare_public_input(proof, &key)?;

    let mut pairing_input = [0u8; 768];
    let mut offset = 0;
    for bytes in [
        proof.proof_a.as_slice(),
        proof.proof_b.as_slice(),
        prepared.as_slice(),
        key.gamma_g2.as_slice(),
        proof.proof_c.as_slice(),
        key.delta_g2.as_slice(),
        key.alpha_g1.as_slice(),
        key.beta_g2.as_slice(),
    ] {
        let end = offset + bytes.len();
        pairing_input[offset..end].copy_from_slice(bytes);
        offset = end;
    }
    debug_assert_eq!(offset, pairing_input.len());
    require_pairing_identity(&pairing_input)
}

fn prepare_public_input(
    proof: &Groth16ProofInput,
    key: &VerificationKeyRef<'_>,
) -> Result<[u8; 64]> {
    require!(
        proof.public_input < BN254_SCALAR_MODULUS_BE,
        BeatExError::InvalidGroth16Proof
    );
    let multiplied = multiply_g1(&key.ic[1], &proof.public_input)?;
    add_g1(&multiplied, &key.ic[0])
}

fn require_pairing_identity(input: &[u8]) -> Result<()> {
    let pairing = pairing_output(input)?;
    let mut expected = [0u8; 32];
    expected[31] = 1;
    require!(pairing == expected, BeatExError::InvalidGroth16Proof);
    Ok(())
}

#[cfg(target_os = "solana")]
fn group_operation<const N: usize>(operation: u64, input: &[u8]) -> Result<[u8; N]> {
    let mut output = [0u8; N];
    // SAFETY: the syscall reads exactly `input.len()` bytes and writes the
    // fixed result size dictated by `operation`; every caller supplies that
    // operation's exact output buffer size.
    let status = unsafe {
        syscalls::sol_alt_bn128_group_op(
            operation,
            input.as_ptr(),
            input.len() as u64,
            output.as_mut_ptr(),
        )
    };
    require!(status == 0, BeatExError::InvalidGroth16Proof);
    Ok(output)
}

fn multiply_g1(point: &[u8; 64], scalar: &[u8; 32]) -> Result<[u8; 64]> {
    let mut input = [0u8; 96];
    input[..64].copy_from_slice(point);
    input[64..].copy_from_slice(scalar);
    #[cfg(target_os = "solana")]
    {
        group_operation(ALT_BN128_MUL, &input)
    }
    #[cfg(not(target_os = "solana"))]
    {
        let output = alt_bn128_multiplication(&input)
            .map_err(|_| error!(BeatExError::InvalidGroth16Proof))?;
        output
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidGroth16Proof))
    }
}

fn add_g1(left: &[u8; 64], right: &[u8; 64]) -> Result<[u8; 64]> {
    let mut input = [0u8; 128];
    input[..64].copy_from_slice(left);
    input[64..].copy_from_slice(right);
    #[cfg(target_os = "solana")]
    {
        group_operation(ALT_BN128_ADD, &input)
    }
    #[cfg(not(target_os = "solana"))]
    {
        let output =
            alt_bn128_addition(&input).map_err(|_| error!(BeatExError::InvalidGroth16Proof))?;
        output
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidGroth16Proof))
    }
}

fn pairing_output(input: &[u8]) -> Result<[u8; 32]> {
    #[cfg(target_os = "solana")]
    {
        group_operation(ALT_BN128_PAIRING, input)
    }
    #[cfg(not(target_os = "solana"))]
    {
        let output =
            alt_bn128_pairing(input).map_err(|_| error!(BeatExError::InvalidGroth16Proof))?;
        output
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidGroth16Proof))
    }
}

#[cfg(test)]
mod tests {
    use super::generated_constants::{
        GROTH16_PIPELINE_PROOF_A, GROTH16_PIPELINE_PROOF_B, GROTH16_PIPELINE_PROOF_C,
        GROTH16_PIPELINE_PUBLIC_INPUT,
    };
    use super::*;

    fn fixture() -> Groth16ProofInput {
        Groth16ProofInput {
            proof_a: GROTH16_PIPELINE_PROOF_A,
            proof_b: GROTH16_PIPELINE_PROOF_B,
            proof_c: GROTH16_PIPELINE_PROOF_C,
            public_input: GROTH16_PIPELINE_PUBLIC_INPUT,
        }
    }

    #[test]
    fn generated_fixture_verifies() {
        verify_pipeline_proof(&fixture()).unwrap();
    }

    #[test]
    fn mutated_public_input_is_rejected() {
        let mut proof = fixture();
        proof.public_input[31] ^= 1;
        assert!(verify_pipeline_proof(&proof).is_err());
    }

    #[test]
    fn mutated_proof_is_rejected() {
        let mut proof = fixture();
        proof.proof_c[17] ^= 1;
        assert!(verify_pipeline_proof(&proof).is_err());
    }

    #[test]
    fn non_canonical_public_input_is_rejected() {
        let mut proof = fixture();
        proof.public_input = BN254_SCALAR_MODULUS_BE;
        assert!(verify_pipeline_proof(&proof).is_err());
    }
}

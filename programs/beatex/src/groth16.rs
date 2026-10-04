//! Versioned Groth16 verification pipeline.
//!
//! The current verifying key belongs to a deliberately tiny square circuit.
//! It validates BN254 serialization, proof-A negation, native syscall routing,
//! and public-input rejection before the final order/claim circuits are frozen.
//! It does **not** validate an order or authorize token movement.

use {
    crate::{queue::QueueVerificationKey, BeatExError},
    anchor_lang::prelude::*,
    groth16_solana::groth16::{Groth16Verifier, Groth16Verifyingkey},
};

#[allow(dead_code)]
mod generated_constants {
    include!(concat!(env!("OUT_DIR"), "/crypto_constants.rs"));
}
use generated_constants::{
    GROTH16_PIPELINE_VK_ALPHA_G1, GROTH16_PIPELINE_VK_BETA_G2, GROTH16_PIPELINE_VK_DELTA_G2,
    GROTH16_PIPELINE_VK_GAMMA_G2, GROTH16_PIPELINE_VK_IC,
};

#[cfg(feature = "sbf-test-fixtures")]
use generated_constants::{
    GROTH16_PIPELINE_PROOF_A, GROTH16_PIPELINE_PROOF_B, GROTH16_PIPELINE_PROOF_C,
    GROTH16_PIPELINE_PUBLIC_INPUT,
};

pub const PIPELINE_CIRCUIT_VERSION: u16 = 1;

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

pub fn verify_pipeline_proof(proof: &Groth16ProofInput) -> Result<()> {
    let verifying_key = Groth16Verifyingkey {
        nr_pubinputs: 2,
        vk_alpha_g1: GROTH16_PIPELINE_VK_ALPHA_G1,
        vk_beta_g2: GROTH16_PIPELINE_VK_BETA_G2,
        vk_gamme_g2: GROTH16_PIPELINE_VK_GAMMA_G2,
        vk_delta_g2: GROTH16_PIPELINE_VK_DELTA_G2,
        vk_ic: &GROTH16_PIPELINE_VK_IC,
    };
    let public_inputs = [proof.public_input];
    let mut verifier = Groth16Verifier::<1>::new(
        &proof.proof_a,
        &proof.proof_b,
        &proof.proof_c,
        &public_inputs,
        &verifying_key,
    )
    .map_err(|_| error!(BeatExError::InvalidGroth16Proof))?;
    verifier
        .verify()
        .map_err(|_| error!(BeatExError::InvalidGroth16Proof))
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
    let verifying_key = Groth16Verifyingkey {
        nr_pubinputs: 2,
        vk_alpha_g1: key.alpha_g1,
        vk_beta_g2: key.beta_g2,
        vk_gamme_g2: key.gamma_g2,
        vk_delta_g2: key.delta_g2,
        vk_ic: &key.ic,
    };
    let public_inputs = [proof.public_input];
    let mut verifier = Groth16Verifier::<1>::new(
        &proof.proof_a,
        &proof.proof_b,
        &proof.proof_c,
        &public_inputs,
        &verifying_key,
    )
    .map_err(|_| error!(BeatExError::InvalidGroth16Proof))?;
    verifier
        .verify()
        .map_err(|_| error!(BeatExError::InvalidGroth16Proof))
}

#[cfg(feature = "sbf-test-fixtures")]
pub fn pipeline_fixture() -> Groth16ProofInput {
    Groth16ProofInput {
        proof_a: GROTH16_PIPELINE_PROOF_A,
        proof_b: GROTH16_PIPELINE_PROOF_B,
        proof_c: GROTH16_PIPELINE_PROOF_C,
        public_input: GROTH16_PIPELINE_PUBLIC_INPUT,
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
}

use {
    ark_bn254::{Bn254, Fr, G1Affine, G2Affine},
    ark_ff::{BigInteger, PrimeField},
    ark_groth16::Groth16,
    ark_relations::{
        lc,
        r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError},
    },
    ark_serialize::{CanonicalSerialize, Compress},
    curve25519_dalek::scalar::Scalar,
    rand::SeedableRng,
    rand_chacha::ChaCha20Rng,
    solana_zk_sdk::encryption::pedersen::{G, H},
    std::{
        env,
        fs::File,
        io::{BufWriter, Write},
        ops::Neg,
        path::PathBuf,
    },
};

#[derive(Clone)]
struct Groth16PipelineCircuit {
    witness: Option<Fr>,
}

impl ConstraintSynthesizer<Fr> for Groth16PipelineCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let witness =
            cs.new_witness_variable(|| self.witness.ok_or(SynthesisError::AssignmentMissing))?;
        let square = cs.new_input_variable(|| {
            let witness = self.witness.ok_or(SynthesisError::AssignmentMissing)?;
            Ok(witness * witness)
        })?;
        cs.enforce_constraint(lc!() + witness, lc!() + witness, lc!() + square)
    }
}

fn reverse_chunks<const CHUNK_SIZE: usize, const ARRAY_SIZE: usize>(
    bytes: &[u8; ARRAY_SIZE],
) -> [u8; ARRAY_SIZE] {
    let mut result = [0_u8; ARRAY_SIZE];
    for (source, destination) in bytes
        .chunks_exact(CHUNK_SIZE)
        .zip(result.chunks_exact_mut(CHUNK_SIZE))
    {
        for (left, right) in source.iter().zip(destination.iter_mut().rev()) {
            *right = *left;
        }
    }
    result
}

fn g1_bytes(point: G1Affine) -> [u8; 64] {
    let mut bytes = [0_u8; 64];
    point
        .x
        .serialize_with_mode(&mut bytes[..32], Compress::No)
        .expect("serialize G1 x");
    point
        .y
        .serialize_with_mode(&mut bytes[32..], Compress::No)
        .expect("serialize G1 y");
    reverse_chunks::<32, 64>(&bytes)
}

fn g2_bytes(point: G2Affine) -> [u8; 128] {
    let mut bytes = [0_u8; 128];
    point
        .serialize_with_mode(&mut bytes[..], Compress::No)
        .expect("serialize G2");
    reverse_chunks::<64, 128>(&bytes)
}

fn field_bytes(value: Fr) -> [u8; 32] {
    let encoded = value.into_bigint().to_bytes_be();
    let mut result = [0_u8; 32];
    result[32 - encoded.len()..].copy_from_slice(&encoded);
    result
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let output_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("crypto_constants.rs");
    let output = File::create(output_path).expect("create generated crypto constants");
    let mut output = BufWriter::new(output);

    writeln!(
        output,
        "pub const G_BYTES: [u8; 32] = {:?};",
        G.compress().to_bytes()
    )
    .expect("write G");
    writeln!(
        output,
        "pub const H_BYTES: [u8; 32] = {:?};",
        H.compress().to_bytes()
    )
    .expect("write H");
    writeln!(
        output,
        "pub const LAGRANGE_COEFFICIENTS: [([u8; 3], [[u8; 32]; 3]); 35] = ["
    )
    .expect("start coefficient table");

    for first in 1_u8..=5 {
        for second in (first + 1)..=6 {
            for third in (second + 1)..=7 {
                let indices = [first, second, third];
                let coefficients = indices.map(|index| {
                    let x_i = Scalar::from(index as u64);
                    let coefficient = indices
                        .iter()
                        .copied()
                        .filter(|other| *other != index)
                        .fold(Scalar::ONE, |coefficient, other| {
                            let x_j = Scalar::from(other as u64);
                            coefficient * (-x_j) * (x_i - x_j).invert()
                        });
                    (-coefficient).to_bytes()
                });

                writeln!(output, "    ({indices:?}, {coefficients:?}),")
                    .expect("write coefficient entry");
            }
        }
    }

    writeln!(output, "];\n").expect("finish coefficient table");

    // This deterministic one-constraint circuit is a serialization/syscall
    // pipeline fixture, not the production order-validity relation. Keeping
    // it generated here makes the exact proof-A negation and BN254 byte order
    // continuously testable on both the host and SBF.
    let mut rng = ChaCha20Rng::from_seed([0x42; 32]);
    let parameters = Groth16::<Bn254>::generate_random_parameters_with_reduction(
        Groth16PipelineCircuit { witness: None },
        &mut rng,
    )
    .expect("generate Groth16 fixture parameters");
    let witness = Fr::from(9_u64);
    let proof = Groth16::<Bn254>::create_random_proof_with_reduction(
        Groth16PipelineCircuit {
            witness: Some(witness),
        },
        &parameters,
        &mut rng,
    )
    .expect("generate Groth16 fixture proof");
    let verifying_key = parameters.vk;

    writeln!(
        output,
        "pub const GROTH16_PIPELINE_VK_ALPHA_G1: [u8; 64] = {:?};",
        g1_bytes(verifying_key.alpha_g1)
    )
    .expect("write Groth16 alpha");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_VK_BETA_G2: [u8; 128] = {:?};",
        g2_bytes(verifying_key.beta_g2)
    )
    .expect("write Groth16 beta");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_VK_GAMMA_G2: [u8; 128] = {:?};",
        g2_bytes(verifying_key.gamma_g2)
    )
    .expect("write Groth16 gamma");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_VK_DELTA_G2: [u8; 128] = {:?};",
        g2_bytes(verifying_key.delta_g2)
    )
    .expect("write Groth16 delta");
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(g1_bytes)
        .collect::<Vec<_>>();
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_VK_IC: [[u8; 64]; 2] = {ic:?};"
    )
    .expect("write Groth16 IC");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_PROOF_A: [u8; 64] = {:?};",
        g1_bytes(proof.a.neg())
    )
    .expect("write Groth16 proof A");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_PROOF_B: [u8; 128] = {:?};",
        g2_bytes(proof.b)
    )
    .expect("write Groth16 proof B");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_PROOF_C: [u8; 64] = {:?};",
        g1_bytes(proof.c)
    )
    .expect("write Groth16 proof C");
    writeln!(
        output,
        "pub const GROTH16_PIPELINE_PUBLIC_INPUT: [u8; 32] = {:?};",
        field_bytes(witness * witness)
    )
    .expect("write Groth16 public input");
}

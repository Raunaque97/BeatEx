//! Isolated SBF cost spike for direct Baby-Jubjub ElGamal aggregation.
//!
//! The benchmark intentionally repeats one compressed point 68 times so the
//! legacy transaction wrapper used by LiteSVM does not reject the test before
//! execution. The executed SBF work is the same 68 decompressions and mixed
//! additions needed by a 17-level order with two sides and two ElGamal points.

use {
    ark_ec::{twisted_edwards::TECurveConfig, CurveGroup, PrimeGroup},
    ark_ed_on_bn254::{EdwardsAffine, EdwardsProjective, Fq},
    ark_ff::{BigInt, BigInteger, Field, MontFp, One, PrimeField, Zero},
    ark_serialize::CanonicalDeserialize,
    solana_account_info::{next_account_info, AccountInfo},
    solana_program_entrypoint::{entrypoint, ProgramResult},
    solana_program_error::ProgramError,
    solana_pubkey::Pubkey,
};

const FR_MODULUS_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58,
    0x5d, 0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93, 0xf0, 0x00,
    0x00, 0x01,
];
const FR_MINUS_TWO_BE: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58,
    0x5d, 0x28, 0x33, 0xe8, 0x48, 0x79, 0xb9, 0x70, 0x91, 0x43, 0xe1, 0xf5, 0x93, 0xef, 0xff,
    0xff, 0xff,
];
const TONELLI_Q_BE: [u8; 32] = [
    0, 0, 0, 3, 0x06, 0x44, 0xe7, 0x2e, 0x13, 0x1a, 0x02, 0x9b, 0x85, 0x04, 0x5b, 0x68, 0x18,
    0x15, 0x85, 0xd2, 0x83, 0x3e, 0x84, 0x87, 0x9b, 0x97, 0x09, 0x14, 0x3e, 0x1f, 0x59, 0x3f,
];
const TONELLI_Q_PLUS_ONE_OVER_TWO_BE: [u8; 32] = [
    0, 0, 0, 1, 0x83, 0x22, 0x73, 0x97, 0x09, 0x8d, 0x01, 0x4d, 0xc2, 0x82, 0x2d, 0xb4, 0x0c,
    0x0a, 0xc2, 0xe9, 0x41, 0x9f, 0x42, 0x43, 0xcd, 0xcb, 0x84, 0x8a, 0x1f, 0x0f, 0xac, 0xa0,
];
const TONELLI_C: Fq =
    MontFp!("19103219067921713944291392827692070036145651957329286315305642004821462161904");

pub const POINTS_PER_ORDER: usize = 17 * 2 * 2;
pub const FIELD_BYTES: usize = 32;
pub const EXTENDED_POINT_BYTES: usize = FIELD_BYTES * 4;
pub const AGGREGATE_BYTES: usize = POINTS_PER_ORDER * EXTENDED_POINT_BYTES;

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    input: &[u8],
) -> ProgramResult {
    if input.len() != 2 && input.len() != 2 + FIELD_BYTES {
        return Err(ProgramError::InvalidInstructionData);
    }
    let aggregate_info = next_account_info(&mut accounts.iter())?;
    if aggregate_info.owner != program_id || !aggregate_info.is_writable {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if aggregate_info.data_len() != AGGREGATE_BYTES {
        return Err(ProgramError::InvalidAccountData);
    }

    let count = usize::from(input[1]);
    if count == 0 || count > POINTS_PER_ORDER {
        return Err(ProgramError::InvalidInstructionData);
    }
    let fixed_incoming = EdwardsProjective::generator().into_affine();
    let fixed_dxy = ark_ed_on_bn254::EdwardsConfig::COEFF_D
        * fixed_incoming.x
        * fixed_incoming.y;
    let mut data = aggregate_info.try_borrow_mut_data()?;
    for chunk in data
        .chunks_exact_mut(EXTENDED_POINT_BYTES)
        .take(count)
    {
        let incoming = match input[0] {
            // Add-only lower bound: use a fixed, already-materialized affine
            // point and measure the 68 additions plus aggregate state I/O.
            0 if input.len() == 2 => fixed_incoming,
            // Full curve-work path: recover a compressed point on every loop
            // iteration, then add it. Repeating the bytes keeps the test tx
            // small without reducing the executed SBF arithmetic.
            1 if input.len() == 2 + FIELD_BYTES => {
                let point = EdwardsAffine::deserialize_compressed_unchecked(
                    &mut &input[2..2 + FIELD_BYTES],
                )
                .map_err(|_| ProgramError::InvalidInstructionData)?;
                if point.is_zero() || !point.is_on_curve() {
                    return Err(ProgramError::InvalidInstructionData);
                }
                point
            }
            // Same compressed format, but recover x with Solana's generic
            // modular-exponentiation syscall and a fixed-prime Tonelli-Shanks
            // implementation rather than Arkworks' in-program exponentiation.
            2 if input.len() == 2 + FIELD_BYTES => {
                decompress_with_modexp(&input[2..2 + FIELD_BYTES])?
            }
            // Addition-only optimized formula with d*x*y cached alongside
            // the incoming point by the decoder.
            3 if input.len() == 2 => fixed_incoming,
            _ => return Err(ProgramError::InvalidInstructionData),
        };
        let mut aggregate = decode_extended(chunk)?;
        if input[0] == 3 {
            add_cached_affine(
                &mut aggregate,
                incoming.x,
                incoming.y,
                fixed_dxy,
            );
        } else {
            aggregate += incoming;
        }
        encode_extended(&aggregate, chunk);
    }
    Ok(())
}

fn decode_extended(input: &[u8]) -> Result<EdwardsProjective, ProgramError> {
    let x = decode_field(&input[0..32]);
    let y = decode_field(&input[32..64]);
    let t = decode_field(&input[64..96]);
    let z = decode_field(&input[96..128]);

    // A zero-filled benchmark account denotes 68 identity accumulators. Once
    // written, all four extended coordinates are stored explicitly so later
    // orders do not normalize or decompress the old aggregate.
    if x.is_zero() && y.is_zero() && t.is_zero() && z.is_zero() {
        return Ok(EdwardsProjective::zero());
    }
    Ok(EdwardsProjective::new_unchecked(x, y, t, z))
}

fn decode_field(input: &[u8]) -> Fq {
    let mut limbs = [0_u64; 4];
    for (limb, bytes) in limbs.iter_mut().zip(input.chunks_exact(8)) {
        *limb = u64::from_le_bytes(bytes.try_into().expect("eight-byte field limb"));
    }
    Fq::new_unchecked(BigInt(limbs))
}

fn encode_extended(point: &EdwardsProjective, output: &mut [u8]) {
    encode_field(&point.x, &mut output[0..32]);
    encode_field(&point.y, &mut output[32..64]);
    encode_field(&point.t, &mut output[64..96]);
    encode_field(&point.z, &mut output[96..128]);
}

fn encode_field(value: &Fq, output: &mut [u8]) {
    for (limb, bytes) in value.0.0.iter().zip(output.chunks_exact_mut(8)) {
        bytes.copy_from_slice(&limb.to_le_bytes());
    }
}

fn add_cached_affine(aggregate: &mut EdwardsProjective, x: Fq, y: Fq, dxy: Fq) {
    // madd-2008-hwcd specialized for a=1 with d*x*y cached by point
    // decompression: 8 general field multiplications instead of Arkworks' 10.
    let a = aggregate.x * x;
    let b = aggregate.y * y;
    let c = aggregate.t * dxy;
    let e = (aggregate.x + aggregate.y) * (x + y) - a - b;
    let f = aggregate.z - c;
    let g = aggregate.z + c;
    let h = b - a;
    aggregate.x = e * f;
    aggregate.y = g * h;
    aggregate.t = e * h;
    aggregate.z = f * g;
}

fn decompress_with_modexp(input: &[u8]) -> Result<EdwardsAffine, ProgramError> {
    let mut y_bytes: [u8; 32] = input
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    let x_is_negative = y_bytes[31] & 0x80 != 0;
    y_bytes[31] &= 0x7f;
    let y = Fq::from_le_bytes_mod_order(&y_bytes);
    let y_squared = y.square();
    let numerator = Fq::one() - y_squared;
    let denominator = Fq::one() - ark_ed_on_bn254::EdwardsConfig::COEFF_D * y_squared;
    if denominator.is_zero() {
        return Err(ProgramError::InvalidInstructionData);
    }
    let inverse = modexp_field(denominator, &FR_MINUS_TWO_BE);
    let x_squared = numerator * inverse;
    let mut x = tonelli_shanks_modexp(x_squared)?;
    let negative_x = -x;
    if x_is_negative == (x <= negative_x) {
        x = negative_x;
    }
    Ok(EdwardsAffine::new_unchecked(x, y))
}

fn tonelli_shanks_modexp(value: Fq) -> Result<Fq, ProgramError> {
    if value.is_zero() {
        return Ok(value);
    }
    // q - 1 = odd_q * 2^28. Five is a quadratic non-residue; the
    // corresponding 2^28-th root of unity is precomputed here.
    let mut c = TONELLI_C;
    let mut t = modexp_field(value, &TONELLI_Q_BE);
    let mut root = modexp_field(value, &TONELLI_Q_PLUS_ONE_OVER_TWO_BE);
    let mut m = 28_u32;
    while !t.is_one() {
        let mut i = 1_u32;
        let mut t_power = t.square();
        while !t_power.is_one() {
            t_power.square_in_place();
            i += 1;
            if i >= m {
                return Err(ProgramError::InvalidInstructionData);
            }
        }
        let mut b = c;
        for _ in 0..(m - i - 1) {
            b.square_in_place();
        }
        root *= b;
        let b_squared = b.square();
        t *= b_squared;
        c = b_squared;
        m = i;
    }
    if root.square() != value {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(root)
}

fn modexp_field(base: Fq, exponent_be: &[u8]) -> Fq {
    let base_be = base.into_bigint().to_bytes_be();
    let result = solana_big_mod_exp::big_mod_exp(&base_be, exponent_be, &FR_MODULUS_BE);
    Fq::from_be_bytes_mod_order(&result)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        ark_ec::{AdditiveGroup, CurveConfig, PrimeGroup},
        ark_ed_on_bn254::EdwardsConfig,
        ark_serialize::CanonicalSerialize,
        litesvm::LiteSVM,
        solana_account::Account,
        solana_compute_budget_interface::ComputeBudgetInstruction,
        solana_instruction::{AccountMeta, Instruction},
        solana_keypair::Keypair,
        solana_message::Message,
        solana_signer::Signer,
        solana_transaction::Transaction,
        std::path::PathBuf,
    };

    fn program_path() -> PathBuf {
        std::env::var_os("BABYJUB_SBF_SO")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target/deploy/babyjub_sbf_spike.so")
            })
    }

    #[test]
    fn compressed_round_trip_and_count_are_stable() {
        let point = EdwardsProjective::generator().double().into_affine();
        let mut bytes = Vec::new();
        point.serialize_compressed(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 32);
        let decoded = EdwardsAffine::deserialize_compressed_unchecked(&mut bytes.as_slice())
            .unwrap();
        assert_eq!(decoded, point);
        assert_eq!(decompress_with_modexp(&bytes).unwrap(), point);
        assert_eq!(POINTS_PER_ORDER, 68);
        assert_eq!(AGGREGATE_BYTES, 8_704);
        assert_eq!(EdwardsConfig::COFACTOR, &[8]);
    }

    #[test]
    #[ignore = "requires cargo build-sbf"]
    fn measure_add_only_and_68_decompressions_in_sbf() {
        let program_path = program_path();
        assert!(program_path.exists(), "run cargo build-sbf first");

        let program_id = Pubkey::new_unique();
        let aggregate = Pubkey::new_unique();
        let payer = Keypair::new();
        let point = EdwardsProjective::generator().double().into_affine();
        let mut compressed = Vec::new();
        point.serialize_compressed(&mut compressed).unwrap();

        let mut svm = LiteSVM::new();
        svm.add_program_from_file(program_id, program_path).unwrap();
        svm.airdrop(&payer.pubkey(), 1_000_000_000).unwrap();
        svm.set_account(
            aggregate,
            Account {
                lamports: 1_000_000_000,
                data: vec![0; AGGREGATE_BYTES],
                owner: program_id,
                ..Account::default()
            },
        )
        .unwrap();

        let execute = |svm: &mut LiteSVM, input: Vec<u8>| {
            let instruction = Instruction {
                program_id,
                accounts: vec![AccountMeta::new(aggregate, false)],
                data: input,
            };
            let transaction = Transaction::new(
                &[&payer],
                Message::new(
                    &[
                        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                        instruction,
                    ],
                    Some(&payer.pubkey()),
                ),
                svm.latest_blockhash(),
            );
            svm.send_transaction(transaction)
        };
        for count in [1_u8, 4, 8, 16, 32, 68] {
            svm.set_account(
                aggregate,
                Account {
                    lamports: 1_000_000_000,
                    data: vec![0; AGGREGATE_BYTES],
                    owner: program_id,
                    ..Account::default()
                },
            )
            .unwrap();
            match execute(&mut svm, vec![0, count]) {
                Ok(result) => eprintln!(
                    "{count} Baby-Jubjub additions + state I/O consumed {} CU",
                    result.compute_units_consumed
                ),
                Err(error) => eprintln!("{count} Baby-Jubjub additions failed: {error:?}"),
            }
        }

        for count in [1_u8, 8, 16, 32, 68] {
            svm.set_account(
                aggregate,
                Account {
                    lamports: 1_000_000_000,
                    data: vec![0; AGGREGATE_BYTES],
                    owner: program_id,
                    ..Account::default()
                },
            )
            .unwrap();
            match execute(&mut svm, vec![3, count]) {
                Ok(result) => eprintln!(
                    "{count} cached-input Baby-Jubjub additions consumed {} CU",
                    result.compute_units_consumed
                ),
                Err(error) => {
                    eprintln!("{count} cached-input additions failed: {error:?}")
                }
            }
        }

        for mode in [1_u8, 2] {
            for count in [1_u8, 2, 4, 8] {
            svm.set_account(
                aggregate,
                Account {
                    lamports: 1_000_000_000,
                    data: vec![0; AGGREGATE_BYTES],
                    owner: program_id,
                    ..Account::default()
                },
            )
            .unwrap();
            let mut decompress_input = vec![mode, count];
            decompress_input.extend_from_slice(&compressed);
            match execute(&mut svm, decompress_input) {
                Ok(result) => eprintln!(
                    "mode {mode}: {count} Baby-Jubjub compressed decodes + additions consumed {} CU",
                    result.compute_units_consumed
                ),
                Err(error) => {
                    eprintln!("mode {mode}: {count} Baby-Jubjub decodes + additions failed: {error:?}")
                }
            }
            }
        }

    }
}

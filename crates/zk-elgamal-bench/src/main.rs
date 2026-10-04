use {
    solana_zk_sdk::{
        encryption::{
            elgamal::{ElGamalKeypair, ElGamalPubkey},
            grouped_elgamal::{GroupedElGamal, GroupedElGamalCiphertext},
            pedersen::{Pedersen, PedersenOpening},
        },
        zk_elgamal_proof_program::{
            build_batched_grouped_ciphertext_2_handles_validity_proof_data,
            build_batched_range_proof_u64_data, VerifyZkProof,
        },
    },
    std::{env, mem::size_of_val, time::Instant},
};

const QUANTITY_BITS: usize = 4;
const MAX_REAL_VALUES_PER_RANGE_PROOF: usize = 7;
const GROUPED_CIPHERTEXT_BYTES: usize = 96;
const VALIDITY_CONTEXT_BYTES: usize = 256;
const RANGE_CONTEXT_BYTES: usize = 264;
const CONTEXT_STATE_HEADER_BYTES: usize = 33;

#[derive(Debug)]
struct EncryptedQuantity {
    amount: u64,
    opening: PedersenOpening,
    ciphertext: GroupedElGamalCiphertext<2>,
}

#[derive(Default)]
struct Sample {
    encryption_ms: f64,
    validity_prove_ms: f64,
    validity_verify_ms: f64,
    range_prove_ms: f64,
    range_verify_ms: f64,
    validity_proofs: usize,
    range_proofs: usize,
    proof_data_bytes: usize,
}

fn main() {
    let rounds = env::args()
        .skip_while(|argument| argument != "--rounds")
        .nth(1)
        .map(|value| value.parse::<usize>().expect("--rounds must be an integer"))
        .unwrap_or(5);
    assert!(rounds > 0, "rounds must be positive");

    let committee = ElGamalKeypair::new_rand();
    let recovery = ElGamalKeypair::new_rand();
    println!("BeatEx native ZK ElGamal benchmark ({rounds} rounds)");
    println!(
        "levels,quantities,validity_proofs,range_proofs,proof_txs,ciphertext_bytes,proof_data_bytes,context_state_bytes,median_encrypt_ms,median_validity_prove_ms,median_range_prove_ms,median_host_verify_ms"
    );

    for levels in [3, 9, 17, 33, 65] {
        let mut samples = Vec::with_capacity(rounds);
        for _ in 0..rounds {
            samples.push(benchmark_order(
                levels,
                committee.pubkey(),
                recovery.pubkey(),
            ));
        }

        let first = &samples[0];
        assert!(samples.iter().all(|sample| {
            sample.validity_proofs == first.validity_proofs
                && sample.range_proofs == first.range_proofs
                && sample.proof_data_bytes == first.proof_data_bytes
        }));
        // Two 417-byte validity instructions fit in a transaction. A 937-byte
        // U64 range-proof instruction consumes essentially one transaction.
        let proof_transactions = first.validity_proofs.div_ceil(2) + first.range_proofs;
        let context_state_bytes = first.validity_proofs
            * (CONTEXT_STATE_HEADER_BYTES + VALIDITY_CONTEXT_BYTES)
            + first.range_proofs * (CONTEXT_STATE_HEADER_BYTES + RANGE_CONTEXT_BYTES);
        let host_verify = samples
            .iter()
            .map(|sample| sample.validity_verify_ms + sample.range_verify_ms)
            .collect::<Vec<_>>();

        println!(
            "{levels},{},{},{},{proof_transactions},{},{},{},{:.3},{:.3},{:.3},{:.3}",
            levels * 2,
            first.validity_proofs,
            first.range_proofs,
            levels * 2 * GROUPED_CIPHERTEXT_BYTES,
            first.proof_data_bytes,
            context_state_bytes,
            median(samples.iter().map(|sample| sample.encryption_ms).collect()),
            median(
                samples
                    .iter()
                    .map(|sample| sample.validity_prove_ms)
                    .collect()
            ),
            median(samples.iter().map(|sample| sample.range_prove_ms).collect()),
            median(host_verify),
        );
    }

    println!();
    println!(
        "Caveat: these proofs enforce ciphertext well-formedness and 0..=15 quantities; they do not enforce buy*sell=0 privately."
    );
}

fn benchmark_order(levels: usize, committee: &ElGamalPubkey, recovery: &ElGamalPubkey) -> Sample {
    let encryption_started = Instant::now();
    let quantities = deterministic_quantities(levels)
        .into_iter()
        .map(|amount| {
            let opening = PedersenOpening::new_rand();
            let ciphertext =
                GroupedElGamal::<2>::encrypt_with([committee, recovery], amount, &opening);
            EncryptedQuantity {
                amount,
                opening,
                ciphertext,
            }
        })
        .collect::<Vec<_>>();
    let encryption_ms = elapsed_ms(encryption_started);

    let validity_started = Instant::now();
    let validity_proofs = quantities
        .chunks_exact(2)
        .map(|pair| {
            build_batched_grouped_ciphertext_2_handles_validity_proof_data(
                committee,
                recovery,
                &pair[0].ciphertext,
                &pair[1].ciphertext,
                pair[0].amount,
                pair[1].amount,
                &pair[0].opening,
                &pair[1].opening,
            )
            .expect("deterministic ciphertexts are consistent")
        })
        .collect::<Vec<_>>();
    let validity_prove_ms = elapsed_ms(validity_started);

    let range_started = Instant::now();
    let range_proofs = quantities
        .chunks(MAX_REAL_VALUES_PER_RANGE_PROOF)
        .map(|chunk| {
            let (padding_commitment, padding_opening) = Pedersen::new(0u64);
            let mut commitments = chunk
                .iter()
                .map(|quantity| &quantity.ciphertext.commitment)
                .collect::<Vec<_>>();
            let mut amounts = chunk
                .iter()
                .map(|quantity| quantity.amount)
                .collect::<Vec<_>>();
            let mut bit_lengths = vec![QUANTITY_BITS; chunk.len()];
            let mut openings = chunk
                .iter()
                .map(|quantity| &quantity.opening)
                .collect::<Vec<_>>();

            commitments.push(&padding_commitment);
            amounts.push(0);
            bit_lengths.push(64 - QUANTITY_BITS * chunk.len());
            openings.push(&padding_opening);

            build_batched_range_proof_u64_data(commitments, amounts, bit_lengths, openings)
                .expect("strict four-bit batch plus padding totals 64 bits")
        })
        .collect::<Vec<_>>();
    let range_prove_ms = elapsed_ms(range_started);

    let validity_verify_started = Instant::now();
    for proof in &validity_proofs {
        proof.verify_proof().expect("validity proof verifies");
    }
    let validity_verify_ms = elapsed_ms(validity_verify_started);

    let range_verify_started = Instant::now();
    for proof in &range_proofs {
        proof.verify_proof().expect("range proof verifies");
    }
    let range_verify_ms = elapsed_ms(range_verify_started);

    let proof_data_bytes = validity_proofs.iter().map(size_of_val).sum::<usize>()
        + range_proofs.iter().map(size_of_val).sum::<usize>();

    Sample {
        encryption_ms,
        validity_prove_ms,
        validity_verify_ms,
        range_prove_ms,
        range_verify_ms,
        validity_proofs: validity_proofs.len(),
        range_proofs: range_proofs.len(),
        proof_data_bytes,
    }
}

fn deterministic_quantities(levels: usize) -> Vec<u64> {
    let mut quantities = Vec::with_capacity(levels * 2);
    let middle = levels / 2;
    for level in 0..levels {
        let quantity = ((level * 7 + 3) % 15 + 1) as u64;
        if level < middle {
            quantities.extend_from_slice(&[quantity, 0]);
        } else if level > middle {
            quantities.extend_from_slice(&[0, quantity]);
        } else {
            quantities.extend_from_slice(&[0, 0]);
        }
    }
    quantities
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_batch_counts_and_sizes_are_stable() {
        let committee = ElGamalKeypair::new_rand();
        let recovery = ElGamalKeypair::new_rand();
        let sample = benchmark_order(17, committee.pubkey(), recovery.pubkey());
        assert_eq!(sample.validity_proofs, 17);
        assert_eq!(sample.range_proofs, 5);
        assert_eq!(sample.proof_data_bytes, 17 * 416 + 5 * 936);
    }
}

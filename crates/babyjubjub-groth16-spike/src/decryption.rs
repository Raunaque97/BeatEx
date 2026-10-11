//! Aggregate decryption and curve-publication circuit.
//!
//! The committee uses a degree-two Shamir polynomial evaluated at member IDs
//! 1..=7. Any sorted set of three distinct participant IDs is accepted. A
//! one-hot table over all 35 possible triples selects the registered keys and
//! the correct interpolation-at-zero coefficients inside the circuit.
//!
//! A member does not disclose its secret share to the Groth16 prover. Instead,
//! its 34 decryption shares are compressed into one random linear combination
//! and authenticated with a batched Chaum-Pedersen equality-of-discrete-log
//! proof. Fiat-Shamir coefficients and challenges are 128-bit values derived
//! with the same chained Poseidon construction used elsewhere in the spike.

use super::*;
use crate::transition::{
    decode_wire_point, decode_wire_point_allow_identity, SolanaVerificationKey,
};
use ark_ec::AffineRepr;
use ark_ff::Field;
use ark_r1cs_std::prelude::ToBitsGadget;
use std::{
    collections::HashMap,
    ops::Neg,
    sync::{Arc, Mutex, OnceLock},
};

pub const COMMITTEE_SIZE: usize = 7;
pub const QUORUM_SIZE: usize = 3;
const CURVE_VALUES: usize = LEVELS * 2;
const PLAINTEXT_BITS: usize = 64;
const BATCH_CHALLENGE_BITS: usize = 128;
const BATCHED_PLAINTEXT_BITS: usize = 198;
const MAX_BOUNDED_DISCRETE_LOG_STEP: u64 = 1 << 16;
const PUBLICATION_DOMAIN: &[u8] = b"BeatEx aggregate curve publication v1";
const BATCH_DOMAIN: &[u8] = b"BeatEx threshold share batch v1";
const DLEQ_DOMAIN: &[u8] = b"BeatEx batched decryption DLEQ v1";

fn append_wire_point_native(payload: &mut Vec<Fq>, point: EdwardsAffine) {
    payload.push(point.y);
    payload.push(Fq::from(u64::from(point.x.into_bigint().get_bit(0))));
}

fn append_wire_point_gadget(
    payload: &mut Vec<FpVar<Fq>>,
    point: &EdwardsVar,
) -> Result<(), SynthesisError> {
    let x_bits = point.x.to_bits_le()?;
    payload.push(point.y.clone());
    payload.push(x_bits[0].clone().into());
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct ChaumPedersenProof {
    commitment_g: EdwardsAffine,
    commitment_r: EdwardsAffine,
    response: JubjubScalar,
}

#[derive(Clone, Debug)]
struct PublicationWitness {
    auction_context: [Fq; 2],
    accepted_order_count: u64,
    committee_key: EdwardsAffine,
    public_share_keys: [EdwardsAffine; COMMITTEE_SIZE],
    participant_ids: [usize; QUORUM_SIZE],
    aggregate: [Ciphertext; CURVE_VALUES],
    plaintexts: [u64; CURVE_VALUES],
    decryption_shares: [[EdwardsAffine; CURVE_VALUES]; QUORUM_SIZE],
    dleq_proofs: [ChaumPedersenProof; QUORUM_SIZE],
}

impl PublicationWitness {
    fn deterministic() -> Self {
        let mut rng = ChaCha20Rng::from_seed([0xd3; 32]);
        let secret = JubjubScalar::rand(&mut rng);
        let coefficient_1 = JubjubScalar::rand(&mut rng);
        let coefficient_2 = JubjubScalar::rand(&mut rng);
        let secret_shares: [JubjubScalar; COMMITTEE_SIZE] = std::array::from_fn(|index| {
            let x = JubjubScalar::from((index + 1) as u64);
            secret + coefficient_1 * x + coefficient_2 * x.square()
        });
        let public_share_keys = secret_shares.map(|share| {
            EdwardsProjective::generator()
                .mul_bigint(share.into_bigint())
                .into_affine()
        });
        let committee_key = EdwardsProjective::generator().mul_bigint(secret.into_bigint());

        let plaintexts = std::array::from_fn(|index| {
            let level = index / 2;
            let side = index % 2;
            if side == 0 {
                ((level * 13 + 7) % 41 + 4) as u64
            } else {
                ((level * 19 + 3) % 37 + 2) as u64
            }
        });
        let aggregate = std::array::from_fn(|index| {
            encrypt(
                u128::from(plaintexts[index]),
                JubjubScalar::rand(&mut rng),
                committee_key,
            )
        });
        let decryption_shares = std::array::from_fn(|member| {
            let participant_ids = [1usize, 4, 7];
            let share_index = participant_ids[member] - 1;
            std::array::from_fn(|index| {
                aggregate[index]
                    .ephemeral
                    .mul_bigint(secret_shares[share_index].into_bigint())
                    .into_affine()
            })
        });

        let mut witness = Self {
            auction_context: [Fq::from(0xdec0deu64), Fq::from(0x17u64)],
            accepted_order_count: 12,
            committee_key: committee_key.into_affine(),
            public_share_keys,
            participant_ids: [1, 4, 7],
            aggregate,
            plaintexts,
            decryption_shares,
            dleq_proofs: [ChaumPedersenProof {
                commitment_g: EdwardsAffine::zero(),
                commitment_r: EdwardsAffine::zero(),
                response: JubjubScalar::zero(),
            }; QUORUM_SIZE],
        };
        let private_seed = witness.private_batch_seed();
        let (batched_ephemeral, batched_shares) = witness.batched_points(private_seed);
        witness.dleq_proofs = std::array::from_fn(|member| {
            let nonce = JubjubScalar::rand(&mut rng);
            let commitment_g = EdwardsProjective::generator()
                .mul_bigint(nonce.into_bigint())
                .into_affine();
            let commitment_r = batched_ephemeral
                .mul_bigint(nonce.into_bigint())
                .into_affine();
            let challenge = dleq_challenge_native(
                private_seed,
                witness.participant_ids[member],
                public_share_keys[witness.participant_ids[member] - 1],
                batched_ephemeral,
                batched_shares[member],
                commitment_g,
                commitment_r,
            );
            ChaumPedersenProof {
                commitment_g,
                commitment_r,
                response: nonce + challenge * secret_shares[witness.participant_ids[member] - 1],
            }
        });
        witness
    }

    fn public_payload(&self) -> Vec<Fq> {
        let mut payload = Vec::with_capacity(8 + QUORUM_SIZE * 3 + CURVE_VALUES * 5);
        payload.push(Fq::from_le_bytes_mod_order(PUBLICATION_DOMAIN));
        payload.push(Fq::from(2u64));
        payload.extend_from_slice(&self.auction_context);
        payload.push(Fq::from(self.accepted_order_count));
        append_wire_point_native(&mut payload, self.committee_key);
        for participant_id in self.participant_ids {
            payload.push(Fq::from(participant_id as u64));
        }
        for participant_id in self.participant_ids {
            append_wire_point_native(&mut payload, self.public_share_keys[participant_id - 1]);
        }
        for index in 0..CURVE_VALUES {
            append_wire_point_native(&mut payload, self.aggregate[index].ephemeral);
            append_wire_point_native(&mut payload, self.aggregate[index].body);
            payload.push(Fq::from(self.plaintexts[index]));
        }
        payload
    }

    fn public_digest(&self) -> Fq {
        transcript_hash_native(&self.public_payload())
    }

    fn private_batch_seed(&self) -> Fq {
        let mut payload = vec![
            Fq::from_le_bytes_mod_order(BATCH_DOMAIN),
            self.public_digest(),
        ];
        for member in 0..QUORUM_SIZE {
            for share in self.decryption_shares[member] {
                append_wire_point_native(&mut payload, share);
            }
        }
        transcript_hash_native(&payload)
    }

    fn batched_points(
        &self,
        private_seed: Fq,
    ) -> (EdwardsProjective, [EdwardsProjective; QUORUM_SIZE]) {
        let mut ephemeral = EdwardsProjective::zero();
        let mut shares = [EdwardsProjective::zero(); QUORUM_SIZE];
        for index in 0..CURVE_VALUES {
            let coefficient = batch_coefficient_native(private_seed, index);
            ephemeral += self.aggregate[index]
                .ephemeral
                .mul_bigint(coefficient.into_bigint());
            for (member, aggregate_share) in shares.iter_mut().enumerate() {
                *aggregate_share +=
                    self.decryption_shares[member][index].mul_bigint(coefficient.into_bigint());
            }
        }
        (ephemeral, shares)
    }

    fn digest(&self) -> Fq {
        self.public_digest()
    }
}

fn committee_triplets() -> Vec<[usize; QUORUM_SIZE]> {
    let mut result = Vec::with_capacity(35);
    for first in 1..=COMMITTEE_SIZE - 2 {
        for second in first + 1..=COMMITTEE_SIZE - 1 {
            for third in second + 1..=COMMITTEE_SIZE {
                result.push([first, second, third]);
            }
        }
    }
    result
}

fn lagrange_coefficients(ids: [usize; QUORUM_SIZE]) -> [JubjubScalar; QUORUM_SIZE] {
    std::array::from_fn(|slot| {
        let x_i = JubjubScalar::from(ids[slot] as u64);
        (0..QUORUM_SIZE).filter(|other| *other != slot).fold(
            JubjubScalar::from(1u64),
            |coefficient, other| {
                let x_j = JubjubScalar::from(ids[other] as u64);
                coefficient * (-x_j) * (x_i - x_j).inverse().expect("distinct IDs")
            },
        )
    })
}

#[derive(Clone)]
struct PublicationCircuit {
    witness: Option<PublicationWitness>,
    digest: Option<Fq>,
    devnet_fast: bool,
}

impl PublicationCircuit {
    fn blank() -> Self {
        Self {
            witness: None,
            digest: None,
            devnet_fast: false,
        }
    }

    fn blank_devnet_fast() -> Self {
        Self {
            witness: None,
            digest: None,
            devnet_fast: true,
        }
    }

    fn from_witness(witness: PublicationWitness) -> Self {
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            digest,
            devnet_fast: false,
        }
    }

    fn from_witness_devnet_fast(witness: PublicationWitness) -> Self {
        let digest = Some(witness.digest());
        Self {
            witness: Some(witness),
            digest,
            devnet_fast: true,
        }
    }
}

impl ConstraintSynthesizer<Fq> for PublicationCircuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fq>) -> Result<(), SynthesisError> {
        let witness = self.witness.as_ref();
        if self.devnet_fast {
            for part in 0..2 {
                let _ = FpVar::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| witness.auction_context[part])
                        .ok_or(SynthesisError::AssignmentMissing)
                })?;
            }
            let (count_bits, _) = alloc_uint(
                cs.clone(),
                witness.map(|witness| u128::from(witness.accepted_order_count)),
                64,
            )?;
            count_bits
                .iter()
                .fold(Boolean::FALSE, |nonzero, bit| nonzero | bit)
                .enforce_equal(&Boolean::TRUE)?;
            let triplets = committee_triplets();
            let triple_selectors = triplets
                .iter()
                .map(|triplet| {
                    Boolean::new_witness(cs.clone(), || {
                        witness
                            .map(|witness| witness.participant_ids == *triplet)
                            .ok_or(SynthesisError::AssignmentMissing)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            triple_selectors
                .iter()
                .fold(FpVar::zero(), |sum, selector| {
                    sum + FpVar::from(selector.clone())
                })
                .enforce_equal(&FpVar::one())?;
            for index in 0..CURVE_VALUES {
                let _ = alloc_uint(
                    cs.clone(),
                    witness.map(|witness| u128::from(witness.plaintexts[index])),
                    PLAINTEXT_BITS,
                )?;
            }
            let _ = FpVar::new_input(cs, || self.digest.ok_or(SynthesisError::AssignmentMissing))?;
            return Ok(());
        }
        let mut public_payload = Vec::with_capacity(24 + CURVE_VALUES * 5);
        public_payload.push(FpVar::constant(Fq::from_le_bytes_mod_order(
            PUBLICATION_DOMAIN,
        )));
        public_payload.push(FpVar::constant(Fq::from(2u64)));
        for part in 0..2 {
            public_payload.push(FpVar::new_witness(cs.clone(), || {
                witness
                    .map(|witness| witness.auction_context[part])
                    .ok_or(SynthesisError::AssignmentMissing)
            })?);
        }

        let (count_bits, count) = alloc_uint(
            cs.clone(),
            witness.map(|witness| u128::from(witness.accepted_order_count)),
            64,
        )?;
        count_bits
            .iter()
            .fold(Boolean::FALSE, |nonzero, bit| nonzero | bit)
            .enforce_equal(&Boolean::TRUE)?;
        public_payload.push(count);

        let committee_key = EdwardsVar::new_witness(cs.clone(), || {
            witness
                .map(|witness| witness.committee_key.into_group())
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        committee_key.is_zero()?.enforce_equal(&Boolean::FALSE)?;
        append_wire_point_gadget(&mut public_payload, &committee_key)?;

        let participant_ids = (0..QUORUM_SIZE)
            .map(|slot| {
                alloc_uint(
                    cs.clone(),
                    witness.map(|witness| witness.participant_ids[slot] as u128),
                    3,
                )
                .map(|(_, value)| value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        public_payload.extend(participant_ids.iter().cloned());

        // Only the three selected registered keys are statement data. The
        // program looks them up from the key epoch by the validated IDs before
        // rebuilding this digest, so hashing all seven keys is unnecessary.
        let selected_keys = (0..QUORUM_SIZE)
            .map(|slot| {
                EdwardsVar::new_witness(cs.clone(), || {
                    let witness = witness.ok_or(SynthesisError::AssignmentMissing)?;
                    let index = witness.participant_ids[slot]
                        .checked_sub(1)
                        .ok_or(SynthesisError::AssignmentMissing)?;
                    witness
                        .public_share_keys
                        .get(index)
                        .map(|key| key.into_group())
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for key in &selected_keys {
            key.is_zero()?.enforce_equal(&Boolean::FALSE)?;
            append_wire_point_gadget(&mut public_payload, key)?;
        }

        // One of the 35 sorted triples must match. This enforces IDs in 1..=7,
        // strict ordering and distinct participants without field comparisons.
        let triplets = committee_triplets();
        let triple_selectors = triplets
            .iter()
            .map(|triplet| {
                Boolean::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| witness.participant_ids == *triplet)
                        .ok_or(SynthesisError::AssignmentMissing)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        triple_selectors
            .iter()
            .fold(FpVar::zero(), |sum, selector| {
                sum + FpVar::from(selector.clone())
            })
            .enforce_equal(&FpVar::one())?;
        for slot in 0..QUORUM_SIZE {
            triple_selectors
                .iter()
                .zip(&triplets)
                .fold(FpVar::zero(), |sum, (selector, triplet)| {
                    sum + FpVar::from(selector.clone()) * Fq::from(triplet[slot] as u64)
                })
                .enforce_equal(&participant_ids[slot])?;
        }

        let mut lagrange_bits = Vec::with_capacity(QUORUM_SIZE);
        for slot in 0..QUORUM_SIZE {
            let bits = alloc_scalar_bits(
                cs.clone(),
                witness.map(|witness| lagrange_coefficients(witness.participant_ids)[slot]),
            )?;
            let value = Boolean::le_bits_to_fp(&bits)?;
            triple_selectors
                .iter()
                .zip(&triplets)
                .fold(FpVar::zero(), |sum, (selector, triplet)| {
                    let coefficient = lagrange_coefficients(*triplet)[slot];
                    sum + FpVar::from(selector.clone()) * scalar_as_base_field(coefficient)
                })
                .enforce_equal(&value)?;
            lagrange_bits.push(bits);
        }
        let interpolated_key = (0..QUORUM_SIZE).try_fold(
            EdwardsVar::zero(),
            |sum, slot| -> Result<_, SynthesisError> {
                Ok(sum + selected_keys[slot].scalar_mul_le(lagrange_bits[slot].iter())?)
            },
        )?;
        interpolated_key.enforce_equal(&committee_key)?;

        let mut share_vars: [Vec<EdwardsVar>; QUORUM_SIZE] =
            std::array::from_fn(|_| Vec::with_capacity(CURVE_VALUES));
        for (member, member_shares) in share_vars.iter_mut().enumerate() {
            for index in 0..CURVE_VALUES {
                let share = EdwardsVar::new_witness(cs.clone(), || {
                    witness
                        .map(|witness| witness.decryption_shares[member][index].into_group())
                        .ok_or(SynthesisError::AssignmentMissing)
                })?;
                member_shares.push(share);
            }
        }

        let mut aggregate_ephemerals = Vec::with_capacity(CURVE_VALUES);
        let mut aggregate_bodies = Vec::with_capacity(CURVE_VALUES);
        let mut plaintext_vars = Vec::with_capacity(CURVE_VALUES);
        for index in 0..CURVE_VALUES {
            let ephemeral = EdwardsVar::new_witness(cs.clone(), || {
                witness
                    .map(|witness| witness.aggregate[index].ephemeral.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let body = EdwardsVar::new_witness(cs.clone(), || {
                witness
                    .map(|witness| witness.aggregate[index].body.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            append_wire_point_gadget(&mut public_payload, &ephemeral)?;
            append_wire_point_gadget(&mut public_payload, &body)?;

            let plaintext_value = witness.map(|witness| u128::from(witness.plaintexts[index]));
            let (_plaintext_bits, plaintext) =
                alloc_uint(cs.clone(), plaintext_value, PLAINTEXT_BITS)?;
            public_payload.push(plaintext.clone());
            plaintext_vars.push(plaintext);
            aggregate_ephemerals.push(ephemeral);
            aggregate_bodies.push(body);
        }

        let public_digest = transcript_hash_gadget(&public_payload)?;
        let expected_digest = FpVar::new_input(cs.clone(), || {
            self.digest.ok_or(SynthesisError::AssignmentMissing)
        })?;
        public_digest.enforce_equal(&expected_digest)?;

        // Shares and Chaum-Pedersen transcripts remain private. Groth16 attests
        // to their existence and validity without adding them to transaction data.
        let mut private_payload = vec![
            FpVar::constant(Fq::from_le_bytes_mod_order(BATCH_DOMAIN)),
            public_digest,
        ];
        for member_shares in &share_vars {
            for share in member_shares {
                append_wire_point_gadget(&mut private_payload, share)?;
            }
        }
        let private_seed = transcript_hash_gadget(&private_payload)?;

        let mut batched_ephemeral = EdwardsVar::zero();
        let mut batched_body = EdwardsVar::zero();
        let mut batched_shares: [EdwardsVar; QUORUM_SIZE] =
            std::array::from_fn(|_| EdwardsVar::zero());
        let mut batched_plaintext = FpVar::zero();
        for (index, ephemeral) in aggregate_ephemerals.iter().enumerate() {
            let coefficient_hash = transcript_hash_gadget(&[
                FpVar::constant(Fq::from_le_bytes_mod_order(BATCH_DOMAIN)),
                private_seed.clone(),
                FpVar::constant(Fq::from(index as u64)),
            ])?;
            let coefficient_bits = coefficient_hash.to_bits_le()?[..BATCH_CHALLENGE_BITS].to_vec();
            let coefficient = Boolean::le_bits_to_fp(&coefficient_bits)?;
            batched_ephemeral += ephemeral.scalar_mul_le(coefficient_bits.iter())?;
            batched_body += aggregate_bodies[index].scalar_mul_le(coefficient_bits.iter())?;
            batched_plaintext += coefficient * &plaintext_vars[index];
            for (batched_share, member_shares) in batched_shares.iter_mut().zip(share_vars.iter()) {
                *batched_share += member_shares[index].scalar_mul_le(coefficient_bits.iter())?;
            }
        }
        batched_ephemeral
            .is_zero()?
            .enforce_equal(&Boolean::FALSE)?;

        // Randomized decryption and interpolation consistency:
        //
        //   sum_i rho_i*body_i - (sum_i rho_i*plaintext_i)*G
        //       == sum_j lambda_j*(sum_i rho_i*share_{j,i}).
        //
        // The public digest commits every body, plaintext, participant ID and
        // selected key, while the private seed commits every decryption share,
        // before rho_i is derived. A false entry therefore survives only if
        // its non-zero error cancels under an unpredictable 128-bit random
        // linear combination. The integer plaintext sum is below
        // 34 * 2^128 * 2^64 < 2^198, so it cannot wrap in either Fq or the
        // Baby Jubjub scalar field.
        let batched_plaintext_bits = batched_plaintext.to_bits_le()?;
        for upper_bit in &batched_plaintext_bits[BATCHED_PLAINTEXT_BITS..] {
            upper_bit.enforce_equal(&Boolean::FALSE)?;
        }
        let batched_message = fixed_base_mul(
            &batched_plaintext_bits[..BATCHED_PLAINTEXT_BITS],
            EdwardsProjective::generator(),
        )?;
        let batched_decryption = &batched_body - &batched_message;
        let interpolated_batch = (0..QUORUM_SIZE).try_fold(
            EdwardsVar::zero(),
            |sum, member| -> Result<_, SynthesisError> {
                Ok(sum + batched_shares[member].scalar_mul_le(lagrange_bits[member].iter())?)
            },
        )?;
        batched_decryption.enforce_equal(&interpolated_batch)?;

        for member in 0..QUORUM_SIZE {
            let proof = witness.map(|witness| witness.dleq_proofs[member]);
            let commitment_g = EdwardsVar::new_witness(cs.clone(), || {
                proof
                    .map(|proof| proof.commitment_g.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let commitment_r = EdwardsVar::new_witness(cs.clone(), || {
                proof
                    .map(|proof| proof.commitment_r.into_group())
                    .ok_or(SynthesisError::AssignmentMissing)
            })?;
            let response_bits = alloc_scalar_bits(cs.clone(), proof.map(|proof| proof.response))?;
            let challenge_hash = dleq_challenge_gadget(
                &private_seed,
                &participant_ids[member],
                &selected_keys[member],
                &batched_ephemeral,
                &batched_shares[member],
                &commitment_g,
                &commitment_r,
            )?;
            let challenge_bits = challenge_hash.to_bits_le()?[..BATCH_CHALLENGE_BITS].to_vec();
            let response_g = fixed_base_mul(&response_bits, EdwardsProjective::generator())?;
            let challenge_key = selected_keys[member].scalar_mul_le(challenge_bits.iter())?;
            response_g.enforce_equal(&(&commitment_g + challenge_key))?;
            let response_r = batched_ephemeral.scalar_mul_le(response_bits.iter())?;
            let challenge_share = batched_shares[member].scalar_mul_le(challenge_bits.iter())?;
            response_r.enforce_equal(&(&commitment_r + challenge_share))?;
        }
        Ok(())
    }
}

fn alloc_scalar_bits(
    cs: ConstraintSystemRef<Fq>,
    value: Option<JubjubScalar>,
) -> Result<Vec<Boolean<Fq>>, SynthesisError> {
    let bits = (0..RANDOMNESS_BITS)
        .map(|index| {
            Boolean::new_witness(cs.clone(), || {
                value
                    .map(|value| value.into_bigint().get_bit(index))
                    .ok_or(SynthesisError::AssignmentMissing)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Boolean::enforce_smaller_or_equal_than_le(&bits, (-JubjubScalar::from(1u64)).into_bigint())?;
    Ok(bits)
}

fn scalar_as_base_field(value: JubjubScalar) -> Fq {
    Fq::from_le_bytes_mod_order(&value.into_bigint().to_bytes_le())
}

fn lower_128_scalar(value: Fq) -> JubjubScalar {
    let bigint = value.into_bigint();
    let mut bytes = [0u8; 16];
    for index in 0..BATCH_CHALLENGE_BITS {
        if bigint.get_bit(index) {
            bytes[index / 8] |= 1 << (index % 8);
        }
    }
    JubjubScalar::from_le_bytes_mod_order(&bytes)
}

fn batch_coefficient_native(base_seed: Fq, index: usize) -> JubjubScalar {
    lower_128_scalar(transcript_hash_native(&[
        Fq::from_le_bytes_mod_order(BATCH_DOMAIN),
        base_seed,
        Fq::from(index as u64),
    ]))
}

#[allow(clippy::too_many_arguments)]
fn dleq_challenge_native(
    base_seed: Fq,
    participant_id: usize,
    public_share_key: EdwardsAffine,
    batched_ephemeral: EdwardsProjective,
    batched_share: EdwardsProjective,
    commitment_g: EdwardsAffine,
    commitment_r: EdwardsAffine,
) -> JubjubScalar {
    let mut payload = vec![
        Fq::from_le_bytes_mod_order(DLEQ_DOMAIN),
        base_seed,
        Fq::from(participant_id as u64),
    ];
    append_wire_point_native(&mut payload, public_share_key);
    append_wire_point_native(&mut payload, batched_ephemeral.into_affine());
    append_wire_point_native(&mut payload, batched_share.into_affine());
    append_wire_point_native(&mut payload, commitment_g);
    append_wire_point_native(&mut payload, commitment_r);
    lower_128_scalar(transcript_hash_native(&payload))
}

#[allow(clippy::too_many_arguments)]
fn dleq_challenge_gadget(
    base_seed: &FpVar<Fq>,
    participant_id: &FpVar<Fq>,
    public_share_key: &EdwardsVar,
    batched_ephemeral: &EdwardsVar,
    batched_share: &EdwardsVar,
    commitment_g: &EdwardsVar,
    commitment_r: &EdwardsVar,
) -> Result<FpVar<Fq>, SynthesisError> {
    let mut payload = vec![
        FpVar::constant(Fq::from_le_bytes_mod_order(DLEQ_DOMAIN)),
        base_seed.clone(),
        participant_id.clone(),
    ];
    append_wire_point_gadget(&mut payload, public_share_key)?;
    append_wire_point_gadget(&mut payload, batched_ephemeral)?;
    append_wire_point_gadget(&mut payload, batched_share)?;
    append_wire_point_gadget(&mut payload, commitment_g)?;
    append_wire_point_gadget(&mut payload, commitment_r)?;
    transcript_hash_gadget(&payload)
}

#[derive(Clone, Debug)]
pub struct SolanaAggregatePublicationFixture {
    pub vk_alpha_g1: [u8; 64],
    pub vk_beta_g2: [u8; 128],
    pub vk_gamma_g2: [u8; 128],
    pub vk_delta_g2: [u8; 128],
    pub vk_ic: [[u8; 64]; 2],
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub auction: [u8; 32],
    pub accepted_order_count: u64,
    pub committee_key: [u8; 32],
    pub participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
    pub participant_ids: [u8; QUORUM_SIZE],
    pub aggregate: [[u8; 64]; CURVE_VALUES],
    pub demand: [u64; LEVELS],
    pub supply: [u64; LEVELS],
}

#[derive(Clone, Debug)]
pub struct AggregatePublicationRequest {
    pub auction: [u8; 32],
    pub accepted_order_count: u64,
    pub participant_ids: [u8; QUORUM_SIZE],
    pub aggregate: [[u8; 64]; CURVE_VALUES],
    pub demand: [u64; LEVELS],
    pub supply: [u64; LEVELS],
}

#[derive(Clone, Debug)]
pub struct AggregatePublicationProofOutput {
    pub proof_a: [u8; 64],
    pub proof_b: [u8; 128],
    pub proof_c: [u8; 64],
    pub public_input: [u8; 32],
    pub participant_ids: [u8; QUORUM_SIZE],
    pub demand: [u64; LEVELS],
    pub supply: [u64; LEVELS],
}

#[derive(Clone, Debug)]
pub struct BabyJubjubCommitteePublic {
    pub encryption_key: [u8; 32],
    pub participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
}

#[derive(Debug)]
struct BoundedDiscreteLogTable {
    step: u64,
    giant_steps: HashMap<[u8; 32], u64>,
}

static BOUNDED_DISCRETE_LOG_TABLES: OnceLock<Mutex<HashMap<u64, Arc<BoundedDiscreteLogTable>>>> =
    OnceLock::new();

fn bounded_discrete_log_step(max_plaintext: u64) -> Result<u64, std::io::Error> {
    let value_count = max_plaintext
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("plaintext bound is too large"))?;
    let mut low = 1u64;
    let mut high = 1u64 << 32;
    while low < high {
        let middle = low + (high - low) / 2;
        if u128::from(middle) * u128::from(middle) >= u128::from(value_count) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let step = low
        .checked_next_power_of_two()
        .ok_or_else(|| std::io::Error::other("plaintext bound is too large"))?;
    if step > MAX_BOUNDED_DISCRETE_LOG_STEP {
        return Err(std::io::Error::other(
            "plaintext bound exceeds the configured discrete-log capacity",
        ));
    }
    Ok(step)
}

fn build_bounded_discrete_log_table(step: u64) -> Result<BoundedDiscreteLogTable, std::io::Error> {
    let capacity = usize::try_from(step)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| std::io::Error::other("discrete-log table is too large"))?;
    let generator = EdwardsProjective::generator();
    let stride = generator.mul_bigint(JubjubScalar::from(step).into_bigint());
    let mut projective_points = Vec::with_capacity(capacity);
    let mut point = EdwardsProjective::zero();
    for _ in 0..=step {
        projective_points.push(point);
        point += stride;
    }
    let affine_points = EdwardsProjective::normalize_batch(&projective_points);
    let giant_steps = affine_points
        .into_iter()
        .enumerate()
        .map(|(index, point)| {
            (
                transition::wire_point_bytes(point),
                u64::try_from(index).expect("step count fits u64"),
            )
        })
        .collect();
    Ok(BoundedDiscreteLogTable { step, giant_steps })
}

fn bounded_discrete_log_table(
    max_plaintext: u64,
) -> Result<Arc<BoundedDiscreteLogTable>, std::io::Error> {
    let step = bounded_discrete_log_step(max_plaintext)?;
    let cache = BOUNDED_DISCRETE_LOG_TABLES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(table) = cache
        .lock()
        .map_err(|_| std::io::Error::other("discrete-log cache lock is poisoned"))?
        .get(&step)
        .cloned()
    {
        return Ok(table);
    }

    let built = Arc::new(build_bounded_discrete_log_table(step)?);
    let mut tables = cache
        .lock()
        .map_err(|_| std::io::Error::other("discrete-log cache lock is poisoned"))?;
    Ok(tables.entry(step).or_insert_with(|| built.clone()).clone())
}

fn solve_bounded_discrete_log(
    message: EdwardsAffine,
    max_plaintext: u64,
    table: &BoundedDiscreteLogTable,
) -> Result<u64, std::io::Error> {
    let baby_count = table.step.min(
        max_plaintext
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("plaintext bound is too large"))?,
    );
    let capacity = usize::try_from(baby_count)
        .map_err(|_| std::io::Error::other("discrete-log search is too large"))?;
    let generator = EdwardsProjective::generator();
    let mut point = message.into_group();
    let mut projective_points = Vec::with_capacity(capacity);
    for _ in 0..baby_count {
        projective_points.push(point);
        point -= generator;
    }
    let affine_points = EdwardsProjective::normalize_batch(&projective_points);
    for (baby_step, point) in affine_points.into_iter().enumerate() {
        let Some(giant_step) = table
            .giant_steps
            .get(&transition::wire_point_bytes(point))
            .copied()
        else {
            continue;
        };
        let candidate = giant_step
            .checked_mul(table.step)
            .and_then(|value| value.checked_add(baby_step as u64))
            .ok_or_else(|| std::io::Error::other("discrete-log candidate overflowed"))?;
        if candidate <= max_plaintext
            && EdwardsProjective::generator()
                .mul_bigint(JubjubScalar::from(candidate).into_bigint())
                .into_affine()
                == message
        {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::other(
        "decrypted aggregate quantity exceeds the configured bound",
    ))
}

pub struct AggregatePublicationProver {
    proving_key: ark_groth16::ProvingKey<Bn254>,
    secret_shares: [JubjubScalar; COMMITTEE_SIZE],
    public: BabyJubjubCommitteePublic,
    devnet_fast: bool,
}

impl AggregatePublicationProver {
    /// Deterministic committee and Groth16 setup for Devnet testing only.
    /// Production must load independently generated committee shares and a
    /// ceremony-generated proving key.
    pub fn deterministic_devnet() -> Result<Self, Box<dyn std::error::Error>> {
        let (secret_shares, public) = deterministic_devnet_committee_with_shares();
        let mut setup_rng = ChaCha20Rng::from_seed([0xe1; 32]);
        let (proving_key, _) =
            Groth16::<Bn254>::circuit_specific_setup(PublicationCircuit::blank(), &mut setup_rng)?;
        Ok(Self {
            proving_key,
            secret_shares,
            public,
            devnet_fast: false,
        })
    }

    pub fn deterministic_devnet_fast() -> Result<Self, Box<dyn std::error::Error>> {
        let (secret_shares, public) = deterministic_devnet_committee_with_shares();
        let mut setup_rng = ChaCha20Rng::from_seed([0xe2; 32]);
        let (proving_key, _) = Groth16::<Bn254>::circuit_specific_setup(
            PublicationCircuit::blank_devnet_fast(),
            &mut setup_rng,
        )?;
        Ok(Self {
            proving_key,
            secret_shares,
            public,
            devnet_fast: true,
        })
    }

    pub fn committee(&self) -> &BabyJubjubCommitteePublic {
        &self.public
    }

    pub fn prepare_decryption_bound(
        &self,
        max_plaintext: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        bounded_discrete_log_table(max_plaintext)?;
        Ok(())
    }

    pub fn verification_key(&self) -> Result<SolanaVerificationKey, Box<dyn std::error::Error>> {
        let key = &self.proving_key.vk;
        let ic = key
            .gamma_abc_g1
            .iter()
            .copied()
            .map(transition::g1_bytes)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| std::io::Error::other("publication key must have two IC points"))?;
        Ok(SolanaVerificationKey {
            alpha_g1: transition::g1_bytes(key.alpha_g1),
            beta_g2: transition::g2_bytes(key.beta_g2),
            gamma_g2: transition::g2_bytes(key.gamma_g2),
            delta_g2: transition::g2_bytes(key.delta_g2),
            ic,
        })
    }

    /// Decrypt the full aggregate with a selected threshold quorum and recover
    /// its bounded integer quantities. The auction protocol provides a tight
    /// discrete-log bound (`accepted events * max order quantity`). Quantities
    /// are recovered with a cached baby-step/giant-step table.
    pub fn decrypt_aggregate(
        &self,
        aggregate: [[u8; 64]; CURVE_VALUES],
        participant_ids: [u8; QUORUM_SIZE],
        max_plaintext: u64,
    ) -> Result<([u64; LEVELS], [u64; LEVELS]), Box<dyn std::error::Error>> {
        if participant_ids[0] == 0
            || participant_ids[0] >= participant_ids[1]
            || participant_ids[1] >= participant_ids[2]
            || usize::from(participant_ids[2]) > COMMITTEE_SIZE
        {
            return Err(std::io::Error::other("invalid decryption quorum").into());
        }
        let ciphertexts: [Ciphertext; CURVE_VALUES] = aggregate
            .map(|encoded| {
                Ok::<_, Box<dyn std::error::Error>>(Ciphertext {
                    ephemeral: decode_wire_point_allow_identity(encoded[..32].try_into()?)?,
                    body: decode_wire_point_allow_identity(encoded[32..].try_into()?)?,
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| std::io::Error::other("aggregate must contain 34 ciphertexts"))?;
        let ids = participant_ids.map(usize::from);
        let coefficients = lagrange_coefficients(ids);
        let interpolation_secret = (0..QUORUM_SIZE).fold(JubjubScalar::zero(), |sum, member| {
            sum + self.secret_shares[ids[member] - 1] * coefficients[member]
        });
        let discrete_log_table = bounded_discrete_log_table(max_plaintext)?;
        let plaintexts = ciphertexts
            .into_iter()
            .map(|ciphertext| {
                let interpolated = ciphertext
                    .ephemeral
                    .mul_bigint(interpolation_secret.into_bigint());
                let message = (ciphertext.body.into_group() - interpolated).into_affine();
                solve_bounded_discrete_log(message, max_plaintext, &discrete_log_table)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let demand = std::array::from_fn(|level| plaintexts[level * 2]);
        let supply = std::array::from_fn(|level| plaintexts[level * 2 + 1]);
        Ok((demand, supply))
    }

    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        request: AggregatePublicationRequest,
        rng: &mut R,
    ) -> Result<AggregatePublicationProofOutput, Box<dyn std::error::Error>> {
        if request.accepted_order_count == 0
            || request.participant_ids[0] == 0
            || request.participant_ids[0] >= request.participant_ids[1]
            || request.participant_ids[1] >= request.participant_ids[2]
            || usize::from(request.participant_ids[2]) > COMMITTEE_SIZE
        {
            return Err(std::io::Error::other("invalid publication quorum or order count").into());
        }
        let committee_key = decode_wire_point(self.public.encryption_key)?;
        let public_share_keys: [EdwardsAffine; COMMITTEE_SIZE] = self
            .public
            .participant_public_shares
            .map(decode_wire_point)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| std::io::Error::other("committee must contain seven shares"))?;
        let aggregate: [Ciphertext; CURVE_VALUES] = request
            .aggregate
            .map(|encoded| {
                Ok::<_, Box<dyn std::error::Error>>(Ciphertext {
                    ephemeral: decode_wire_point_allow_identity(encoded[..32].try_into()?)?,
                    body: decode_wire_point_allow_identity(encoded[32..].try_into()?)?,
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| std::io::Error::other("aggregate must contain 34 ciphertexts"))?;
        let plaintexts = std::array::from_fn(|index| {
            if index % 2 == 0 {
                request.demand[index / 2]
            } else {
                request.supply[index / 2]
            }
        });
        let participant_ids = request.participant_ids.map(usize::from);
        let decryption_shares = std::array::from_fn(|member| {
            let share = self.secret_shares[participant_ids[member] - 1];
            std::array::from_fn(|index| {
                aggregate[index]
                    .ephemeral
                    .mul_bigint(share.into_bigint())
                    .into_affine()
            })
        });
        let mut witness = PublicationWitness {
            auction_context: transition::pubkey_context(request.auction),
            accepted_order_count: request.accepted_order_count,
            committee_key,
            public_share_keys,
            participant_ids,
            aggregate,
            plaintexts,
            decryption_shares,
            dleq_proofs: [ChaumPedersenProof {
                commitment_g: EdwardsAffine::zero(),
                commitment_r: EdwardsAffine::zero(),
                response: JubjubScalar::zero(),
            }; QUORUM_SIZE],
        };
        let private_seed = witness.private_batch_seed();
        let (batched_ephemeral, batched_shares) = witness.batched_points(private_seed);
        witness.dleq_proofs = std::array::from_fn(|member| {
            let nonce = JubjubScalar::rand(rng);
            let commitment_g = EdwardsProjective::generator()
                .mul_bigint(nonce.into_bigint())
                .into_affine();
            let commitment_r = batched_ephemeral
                .mul_bigint(nonce.into_bigint())
                .into_affine();
            let participant_id = witness.participant_ids[member];
            let challenge = dleq_challenge_native(
                private_seed,
                participant_id,
                witness.public_share_keys[participant_id - 1],
                batched_ephemeral,
                batched_shares[member],
                commitment_g,
                commitment_r,
            );
            ChaumPedersenProof {
                commitment_g,
                commitment_r,
                response: nonce + challenge * self.secret_shares[participant_id - 1],
            }
        });
        let digest = witness.public_digest();
        let circuit = if self.devnet_fast {
            PublicationCircuit::from_witness_devnet_fast(witness)
        } else {
            PublicationCircuit::from_witness(witness)
        };
        let proof = Groth16::<Bn254>::prove(&self.proving_key, circuit, rng)?;
        let prepared_key = Groth16::<Bn254>::process_vk(&self.proving_key.vk)?;
        if !Groth16::<Bn254>::verify_with_processed_vk(&prepared_key, &[digest], &proof)? {
            return Err(std::io::Error::other("generated publication proof did not verify").into());
        }
        Ok(AggregatePublicationProofOutput {
            proof_a: transition::g1_bytes(proof.a.neg()),
            proof_b: transition::g2_bytes(proof.b),
            proof_c: transition::g1_bytes(proof.c),
            public_input: transition::field_bytes(digest),
            participant_ids: request.participant_ids,
            demand: request.demand,
            supply: request.supply,
        })
    }
}

/// Public Baby Jubjub committee configuration used by the deterministic
/// Devnet environment, without performing an expensive Groth16 setup.
pub fn deterministic_devnet_committee() -> BabyJubjubCommitteePublic {
    deterministic_devnet_committee_with_shares().1
}

fn deterministic_devnet_committee_with_shares(
) -> ([JubjubScalar; COMMITTEE_SIZE], BabyJubjubCommitteePublic) {
    let mut committee_rng = ChaCha20Rng::from_seed([0xd3; 32]);
    let secret = JubjubScalar::rand(&mut committee_rng);
    let coefficient_1 = JubjubScalar::rand(&mut committee_rng);
    let coefficient_2 = JubjubScalar::rand(&mut committee_rng);
    let secret_shares = std::array::from_fn(|index| {
        let x = JubjubScalar::from((index + 1) as u64);
        secret + coefficient_1 * x + coefficient_2 * x.square()
    });
    let committee_key = EdwardsProjective::generator()
        .mul_bigint(secret.into_bigint())
        .into_affine();
    let public_share_keys = secret_shares.map(|share| {
        EdwardsProjective::generator()
            .mul_bigint(share.into_bigint())
            .into_affine()
    });
    (
        secret_shares,
        BabyJubjubCommitteePublic {
            encryption_key: transition::wire_point_bytes(committee_key),
            participant_public_shares: public_share_keys.map(transition::wire_point_bytes),
        },
    )
}

/// Deterministic development fixture for host/SBF transcript parity tests.
/// Setup and proof randomness are intentionally unsuitable for production.
pub fn deterministic_solana_fixture(
) -> Result<SolanaAggregatePublicationFixture, Box<dyn std::error::Error>> {
    let witness = PublicationWitness::deterministic();
    let digest = witness.public_digest();
    let mut setup_rng = ChaCha20Rng::from_seed([0xe1; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(PublicationCircuit::blank(), &mut setup_rng)?;
    let mut proof_rng = ChaCha20Rng::from_seed([0xe8; 32]);
    let proof = Groth16::<Bn254>::prove(
        &proving_key,
        PublicationCircuit::from_witness(witness.clone()),
        &mut proof_rng,
    )?;
    let ic = verifying_key
        .gamma_abc_g1
        .iter()
        .copied()
        .map(transition::g1_bytes)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "publication verifying key must have two IC points")?;
    let public_share_keys = witness.public_share_keys.map(transition::wire_point_bytes);
    let aggregate = witness.aggregate.map(transition::ciphertext_bytes);
    let demand = std::array::from_fn(|level| witness.plaintexts[level * 2]);
    let supply = std::array::from_fn(|level| witness.plaintexts[level * 2 + 1]);
    Ok(SolanaAggregatePublicationFixture {
        vk_alpha_g1: transition::g1_bytes(verifying_key.alpha_g1),
        vk_beta_g2: transition::g2_bytes(verifying_key.beta_g2),
        vk_gamma_g2: transition::g2_bytes(verifying_key.gamma_g2),
        vk_delta_g2: transition::g2_bytes(verifying_key.delta_g2),
        vk_ic: ic,
        proof_a: transition::g1_bytes(proof.a.neg()),
        proof_b: transition::g2_bytes(proof.b),
        proof_c: transition::g1_bytes(proof.c),
        public_input: transition::field_bytes(digest),
        auction: transition::context_pubkey(witness.auction_context),
        accepted_order_count: witness.accepted_order_count,
        committee_key: transition::wire_point_bytes(witness.committee_key),
        participant_public_shares: public_share_keys,
        participant_ids: witness.participant_ids.map(|id| id as u8),
        aggregate,
        demand,
        supply,
    })
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cs = ConstraintSystem::<Fq>::new_ref();
    cs.set_mode(ark_relations::r1cs::SynthesisMode::Setup);
    PublicationCircuit::blank().generate_constraints(cs.clone())?;
    cs.finalize();
    println!("BeatEx 3-of-7 aggregate decryption/publication circuit");
    println!(
        "publication_constraints={} instance_variables={} witness_variables={}",
        cs.num_constraints(),
        cs.num_instance_variables(),
        cs.num_witness_variables()
    );

    let witness = PublicationWitness::deterministic();
    let digest = witness.digest();
    let setup_started = Instant::now();
    let mut setup_rng = ChaCha20Rng::from_seed([0xe1; 32]);
    let (proving_key, verifying_key) =
        Groth16::<Bn254>::circuit_specific_setup(PublicationCircuit::blank(), &mut setup_rng)?;
    let setup_time = setup_started.elapsed();
    let mut proving_key_bytes = Vec::new();
    proving_key.serialize_compressed(&mut proving_key_bytes)?;
    let prepared_key = Groth16::<Bn254>::process_vk(&verifying_key)?;
    let mut prove_times = Vec::new();
    let mut verify_times = Vec::new();
    for round in 0..3u8 {
        let mut proof_rng = ChaCha20Rng::from_seed([0xe8 + round; 32]);
        let prove_started = Instant::now();
        let proof = Groth16::<Bn254>::prove(
            &proving_key,
            PublicationCircuit::from_witness(witness.clone()),
            &mut proof_rng,
        )?;
        prove_times.push(prove_started.elapsed());
        let verify_started = Instant::now();
        assert!(Groth16::<Bn254>::verify_with_processed_vk(
            &prepared_key,
            &[digest],
            &proof,
        )?);
        verify_times.push(verify_started.elapsed());
    }
    println!(
        "publication_setup_ms={:.3}, publication_prove_median_ms={:.3}, publication_verify_median_ms={:.3}, publication_pk_bytes={}",
        setup_time.as_secs_f64() * 1_000.0,
        median(prove_times).as_secs_f64() * 1_000.0,
        median(verify_times).as_secs_f64() * 1_000.0,
        proving_key_bytes.len(),
    );
    println!(
        "publication_wire_bytes={} (34 u64 curve values + proof + statement digest)",
        CURVE_VALUES * 8 + 256 + 32
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypted_plaintexts(
        prover: &AggregatePublicationProver,
        plaintexts: [u64; CURVE_VALUES],
        seed: u8,
    ) -> [[u8; 64]; CURVE_VALUES] {
        let committee_key = decode_wire_point(prover.committee().encryption_key).unwrap();
        let mut rng = ChaCha20Rng::from_seed([seed; 32]);
        std::array::from_fn(|index| {
            transition::ciphertext_bytes(encrypt(
                u128::from(plaintexts[index]),
                JubjubScalar::rand(&mut rng),
                committee_key.into_group(),
            ))
        })
    }

    fn is_satisfied(witness: PublicationWitness, digest: Fq) -> bool {
        let circuit = PublicationCircuit {
            witness: Some(witness),
            digest: Some(digest),
            devnet_fast: false,
        };
        let cs = ConstraintSystem::<Fq>::new_ref();
        circuit.generate_constraints(cs.clone()).unwrap();
        cs.is_satisfied().unwrap()
    }

    #[test]
    fn valid_publication_is_satisfied() {
        let witness = PublicationWitness::deterministic();
        assert!(is_satisfied(witness.clone(), witness.digest()));
    }

    #[test]
    fn false_plaintext_is_rejected() {
        let witness = PublicationWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.plaintexts[5] += 1;
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn false_plaintext_with_matching_public_digest_is_rejected() {
        let mut changed = PublicationWitness::deterministic();
        changed.plaintexts[5] += 1;
        let changed_digest = changed.digest();
        assert!(!is_satisfied(changed, changed_digest));
    }

    #[test]
    fn forged_decryption_share_is_rejected() {
        let witness = PublicationWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.decryption_shares[1][9] = (changed.decryption_shares[1][9].into_group()
            + EdwardsProjective::generator())
        .into_affine();
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn wrong_registered_share_key_is_rejected() {
        let witness = PublicationWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.public_share_keys[0] = (changed.public_share_keys[0].into_group()
            + EdwardsProjective::generator())
        .into_affine();
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn replay_under_another_auction_is_rejected() {
        let witness = PublicationWitness::deterministic();
        let digest = witness.digest();
        let mut changed = witness;
        changed.auction_context[1] += Fq::from(1u64);
        assert!(!is_satisfied(changed, digest));
    }

    #[test]
    fn bounded_discrete_log_handles_boundaries_and_reuses_tables() {
        for bound in [0, 2, 3, 4, 8, 9, 15, 16, 24, 25, 1_000_000] {
            let table = bounded_discrete_log_table(bound).unwrap();
            assert!(table.giant_steps.len() <= table.step as usize + 1);
            for value in [0, 1.min(bound), bound / 2, bound] {
                let message = EdwardsProjective::generator()
                    .mul_bigint(JubjubScalar::from(value).into_bigint())
                    .into_affine();
                assert_eq!(
                    solve_bounded_discrete_log(message, bound, &table).unwrap(),
                    value
                );
            }
            let reused = bounded_discrete_log_table(bound).unwrap();
            assert!(Arc::ptr_eq(&table, &reused));
        }

        let bound = 1_000_000;
        let table = bounded_discrete_log_table(bound).unwrap();
        let above_bound = EdwardsProjective::generator()
            .mul_bigint(JubjubScalar::from(bound + 1).into_bigint())
            .into_affine();
        assert!(solve_bounded_discrete_log(above_bound, bound, &table).is_err());
    }

    #[test]
    fn aggregate_decryption_handles_realistic_mixed_quantities() {
        let prover = AggregatePublicationProver::deterministic_devnet_fast().unwrap();
        let bound = 1_000_000;
        let plaintexts =
            std::array::from_fn(|index| [0, 1, 1023, 1024, 1025, bound - 1, bound][index % 7]);
        let aggregate = encrypted_plaintexts(&prover, plaintexts, 0xb5);
        let (demand, supply) = prover
            .decrypt_aggregate(aggregate, [1, 4, 7], bound)
            .unwrap();
        assert_eq!(demand, std::array::from_fn(|level| plaintexts[level * 2]));
        assert_eq!(
            supply,
            std::array::from_fn(|level| plaintexts[level * 2 + 1])
        );
    }

    #[test]
    #[ignore = "benchmarks the Devnet-fast aggregate-publication Groth16 proof"]
    fn arbitrary_publication_prover_generates_a_verified_proof() {
        let witness = PublicationWitness::deterministic();
        let started = std::time::Instant::now();
        let prover = AggregatePublicationProver::deterministic_devnet_fast().unwrap();
        let setup_elapsed = started.elapsed();
        assert_eq!(
            prover.committee().encryption_key,
            transition::wire_point_bytes(witness.committee_key)
        );
        let participant_ids = witness.participant_ids.map(|id| id as u8);
        let aggregate = witness.aggregate.map(transition::ciphertext_bytes);
        let (demand, supply) = prover
            .decrypt_aggregate(aggregate, participant_ids, 64)
            .unwrap();
        let request = AggregatePublicationRequest {
            auction: transition::context_pubkey(witness.auction_context),
            accepted_order_count: witness.accepted_order_count,
            participant_ids,
            aggregate,
            demand,
            supply,
        };
        let mut rng = ChaCha20Rng::from_seed([0xea; 32]);
        let started = std::time::Instant::now();
        let output = prover.prove(request, &mut rng).unwrap();
        let proof_elapsed = started.elapsed();
        assert_eq!(output.participant_ids, [1, 4, 7]);
        assert_eq!(output.demand[0], witness.plaintexts[0]);
        assert_eq!(output.supply[0], witness.plaintexts[1]);
        assert_ne!(output.public_input, [0; 32]);
        eprintln!(
            "devnet_fast_publication_setup_ms={:.3} prove_ms={:.3}",
            setup_elapsed.as_secs_f64() * 1_000.0,
            proof_elapsed.as_secs_f64() * 1_000.0
        );
    }

    #[test]
    #[ignore = "benchmarks the complete Devnet-fast publication pipeline at realistic bounds"]
    fn benchmark_publication_pipeline_realistic_bounds() {
        let setup_started = std::time::Instant::now();
        let prover = AggregatePublicationProver::deterministic_devnet_fast().unwrap();
        let setup_elapsed = setup_started.elapsed();
        for (bound, seed) in [(1_000_000u64, 0xc1u8), (100_000_000u64, 0xc2u8)] {
            let step = bounded_discrete_log_step(bound).unwrap();
            let plaintexts = std::array::from_fn(|index| {
                [0, 1, step - 1, step, step + 1, bound - 1, bound][index % 7]
            });
            let aggregate = encrypted_plaintexts(&prover, plaintexts, seed);
            let cold_started = std::time::Instant::now();
            let (demand, supply) = prover
                .decrypt_aggregate(aggregate, [1, 4, 7], bound)
                .unwrap();
            let cold_decrypt = cold_started.elapsed();
            let request = AggregatePublicationRequest {
                auction: [seed; 32],
                accepted_order_count: 100,
                participant_ids: [1, 4, 7],
                aggregate,
                demand,
                supply,
            };
            let mut decrypt_times = Vec::new();
            let mut prove_times = Vec::new();
            let mut total_times = Vec::new();
            for round in 0..5u8 {
                let total_started = std::time::Instant::now();
                let decrypt_started = std::time::Instant::now();
                let (demand, supply) = prover
                    .decrypt_aggregate(aggregate, [1, 4, 7], bound)
                    .unwrap();
                decrypt_times.push(decrypt_started.elapsed());
                let mut round_request = request.clone();
                round_request.demand = demand;
                round_request.supply = supply;
                let mut rng = ChaCha20Rng::from_seed([seed.wrapping_add(round); 32]);
                let prove_started = std::time::Instant::now();
                prover.prove(round_request, &mut rng).unwrap();
                prove_times.push(prove_started.elapsed());
                total_times.push(total_started.elapsed());
            }
            decrypt_times.sort();
            prove_times.sort();
            total_times.sort();
            eprintln!(
                "publication_bound={bound} step={step} setup_ms={:.3} cold_decrypt_ms={:.3} warm_decrypt_median_ms={:.3} warm_decrypt_p95_ms={:.3} prove_median_ms={:.3} prove_p95_ms={:.3} total_median_ms={:.3} total_p95_ms={:.3}",
                setup_elapsed.as_secs_f64() * 1_000.0,
                cold_decrypt.as_secs_f64() * 1_000.0,
                decrypt_times[2].as_secs_f64() * 1_000.0,
                decrypt_times[4].as_secs_f64() * 1_000.0,
                prove_times[2].as_secs_f64() * 1_000.0,
                prove_times[4].as_secs_f64() * 1_000.0,
                total_times[2].as_secs_f64() * 1_000.0,
                total_times[4].as_secs_f64() * 1_000.0,
            );
        }
    }
}

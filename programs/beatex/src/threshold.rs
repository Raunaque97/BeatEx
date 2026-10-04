use {
    crate::{BeatExError, KeyEpoch, COMMITTEE_SIZE, RISTRETTO_IDENTITY},
    anchor_lang::prelude::*,
    solana_curve25519::{
        ristretto::{multiscalar_multiply_ristretto, validate_ristretto, PodRistrettoPoint},
        scalar::PodScalar,
    },
    solana_sha256_hasher::hash,
};

#[allow(dead_code)]
mod generated_constants {
    include!(concat!(env!("OUT_DIR"), "/crypto_constants.rs"));
}
use generated_constants::{G_BYTES, H_BYTES, LAGRANGE_COEFFICIENTS};

const DECRYPTION_SHARE_DOMAIN: &[u8] = b"beatex-decryption-share-v2";
const KEY_RELATION_DOMAIN: &[u8] = b"beatex-key-relation-v2";
const PROOF_CONTEXT_VERSION: u8 = 2;
const PROOF_CONTEXT_LEN: usize = 149;
const KEY_RELATION_CONTEXT_LEN: usize = 105;
const SCALAR_MODULUS: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];
const ONE: PodScalar = PodScalar([
    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
]);

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
pub struct DleqProofInput {
    pub commitment_1: [u8; 32],
    pub commitment_2: [u8; 32],
    pub response: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecryptionShareInput {
    pub participant_index: u8,
    pub point: [u8; 32],
    pub proof: DleqProofInput,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct VerifyThresholdDecryptionArgs {
    pub auction_id: u64,
    pub side: u8,
    pub position: u16,
    pub ciphertext: [u8; 64],
    pub claimed_amount: u64,
    pub shares: [DecryptionShareInput; 3],
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_key_epoch_configuration(
    program_id: &Pubkey,
    market: &Pubkey,
    key_epoch_address: &Pubkey,
    epoch: u64,
    encryption_pubkey: [u8; 32],
    coefficient_commitments: &[[u8; 32]; 3],
    participant_public_shares: &[[u8; 32]; COMMITTEE_SIZE],
    key_relation_proof: &DleqProofInput,
) -> Result<()> {
    require!(
        encryption_pubkey != RISTRETTO_IDENTITY
            && coefficient_commitments[0] != RISTRETTO_IDENTITY
            && coefficient_commitments[2] != RISTRETTO_IDENTITY
            && participant_public_shares
                .iter()
                .all(|share| *share != RISTRETTO_IDENTITY),
        BeatExError::IdentityPoint
    );
    require!(
        validate_ristretto(&PodRistrettoPoint(encryption_pubkey))
            && coefficient_commitments
                .iter()
                .all(|point| validate_ristretto(&PodRistrettoPoint(*point)))
            && participant_public_shares
                .iter()
                .all(|point| validate_ristretto(&PodRistrettoPoint(*point))),
        BeatExError::InvalidRistrettoPoint
    );
    let encryption_pubkey = PodRistrettoPoint(encryption_pubkey);
    let h = PodRistrettoPoint(H_BYTES);
    let constant_commitment = PodRistrettoPoint(coefficient_commitments[0]);
    let proof_commitment_1 = PodRistrettoPoint(key_relation_proof.commitment_1);
    let proof_commitment_2 = PodRistrettoPoint(key_relation_proof.commitment_2);
    let response = PodScalar(key_relation_proof.response);
    let context = key_relation_context(program_id, market, key_epoch_address, epoch);
    let challenge = dleq_challenge(
        KEY_RELATION_DOMAIN,
        &context,
        &encryption_pubkey,
        &h,
        &h,
        &constant_commitment,
        &proof_commitment_1,
        &proof_commitment_2,
    );
    let negative_challenge = negate_scalar(challenge);

    verify_dleq_equation(
        response,
        negative_challenge,
        encryption_pubkey,
        h,
        proof_commitment_1,
    )?;
    verify_dleq_equation(
        response,
        negative_challenge,
        h,
        constant_commitment,
        proof_commitment_2,
    )?;

    for (position, participant_public_share) in participant_public_shares.iter().enumerate() {
        let index = (position + 1) as u64;
        let index_squared = index * index;
        let scalars = [
            ONE,
            negate_scalar(ONE),
            negate_scalar(scalar_from_u64(index)),
            negate_scalar(scalar_from_u64(index_squared)),
        ];
        let points = [
            PodRistrettoPoint(*participant_public_share),
            constant_commitment,
            PodRistrettoPoint(coefficient_commitments[1]),
            PodRistrettoPoint(coefficient_commitments[2]),
        ];
        let result = multiscalar_multiply_ristretto(&scalars, &points)
            .ok_or_else(|| error!(BeatExError::InvalidCurveInput))?;
        require!(
            result == PodRistrettoPoint(RISTRETTO_IDENTITY),
            BeatExError::InvalidPublicShare
        );
    }

    Ok(())
}

pub fn verify_threshold_decryption(
    program_id: &Pubkey,
    key_epoch_address: &Pubkey,
    key_epoch: &KeyEpoch,
    args: &VerifyThresholdDecryptionArgs,
) -> Result<()> {
    require!(
        key_epoch.threshold == crate::THRESHOLD,
        BeatExError::InvalidKeyEpoch
    );
    require!(
        key_epoch.proof_transcript_version == PROOF_CONTEXT_VERSION,
        BeatExError::InvalidKeyEpoch
    );
    require!(args.side <= 1, BeatExError::InvalidSide);

    let indices = args.shares.map(|share| share.participant_index);
    require!(
        indices[0] < indices[1] && indices[1] < indices[2],
        BeatExError::ParticipantOrder
    );
    require!(
        indices[0] >= 1 && indices[2] as usize <= COMMITTEE_SIZE,
        BeatExError::UnknownParticipant
    );

    let commitment = PodRistrettoPoint(
        args.ciphertext[..32]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let handle = PodRistrettoPoint(
        args.ciphertext[32..]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let h = PodRistrettoPoint(H_BYTES);

    for share in &args.shares {
        let participant = PodRistrettoPoint(
            key_epoch.participant_public_shares[(share.participant_index - 1) as usize],
        );
        let share_point = PodRistrettoPoint(share.point);
        let proof_commitment_1 = PodRistrettoPoint(share.proof.commitment_1);
        let proof_commitment_2 = PodRistrettoPoint(share.proof.commitment_2);
        let response = PodScalar(share.proof.response);
        let context = proof_context(
            program_id,
            key_epoch_address,
            key_epoch,
            args,
            share.participant_index,
        );
        let challenge = dleq_challenge(
            DECRYPTION_SHARE_DOMAIN,
            &context,
            &h,
            &participant,
            &handle,
            &share_point,
            &proof_commitment_1,
            &proof_commitment_2,
        );
        let negative_challenge = negate_scalar(challenge);

        verify_dleq_equation(
            response,
            negative_challenge,
            h,
            participant,
            proof_commitment_1,
        )?;
        verify_dleq_equation(
            response,
            negative_challenge,
            handle,
            share_point,
            proof_commitment_2,
        )?;
    }

    let negative_lagrange = LAGRANGE_COEFFICIENTS
        .iter()
        .find_map(|(candidate, coefficients)| (*candidate == indices).then_some(*coefficients))
        .ok_or_else(|| error!(BeatExError::UnknownParticipantSet))?;
    let negative_amount = negate_scalar(scalar_from_u64(args.claimed_amount));
    let scalars = [
        ONE,
        PodScalar(negative_lagrange[0]),
        PodScalar(negative_lagrange[1]),
        PodScalar(negative_lagrange[2]),
        negative_amount,
    ];
    let points = [
        commitment,
        PodRistrettoPoint(args.shares[0].point),
        PodRistrettoPoint(args.shares[1].point),
        PodRistrettoPoint(args.shares[2].point),
        PodRistrettoPoint(G_BYTES),
    ];

    let result = multiscalar_multiply_ristretto(&scalars, &points)
        .ok_or_else(|| error!(BeatExError::InvalidCurveInput))?;
    require!(
        result == PodRistrettoPoint(RISTRETTO_IDENTITY),
        BeatExError::PlaintextMismatch
    );

    Ok(())
}

fn verify_dleq_equation(
    response: PodScalar,
    negative_challenge: PodScalar,
    base: PodRistrettoPoint,
    public: PodRistrettoPoint,
    proof_commitment: PodRistrettoPoint,
) -> Result<()> {
    let scalars = [response, negative_challenge, negate_scalar(ONE)];
    let points = [base, public, proof_commitment];
    let result = multiscalar_multiply_ristretto(&scalars, &points)
        .ok_or_else(|| error!(BeatExError::InvalidCurveInput))?;

    require!(
        result == PodRistrettoPoint(RISTRETTO_IDENTITY),
        BeatExError::InvalidDecryptionProof
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn dleq_challenge(
    domain: &[u8],
    context: &[u8],
    base_1: &PodRistrettoPoint,
    result_1: &PodRistrettoPoint,
    base_2: &PodRistrettoPoint,
    result_2: &PodRistrettoPoint,
    commitment_1: &PodRistrettoPoint,
    commitment_2: &PodRistrettoPoint,
) -> PodScalar {
    // The SHA-256 syscall accepts at most 20 slices. Serializing the exact
    // transcript into one bounded buffer avoids that runtime-only limit while
    // preserving the offchain byte format.
    let mut transcript = [0_u8; 561];
    let mut length = 0;
    append_transcript_item(&mut transcript, &mut length, b"domain", domain);
    append_transcript_item(&mut transcript, &mut length, b"context", context);
    append_transcript_item(&mut transcript, &mut length, b"base-1", &base_1.0);
    append_transcript_item(&mut transcript, &mut length, b"result-1", &result_1.0);
    append_transcript_item(&mut transcript, &mut length, b"base-2", &base_2.0);
    append_transcript_item(&mut transcript, &mut length, b"result-2", &result_2.0);
    append_transcript_item(
        &mut transcript,
        &mut length,
        b"commitment-1",
        &commitment_1.0,
    );
    append_transcript_item(
        &mut transcript,
        &mut length,
        b"commitment-2",
        &commitment_2.0,
    );
    let mut digest = hash(&transcript[..length]).to_bytes();
    digest[31] &= 0x0f;
    PodScalar(digest)
}

fn append_transcript_item(
    transcript: &mut [u8; 561],
    length: &mut usize,
    label: &[u8],
    value: &[u8],
) {
    let label_len = (label.len() as u64).to_le_bytes();
    let value_len = (value.len() as u64).to_le_bytes();
    for bytes in [&label_len[..], label, &value_len[..], value] {
        let end = *length + bytes.len();
        transcript[*length..end].copy_from_slice(bytes);
        *length = end;
    }
}

fn key_relation_context(
    program_id: &Pubkey,
    market: &Pubkey,
    key_epoch_address: &Pubkey,
    epoch: u64,
) -> [u8; KEY_RELATION_CONTEXT_LEN] {
    let mut context = [0_u8; KEY_RELATION_CONTEXT_LEN];
    context[0] = PROOF_CONTEXT_VERSION;
    context[1..33].copy_from_slice(program_id.as_ref());
    context[33..65].copy_from_slice(market.as_ref());
    context[65..97].copy_from_slice(key_epoch_address.as_ref());
    context[97..105].copy_from_slice(&epoch.to_le_bytes());
    context
}

fn proof_context(
    program_id: &Pubkey,
    key_epoch_address: &Pubkey,
    key_epoch: &KeyEpoch,
    args: &VerifyThresholdDecryptionArgs,
    participant_index: u8,
) -> [u8; PROOF_CONTEXT_LEN] {
    let mut context = [0_u8; PROOF_CONTEXT_LEN];
    context[0] = PROOF_CONTEXT_VERSION;
    context[1..33].copy_from_slice(program_id.as_ref());
    context[33..65].copy_from_slice(key_epoch.market.as_ref());
    context[65..97].copy_from_slice(key_epoch_address.as_ref());
    context[97..105].copy_from_slice(&key_epoch.epoch.to_le_bytes());
    context[105..113].copy_from_slice(&args.auction_id.to_le_bytes());
    context[113] = args.side;
    context[114..116].copy_from_slice(&args.position.to_le_bytes());
    context[116] = participant_index;
    context[117..149].copy_from_slice(&args.ciphertext[..32]);
    context
}

fn scalar_from_u64(value: u64) -> PodScalar {
    let mut bytes = [0_u8; 32];
    bytes[..8].copy_from_slice(&value.to_le_bytes());
    PodScalar(bytes)
}

fn negate_scalar(value: PodScalar) -> PodScalar {
    if value.0 == [0; 32] {
        return value;
    }

    let mut output = [0_u8; 32];
    let mut borrow = 0_i16;
    for (position, output_byte) in output.iter_mut().enumerate() {
        let difference = SCALAR_MODULUS[position] as i16 - value.0[position] as i16 - borrow;
        if difference < 0 {
            *output_byte = (difference + 256) as u8;
            borrow = 1;
        } else {
            *output_byte = difference as u8;
            borrow = 0;
        }
    }
    PodScalar(output)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        beatex_threshold_crypto::{
            create_decryption_share, dealer_setup_3_of_7, DecryptionProofContext,
            KeyRelationProofContext,
        },
    };

    const SETUP_CONTEXT: &[u8] = b"beatex/onchain-test/market/key-epoch-1";

    #[test]
    fn every_three_of_seven_subset_verifies_with_program_logic() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let program_id = crate::ID;
        let market = Pubkey::new_from_array([7; 32]);
        let key_epoch_address = Pubkey::new_from_array([8; 32]);
        let ciphertext = setup.public.encryption_pubkey.encrypt_u64(47);
        let ciphertext_bytes = ciphertext.to_bytes();
        let key_epoch = KeyEpoch {
            authority: Pubkey::new_from_array([9; 32]),
            market,
            epoch: 1,
            encryption_pubkey: setup.public.encryption_pubkey.to_bytes(),
            babyjub_encryption_pubkey: [0; 32],
            babyjub_participant_public_shares: [[0; 32]; COMMITTEE_SIZE],
            coefficient_commitments: setup
                .public
                .coefficient_commitments
                .iter()
                .map(|commitment| commitment.compress().to_bytes())
                .collect::<Vec<_>>()
                .try_into()
                .expect("the setup contains three coefficient commitments"),
            participant_public_shares: setup
                .public
                .participant_shares
                .iter()
                .map(|share| share.point.compress().to_bytes())
                .collect::<Vec<_>>()
                .try_into()
                .expect("the setup contains seven shares"),
            participant_authorities: [Pubkey::new_from_array([10; 32]); 7],
            key_relation_proof: DleqProofInput {
                commitment_1: setup.public.key_relation_proof.commitment_1_bytes(),
                commitment_2: setup.public.key_relation_proof.commitment_2_bytes(),
                response: setup.public.key_relation_proof.response_bytes(),
            },
            threshold: 3,
            proof_transcript_version: 2,
            activation_slot: 1,
            retired: false,
            acknowledgement_bitmap: 0x7f,
            bump: 254,
        };

        for first in 0..5 {
            for second in (first + 1)..6 {
                for third in (second + 1)..7 {
                    let shares = [first, second, third].map(|share_position| {
                        let participant_index = setup.secret_shares[share_position].index() as u8;
                        let context = DecryptionProofContext {
                            program_id: program_id.to_bytes(),
                            market: market.to_bytes(),
                            key_epoch_address: key_epoch_address.to_bytes(),
                            epoch: 1,
                            auction_id: 42,
                            side: 0,
                            position: 3,
                            participant_index,
                            commitment: ciphertext_bytes[..32]
                                .try_into()
                                .expect("ciphertext commitment is 32 bytes"),
                        }
                        .to_bytes();
                        let share = create_decryption_share(
                            &setup.secret_shares[share_position],
                            &ciphertext,
                            &context,
                        );

                        DecryptionShareInput {
                            participant_index,
                            point: share.point_bytes(),
                            proof: DleqProofInput {
                                commitment_1: share.proof.commitment_1_bytes(),
                                commitment_2: share.proof.commitment_2_bytes(),
                                response: share.proof.response_bytes(),
                            },
                        }
                    });
                    let args = VerifyThresholdDecryptionArgs {
                        auction_id: 42,
                        side: 0,
                        position: 3,
                        ciphertext: ciphertext_bytes,
                        claimed_amount: 47,
                        shares,
                    };

                    assert!(verify_threshold_decryption(
                        &program_id,
                        &key_epoch_address,
                        &key_epoch,
                        &args,
                    )
                    .is_ok());

                    let bad_plaintext = VerifyThresholdDecryptionArgs {
                        claimed_amount: 48,
                        ..args
                    };
                    assert!(verify_threshold_decryption(
                        &program_id,
                        &key_epoch_address,
                        &key_epoch,
                        &bad_plaintext,
                    )
                    .is_err());
                }
            }
        }
    }

    #[test]
    fn key_epoch_configuration_checks_key_relation_and_all_public_shares() {
        let program_id = crate::ID;
        let market = Pubkey::new_from_array([7; 32]);
        let key_epoch_address = Pubkey::new_from_array([8; 32]);
        let context = KeyRelationProofContext {
            program_id: program_id.to_bytes(),
            market: market.to_bytes(),
            key_epoch_address: key_epoch_address.to_bytes(),
            epoch: 1,
        }
        .to_bytes();
        let setup = dealer_setup_3_of_7(&context);
        let coefficient_commitments = setup
            .public
            .coefficient_commitments
            .iter()
            .map(|commitment| commitment.compress().to_bytes())
            .collect::<Vec<_>>()
            .try_into()
            .expect("the setup contains three coefficient commitments");
        let participant_public_shares = setup
            .public
            .participant_shares
            .iter()
            .map(|share| share.point.compress().to_bytes())
            .collect::<Vec<_>>()
            .try_into()
            .expect("the setup contains seven shares");
        let key_relation_proof = DleqProofInput {
            commitment_1: setup.public.key_relation_proof.commitment_1_bytes(),
            commitment_2: setup.public.key_relation_proof.commitment_2_bytes(),
            response: setup.public.key_relation_proof.response_bytes(),
        };

        assert!(verify_key_epoch_configuration(
            &program_id,
            &market,
            &key_epoch_address,
            1,
            setup.public.encryption_pubkey.to_bytes(),
            &coefficient_commitments,
            &participant_public_shares,
            &key_relation_proof,
        )
        .is_ok());

        let mut corrupted_shares = participant_public_shares;
        corrupted_shares[3] = participant_public_shares[4];
        assert!(verify_key_epoch_configuration(
            &program_id,
            &market,
            &key_epoch_address,
            1,
            setup.public.encryption_pubkey.to_bytes(),
            &coefficient_commitments,
            &corrupted_shares,
            &key_relation_proof,
        )
        .is_err());

        let mut corrupted_proof = key_relation_proof;
        corrupted_proof.response[0] ^= 1;
        assert!(verify_key_epoch_configuration(
            &program_id,
            &market,
            &key_epoch_address,
            1,
            setup.public.encryption_pubkey.to_bytes(),
            &coefficient_commitments,
            &participant_public_shares,
            &corrupted_proof,
        )
        .is_err());
    }
}

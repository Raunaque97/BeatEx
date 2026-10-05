//! Threshold-decryption feasibility harness for BeatEx.
//!
//! The ciphertext and group types in this crate come directly from Solana's
//! `solana-zk-sdk`. This crate adds dealer-assisted Shamir sharing and
//! Chaum-Pedersen proofs; it does not define an alternative ElGamal format.

use {
    curve25519_dalek::{
        ristretto::RistrettoPoint,
        scalar::Scalar,
        traits::{Identity, IsIdentity},
    },
    rand::rngs::OsRng,
    sha2::{Digest, Sha256},
    solana_zk_sdk::encryption::{
        elgamal::{ElGamalCiphertext, ElGamalKeypair, ElGamalPubkey, ElGamalSecretKey},
        pedersen::{G, H},
    },
    std::collections::BTreeSet,
    thiserror::Error,
    zeroize::Zeroize,
};

/// Domain for proving that `P = s^-1 H` and `A0 = sH` use the same `s`.
const KEY_RELATION_DOMAIN: &[u8] = b"beatex-key-relation-v2";

/// Domain for proving that a participant computed `T_j = s_j D`.
const DECRYPTION_SHARE_DOMAIN: &[u8] = b"beatex-decryption-share-v2";

pub const DECRYPTION_PROOF_CONTEXT_LEN: usize = 149;
pub const KEY_RELATION_PROOF_CONTEXT_LEN: usize = 105;

/// Fixed-width public context bound into a dealer key-relation proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyRelationProofContext {
    pub program_id: [u8; 32],
    pub market: [u8; 32],
    pub key_epoch_address: [u8; 32],
    pub epoch: u64,
}

impl KeyRelationProofContext {
    pub fn to_bytes(&self) -> [u8; KEY_RELATION_PROOF_CONTEXT_LEN] {
        let mut context = [0_u8; KEY_RELATION_PROOF_CONTEXT_LEN];
        context[0] = 2;
        context[1..33].copy_from_slice(&self.program_id);
        context[33..65].copy_from_slice(&self.market);
        context[65..97].copy_from_slice(&self.key_epoch_address);
        context[97..105].copy_from_slice(&self.epoch.to_le_bytes());
        context
    }
}

/// Fixed-width public context bound into a threshold-decryption share proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecryptionProofContext {
    pub program_id: [u8; 32],
    pub market: [u8; 32],
    pub key_epoch_address: [u8; 32],
    pub epoch: u64,
    pub auction_id: u64,
    pub side: u8,
    pub position: u16,
    pub participant_index: u8,
    pub commitment: [u8; 32],
}

impl DecryptionProofContext {
    pub fn to_bytes(&self) -> [u8; DECRYPTION_PROOF_CONTEXT_LEN] {
        let mut context = [0_u8; DECRYPTION_PROOF_CONTEXT_LEN];
        context[0] = 2;
        context[1..33].copy_from_slice(&self.program_id);
        context[33..65].copy_from_slice(&self.market);
        context[65..97].copy_from_slice(&self.key_epoch_address);
        context[97..105].copy_from_slice(&self.epoch.to_le_bytes());
        context[105..113].copy_from_slice(&self.auction_id.to_le_bytes());
        context[113] = self.side;
        context[114..116].copy_from_slice(&self.position.to_le_bytes());
        context[116] = self.participant_index;
        context[117..149].copy_from_slice(&self.commitment);
        context
    }
}

/// Public information for one committee participant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantPublicShare {
    pub index: u64,
    pub point: RistrettoPoint,
}

/// A committee participant's secret Shamir share.
///
/// The scalar is redacted from `Debug` output and zeroized on drop.
pub struct ParticipantSecretShare {
    index: u64,
    scalar: Scalar,
    public: ParticipantPublicShare,
}

impl ParticipantSecretShare {
    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn public_share(&self) -> &ParticipantPublicShare {
        &self.public
    }
}

impl std::fmt::Debug for ParticipantSecretShare {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ParticipantSecretShare")
            .field("index", &self.index)
            .field("scalar", &"[REDACTED]")
            .field("public", &self.public)
            .finish()
    }
}

impl Drop for ParticipantSecretShare {
    fn drop(&mut self) {
        self.scalar.zeroize();
    }
}

/// A non-interactive Chaum-Pedersen equality-of-discrete-logs proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DleqProof {
    commitment_1: RistrettoPoint,
    commitment_2: RistrettoPoint,
    response: Scalar,
}

impl DleqProof {
    pub fn commitment_1_bytes(&self) -> [u8; 32] {
        self.commitment_1.compress().to_bytes()
    }

    pub fn commitment_2_bytes(&self) -> [u8; 32] {
        self.commitment_2.compress().to_bytes()
    }

    pub fn response_bytes(&self) -> [u8; 32] {
        self.response.to_bytes()
    }
}

/// One participant's partial decryption and its correctness proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecryptionShare {
    pub participant_index: u64,
    pub point: RistrettoPoint,
    pub proof: DleqProof,
}

impl DecryptionShare {
    pub fn point_bytes(&self) -> [u8; 32] {
        self.point.compress().to_bytes()
    }
}

/// Public threshold configuration that can be persisted onchain later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThresholdPublicConfig {
    pub threshold: usize,
    pub participant_count: usize,
    pub encryption_pubkey: ElGamalPubkey,
    pub coefficient_commitments: Vec<RistrettoPoint>,
    pub participant_shares: Vec<ParticipantPublicShare>,
    pub key_relation_proof: DleqProof,
}

/// Output of the prototype dealer-assisted setup.
///
/// The master scalar is not retained. Only the individual shares remain.
#[derive(Debug)]
pub struct DealerSetup {
    pub public: ThresholdPublicConfig,
    pub secret_shares: Vec<ParticipantSecretShare>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ThresholdError {
    #[error("threshold parameters are invalid")]
    InvalidParameters,
    #[error("public threshold configuration is invalid")]
    InvalidSetup,
    #[error("not enough valid decryption shares")]
    InsufficientShares,
    #[error("the number of proof contexts does not match the number of shares")]
    ContextCountMismatch,
    #[error("participant index {0} appears more than once")]
    DuplicateParticipant(u64),
    #[error("participant index {0} is not registered")]
    UnknownParticipant(u64),
    #[error("participant index {0} supplied an invalid decryption proof")]
    InvalidDecryptionProof(u64),
    #[error("the claimed plaintext does not match the threshold decryption")]
    PlaintextMismatch,
    #[error("participant index {0} supplied an invalid secret share")]
    InvalidSecretShare(u64),
    #[error("the reconstructed secret does not match the committee encryption key")]
    ReconstructedKeyMismatch,
}

/// Create the prototype's fixed 3-of-7 dealer-assisted committee.
pub fn dealer_setup_3_of_7(context: &[u8]) -> DealerSetup {
    dealer_setup(3, 7, context).expect("the fixed 3-of-7 parameters are valid")
}

/// Create a dealer-assisted threshold key around Solana's twisted ElGamal key.
///
/// `context` must canonically bind the protocol version, program, market, and
/// key epoch. The caller is responsible for persisting and reproducing it.
pub fn dealer_setup(
    threshold: usize,
    participant_count: usize,
    context: &[u8],
) -> Result<DealerSetup, ThresholdError> {
    if threshold == 0 || participant_count < threshold || participant_count > u32::MAX as usize {
        return Err(ThresholdError::InvalidParameters);
    }

    let keypair = ElGamalKeypair::new_rand();
    let mut master_secret = *keypair.secret().get_scalar();
    let encryption_pubkey = keypair.pubkey_owned();

    let mut coefficients = Vec::with_capacity(threshold);
    coefficients.push(master_secret);
    for _ in 1..threshold {
        coefficients.push(Scalar::random(&mut OsRng));
    }

    let coefficient_commitments = coefficients
        .iter()
        .map(|coefficient| coefficient * *H)
        .collect::<Vec<_>>();

    let key_relation_proof = prove_equal_discrete_logs(
        &master_secret,
        encryption_pubkey.get_point(),
        &H,
        &H,
        &coefficient_commitments[0],
        KEY_RELATION_DOMAIN,
        context,
    );

    let mut participant_shares = Vec::with_capacity(participant_count);
    let mut secret_shares = Vec::with_capacity(participant_count);

    for participant_index in 1..=participant_count {
        let index = participant_index as u64;
        let scalar = evaluate_polynomial(&coefficients, Scalar::from(index));
        let public = ParticipantPublicShare {
            index,
            point: scalar * *H,
        };

        participant_shares.push(public.clone());
        secret_shares.push(ParticipantSecretShare {
            index,
            scalar,
            public,
        });
    }

    coefficients.zeroize();
    master_secret.zeroize();

    let public = ThresholdPublicConfig {
        threshold,
        participant_count,
        encryption_pubkey,
        coefficient_commitments,
        participant_shares,
        key_relation_proof,
    };

    if !public.verify(context) {
        return Err(ThresholdError::InvalidSetup);
    }

    Ok(DealerSetup {
        public,
        secret_shares,
    })
}

/// Reconstruct the committee ElGamal key from a threshold-sized set of secret
/// shares and verify it against the registered public configuration.
///
/// This is intentionally an offchain-only escape hatch for Token-2022 proof
/// generation. The returned SDK keypair zeroizes its secret on drop. Callers
/// should keep its lifetime as short as possible and must not persist the
/// reconstructed key.
pub fn reconstruct_elgamal_keypair(
    config: &ThresholdPublicConfig,
    shares: &[&ParticipantSecretShare],
) -> Result<ElGamalKeypair, ThresholdError> {
    if shares.len() < config.threshold {
        return Err(ThresholdError::InsufficientShares);
    }

    let mut seen_indices = BTreeSet::new();
    for share in shares {
        if !seen_indices.insert(share.index) {
            return Err(ThresholdError::DuplicateParticipant(share.index));
        }
        let registered = config
            .participant(share.index)
            .ok_or(ThresholdError::UnknownParticipant(share.index))?;
        if registered != &share.public || share.scalar * *H != share.public.point {
            return Err(ThresholdError::InvalidSecretShare(share.index));
        }
    }

    let mut master_secret = Scalar::ZERO;
    for share in shares {
        let x_i = Scalar::from(share.index);
        let coefficient = shares
            .iter()
            .filter(|other| other.index != share.index)
            .fold(Scalar::ONE, |coefficient, other| {
                let x_j = Scalar::from(other.index);
                coefficient * (-x_j) * (x_i - x_j).invert()
            });
        master_secret += coefficient * share.scalar;
    }

    let keypair = ElGamalKeypair::new(ElGamalSecretKey::from(master_secret));
    if keypair.pubkey() != &config.encryption_pubkey {
        return Err(ThresholdError::ReconstructedKeyMismatch);
    }
    Ok(keypair)
}

impl ThresholdPublicConfig {
    /// Verify the dealer's key relation and every participant's Feldman share.
    pub fn verify(&self, context: &[u8]) -> bool {
        if self.threshold == 0
            || self.participant_count < self.threshold
            || self.coefficient_commitments.len() != self.threshold
            || self.participant_shares.len() != self.participant_count
            || self.encryption_pubkey.get_point().is_identity()
            || self.coefficient_commitments[0].is_identity()
            || self.coefficient_commitments[self.threshold - 1].is_identity()
            || self
                .participant_shares
                .iter()
                .any(|share| share.point.is_identity())
        {
            return false;
        }

        if !verify_equal_discrete_logs(
            self.encryption_pubkey.get_point(),
            &H,
            &H,
            &self.coefficient_commitments[0],
            &self.key_relation_proof,
            KEY_RELATION_DOMAIN,
            context,
        ) {
            return false;
        }

        let mut seen_indices = BTreeSet::new();
        self.participant_shares
            .iter()
            .enumerate()
            .all(|(position, share)| {
                share.index == position as u64 + 1
                    && seen_indices.insert(share.index)
                    && share.point
                        == evaluate_commitment_polynomial(
                            &self.coefficient_commitments,
                            Scalar::from(share.index),
                        )
            })
    }

    pub fn participant(&self, index: u64) -> Option<&ParticipantPublicShare> {
        self.participant_shares
            .iter()
            .find(|participant| participant.index == index)
    }
}

/// Produce a verifiable partial decryption for one ciphertext.
pub fn create_decryption_share(
    share: &ParticipantSecretShare,
    ciphertext: &ElGamalCiphertext,
    context: &[u8],
) -> DecryptionShare {
    let handle = ciphertext.handle.get_point();
    let point = share.scalar * handle;
    let proof = prove_equal_discrete_logs(
        &share.scalar,
        &H,
        &share.public.point,
        handle,
        &point,
        DECRYPTION_SHARE_DOMAIN,
        context,
    );

    DecryptionShare {
        participant_index: share.index,
        point,
        proof,
    }
}

/// Verify one participant's partial decryption against the exact ciphertext.
pub fn verify_decryption_share(
    participant: &ParticipantPublicShare,
    ciphertext: &ElGamalCiphertext,
    share: &DecryptionShare,
    context: &[u8],
) -> bool {
    participant.index == share.participant_index
        && verify_equal_discrete_logs(
            &H,
            &participant.point,
            ciphertext.handle.get_point(),
            &share.point,
            &share.proof,
            DECRYPTION_SHARE_DOMAIN,
            context,
        )
}

/// Combine valid shares and verify a caller-supplied aggregate plaintext.
///
/// The discrete logarithm is intentionally solved offchain. This function
/// mirrors the eventual onchain operation: combine shares into `sD`, subtract
/// it from the commitment, and check the supplied `amount * G`.
pub fn verify_plaintext_with_shares(
    config: &ThresholdPublicConfig,
    ciphertext: &ElGamalCiphertext,
    shares: &[DecryptionShare],
    claimed_amount: u64,
    context: &[u8],
) -> Result<(), ThresholdError> {
    if shares.len() < config.threshold {
        return Err(ThresholdError::InsufficientShares);
    }

    let mut seen_indices = BTreeSet::new();
    for share in shares {
        if !seen_indices.insert(share.participant_index) {
            return Err(ThresholdError::DuplicateParticipant(
                share.participant_index,
            ));
        }

        let participant = config
            .participant(share.participant_index)
            .ok_or(ThresholdError::UnknownParticipant(share.participant_index))?;

        if !verify_decryption_share(participant, ciphertext, share, context) {
            return Err(ThresholdError::InvalidDecryptionProof(
                share.participant_index,
            ));
        }
    }

    verify_combined_plaintext(ciphertext, shares, claimed_amount)
}

/// Verify participant-specific proof contexts, then combine the shares.
///
/// Production contexts include the participant index, so callers should use
/// this form when mirroring the onchain transcript.
pub fn verify_plaintext_with_share_contexts(
    config: &ThresholdPublicConfig,
    ciphertext: &ElGamalCiphertext,
    shares: &[DecryptionShare],
    claimed_amount: u64,
    contexts: &[&[u8]],
) -> Result<(), ThresholdError> {
    if shares.len() < config.threshold {
        return Err(ThresholdError::InsufficientShares);
    }
    if contexts.len() != shares.len() {
        return Err(ThresholdError::ContextCountMismatch);
    }

    let mut seen_indices = BTreeSet::new();
    for (share, context) in shares.iter().zip(contexts) {
        if !seen_indices.insert(share.participant_index) {
            return Err(ThresholdError::DuplicateParticipant(
                share.participant_index,
            ));
        }
        let participant = config
            .participant(share.participant_index)
            .ok_or(ThresholdError::UnknownParticipant(share.participant_index))?;
        if !verify_decryption_share(participant, ciphertext, share, context) {
            return Err(ThresholdError::InvalidDecryptionProof(
                share.participant_index,
            ));
        }
    }

    verify_combined_plaintext(ciphertext, shares, claimed_amount)
}

fn verify_combined_plaintext(
    ciphertext: &ElGamalCiphertext,
    shares: &[DecryptionShare],
    claimed_amount: u64,
) -> Result<(), ThresholdError> {
    let combined_decryption =
        shares
            .iter()
            .fold(RistrettoPoint::identity(), |accumulator, share| {
                accumulator
                    + lagrange_coefficient_at_zero(share.participant_index, shares) * share.point
            });

    let message_point = ciphertext.commitment.get_point() - combined_decryption;
    let claimed_point = Scalar::from(claimed_amount) * G;

    if message_point != claimed_point {
        return Err(ThresholdError::PlaintextMismatch);
    }

    Ok(())
}

fn evaluate_polynomial(coefficients: &[Scalar], x: Scalar) -> Scalar {
    coefficients
        .iter()
        .rev()
        .fold(Scalar::ZERO, |accumulator, coefficient| {
            accumulator * x + coefficient
        })
}

fn evaluate_commitment_polynomial(commitments: &[RistrettoPoint], x: Scalar) -> RistrettoPoint {
    commitments
        .iter()
        .rev()
        .fold(RistrettoPoint::identity(), |accumulator, commitment| {
            accumulator * x + commitment
        })
}

fn lagrange_coefficient_at_zero(participant_index: u64, shares: &[DecryptionShare]) -> Scalar {
    let x_i = Scalar::from(participant_index);
    shares
        .iter()
        .filter(|share| share.participant_index != participant_index)
        .fold(Scalar::ONE, |coefficient, share| {
            let x_j = Scalar::from(share.participant_index);
            coefficient * (-x_j) * (x_i - x_j).invert()
        })
}

#[allow(clippy::too_many_arguments)]
fn prove_equal_discrete_logs(
    witness: &Scalar,
    base_1: &RistrettoPoint,
    result_1: &RistrettoPoint,
    base_2: &RistrettoPoint,
    result_2: &RistrettoPoint,
    domain: &[u8],
    context: &[u8],
) -> DleqProof {
    debug_assert_eq!(witness * base_1, *result_1);
    debug_assert_eq!(witness * base_2, *result_2);

    let nonce = Scalar::random(&mut OsRng);
    let commitment_1 = nonce * base_1;
    let commitment_2 = nonce * base_2;
    let challenge = dleq_challenge(
        domain,
        context,
        base_1,
        result_1,
        base_2,
        result_2,
        &commitment_1,
        &commitment_2,
    );

    DleqProof {
        commitment_1,
        commitment_2,
        response: nonce + challenge * witness,
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_equal_discrete_logs(
    base_1: &RistrettoPoint,
    result_1: &RistrettoPoint,
    base_2: &RistrettoPoint,
    result_2: &RistrettoPoint,
    proof: &DleqProof,
    domain: &[u8],
    context: &[u8],
) -> bool {
    let challenge = dleq_challenge(
        domain,
        context,
        base_1,
        result_1,
        base_2,
        result_2,
        &proof.commitment_1,
        &proof.commitment_2,
    );

    proof.response * base_1 == proof.commitment_1 + challenge * result_1
        && proof.response * base_2 == proof.commitment_2 + challenge * result_2
}

#[allow(clippy::too_many_arguments)]
fn dleq_challenge(
    domain: &[u8],
    context: &[u8],
    base_1: &RistrettoPoint,
    result_1: &RistrettoPoint,
    base_2: &RistrettoPoint,
    result_2: &RistrettoPoint,
    commitment_1: &RistrettoPoint,
    commitment_2: &RistrettoPoint,
) -> Scalar {
    let mut transcript = Sha256::new();
    append_transcript_item(&mut transcript, b"domain", domain);
    append_transcript_item(&mut transcript, b"context", context);
    append_transcript_point(&mut transcript, b"base-1", base_1);
    append_transcript_point(&mut transcript, b"result-1", result_1);
    append_transcript_point(&mut transcript, b"base-2", base_2);
    append_transcript_point(&mut transcript, b"result-2", result_2);
    append_transcript_point(&mut transcript, b"commitment-1", commitment_1);
    append_transcript_point(&mut transcript, b"commitment-2", commitment_2);

    let mut challenge_bytes: [u8; 32] = transcript.finalize().into();
    challenge_bytes[31] &= 0x0f;

    Scalar::from_canonical_bytes(challenge_bytes)
        .into_option()
        .expect("a 252-bit integer is canonical in the Ristretto scalar field")
}

fn append_transcript_point(transcript: &mut Sha256, label: &[u8], point: &RistrettoPoint) {
    append_transcript_item(transcript, label, point.compress().as_bytes());
}

fn append_transcript_item(transcript: &mut Sha256, label: &[u8], value: &[u8]) {
    transcript.update((label.len() as u64).to_le_bytes());
    transcript.update(label);
    transcript.update((value.len() as u64).to_le_bytes());
    transcript.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETUP_CONTEXT: &[u8] = b"beatex/test-program/market-1/key-epoch-1";
    const AUCTION_CONTEXT: &[u8] = b"beatex/test-program/market-1/key-epoch-1/auction-42/buy/0";

    fn aggregate_ciphertext(public_key: &ElGamalPubkey, amounts: &[u64]) -> ElGamalCiphertext {
        let ciphertexts = amounts
            .iter()
            .map(|amount| public_key.encrypt_u64(*amount))
            .collect::<Vec<_>>();

        ciphertexts
            .iter()
            .skip(1)
            .fold(ciphertexts[0], |aggregate, ciphertext| {
                aggregate + ciphertext
            })
    }

    #[test]
    fn every_three_of_seven_subset_decrypts_the_aggregate() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[11, 29, 7]);

        for first in 0..5 {
            for second in (first + 1)..6 {
                for third in (second + 1)..7 {
                    let shares = [first, second, third].map(|index| {
                        create_decryption_share(
                            &setup.secret_shares[index],
                            &ciphertext,
                            AUCTION_CONTEXT,
                        )
                    });

                    assert_eq!(
                        verify_plaintext_with_shares(
                            &setup.public,
                            &ciphertext,
                            &shares,
                            47,
                            AUCTION_CONTEXT,
                        ),
                        Ok(())
                    );
                }
            }
        }
    }

    #[test]
    fn every_three_of_seven_subset_reconstructs_the_registered_key() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);

        for first in 0..5 {
            for second in (first + 1)..6 {
                for third in (second + 1)..7 {
                    let shares = [
                        &setup.secret_shares[first],
                        &setup.secret_shares[second],
                        &setup.secret_shares[third],
                    ];
                    let keypair = reconstruct_elgamal_keypair(&setup.public, &shares).unwrap();
                    assert_eq!(keypair.pubkey(), &setup.public.encryption_pubkey);
                }
            }
        }
    }

    #[test]
    fn reconstruction_rejects_duplicate_and_mismatched_shares() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let duplicate = [
            &setup.secret_shares[0],
            &setup.secret_shares[0],
            &setup.secret_shares[2],
        ];
        assert!(matches!(
            reconstruct_elgamal_keypair(&setup.public, &duplicate),
            Err(ThresholdError::DuplicateParticipant(1))
        ));

        let other_setup = dealer_setup_3_of_7(b"another-key-epoch");
        let mixed = [
            &setup.secret_shares[0],
            &setup.secret_shares[1],
            &other_setup.secret_shares[2],
        ];
        assert!(matches!(
            reconstruct_elgamal_keypair(&setup.public, &mixed),
            Err(ThresholdError::InvalidSecretShare(3))
        ));
    }

    #[test]
    fn setup_and_ciphertexts_use_solana_sdk_types() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        assert!(setup.public.verify(SETUP_CONTEXT));
        assert!(!setup.public.verify(b"a-different-key-epoch"));

        let mut tampered_config = setup.public.clone();
        tampered_config.participant_shares[0].point += G;
        assert!(!tampered_config.verify(SETUP_CONTEXT));

        let ciphertext = setup.public.encryption_pubkey.encrypt_u64(123);
        let bytes = ciphertext.to_bytes();
        assert_eq!(bytes.len(), 64);
        assert_eq!(ElGamalCiphertext::from_bytes(&bytes), Some(ciphertext));
    }

    #[test]
    fn fewer_than_threshold_shares_are_rejected() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[3, 5]);
        let shares = setup.secret_shares[..2]
            .iter()
            .map(|share| create_decryption_share(share, &ciphertext, AUCTION_CONTEXT))
            .collect::<Vec<_>>();

        assert_eq!(
            verify_plaintext_with_shares(&setup.public, &ciphertext, &shares, 8, AUCTION_CONTEXT,),
            Err(ThresholdError::InsufficientShares)
        );
    }

    #[test]
    fn duplicate_participant_is_rejected() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[3, 5]);
        let first = create_decryption_share(&setup.secret_shares[0], &ciphertext, AUCTION_CONTEXT);
        let shares = vec![
            first.clone(),
            first,
            create_decryption_share(&setup.secret_shares[1], &ciphertext, AUCTION_CONTEXT),
        ];

        assert_eq!(
            verify_plaintext_with_shares(&setup.public, &ciphertext, &shares, 8, AUCTION_CONTEXT,),
            Err(ThresholdError::DuplicateParticipant(1))
        );
    }

    #[test]
    fn a_share_is_bound_to_ciphertext_and_context() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let first_ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[3, 5]);
        let second_ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[3, 6]);
        let participant = setup.secret_shares[0].public_share();
        let share =
            create_decryption_share(&setup.secret_shares[0], &first_ciphertext, AUCTION_CONTEXT);

        assert!(verify_decryption_share(
            participant,
            &first_ciphertext,
            &share,
            AUCTION_CONTEXT,
        ));
        assert!(!verify_decryption_share(
            participant,
            &second_ciphertext,
            &share,
            AUCTION_CONTEXT,
        ));
        assert!(!verify_decryption_share(
            participant,
            &first_ciphertext,
            &share,
            b"another-auction",
        ));
    }

    #[test]
    fn corrupted_share_proof_is_rejected() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[3, 5]);
        let mut share =
            create_decryption_share(&setup.secret_shares[0], &ciphertext, AUCTION_CONTEXT);
        share.proof.response += Scalar::ONE;

        assert!(!verify_decryption_share(
            setup.secret_shares[0].public_share(),
            &ciphertext,
            &share,
            AUCTION_CONTEXT,
        ));
    }

    #[test]
    fn incorrect_plaintext_is_rejected() {
        let setup = dealer_setup_3_of_7(SETUP_CONTEXT);
        let ciphertext = aggregate_ciphertext(&setup.public.encryption_pubkey, &[11, 29, 7]);
        let shares = setup.secret_shares[..3]
            .iter()
            .map(|share| create_decryption_share(share, &ciphertext, AUCTION_CONTEXT))
            .collect::<Vec<_>>();

        assert_eq!(
            verify_plaintext_with_shares(&setup.public, &ciphertext, &shares, 48, AUCTION_CONTEXT,),
            Err(ThresholdError::PlaintextMismatch)
        );
    }
}

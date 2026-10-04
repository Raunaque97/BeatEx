//! Canonical public-input hashing for the queued-order Groth16 circuits.

use {
    crate::{
        queue::{EncryptedOrder, EncryptedValue, QUEUED_CIPHERTEXT_COUNT},
        BeatExError,
    },
    anchor_lang::prelude::*,
    solana_poseidon::{hashv, Endianness, Parameters},
};

const FRAME_PAYLOAD: usize = 11;
const ZERO: [u8; 32] = [0; 32];
const ONE: [u8; 32] = {
    let mut value = [0; 32];
    value[0] = 1;
    value
};
const TWO: [u8; 32] = {
    let mut value = [0; 32];
    value[0] = 2;
    value
};

// `Fq::from_le_bytes_mod_order(domain)` for the BN254 scalar field used by
// ark-ed-on-bn254. Keeping these reduced constants explicit means the SBF
// program never needs bigint arithmetic.
const ORDER_SUBMISSION_DOMAIN: [u8; 32] = [
    61, 100, 97, 196, 113, 24, 193, 59, 91, 161, 101, 75, 128, 78, 131, 117, 106, 78, 147, 23, 97,
    98, 139, 131, 224, 40, 219, 209, 79, 115, 220, 31,
];
const ORDER_CIPHERTEXT_DOMAIN: [u8; 32] = [
    154, 95, 96, 244, 127, 123, 119, 133, 235, 107, 255, 115, 186, 204, 140, 36, 69, 110, 121, 68,
    149, 151, 175, 156, 26, 206, 152, 4, 125, 217, 164, 26,
];
const FOLD_DOMAIN: [u8; 32] = [
    66, 101, 97, 116, 69, 120, 32, 113, 117, 101, 117, 101, 100, 32, 97, 103, 103, 114, 101, 103,
    97, 116, 101, 32, 102, 111, 108, 100, 32, 118, 49, 0,
];
const BALANCE_DEPOSIT_DOMAIN: [u8; 32] = [
    66, 101, 97, 116, 69, 120, 32, 98, 97, 108, 97, 110, 99, 101, 32, 100, 101, 112, 111, 115, 105,
    116, 32, 118, 49, 0, 0, 0, 0, 0, 0, 0,
];
const AGGREGATE_PUBLICATION_DOMAIN: [u8; 32] = [
    127, 140, 212, 251, 15, 85, 102, 242, 234, 78, 218, 28, 27, 68, 89, 72, 136, 155, 32, 56, 121,
    130, 126, 98, 114, 162, 223, 31, 24, 175, 167, 8,
];

pub const FOLD_BATCH_SIZE: usize = 4;

pub struct OrderStatement<'a> {
    pub auction: &'a Pubkey,
    pub owner: &'a Pubkey,
    pub client_order_nonce: u64,
    /// Little-endian BN254 field element produced by the private-order
    /// commitment transcript.
    pub order_commitment: [u8; 32],
    pub committee_key: [u8; 32],
    pub reference_price: u64,
    pub tick_size: u64,
    pub ciphertexts: &'a [u8],
    pub old_base: &'a EncryptedValue,
    pub old_quote: &'a EncryptedValue,
    pub new_base: &'a EncryptedValue,
    pub new_quote: &'a EncryptedValue,
}

pub struct FoldOrderInput {
    pub order: Pubkey,
    pub ciphertext_digest: [u8; 32],
}

pub struct FoldStatement<'a> {
    pub auction: &'a Pubkey,
    pub old_folded_count: u64,
    pub active_orders: &'a [FoldOrderInput],
    pub old_aggregate: &'a [u8],
    pub new_aggregate: &'a [u8],
}

/// Public statement for atomically crediting public Token-2022 deposits to a
/// Baby Jubjub encrypted balance. The corresponding circuit proves that each
/// new plaintext balance equals its old value plus the public deposit amount,
/// using the same committee key for all four ciphertexts.
pub struct BalanceDepositStatement<'a> {
    pub market: &'a Pubkey,
    pub owner: &'a Pubkey,
    pub key_epoch: &'a Pubkey,
    pub committee_key: [u8; 32],
    pub old_revision: u64,
    pub base_amount: u64,
    pub quote_amount: u64,
    pub old_base: &'a EncryptedValue,
    pub old_quote: &'a EncryptedValue,
    pub new_base: &'a EncryptedValue,
    pub new_quote: &'a EncryptedValue,
}

/// Compact public statement for publishing the decrypted aggregate. The
/// committee's decryption shares and Chaum-Pedersen transcripts are private
/// circuit witnesses and therefore never enter the Solana transaction.
pub struct AggregatePublicationStatement<'a> {
    pub auction: &'a Pubkey,
    pub accepted_order_count: u64,
    pub committee_key: [u8; 32],
    pub participant_ids: [u8; 3],
    /// Public keys for the three selected participants, ordered exactly like
    /// `participant_ids`. Decryption shares remain private circuit witnesses.
    pub selected_participant_public_shares: [[u8; 32]; 3],
    pub aggregate: &'a [u8],
    pub demand: &'a [u64; 17],
    pub supply: &'a [u64; 17],
}

pub fn order_statement_digest(statement: &OrderStatement<'_>) -> Result<[u8; 32]> {
    let mut payload = Vec::with_capacity(150);
    payload.push(ORDER_SUBMISSION_DOMAIN);
    payload.push(ONE);
    payload.extend(pubkey_fields(statement.auction));
    payload.extend(pubkey_fields(statement.owner));
    payload.push(u64_field(statement.client_order_nonce));
    validate_field(&statement.order_commitment)?;
    payload.push(statement.order_commitment);
    append_point(&mut payload, &statement.committee_key)?;
    payload.push(u64_field(statement.reference_price));
    payload.push(u64_field(statement.tick_size));
    append_order(&mut payload, statement.ciphertexts)?;
    for ciphertext in [
        statement.old_base,
        statement.old_quote,
        statement.new_base,
        statement.new_quote,
    ] {
        append_ciphertext(&mut payload, ciphertext)?;
    }
    transcript_hash(&payload)
}

pub fn order_ciphertext_digest(
    auction: &Pubkey,
    order: &Pubkey,
    ciphertexts: &[u8],
) -> Result<[u8; 32]> {
    let mut payload = Vec::with_capacity(5 + QUEUED_CIPHERTEXT_COUNT * 4);
    payload.push(ORDER_CIPHERTEXT_DOMAIN);
    payload.extend(pubkey_fields(auction));
    payload.extend(pubkey_fields(order));
    append_order(&mut payload, ciphertexts)?;
    transcript_hash(&payload)
}

pub fn fold_statement_digest(statement: &FoldStatement<'_>) -> Result<[u8; 32]> {
    require!(
        !statement.active_orders.is_empty() && statement.active_orders.len() <= FOLD_BATCH_SIZE,
        BeatExError::InvalidFoldBatch
    );
    let active_count = statement.active_orders.len() as u64;
    let new_count = statement
        .old_folded_count
        .checked_add(active_count)
        .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
    let mut payload = Vec::with_capacity(300);
    payload.push(FOLD_DOMAIN);
    payload.push(ONE);
    payload.extend(pubkey_fields(statement.auction));
    payload.push(u64_field(statement.old_folded_count));
    payload.push(u64_field(new_count));
    payload.push(u64_field(active_count));

    for slot in 0..FOLD_BATCH_SIZE {
        if let Some(order) = statement.active_orders.get(slot) {
            validate_field(&order.ciphertext_digest)?;
            payload.extend(pubkey_fields(&order.order));
            payload.push(order.ciphertext_digest);
        } else {
            payload.push(ZERO);
            payload.push(ZERO);
            payload.push(inactive_order_digest(statement.auction)?);
        }
    }

    require!(
        statement.old_aggregate.len() == QUEUED_CIPHERTEXT_COUNT * 64
            && statement.new_aggregate.len() == QUEUED_CIPHERTEXT_COUNT * 64,
        BeatExError::UnsupportedQueueGrid
    );
    for (old, new) in statement
        .old_aggregate
        .chunks_exact(64)
        .zip(statement.new_aggregate.chunks_exact(64))
    {
        let old: &[u8; 64] = old
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        let new: &[u8; 64] = new
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        append_ciphertext(&mut payload, old)?;
        append_ciphertext(&mut payload, new)?;
    }
    transcript_hash(&payload)
}

pub fn balance_deposit_statement_digest(
    statement: &BalanceDepositStatement<'_>,
) -> Result<[u8; 32]> {
    let new_revision = statement
        .old_revision
        .checked_add(1)
        .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
    let mut payload = Vec::with_capacity(32);
    payload.push(BALANCE_DEPOSIT_DOMAIN);
    payload.push(ONE);
    payload.extend(pubkey_fields(statement.market));
    payload.extend(pubkey_fields(statement.owner));
    payload.extend(pubkey_fields(statement.key_epoch));
    append_point(&mut payload, &statement.committee_key)?;
    payload.push(u64_field(statement.old_revision));
    payload.push(u64_field(new_revision));
    payload.push(u64_field(statement.base_amount));
    payload.push(u64_field(statement.quote_amount));
    for ciphertext in [
        statement.old_base,
        statement.old_quote,
        statement.new_base,
        statement.new_quote,
    ] {
        append_ciphertext(&mut payload, ciphertext)?;
    }
    transcript_hash(&payload)
}

pub fn aggregate_publication_statement_digest(
    statement: &AggregatePublicationStatement<'_>,
) -> Result<[u8; 32]> {
    require!(
        statement.participant_ids[0] < statement.participant_ids[1]
            && statement.participant_ids[1] < statement.participant_ids[2]
            && statement.participant_ids[0] >= 1
            && statement.participant_ids[2] <= 7,
        BeatExError::InvalidCommittee
    );
    require!(
        statement.accepted_order_count > 0,
        BeatExError::AggregationIncomplete
    );

    // Stream the statement through the 11-field Poseidon framing. The exact
    // order is domain, version, auction, count, committee key, participant
    // IDs, selected public keys, then each aggregate ciphertext and plaintext.
    let mut transcript = TranscriptBuilder::new();
    transcript.push(AGGREGATE_PUBLICATION_DOMAIN)?;
    transcript.push(TWO)?;
    for field in pubkey_fields(statement.auction) {
        transcript.push(field)?;
    }
    transcript.push(u64_field(statement.accepted_order_count))?;
    transcript.push_point(&statement.committee_key)?;
    for participant_id in statement.participant_ids {
        transcript.push(u64_field(u64::from(participant_id)))?;
    }
    for public_share in &statement.selected_participant_public_shares {
        transcript.push_point(public_share)?;
    }
    require!(
        statement.aggregate.len() == QUEUED_CIPHERTEXT_COUNT * 64,
        BeatExError::UnsupportedQueueGrid
    );
    for (index, chunk) in statement.aggregate.chunks_exact(64).enumerate() {
        let ciphertext: &[u8; 64] = chunk
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        transcript.push_ciphertext(ciphertext)?;
        let level = index / 2;
        let plaintext = if index % 2 == 0 {
            statement.demand[level]
        } else {
            statement.supply[level]
        };
        transcript.push(u64_field(plaintext))?;
    }
    transcript.finish()
}

fn inactive_order_digest(auction: &Pubkey) -> Result<[u8; 32]> {
    let identity = order_bytes(&identity_order());
    let mut payload = Vec::with_capacity(5 + QUEUED_CIPHERTEXT_COUNT * 4);
    payload.push(ORDER_CIPHERTEXT_DOMAIN);
    payload.extend(pubkey_fields(auction));
    payload.push(ZERO);
    payload.push(ZERO);
    append_order(&mut payload, &identity)?;
    transcript_hash(&payload)
}

pub fn identity_order() -> EncryptedOrder {
    let mut point = [0u8; 32];
    // Twisted Edwards identity is (0, 1); the wire format stores y and x parity.
    point[0] = 1;
    let mut ciphertext = [0u8; 64];
    ciphertext[..32].copy_from_slice(&point);
    ciphertext[32..].copy_from_slice(&point);
    [ciphertext; QUEUED_CIPHERTEXT_COUNT]
}

pub fn order_bytes(order: &EncryptedOrder) -> Vec<u8> {
    order
        .iter()
        .flat_map(|ciphertext| ciphertext.iter().copied())
        .collect()
}

pub fn identity_ciphertext() -> EncryptedValue {
    identity_order()[0]
}

pub fn validate_committee_key(compressed: &[u8; 32]) -> Result<()> {
    let mut identity = [0u8; 32];
    identity[0] = 1;
    require!(
        *compressed != identity,
        BeatExError::InvalidBabyJubjubEncoding
    );
    let mut payload = Vec::with_capacity(2);
    append_point(&mut payload, compressed)
}

/// Validate the canonical wire encoding of a Baby Jubjub point. Unlike a key,
/// a decryption share may be the identity when its aggregate ephemeral is the
/// identity, so this intentionally performs no non-identity check.
pub fn validate_babyjub_point(compressed: &[u8; 32]) -> Result<()> {
    let mut payload = Vec::with_capacity(2);
    append_point(&mut payload, compressed)
}

fn append_order(payload: &mut Vec<[u8; 32]>, order: &[u8]) -> Result<()> {
    require!(
        order.len() == QUEUED_CIPHERTEXT_COUNT * 64,
        BeatExError::UnsupportedQueueGrid
    );
    for chunk in order.chunks_exact(64) {
        let ciphertext: &[u8; 64] = chunk
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        append_ciphertext(payload, ciphertext)?;
    }
    Ok(())
}

fn append_ciphertext(payload: &mut Vec<[u8; 32]>, ciphertext: &EncryptedValue) -> Result<()> {
    let first: &[u8; 32] = ciphertext[..32]
        .try_into()
        .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
    let second: &[u8; 32] = ciphertext[32..]
        .try_into()
        .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
    append_point(payload, first)?;
    append_point(payload, second)
}

fn append_point(payload: &mut Vec<[u8; 32]>, compressed: &[u8; 32]) -> Result<()> {
    let (y, sign) = point_fields(compressed)?;
    payload.push(y);
    payload.push(sign);
    Ok(())
}

fn point_fields(compressed: &[u8; 32]) -> Result<([u8; 32], [u8; 32])> {
    // Canonical custom encoding: y occupies bits 0..253, bit 254 is zero,
    // and bit 255 stores parity(x).
    require!(
        compressed[31] & 0x40 == 0,
        BeatExError::InvalidBabyJubjubEncoding
    );
    let mut y = *compressed;
    let sign = y[31] >> 7;
    y[31] &= 0x7f;
    validate_field(&y)?;
    Ok((y, u64_field(sign.into())))
}

fn pubkey_fields(key: &Pubkey) -> [[u8; 32]; 2] {
    let bytes = key.to_bytes();
    let mut low = [0u8; 32];
    let mut high = [0u8; 32];
    low[..16].copy_from_slice(&bytes[..16]);
    high[..16].copy_from_slice(&bytes[16..]);
    [low, high]
}

fn u64_field(value: u64) -> [u8; 32] {
    let mut field = [0u8; 32];
    field[..8].copy_from_slice(&value.to_le_bytes());
    field
}

fn validate_field(value: &[u8; 32]) -> Result<()> {
    // Asking the syscall to hash the value is needlessly expensive. This is
    // the little-endian BN254 scalar modulus used by Solana Poseidon.
    const MODULUS: [u8; 32] = [
        1, 0, 0, 240, 147, 245, 225, 67, 145, 112, 185, 121, 72, 232, 51, 40, 93, 88, 129, 129,
        182, 69, 80, 184, 41, 160, 49, 225, 114, 78, 100, 48,
    ];
    require!(
        value.iter().rev().cmp(MODULUS.iter().rev()).is_lt(),
        BeatExError::InvalidBabyJubjubEncoding
    );
    Ok(())
}

fn transcript_hash(payload: &[[u8; 32]]) -> Result<[u8; 32]> {
    let mut digest = ZERO;
    for chunk in payload.chunks(FRAME_PAYLOAD) {
        let mut inputs = [[0u8; 32]; 12];
        inputs[0] = digest;
        inputs[1..1 + chunk.len()].copy_from_slice(chunk);
        let refs = inputs
            .iter()
            .map(|input| input.as_slice())
            .collect::<Vec<_>>();
        digest = hashv(Parameters::Bn254X5, Endianness::LittleEndian, &refs)
            .map_err(|_| error!(BeatExError::PoseidonHashFailure))?
            .to_bytes();
    }
    Ok(digest)
}

struct TranscriptBuilder {
    digest: [u8; 32],
    chunk: [[u8; 32]; FRAME_PAYLOAD],
    len: usize,
}

impl TranscriptBuilder {
    const fn new() -> Self {
        Self {
            digest: ZERO,
            chunk: [ZERO; FRAME_PAYLOAD],
            len: 0,
        }
    }

    fn push(&mut self, field: [u8; 32]) -> Result<()> {
        self.chunk[self.len] = field;
        self.len += 1;
        if self.len == FRAME_PAYLOAD {
            self.flush()?;
        }
        Ok(())
    }

    fn push_point(&mut self, compressed: &[u8; 32]) -> Result<()> {
        let (y, sign) = point_fields(compressed)?;
        self.push(y)?;
        self.push(sign)
    }

    fn push_ciphertext(&mut self, ciphertext: &EncryptedValue) -> Result<()> {
        let first: &[u8; 32] = ciphertext[..32]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        let second: &[u8; 32] = ciphertext[32..]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidBabyJubjubEncoding))?;
        self.push_point(first)?;
        self.push_point(second)
    }

    fn flush(&mut self) -> Result<()> {
        let mut inputs = [[0u8; 32]; 12];
        inputs[0] = self.digest;
        inputs[1..1 + self.len].copy_from_slice(&self.chunk[..self.len]);
        let refs: [&[u8]; 12] = std::array::from_fn(|index| inputs[index].as_slice());
        self.digest = hashv(Parameters::Bn254X5, Endianness::LittleEndian, &refs)
            .map_err(|_| error!(BeatExError::PoseidonHashFailure))?
            .to_bytes();
        self.chunk = [ZERO; FRAME_PAYLOAD];
        self.len = 0;
        Ok(())
    }

    fn finish(mut self) -> Result<[u8; 32]> {
        if self.len > 0 {
            self.flush()?;
        }
        Ok(self.digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{groth16, queue::QueueCircuitKind, queue::QueueVerificationKey};

    #[test]
    fn identity_has_canonical_wire_encoding() {
        let order = identity_order();
        assert!(order.iter().all(|ciphertext| {
            ciphertext[0] == 1
                && ciphertext[1..32].iter().all(|byte| *byte == 0)
                && ciphertext[32] == 1
                && ciphertext[33..].iter().all(|byte| *byte == 0)
        }));
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(validate_committee_key(&identity).is_err());
    }

    #[test]
    fn transcript_is_order_sensitive_for_fixed_schema() {
        let a = transcript_hash(&[ONE, ZERO]).unwrap();
        let b = transcript_hash(&[ZERO, ONE]).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn deposit_statement_binds_amount_owner_epoch_and_revision() {
        let market = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let key_epoch = Pubkey::new_unique();
        let ciphertext = identity_ciphertext();
        let mut committee_key = [0u8; 32];
        committee_key[0] = 2;
        let digest = |owner: &Pubkey, epoch: &Pubkey, revision, base_amount| {
            balance_deposit_statement_digest(&BalanceDepositStatement {
                market: &market,
                owner,
                key_epoch: epoch,
                committee_key,
                old_revision: revision,
                base_amount,
                quote_amount: 5,
                old_base: &ciphertext,
                old_quote: &ciphertext,
                new_base: &ciphertext,
                new_quote: &ciphertext,
            })
            .unwrap()
        };
        let expected = digest(&owner, &key_epoch, 7, 11);
        assert_ne!(expected, digest(&Pubkey::new_unique(), &key_epoch, 7, 11));
        assert_ne!(expected, digest(&owner, &Pubkey::new_unique(), 7, 11));
        assert_ne!(expected, digest(&owner, &key_epoch, 8, 11));
        assert_ne!(expected, digest(&owner, &key_epoch, 7, 12));
    }

    #[test]
    fn publication_statement_binds_curves_quorum_and_accumulator() {
        let auction = Pubkey::new_unique();
        let mut aggregate = order_bytes(&identity_order());
        let mut committee_key = [0u8; 32];
        committee_key[0] = 2;
        let selected_public_shares = [committee_key; 3];
        let mut demand = [0u64; 17];
        let supply = [0u64; 17];
        let digest = |aggregate: &[u8], ids, demand: &[u64; 17], shares| {
            aggregate_publication_statement_digest(&AggregatePublicationStatement {
                auction: &auction,
                accepted_order_count: 2,
                committee_key,
                participant_ids: ids,
                selected_participant_public_shares: shares,
                aggregate,
                demand,
                supply: &supply,
            })
            .unwrap()
        };
        let expected = digest(&aggregate, [1, 4, 7], &demand, selected_public_shares);
        demand[3] = 1;
        assert_ne!(
            expected,
            digest(&aggregate, [1, 4, 7], &demand, selected_public_shares)
        );
        assert_ne!(
            expected,
            digest(&aggregate, [1, 3, 7], &[0; 17], selected_public_shares)
        );
        let mut changed_shares = selected_public_shares;
        changed_shares[0][0] = 3;
        assert_ne!(
            expected,
            digest(&aggregate, [1, 4, 7], &[0; 17], changed_shares)
        );
        aggregate[0] = 2;
        assert_ne!(
            expected,
            digest(&aggregate, [1, 4, 7], &[0; 17], selected_public_shares,)
        );
        assert!(
            aggregate_publication_statement_digest(&AggregatePublicationStatement {
                auction: &auction,
                accepted_order_count: 2,
                committee_key,
                participant_ids: [1, 1, 7],
                selected_participant_public_shares: selected_public_shares,
                aggregate: &aggregate,
                demand: &[0; 17],
                supply: &supply,
            })
            .is_err()
        );
        assert!(
            aggregate_publication_statement_digest(&AggregatePublicationStatement {
                auction: &auction,
                accepted_order_count: 2,
                committee_key,
                participant_ids: [0, 4, 7],
                selected_participant_public_shares: selected_public_shares,
                aggregate: &aggregate,
                demand: &[0; 17],
                supply: &supply,
            })
            .is_err()
        );
    }

    #[test]
    #[ignore = "generates a deterministic deposit Groth16 setup"]
    fn deposit_statement_matches_and_verifies_exact_circuit_fixture() {
        let fixture =
            beatex_babyjubjub_groth16_spike::deposit::deterministic_solana_fixture().unwrap();
        let digest = balance_deposit_statement_digest(&BalanceDepositStatement {
            market: &Pubkey::new_from_array(fixture.market),
            owner: &Pubkey::new_from_array(fixture.owner),
            key_epoch: &Pubkey::new_from_array(fixture.key_epoch),
            committee_key: fixture.committee_key,
            old_revision: fixture.old_revision,
            base_amount: fixture.base_amount,
            quote_amount: fixture.quote_amount,
            old_base: &fixture.old_base,
            old_quote: &fixture.old_quote,
            new_base: &fixture.new_base,
            new_quote: &fixture.new_quote,
        })
        .unwrap();
        let mut expected_le = fixture.public_input;
        expected_le.reverse();
        assert_eq!(digest, expected_le);
        let key = QueueVerificationKey {
            version: 1,
            market: Pubkey::new_unique(),
            circuit_kind: QueueCircuitKind::BalanceDeposit,
            circuit_version: 1,
            alpha_g1: fixture.vk_alpha_g1,
            beta_g2: fixture.vk_beta_g2,
            gamma_g2: fixture.vk_gamma_g2,
            delta_g2: fixture.vk_delta_g2,
            ic: fixture.vk_ic,
            bump: 0,
        };
        let proof = groth16::Groth16ProofInput {
            proof_a: fixture.proof_a,
            proof_b: fixture.proof_b,
            proof_c: fixture.proof_c,
            public_input: fixture.public_input,
        };
        groth16::verify_queue_proof(&proof, &digest, &key).unwrap();
    }

    #[test]
    #[ignore = "generates a deterministic aggregate-publication Groth16 setup"]
    fn publication_statement_matches_and_verifies_exact_circuit_fixture() {
        let fixture =
            beatex_babyjubjub_groth16_spike::decryption::deterministic_solana_fixture().unwrap();
        let aggregate = order_bytes(&fixture.aggregate);
        let digest = aggregate_publication_statement_digest(&AggregatePublicationStatement {
            auction: &Pubkey::new_from_array(fixture.auction),
            accepted_order_count: fixture.accepted_order_count,
            committee_key: fixture.committee_key,
            participant_ids: fixture.participant_ids,
            selected_participant_public_shares: fixture.participant_ids.map(|participant_id| {
                fixture.participant_public_shares[usize::from(participant_id - 1)]
            }),
            aggregate: &aggregate,
            demand: &fixture.demand,
            supply: &fixture.supply,
        })
        .unwrap();
        let mut expected_le = fixture.public_input;
        expected_le.reverse();
        assert_eq!(digest, expected_le);
        let key = QueueVerificationKey {
            version: 1,
            market: Pubkey::new_unique(),
            circuit_kind: QueueCircuitKind::AggregatePublication,
            circuit_version: crate::queue::AGGREGATE_PUBLICATION_CIRCUIT_VERSION,
            alpha_g1: fixture.vk_alpha_g1,
            beta_g2: fixture.vk_beta_g2,
            gamma_g2: fixture.vk_gamma_g2,
            delta_g2: fixture.vk_delta_g2,
            ic: fixture.vk_ic,
            bump: 0,
        };
        let proof = groth16::Groth16ProofInput {
            proof_a: fixture.proof_a,
            proof_b: fixture.proof_b,
            proof_c: fixture.proof_c,
            public_input: fixture.public_input,
        };
        groth16::verify_queue_proof(&proof, &digest, &key).unwrap();
    }

    #[test]
    #[ignore = "generates a deterministic 234k-constraint Groth16 setup"]
    fn order_statement_matches_and_verifies_exact_circuit_fixture() {
        let fixture =
            beatex_babyjubjub_groth16_spike::transition::deterministic_solana_fixture().unwrap();
        let auction = Pubkey::new_from_array(fixture.auction);
        let owner = Pubkey::new_from_array(fixture.owner);
        let ciphertexts = order_bytes(&fixture.ciphertexts);
        let digest = order_statement_digest(&OrderStatement {
            auction: &auction,
            owner: &owner,
            client_order_nonce: fixture.client_order_nonce,
            order_commitment: fixture.order_commitment,
            committee_key: fixture.committee_key,
            reference_price: fixture.reference_price,
            tick_size: fixture.tick_size,
            ciphertexts: &ciphertexts,
            old_base: &fixture.old_base,
            old_quote: &fixture.old_quote,
            new_base: &fixture.new_base,
            new_quote: &fixture.new_quote,
        })
        .unwrap();
        let mut expected_le = fixture.public_input;
        expected_le.reverse();
        assert_eq!(digest, expected_le);

        let key = QueueVerificationKey {
            version: 1,
            market: Pubkey::new_unique(),
            circuit_kind: QueueCircuitKind::OrderSubmission,
            circuit_version: 1,
            alpha_g1: fixture.vk_alpha_g1,
            beta_g2: fixture.vk_beta_g2,
            gamma_g2: fixture.vk_gamma_g2,
            delta_g2: fixture.vk_delta_g2,
            ic: fixture.vk_ic,
            bump: 0,
        };
        let proof = groth16::Groth16ProofInput {
            proof_a: fixture.proof_a,
            proof_b: fixture.proof_b,
            proof_c: fixture.proof_c,
            public_input: fixture.public_input,
        };
        groth16::verify_queue_proof(&proof, &digest, &key).unwrap();
    }

    #[test]
    #[ignore = "generates a deterministic 311k-constraint Groth16 setup"]
    fn fold_statement_matches_and_verifies_exact_circuit_fixture() {
        let fixture =
            beatex_babyjubjub_groth16_spike::transition::deterministic_fold_solana_fixture()
                .unwrap();
        let auction = Pubkey::new_from_array(fixture.auction);
        let active_orders = (0..3)
            .map(|index| FoldOrderInput {
                order: Pubkey::new_from_array(fixture.order_keys[index]),
                ciphertext_digest: fixture.order_digests[index],
            })
            .collect::<Vec<_>>();
        let old_aggregate = order_bytes(&fixture.old_aggregate);
        let new_aggregate = order_bytes(&fixture.new_aggregate);
        let digest = fold_statement_digest(&FoldStatement {
            auction: &auction,
            old_folded_count: fixture.old_folded_count,
            active_orders: &active_orders,
            old_aggregate: &old_aggregate,
            new_aggregate: &new_aggregate,
        })
        .unwrap();
        let mut expected_le = fixture.public_input;
        expected_le.reverse();
        assert_eq!(digest, expected_le);

        let key = QueueVerificationKey {
            version: 1,
            market: Pubkey::new_unique(),
            circuit_kind: QueueCircuitKind::AggregateFold,
            circuit_version: 1,
            alpha_g1: fixture.vk_alpha_g1,
            beta_g2: fixture.vk_beta_g2,
            gamma_g2: fixture.vk_gamma_g2,
            delta_g2: fixture.vk_delta_g2,
            ic: fixture.vk_ic,
            bump: 0,
        };
        let proof = groth16::Groth16ProofInput {
            proof_a: fixture.proof_a,
            proof_b: fixture.proof_b,
            proof_c: fixture.proof_c,
            public_input: fixture.public_input,
        };
        groth16::verify_queue_proof(&proof, &digest, &key).unwrap();
    }
}

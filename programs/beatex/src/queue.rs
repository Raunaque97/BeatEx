//! State and pure invariants for the encrypted order queue.
//!
//! Order submission creates an independent [`QueuedOrder`] account and only
//! contends on the lightweight [`SubmissionCounter`]. Permissionless fold
//! transactions consume accepted orders into one [`AggregateAccumulator`]. Curve
//! arithmetic and proof verification deliberately live outside this module;
//! the helpers here define the state transitions that instruction handlers
//! must enforce atomically with proof verification and account writes.

use anchor_lang::prelude::*;

/// The fixed auction grid has seventeen price levels.
pub const QUEUE_PRICE_LEVELS: usize = 17;
/// Each price level contributes one buy and one sell ciphertext.
pub const QUEUED_CIPHERTEXT_COUNT: usize = QUEUE_PRICE_LEVELS * 2;
/// A compressed Baby Jubjub ElGamal ciphertext is two 32-byte points.
pub const ELGAMAL_CIPHERTEXT_BYTES: usize = 64;
/// Digest size used to bind ciphertext account data to Groth16 public inputs.
pub const QUEUE_DIGEST_BYTES: usize = 32;
pub const ORDER_SUBMISSION_CIRCUIT_VERSION: u16 = 2;
pub const BALANCE_DEPOSIT_CIRCUIT_VERSION: u16 = 1;
pub const AGGREGATE_FOLD_CIRCUIT_VERSION: u16 = 2;
pub const ORDER_MODIFICATION_CIRCUIT_VERSION: u16 = 2;
pub const ROLLOVER_SETTLEMENT_CIRCUIT_VERSION: u16 = 2;
pub const PRIVATE_ORDER_QUANTITY_BITS: u32 = 28;
pub const MAX_PRIVATE_ORDER_QUANTITY: u64 = (1_u64 << PRIVATE_ORDER_QUANTITY_BITS) - 1;
/// Version 2 replaces per-entry threshold interpolation with one
/// transcript-randomized batch equation while preserving full curve output.
pub const AGGREGATE_PUBLICATION_CIRCUIT_VERSION: u16 = 2;
/// First private-claim circuit version. Its arithmetic and transcript are
/// intentionally versioned independently from order submission.
pub const PRIVATE_CLAIM_CIRCUIT_VERSION: u16 = 2;
/// First cancellation-refund circuit version. A distinct proof kind prevents
/// a refund proof from being replayed through the settled-auction claim path.
pub const CANCELLATION_REFUND_CIRCUIT_VERSION: u16 = 2;

pub type EncryptedValue = [u8; ELGAMAL_CIPHERTEXT_BYTES];
pub type EncryptedOrder = [EncryptedValue; QUEUED_CIPHERTEXT_COUNT];
pub type QueueDigest = [u8; QUEUE_DIGEST_BYTES];

/// Queue-specific order state. There is no transition back to `Accepted`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum QueuedOrderState {
    Accepted = 0,
    Folded = 1,
}

/// Settlement state is independent from aggregate-fold state. In particular,
/// a cancelled auction must be able to refund an accepted order that was never
/// folded before the aggregation timeout.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum WithdrawalState {
    Pending = 0,
    Claimed = 1,
    Refunded = 2,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum WithdrawalKind {
    Claim = 0,
    Refund = 1,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum QueueCircuitKind {
    OrderSubmission = 0,
    AggregateFold = 1,
    /// Credits public Token-2022 deposits into the trader's encrypted balance.
    BalanceDeposit = 2,
    /// Publishes the plaintext aggregate curves after threshold decryption.
    ///
    AggregatePublication = 3,
    /// Proves one private allocation and binds its confidential payouts.
    PrivateClaim = 4,
    /// Proves return of the original reserves after auction cancellation.
    ///
    /// Explicit discriminants are part of the verification-key PDA namespace.
    /// Existing variants must never be renumbered; new variants append here.
    CancellationRefund = 5,
    /// Replaces one active order or cancels it back to the encrypted balance.
    OrderModification = 6,
    /// Credits a settled private payout directly into the encrypted balance.
    RolloverSettlement = 7,
}

pub const fn circuit_version(kind: QueueCircuitKind) -> u16 {
    match kind {
        QueueCircuitKind::AggregatePublication => AGGREGATE_PUBLICATION_CIRCUIT_VERSION,
        QueueCircuitKind::PrivateClaim => PRIVATE_CLAIM_CIRCUIT_VERSION,
        QueueCircuitKind::CancellationRefund => CANCELLATION_REFUND_CIRCUIT_VERSION,
        QueueCircuitKind::AggregateFold => AGGREGATE_FOLD_CIRCUIT_VERSION,
        QueueCircuitKind::OrderModification => ORDER_MODIFICATION_CIRCUIT_VERSION,
        QueueCircuitKind::RolloverSettlement => ROLLOVER_SETTLEMENT_CIRCUIT_VERSION,
        QueueCircuitKind::OrderSubmission => ORDER_SUBMISSION_CIRCUIT_VERSION,
        QueueCircuitKind::BalanceDeposit => BALANCE_DEPOSIT_CIRCUIT_VERSION,
    }
}

/// Authority-installed circuit-specific Groth16 verification key. The PDA
/// seed fixes the circuit kind and version, so order submitters cannot choose
/// a weaker key in their transaction.
#[account]
#[derive(InitSpace)]
pub struct QueueVerificationKey {
    pub version: u8,
    pub market: Pubkey,
    pub circuit_kind: QueueCircuitKind,
    pub circuit_version: u16,
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: [[u8; 64]; 2],
    pub bump: u8,
}

/// Immutable queue deadlines fixed when the auction is opened.
///
/// Both ranges are half-open: submission is valid before
/// `collection_end_slot`, and folding is valid before
/// `aggregation_deadline_slot`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
pub struct QueueDeadlines {
    pub collection_end_slot: u64,
    pub aggregation_deadline_slot: u64,
}

impl QueueDeadlines {
    pub const fn is_valid(self) -> bool {
        self.collection_end_slot < self.aggregation_deadline_slot
    }

    pub const fn collection_is_open(self, current_slot: u64) -> bool {
        current_slot < self.collection_end_slot
    }

    pub const fn aggregation_is_open(self, current_slot: u64) -> bool {
        current_slot < self.aggregation_deadline_slot
    }

    pub const fn aggregation_timed_out(self, current_slot: u64) -> bool {
        current_slot >= self.aggregation_deadline_slot
    }
}

/// One independently writable encrypted order inbox entry.
///
/// `sequence` is the pre-increment value of the auction's accepted counter.
/// It gives every accepted order a unique position without requiring a shared
/// ciphertext accumulator in the trader transaction.
#[account]
#[derive(InitSpace)]
pub struct QueuedOrder {
    pub version: u8,
    pub market: Pubkey,
    pub auction: Pubkey,
    pub owner: Pubkey,
    pub funder: Pubkey,
    pub client_order_nonce: u64,
    pub sequence: u64,
    pub state: QueuedOrderState,
    pub withdrawal_state: WithdrawalState,
    /// Circuit-derived terminal nullifier. It remains zero while pending and is
    /// written atomically with the eventual claim or refund token movement.
    pub terminal_nullifier: [u8; QUEUE_DIGEST_BYTES],
    /// Immutable event whose ciphertexts this event replaces. Initial
    /// submissions use `Pubkey::default()` and therefore replace encrypted
    /// zero. Following the pointer keeps the old curve available to fold bots
    /// without duplicating 2,176 bytes in every replacement account.
    pub previous_event: Pubkey,
    #[max_len(2176)]
    pub ciphertexts: Vec<u8>,
    /// Hash of the canonical auction/order metadata and all ciphertext bytes.
    pub ciphertext_digest: [u8; QUEUE_DIGEST_BYTES],
    /// Poseidon commitment to private quantities, reserves and salt.
    pub order_commitment: [u8; QUEUE_DIGEST_BYTES],
    pub order_circuit_version: u16,
    pub bump: u8,
}

const QUEUED_ORDER_VERSION_OFFSET: usize = 8;
const QUEUED_ORDER_AUCTION_OFFSET: usize = QUEUED_ORDER_VERSION_OFFSET + 1 + 32;
const QUEUED_ORDER_STATE_OFFSET: usize = QUEUED_ORDER_AUCTION_OFFSET + 32 + 32 + 32 + 8 + 8;
const QUEUED_ORDER_CIPHERTEXTS_LEN_OFFSET: usize =
    QUEUED_ORDER_STATE_OFFSET + 1 + 1 + QUEUE_DIGEST_BYTES + 32;

pub struct FoldOrderMetadata {
    pub auction: Pubkey,
    pub state: QueuedOrderState,
    pub ciphertext_digest: QueueDigest,
}

/// Read only the fixed fields needed by a fold. This avoids allocating and
/// copying the 2,176-byte ciphertext vector during SBF execution.
pub fn fold_order_metadata(data: &[u8]) -> Option<FoldOrderMetadata> {
    if data.len() < QUEUED_ORDER_CIPHERTEXTS_LEN_OFFSET + 4
        || &data[..8] != QueuedOrder::DISCRIMINATOR
        || data[QUEUED_ORDER_VERSION_OFFSET] != crate::state::PROTOCOL_VERSION
    {
        return None;
    }
    let auction = Pubkey::new_from_array(
        data[QUEUED_ORDER_AUCTION_OFFSET..QUEUED_ORDER_AUCTION_OFFSET + 32]
            .try_into()
            .ok()?,
    );
    let state = match data[QUEUED_ORDER_STATE_OFFSET] {
        value if value == QueuedOrderState::Accepted as u8 => QueuedOrderState::Accepted,
        value if value == QueuedOrderState::Folded as u8 => QueuedOrderState::Folded,
        _ => return None,
    };
    let ciphertext_len = u32::from_le_bytes(
        data[QUEUED_ORDER_CIPHERTEXTS_LEN_OFFSET..QUEUED_ORDER_CIPHERTEXTS_LEN_OFFSET + 4]
            .try_into()
            .ok()?,
    ) as usize;
    if ciphertext_len != QUEUED_CIPHERTEXT_COUNT * ELGAMAL_CIPHERTEXT_BYTES {
        return None;
    }
    let digest_offset = QUEUED_ORDER_CIPHERTEXTS_LEN_OFFSET
        .checked_add(4)?
        .checked_add(ciphertext_len)?;
    let ciphertext_digest = data
        .get(digest_offset..digest_offset + QUEUE_DIGEST_BYTES)?
        .try_into()
        .ok()?;
    Some(FoldOrderMetadata {
        auction,
        state,
        ciphertext_digest,
    })
}

pub fn mark_order_folded(data: &mut [u8]) -> QueueInvariantResult<()> {
    let metadata = fold_order_metadata(data).ok_or(QueueInvariantError::OrderAlreadyFolded)?;
    fold_order_state(metadata.state)?;
    data[QUEUED_ORDER_STATE_OFFSET] = QueuedOrderState::Folded as u8;
    Ok(())
}

/// One mutable pointer to the latest private order for `(auction, owner)`.
/// Historical queue events remain immutable and independently foldable.
#[account]
#[derive(InitSpace)]
pub struct ActiveOrder {
    pub version: u8,
    pub market: Pubkey,
    pub auction: Pubkey,
    pub owner: Pubkey,
    pub latest_event: Pubkey,
    pub revision: u64,
    pub active: bool,
    pub bump: u8,
}

/// Lightweight shared state written by accepted order submissions.
#[account]
#[derive(InitSpace)]
pub struct SubmissionCounter {
    pub version: u8,
    pub auction: Pubkey,
    pub accepted_order_count: u64,
    pub bump: u8,
}

/// The single encrypted aggregate advanced by permissionless fold transactions.
#[account]
#[derive(InitSpace)]
pub struct AggregateAccumulator {
    pub version: u8,
    pub auction: Pubkey,
    pub folded_order_count: u64,
    /// Incremented once per successful fold transaction, independent of batch size.
    pub fold_revision: u64,
    #[max_len(2176)]
    pub ciphertexts: Vec<u8>,
    /// Hash of the canonical accumulator metadata and ciphertext bytes.
    pub ciphertext_digest: [u8; QUEUE_DIGEST_BYTES],
    pub bump: u8,
}

/// A trader's proof-controlled encrypted balances for one market.
#[account]
#[derive(InitSpace)]
pub struct EncryptedPrivateBalance {
    pub version: u8,
    pub market: Pubkey,
    pub owner: Pubkey,
    pub key_epoch: Pubkey,
    pub encrypted_base: [u8; ELGAMAL_CIPHERTEXT_BYTES],
    pub encrypted_quote: [u8; ELGAMAL_CIPHERTEXT_BYTES],
    /// Public Token-2022 collateral moved into the market vaults. These totals
    /// make the custody side independently auditable; the deposit circuit
    /// proves that each increment is reflected in the encrypted balances.
    pub deposited_base: u64,
    pub deposited_quote: u64,
    /// Hash bound by the balance-transition proof's public inputs.
    pub balance_digest: [u8; QUEUE_DIGEST_BYTES],
    /// Monotonic replay guard for balance-transition proofs.
    pub revision: u64,
    pub bump: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueInvariantError {
    InvalidDeadlines,
    CollectionClosed,
    AggregationDeadlineReached,
    AggregationDeadlineNotReached,
    OrderAlreadyFolded,
    EmptyFoldBatch,
    CountOverflow,
    FoldedCountExceedsAccepted,
    CollectionStillOpen,
    AggregationIncomplete,
    AggregationAlreadyComplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WithdrawalInvariantError {
    ClaimRequiresFoldedOrder,
    AlreadyWithdrawn,
}

pub type QueueInvariantResult<T> = core::result::Result<T, QueueInvariantError>;
pub type WithdrawalInvariantResult<T> = core::result::Result<T, WithdrawalInvariantError>;

/// Return the irreversible settlement state for a queued order.
///
/// Claims consume only orders proven to be part of the final aggregate.
/// Refunds deliberately accept either aggregation state because cancellation
/// may occur with accepted orders still waiting in the queue.
pub const fn advance_withdrawal(
    order_state: QueuedOrderState,
    withdrawal_state: WithdrawalState,
    kind: WithdrawalKind,
) -> WithdrawalInvariantResult<WithdrawalState> {
    if !matches!(withdrawal_state, WithdrawalState::Pending) {
        return Err(WithdrawalInvariantError::AlreadyWithdrawn);
    }
    match (kind, order_state) {
        (WithdrawalKind::Claim, QueuedOrderState::Folded) => Ok(WithdrawalState::Claimed),
        (WithdrawalKind::Claim, QueuedOrderState::Accepted) => {
            Err(WithdrawalInvariantError::ClaimRequiresFoldedOrder)
        }
        (WithdrawalKind::Refund, QueuedOrderState::Accepted | QueuedOrderState::Folded) => {
            Ok(WithdrawalState::Refunded)
        }
    }
}

/// Authorize submission and return `(sequence, next_accepted_count)`.
///
/// The handler must create the order account and store both values atomically
/// with the proof-controlled encrypted balance transition.
pub fn accept_order(
    current_slot: u64,
    deadlines: QueueDeadlines,
    accepted_order_count: u64,
) -> QueueInvariantResult<(u64, u64)> {
    validate_deadlines(deadlines)?;
    if !deadlines.collection_is_open(current_slot) {
        return Err(QueueInvariantError::CollectionClosed);
    }

    let next = accepted_order_count
        .checked_add(1)
        .ok_or(QueueInvariantError::CountOverflow)?;
    Ok((accepted_order_count, next))
}

/// Return the irreversible next state for one order selected for folding.
pub const fn fold_order_state(state: QueuedOrderState) -> QueueInvariantResult<QueuedOrderState> {
    match state {
        QueuedOrderState::Accepted => Ok(QueuedOrderState::Folded),
        QueuedOrderState::Folded => Err(QueueInvariantError::OrderAlreadyFolded),
    }
}

/// Validate a fold and return `(next_folded_count, next_fold_revision)`.
///
/// Each supplied order must separately pass [`fold_order_state`] and belong to
/// the same auction. This count preflight prevents a malformed batch from ever
/// making the folded count exceed the authoritative accepted count.
pub fn advance_fold(
    current_slot: u64,
    deadlines: QueueDeadlines,
    accepted_order_count: u64,
    folded_order_count: u64,
    fold_revision: u64,
    batch_size: u64,
) -> QueueInvariantResult<(u64, u64)> {
    validate_deadlines(deadlines)?;
    validate_counts(accepted_order_count, folded_order_count)?;
    if !deadlines.aggregation_is_open(current_slot) {
        return Err(QueueInvariantError::AggregationDeadlineReached);
    }
    if batch_size == 0 {
        return Err(QueueInvariantError::EmptyFoldBatch);
    }

    let next_folded = folded_order_count
        .checked_add(batch_size)
        .ok_or(QueueInvariantError::CountOverflow)?;
    if next_folded > accepted_order_count {
        return Err(QueueInvariantError::FoldedCountExceedsAccepted);
    }
    let next_revision = fold_revision
        .checked_add(1)
        .ok_or(QueueInvariantError::CountOverflow)?;
    Ok((next_folded, next_revision))
}

/// Whether all and only accepted orders have been folded.
pub const fn aggregation_is_complete(accepted_order_count: u64, folded_order_count: u64) -> bool {
    accepted_order_count == folded_order_count
}

/// Authorize entering decryption/settlement after collection.
///
/// There is deliberately no `current_slot < aggregation_deadline_slot` check:
/// a fold cannot complete at or after that deadline, so an aggregate which is
/// already complete remains settleable even if the settlement transaction is
/// submitted later. This also makes a zero-order auction settleable.
pub fn authorize_settlement(
    current_slot: u64,
    deadlines: QueueDeadlines,
    accepted_order_count: u64,
    folded_order_count: u64,
) -> QueueInvariantResult<()> {
    validate_deadlines(deadlines)?;
    validate_counts(accepted_order_count, folded_order_count)?;
    if deadlines.collection_is_open(current_slot) {
        return Err(QueueInvariantError::CollectionStillOpen);
    }
    if !aggregation_is_complete(accepted_order_count, folded_order_count) {
        return Err(QueueInvariantError::AggregationIncomplete);
    }
    Ok(())
}

/// Authorize permissionless cancellation of an incomplete aggregate.
pub fn authorize_aggregation_timeout(
    current_slot: u64,
    deadlines: QueueDeadlines,
    accepted_order_count: u64,
    folded_order_count: u64,
) -> QueueInvariantResult<()> {
    validate_deadlines(deadlines)?;
    validate_counts(accepted_order_count, folded_order_count)?;
    if !deadlines.aggregation_timed_out(current_slot) {
        return Err(QueueInvariantError::AggregationDeadlineNotReached);
    }
    if aggregation_is_complete(accepted_order_count, folded_order_count) {
        return Err(QueueInvariantError::AggregationAlreadyComplete);
    }
    Ok(())
}

const fn validate_deadlines(deadlines: QueueDeadlines) -> QueueInvariantResult<()> {
    if deadlines.is_valid() {
        Ok(())
    } else {
        Err(QueueInvariantError::InvalidDeadlines)
    }
}

const fn validate_counts(
    accepted_order_count: u64,
    folded_order_count: u64,
) -> QueueInvariantResult<()> {
    if folded_order_count <= accepted_order_count {
        Ok(())
    } else {
        Err(QueueInvariantError::FoldedCountExceedsAccepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BindRolloverOrderArgs, BindRolloverSettlementArgs, CancelActiveOrderArgs,
        FoldQueuedOrdersArgs, Groth16ProofInput, ReleaseOrderCollateralArgs,
        ReplaceQueuedOrderArgs, RolloverNextOrderArgs, RolloverOrderArgs, RolloverSettlementArgs,
        SubmitQueuedOrderArgs, VerifyRolloverProofArgs,
    };

    const DEADLINES: QueueDeadlines = QueueDeadlines {
        collection_end_slot: 100,
        aggregation_deadline_slot: 200,
    };

    #[test]
    fn layout_is_exactly_seventeen_buy_and_seventeen_sell_ciphertexts() {
        assert_eq!(QUEUE_PRICE_LEVELS, 17);
        assert_eq!(QUEUED_CIPHERTEXT_COUNT, 34);
        assert_eq!(core::mem::size_of::<EncryptedOrder>(), 34 * 64);
    }

    #[test]
    fn allocation_free_fold_view_matches_anchor_layout() {
        let auction = Pubkey::new_unique();
        let digest = [0x5a; QUEUE_DIGEST_BYTES];
        let order = QueuedOrder {
            version: crate::state::PROTOCOL_VERSION,
            market: Pubkey::new_unique(),
            auction,
            owner: Pubkey::new_unique(),
            funder: Pubkey::new_unique(),
            client_order_nonce: 7,
            sequence: 11,
            state: QueuedOrderState::Accepted,
            withdrawal_state: WithdrawalState::Pending,
            terminal_nullifier: [0; QUEUE_DIGEST_BYTES],
            previous_event: Pubkey::default(),
            ciphertexts: vec![1; QUEUED_CIPHERTEXT_COUNT * ELGAMAL_CIPHERTEXT_BYTES],
            ciphertext_digest: digest,
            order_commitment: [2; QUEUE_DIGEST_BYTES],
            order_circuit_version: ORDER_SUBMISSION_CIRCUIT_VERSION,
            bump: 3,
        };
        let mut data = Vec::new();
        order.try_serialize(&mut data).unwrap();
        let metadata = fold_order_metadata(&data).unwrap();
        assert_eq!(metadata.auction, auction);
        assert_eq!(metadata.state, QueuedOrderState::Accepted);
        assert_eq!(metadata.ciphertext_digest, digest);
        mark_order_folded(&mut data).unwrap();
        let decoded = QueuedOrder::try_deserialize(&mut data.as_slice()).unwrap();
        assert_eq!(decoded.state, QueuedOrderState::Folded);
        assert_eq!(decoded.ciphertexts, order.ciphertexts);
    }

    #[test]
    fn circuit_kinds_are_append_only_and_independently_versioned() {
        assert_eq!(QueueCircuitKind::OrderSubmission as u8, 0);
        assert_eq!(QueueCircuitKind::AggregateFold as u8, 1);
        assert_eq!(QueueCircuitKind::BalanceDeposit as u8, 2);
        assert_eq!(QueueCircuitKind::AggregatePublication as u8, 3);
        assert_eq!(QueueCircuitKind::PrivateClaim as u8, 4);
        assert_eq!(QueueCircuitKind::CancellationRefund as u8, 5);
        assert_eq!(QueueCircuitKind::OrderModification as u8, 6);
        assert_eq!(QueueCircuitKind::RolloverSettlement as u8, 7);
        assert_eq!(
            circuit_version(QueueCircuitKind::AggregateFold),
            AGGREGATE_FOLD_CIRCUIT_VERSION
        );
        assert_eq!(
            circuit_version(QueueCircuitKind::PrivateClaim),
            PRIVATE_CLAIM_CIRCUIT_VERSION
        );
        assert_eq!(
            circuit_version(QueueCircuitKind::CancellationRefund),
            CANCELLATION_REFUND_CIRCUIT_VERSION
        );
        assert_eq!(
            circuit_version(QueueCircuitKind::OrderModification),
            ORDER_MODIFICATION_CIRCUIT_VERSION
        );
        assert_eq!(
            circuit_version(QueueCircuitKind::RolloverSettlement),
            ROLLOVER_SETTLEMENT_CIRCUIT_VERSION
        );
    }

    #[test]
    fn submission_assigns_preincrement_sequence() {
        assert_eq!(accept_order(99, DEADLINES, 41), Ok((41, 42)));
        assert_eq!(
            accept_order(100, DEADLINES, 41),
            Err(QueueInvariantError::CollectionClosed)
        );
        assert_eq!(
            accept_order(99, DEADLINES, u64::MAX),
            Err(QueueInvariantError::CountOverflow)
        );
    }

    #[test]
    fn accepted_order_can_be_folded_exactly_once() {
        assert_eq!(
            fold_order_state(QueuedOrderState::Accepted),
            Ok(QueuedOrderState::Folded)
        );
        assert_eq!(
            fold_order_state(QueuedOrderState::Folded),
            Err(QueueInvariantError::OrderAlreadyFolded)
        );
    }

    #[test]
    fn claims_require_a_folded_pending_order() {
        assert_eq!(
            advance_withdrawal(
                QueuedOrderState::Folded,
                WithdrawalState::Pending,
                WithdrawalKind::Claim,
            ),
            Ok(WithdrawalState::Claimed)
        );
        assert_eq!(
            advance_withdrawal(
                QueuedOrderState::Accepted,
                WithdrawalState::Pending,
                WithdrawalKind::Claim,
            ),
            Err(WithdrawalInvariantError::ClaimRequiresFoldedOrder)
        );
    }

    #[test]
    fn cancellation_refunds_accepted_or_folded_orders() {
        for order_state in [QueuedOrderState::Accepted, QueuedOrderState::Folded] {
            assert_eq!(
                advance_withdrawal(
                    order_state,
                    WithdrawalState::Pending,
                    WithdrawalKind::Refund,
                ),
                Ok(WithdrawalState::Refunded)
            );
        }
    }

    #[test]
    fn terminal_withdrawals_cannot_be_replayed_or_changed() {
        for withdrawal_state in [WithdrawalState::Claimed, WithdrawalState::Refunded] {
            for order_state in [QueuedOrderState::Accepted, QueuedOrderState::Folded] {
                for kind in [WithdrawalKind::Claim, WithdrawalKind::Refund] {
                    assert_eq!(
                        advance_withdrawal(order_state, withdrawal_state, kind),
                        Err(WithdrawalInvariantError::AlreadyWithdrawn)
                    );
                }
            }
        }
    }

    #[test]
    fn folding_is_allowed_until_the_temporal_deadline() {
        assert_eq!(advance_fold(150, DEADLINES, 10, 3, 7, 4), Ok((7, 8)));
    }

    #[test]
    fn fold_preflight_preserves_count_invariant() {
        assert_eq!(
            advance_fold(50, DEADLINES, 5, 3, 9, 0),
            Err(QueueInvariantError::EmptyFoldBatch)
        );
        assert_eq!(
            advance_fold(50, DEADLINES, 5, 3, 9, 3),
            Err(QueueInvariantError::FoldedCountExceedsAccepted)
        );
        assert_eq!(
            advance_fold(50, DEADLINES, 4, 5, 9, 1),
            Err(QueueInvariantError::FoldedCountExceedsAccepted)
        );
        assert_eq!(
            advance_fold(50, DEADLINES, 5, 4, u64::MAX, 1),
            Err(QueueInvariantError::CountOverflow)
        );
    }

    #[test]
    fn aggregation_deadline_is_half_open() {
        assert_eq!(advance_fold(199, DEADLINES, 2, 0, 0, 2), Ok((2, 1)));
        assert_eq!(
            advance_fold(200, DEADLINES, 2, 0, 0, 2),
            Err(QueueInvariantError::AggregationDeadlineReached)
        );
    }

    #[test]
    fn settlement_waits_for_collection_and_exact_completeness() {
        assert_eq!(
            authorize_settlement(99, DEADLINES, 2, 2),
            Err(QueueInvariantError::CollectionStillOpen)
        );
        assert_eq!(
            authorize_settlement(100, DEADLINES, 2, 1),
            Err(QueueInvariantError::AggregationIncomplete)
        );
        assert_eq!(authorize_settlement(100, DEADLINES, 2, 2), Ok(()));
        assert_eq!(authorize_settlement(250, DEADLINES, 2, 2), Ok(()));
        assert_eq!(authorize_settlement(100, DEADLINES, 0, 0), Ok(()));
    }

    #[test]
    fn timeout_only_cancels_incomplete_aggregation() {
        assert_eq!(
            authorize_aggregation_timeout(199, DEADLINES, 2, 1),
            Err(QueueInvariantError::AggregationDeadlineNotReached)
        );
        assert_eq!(authorize_aggregation_timeout(200, DEADLINES, 2, 1), Ok(()));
        assert_eq!(
            authorize_aggregation_timeout(200, DEADLINES, 2, 2),
            Err(QueueInvariantError::AggregationAlreadyComplete)
        );
    }

    #[test]
    fn deadline_ordering_is_strict() {
        for deadlines in [
            QueueDeadlines {
                collection_end_slot: 100,
                aggregation_deadline_slot: 100,
            },
            QueueDeadlines {
                collection_end_slot: 101,
                aggregation_deadline_slot: 100,
            },
        ] {
            assert_eq!(
                accept_order(1, deadlines, 0),
                Err(QueueInvariantError::InvalidDeadlines)
            );
        }
    }

    #[test]
    fn queue_instruction_payloads_fit_v1_with_framing_headroom() {
        let proof = Groth16ProofInput {
            proof_a: [0; 64],
            proof_b: [0; 128],
            proof_c: [0; 64],
            public_input: [0; 32],
        };
        let submit = SubmitQueuedOrderArgs {
            client_order_nonce: 0,
            order_commitment: [0; 32],
            ciphertexts: vec![0; ELGAMAL_CIPHERTEXT_BYTES * QUEUED_CIPHERTEXT_COUNT],
            new_encrypted_base: [0; 64],
            new_encrypted_quote: [0; 64],
            proof: proof.clone(),
        };
        let fold = FoldQueuedOrdersArgs {
            new_aggregate: vec![0; ELGAMAL_CIPHERTEXT_BYTES * QUEUED_CIPHERTEXT_COUNT],
            proof: proof.clone(),
        };
        let release = ReleaseOrderCollateralArgs {
            released_encrypted_base: [0; 64],
            released_encrypted_quote: [0; 64],
            proof: proof.clone(),
        };
        let replacement = ReplaceQueuedOrderArgs {
            release: release.clone(),
            replacement: submit.clone(),
        };
        let cancellation = CancelActiveOrderArgs { release };
        let rollover = RolloverOrderArgs {
            settlement: RolloverSettlementArgs {
                terminal_nullifier: [1; 32],
                credited_encrypted_base: [0; 64],
                credited_encrypted_quote: [0; 64],
            },
            next_order: RolloverNextOrderArgs {
                client_order_nonce: submit.client_order_nonce,
                order_commitment: submit.order_commitment,
                ciphertexts: submit.ciphertexts.clone(),
                new_encrypted_base: submit.new_encrypted_base,
                new_encrypted_quote: submit.new_encrypted_quote,
            },
        };
        let rollover_verification = VerifyRolloverProofArgs {
            circuit_kind: QueueCircuitKind::RolloverSettlement,
            expected_public_input: [0; 32],
            proof,
        };
        let settlement_binding = BindRolloverSettlementArgs {
            settlement: rollover.settlement.clone(),
            expected_public_input: [0; 32],
        };
        let order_binding = BindRolloverOrderArgs {
            next_order: rollover.next_order.clone(),
            credited_encrypted_base: rollover.settlement.credited_encrypted_base,
            credited_encrypted_quote: rollover.settlement.credited_encrypted_quote,
            expected_public_input: [0; 32],
        };
        let mut submit_bytes = Vec::new();
        submit.serialize(&mut submit_bytes).unwrap();
        let mut fold_bytes = Vec::new();
        fold.serialize(&mut fold_bytes).unwrap();
        let mut replacement_bytes = Vec::new();
        replacement.serialize(&mut replacement_bytes).unwrap();
        let mut cancellation_bytes = Vec::new();
        cancellation.serialize(&mut cancellation_bytes).unwrap();
        let mut rollover_bytes = Vec::new();
        rollover.serialize(&mut rollover_bytes).unwrap();
        let mut rollover_verification_bytes = Vec::new();
        rollover_verification
            .serialize(&mut rollover_verification_bytes)
            .unwrap();
        let mut settlement_binding_bytes = Vec::new();
        settlement_binding
            .serialize(&mut settlement_binding_bytes)
            .unwrap();
        let mut order_binding_bytes = Vec::new();
        order_binding.serialize(&mut order_binding_bytes).unwrap();
        assert_eq!(submit_bytes.len(), 2_636);
        assert_eq!(fold_bytes.len(), 2_468);
        assert_eq!(replacement_bytes.len(), 3_052);
        assert_eq!(cancellation_bytes.len(), 416);
        assert_eq!(rollover_bytes.len(), 2_508);
        assert_eq!(rollover_verification_bytes.len(), 321);
        assert_eq!(settlement_binding_bytes.len(), 192);
        assert_eq!(order_binding_bytes.len(), 2_508);
        let rollover_instruction_data = 2 * rollover_verification_bytes.len()
            + settlement_binding_bytes.len()
            + order_binding_bytes.len()
            + 6 * 8;
        assert_eq!(rollover_instruction_data, 3_390);
        // The six-instruction atomic rollover remains below the 4,096-byte v1
        // packet limit after account indexes and message framing.
        assert!(submit_bytes.len() < 3_000);
        assert!(fold_bytes.len() < 3_000);
        assert!(replacement_bytes.len() < 3_200);
        assert!(rollover_instruction_data < 3_500);
    }
}

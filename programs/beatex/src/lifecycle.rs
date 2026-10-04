//! Pure auction lifecycle and withdrawal policy.
//!
//! Slot intervals are half-open. An operation whose deadline is `d` is valid
//! while `current_slot < d`; its timeout transition becomes valid at
//! `current_slot >= d`.

use anchor_lang::prelude::{AnchorDeserialize, AnchorSerialize, InitSpace};

/// Coarse auction phase stored by the auction account.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum AuctionPhase {
    /// Orders may be finalized and added to the aggregate.
    OrderCollection = 0,
    /// Collection is closed and the aggregate is immutable.
    Sealed = 1,
    /// At least one decryption share has been accepted.
    Decrypting = 2,
    /// Clearing is complete and claims may be withdrawn before their deadline.
    Settled = 3,
    /// Settlement timed out; pending orders may withdraw refunds.
    Cancelled = 4,
}

impl AuctionPhase {
    /// Only these phases may be cancelled by the permissionless timeout path.
    pub const fn is_unsettled(self) -> bool {
        matches!(
            self,
            Self::OrderCollection | Self::Sealed | Self::Decrypting
        )
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Cancelled)
    }
}

/// Immutable slot boundaries fixed when an auction is opened.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuctionDeadlines {
    /// Last exclusive slot for finalizing orders.
    pub order_collection_end_slot: u64,
    /// Last exclusive slot for completing decryption and settlement.
    pub settlement_deadline_slot: u64,
    /// Last exclusive slot for withdrawing a successful settlement claim.
    pub claim_deadline_slot: u64,
}

impl AuctionDeadlines {
    /// Deadlines must leave at least one slot for settlement and one for claims.
    pub const fn is_valid(self) -> bool {
        self.order_collection_end_slot < self.settlement_deadline_slot
            && self.settlement_deadline_slot < self.claim_deadline_slot
    }

    pub const fn order_collection_is_open(self, current_slot: u64) -> bool {
        current_slot < self.order_collection_end_slot
    }

    pub const fn settlement_is_open(self, current_slot: u64) -> bool {
        current_slot >= self.order_collection_end_slot
            && current_slot < self.settlement_deadline_slot
    }

    pub const fn settlement_timed_out(self, current_slot: u64) -> bool {
        current_slot >= self.settlement_deadline_slot
    }

    pub const fn claim_is_open(self, current_slot: u64) -> bool {
        current_slot < self.claim_deadline_slot
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAction {
    Seal,
    BeginDecryption,
    Settle,
    CancelAfterTimeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleError {
    InvalidDeadlines,
    OrderCollectionStillOpen,
    SettlementWindowNotOpen,
    SettlementDeadlineReached,
    SettlementDeadlineNotReached,
    ClaimDeadlineReached,
    InvalidTransition {
        phase: AuctionPhase,
        action: LifecycleAction,
    },
    OrderAlreadyWithdrawn,
    WithdrawalNotAllowed,
}

/// Apply one auction transition without mutating account state.
///
/// `CancelAfterTimeout` is deliberately authority-free: any caller may request
/// it for an unsettled auction once the settlement deadline has been reached.
pub fn transition(
    phase: AuctionPhase,
    action: LifecycleAction,
    current_slot: u64,
    deadlines: AuctionDeadlines,
) -> Result<AuctionPhase, LifecycleError> {
    if !deadlines.is_valid() {
        return Err(LifecycleError::InvalidDeadlines);
    }

    match (phase, action) {
        (AuctionPhase::OrderCollection, LifecycleAction::Seal) => {
            if deadlines.order_collection_is_open(current_slot) {
                Err(LifecycleError::OrderCollectionStillOpen)
            } else if deadlines.settlement_timed_out(current_slot) {
                Err(LifecycleError::SettlementDeadlineReached)
            } else {
                Ok(AuctionPhase::Sealed)
            }
        }
        (AuctionPhase::Sealed, LifecycleAction::BeginDecryption) => {
            if current_slot < deadlines.order_collection_end_slot {
                Err(LifecycleError::SettlementWindowNotOpen)
            } else if deadlines.settlement_timed_out(current_slot) {
                Err(LifecycleError::SettlementDeadlineReached)
            } else {
                Ok(AuctionPhase::Decrypting)
            }
        }
        (AuctionPhase::Decrypting, LifecycleAction::Settle) => {
            if current_slot < deadlines.order_collection_end_slot {
                Err(LifecycleError::SettlementWindowNotOpen)
            } else if deadlines.settlement_timed_out(current_slot) {
                Err(LifecycleError::SettlementDeadlineReached)
            } else {
                Ok(AuctionPhase::Settled)
            }
        }
        (phase, LifecycleAction::CancelAfterTimeout) if phase.is_unsettled() => {
            if deadlines.settlement_timed_out(current_slot) {
                Ok(AuctionPhase::Cancelled)
            } else {
                Err(LifecycleError::SettlementDeadlineNotReached)
            }
        }
        (phase, action) => Err(LifecycleError::InvalidTransition { phase, action }),
    }
}

/// Per-order state used to make claim and refund withdrawals mutually exclusive.
#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum OrderWithdrawalState {
    Pending = 0,
    Claimed = 1,
    Refunded = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WithdrawalKind {
    /// Withdraw the proof-authorized allocation from a settled auction.
    Claim,
    /// Withdraw the original collateral from a cancelled auction.
    Refund,
}

/// Validate a withdrawal and return the order's next terminal state.
///
/// This function enforces lifecycle, deadline, and replay policy. A caller must
/// still verify the claim/refund proof and complete the token CPI atomically
/// with writing the returned state.
pub fn authorize_withdrawal(
    phase: AuctionPhase,
    order_state: OrderWithdrawalState,
    kind: WithdrawalKind,
    current_slot: u64,
    deadlines: AuctionDeadlines,
) -> Result<OrderWithdrawalState, LifecycleError> {
    if !deadlines.is_valid() {
        return Err(LifecycleError::InvalidDeadlines);
    }
    if order_state != OrderWithdrawalState::Pending {
        return Err(LifecycleError::OrderAlreadyWithdrawn);
    }

    match (phase, kind) {
        (AuctionPhase::Settled, WithdrawalKind::Claim) => {
            if deadlines.claim_is_open(current_slot) {
                Ok(OrderWithdrawalState::Claimed)
            } else {
                Err(LifecycleError::ClaimDeadlineReached)
            }
        }
        (AuctionPhase::Cancelled, WithdrawalKind::Refund) => Ok(OrderWithdrawalState::Refunded),
        _ => Err(LifecycleError::WithdrawalNotAllowed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADLINES: AuctionDeadlines = AuctionDeadlines {
        order_collection_end_slot: 10,
        settlement_deadline_slot: 20,
        claim_deadline_slot: 30,
    };

    const PHASES: [AuctionPhase; 5] = [
        AuctionPhase::OrderCollection,
        AuctionPhase::Sealed,
        AuctionPhase::Decrypting,
        AuctionPhase::Settled,
        AuctionPhase::Cancelled,
    ];

    const ACTIONS: [LifecycleAction; 4] = [
        LifecycleAction::Seal,
        LifecycleAction::BeginDecryption,
        LifecycleAction::Settle,
        LifecycleAction::CancelAfterTimeout,
    ];

    const ORDER_STATES: [OrderWithdrawalState; 3] = [
        OrderWithdrawalState::Pending,
        OrderWithdrawalState::Claimed,
        OrderWithdrawalState::Refunded,
    ];

    const WITHDRAWAL_KINDS: [WithdrawalKind; 2] = [WithdrawalKind::Claim, WithdrawalKind::Refund];

    #[test]
    fn deadline_ordering_is_strict() {
        assert!(DEADLINES.is_valid());

        for invalid in [
            AuctionDeadlines {
                order_collection_end_slot: 10,
                settlement_deadline_slot: 10,
                claim_deadline_slot: 30,
            },
            AuctionDeadlines {
                order_collection_end_slot: 11,
                settlement_deadline_slot: 10,
                claim_deadline_slot: 30,
            },
            AuctionDeadlines {
                order_collection_end_slot: 10,
                settlement_deadline_slot: 30,
                claim_deadline_slot: 30,
            },
            AuctionDeadlines {
                order_collection_end_slot: 10,
                settlement_deadline_slot: 31,
                claim_deadline_slot: 30,
            },
        ] {
            assert!(!invalid.is_valid());
            assert_eq!(
                transition(
                    AuctionPhase::OrderCollection,
                    LifecycleAction::Seal,
                    10,
                    invalid,
                ),
                Err(LifecycleError::InvalidDeadlines)
            );
        }
    }

    #[test]
    fn slot_windows_are_half_open() {
        assert!(DEADLINES.order_collection_is_open(9));
        assert!(!DEADLINES.order_collection_is_open(10));

        assert!(!DEADLINES.settlement_is_open(9));
        assert!(DEADLINES.settlement_is_open(10));
        assert!(DEADLINES.settlement_is_open(19));
        assert!(!DEADLINES.settlement_is_open(20));
        assert!(!DEADLINES.settlement_timed_out(19));
        assert!(DEADLINES.settlement_timed_out(20));

        assert!(DEADLINES.claim_is_open(29));
        assert!(!DEADLINES.claim_is_open(30));
    }

    #[test]
    fn happy_path_reaches_settlement() {
        let sealed = transition(
            AuctionPhase::OrderCollection,
            LifecycleAction::Seal,
            10,
            DEADLINES,
        )
        .unwrap();
        let decrypting =
            transition(sealed, LifecycleAction::BeginDecryption, 10, DEADLINES).unwrap();
        let settled = transition(decrypting, LifecycleAction::Settle, 19, DEADLINES).unwrap();

        assert_eq!(settled, AuctionPhase::Settled);
        assert!(settled.is_terminal());
    }

    #[test]
    fn transition_matrix_before_timeout_is_exhaustive() {
        for phase in PHASES {
            for action in ACTIONS {
                let result = transition(phase, action, 15, DEADLINES);
                let expected = match (phase, action) {
                    (AuctionPhase::OrderCollection, LifecycleAction::Seal) => {
                        Ok(AuctionPhase::Sealed)
                    }
                    (AuctionPhase::Sealed, LifecycleAction::BeginDecryption) => {
                        Ok(AuctionPhase::Decrypting)
                    }
                    (AuctionPhase::Decrypting, LifecycleAction::Settle) => {
                        Ok(AuctionPhase::Settled)
                    }
                    (phase, LifecycleAction::CancelAfterTimeout) if phase.is_unsettled() => {
                        Err(LifecycleError::SettlementDeadlineNotReached)
                    }
                    (phase, action) => Err(LifecycleError::InvalidTransition { phase, action }),
                };
                assert_eq!(result, expected, "phase={phase:?}, action={action:?}");
            }
        }
    }

    #[test]
    fn timeout_cancellation_matrix_is_exhaustive_and_permissionless() {
        for phase in PHASES {
            let result = transition(
                phase,
                LifecycleAction::CancelAfterTimeout,
                DEADLINES.settlement_deadline_slot,
                DEADLINES,
            );
            let expected = if phase.is_unsettled() {
                Ok(AuctionPhase::Cancelled)
            } else {
                Err(LifecycleError::InvalidTransition {
                    phase,
                    action: LifecycleAction::CancelAfterTimeout,
                })
            };
            assert_eq!(result, expected, "phase={phase:?}");
        }
    }

    #[test]
    fn settlement_cannot_race_timeout_at_boundary() {
        assert_eq!(
            transition(
                AuctionPhase::Decrypting,
                LifecycleAction::Settle,
                19,
                DEADLINES,
            ),
            Ok(AuctionPhase::Settled)
        );
        assert_eq!(
            transition(
                AuctionPhase::Decrypting,
                LifecycleAction::Settle,
                20,
                DEADLINES,
            ),
            Err(LifecycleError::SettlementDeadlineReached)
        );
        assert_eq!(
            transition(
                AuctionPhase::Decrypting,
                LifecycleAction::CancelAfterTimeout,
                20,
                DEADLINES,
            ),
            Ok(AuctionPhase::Cancelled)
        );
    }

    #[test]
    fn withdrawal_matrix_is_exhaustive() {
        for current_slot in [19, 29, 30, u64::MAX] {
            for phase in PHASES {
                for order_state in ORDER_STATES {
                    for kind in WITHDRAWAL_KINDS {
                        let result =
                            authorize_withdrawal(phase, order_state, kind, current_slot, DEADLINES);

                        let expected_success = order_state == OrderWithdrawalState::Pending
                            && match (phase, kind) {
                                (AuctionPhase::Settled, WithdrawalKind::Claim) => {
                                    current_slot < DEADLINES.claim_deadline_slot
                                }
                                (AuctionPhase::Cancelled, WithdrawalKind::Refund) => true,
                                _ => false,
                            };

                        assert_eq!(
                            result.is_ok(),
                            expected_success,
                            "slot={current_slot}, phase={phase:?}, order={order_state:?}, kind={kind:?}"
                        );

                        if let Ok(next) = result {
                            let expected_state = match kind {
                                WithdrawalKind::Claim => OrderWithdrawalState::Claimed,
                                WithdrawalKind::Refund => OrderWithdrawalState::Refunded,
                            };
                            assert_eq!(next, expected_state);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn claim_deadline_and_replay_errors_are_specific() {
        assert_eq!(
            authorize_withdrawal(
                AuctionPhase::Settled,
                OrderWithdrawalState::Pending,
                WithdrawalKind::Claim,
                30,
                DEADLINES,
            ),
            Err(LifecycleError::ClaimDeadlineReached)
        );

        for terminal_order_state in [
            OrderWithdrawalState::Claimed,
            OrderWithdrawalState::Refunded,
        ] {
            assert_eq!(
                authorize_withdrawal(
                    AuctionPhase::Cancelled,
                    terminal_order_state,
                    WithdrawalKind::Refund,
                    30,
                    DEADLINES,
                ),
                Err(LifecycleError::OrderAlreadyWithdrawn)
            );
        }
    }
}

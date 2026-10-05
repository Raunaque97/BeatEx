use {crate::VaultProofError, beatex::queue::WithdrawalKind, solana_pubkey::Pubkey};

/// Token-2022 confidential transfers split the amount into 16 low bits and 32
/// high bits, so transfer amounts are limited to 48 bits.
pub const MAX_TRANSFER_AMOUNT: u64 = (1_u64 << 48) - 1;

/// A read-only snapshot of one shared confidential vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultSnapshot {
    pub token_account: [u8; 32],
    pub mint: [u8; 32],
    pub elgamal_pubkey: [u8; 32],
    pub available_balance: [u8; 64],
    pub decryptable_available_balance: [u8; 36],
    pub pending_balance_credit_counter: u64,
    pub expected_pending_balance_credit_counter: u64,
    pub actual_pending_balance_credit_counter: u64,
}

/// The confidential destination selected by a claim or refund.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfidentialDestination {
    pub token_account: [u8; 32],
    pub mint: [u8; 32],
    pub owner: [u8; 32],
    pub elgamal_pubkey: [u8; 32],
}

/// Allocation-independent inputs needed for one Token-2022 transfer proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferRequest {
    pub source: VaultSnapshot,
    pub destination: ConfidentialDestination,
    pub amount: u64,
    /// `None` represents Token-2022's default/disabled auditor key.
    pub auditor_elgamal_pubkey: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateOrderOpening {
    pub buy: [u64; beatex::queue::QUEUE_PRICE_LEVELS],
    pub sell: [u64; beatex::queue::QUEUE_PRICE_LEVELS],
    pub base_reserve: u64,
    pub quote_reserve: u64,
    /// Little-endian scalar-field representation used when the order was
    /// committed during submission.
    pub order_salt: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WithdrawalStatementContext {
    pub kind: WithdrawalKind,
    pub market: Pubkey,
    pub auction: Pubkey,
    pub order: Pubkey,
    pub owner: Pubkey,
    pub key_epoch: Pubkey,
    pub client_order_nonce: u64,
    pub sequence: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub order_commitment: [u8; 32],
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub demand_at_clearing: u64,
    pub supply_at_clearing: u64,
    pub clearing_volume: u64,
    pub terminal_nullifier: [u8; 32],
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_destination: Pubkey,
    pub quote_destination: Pubkey,
}

impl TransferRequest {
    pub fn validate(&self) -> Result<(), VaultProofError> {
        if self.source.token_account == self.destination.token_account {
            return Err(VaultProofError::SelfTransferNotSupported);
        }
        if self.source.mint != self.destination.mint {
            return Err(VaultProofError::MintMismatch);
        }
        if self.amount > MAX_TRANSFER_AMOUNT {
            return Err(VaultProofError::TransferAmountTooLarge);
        }
        Ok(())
    }
}

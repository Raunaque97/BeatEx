use anchor_lang::prelude::*;

pub const PROTOCOL_VERSION: u8 = 1;
pub const PROOF_TRANSCRIPT_VERSION: u8 = 2;
/// Prototype SBF limit selected from stack-frame builds. Larger grids will use
/// sharded curve accounts instead of enlarging this monolithic account.
pub const MAX_GRID_RADIUS: u8 = 8;
pub const MAX_GRID_SIZE: usize = MAX_GRID_RADIUS as usize * 2 + 1;
pub const MAX_ORDER_CHUNKS: usize = 8;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
pub enum AuctionState {
    Open,
    Sealed,
    Decrypting,
    Decrypted,
    ClaimsOpen,
    Cancelled,
    Finalized,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
pub enum OrderState {
    Writing,
    Finalized,
    Claimed,
    Refunded,
}

#[account]
#[derive(InitSpace)]
pub struct Market {
    pub version: u8,
    pub authority: Pubkey,
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub tick_size: u64,
    pub grid_radius: u8,
    pub max_order_quantity: u64,
    pub max_aggregate_quantity: u64,
    pub current_key_epoch: Pubkey,
    pub last_clearing_price: u64,
    /// Highest cleared auction id plus one; zero means no auction has cleared.
    pub last_cleared_auction_sequence: u64,
    pub next_auction_id: u64,
    pub submission_duration_slots: u64,
    pub aggregation_duration_slots: u64,
    pub decryption_duration_slots: u64,
    pub claim_duration_slots: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub paused: bool,
    pub vault_authority_bump: u8,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Auction {
    pub version: u8,
    pub market: Pubkey,
    pub auction_id: u64,
    pub key_epoch: Pubkey,
    pub state: AuctionState,
    pub open_slot: u64,
    pub submission_end_slot: u64,
    pub aggregation_deadline_slot: u64,
    pub decryption_deadline_slot: u64,
    pub claim_deadline_slot: u64,
    pub reference_price: u64,
    pub tick_size: u64,
    pub grid_size: u8,
    pub finalized_order_count: u32,
    pub aggregate_buy: [[u8; 64]; MAX_GRID_SIZE],
    pub aggregate_sell: [[u8; 64]; MAX_GRID_SIZE],
    pub decrypted_demand: [u64; MAX_GRID_SIZE],
    pub decrypted_supply: [u64; MAX_GRID_SIZE],
    pub decrypted_entry_bitmap: u128,
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub clearing_volume: u64,
    pub base_residual: u64,
    pub quote_residual: u64,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Order {
    pub version: u8,
    pub market: Pubkey,
    pub auction: Pubkey,
    pub owner: Pubkey,
    pub funder: Pubkey,
    pub client_order_nonce: u64,
    pub state: OrderState,
    pub grid_size: u8,
    pub written_entry_bitmap: u128,
    pub encrypted_buy: [[u8; 64]; MAX_GRID_SIZE],
    pub encrypted_sell: [[u8; 64]; MAX_GRID_SIZE],
    pub base_collateral_commitment: [u8; 32],
    pub quote_collateral_commitment: [u8; 32],
    pub order_circuit_version: u16,
    pub claim_nullifier_commitment: [u8; 32],
    pub aggregated: bool,
    pub bump: u8,
}

impl Auction {
    pub fn active_grid_size(&self) -> usize {
        self.grid_size as usize
    }

    pub fn all_entries_decrypted(&self) -> bool {
        let count = self.active_grid_size() * 2;
        let required = if count == 128 {
            u128::MAX
        } else {
            (1_u128 << count) - 1
        };
        self.decrypted_entry_bitmap & required == required
    }
}

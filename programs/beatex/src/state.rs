use anchor_lang::prelude::*;

pub const PROTOCOL_VERSION: u8 = 2;
pub const PROOF_TRANSCRIPT_VERSION: u8 = 2;
/// One private-order quantity unit is one hundred base-token atoms. For the
/// eight-decimal tBTC market this is one hundred satoshis.
pub const BASE_UNIT_SIZE: u64 = 100;
/// One quote unit is one quote-token atom. For six-decimal tUSD this is one
/// micro-tUSD.
pub const QUOTE_UNIT_SIZE: u64 = 1;
pub const DEFAULT_GRID_STEP_BPS: u16 = 10;
/// Prototype SBF limit selected from stack-frame builds. Larger grids require
/// a future account-layout and circuit revision instead of enlarging this
/// monolithic account in protocol v2.
pub const MAX_GRID_RADIUS: u8 = 8;
pub const MAX_GRID_SIZE: usize = MAX_GRID_RADIUS as usize * 2 + 1;
pub const MAX_ORDER_CHUNKS: usize = 8;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, InitSpace, PartialEq)]
pub enum AuctionState {
    Pending,
    Settled,
    Cancelled,
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
    pub base_unit_size: u64,
    pub quote_unit_size: u64,
    pub grid_step_bps: u16,
    pub grid_radius: u8,
    pub max_order_quantity: u64,
    pub max_aggregate_quantity: u64,
    pub current_key_epoch: Pubkey,
    /// Slot at which deterministic auction zero begins collecting orders.
    pub market_start_slot: u64,
    /// Quote-token atoms paid per protocol base unit.
    pub last_clearing_price: u64,
    pub last_clearing_volume: u64,
    /// Highest cleared auction id plus one; zero means no auction has cleared.
    pub last_cleared_auction_sequence: u64,
    /// Minimum previous-auction volume required to reuse its clearing price.
    pub minimum_reference_volume: u64,
    /// Canonical Pyth price update account and feed configured at market creation.
    pub oracle_price_account: Pubkey,
    pub oracle_feed_id: [u8; 32],
    pub oracle_max_age_seconds: u64,
    pub oracle_max_confidence_bps: u16,
    /// Added to the Pyth exponent to convert the feed into quote units per
    /// protocol base unit.
    pub oracle_exponent_adjustment: i8,
    /// Reference-price bounds protect grid arithmetic and circuit amount bounds.
    pub min_reference_price: u64,
    pub max_reference_price: u64,
    /// The collection duration is also the interval between consecutive auctions.
    pub submission_duration_slots: u64,
    pub aggregation_duration_slots: u64,
    pub decryption_duration_slots: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub paused: bool,
    pub vault_authority_bump: u8,
    pub bump: u8,
    /// The latest published clearings, highest auction id first. Empty slots
    /// have a zero clearing price.
    pub recent_clearings: [RecentClearing; RECENT_CLEARINGS],
}

/// How many published clearings the market keeps for on-chain price logic.
pub const RECENT_CLEARINGS: usize = 8;

#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, Eq, InitSpace, PartialEq,
)]
pub struct RecentClearing {
    pub auction_id: u64,
    pub clearing_price: u64,
    pub clearing_volume: u64,
}

impl Market {
    /// Insert a publication in auction-id order, dropping the oldest entry.
    /// Publications can land out of order, so the list is kept sorted rather
    /// than treated as a plain ring.
    pub fn record_clearing(&mut self, clearing: RecentClearing) {
        insert_recent_clearing(&mut self.recent_clearings, clearing);
    }
}

fn insert_recent_clearing(
    recent: &mut [RecentClearing; RECENT_CLEARINGS],
    clearing: RecentClearing,
) {
    let position = recent
        .iter()
        .position(|entry| entry.clearing_price == 0 || entry.auction_id < clearing.auction_id);
    if let Some(position) = position {
        recent.copy_within(position..RECENT_CLEARINGS - 1, position + 1);
        recent[position] = clearing;
    }
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
    /// Quote-token atoms paid per protocol base unit.
    pub reference_price: u64,
    /// True when `reference_price` came from the configured Pyth feed.
    pub reference_from_oracle: bool,
    /// Pyth publish time used for an oracle reference; zero for prior clearing.
    pub oracle_publish_time: i64,
    pub grid_step_bps: u16,
    pub grid_size: u8,
    /// Immutable protocol versions captured when the auction opens.
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
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

/// The compact record an auction is rewritten to, at the same address, once it
/// settles or is cancelled. Settlement, refund and rollover read only these
/// fields, so the working state (aggregate ciphertexts and full decrypted
/// curves) is dropped and its rent returned. The distinct discriminator also
/// stops pending-phase instructions from accepting a finished auction.
#[account]
#[derive(InitSpace)]
pub struct AuctionResult {
    pub version: u8,
    pub market: Pubkey,
    pub auction_id: u64,
    pub key_epoch: Pubkey,
    pub state: AuctionState,
    pub reference_price: u64,
    pub reference_from_oracle: bool,
    pub grid_step_bps: u16,
    pub grid_size: u8,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub clearing_volume: u64,
    pub demand_at_clearing: u64,
    pub supply_at_clearing: u64,
    pub bump: u8,
}

/// The fixed-size leading fields of an `Auction` account, in layout order.
/// Finishing an auction reads only these and leaves the ~2.4 KB of curves that
/// follow unread, which keeps those handlers within the 4 KiB SBF stack.
#[derive(AnchorDeserialize, Clone, Debug, Eq, PartialEq)]
pub struct AuctionHeader {
    pub version: u8,
    pub market: Pubkey,
    pub auction_id: u64,
    pub key_epoch: Pubkey,
    pub state: AuctionState,
    pub open_slot: u64,
    pub submission_end_slot: u64,
    pub aggregation_deadline_slot: u64,
    pub decryption_deadline_slot: u64,
    pub reference_price: u64,
    pub reference_from_oracle: bool,
    pub oracle_publish_time: i64,
    pub grid_step_bps: u16,
    pub grid_size: u8,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
}

/// How an auction finished, for building its compact result.
pub struct AuctionOutcome {
    pub state: AuctionState,
    pub clearing_index: u8,
    pub clearing_price: u64,
    pub clearing_volume: u64,
    pub demand_at_clearing: u64,
    pub supply_at_clearing: u64,
}

impl AuctionResult {
    pub fn finished(header: &AuctionHeader, bump: u8, outcome: AuctionOutcome) -> Self {
        Self {
            version: header.version,
            market: header.market,
            auction_id: header.auction_id,
            key_epoch: header.key_epoch,
            state: outcome.state,
            reference_price: header.reference_price,
            reference_from_oracle: header.reference_from_oracle,
            grid_step_bps: header.grid_step_bps,
            grid_size: header.grid_size,
            allocation_rule_version: header.allocation_rule_version,
            order_circuit_version: header.order_circuit_version,
            claim_circuit_version: header.claim_circuit_version,
            clearing_index: outcome.clearing_index,
            clearing_price: outcome.clearing_price,
            clearing_volume: outcome.clearing_volume,
            demand_at_clearing: outcome.demand_at_clearing,
            supply_at_clearing: outcome.supply_at_clearing,
            bump,
        }
    }

    pub fn from_auction(auction: &Auction) -> Self {
        let clearing = usize::from(auction.clearing_index).min(MAX_GRID_SIZE - 1);
        Self {
            version: auction.version,
            market: auction.market,
            auction_id: auction.auction_id,
            key_epoch: auction.key_epoch,
            state: auction.state,
            reference_price: auction.reference_price,
            reference_from_oracle: auction.reference_from_oracle,
            grid_step_bps: auction.grid_step_bps,
            grid_size: auction.grid_size,
            allocation_rule_version: auction.allocation_rule_version,
            order_circuit_version: auction.order_circuit_version,
            claim_circuit_version: auction.claim_circuit_version,
            clearing_index: auction.clearing_index,
            clearing_price: auction.clearing_price,
            clearing_volume: auction.clearing_volume,
            demand_at_clearing: auction.decrypted_demand[clearing],
            supply_at_clearing: auction.decrypted_supply[clearing],
            bump: auction.bump,
        }
    }

    pub fn active_grid_size(&self) -> usize {
        self.grid_size as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clearing(auction_id: u64) -> RecentClearing {
        RecentClearing {
            auction_id,
            clearing_price: 1_000 + auction_id,
            clearing_volume: 1,
        }
    }

    #[test]
    fn recent_clearings_stay_sorted_and_drop_the_oldest() {
        let mut recent = [RecentClearing::default(); RECENT_CLEARINGS];
        for auction_id in [3, 1, 2] {
            insert_recent_clearing(&mut recent, clearing(auction_id));
        }
        assert_eq!(
            recent[..4]
                .iter()
                .map(|entry| entry.auction_id)
                .collect::<Vec<_>>(),
            [3, 2, 1, 0]
        );
        assert_eq!(recent[3].clearing_price, 0);

        for auction_id in 4..=10 {
            insert_recent_clearing(&mut recent, clearing(auction_id));
        }
        let ids: Vec<_> = recent.iter().map(|entry| entry.auction_id).collect();
        assert_eq!(ids, [10, 9, 8, 7, 6, 5, 4, 3]);
        // A publication older than everything kept is ignored.
        insert_recent_clearing(&mut recent, clearing(2));
        assert_eq!(recent[RECENT_CLEARINGS - 1].auction_id, 3);
    }

    #[test]
    fn auction_header_is_the_leading_auction_layout() {
        let auction = Auction {
            version: PROTOCOL_VERSION,
            market: Pubkey::new_unique(),
            auction_id: 42,
            key_epoch: Pubkey::new_unique(),
            state: AuctionState::Pending,
            open_slot: 1,
            submission_end_slot: 2,
            aggregation_deadline_slot: 3,
            decryption_deadline_slot: 4,
            reference_price: 5,
            reference_from_oracle: true,
            oracle_publish_time: -6,
            grid_step_bps: 7,
            grid_size: 17,
            allocation_rule_version: 8,
            order_circuit_version: 9,
            claim_circuit_version: 10,
            finalized_order_count: 11,
            aggregate_buy: [[12; 64]; MAX_GRID_SIZE],
            aggregate_sell: [[13; 64]; MAX_GRID_SIZE],
            decrypted_demand: [14; MAX_GRID_SIZE],
            decrypted_supply: [15; MAX_GRID_SIZE],
            decrypted_entry_bitmap: 16,
            clearing_index: 8,
            clearing_price: 17,
            clearing_volume: 18,
            base_residual: 19,
            quote_residual: 20,
            bump: 21,
        };
        let mut data = Vec::new();
        auction.try_serialize(&mut data).unwrap();
        assert_eq!(data.len(), 8 + Auction::INIT_SPACE);
        let header = AuctionHeader::deserialize(&mut &data[8..]).unwrap();
        assert_eq!(header.market, auction.market);
        assert_eq!(header.auction_id, auction.auction_id);
        assert_eq!(header.oracle_publish_time, auction.oracle_publish_time);
        assert_eq!(header.claim_circuit_version, auction.claim_circuit_version);
        assert_eq!(data[data.len() - 1], auction.bump);
    }
}

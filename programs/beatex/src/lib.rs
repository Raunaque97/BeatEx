// Agave 3.1 marks the curve syscall wrapper as unstable/deprecated in advance
// of its v4 feature gate. The program intentionally pins the audited 3.1 API.
#![allow(deprecated)]

use {
    anchor_lang::prelude::*,
    solana_curve25519::ristretto::{add_ristretto, validate_ristretto, PodRistrettoPoint},
};

pub mod allocation;
pub mod auction;
pub mod groth16;
pub mod lifecycle;
pub mod queue;
pub mod queue_hash;
pub mod state;
mod threshold;
pub mod token2022;

pub use groth16::Groth16ProofInput;
pub use queue::{
    AggregateAccumulator, EncryptedPrivateBalance, QueueCircuitKind, QueueVerificationKey,
    QueuedOrder, QueuedOrderState, SubmissionCounter, WithdrawalKind, WithdrawalState,
};
pub use state::{Auction, AuctionState, Market, Order, OrderState};
pub use threshold::{DecryptionShareInput, DleqProofInput, VerifyThresholdDecryptionArgs};

declare_id!("2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd");

pub const COMMITTEE_SIZE: usize = 7;
pub const THRESHOLD: u8 = 3;
pub const RISTRETTO_IDENTITY: [u8; 32] = [0; 32];
pub const MAX_ENTRIES_PER_CHUNK: usize = 4;

#[program]
pub mod beatex {
    use super::*;

    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        args: InitializeMarketArgs,
    ) -> Result<()> {
        // Circuit v1 treats order quantities and prices as raw token units.
        // Fixing both lot sizes to one prevents the program and proof from
        // silently interpreting the same witness in different denominations.
        require!(
            args.base_lot_size == 1,
            BeatExError::InvalidMarketParameters
        );
        require!(
            args.quote_lot_size == 1,
            BeatExError::InvalidMarketParameters
        );
        require!(
            args.allocation_rule_version == allocation::ALLOCATION_RULE_VERSION
                && args.order_circuit_version == queue::QUEUE_CIRCUIT_VERSION
                && args.claim_circuit_version == queue::PRIVATE_CLAIM_CIRCUIT_VERSION,
            BeatExError::InvalidMarketParameters
        );
        require!(args.tick_size > 0, BeatExError::InvalidMarketParameters);
        require!(
            args.grid_radius <= state::MAX_GRID_RADIUS,
            BeatExError::InvalidMarketParameters
        );
        require!(
            args.max_order_quantity > 0
                && args.max_order_quantity <= queue::MAX_PRIVATE_ORDER_QUANTITY
                && args.max_aggregate_quantity >= args.max_order_quantity,
            BeatExError::InvalidMarketParameters
        );
        require!(
            args.submission_duration_slots > 0
                && args.aggregation_duration_slots > 0
                && args.decryption_duration_slots > 0
                && args.claim_duration_slots > 0,
            BeatExError::InvalidMarketParameters
        );
        let price_span = args
            .tick_size
            .checked_mul(args.grid_radius as u64)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        args.initial_reference_price
            .checked_sub(price_span)
            .ok_or_else(|| error!(BeatExError::InvalidMarketParameters))?;
        let upper_price = args
            .initial_reference_price
            .checked_add(price_span)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        // A two-sided contingent order can withdraw its unused reserve and
        // proceeds in the same asset. Bound that worst case to Token-2022's
        // 48-bit confidential-transfer amount.
        require!(
            u128::from(upper_price)
                .checked_mul(u128::from(args.max_order_quantity))
                .and_then(|value| value.checked_mul(2))
                .is_some_and(|value| {
                    value <= u128::from(allocation::MAX_CONFIDENTIAL_TRANSFER_AMOUNT)
                }),
            BeatExError::InvalidMarketParameters
        );

        let market = &mut ctx.accounts.market;
        market.version = state::PROTOCOL_VERSION;
        market.authority = ctx.accounts.authority.key();
        market.base_mint = ctx.accounts.base_mint.key();
        market.quote_mint = ctx.accounts.quote_mint.key();
        market.base_vault = ctx.accounts.base_vault.key();
        market.quote_vault = ctx.accounts.quote_vault.key();
        market.base_lot_size = args.base_lot_size;
        market.quote_lot_size = args.quote_lot_size;
        market.tick_size = args.tick_size;
        market.grid_radius = args.grid_radius;
        market.max_order_quantity = args.max_order_quantity;
        market.max_aggregate_quantity = args.max_aggregate_quantity;
        market.current_key_epoch = Pubkey::default();
        market.last_clearing_price = args.initial_reference_price;
        market.last_cleared_auction_sequence = 0;
        market.next_auction_id = 0;
        market.submission_duration_slots = args.submission_duration_slots;
        market.aggregation_duration_slots = args.aggregation_duration_slots;
        market.decryption_duration_slots = args.decryption_duration_slots;
        market.claim_duration_slots = args.claim_duration_slots;
        market.allocation_rule_version = args.allocation_rule_version;
        market.order_circuit_version = args.order_circuit_version;
        market.claim_circuit_version = args.claim_circuit_version;
        market.paused = false;
        market.vault_authority_bump = ctx.bumps.vault_authority;
        market.bump = ctx.bumps.market;
        Ok(())
    }

    /// Store the public half of a fixed 3-of-7 threshold key epoch.
    ///
    pub fn initialize_key_epoch(
        ctx: Context<InitializeKeyEpoch>,
        args: InitializeKeyEpochArgs,
    ) -> Result<()> {
        queue_hash::validate_committee_key(&args.babyjub_encryption_pubkey)?;
        for public_share in &args.babyjub_participant_public_shares {
            queue_hash::validate_committee_key(public_share)?;
        }
        validate_non_identity_point(&args.encryption_pubkey)?;
        validate_non_identity_point(&args.coefficient_commitments[0])?;
        validate_point(&args.coefficient_commitments[1])?;
        validate_non_identity_point(&args.coefficient_commitments[2])?;
        for public_share in &args.participant_public_shares {
            validate_non_identity_point(public_share)?;
        }
        for (position, participant) in args.participant_authorities.iter().enumerate() {
            require!(
                *participant != Pubkey::default(),
                BeatExError::InvalidCommittee
            );
            require!(
                !args.participant_authorities[..position].contains(participant),
                BeatExError::InvalidCommittee
            );
        }
        threshold::verify_key_epoch_configuration(
            &crate::ID,
            &ctx.accounts.market.key(),
            &ctx.accounts.key_epoch.key(),
            args.epoch,
            args.encryption_pubkey,
            &args.coefficient_commitments,
            &args.participant_public_shares,
            &args.key_relation_proof,
        )?;

        let key_epoch = &mut ctx.accounts.key_epoch;
        key_epoch.authority = ctx.accounts.authority.key();
        key_epoch.market = ctx.accounts.market.key();
        key_epoch.epoch = args.epoch;
        key_epoch.encryption_pubkey = args.encryption_pubkey;
        key_epoch.babyjub_encryption_pubkey = args.babyjub_encryption_pubkey;
        key_epoch.babyjub_participant_public_shares = args.babyjub_participant_public_shares;
        key_epoch.coefficient_commitments = args.coefficient_commitments;
        key_epoch.participant_public_shares = args.participant_public_shares;
        key_epoch.participant_authorities = args.participant_authorities;
        key_epoch.key_relation_proof = args.key_relation_proof;
        key_epoch.threshold = THRESHOLD;
        key_epoch.proof_transcript_version = state::PROOF_TRANSCRIPT_VERSION;
        key_epoch.activation_slot = 0;
        key_epoch.retired = false;
        key_epoch.acknowledgement_bitmap = 0;
        key_epoch.bump = ctx.bumps.key_epoch;

        Ok(())
    }

    pub fn acknowledge_key_epoch(
        ctx: Context<AcknowledgeKeyEpoch>,
        participant_index: u8,
    ) -> Result<()> {
        require!(
            (1..=COMMITTEE_SIZE as u8).contains(&participant_index),
            BeatExError::UnknownParticipant
        );
        let position = (participant_index - 1) as usize;
        require_keys_eq!(
            ctx.accounts.key_epoch.participant_authorities[position],
            ctx.accounts.participant.key(),
            BeatExError::InvalidCommittee
        );
        ctx.accounts.key_epoch.acknowledgement_bitmap |= 1_u8 << position;
        Ok(())
    }

    pub fn activate_key_epoch(ctx: Context<ActivateKeyEpoch>) -> Result<()> {
        require!(
            !ctx.accounts.key_epoch.retired,
            BeatExError::RetiredKeyEpoch
        );
        require!(
            ctx.accounts.key_epoch.acknowledgement_bitmap == 0x7f,
            BeatExError::CommitteeNotReady
        );
        let slot = Clock::get()?.slot;
        ctx.accounts.key_epoch.activation_slot = slot;
        ctx.accounts.market.current_key_epoch = ctx.accounts.key_epoch.key();
        Ok(())
    }

    pub fn open_auction(ctx: Context<OpenAuction>, auction_id: u64) -> Result<()> {
        let market = &mut ctx.accounts.market;
        require!(!market.paused, BeatExError::MarketPaused);
        require!(
            market.current_key_epoch != Pubkey::default(),
            BeatExError::NoActiveKeyEpoch
        );
        require!(auction_id == market.next_auction_id, BeatExError::AuctionId);
        let slot = Clock::get()?.slot;
        let submission_end_slot = slot
            .checked_add(market.submission_duration_slots)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        let aggregation_deadline_slot = submission_end_slot
            .checked_add(market.aggregation_duration_slots)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        let decryption_deadline_slot = aggregation_deadline_slot
            .checked_add(market.decryption_duration_slots)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        let claim_deadline_slot = decryption_deadline_slot
            .checked_add(market.claim_duration_slots)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;

        let auction = &mut ctx.accounts.auction;
        auction.version = state::PROTOCOL_VERSION;
        auction.market = market.key();
        auction.auction_id = auction_id;
        auction.key_epoch = market.current_key_epoch;
        auction.state = AuctionState::Open;
        auction.open_slot = slot;
        auction.submission_end_slot = submission_end_slot;
        auction.aggregation_deadline_slot = aggregation_deadline_slot;
        auction.decryption_deadline_slot = decryption_deadline_slot;
        auction.claim_deadline_slot = claim_deadline_slot;
        auction.reference_price = market.last_clearing_price;
        auction.tick_size = market.tick_size;
        auction.grid_size = market.grid_radius * 2 + 1;
        auction.allocation_rule_version = market.allocation_rule_version;
        auction.order_circuit_version = market.order_circuit_version;
        auction.claim_circuit_version = market.claim_circuit_version;
        auction.clearing_index = market.grid_radius;
        auction.clearing_price = market.last_clearing_price;
        auction.bump = ctx.bumps.auction;

        market.next_auction_id = market
            .next_auction_id
            .checked_add(1)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        Ok(())
    }

    pub fn initialize_queue_verification_key(
        ctx: Context<InitializeQueueVerificationKey>,
        args: InitializeQueueVerificationKeyArgs,
    ) -> Result<()> {
        require!(
            args.circuit_version == queue::circuit_version(args.circuit_kind),
            BeatExError::InvalidCircuitVersion
        );
        let key = &mut ctx.accounts.verification_key;
        key.version = state::PROTOCOL_VERSION;
        key.market = ctx.accounts.market.key();
        key.circuit_kind = args.circuit_kind;
        key.circuit_version = args.circuit_version;
        key.alpha_g1 = args.alpha_g1;
        key.beta_g2 = args.beta_g2;
        key.gamma_g2 = args.gamma_g2;
        key.delta_g2 = args.delta_g2;
        key.ic = args.ic;
        key.bump = ctx.bumps.verification_key;
        Ok(())
    }

    pub fn initialize_queue_state(ctx: Context<InitializeQueueState>) -> Result<()> {
        require!(
            ctx.accounts.auction.state == AuctionState::Open,
            BeatExError::AuctionState
        );
        require!(
            ctx.accounts.auction.grid_size as usize == queue::QUEUE_PRICE_LEVELS,
            BeatExError::UnsupportedQueueGrid
        );
        let counter = &mut ctx.accounts.submission_counter;
        counter.version = state::PROTOCOL_VERSION;
        counter.auction = ctx.accounts.auction.key();
        counter.accepted_order_count = 0;
        counter.bump = ctx.bumps.submission_counter;

        let accumulator = &mut ctx.accounts.accumulator;
        accumulator.version = state::PROTOCOL_VERSION;
        accumulator.auction = ctx.accounts.auction.key();
        accumulator.folded_order_count = 0;
        accumulator.fold_revision = 0;
        accumulator.ciphertexts = queue_hash::order_bytes(&queue_hash::identity_order());
        accumulator.ciphertext_digest = queue_hash::order_ciphertext_digest(
            &ctx.accounts.auction.key(),
            &accumulator.key(),
            &accumulator.ciphertexts,
        )?;
        accumulator.bump = ctx.bumps.accumulator;
        Ok(())
    }

    /// Create an empty encrypted balance. Credits are added only through
    /// `deposit_collateral`, which atomically moves Token-2022 funds and proves
    /// the corresponding Baby Jubjub balance transition.
    pub fn initialize_private_balance(
        ctx: Context<InitializePrivateBalance>,
        args: InitializePrivateBalanceArgs,
    ) -> Result<()> {
        let balance = &mut ctx.accounts.private_balance;
        balance.version = state::PROTOCOL_VERSION;
        balance.market = ctx.accounts.market.key();
        balance.owner = ctx.accounts.owner.key();
        balance.key_epoch = ctx.accounts.key_epoch.key();
        let identity = queue_hash::identity_ciphertext();
        require!(
            args.encrypted_base == identity && args.encrypted_quote == identity,
            BeatExError::InvalidInitialBalance
        );
        balance.encrypted_base = identity;
        balance.encrypted_quote = identity;
        balance.deposited_base = 0;
        balance.deposited_quote = 0;
        balance.balance_digest = [0; 32];
        balance.revision = 0;
        balance.bump = ctx.bumps.private_balance;
        Ok(())
    }

    /// Deposit public demo collateral (for example tBTC/tUSD) and credit the
    /// exact amounts to the trader's encrypted balances. The Token-2022 CPIs,
    /// Groth16 proof, counters and ciphertext update are one atomic operation.
    pub fn deposit_collateral(
        ctx: Context<DepositCollateral>,
        args: DepositCollateralArgs,
    ) -> Result<()> {
        require!(
            args.base_amount != 0 || args.quote_amount != 0,
            BeatExError::EmptyDeposit
        );
        require!(
            ctx.accounts.deposit_verification_key.circuit_kind == QueueCircuitKind::BalanceDeposit
                && ctx.accounts.deposit_verification_key.circuit_version
                    == queue::QUEUE_CIRCUIT_VERSION,
            BeatExError::InvalidCircuitVersion
        );

        let balance = &ctx.accounts.private_balance;
        let digest =
            queue_hash::balance_deposit_statement_digest(&queue_hash::BalanceDepositStatement {
                market: &ctx.accounts.market.key(),
                owner: &ctx.accounts.owner.key(),
                key_epoch: &ctx.accounts.key_epoch.key(),
                committee_key: ctx.accounts.key_epoch.babyjub_encryption_pubkey,
                old_revision: balance.revision,
                base_amount: args.base_amount,
                quote_amount: args.quote_amount,
                old_base: &balance.encrypted_base,
                old_quote: &balance.encrypted_quote,
                new_base: &args.new_encrypted_base,
                new_quote: &args.new_encrypted_quote,
            })?;
        groth16::verify_queue_proof(&args.proof, &digest, &ctx.accounts.deposit_verification_key)?;

        transfer_demo_deposit_asset(
            args.base_amount,
            &ctx.accounts.owner.to_account_info(),
            &ctx.accounts.base_source.to_account_info(),
            &ctx.accounts.base_mint.to_account_info(),
            &ctx.accounts.base_vault.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            &ctx.accounts.market.base_mint,
            &ctx.accounts.market.base_vault,
            &ctx.accounts.market.key(),
            ctx.accounts.market.vault_authority_bump,
            &ctx.accounts.key_epoch.encryption_pubkey,
        )?;
        transfer_demo_deposit_asset(
            args.quote_amount,
            &ctx.accounts.owner.to_account_info(),
            &ctx.accounts.quote_source.to_account_info(),
            &ctx.accounts.quote_mint.to_account_info(),
            &ctx.accounts.quote_vault.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            &ctx.accounts.market.quote_mint,
            &ctx.accounts.market.quote_vault,
            &ctx.accounts.market.key(),
            ctx.accounts.market.vault_authority_bump,
            &ctx.accounts.key_epoch.encryption_pubkey,
        )?;

        let balance = &mut ctx.accounts.private_balance;
        balance.encrypted_base = args.new_encrypted_base;
        balance.encrypted_quote = args.new_encrypted_quote;
        balance.deposited_base = balance
            .deposited_base
            .checked_add(args.base_amount)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        balance.deposited_quote = balance
            .deposited_quote
            .checked_add(args.quote_amount)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        balance.balance_digest = digest;
        balance.revision = balance
            .revision
            .checked_add(1)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        Ok(())
    }

    /// Accept one independent encrypted order. No aggregate ciphertext is read
    /// or written; only the lightweight accepted-order counter is contended.
    pub fn submit_queued_order(
        ctx: Context<SubmitQueuedOrder>,
        args: SubmitQueuedOrderArgs,
    ) -> Result<()> {
        require!(
            args.ciphertexts.len()
                == queue::QUEUED_CIPHERTEXT_COUNT * queue::ELGAMAL_CIPHERTEXT_BYTES,
            BeatExError::UnsupportedQueueGrid
        );
        let slot = Clock::get()?.slot;
        let queue_accounts = &ctx.accounts.queue_accounts;
        require_keys_eq!(
            queue_accounts.private_balance.owner,
            ctx.accounts.owner.key(),
            BeatExError::OrderState
        );
        let auction = &queue_accounts.auction;
        let deadlines = queue_deadlines(auction);
        let (sequence, next_count) = queue::accept_order(
            queue_phase(auction.state)?,
            slot,
            deadlines,
            queue_accounts.submission_counter.accepted_order_count,
        )
        .map_err(|_| error!(BeatExError::QueueInvariant))?;
        require!(
            auction.grid_size as usize == queue::QUEUE_PRICE_LEVELS,
            BeatExError::UnsupportedQueueGrid
        );
        require!(
            queue_accounts.order_verification_key.circuit_kind == QueueCircuitKind::OrderSubmission
                && queue_accounts.order_verification_key.circuit_version
                    == auction.order_circuit_version,
            BeatExError::InvalidCircuitVersion
        );

        let digest = queue_hash::order_statement_digest(&queue_hash::OrderStatement {
            auction: &auction.key(),
            owner: &ctx.accounts.owner.key(),
            client_order_nonce: args.client_order_nonce,
            order_commitment: args.order_commitment,
            committee_key: queue_accounts.key_epoch.babyjub_encryption_pubkey,
            reference_price: auction.reference_price,
            tick_size: auction.tick_size,
            ciphertexts: &args.ciphertexts,
            old_base: &queue_accounts.private_balance.encrypted_base,
            old_quote: &queue_accounts.private_balance.encrypted_quote,
            new_base: &args.new_encrypted_base,
            new_quote: &args.new_encrypted_quote,
        })?;
        groth16::verify_queue_proof(&args.proof, &digest, &queue_accounts.order_verification_key)?;

        let order = &mut ctx.accounts.order;
        order.version = state::PROTOCOL_VERSION;
        order.market = queue_accounts.market.key();
        order.auction = auction.key();
        order.owner = ctx.accounts.owner.key();
        order.funder = ctx.accounts.owner.key();
        order.client_order_nonce = args.client_order_nonce;
        order.sequence = sequence;
        order.state = QueuedOrderState::Accepted;
        order.withdrawal_state = WithdrawalState::Pending;
        order.terminal_nullifier = [0; queue::QUEUE_DIGEST_BYTES];
        order.ciphertexts = args.ciphertexts;
        order.ciphertext_digest =
            queue_hash::order_ciphertext_digest(&auction.key(), &order.key(), &order.ciphertexts)?;
        order.order_commitment = args.order_commitment;
        order.order_circuit_version = auction.order_circuit_version;
        order.bump = ctx.bumps.order;

        let balance = &mut ctx.accounts.queue_accounts.private_balance;
        balance.encrypted_base = args.new_encrypted_base;
        balance.encrypted_quote = args.new_encrypted_quote;
        balance.balance_digest = digest;
        balance.revision = balance
            .revision
            .checked_add(1)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        ctx.accounts
            .queue_accounts
            .submission_counter
            .accepted_order_count = next_count;
        Ok(())
    }

    /// Verify and apply a permissionless aggregate fold. The active queued
    /// orders are supplied as 1..=4 writable remaining accounts in ascending
    /// public-key order.
    pub fn fold_queued_orders(
        ctx: Context<FoldQueuedOrders>,
        args: FoldQueuedOrdersArgs,
    ) -> Result<()> {
        require!(
            args.new_aggregate.len()
                == queue::QUEUED_CIPHERTEXT_COUNT * queue::ELGAMAL_CIPHERTEXT_BYTES,
            BeatExError::UnsupportedQueueGrid
        );
        require!(
            ctx.accounts.fold_verification_key.circuit_kind == QueueCircuitKind::AggregateFold
                && ctx.accounts.fold_verification_key.circuit_version
                    == queue::QUEUE_CIRCUIT_VERSION,
            BeatExError::InvalidCircuitVersion
        );
        let batch_size = ctx.remaining_accounts.len();
        require!(
            (1..=queue_hash::FOLD_BATCH_SIZE).contains(&batch_size),
            BeatExError::InvalidFoldBatch
        );

        let mut fold_orders = Vec::with_capacity(batch_size);
        let mut previous = None;
        for info in ctx.remaining_accounts {
            require!(info.is_writable, BeatExError::InvalidFoldBatch);
            require_keys_eq!(*info.owner, crate::ID, BeatExError::InvalidFoldBatch);
            if let Some(previous) = previous {
                require!(previous < *info.key, BeatExError::InvalidFoldBatch);
            }
            let order = Account::<QueuedOrder>::try_from(info)?;
            require_keys_eq!(
                order.auction,
                ctx.accounts.auction.key(),
                BeatExError::InvalidFoldBatch
            );
            require!(
                order.state == QueuedOrderState::Accepted,
                BeatExError::OrderAlreadyAggregated
            );
            fold_orders.push(queue_hash::FoldOrderInput {
                order: *info.key,
                ciphertext_digest: order.ciphertext_digest,
            });
            previous = Some(*info.key);
        }

        let (next_count, next_revision) = queue::advance_fold(
            queue_phase(ctx.accounts.auction.state)?,
            Clock::get()?.slot,
            queue_deadlines(&ctx.accounts.auction),
            ctx.accounts.submission_counter.accepted_order_count,
            ctx.accounts.accumulator.folded_order_count,
            ctx.accounts.accumulator.fold_revision,
            batch_size as u64,
        )
        .map_err(|_| error!(BeatExError::QueueInvariant))?;
        let digest = queue_hash::fold_statement_digest(&queue_hash::FoldStatement {
            auction: &ctx.accounts.auction.key(),
            old_folded_count: ctx.accounts.accumulator.folded_order_count,
            active_orders: &fold_orders,
            old_aggregate: &ctx.accounts.accumulator.ciphertexts,
            new_aggregate: &args.new_aggregate,
        })?;
        groth16::verify_queue_proof(&args.proof, &digest, &ctx.accounts.fold_verification_key)?;

        for info in ctx.remaining_accounts {
            let mut order = Account::<QueuedOrder>::try_from(info)?;
            order.state = queue::fold_order_state(order.state)
                .map_err(|_| error!(BeatExError::OrderAlreadyAggregated))?;
            order.exit(&crate::ID)?;
        }
        let accumulator = &mut ctx.accounts.accumulator;
        accumulator.ciphertexts = args.new_aggregate;
        accumulator.ciphertext_digest = digest;
        accumulator.folded_order_count = next_count;
        accumulator.fold_revision = next_revision;
        Ok(())
    }

    pub fn finalize_queue_aggregation(ctx: Context<FinalizeQueueAggregation>) -> Result<()> {
        queue::authorize_settlement(
            queue_phase(ctx.accounts.auction.state)?,
            Clock::get()?.slot,
            queue_deadlines(&ctx.accounts.auction),
            ctx.accounts.submission_counter.accepted_order_count,
            ctx.accounts.accumulator.folded_order_count,
        )
        .map_err(|_| error!(BeatExError::AggregationIncomplete))?;
        ctx.accounts.auction.state = AuctionState::Decrypting;
        Ok(())
    }

    /// Publish the exact per-price aggregate buy and sell quantities after an
    /// arbitrary registered 3-of-7 Baby Jubjub quorum has decrypted the frozen
    /// accumulator. Decryption shares and DLEQ transcripts remain private
    /// witnesses; the proof binds the selected participant public keys.
    pub fn publish_aggregate_curves(
        ctx: Context<PublishAggregateCurves>,
        args: PublishAggregateCurvesArgs,
    ) -> Result<()> {
        let auction = &ctx.accounts.auction;
        require!(
            auction.state == AuctionState::Decrypting,
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot < auction.decryption_deadline_slot,
            BeatExError::DeadlineReached
        );
        require!(
            auction.grid_size as usize == queue::QUEUE_PRICE_LEVELS,
            BeatExError::UnsupportedQueueGrid
        );
        require!(
            ctx.accounts.submission_counter.accepted_order_count > 0
                && ctx.accounts.submission_counter.accepted_order_count
                    == ctx.accounts.accumulator.folded_order_count,
            BeatExError::AggregationIncomplete
        );
        require!(
            ctx.accounts.publication_verification_key.circuit_kind
                == QueueCircuitKind::AggregatePublication
                && ctx.accounts.publication_verification_key.circuit_version
                    == queue::AGGREGATE_PUBLICATION_CIRCUIT_VERSION,
            BeatExError::InvalidCircuitVersion
        );
        require!(
            args.participant_ids[0] >= 1
                && args.participant_ids[0] < args.participant_ids[1]
                && args.participant_ids[1] < args.participant_ids[2]
                && args.participant_ids[2] <= COMMITTEE_SIZE as u8,
            BeatExError::InvalidCommittee
        );
        let grid = auction::PriceGrid {
            reference_price: auction.reference_price,
            tick_size: auction.tick_size,
            level_count: auction.grid_size as u16,
        };
        auction::validate_aggregate_curves(&grid, &args.demand, &args.supply)
            .map_err(|_| error!(BeatExError::InvalidCurves))?;
        require!(
            args.demand
                .iter()
                .chain(&args.supply)
                .all(|quantity| *quantity <= ctx.accounts.market.max_aggregate_quantity),
            BeatExError::QuantityBound
        );

        let digest = queue_hash::aggregate_publication_statement_digest(
            &queue_hash::AggregatePublicationStatement {
                auction: &ctx.accounts.auction.key(),
                accepted_order_count: ctx.accounts.submission_counter.accepted_order_count,
                committee_key: ctx.accounts.key_epoch.babyjub_encryption_pubkey,
                participant_ids: args.participant_ids,
                selected_participant_public_shares: args.participant_ids.map(|participant_id| {
                    ctx.accounts.key_epoch.babyjub_participant_public_shares
                        [usize::from(participant_id - 1)]
                }),
                aggregate: &ctx.accounts.accumulator.ciphertexts,
                demand: &args.demand,
                supply: &args.supply,
            },
        )?;
        groth16::verify_queue_proof(
            &args.proof,
            &digest,
            &ctx.accounts.publication_verification_key,
        )?;

        let auction = &mut ctx.accounts.auction;
        auction.decrypted_demand = args.demand;
        auction.decrypted_supply = args.supply;
        auction.decrypted_entry_bitmap = (1_u128 << queue::QUEUED_CIPHERTEXT_COUNT) - 1;
        auction.state = AuctionState::Decrypted;
        Ok(())
    }

    pub fn cancel_stalled_aggregation(ctx: Context<FinalizeQueueAggregation>) -> Result<()> {
        queue::authorize_aggregation_timeout(
            queue_phase(ctx.accounts.auction.state)?,
            Clock::get()?.slot,
            queue_deadlines(&ctx.accounts.auction),
            ctx.accounts.submission_counter.accepted_order_count,
            ctx.accounts.accumulator.folded_order_count,
        )
        .map_err(|_| error!(BeatExError::QueueInvariant))?;
        ctx.accounts.auction.state = AuctionState::Cancelled;
        Ok(())
    }

    pub fn seal_auction(ctx: Context<MutateAuction>) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            auction.state == AuctionState::Open,
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot >= auction.submission_end_slot,
            BeatExError::DeadlineNotReached
        );
        auction.state = AuctionState::Sealed;
        Ok(())
    }

    pub fn cancel_expired_auction(ctx: Context<MutateAuction>) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            matches!(
                auction.state,
                AuctionState::Open
                    | AuctionState::Sealed
                    | AuctionState::Decrypting
                    | AuctionState::Decrypted
            ),
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot >= auction.decryption_deadline_slot,
            BeatExError::DeadlineNotReached
        );
        auction.state = AuctionState::Cancelled;
        Ok(())
    }

    /// Test/demo-only plaintext ingress used to exercise the complete auction
    /// state machine before the encrypted collection proof is wired in.
    pub fn submit_test_plaintext_curves(
        ctx: Context<SubmitTestPlaintextCurves>,
        demand: Vec<u64>,
        supply: Vec<u64>,
    ) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            matches!(
                auction.state,
                AuctionState::Sealed | AuctionState::Decrypting
            ),
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot < auction.decryption_deadline_slot,
            BeatExError::DeadlineReached
        );
        let grid = auction::PriceGrid {
            reference_price: auction.reference_price,
            tick_size: auction.tick_size,
            level_count: auction.grid_size as u16,
        };
        auction::validate_aggregate_curves(&grid, &demand, &supply)
            .map_err(|_| error!(BeatExError::InvalidCurves))?;
        require!(
            demand
                .iter()
                .chain(&supply)
                .all(|quantity| *quantity <= ctx.accounts.market.max_aggregate_quantity),
            BeatExError::QuantityBound
        );

        let grid_size = auction.active_grid_size();
        auction.decrypted_demand[..grid_size].copy_from_slice(&demand);
        auction.decrypted_supply[..grid_size].copy_from_slice(&supply);
        auction.decrypted_entry_bitmap = (1_u128 << (grid_size * 2)) - 1;
        auction.state = AuctionState::Decrypted;
        Ok(())
    }

    pub fn clear_auction(ctx: Context<ClearAuction>) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            auction.state == AuctionState::Decrypted,
            BeatExError::AuctionState
        );
        require!(
            auction.all_entries_decrypted(),
            BeatExError::IncompleteDecryption
        );
        require!(
            Clock::get()?.slot < auction.decryption_deadline_slot,
            BeatExError::DeadlineReached
        );
        let grid_size = auction.active_grid_size();
        let grid = auction::PriceGrid {
            reference_price: auction.reference_price,
            tick_size: auction.tick_size,
            level_count: auction.grid_size as u16,
        };
        let result = auction::clear_auction(
            &grid,
            &auction.decrypted_demand[..grid_size],
            &auction.decrypted_supply[..grid_size],
        )
        .map_err(|_| error!(BeatExError::InvalidCurves))?;
        auction.clearing_index = result.index as u8;
        auction.clearing_price = result.price;
        auction.clearing_volume = result.executable_volume;
        auction.state = AuctionState::ClaimsOpen;

        let sequence = auction
            .auction_id
            .checked_add(1)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        if sequence > ctx.accounts.market.last_cleared_auction_sequence {
            ctx.accounts.market.last_cleared_auction_sequence = sequence;
            ctx.accounts.market.last_clearing_price = result.price;
        }
        Ok(())
    }

    pub fn initialize_order(
        ctx: Context<InitializeOrder>,
        args: InitializeOrderArgs,
    ) -> Result<()> {
        let auction = &ctx.accounts.auction;
        require!(
            auction.state == AuctionState::Open,
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot < auction.submission_end_slot,
            BeatExError::DeadlineReached
        );
        let order = &mut ctx.accounts.order;
        order.version = state::PROTOCOL_VERSION;
        order.market = ctx.accounts.market.key();
        order.auction = auction.key();
        order.owner = ctx.accounts.owner.key();
        order.funder = ctx.accounts.owner.key();
        order.client_order_nonce = args.client_order_nonce;
        order.state = OrderState::Writing;
        order.grid_size = auction.grid_size;
        order.base_collateral_commitment = args.base_collateral_commitment;
        order.quote_collateral_commitment = args.quote_collateral_commitment;
        order.order_circuit_version = ctx.accounts.market.order_circuit_version;
        order.claim_nullifier_commitment = args.claim_nullifier_commitment;
        order.aggregated = false;
        order.bump = ctx.bumps.order;
        Ok(())
    }

    pub fn write_order_chunk(
        ctx: Context<WriteOrderChunk>,
        entries: Vec<EncryptedOrderEntry>,
    ) -> Result<()> {
        let auction = &ctx.accounts.auction;
        let order = &mut ctx.accounts.order;
        require!(
            auction.state == AuctionState::Open,
            BeatExError::AuctionState
        );
        require!(order.state == OrderState::Writing, BeatExError::OrderState);
        require!(
            Clock::get()?.slot < auction.submission_end_slot,
            BeatExError::DeadlineReached
        );
        require!(
            !entries.is_empty() && entries.len() <= MAX_ENTRIES_PER_CHUNK,
            BeatExError::ChunkSize
        );

        let mut incoming_bitmap = 0_u128;
        for entry in &entries {
            let index = entry.index as usize;
            require!(index < order.grid_size as usize, BeatExError::GridIndex);
            let bit = 1_u128 << index;
            require!(incoming_bitmap & bit == 0, BeatExError::DuplicateEntry);
            require!(
                order.written_entry_bitmap & bit == 0,
                BeatExError::DuplicateEntry
            );
            validate_ciphertext(&entry.buy)?;
            validate_ciphertext(&entry.sell)?;
            incoming_bitmap |= bit;
        }

        for entry in entries {
            let index = entry.index as usize;
            order.encrypted_buy[index] = entry.buy;
            order.encrypted_sell[index] = entry.sell;
        }
        order.written_entry_bitmap |= incoming_bitmap;
        Ok(())
    }

    /// Temporary authority-gated finalization endpoint. It exercises immutable
    /// ciphertext aggregation while the Groth16 order-validity verifier is
    /// still being integrated; it must be removed before an untrusted demo.
    pub fn finalize_test_order(ctx: Context<FinalizeTestOrder>) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        let order = &mut ctx.accounts.order;
        require!(
            auction.state == AuctionState::Open,
            BeatExError::AuctionState
        );
        require!(order.state == OrderState::Writing, BeatExError::OrderState);
        require!(!order.aggregated, BeatExError::OrderAlreadyAggregated);
        require!(
            Clock::get()?.slot < auction.submission_end_slot,
            BeatExError::DeadlineReached
        );
        let grid_size = order.grid_size as usize;
        let required_bitmap = (1_u128 << grid_size) - 1;
        require!(
            order.written_entry_bitmap & required_bitmap == required_bitmap,
            BeatExError::IncompleteOrder
        );

        for index in 0..grid_size {
            auction.aggregate_buy[index] =
                add_ciphertexts(&auction.aggregate_buy[index], &order.encrypted_buy[index])?;
            auction.aggregate_sell[index] =
                add_ciphertexts(&auction.aggregate_sell[index], &order.encrypted_sell[index])?;
        }
        auction.finalized_order_count = auction
            .finalized_order_count
            .checked_add(1)
            .ok_or_else(|| error!(BeatExError::ArithmeticOverflow))?;
        order.aggregated = true;
        order.state = OrderState::Finalized;
        Ok(())
    }

    pub fn close_unfinalized_order(_ctx: Context<CloseUnfinalizedOrder>) -> Result<()> {
        Ok(())
    }

    pub fn finalize_decrypted_entry(
        ctx: Context<FinalizeDecryptedEntry>,
        args: VerifyThresholdDecryptionArgs,
    ) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            matches!(
                auction.state,
                AuctionState::Sealed | AuctionState::Decrypting
            ),
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot < auction.decryption_deadline_slot,
            BeatExError::DeadlineReached
        );
        require!(
            args.auction_id == auction.auction_id,
            BeatExError::AuctionId
        );
        let position = args.position as usize;
        require!(
            position < auction.active_grid_size(),
            BeatExError::GridIndex
        );
        let expected_ciphertext = if args.side == 0 {
            auction.aggregate_buy[position]
        } else if args.side == 1 {
            auction.aggregate_sell[position]
        } else {
            return err!(BeatExError::InvalidSide);
        };
        require!(
            args.ciphertext == expected_ciphertext,
            BeatExError::CiphertextMismatch
        );

        threshold::verify_threshold_decryption(
            &crate::ID,
            &ctx.accounts.key_epoch.key(),
            &ctx.accounts.key_epoch,
            &args,
        )?;
        require!(
            args.claimed_amount <= ctx.accounts.market.max_aggregate_quantity,
            BeatExError::QuantityBound
        );
        let bitmap_position = position + args.side as usize * auction.active_grid_size();
        let bit = 1_u128 << bitmap_position;
        require!(
            auction.decrypted_entry_bitmap & bit == 0,
            BeatExError::DuplicateDecryption
        );
        if args.side == 0 {
            auction.decrypted_demand[position] = args.claimed_amount;
        } else {
            auction.decrypted_supply[position] = args.claimed_amount;
        }
        auction.decrypted_entry_bitmap |= bit;
        auction.state = AuctionState::Decrypting;
        Ok(())
    }

    pub fn finalize_decryption(ctx: Context<FinalizeDecryption>) -> Result<()> {
        let auction = &mut ctx.accounts.auction;
        require!(
            auction.state == AuctionState::Decrypting,
            BeatExError::AuctionState
        );
        require!(
            Clock::get()?.slot < auction.decryption_deadline_slot,
            BeatExError::DeadlineReached
        );
        require!(
            auction.all_entries_decrypted(),
            BeatExError::IncompleteDecryption
        );
        auction.state = AuctionState::Decrypted;
        Ok(())
    }

    pub fn apply_vault_pending_balance(
        ctx: Context<ApplyVaultPendingBalance>,
        is_base_vault: bool,
        expected_pending_balance_credit_counter: u64,
        new_decryptable_available_balance: [u8; token2022::AE_CIPHERTEXT_LEN],
    ) -> Result<()> {
        let market = &ctx.accounts.market;
        let expected_mint = if is_base_vault {
            market.base_mint
        } else {
            market.quote_mint
        };
        let expected_vault = if is_base_vault {
            market.base_vault
        } else {
            market.quote_vault
        };
        require_keys_eq!(
            ctx.accounts.mint.key(),
            expected_mint,
            BeatExError::InvalidConfidentialVault
        );
        require_keys_eq!(
            ctx.accounts.vault.key(),
            expected_vault,
            BeatExError::InvalidConfidentialVault
        );
        token2022::validate_confidential_vault(
            &ctx.accounts.vault.try_borrow_data()?,
            &expected_mint,
            &ctx.accounts.vault_authority.key(),
            &ctx.accounts.key_epoch.encryption_pubkey,
        )?;

        let market_key = market.key();
        let bump = [market.vault_authority_bump];
        let signer_seeds: &[&[u8]] = &[b"vault-authority", market_key.as_ref(), &bump];
        token2022::invoke_apply_pending_balance(
            &ctx.accounts.token_program.to_account_info(),
            &ctx.accounts.vault.to_account_info(),
            &ctx.accounts.vault_authority.to_account_info(),
            expected_pending_balance_credit_counter,
            new_decryptable_available_balance,
            &[signer_seeds],
        )
    }

    /// Authority-gated Phase-0 settlement spike.
    ///
    /// This performs both confidential vault transfers atomically and marks
    /// the order terminal only after both Token-2022 CPIs succeed. The market
    /// authority gate is deliberate: the production replacement must verify
    /// the private claim/refund Groth16 proof before permitting an untrusted
    /// caller to select destinations and transfer proof contexts.
    pub fn settle_test_order(
        ctx: Context<SettleTestOrder>,
        is_refund: bool,
        base_transfer: ConfidentialTransferArgs,
        quote_transfer: ConfidentialTransferArgs,
    ) -> Result<()> {
        let auction = &ctx.accounts.auction;
        let order = &ctx.accounts.order;
        require!(
            order.state == OrderState::Finalized,
            BeatExError::OrderState
        );
        if is_refund {
            require!(
                auction.state == AuctionState::Cancelled,
                BeatExError::WithdrawalNotAllowed
            );
        } else {
            require!(
                auction.state == AuctionState::ClaimsOpen,
                BeatExError::WithdrawalNotAllowed
            );
            require!(
                Clock::get()?.slot < auction.claim_deadline_slot,
                BeatExError::DeadlineReached
            );
        }

        validate_test_settlement_accounts(&ctx)?;

        let market_key = ctx.accounts.market.key();
        let bump = [ctx.accounts.market.vault_authority_bump];
        let signer_seeds: &[&[u8]] = &[b"vault-authority", market_key.as_ref(), &bump];
        let token_program = ctx.accounts.token_program.to_account_info();
        let vault_authority = ctx.accounts.vault_authority.to_account_info();

        token2022::invoke_confidential_transfer(
            &token_program,
            &ctx.accounts.base_transfer_accounts.vault.to_account_info(),
            &ctx.accounts.base_transfer_accounts.mint.to_account_info(),
            &ctx.accounts
                .base_transfer_accounts
                .destination
                .to_account_info(),
            &vault_authority,
            &ctx.accounts
                .base_transfer_accounts
                .equality_proof
                .to_account_info(),
            &ctx.accounts
                .base_transfer_accounts
                .validity_proof
                .to_account_info(),
            &ctx.accounts
                .base_transfer_accounts
                .range_proof
                .to_account_info(),
            base_transfer.new_source_decryptable_available_balance,
            base_transfer.auditor_ciphertext_lo,
            base_transfer.auditor_ciphertext_hi,
            &[signer_seeds],
        )?;
        token2022::invoke_confidential_transfer(
            &token_program,
            &ctx.accounts.quote_transfer_accounts.vault.to_account_info(),
            &ctx.accounts.quote_transfer_accounts.mint.to_account_info(),
            &ctx.accounts
                .quote_transfer_accounts
                .destination
                .to_account_info(),
            &vault_authority,
            &ctx.accounts
                .quote_transfer_accounts
                .equality_proof
                .to_account_info(),
            &ctx.accounts
                .quote_transfer_accounts
                .validity_proof
                .to_account_info(),
            &ctx.accounts
                .quote_transfer_accounts
                .range_proof
                .to_account_info(),
            quote_transfer.new_source_decryptable_available_balance,
            quote_transfer.auditor_ciphertext_lo,
            quote_transfer.auditor_ciphertext_hi,
            &[signer_seeds],
        )?;

        ctx.accounts.order.state = if is_refund {
            OrderState::Refunded
        } else {
            OrderState::Claimed
        };
        Ok(())
    }

    /// Settle one folded queued order without revealing its price-level
    /// quantities or reserves. The caller is only a relayer: the Groth16 proof
    /// binds the private allocation to both Token-2022 transfer contexts.
    pub fn claim_queued_order(
        ctx: Context<PrivateWithdrawal>,
        args: PrivateWithdrawalArgs,
    ) -> Result<()> {
        execute_private_withdrawal(ctx, WithdrawalKind::Claim, args)
    }

    /// Return the private reserves of an accepted or folded order after
    /// permissionless auction cancellation. The refund circuit uses a distinct
    /// verification key and proves that the transfer amounts equal the original
    /// committed reserves.
    pub fn refund_queued_order(
        ctx: Context<PrivateWithdrawal>,
        args: PrivateWithdrawalArgs,
    ) -> Result<()> {
        execute_private_withdrawal(ctx, WithdrawalKind::Refund, args)
    }

    /// Verify three DLEQ-proven shares and a caller-supplied plaintext against
    /// one Solana-compatible twisted-ElGamal ciphertext.
    pub fn verify_threshold_decryption(
        ctx: Context<VerifyThresholdDecryption>,
        args: VerifyThresholdDecryptionArgs,
    ) -> Result<()> {
        threshold::verify_threshold_decryption(
            &crate::ID,
            &ctx.accounts.key_epoch.key(),
            &ctx.accounts.key_epoch,
            &args,
        )
    }

    /// Phase-0 BN254/Groth16 syscall and serialization feasibility endpoint.
    /// This fixture circuit is intentionally unable to authorize any state
    /// change or token movement.
    pub fn verify_groth16_pipeline(
        _ctx: Context<VerifyGroth16Pipeline>,
        circuit_version: u16,
        proof: Groth16ProofInput,
    ) -> Result<()> {
        require!(
            circuit_version == groth16::PIPELINE_CIRCUIT_VERSION,
            BeatExError::InvalidCircuitVersion
        );
        groth16::verify_pipeline_proof(&proof)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeQueueVerificationKeyArgs {
    pub circuit_kind: QueueCircuitKind,
    pub circuit_version: u16,
    pub alpha_g1: [u8; 64],
    pub beta_g2: [u8; 128],
    pub gamma_g2: [u8; 128],
    pub delta_g2: [u8; 128],
    pub ic: [[u8; 64]; 2],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializePrivateBalanceArgs {
    pub encrypted_base: [u8; 64],
    pub encrypted_quote: [u8; 64],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct DepositCollateralArgs {
    pub base_amount: u64,
    pub quote_amount: u64,
    pub new_encrypted_base: queue::EncryptedValue,
    pub new_encrypted_quote: queue::EncryptedValue,
    pub proof: Groth16ProofInput,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SubmitQueuedOrderArgs {
    pub client_order_nonce: u64,
    pub order_commitment: [u8; 32],
    pub ciphertexts: Vec<u8>,
    pub new_encrypted_base: queue::EncryptedValue,
    pub new_encrypted_quote: queue::EncryptedValue,
    pub proof: Groth16ProofInput,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct FoldQueuedOrdersArgs {
    pub new_aggregate: Vec<u8>,
    pub proof: Groth16ProofInput,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PublishAggregateCurvesArgs {
    pub participant_ids: [u8; 3],
    pub demand: [u64; queue::QUEUE_PRICE_LEVELS],
    pub supply: [u64; queue::QUEUE_PRICE_LEVELS],
    pub proof: Groth16ProofInput,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeMarketArgs {
    pub market_nonce: u64,
    pub base_lot_size: u64,
    pub quote_lot_size: u64,
    pub tick_size: u64,
    pub grid_radius: u8,
    pub max_order_quantity: u64,
    pub max_aggregate_quantity: u64,
    pub initial_reference_price: u64,
    pub submission_duration_slots: u64,
    pub aggregation_duration_slots: u64,
    pub decryption_duration_slots: u64,
    pub claim_duration_slots: u64,
    pub allocation_rule_version: u16,
    pub order_circuit_version: u16,
    pub claim_circuit_version: u16,
}

#[derive(Accounts)]
#[instruction(args: InitializeQueueVerificationKeyArgs)]
pub struct InitializeQueueVerificationKey<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(
        init,
        payer = authority,
        space = 8 + QueueVerificationKey::INIT_SPACE,
        seeds = [
            b"queue-vk",
            market.key().as_ref(),
            &[args.circuit_kind as u8],
            &args.circuit_version.to_le_bytes(),
        ],
        bump,
    )]
    pub verification_key: Box<Account<'info, QueueVerificationKey>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeQueueState<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(
        init,
        payer = authority,
        space = 8 + SubmissionCounter::INIT_SPACE,
        seeds = [b"submission-counter", auction.key().as_ref()],
        bump,
    )]
    pub submission_counter: Box<Account<'info, SubmissionCounter>>,
    #[account(
        init,
        payer = authority,
        space = 8 + AggregateAccumulator::INIT_SPACE,
        seeds = [b"aggregate-accumulator", auction.key().as_ref()],
        bump,
    )]
    pub accumulator: Box<Account<'info, AggregateAccumulator>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializePrivateBalance<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = key_epoch.key() == market.current_key_epoch @ BeatExError::InvalidKeyEpoch,
        has_one = market,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        init,
        payer = owner,
        space = 8 + EncryptedPrivateBalance::INIT_SPACE,
        seeds = [b"private-balance", market.key().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub private_balance: Box<Account<'info, EncryptedPrivateBalance>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = key_epoch.key() == market.current_key_epoch @ BeatExError::InvalidKeyEpoch,
        has_one = market,
        constraint = !key_epoch.retired @ BeatExError::RetiredKeyEpoch,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        has_one = market,
        constraint = deposit_verification_key.circuit_kind == QueueCircuitKind::BalanceDeposit,
    )]
    pub deposit_verification_key: Box<Account<'info, QueueVerificationKey>>,
    #[account(mut, has_one = market, has_one = owner, has_one = key_epoch)]
    pub private_balance: Box<Account<'info, EncryptedPrivateBalance>>,
    /// CHECK: Address, Token-2022 ownership and mint data are checked in the handler.
    #[account(owner = token_program.key())]
    pub base_mint: UncheckedAccount<'info>,
    /// CHECK: Address, Token-2022 ownership and mint data are checked in the handler.
    #[account(owner = token_program.key())]
    pub quote_mint: UncheckedAccount<'info>,
    /// CHECK: Token account mint and owner are checked before the CPI.
    #[account(mut, owner = token_program.key())]
    pub base_source: UncheckedAccount<'info>,
    /// CHECK: Token account mint and owner are checked before the CPI.
    #[account(mut, owner = token_program.key())]
    pub quote_source: UncheckedAccount<'info>,
    /// CHECK: Address, authority, mint and confidential extension are checked.
    #[account(mut, owner = token_program.key())]
    pub base_vault: UncheckedAccount<'info>,
    /// CHECK: Address, authority, mint and confidential extension are checked.
    #[account(mut, owner = token_program.key())]
    pub quote_vault: UncheckedAccount<'info>,
    /// CHECK: PDA authority for both market vaults; seeds are checked here and
    /// it signs the confidential-deposit CPIs.
    #[account(
        seeds = [b"vault-authority", market.key().as_ref()],
        bump = market.vault_authority_bump,
    )]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: The handler requires the canonical Token-2022 program id.
    #[account(executable)]
    pub token_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SubmitQueuedOrder<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    pub queue_accounts: SubmitQueueAccounts<'info>,
    #[account(
        init,
        payer = owner,
        space = 8 + QueuedOrder::INIT_SPACE,
        seeds = [
            b"queued-order",
            queue_accounts.auction.key().as_ref(),
            &queue_accounts.submission_counter.accepted_order_count.to_le_bytes(),
        ],
        bump,
    )]
    pub order: Box<Account<'info, QueuedOrder>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SubmitQueueAccounts<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market, has_one = key_epoch)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(has_one = market)]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        has_one = market,
        constraint = order_verification_key.circuit_kind == QueueCircuitKind::OrderSubmission,
    )]
    pub order_verification_key: Box<Account<'info, QueueVerificationKey>>,
    #[account(mut, has_one = auction)]
    pub submission_counter: Box<Account<'info, SubmissionCounter>>,
    #[account(mut, has_one = market, has_one = key_epoch)]
    pub private_balance: Box<Account<'info, EncryptedPrivateBalance>>,
}

#[derive(Accounts)]
pub struct FoldQueuedOrders<'info> {
    pub aggregator: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(has_one = auction)]
    pub submission_counter: Box<Account<'info, SubmissionCounter>>,
    #[account(mut, has_one = auction)]
    pub accumulator: Box<Account<'info, AggregateAccumulator>>,
    #[account(
        has_one = market,
        constraint = fold_verification_key.circuit_kind == QueueCircuitKind::AggregateFold,
    )]
    pub fold_verification_key: Box<Account<'info, QueueVerificationKey>>,
}

#[derive(Accounts)]
pub struct FinalizeQueueAggregation<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(has_one = auction)]
    pub submission_counter: Box<Account<'info, SubmissionCounter>>,
    #[account(has_one = auction)]
    pub accumulator: Box<Account<'info, AggregateAccumulator>>,
}

#[derive(Accounts)]
#[instruction(args: PublishAggregateCurvesArgs)]
pub struct PublishAggregateCurves<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market, has_one = key_epoch)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(has_one = market)]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(has_one = auction)]
    pub submission_counter: Box<Account<'info, SubmissionCounter>>,
    #[account(has_one = auction)]
    pub accumulator: Box<Account<'info, AggregateAccumulator>>,
    #[account(
        has_one = market,
        constraint = publication_verification_key.circuit_kind
            == QueueCircuitKind::AggregatePublication,
    )]
    pub publication_verification_key: Box<Account<'info, QueueVerificationKey>>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct ConfidentialTransferArgs {
    pub new_source_decryptable_available_balance: [u8; token2022::AE_CIPHERTEXT_LEN],
    pub auditor_ciphertext_lo: [u8; 64],
    pub auditor_ciphertext_hi: [u8; 64],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PrivateWithdrawalArgs {
    pub terminal_nullifier: [u8; 32],
    pub base_transfer: ConfidentialTransferArgs,
    pub quote_transfer: ConfidentialTransferArgs,
    pub proof: Groth16ProofInput,
}

#[derive(Accounts)]
#[instruction(args: InitializeMarketArgs)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: Token-2022 mint ownership and extensions are enforced when the
    /// confidential-vault feasibility gate is integrated.
    pub base_mint: UncheckedAccount<'info>,
    /// CHECK: See `base_mint`.
    pub quote_mint: UncheckedAccount<'info>,
    /// CHECK: Typed Token-2022 validation is added with Phase 4.
    pub base_vault: UncheckedAccount<'info>,
    /// CHECK: Typed Token-2022 validation is added with Phase 4.
    pub quote_vault: UncheckedAccount<'info>,
    /// CHECK: This PDA signs vault CPIs and stores no data.
    #[account(seeds = [b"vault-authority", market.key().as_ref()], bump)]
    pub vault_authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = authority,
        space = 8 + Market::INIT_SPACE,
        seeds = [
            b"market",
            base_mint.key().as_ref(),
            quote_mint.key().as_ref(),
            &args.market_nonce.to_le_bytes(),
        ],
        bump,
    )]
    pub market: Box<Account<'info, Market>>,
    pub system_program: Program<'info, System>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeOrderArgs {
    pub client_order_nonce: u64,
    pub base_collateral_commitment: [u8; 32],
    pub quote_collateral_commitment: [u8; 32],
    pub claim_nullifier_commitment: [u8; 32],
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncryptedOrderEntry {
    pub index: u8,
    pub buy: [u8; 64],
    pub sell: [u8; 64],
}

#[derive(Accounts)]
#[instruction(args: InitializeOrderArgs)]
pub struct InitializeOrder<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(
        init,
        payer = owner,
        space = 8 + Order::INIT_SPACE,
        seeds = [
            b"order",
            auction.key().as_ref(),
            owner.key().as_ref(),
            &args.client_order_nonce.to_le_bytes(),
        ],
        bump,
    )]
    pub order: Box<Account<'info, Order>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct WriteOrderChunk<'info> {
    pub owner: Signer<'info>,
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(mut, has_one = owner, has_one = auction)]
    pub order: Box<Account<'info, Order>>,
}

#[derive(Accounts)]
pub struct FinalizeTestOrder<'info> {
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(mut, has_one = auction, constraint = order.market == market.key())]
    pub order: Box<Account<'info, Order>>,
}

#[derive(Accounts)]
pub struct CloseUnfinalizedOrder<'info> {
    #[account(mut)]
    pub funder: Signer<'info>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    pub market: Box<Account<'info, Market>>,
    #[account(
        mut,
        close = funder,
        has_one = funder,
        has_one = auction,
        constraint = order.state == OrderState::Writing @ BeatExError::OrderState,
        constraint = Clock::get()?.slot >= auction.submission_end_slot @ BeatExError::DeadlineNotReached,
    )]
    pub order: Box<Account<'info, Order>>,
}

#[derive(Accounts)]
pub struct FinalizeDecryptedEntry<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market, has_one = key_epoch)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(has_one = market)]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
}

#[derive(Accounts)]
pub struct FinalizeDecryption<'info> {
    #[account(mut)]
    pub auction: Box<Account<'info, Auction>>,
}

#[derive(Accounts)]
pub struct ApplyVaultPendingBalance<'info> {
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = key_epoch.key() == market.current_key_epoch @ BeatExError::InvalidKeyEpoch,
        has_one = market,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    /// CHECK: Address and Token-2022 state are checked by the handler and CPI.
    #[account(owner = token_program.key())]
    pub mint: UncheckedAccount<'info>,
    /// CHECK: Address, owner, mint, authority and confidential extension are checked.
    #[account(mut, owner = token_program.key())]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: PDA signs the Token-2022 CPI and stores no data.
    #[account(
        seeds = [b"vault-authority", market.key().as_ref()],
        bump = market.vault_authority_bump,
    )]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: The helper requires the canonical Token-2022 program id.
    #[account(executable)]
    pub token_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SettleTestOrder<'info> {
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(
        constraint = key_epoch.key() == auction.key_epoch @ BeatExError::InvalidKeyEpoch,
        has_one = market,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        mut,
        has_one = auction,
        constraint = order.market == market.key(),
    )]
    pub order: Box<Account<'info, Order>>,
    pub base_transfer_accounts: ConfidentialTransferAccounts<'info>,
    pub quote_transfer_accounts: ConfidentialTransferAccounts<'info>,
    /// CHECK: PDA signs the Token-2022 CPIs and stores no data.
    #[account(
        seeds = [b"vault-authority", market.key().as_ref()],
        bump = market.vault_authority_bump,
    )]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: The handler requires the canonical Token-2022 program id.
    #[account(executable)]
    pub token_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct PrivateWithdrawal<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
    #[account(
        constraint = key_epoch.key() == auction.key_epoch @ BeatExError::InvalidKeyEpoch,
        has_one = market,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        mut,
        has_one = market,
        has_one = auction,
    )]
    pub order: Box<Account<'info, QueuedOrder>>,
    #[account(has_one = market)]
    pub withdrawal_verification_key: Box<Account<'info, QueueVerificationKey>>,
    pub base_transfer_accounts: ConfidentialTransferAccounts<'info>,
    pub quote_transfer_accounts: ConfidentialTransferAccounts<'info>,
    /// CHECK: PDA signs the two Token-2022 CPIs and stores no data.
    #[account(
        seeds = [b"vault-authority", market.key().as_ref()],
        bump = market.vault_authority_bump,
    )]
    pub vault_authority: UncheckedAccount<'info>,
    /// CHECK: The helper requires the canonical Token-2022 program id.
    #[account(executable)]
    pub token_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ConfidentialTransferAccounts<'info> {
    /// CHECK: Address, Token-2022 ownership and mint data are checked by the handler.
    pub mint: UncheckedAccount<'info>,
    /// CHECK: Address, authority, mint and confidential extension are checked by the handler.
    #[account(mut)]
    pub vault: UncheckedAccount<'info>,
    /// CHECK: Token-2022 validates the destination mint, keys and confidential extension.
    #[account(mut)]
    pub destination: UncheckedAccount<'info>,
    /// CHECK: Canonical proof context is extracted before Groth16 verification and CPI.
    pub equality_proof: UncheckedAccount<'info>,
    /// CHECK: Canonical proof context is extracted before Groth16 verification and CPI.
    pub validity_proof: UncheckedAccount<'info>,
    /// CHECK: Canonical proof context is extracted before Groth16 verification and CPI.
    pub range_proof: UncheckedAccount<'info>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeKeyEpochArgs {
    pub epoch: u64,
    pub encryption_pubkey: [u8; 32],
    /// Custom compressed Baby Jubjub encoding: y plus parity(x) in bit 255.
    pub babyjub_encryption_pubkey: [u8; 32],
    /// Baby Jubjub public share keys for participant IDs 1 through 7.
    pub babyjub_participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
    pub coefficient_commitments: [[u8; 32]; 3],
    pub participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
    pub participant_authorities: [Pubkey; COMMITTEE_SIZE],
    pub key_relation_proof: DleqProofInput,
}

#[derive(Accounts)]
#[instruction(args: InitializeKeyEpochArgs)]
pub struct InitializeKeyEpoch<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,

    #[account(
        init,
        payer = authority,
        space = 8 + KeyEpoch::INIT_SPACE,
        seeds = [b"key-epoch", market.key().as_ref(), &args.epoch.to_le_bytes()],
        bump,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ActivateKeyEpoch<'info> {
    pub authority: Signer<'info>,
    #[account(mut, has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market, has_one = authority)]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
}

#[derive(Accounts)]
pub struct AcknowledgeKeyEpoch<'info> {
    pub participant: Signer<'info>,
    #[account(mut)]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
}

#[derive(Accounts)]
#[instruction(auction_id: u64)]
pub struct OpenAuction<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(mut, has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(
        constraint = key_epoch.key() == market.current_key_epoch @ BeatExError::NoActiveKeyEpoch,
        constraint = !key_epoch.retired @ BeatExError::RetiredKeyEpoch,
    )]
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
    #[account(
        init,
        payer = authority,
        space = 8 + Auction::INIT_SPACE,
        seeds = [b"auction", market.key().as_ref(), &auction_id.to_le_bytes()],
        bump,
    )]
    pub auction: Box<Account<'info, Auction>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MutateAuction<'info> {
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
}

#[derive(Accounts)]
pub struct SubmitTestPlaintextCurves<'info> {
    pub authority: Signer<'info>,
    #[account(has_one = authority)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
}

#[derive(Accounts)]
pub struct ClearAuction<'info> {
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, has_one = market)]
    pub auction: Box<Account<'info, Auction>>,
}

#[derive(Accounts)]
pub struct VerifyThresholdDecryption<'info> {
    pub key_epoch: Box<Account<'info, KeyEpoch>>,
}

#[derive(Accounts)]
pub struct VerifyGroth16Pipeline {}

#[account]
#[derive(InitSpace)]
pub struct KeyEpoch {
    pub authority: Pubkey,
    pub market: Pubkey,
    pub epoch: u64,
    pub encryption_pubkey: [u8; 32],
    pub babyjub_encryption_pubkey: [u8; 32],
    /// Baby Jubjub public share keys, indexed by participant ID minus one.
    pub babyjub_participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
    pub coefficient_commitments: [[u8; 32]; 3],
    pub participant_public_shares: [[u8; 32]; COMMITTEE_SIZE],
    pub participant_authorities: [Pubkey; COMMITTEE_SIZE],
    pub key_relation_proof: DleqProofInput,
    pub threshold: u8,
    pub proof_transcript_version: u8,
    pub activation_slot: u64,
    pub retired: bool,
    pub acknowledgement_bitmap: u8,
    pub bump: u8,
}

#[allow(clippy::too_many_arguments)]
fn transfer_demo_deposit_asset<'info>(
    amount: u64,
    owner: &AccountInfo<'info>,
    source: &AccountInfo<'info>,
    mint: &AccountInfo<'info>,
    vault: &AccountInfo<'info>,
    vault_authority_account: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    expected_mint: &Pubkey,
    expected_vault: &Pubkey,
    market: &Pubkey,
    vault_authority_bump: u8,
    expected_elgamal_pubkey: &[u8; 32],
) -> Result<()> {
    require_keys_eq!(
        *mint.key,
        *expected_mint,
        BeatExError::InvalidDepositAccount
    );
    require_keys_eq!(
        *vault.key,
        *expected_vault,
        BeatExError::InvalidDepositAccount
    );
    require!(source.key != vault.key, BeatExError::InvalidDepositAccount);

    let (vault_authority, derived_bump) =
        Pubkey::find_program_address(&[b"vault-authority", market.as_ref()], &crate::ID);
    require!(
        derived_bump == vault_authority_bump,
        BeatExError::InvalidDepositAccount
    );
    require_keys_eq!(
        *vault_authority_account.key,
        vault_authority,
        BeatExError::InvalidDepositAccount
    );
    let decimals = token2022::mint_decimals(&mint.try_borrow_data()?)?;
    let source_before = token2022::validate_public_token_account(
        &source.try_borrow_data()?,
        expected_mint,
        owner.key,
    )?;
    let vault_before = token2022::validate_public_token_account(
        &vault.try_borrow_data()?,
        expected_mint,
        &vault_authority,
    )?;
    token2022::validate_confidential_vault(
        &vault.try_borrow_data()?,
        expected_mint,
        &vault_authority,
        expected_elgamal_pubkey,
    )?;

    if amount == 0 {
        return Ok(());
    }
    require!(
        source_before >= amount,
        BeatExError::InsufficientDepositFunds
    );
    token2022::invoke_public_transfer_checked(
        token_program,
        source,
        mint,
        vault,
        owner,
        amount,
        decimals,
    )?;
    let bump = [vault_authority_bump];
    let signer_seeds: &[&[u8]] = &[b"vault-authority", market.as_ref(), &bump];
    token2022::invoke_confidential_deposit(
        token_program,
        vault,
        mint,
        vault_authority_account,
        amount,
        decimals,
        &[signer_seeds],
    )?;

    // Reject fee-bearing or otherwise non-exact demo deposits. This keeps the
    // public amount in the Groth16 statement identical to actual vault credit.
    let source_after = token2022::validate_public_token_account(
        &source.try_borrow_data()?,
        expected_mint,
        owner.key,
    )?;
    let vault_after = token2022::validate_public_token_account(
        &vault.try_borrow_data()?,
        expected_mint,
        &vault_authority,
    )?;
    token2022::validate_confidential_deposit_deltas(
        source_before,
        source_after,
        vault_before,
        vault_after,
        amount,
    )
}

fn validate_test_settlement_accounts(ctx: &Context<SettleTestOrder>) -> Result<()> {
    let market = &ctx.accounts.market;
    let base = &ctx.accounts.base_transfer_accounts;
    let quote = &ctx.accounts.quote_transfer_accounts;
    require_keys_eq!(
        base.mint.key(),
        market.base_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        quote.mint.key(),
        market.quote_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        base.vault.key(),
        market.base_vault,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        quote.vault.key(),
        market.quote_vault,
        BeatExError::InvalidConfidentialVault
    );
    require!(
        base.destination.key() != market.base_vault
            && quote.destination.key() != market.quote_vault,
        BeatExError::InvalidSettlementDestination
    );
    token2022::validate_confidential_vault(
        &base.vault.try_borrow_data()?,
        &market.base_mint,
        &ctx.accounts.vault_authority.key(),
        &ctx.accounts.key_epoch.encryption_pubkey,
    )?;
    token2022::validate_confidential_vault(
        &quote.vault.try_borrow_data()?,
        &market.quote_mint,
        &ctx.accounts.vault_authority.key(),
        &ctx.accounts.key_epoch.encryption_pubkey,
    )
}

fn execute_private_withdrawal(
    ctx: Context<PrivateWithdrawal>,
    kind: WithdrawalKind,
    args: PrivateWithdrawalArgs,
) -> Result<()> {
    let auction = &ctx.accounts.auction;
    let order = &ctx.accounts.order;
    let next_state =
        queue::advance_withdrawal(order.state, order.withdrawal_state, kind).map_err(|reason| {
            match reason {
                queue::WithdrawalInvariantError::ClaimRequiresFoldedOrder => {
                    error!(BeatExError::WithdrawalNotAllowed)
                }
                queue::WithdrawalInvariantError::AlreadyWithdrawn => {
                    error!(BeatExError::WithdrawalAlreadyProcessed)
                }
            }
        })?;
    match kind {
        WithdrawalKind::Claim => {
            require!(
                auction.state == AuctionState::ClaimsOpen,
                BeatExError::WithdrawalNotAllowed
            );
            // A cleared auction cannot safely return full reserves, because
            // counterparties may already have claimed. Claims therefore stay
            // open until consumed instead of stranding funds at a deadline.
        }
        WithdrawalKind::Refund => require!(
            auction.state == AuctionState::Cancelled,
            BeatExError::WithdrawalNotAllowed
        ),
    }
    require!(
        auction.allocation_rule_version == allocation::ALLOCATION_RULE_VERSION
            && auction.order_circuit_version == order.order_circuit_version
            && auction.claim_circuit_version == queue::PRIVATE_CLAIM_CIRCUIT_VERSION,
        BeatExError::InvalidCircuitVersion
    );
    let expected_kind = match kind {
        WithdrawalKind::Claim => QueueCircuitKind::PrivateClaim,
        WithdrawalKind::Refund => QueueCircuitKind::CancellationRefund,
    };
    require!(
        ctx.accounts.withdrawal_verification_key.circuit_kind == expected_kind
            && ctx.accounts.withdrawal_verification_key.circuit_version
                == queue::circuit_version(expected_kind),
        BeatExError::InvalidCircuitVersion
    );

    validate_private_withdrawal_accounts(&ctx)?;
    let base = &ctx.accounts.base_transfer_accounts;
    let quote = &ctx.accounts.quote_transfer_accounts;
    let base_binding = token2022::extract_transfer_proof_binding(
        &base.equality_proof.to_account_info(),
        &base.validity_proof.to_account_info(),
        &base.range_proof.to_account_info(),
    )?;
    let quote_binding = token2022::extract_transfer_proof_binding(
        &quote.equality_proof.to_account_info(),
        &quote.validity_proof.to_account_info(),
        &quote.range_proof.to_account_info(),
    )?;
    require!(
        base_binding.source_pubkey == ctx.accounts.key_epoch.encryption_pubkey
            && quote_binding.source_pubkey == ctx.accounts.key_epoch.encryption_pubkey,
        BeatExError::InvalidCurveInput
    );
    token2022::validate_transfer_args(
        &base_binding,
        &args.base_transfer.new_source_decryptable_available_balance,
        &args.base_transfer.auditor_ciphertext_lo,
        &args.base_transfer.auditor_ciphertext_hi,
    )?;
    token2022::validate_transfer_args(
        &quote_binding,
        &args.quote_transfer.new_source_decryptable_available_balance,
        &args.quote_transfer.auditor_ciphertext_lo,
        &args.quote_transfer.auditor_ciphertext_hi,
    )?;
    let base_amount_commitment = base_binding.combined_amount_commitment()?;
    let quote_amount_commitment = quote_binding.combined_amount_commitment()?;
    let batched_amount_commitment = token2022::batch_transfer_amount_commitments(
        base_amount_commitment,
        quote_amount_commitment,
    )?;

    let clearing_index = usize::from(auction.clearing_index);
    require!(
        clearing_index < auction.active_grid_size(),
        BeatExError::GridIndex
    );
    let digest =
        queue_hash::private_withdrawal_statement_digest(&queue_hash::PrivateWithdrawalStatement {
            kind,
            market: &ctx.accounts.market.key(),
            auction: &auction.key(),
            order: &order.key(),
            owner: &order.owner,
            key_epoch: &auction.key_epoch,
            client_order_nonce: order.client_order_nonce,
            sequence: order.sequence,
            allocation_rule_version: auction.allocation_rule_version,
            order_circuit_version: auction.order_circuit_version,
            claim_circuit_version: auction.claim_circuit_version,
            order_commitment: order.order_commitment,
            clearing_index: auction.clearing_index,
            clearing_price: auction.clearing_price,
            demand_at_clearing: auction.decrypted_demand[clearing_index],
            supply_at_clearing: auction.decrypted_supply[clearing_index],
            clearing_volume: auction.clearing_volume,
            terminal_nullifier: args.terminal_nullifier,
            base_mint: &base.mint.key(),
            quote_mint: &quote.mint.key(),
            base_vault: &base.vault.key(),
            quote_vault: &quote.vault.key(),
            base_destination: &base.destination.key(),
            quote_destination: &quote.destination.key(),
            base_transfer: &base_binding,
            quote_transfer: &quote_binding,
            base_new_decryptable_balance: &args
                .base_transfer
                .new_source_decryptable_available_balance,
            quote_new_decryptable_balance: &args
                .quote_transfer
                .new_source_decryptable_available_balance,
            base_amount_commitment,
            quote_amount_commitment,
            batched_amount_commitment,
        })?;
    groth16::verify_queue_proof(
        &args.proof,
        &digest,
        &ctx.accounts.withdrawal_verification_key,
    )?;

    let market_key = ctx.accounts.market.key();
    let bump = [ctx.accounts.market.vault_authority_bump];
    let signer_seeds: &[&[u8]] = &[b"vault-authority", market_key.as_ref(), &bump];
    let token_program = ctx.accounts.token_program.to_account_info();
    let vault_authority = ctx.accounts.vault_authority.to_account_info();
    token2022::invoke_confidential_transfer(
        &token_program,
        &base.vault.to_account_info(),
        &base.mint.to_account_info(),
        &base.destination.to_account_info(),
        &vault_authority,
        &base.equality_proof.to_account_info(),
        &base.validity_proof.to_account_info(),
        &base.range_proof.to_account_info(),
        args.base_transfer.new_source_decryptable_available_balance,
        args.base_transfer.auditor_ciphertext_lo,
        args.base_transfer.auditor_ciphertext_hi,
        &[signer_seeds],
    )?;
    token2022::invoke_confidential_transfer(
        &token_program,
        &quote.vault.to_account_info(),
        &quote.mint.to_account_info(),
        &quote.destination.to_account_info(),
        &vault_authority,
        &quote.equality_proof.to_account_info(),
        &quote.validity_proof.to_account_info(),
        &quote.range_proof.to_account_info(),
        args.quote_transfer.new_source_decryptable_available_balance,
        args.quote_transfer.auditor_ciphertext_lo,
        args.quote_transfer.auditor_ciphertext_hi,
        &[signer_seeds],
    )?;

    let order = &mut ctx.accounts.order;
    order.withdrawal_state = next_state;
    order.terminal_nullifier = args.terminal_nullifier;
    Ok(())
}

fn validate_private_withdrawal_accounts(ctx: &Context<PrivateWithdrawal>) -> Result<()> {
    let market = &ctx.accounts.market;
    let base = &ctx.accounts.base_transfer_accounts;
    let quote = &ctx.accounts.quote_transfer_accounts;
    let token_program = ctx.accounts.token_program.key();
    for account in [
        &base.mint,
        &base.vault,
        &base.destination,
        &quote.mint,
        &quote.vault,
        &quote.destination,
    ] {
        require_keys_eq!(
            *account.owner,
            token_program,
            BeatExError::InvalidTokenProgram
        );
    }
    require_keys_eq!(
        base.mint.key(),
        market.base_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        quote.mint.key(),
        market.quote_mint,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        base.vault.key(),
        market.base_vault,
        BeatExError::InvalidConfidentialVault
    );
    require_keys_eq!(
        quote.vault.key(),
        market.quote_vault,
        BeatExError::InvalidConfidentialVault
    );
    require!(
        base.destination.key() != market.base_vault
            && quote.destination.key() != market.quote_vault,
        BeatExError::InvalidSettlementDestination
    );
    token2022::validate_confidential_vault(
        &base.vault.try_borrow_data()?,
        &market.base_mint,
        &ctx.accounts.vault_authority.key(),
        &ctx.accounts.key_epoch.encryption_pubkey,
    )?;
    token2022::validate_confidential_vault(
        &quote.vault.try_borrow_data()?,
        &market.quote_mint,
        &ctx.accounts.vault_authority.key(),
        &ctx.accounts.key_epoch.encryption_pubkey,
    )
}

fn validate_point(bytes: &[u8; 32]) -> Result<()> {
    require!(
        validate_ristretto(&PodRistrettoPoint(*bytes)),
        BeatExError::InvalidRistrettoPoint
    );
    Ok(())
}

fn validate_non_identity_point(bytes: &[u8; 32]) -> Result<()> {
    require!(*bytes != RISTRETTO_IDENTITY, BeatExError::IdentityPoint);
    validate_point(bytes)
}

fn validate_ciphertext(ciphertext: &[u8; 64]) -> Result<()> {
    let commitment = ciphertext[..32]
        .try_into()
        .map_err(|_| error!(BeatExError::InvalidCiphertext))?;
    let handle = ciphertext[32..]
        .try_into()
        .map_err(|_| error!(BeatExError::InvalidCiphertext))?;
    validate_point(&commitment)?;
    validate_point(&handle)
}

fn add_ciphertexts(left: &[u8; 64], right: &[u8; 64]) -> Result<[u8; 64]> {
    let left_commitment = PodRistrettoPoint(
        left[..32]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let right_commitment = PodRistrettoPoint(
        right[..32]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let left_handle = PodRistrettoPoint(
        left[32..]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let right_handle = PodRistrettoPoint(
        right[32..]
            .try_into()
            .map_err(|_| error!(BeatExError::InvalidCiphertext))?,
    );
    let commitment = add_ristretto(&left_commitment, &right_commitment)
        .ok_or_else(|| error!(BeatExError::InvalidCiphertext))?;
    let handle = add_ristretto(&left_handle, &right_handle)
        .ok_or_else(|| error!(BeatExError::InvalidCiphertext))?;
    let mut result = [0_u8; 64];
    result[..32].copy_from_slice(&commitment.0);
    result[32..].copy_from_slice(&handle.0);
    Ok(result)
}

fn queue_phase(state: AuctionState) -> Result<queue::QueuePhase> {
    match state {
        AuctionState::Open => Ok(queue::QueuePhase::Open),
        AuctionState::Sealed => Ok(queue::QueuePhase::Sealed),
        AuctionState::Decrypting | AuctionState::Decrypted | AuctionState::ClaimsOpen => {
            Ok(queue::QueuePhase::Decrypting)
        }
        AuctionState::Cancelled => Ok(queue::QueuePhase::Cancelled),
        AuctionState::Finalized => Ok(queue::QueuePhase::Settled),
    }
}

fn queue_deadlines(auction: &Auction) -> queue::QueueDeadlines {
    queue::QueueDeadlines {
        collection_end_slot: auction.submission_end_slot,
        aggregation_deadline_slot: auction.aggregation_deadline_slot,
    }
}

#[error_code]
pub enum BeatExError {
    #[msg("the supplied bytes are not a canonical Ristretto point")]
    InvalidRistrettoPoint,
    #[msg("the Ristretto identity is not valid for this key field")]
    IdentityPoint,
    #[msg("the side must be either demand or supply")]
    InvalidSide,
    #[msg("committee participant indices must be distinct and ascending")]
    ParticipantOrder,
    #[msg("the committee participant index is not registered")]
    UnknownParticipant,
    #[msg("the committee participant set is not valid for 3-of-7 recovery")]
    UnknownParticipantSet,
    #[msg("the twisted-ElGamal ciphertext has an invalid encoding")]
    InvalidCiphertext,
    #[msg("a curve point or scalar has an invalid canonical encoding")]
    InvalidCurveInput,
    #[msg("a committee decryption share proof is invalid")]
    InvalidDecryptionProof,
    #[msg("a registered public share is inconsistent with the committed polynomial")]
    InvalidPublicShare,
    #[msg("the claimed plaintext does not match the threshold decryption")]
    PlaintextMismatch,
    #[msg("market parameters are invalid")]
    InvalidMarketParameters,
    #[msg("arithmetic overflow")]
    ArithmeticOverflow,
    #[msg("the market is paused")]
    MarketPaused,
    #[msg("the market has no active key epoch")]
    NoActiveKeyEpoch,
    #[msg("the key epoch is retired")]
    RetiredKeyEpoch,
    #[msg("the key epoch configuration or proof version is invalid")]
    InvalidKeyEpoch,
    #[msg("the auction id is not the market's next id")]
    AuctionId,
    #[msg("the auction is not in the required state")]
    AuctionState,
    #[msg("the required slot deadline has not been reached")]
    DeadlineNotReached,
    #[msg("the slot deadline has already been reached")]
    DeadlineReached,
    #[msg("the plaintext demand or supply curves are invalid")]
    InvalidCurves,
    #[msg("a quantity exceeds the configured market bound")]
    QuantityBound,
    #[msg("not every aggregate entry has been decrypted")]
    IncompleteDecryption,
    #[msg("the order is not in the required state")]
    OrderState,
    #[msg("the order chunk size is invalid")]
    ChunkSize,
    #[msg("the price-grid index is out of bounds")]
    GridIndex,
    #[msg("an order entry was already written")]
    DuplicateEntry,
    #[msg("not every order entry has been written")]
    IncompleteOrder,
    #[msg("the order has already been aggregated")]
    OrderAlreadyAggregated,
    #[msg("the supplied ciphertext is not the auction aggregate entry")]
    CiphertextMismatch,
    #[msg("the aggregate entry has already been decrypted")]
    DuplicateDecryption,
    #[msg("the committee signing configuration is invalid")]
    InvalidCommittee,
    #[msg("not every committee member has acknowledged receiving a usable share")]
    CommitteeNotReady,
    #[msg("the supplied token program is not Token-2022")]
    InvalidTokenProgram,
    #[msg("the token account is not the expected confidential vault")]
    InvalidConfidentialVault,
    #[msg("the settlement destination is not valid")]
    InvalidSettlementDestination,
    #[msg("a claim or refund is not allowed in the current state")]
    WithdrawalNotAllowed,
    #[msg("the private withdrawal nullifier is zero or non-canonical")]
    InvalidWithdrawalNullifier,
    #[msg("this queued order has already been claimed or refunded")]
    WithdrawalAlreadyProcessed,
    #[msg("the Groth16 proof or public inputs are invalid")]
    InvalidGroth16Proof,
    #[msg("the proof circuit version is not supported")]
    InvalidCircuitVersion,
    #[msg("the queued-order state transition violates a counter, phase or deadline invariant")]
    QueueInvariant,
    #[msg("the queued aggregate does not contain every accepted order exactly once")]
    AggregationIncomplete,
    #[msg("the queue circuit currently supports exactly seventeen price levels")]
    UnsupportedQueueGrid,
    #[msg("the Baby Jubjub point or field encoding is not canonical")]
    InvalidBabyJubjubEncoding,
    #[msg("the Poseidon transcript syscall rejected its inputs")]
    PoseidonHashFailure,
    #[msg("the Groth16 public input does not match the on-chain statement")]
    Groth16PublicInputMismatch,
    #[msg("the aggregate fold batch must contain one to four unique accepted orders")]
    InvalidFoldBatch,
    #[msg("a private balance must start at the canonical encrypted zero")]
    InvalidInitialBalance,
    #[msg("the deposit must credit at least one asset")]
    EmptyDeposit,
    #[msg("a Token-2022 mint, source or vault is not valid for this deposit")]
    InvalidDepositAccount,
    #[msg("the depositing token account does not contain enough public funds")]
    InsufficientDepositFunds,
    #[msg("the vault did not receive the exact public amount proved by the deposit circuit")]
    NonExactDeposit,
}

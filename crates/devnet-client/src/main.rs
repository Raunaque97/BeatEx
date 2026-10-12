use {
    anchor_lang::{prelude::Pubkey, AccountDeserialize, InstructionData, Space},
    ark_bn254::Bn254,
    ark_groth16::Groth16,
    ark_snark::SNARK,
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    beatex::{
        allocation, queue, queue_hash, AggregateAccumulator, BindRolloverOrderArgs,
        BindRolloverSettlementArgs, DepositCollateralArgs, DleqProofInput, FoldQueuedOrdersArgs,
        Groth16ProofInput, InitializeKeyEpochArgs, InitializeMarketArgs,
        InitializePrivateBalanceArgs, InitializeQueueVerificationKeyArgs, PrivateWithdrawalArgs,
        PublishAggregateCurvesArgs, QueueCircuitKind, QueuedOrder, QueuedOrderState,
        ReleaseOrderCollateralArgs, ReplaceQueuedOrderArgs, RolloverNextOrderArgs,
        RolloverSettlementArgs, SubmissionCounter, SubmitQueuedOrderArgs, VerifyRolloverProofArgs,
        WithdrawPublicBalanceArgs, WithdrawalState,
    },
    beatex_babyjubjub_groth16_spike::{
        claim::{
            deterministic_private_withdrawal_verification_key, PrivateWithdrawalCircuit,
            PrivateWithdrawalKind,
        },
        decryption::{
            deterministic_devnet_committee, AggregatePublicationProver, AggregatePublicationRequest,
        },
        deposit::{self, DepositProofRequest, DepositProver, PublicWithdrawalProofRequest},
        release::{CollateralReleaseProofRequest, CollateralReleaseProver, ReplacementProver},
        rollover::{MarketMakerRolloverProofOutput, RolloverProofRequest, RolloverProver},
        transition::{
            self, FoldEventInput, FoldProofRequest, FoldProver, OrderProofRequest, OrderProver,
            PrivateBalanceOpening, SolanaVerificationKey,
        },
    },
    beatex_threshold_crypto::{dealer_setup_with_keypair_and_rng, KeyRelationProofContext},
    beatex_vault_proof_service::{
        build_withdrawal_context_plan, derive_private_order_identifiers,
        generate_private_withdrawal_groth16_proof, generate_withdrawal_proof_bundle,
        prepare_private_withdrawal_witness, ConfidentialDestination, PrivateOrderOpening,
        TransferContextAddresses, TransferRequest, VaultAeKey, VaultElGamalKeypair, VaultSnapshot,
        WithdrawalContextAddresses, WithdrawalStatementContext,
    },
    rand_chacha::{rand_core::SeedableRng, ChaCha20Rng},
    serde::{Deserialize, Serialize},
    solana_compute_budget_interface::ComputeBudgetInstruction,
    solana_hash::Hash,
    solana_hash_v1::Hash as V1Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_instruction_v1::{AccountMeta as V1AccountMeta, Instruction as V1Instruction},
    solana_keypair::{read_keypair_file, write_keypair_file, Keypair},
    solana_keypair_v1::read_keypair_file as read_v1_keypair_file,
    solana_message::Message,
    solana_message_v1::{
        v1::{Message as V1Message, TransactionConfig as V1TransactionConfig},
        VersionedMessage as V1VersionedMessage,
    },
    solana_signer::Signer,
    solana_signer_v1::Signer as V1Signer,
    solana_transaction::Transaction,
    solana_transaction_v1::versioned::VersionedTransaction as V1Transaction,
    solana_zk_sdk::encryption::{derivation::derive_confidential_keys, elgamal::ElGamalCiphertext},
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensions,
            StateWithExtensions,
        },
        state::Account as TokenAccount,
    },
    std::{
        cell::RefCell,
        collections::{HashMap, HashSet},
        env,
        error::Error,
        fs::{self, OpenOptions},
        io::{IsTerminal, Read, Write},
        path::{Path, PathBuf},
        str::FromStr,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, OnceLock,
        },
        thread,
        time::{Duration, Instant},
    },
};

#[cfg(feature = "browser-wallet")]
use std::process::{Command, Stdio};

const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
const PYTH_BTC_USD_ACCOUNT: &str = "4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo";
const PYTH_BTC_USD_FEED: &str = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";
// Increment when immutable Devnet market configuration changes.
const MARKET_NONCE: u64 = 11;
const KEY_EPOCH: u64 = 1;
const DEVNET_ORACLE_MAX_AGE_SECONDS: u64 = 24 * 60 * 60;
const DEVNET_SUBMISSION_DURATION_SLOTS: u64 = 40;
const DEVNET_AGGREGATION_DURATION_SLOTS: u64 = 200;
const DEVNET_DECRYPTION_DURATION_SLOTS: u64 = 200;
const MAX_COMMITTEE_AUCTION_WORKERS: usize = 4;
// Market makers submit early in the window, so the last fold starts this many
// slots before the close instead of waiting for it. Orders arriving later are
// picked up by one extra fold before publication.
const FINAL_FOLD_LEAD_SLOTS: u64 = 12;
// Leave enough room for RPC submission and validator execution after proving.
// Two slots was shorter than the observed native transaction path on Devnet.
const MIN_ORDER_SEND_REMAINING_SLOTS: u64 = 8;
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

#[derive(Clone, Debug)]
struct CommonConfig {
    base_mint: Pubkey,
    quote_mint: Pubkey,
    state_dir: PathBuf,
}

#[derive(Debug)]
struct InitializeConfig {
    common: CommonConfig,
    wallet: PathBuf,
    vault_owner: PathBuf,
    base_vault: Pubkey,
    quote_vault: Pubkey,
    rpc_url: String,
    market_start_slot: u64,
    /// Starting price of an oracle-free market, in quote atoms per base unit.
    /// `None` creates the oracle-priced BTC/USD market.
    initial_price: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Addresses {
    market: Pubkey,
    vault_authority: Pubkey,
    key_epoch: Pubkey,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleAccount {
    address: String,
    writable: bool,
    signer: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionBundle {
    keypair: String,
    label: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    signer_keypairs: Vec<String>,
    rpc_url: String,
    instructions: Vec<BundleInstruction>,
    compute_unit_limit: u32,
    loaded_accounts_data_size_limit: u32,
    heap_size: u32,
    priority_fee_lamports: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleInstruction {
    program_id: String,
    accounts: Vec<BundleAccount>,
    data_base64: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransactionCompletion {
    Confirmed,
    ConfirmedNoPreflight,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: RpcResult,
}

#[derive(Deserialize)]
struct RpcResult {
    value: Option<RpcAccount>,
}

#[derive(Deserialize)]
struct RpcAccount {
    data: (String, String),
}

#[derive(Deserialize)]
struct MultipleAccountsResponse {
    result: MultipleAccountsResult,
}

#[derive(Deserialize)]
struct MultipleAccountsResult {
    value: Vec<Option<RpcAccount>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NativeOrderIntent {
    #[serde(default)]
    pub auction_id: Option<u64>,
    pub reference_price: u64,
    pub grid_step_bps: u16,
    pub client_order_nonce: u64,
    pub buy: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
    pub sell: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeOrderEvent {
    Placed {
        auction_id: u64,
    },
    Replaced {
        auction_id: u64,
    },
    Rollover {
        previous_auction_id: u64,
        next_auction_id: u64,
    },
    Fill {
        auction_id: u64,
        side: String,
        base_amount: u64,
        quote_amount: u64,
    },
    Refund {
        auction_id: u64,
        base_amount: u64,
        quote_amount: u64,
    },
}

thread_local! {
    static NATIVE_ORDER_EVENTS: RefCell<Option<Vec<NativeOrderEvent>>> = const { RefCell::new(None) };
    // Groth16 circuit setup is intentionally deterministic for Devnet, but it
    // is still very expensive. Keep one proving key per worker thread so a
    // long-lived native client pays setup cost only on its first use of each
    // circuit. Thread-local storage avoids sharing Arkworks proving keys
    // across workers while keeping the hot proving path lock-free.
    static DEVNET_DEPOSIT_PROVER: RefCell<Option<DepositProver>> = const { RefCell::new(None) };
    static DEVNET_ORDER_PROVER: RefCell<Option<OrderProver>> = const { RefCell::new(None) };
    static DEVNET_FAST_ORDER_PROVER: RefCell<Option<OrderProver>> = const { RefCell::new(None) };
    static DEVNET_REPLACEMENT_PROVER: RefCell<Option<ReplacementProver>> = const { RefCell::new(None) };
    static DEVNET_FAST_REPLACEMENT_PROVER: RefCell<Option<ReplacementProver>> = const { RefCell::new(None) };
    static DEVNET_ROLLOVER_PROVER: RefCell<Option<RolloverProver>> = const { RefCell::new(None) };
    static DEVNET_COLLATERAL_RELEASE_PROVER: RefCell<Option<CollateralReleaseProver>> =
        const { RefCell::new(None) };
}

static DEVNET_FAST_FOLD_PROVER: OnceLock<FoldProver> = OnceLock::new();
static DEVNET_FAST_PUBLICATION_PROVER: OnceLock<AggregatePublicationProver> = OnceLock::new();

fn devnet_fast_fold_prover() -> Result<&'static FoldProver, Box<dyn Error>> {
    if let Some(prover) = DEVNET_FAST_FOLD_PROVER.get() {
        return Ok(prover);
    }
    let prover = FoldProver::deterministic_devnet_fast()?;
    let _ = DEVNET_FAST_FOLD_PROVER.set(prover);
    DEVNET_FAST_FOLD_PROVER
        .get()
        .ok_or_else(|| "fold prover cache initialization failed".into())
}

fn devnet_fast_publication_prover() -> Result<&'static AggregatePublicationProver, Box<dyn Error>> {
    if let Some(prover) = DEVNET_FAST_PUBLICATION_PROVER.get() {
        return Ok(prover);
    }
    let prover = AggregatePublicationProver::deterministic_devnet_fast()?;
    let _ = DEVNET_FAST_PUBLICATION_PROVER.set(prover);
    DEVNET_FAST_PUBLICATION_PROVER
        .get()
        .ok_or_else(|| "publication prover cache initialization failed".into())
}

fn with_deposit_prover<T>(
    f: impl FnOnce(&DepositProver) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    DEVNET_DEPOSIT_PROVER.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(DepositProver::deterministic_devnet()?);
        }
        f(cached.as_ref().expect("deposit prover cache initialized"))
    })
}

fn with_order_prover<T>(
    version: u16,
    f: impl FnOnce(&OrderProver) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let cache = if version == queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION {
        &DEVNET_FAST_ORDER_PROVER
    } else {
        &DEVNET_ORDER_PROVER
    };
    cache.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(if version == queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION {
                OrderProver::deterministic_devnet_fast()?
            } else {
                OrderProver::deterministic_devnet()?
            });
        }
        f(cached.as_ref().expect("order prover cache initialized"))
    })
}

fn with_replacement_prover<T>(
    version: u16,
    f: impl FnOnce(&ReplacementProver) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let cache = if version == queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION {
        &DEVNET_FAST_REPLACEMENT_PROVER
    } else {
        &DEVNET_REPLACEMENT_PROVER
    };
    cache.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(if version == queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION {
                ReplacementProver::deterministic_devnet_fast()?
            } else {
                ReplacementProver::deterministic_devnet()?
            });
        }
        f(cached
            .as_ref()
            .expect("replacement prover cache initialized"))
    })
}

fn with_rollover_prover<T>(
    f: impl FnOnce(&RolloverProver) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    DEVNET_ROLLOVER_PROVER.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(RolloverProver::deterministic_devnet_fast()?);
        }
        f(cached.as_ref().expect("rollover prover cache initialized"))
    })
}

fn with_collateral_release_prover<T>(
    f: impl FnOnce(&CollateralReleaseProver) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    DEVNET_COLLATERAL_RELEASE_PROVER.with(|cached| {
        let mut cached = cached.borrow_mut();
        if cached.is_none() {
            *cached = Some(CollateralReleaseProver::deterministic_devnet()?);
        }
        f(cached
            .as_ref()
            .expect("collateral release prover cache initialized"))
    })
}

/// Initialize the proving keys used by the native market-maker hot path on
/// the calling thread. The caches are thread-local, so long-lived workers
/// should call this once before accepting their first order.
pub fn warm_native_market_maker_provers() -> Result<(), String> {
    with_deposit_prover(|_| Ok(())).map_err(|error| error.to_string())?;
    with_order_prover(queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION, |_| Ok(()))
        .map_err(|error| error.to_string())?;
    with_rollover_prover(|_| Ok(())).map_err(|error| error.to_string())?;
    Ok(())
}

fn emit_order_event(event: NativeOrderEvent) {
    NATIVE_ORDER_EVENTS.with(|capture| {
        let mut capture = capture.borrow_mut();
        if let Some(events) = capture.as_mut() {
            events.push(event);
            return;
        }
        match event {
            NativeOrderEvent::Placed { auction_id } => {
                println!("BEATEX_ORDER_EVENT\tplaced\t{auction_id}")
            }
            NativeOrderEvent::Replaced { auction_id } => {
                println!("BEATEX_ORDER_EVENT\treplaced\t{auction_id}")
            }
            NativeOrderEvent::Rollover {
                previous_auction_id,
                next_auction_id,
            } => println!("BEATEX_ORDER_EVENT\trollover\t{previous_auction_id}\t{next_auction_id}"),
            NativeOrderEvent::Fill {
                auction_id,
                side,
                base_amount,
                quote_amount,
            } => println!(
                "BEATEX_ORDER_EVENT\tfill\t{auction_id}\t{side}\t{base_amount}\t{quote_amount}"
            ),
            NativeOrderEvent::Refund {
                auction_id,
                base_amount,
                quote_amount,
            } => {
                println!("BEATEX_ORDER_EVENT\trefund\t{auction_id}\t{base_amount}\t{quote_amount}")
            }
        }
    });
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredBalance {
    base_value: u64,
    quote_value: u64,
    base_randomness: [u8; 32],
    quote_randomness: [u8; 32],
}

impl StoredBalance {
    fn opening(&self) -> PrivateBalanceOpening {
        PrivateBalanceOpening {
            base_value: self.base_value,
            quote_value: self.quote_value,
            base_randomness: self.base_randomness,
            quote_randomness: self.quote_randomness,
        }
    }
}

impl From<PrivateBalanceOpening> for StoredBalance {
    fn from(value: PrivateBalanceOpening) -> Self {
        Self {
            base_value: value.base_value,
            quote_value: value.quote_value,
            base_randomness: value.base_randomness,
            quote_randomness: value.quote_randomness,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredOrder {
    auction_id: u64,
    auction: String,
    event: String,
    client_order_nonce: u64,
    buy: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
    sell: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
    order_salt: [u8; 32],
    order_commitment: [u8; 32],
    base_reserve: u64,
    quote_reserve: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TraderPrivateState {
    revision: u64,
    balance: StoredBalance,
    #[serde(default)]
    active_orders: Vec<StoredOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_order: Option<StoredOrder>,
}

impl TraderPrivateState {
    fn normalize(mut self) -> Self {
        if let Some(legacy) = self.active_order.take() {
            if !self
                .active_orders
                .iter()
                .any(|order| order.auction_id == legacy.auction_id)
            {
                self.active_orders.push(legacy);
            }
        }
        self.active_orders.sort_by_key(|order| order.auction_id);
        self
    }

    fn order(&self, auction_id: u64) -> Option<&StoredOrder> {
        self.active_orders
            .iter()
            .find(|order| order.auction_id == auction_id)
    }

    fn upsert_order(&mut self, order: StoredOrder) {
        self.active_orders
            .retain(|existing| existing.auction_id != order.auction_id);
        self.active_orders.push(order);
        self.active_orders.sort_by_key(|order| order.auction_id);
    }

    fn remove_order(&mut self, auction_id: u64) {
        self.active_orders
            .retain(|order| order.auction_id != auction_id);
    }

    fn has_active_orders(&self) -> bool {
        !self.active_orders.is_empty()
    }
}

struct FillEvent {
    side: &'static str,
    base_amount: u64,
    quote_amount: u64,
}

fn fill_event(
    auction: &beatex::AuctionResult,
    order: &StoredOrder,
) -> Result<Option<FillEvent>, Box<dyn Error>> {
    let index = usize::from(auction.clearing_index);
    let allocation = allocation::allocate(allocation::AllocationInput {
        buy: order.buy[index],
        sell: order.sell[index],
        demand: auction.demand_at_clearing,
        supply: auction.supply_at_clearing,
        clearing_price: auction.clearing_price,
        base_reserve: order.base_reserve,
        quote_reserve: order.quote_reserve,
    })
    .map_err(|error| format!("private allocation failed: {error:?}"))?;
    if order.buy[index] != 0 {
        Ok(Some(FillEvent {
            side: "buy",
            base_amount: allocation.base_credit,
            quote_amount: allocation.quote_debit,
        }))
    } else if order.sell[index] != 0 {
        Ok(Some(FillEvent {
            side: "sell",
            base_amount: allocation.base_debit,
            quote_amount: allocation.quote_credit,
        }))
    } else {
        Ok(None)
    }
}

#[derive(Debug, Clone)]
pub struct NativeOrderConfig {
    pub base_mint: Pubkey,
    pub quote_mint: Pubkey,
    pub wallet: PathBuf,
    pub state_dir: PathBuf,
    pub rpc_url: String,
    pub base_source: Pubkey,
    pub quote_source: Pubkey,
    pub deposit_base: u64,
    pub deposit_quote: u64,
    /// Working private balance to maintain, in base and quote token atoms.
    /// When either side falls below half its target, both are topped back up.
    /// Zero disables maintenance.
    pub target_base: u64,
    pub target_quote: u64,
}

#[derive(Clone, Debug)]
struct OrderConfig {
    common: CommonConfig,
    wallet: PathBuf,
    base_source: Pubkey,
    quote_source: Pubkey,
    rpc_url: String,
    deposit_base: u64,
    deposit_quote: u64,
    target_base: u64,
    target_quote: u64,
}

#[derive(Debug)]
struct FoldConfig {
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
    max_batches: usize,
    coordinator_logs: bool,
}

#[derive(Debug)]
struct AuctionStatusConfig {
    common: CommonConfig,
    rpc_url: String,
    auction_id: u64,
}

#[derive(Debug)]
struct MarketStatusConfig {
    common: CommonConfig,
    rpc_url: String,
}

#[derive(Clone, Debug)]
struct PipelineAuction {
    auction_id: u64,
    state: beatex::AuctionState,
    submission_end_slot: u64,
    aggregation_deadline_slot: u64,
    decryption_deadline_slot: u64,
    accepted_orders: u64,
    folded_orders: u64,
}

fn fetch_committee_auction_accounts(
    rpc_url: &str,
    addresses: &AuctionAddresses,
) -> Result<Option<(beatex::Auction, SubmissionCounter, AggregateAccumulator)>, Box<dyn Error>> {
    let mut accounts = fetch_multiple_account_bytes(
        rpc_url,
        &[
            addresses.auction,
            addresses.submission_counter,
            addresses.accumulator,
        ],
    )?
    .into_iter();
    let Some(auction_bytes) = accounts.next().flatten() else {
        return Ok(None);
    };
    let Some(counter_bytes) = accounts.next().flatten() else {
        return Ok(None);
    };
    let Some(accumulator_bytes) = accounts.next().flatten() else {
        return Ok(None);
    };
    Ok(Some((
        beatex::Auction::try_deserialize(&mut auction_bytes.as_slice())?,
        SubmissionCounter::try_deserialize(&mut counter_bytes.as_slice())?,
        AggregateAccumulator::try_deserialize(&mut accumulator_bytes.as_slice())?,
    )))
}

#[derive(Debug)]
struct PublishConfig {
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
    coordinator_logs: bool,
}

#[derive(Debug)]
struct ApplyVaultConfig {
    common: CommonConfig,
    wallet: PathBuf,
    vault_owner: PathBuf,
    rpc_url: String,
}

#[derive(Debug)]
struct ClaimConfig {
    common: CommonConfig,
    wallet: PathBuf,
    owner: PathBuf,
    vault_owner: PathBuf,
    base_destination: Pubkey,
    quote_destination: Pubkey,
    rpc_url: String,
    auction_id: u64,
    kind: PrivateWithdrawalKind,
}

#[derive(Debug)]
struct BalanceSettlementConfig {
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
    kind: PrivateWithdrawalKind,
}

#[derive(Debug)]
struct PublicBalanceWithdrawalConfig {
    common: CommonConfig,
    wallet: PathBuf,
    base_destination: Pubkey,
    quote_destination: Pubkey,
    base_amount: u64,
    quote_amount: u64,
    rpc_url: String,
}

#[derive(Clone, Debug)]
struct AuctionAddresses {
    auction_id: u64,
    auction: Pubkey,
    submission_counter: Pubkey,
    accumulator: Pubkey,
}

fn usage() {
    eprintln!(
        "Usage:\n  beatex-devnet-client addresses --base-mint PUBKEY --quote-mint PUBKEY --state-dir DIR\n\n  beatex-devnet-client initialize --base-mint PUBKEY --quote-mint PUBKEY --base-vault PUBKEY --quote-vault PUBKEY --wallet KEYPAIR --vault-owner KEYPAIR --state-dir DIR --market-start-slot SLOT [--rpc-url URL]\n\n  beatex-devnet-client committee --base-mint PUBKEY --quote-mint PUBKEY --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client watch-auctions --base-mint PUBKEY --quote-mint PUBKEY --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client auction --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client market-status|pipeline-status --base-mint PUBKEY --quote-mint PUBKEY --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client auction-status --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client order --base-mint PUBKEY --quote-mint PUBKEY --base-source PUBKEY --quote-source PUBKEY --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client fold --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL] [--max-batches N]\n\n  beatex-devnet-client publish --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client cancel --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client settle-balance|refund-balance --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet TRADER_KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client withdraw --base-mint PUBKEY --quote-mint PUBKEY --wallet TRADER_KEYPAIR --base-destination PUBKEY --quote-destination PUBKEY --base-amount N --quote-amount N --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client claim|refund --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet FEE_PAYER_KEYPAIR --owner TRADER_KEYPAIR --vault-owner KEYPAIR --base-destination PUBKEY --quote-destination PUBKEY --state-dir DIR [--rpc-url URL]\n\nThe default fast path keeps custody in the vault's public Token-2022 balance while all trading balances remain encrypted. settle-balance and refund-balance credit the internal ledger; withdraw debits it and performs one public token withdrawal. Legacy claim and refund retain the slower confidential Token-2022 exit. The client refuses mainnet URLs."
    );
}

fn value(args: &[String], name: &str) -> Result<String, Box<dyn Error>> {
    let position = args
        .iter()
        .position(|arg| arg == name)
        .ok_or_else(|| format!("missing {name}"))?;
    args.get(position + 1)
        .cloned()
        .ok_or_else(|| format!("{name} requires a value").into())
}

fn optional(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|position| args.get(position + 1).cloned())
}

fn common(args: &[String]) -> Result<CommonConfig, Box<dyn Error>> {
    Ok(CommonConfig {
        base_mint: Pubkey::from_str(&value(args, "--base-mint")?)?,
        quote_mint: Pubkey::from_str(&value(args, "--quote-mint")?)?,
        state_dir: PathBuf::from(value(args, "--state-dir")?),
    })
}

fn addresses(config: &CommonConfig) -> Addresses {
    let (market, _) = Pubkey::find_program_address(
        &[
            b"market",
            config.base_mint.as_ref(),
            config.quote_mint.as_ref(),
            &MARKET_NONCE.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let (vault_authority, _) =
        Pubkey::find_program_address(&[b"vault-authority", market.as_ref()], &beatex::ID);
    let (key_epoch, _) = Pubkey::find_program_address(
        &[b"key-epoch", market.as_ref(), &KEY_EPOCH.to_le_bytes()],
        &beatex::ID,
    );
    Addresses {
        market,
        vault_authority,
        key_epoch,
    }
}

fn print_addresses(config: &CommonConfig) -> Result<(), Box<dyn Error>> {
    let addresses = addresses(config);
    fs::create_dir_all(&config.state_dir)?;
    let manifest = config.state_dir.join("market-addresses.tsv");
    let contents = format!(
        "name\taddress\nmarket\t{}\nvault-authority\t{}\nkey-epoch\t{}\n",
        addresses.market, addresses.vault_authority, addresses.key_epoch
    );
    fs::write(&manifest, contents)?;
    println!("market\t{}", addresses.market);
    println!("vault-authority\t{}", addresses.vault_authority);
    println!("key-epoch\t{}", addresses.key_epoch);
    Ok(())
}

fn account_exists(rpc_url: &str, address: &Pubkey) -> bool {
    account_exists_result(rpc_url, address).unwrap_or(false)
}

fn account_exists_result(rpc_url: &str, address: &Pubkey) -> Result<bool, Box<dyn Error>> {
    account_exists_result_with_timeout(rpc_url, address, "20")
}

fn account_exists_result_with_timeout(
    rpc_url: &str,
    address: &Pubkey,
    timeout_seconds: &str,
) -> Result<bool, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [address.to_string(), {"encoding": "base64", "commitment": "confirmed"}],
    })
    .to_string();
    let response =
        serde_json::from_slice::<RpcResponse>(&rpc_request(rpc_url, &request, timeout_seconds)?)?;
    Ok(response.result.value.is_some())
}

// `InitializeAuction` creates three PDAs in one instruction.  A second
// initializer that reaches the same slot while the first transaction is
// still being processed fails in the system program with this message.  Keep
// this check deliberately narrow: all other simulation errors still need to
// be surfaced with their complete RPC payload.
fn is_already_initialized_race(error: impl std::fmt::Display) -> bool {
    let error = error.to_string();
    error.contains("Allocate: account Address") && error.contains("already in use")
}

fn wait_for_account_exists(
    rpc_url: &str,
    address: &Pubkey,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    loop {
        match account_exists_result_with_timeout(rpc_url, address, "1") {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => last_error = Some(error.to_string()),
        }
        if Instant::now() >= deadline {
            return Err(last_error
                .map(|error| format!("timed out confirming auction account {address}: {error}"))
                .unwrap_or_else(|| format!("timed out confirming auction account {address}"))
                .into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn rpc_candidates(primary: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: &str| {
        let candidate = candidate.trim();
        if !candidate.is_empty() && !candidates.iter().any(|existing| existing == candidate) {
            candidates.push(candidate.to_owned());
        }
    };
    push_unique(primary);
    if let Ok(configured) = env::var("BEATEX_RPC_FALLBACK_URLS") {
        for candidate in configured.split(',') {
            push_unique(candidate);
        }
    }
    candidates
}

fn rpc_label(rpc_url: &str) -> String {
    // Helius keys are query parameters and Alchemy keys are path segments.
    // Keep endpoint diagnostics useful without ever putting either credential
    // in a bot log or an error returned to the operator.
    let mut label = rpc_url.split('?').next().unwrap_or(rpc_url).to_owned();
    for marker in ["/v1/", "/v2/"] {
        if let Some(index) = label.find(marker) {
            label.truncate(index + marker.len());
            label.push_str("<redacted>");
            break;
        }
    }
    label
}

fn rpc_method(request: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(request)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
}

fn is_transaction_submission(method: Option<&str>) -> bool {
    matches!(method, Some("sendTransaction" | "sendVersionedTransaction"))
}

fn is_rate_limited_json_rpc(error: &serde_json::Value) -> bool {
    let code = error.get("code").and_then(serde_json::Value::as_i64);
    if matches!(code, Some(-32429 | -32005)) {
        return true;
    }
    error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("rate limit")
                || message.contains("too many requests")
                || message.contains("throttl")
        })
        .unwrap_or(false)
}

fn rpc_request(
    rpc_url: &str,
    request: &str,
    timeout_seconds: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::blocking::Client::new);
    let timeout = Duration::from_secs(timeout_seconds.parse()?);
    let submission = is_transaction_submission(rpc_method(request).as_deref());
    let mut failures = Vec::new();
    // Stateful bot reads must remain on the same provider that accepted the
    // preceding transaction. Round-robin reads at `processed` commitment can
    // otherwise move backwards in time when providers observe the same slot
    // at different moments. Fallbacks are still tried when the primary is
    // unavailable or rate-limited.
    for candidate in rpc_candidates(rpc_url) {
        let response = match client
            .post(&candidate)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(request.to_owned())
            .send()
        {
            Ok(response) => response,
            Err(error) => {
                failures.push(format!("{} request failed: {error}", rpc_label(&candidate)));
                continue;
            }
        };
        let status = response.status();
        let body = match response.bytes() {
            Ok(body) => body,
            Err(error) => {
                failures.push(format!(
                    "{} response body failed: {error}",
                    rpc_label(&candidate)
                ));
                continue;
            }
        };
        if !status.is_success() {
            if submission && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(format!(
                    "{} returned HTTP {}: {}",
                    rpc_label(&candidate),
                    status,
                    String::from_utf8_lossy(&body).trim()
                )
                .into());
            }
            failures.push(format!(
                "{} returned HTTP {}: {}",
                rpc_label(&candidate),
                status,
                String::from_utf8_lossy(&body).trim()
            ));
            continue;
        }
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(response) if response.get("error").is_none() => return Ok(body.to_vec()),
            Ok(response) => {
                let error = &response["error"];
                let code = error.get("code").and_then(serde_json::Value::as_i64);
                // A failed transaction simulation or invalid request is
                // deterministic. Replaying it against fallback providers
                // only duplicates the same program logs and incorrectly
                // presents an application failure as an RPC outage.
                if matches!(code, Some(-32002 | -32600 | -32601 | -32602)) {
                    return Err(format!(
                        "{} returned deterministic JSON-RPC error {error}",
                        rpc_label(&candidate)
                    )
                    .into());
                }
                if submission && !is_rate_limited_json_rpc(error) {
                    return Err(format!(
                        "{} returned JSON-RPC error {error}",
                        rpc_label(&candidate)
                    )
                    .into());
                }
                failures.push(format!(
                    "{} returned JSON-RPC error {error}",
                    rpc_label(&candidate)
                ));
            }
            Err(error) if submission => {
                return Err(
                    format!("{} returned invalid JSON: {error}", rpc_label(&candidate)).into(),
                );
            }
            Err(error) => failures.push(format!(
                "{} returned invalid JSON: {error}",
                rpc_label(&candidate)
            )),
        }
    }
    Err(format!("all RPC endpoints failed: {}", failures.join("; ")).into())
}

fn fetch_anchor_account<T: AccountDeserialize>(
    rpc_url: &str,
    address: &Pubkey,
) -> Result<Option<T>, Box<dyn Error>> {
    fetch_anchor_account_at(rpc_url, address, "confirmed", "20")
}

fn fetch_anchor_account_at<T: AccountDeserialize>(
    rpc_url: &str,
    address: &Pubkey,
    commitment: &str,
    timeout_seconds: &str,
) -> Result<Option<T>, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [address.to_string(), {"encoding": "base64", "commitment": commitment}],
    })
    .to_string();
    let response: RpcResponse =
        serde_json::from_slice(&rpc_request(rpc_url, &request, timeout_seconds)?)?;
    let Some(account) = response.result.value else {
        return Ok(None);
    };
    if account.data.1 != "base64" {
        return Err(format!("unexpected account encoding {}", account.data.1).into());
    }
    let bytes = BASE64.decode(account.data.0)?;
    Ok(Some(T::try_deserialize(&mut bytes.as_slice())?))
}

fn fetch_account_bytes(rpc_url: &str, address: &Pubkey) -> Result<Vec<u8>, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [address.to_string(), {"encoding": "base64", "commitment": "confirmed"}],
    })
    .to_string();
    let response: RpcResponse = serde_json::from_slice(&rpc_request(rpc_url, &request, "20")?)?;
    let account = response.result.value.ok_or("account does not exist")?;
    if account.data.1 != "base64" {
        return Err(format!("unexpected account encoding {}", account.data.1).into());
    }
    Ok(BASE64.decode(account.data.0)?)
}

fn fetch_multiple_account_bytes(
    rpc_url: &str,
    addresses: &[Pubkey],
) -> Result<Vec<Option<Vec<u8>>>, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getMultipleAccounts",
        "params": [
            addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
            {"encoding": "base64", "commitment": "confirmed"}
        ],
    })
    .to_string();
    let response: MultipleAccountsResponse =
        serde_json::from_slice(&rpc_request(rpc_url, &request, "20")?)?;
    if response.result.value.len() != addresses.len() {
        return Err("getMultipleAccounts returned the wrong number of entries".into());
    }
    response
        .result
        .value
        .into_iter()
        .map(|account| {
            account
                .map(|account| {
                    if account.data.1 != "base64" {
                        return Err(format!("unexpected account encoding {}", account.data.1));
                    }
                    BASE64
                        .decode(account.data.0)
                        .map_err(|error| error.to_string())
                })
                .transpose()
                .map_err(Into::into)
        })
        .collect()
}

fn deserialize_anchor_account<T: AccountDeserialize>(
    bytes: Option<Vec<u8>>,
    missing: &'static str,
) -> Result<T, Box<dyn Error>> {
    let bytes = bytes.ok_or(missing)?;
    Ok(T::try_deserialize(&mut bytes.as_slice())?)
}

fn minimum_balance_for_rent_exemption(rpc_url: &str, space: usize) -> Result<u64, Box<dyn Error>> {
    #[derive(Deserialize)]
    struct RentResponse {
        result: u64,
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getMinimumBalanceForRentExemption",
        "params": [space, {"commitment": "processed"}],
    })
    .to_string();
    Ok(serde_json::from_slice::<RentResponse>(&rpc_request(rpc_url, &request, "20")?)?.result)
}

fn lamport_balance(rpc_url: &str, owner: &Pubkey) -> Result<u64, Box<dyn Error>> {
    #[derive(Deserialize)]
    struct BalanceResponse {
        result: BalanceResult,
    }
    #[derive(Deserialize)]
    struct BalanceResult {
        value: u64,
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getBalance",
        "params": [owner.to_string(), {"commitment": "processed"}],
    })
    .to_string();
    Ok(
        serde_json::from_slice::<BalanceResponse>(&rpc_request(rpc_url, &request, "10")?)?
            .result
            .value,
    )
}

fn ensure_committee_payer_can_initialize(
    rpc_url: &str,
    payer: &Pubkey,
) -> Result<(), Box<dyn Error>> {
    const FEE_BUFFER_LAMPORTS: u64 = 100_000;
    let auction_rent =
        minimum_balance_for_rent_exemption(rpc_url, 8 + beatex::Auction::INIT_SPACE)?;
    let counter_rent =
        minimum_balance_for_rent_exemption(rpc_url, queue::SUBMISSION_COUNTER_SPACE)?;
    let accumulator_rent =
        minimum_balance_for_rent_exemption(rpc_url, 8 + AggregateAccumulator::INIT_SPACE)?;
    let account_rent = auction_rent
        .checked_add(counter_rent)
        .and_then(|value| value.checked_add(accumulator_rent))
        .ok_or("auction rent requirement overflow")?;
    let required = account_rent
        .checked_add(FEE_BUFFER_LAMPORTS)
        .ok_or("committee payer requirement overflow")?;
    let available = lamport_balance(rpc_url, payer)?;
    if available < required {
        return Err(format!(
            "committee payer {payer} has {available} lamports ({:.9} SOL); initializing one auction requires at least {required} lamports ({:.9} SOL: {account_rent} rent plus fee buffer). Fund the wallet and restart the committee bot",
            available as f64 / 1_000_000_000.0,
            required as f64 / 1_000_000_000.0,
        )
        .into());
    }
    Ok(())
}

/// The next accepted events to fold, in acceptance order, read from the
/// submission counter's pending-event ring rather than a program-account scan.
fn fetch_pending_orders(
    rpc_url: &str,
    counter: &Pubkey,
    folded_order_count: u64,
    limit: usize,
) -> Result<Vec<(Pubkey, QueuedOrder)>, Box<dyn Error>> {
    let counter_bytes = fetch_account_bytes(rpc_url, counter)?;
    let accepted =
        SubmissionCounter::try_deserialize(&mut counter_bytes.as_slice())?.accepted_order_count;
    let end = accepted.min(folded_order_count.saturating_add(limit as u64));
    let addresses = (folded_order_count..end)
        .map(|sequence| {
            queue::pending_event(&counter_bytes, sequence)
                .map_err(|error| format!("pending event {sequence} is unreadable: {error:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    let accounts = fetch_multiple_account_bytes(rpc_url, &addresses)?;
    addresses
        .into_iter()
        .zip(accounts)
        .map(|(address, bytes)| {
            let bytes = bytes.ok_or_else(|| format!("pending event {address} is missing"))?;
            Ok((
                address,
                QueuedOrder::try_deserialize(&mut bytes.as_slice())?,
            ))
        })
        .collect()
}

/// The two accounts Anchor's `#[event_cpi]` appends to instructions that emit
/// indexable events: the event-authority PDA and the program itself.
fn event_cpi_metas() -> [BundleAccount; 2] {
    let (event_authority, _) = Pubkey::find_program_address(&[b"__event_authority"], &beatex::ID);
    [
        meta(event_authority, false, false),
        meta(beatex::ID, false, false),
    ]
}

fn meta(address: Pubkey, writable: bool, signer: bool) -> BundleAccount {
    BundleAccount {
        address: address.to_string(),
        writable,
        signer,
    }
}

fn send(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    accounts: Vec<BundleAccount>,
    data: Vec<u8>,
    additional_signers: Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    send_with_completion(
        wallet,
        rpc_url,
        label,
        vec![BundleInstruction {
            program_id: beatex::ID.to_string(),
            accounts,
            data_base64: BASE64.encode(data),
        }],
        additional_signers,
        TransactionCompletion::Confirmed,
    )
}

fn send_committee(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    accounts: Vec<BundleAccount>,
    data: Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    send_with_completion(
        wallet,
        rpc_url,
        label,
        vec![BundleInstruction {
            program_id: beatex::ID.to_string(),
            accounts,
            data_base64: BASE64.encode(data),
        }],
        vec![],
        TransactionCompletion::ConfirmedNoPreflight,
    )
}

fn instruction_bundle(instruction: &Instruction) -> BundleInstruction {
    BundleInstruction {
        program_id: instruction.program_id.to_string(),
        accounts: instruction
            .accounts
            .iter()
            .map(|account| meta(account.pubkey, account.is_writable, account.is_signer))
            .collect(),
        data_base64: BASE64.encode(&instruction.data),
    }
}

fn send_instructions(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    instructions: &[Instruction],
    additional_signers: Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    send_bundle(
        wallet,
        rpc_url,
        label,
        instructions.iter().map(instruction_bundle).collect(),
        additional_signers,
    )
}

fn send_bundle(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    instructions: Vec<BundleInstruction>,
    additional_signers: Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    send_with_completion(
        wallet,
        rpc_url,
        label,
        instructions,
        additional_signers,
        TransactionCompletion::Confirmed,
    )
}

fn send_with_completion(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    instructions: Vec<BundleInstruction>,
    additional_signers: Vec<PathBuf>,
    completion: TransactionCompletion,
) -> Result<(), Box<dyn Error>> {
    let mut bundle =
        make_instruction_bundle(wallet, rpc_url, label, instructions, additional_signers)?;
    if completion == TransactionCompletion::ConfirmedNoPreflight {
        bundle.priority_fee_lamports = 10_000;
    }
    if !wallet_file_is_browser_session(wallet)? {
        return send_bundle_rust_v1(&bundle, completion);
    }
    #[cfg(not(feature = "browser-wallet"))]
    {
        return Err(
            "this native-only build does not support interactive browser-wallet sessions".into(),
        );
    }
    #[cfg(feature = "browser-wallet")]
    {
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sender = project_root.join("clients/v1-transaction/send.mjs");
        if !label.starts_with("initialize auction ") {
            eprintln!("sending {label}");
        }
        let mut child = Command::new("node")
            .arg(sender)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        serde_json::to_writer(
            child
                .stdin
                .take()
                .ok_or("transaction sender has no stdin")?,
            &bundle,
        )?;
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("{label} transaction failed with {status}").into());
        }
        Ok(())
    }
}

fn wallet_file_is_browser_session(path: &Path) -> Result<bool, Box<dyn Error>> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("browser-wallet"))
}

fn send_bundle_rust_v1(
    bundle: &InstructionBundle,
    completion: TransactionCompletion,
) -> Result<(), Box<dyn Error>> {
    let blockhash_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "getLatestBlockhash",
        "params": [{"commitment": "processed"}],
    })
    .to_string();
    let blockhash_response: serde_json::Value =
        serde_json::from_slice(&rpc_request(&bundle.rpc_url, &blockhash_request, "5")?)?;
    let blockhash = blockhash_response
        .pointer("/result/value/blockhash")
        .and_then(serde_json::Value::as_str)
        .ok_or("latest blockhash response is missing its blockhash")?
        .parse::<V1Hash>()?;

    let payer = read_v1_keypair_file(&bundle.keypair)?;
    let additional = bundle
        .signer_keypairs
        .iter()
        .map(read_v1_keypair_file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut signer_refs = Vec::with_capacity(additional.len() + 1);
    signer_refs.push(&payer);
    signer_refs.extend(additional.iter());
    let instructions = bundle
        .instructions
        .iter()
        .map(|instruction| {
            Ok(V1Instruction {
                program_id: instruction.program_id.parse()?,
                accounts: instruction
                    .accounts
                    .iter()
                    .map(|account| {
                        let address = account.address.parse()?;
                        Ok(if account.writable {
                            V1AccountMeta::new(address, account.signer)
                        } else {
                            V1AccountMeta::new_readonly(address, account.signer)
                        })
                    })
                    .collect::<Result<Vec<_>, Box<dyn Error>>>()?,
                data: BASE64.decode(&instruction.data_base64)?,
            })
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let config = V1TransactionConfig::empty()
        .with_priority_fee(bundle.priority_fee_lamports)
        .with_compute_unit_limit(bundle.compute_unit_limit)
        .with_loaded_accounts_data_size_limit(bundle.loaded_accounts_data_size_limit)
        .with_heap_size(bundle.heap_size);
    let message =
        V1Message::try_compile_with_config(&payer.pubkey(), &instructions, blockhash, config)?;
    let transaction = V1Transaction::try_new(V1VersionedMessage::V1(message), &signer_refs)?;
    let encoded_bytes = wincode::serialize(&transaction)?;
    if encoded_bytes.len() > 4096 {
        return Err(format!(
            "v1 transaction is {} bytes; maximum is 4096",
            encoded_bytes.len()
        )
        .into());
    }
    let latency_sensitive_transaction = completion == TransactionCompletion::ConfirmedNoPreflight
        || matches!(
            bundle.label.as_str(),
            "submit encrypted order"
                | "replace encrypted order"
                | "claim previous auction and place next order"
        );
    let send_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
        "params": [BASE64.encode(encoded_bytes), {
            // These transactions already contain locally generated Groth16
            // proofs and are constrained by the collection deadline.
            // Re-running the complete program in RPC preflight can consume
            // most of that window. The program still performs every check
            // when the transaction executes on-chain.
            "encoding": "base64", "skipPreflight": latency_sensitive_transaction,
            "preflightCommitment": "processed",
        }],
    })
    .to_string();
    let send_bytes = rpc_request(&bundle.rpc_url, &send_request, "15").map_err(|error| {
        if error.to_string().starts_with("all RPC endpoints failed") {
            format!("transaction submission outcome unknown: {error}").into()
        } else {
            error
        }
    })?;
    let send_response: serde_json::Value = serde_json::from_slice(&send_bytes)?;
    let signature = send_response
        .get("result")
        .and_then(serde_json::Value::as_str)
        .ok_or("sendTransaction response is missing its signature")?;

    let status_endpoints = rpc_candidates(&bundle.rpc_url);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut status_poll = 0usize;
    let mut last_rebroadcast = Instant::now();
    let mut last_status_error = None;
    while Instant::now() < deadline {
        // A provider can acknowledge a transaction without forwarding it to
        // a leader. Rebroadcasting the identical signed bytes is idempotent;
        // it cannot place a second order or charge a second transaction fee.
        if last_rebroadcast.elapsed() >= Duration::from_secs(2) {
            let endpoint = &status_endpoints[(status_poll + 1) % status_endpoints.len()];
            if let Err(error) = rpc_request(endpoint, &send_request, "5") {
                last_status_error = Some(format!("rebroadcast failed: {error}"));
            }
            last_rebroadcast = Instant::now();
        }
        let status_request = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "getSignatureStatuses",
            "params": [[signature]],
        })
        .to_string();
        let status_endpoint = &status_endpoints[status_poll % status_endpoints.len()];
        status_poll += 1;
        let status_response: serde_json::Value =
            match rpc_request(status_endpoint, &status_request, "5")
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            {
                Ok(response) => response,
                Err(error) => {
                    last_status_error = Some(error.to_string());
                    thread::sleep(Duration::from_millis(400));
                    continue;
                }
            };
        if let Some(status) = status_response.pointer("/result/value/0") {
            if !status.is_null() {
                if !status.get("err").is_none_or(serde_json::Value::is_null) {
                    return Err(format!(
                        "transaction {signature} failed on-chain: {}",
                        status["err"]
                    )
                    .into());
                }
                let confirmation_status = status
                    .get("confirmationStatus")
                    .and_then(serde_json::Value::as_str);
                let complete = matches!(confirmation_status, Some("confirmed" | "finalized"));
                if complete {
                    return Ok(());
                }
            }
        }
        thread::sleep(Duration::from_millis(400));
    }
    let history_request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "getSignatureStatuses",
        "params": [[signature], {"searchTransactionHistory": true}],
    })
    .to_string();
    for endpoint in &status_endpoints {
        if let Ok(bytes) = rpc_request(endpoint, &history_request, "5") {
            if let Ok(response) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(status) = response.pointer("/result/value/0") {
                    if !status.is_null() {
                        if !status.get("err").is_none_or(serde_json::Value::is_null) {
                            return Err(format!(
                                "transaction {signature} failed on-chain: {}",
                                status["err"]
                            )
                            .into());
                        }
                        if matches!(
                            status
                                .get("confirmationStatus")
                                .and_then(serde_json::Value::as_str),
                            Some("confirmed" | "finalized")
                        ) {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    let last_status_error = last_status_error
        .map(|error| format!("; last status RPC error: {error}"))
        .unwrap_or_default();
    Err(format!(
        "transaction confirmation outcome unknown for {signature} after 30 seconds{last_status_error}"
    )
    .into())
}

fn ensure_order_submission_time(
    rpc_url: &str,
    auction: &beatex::Auction,
) -> Result<(), Box<dyn Error>> {
    let slot = current_slot(rpc_url)?;
    let remaining = auction.submission_end_slot.saturating_sub(slot);
    if remaining <= MIN_ORDER_SEND_REMAINING_SLOTS {
        return Err(format!(
            "auction {} submission window expired before transaction submission",
            auction.auction_id
        )
        .into());
    }
    Ok(())
}

fn make_instruction_bundle(
    wallet: &Path,
    rpc_url: &str,
    label: &str,
    instructions: Vec<BundleInstruction>,
    additional_signers: Vec<PathBuf>,
) -> Result<InstructionBundle, Box<dyn Error>> {
    let latency_sensitive_order = matches!(
        label,
        "submit encrypted order"
            | "replace encrypted order"
            | "claim previous auction and place next order"
    );
    Ok(InstructionBundle {
        keypair: absolute(wallet)?.display().to_string(),
        label: label.to_owned(),
        signer_keypairs: additional_signers
            .iter()
            .map(|path| absolute(path).map(|path| path.display().to_string()))
            .collect::<Result<_, _>>()?,
        rpc_url: rpc_url.to_owned(),
        instructions,
        compute_unit_limit: 1_400_000,
        loaded_accounts_data_size_limit: 2 * 1024 * 1024,
        heap_size: 256 * 1024,
        priority_fee_lamports: if latency_sensitive_order { 10_000 } else { 0 },
    })
}

fn absolute(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    })
}

fn wallet_pubkey(path: &Path) -> Result<Pubkey, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if let Some(object) = value.as_object() {
        if object.get("version").and_then(serde_json::Value::as_u64) != Some(1)
            || object.get("type").and_then(serde_json::Value::as_str) != Some("browser-wallet")
        {
            return Err("unsupported wallet session file".into());
        }
        return Pubkey::from_str(
            object
                .get("address")
                .and_then(serde_json::Value::as_str)
                .ok_or("browser wallet session is missing address")?,
        )
        .map_err(Into::into);
    }
    Ok(read_keypair_file(path)?.pubkey())
}

fn ensure_participants(state_dir: &Path) -> Result<[PathBuf; 7], Box<dyn Error>> {
    let directory = state_dir.join("committee");
    fs::create_dir_all(&directory)?;
    let paths: [PathBuf; 7] =
        std::array::from_fn(|index| directory.join(format!("participant-{}.json", index + 1)));
    for path in &paths {
        if !path.exists() {
            write_keypair_file(&Keypair::new(), path)?;
            restrict_permissions(path)?;
        }
    }
    Ok(paths)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), Box<dyn Error>> {
    Ok(())
}

fn dleq_input(proof: &beatex_threshold_crypto::DleqProof) -> DleqProofInput {
    DleqProofInput {
        commitment_1: proof.commitment_1_bytes(),
        commitment_2: proof.commitment_2_bytes(),
        response: proof.response_bytes(),
    }
}

fn persist_secret_shares(
    state_dir: &Path,
    setup: &beatex_threshold_crypto::DealerSetup,
) -> Result<(), Box<dyn Error>> {
    let directory = state_dir.join("committee/threshold-shares");
    fs::create_dir_all(&directory)?;
    for share in &setup.secret_shares {
        let path = directory.join(format!("participant-{}.json", share.index()));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        serde_json::to_writer(&mut file, &share.secret_bytes())?;
        file.write_all(b"\n")?;
        restrict_permissions(&path)?;
    }
    Ok(())
}

fn initialize_market(
    config: &InitializeConfig,
    addresses: &Addresses,
    authority: Pubkey,
) -> Result<(), Box<dyn Error>> {
    if account_exists(&config.rpc_url, &addresses.market) {
        eprintln!("market {} already exists", addresses.market);
        return Ok(());
    }
    // An oracle-free market omits the oracle account (Anchor reads the program
    // id as an absent optional account) and starts from `initial_price`.
    let (oracle, initial_reference_price, min_reference_price, max_reference_price) =
        match config.initial_price {
            None => (Pubkey::from_str(PYTH_BTC_USD_ACCOUNT)?, 0, 1_000, 1_000_000),
            // 0.01 to 100 quote tokens per base token at 1.00-token base units.
            Some(price) => (beatex::ID, price, 10_000, 100_000_000),
        };
    let args = InitializeMarketArgs {
        market_nonce: MARKET_NONCE,
        base_unit_size: beatex::state::BASE_UNIT_SIZE,
        quote_unit_size: beatex::state::QUOTE_UNIT_SIZE,
        grid_step_bps: beatex::state::DEFAULT_GRID_STEP_BPS,
        grid_radius: 8,
        max_order_quantity: 1_000_000,
        max_aggregate_quantity: 100_000_000,
        market_start_slot: config.market_start_slot,
        minimum_reference_volume: 1,
        oracle_feed_id: decode_hex_32(PYTH_BTC_USD_FEED)?,
        oracle_max_age_seconds: DEVNET_ORACLE_MAX_AGE_SECONDS,
        oracle_max_confidence_bps: 200,
        oracle_exponent_adjustment: 0,
        min_reference_price,
        max_reference_price,
        submission_duration_slots: DEVNET_SUBMISSION_DURATION_SLOTS,
        aggregation_duration_slots: DEVNET_AGGREGATION_DURATION_SLOTS,
        decryption_duration_slots: DEVNET_DECRYPTION_DURATION_SLOTS,
        allocation_rule_version: allocation::ALLOCATION_RULE_VERSION,
        order_circuit_version: queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION,
        claim_circuit_version: queue::PRIVATE_CLAIM_CIRCUIT_VERSION,
        initial_reference_price,
    };
    send(
        &config.wallet,
        &config.rpc_url,
        "initialize market",
        vec![
            meta(authority, true, true),
            meta(config.common.base_mint, false, false),
            meta(config.common.quote_mint, false, false),
            meta(config.base_vault, false, false),
            meta(config.quote_vault, false, false),
            meta(oracle, false, false),
            meta(addresses.vault_authority, false, false),
            meta(addresses.market, true, false),
            meta(anchor_lang::system_program::ID, false, false),
        ],
        beatex::instruction::InitializeMarket { args }.data(),
        vec![],
    )
}

fn threshold_setup(
    config: &InitializeConfig,
    addresses: &Addresses,
) -> Result<beatex_threshold_crypto::DealerSetup, Box<dyn Error>> {
    threshold_setup_for_owner(&config.vault_owner, addresses)
}

fn threshold_setup_for_owner(
    vault_owner: &Path,
    addresses: &Addresses,
) -> Result<beatex_threshold_crypto::DealerSetup, Box<dyn Error>> {
    let owner = read_keypair_file(vault_owner)?;
    let (vault_keypair, _) = derive_confidential_keys(&owner, b"")?;
    let context = KeyRelationProofContext {
        program_id: beatex::ID.to_bytes(),
        market: addresses.market.to_bytes(),
        key_epoch_address: addresses.key_epoch.to_bytes(),
        epoch: KEY_EPOCH,
    };
    let mut rng = ChaCha20Rng::from_seed([0x45; 32]);
    Ok(dealer_setup_with_keypair_and_rng(
        3,
        7,
        &context.to_bytes(),
        &vault_keypair,
        &mut rng,
    )?)
}

fn initialize_epoch(
    config: &InitializeConfig,
    addresses: &Addresses,
    authority: Pubkey,
    participants: &[PathBuf; 7],
) -> Result<(), Box<dyn Error>> {
    if account_exists(&config.rpc_url, &addresses.key_epoch) {
        eprintln!("key epoch {} already exists", addresses.key_epoch);
        return Ok(());
    }
    let setup = threshold_setup(config, addresses)?;
    persist_secret_shares(&config.common.state_dir, &setup)?;
    let babyjub = deterministic_devnet_committee();
    let participant_authorities = participant_pubkeys(participants)?;
    let args = InitializeKeyEpochArgs {
        epoch: KEY_EPOCH,
        encryption_pubkey: setup.public.encryption_pubkey.to_bytes(),
        babyjub_encryption_pubkey: babyjub.encryption_key,
        babyjub_participant_public_shares: babyjub.participant_public_shares,
        coefficient_commitments: std::array::from_fn(|index| {
            setup.public.coefficient_commitments[index]
                .compress()
                .to_bytes()
        }),
        participant_public_shares: std::array::from_fn(|index| {
            setup.public.participant_shares[index]
                .point
                .compress()
                .to_bytes()
        }),
        participant_authorities,
        key_relation_proof: dleq_input(&setup.public.key_relation_proof),
    };
    send(
        &config.wallet,
        &config.rpc_url,
        "initialize key epoch",
        vec![
            meta(authority, true, true),
            meta(addresses.market, false, false),
            meta(addresses.key_epoch, true, false),
            meta(anchor_lang::system_program::ID, false, false),
        ],
        beatex::instruction::InitializeKeyEpoch { args }.data(),
        vec![],
    )
}

fn participant_pubkeys(paths: &[PathBuf; 7]) -> Result<[Pubkey; 7], Box<dyn Error>> {
    paths
        .iter()
        .map(read_keypair_file)
        .map(|result| result.map(|keypair| keypair.pubkey()))
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| "committee must contain seven participants".into())
}

fn acknowledge_and_activate(
    config: &InitializeConfig,
    addresses: &Addresses,
    authority: Pubkey,
    participants: &[PathBuf; 7],
) -> Result<(), Box<dyn Error>> {
    let epoch = fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &addresses.key_epoch)?
        .ok_or("key epoch was not found after initialization")?;
    for (index, path) in participants.iter().enumerate() {
        if epoch.acknowledgement_bitmap & (1 << index) != 0 {
            continue;
        }
        let participant = read_keypair_file(path)?.pubkey();
        send(
            &config.wallet,
            &config.rpc_url,
            &format!("acknowledge participant {}", index + 1),
            vec![
                meta(participant, false, true),
                meta(addresses.key_epoch, true, false),
            ],
            beatex::instruction::AcknowledgeKeyEpoch {
                participant_index: index as u8 + 1,
            }
            .data(),
            vec![path.clone()],
        )?;
    }
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market was not found after initialization")?;
    if market.current_key_epoch == addresses.key_epoch {
        eprintln!("key epoch {} is already active", addresses.key_epoch);
        Ok(())
    } else {
        send(
            &config.wallet,
            &config.rpc_url,
            "activate key epoch",
            vec![
                meta(authority, false, true),
                meta(addresses.market, true, false),
                meta(addresses.key_epoch, true, false),
            ],
            beatex::instruction::ActivateKeyEpoch {}.data(),
            vec![],
        )
    }
}

fn verification_key_address(market: &Pubkey, kind: QueueCircuitKind, version: u16) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"queue-vk",
            market.as_ref(),
            &[kind as u8],
            &version.to_le_bytes(),
        ],
        &beatex::ID,
    )
    .0
}

fn generate_verification_key(
    kind: QueueCircuitKind,
    version: u16,
) -> Result<SolanaVerificationKey, Box<dyn Error>> {
    eprintln!("generating {:?} verification key", kind);
    match kind {
        QueueCircuitKind::OrderSubmission => {
            with_order_prover(version, |prover| prover.verification_key())
        }
        QueueCircuitKind::AggregateFold => transition::deterministic_fold_verification_key(),
        QueueCircuitKind::BalanceDeposit => deposit::deterministic_verification_key(),
        QueueCircuitKind::AggregatePublication => {
            AggregatePublicationProver::deterministic_devnet_fast()?.verification_key()
        }
        QueueCircuitKind::PrivateClaim => {
            deterministic_private_withdrawal_verification_key(PrivateWithdrawalKind::Claim)
        }
        QueueCircuitKind::CancellationRefund => {
            deterministic_private_withdrawal_verification_key(PrivateWithdrawalKind::Refund)
        }
        QueueCircuitKind::OrderModification => {
            with_collateral_release_prover(|prover| prover.verification_key())
        }
        QueueCircuitKind::RolloverSettlement => {
            with_rollover_prover(|prover| prover.verification_key())
        }
    }
}

fn initialize_verification_keys(
    config: &InitializeConfig,
    addresses: &Addresses,
    authority: Pubkey,
) -> Result<(), Box<dyn Error>> {
    let kinds = [
        QueueCircuitKind::OrderSubmission,
        QueueCircuitKind::AggregateFold,
        QueueCircuitKind::BalanceDeposit,
        QueueCircuitKind::AggregatePublication,
        QueueCircuitKind::PrivateClaim,
        QueueCircuitKind::CancellationRefund,
        QueueCircuitKind::OrderModification,
        QueueCircuitKind::RolloverSettlement,
    ];
    for kind in kinds {
        let version = if kind == QueueCircuitKind::OrderSubmission {
            queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION
        } else {
            queue::circuit_version(kind)
        };
        let address = verification_key_address(&addresses.market, kind, version);
        if account_exists(&config.rpc_url, &address) {
            eprintln!("{:?} key {} already exists", kind, address);
            continue;
        }
        let key = generate_verification_key(kind, version)?;
        let args = InitializeQueueVerificationKeyArgs {
            circuit_kind: kind,
            circuit_version: version,
            alpha_g1: key.alpha_g1,
            beta_g2: key.beta_g2,
            gamma_g2: key.gamma_g2,
            delta_g2: key.delta_g2,
            ic: key.ic,
        };
        send(
            &config.wallet,
            &config.rpc_url,
            &format!("initialize {:?} verification key", kind),
            vec![
                meta(authority, true, true),
                meta(addresses.market, false, false),
                meta(address, true, false),
                meta(anchor_lang::system_program::ID, false, false),
            ],
            beatex::instruction::InitializeQueueVerificationKey { args }.data(),
            vec![],
        )?;
    }
    Ok(())
}

fn current_slot(rpc_url: &str) -> Result<u64, Box<dyn Error>> {
    current_slot_with_timeout(rpc_url, "20")
}

fn current_slot_with_timeout(rpc_url: &str, timeout_seconds: &str) -> Result<u64, Box<dyn Error>> {
    #[derive(Deserialize)]
    struct SlotResponse {
        result: u64,
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot",
        "params": [{"commitment": "processed"}],
    })
    .to_string();
    Ok(
        serde_json::from_slice::<SlotResponse>(&rpc_request(rpc_url, &request, timeout_seconds)?)?
            .result,
    )
}

fn current_auction_addresses(
    rpc_url: &str,
    addresses: &Addresses,
    market: &beatex::Market,
) -> Result<Option<AuctionAddresses>, Box<dyn Error>> {
    let slot = current_slot(rpc_url)?;
    if slot < market.market_start_slot {
        return Ok(None);
    }
    let auction_id = (slot - market.market_start_slot) / market.submission_duration_slots;
    Ok(Some(auction_addresses(addresses, auction_id)))
}

fn auction_addresses(addresses: &Addresses, auction_id: u64) -> AuctionAddresses {
    let (auction, _) = Pubkey::find_program_address(
        &[
            b"auction",
            addresses.market.as_ref(),
            &auction_id.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let (submission_counter, _) =
        Pubkey::find_program_address(&[b"submission-counter", auction.as_ref()], &beatex::ID);
    let (accumulator, _) =
        Pubkey::find_program_address(&[b"aggregate-accumulator", auction.as_ref()], &beatex::ID);
    AuctionAddresses {
        auction_id,
        auction,
        submission_counter,
        accumulator,
    }
}

fn send_initialization_direct(
    wallet: &Path,
    rpc_url: &str,
    instruction: Instruction,
    freshness: Option<(u64, u64, u64)>,
) -> Result<(), Box<dyn Error>> {
    let blockhash_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getLatestBlockhash",
        "params": [{"commitment": "processed"}],
    })
    .to_string();
    let blockhash_response: serde_json::Value =
        serde_json::from_slice(&rpc_request(rpc_url, &blockhash_request, "2")?)?;
    let blockhash = blockhash_response
        .pointer("/result/value/blockhash")
        .and_then(serde_json::Value::as_str)
        .ok_or("latest blockhash response is missing its blockhash")?
        .parse::<Hash>()?;
    if let Some((market_start_slot, submission_duration_slots, auction_id)) = freshness {
        let slot = current_slot_with_timeout(rpc_url, "2")?;
        if !auction_target_is_fresh(
            market_start_slot,
            submission_duration_slots,
            slot,
            auction_id,
        ) {
            return Err("auction initialization target became stale before submission".into());
        }
    }
    let payer = read_keypair_file(wallet)?;
    let transaction = Transaction::new(
        &[&payer],
        Message::new(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
                instruction,
            ],
            Some(&payer.pubkey()),
        ),
        blockhash,
    );
    let encoded = BASE64.encode(bincode::serialize(&transaction)?);
    let send_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "sendTransaction",
        "params": [
            encoded,
            {
                "encoding": "base64",
                // Initialization is latency-sensitive, but it must not be
                // fire-and-forget: without preflight an on-chain failure is
                // indistinguishable from a successful RPC submission and the
                // coordinator silently leaves the auction uninitialized.
                "skipPreflight": false,
                "preflightCommitment": "processed",
            }
        ],
    })
    .to_string();
    let response: serde_json::Value =
        serde_json::from_slice(&rpc_request(rpc_url, &send_request, "5")?)?;
    response
        .get("result")
        .and_then(serde_json::Value::as_str)
        .ok_or("initialize-auction sendTransaction response is missing its signature")?;
    Ok(())
}

fn submit_auction_initialization(
    wallet: &Path,
    rpc_url: &str,
    addresses: &Addresses,
    key_epoch: Pubkey,
    oracle_price: Pubkey,
    payer: Pubkey,
    auction_id: u64,
    quiet: bool,
    freshness: Option<(u64, u64)>,
) -> Result<(), Box<dyn Error>> {
    let current = auction_addresses(addresses, auction_id);
    let instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(addresses.market, false),
            AccountMeta::new_readonly(key_epoch, false),
            // An oracle-free market stores no oracle; Anchor reads the program
            // id as the omitted optional account.
            AccountMeta::new_readonly(
                if oracle_price == Pubkey::default() {
                    beatex::ID
                } else {
                    oracle_price
                },
                false,
            ),
            AccountMeta::new(current.auction, false),
            AccountMeta::new(current.submission_counter, false),
            AccountMeta::new(current.accumulator, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
        ],
        data: beatex::instruction::InitializeAuction { auction_id }.data(),
    };
    let label = format!("initialize auction {auction_id}");
    if quiet {
        send_initialization_direct(
            wallet,
            rpc_url,
            instruction,
            freshness.map(|(start, duration)| (start, duration, auction_id)),
        )
    } else {
        send_instructions(wallet, rpc_url, &label, &[instruction], vec![])
    }
}

fn coordinator_timestamp() -> String {
    let utc = time::OffsetDateTime::now_utc();
    let now = time::UtcOffset::current_local_offset()
        .map(|offset| utc.to_offset(offset))
        .unwrap_or(utc);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

fn coordinator_auction_log(auction_id: u64, action: &str) {
    let color = std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    if color {
        let (foreground, background) = match auction_id % 6 {
            0 => (37, 44),
            1 => (37, 45),
            2 => (30, 46),
            3 => (30, 42),
            4 => (30, 43),
            _ => (37, 100),
        };
        println!(
            "[{}] \x1b[1;{foreground};{background}m auction {auction_id} \x1b[0m {action}",
            coordinator_timestamp()
        );
    } else {
        println!(
            "[{}] auction {auction_id} {action}",
            coordinator_timestamp()
        );
    }
}

fn coordinator_error(error: &dyn std::fmt::Display) {
    for line in error.to_string().lines() {
        eprintln!("[{}] {line}", coordinator_timestamp());
    }
}

fn auction_target_is_fresh(
    market_start_slot: u64,
    submission_duration_slots: u64,
    slot: u64,
    auction_id: u64,
) -> bool {
    if submission_duration_slots == 0 || slot < market_start_slot {
        return false;
    }
    let elapsed = slot - market_start_slot;
    let current_auction_id = elapsed / submission_duration_slots;
    if auction_id == current_auction_id {
        return true;
    }
    let remaining_slots = submission_duration_slots - elapsed % submission_duration_slots;
    auction_id == current_auction_id.saturating_add(1)
        && remaining_slots <= beatex::AUCTION_PREINITIALIZATION_SLOTS
}

fn watch_and_initialize_auctions(
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
) -> Result<(), Box<dyn Error>> {
    if rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&common);
    let market = fetch_anchor_account::<beatex::Market>(&rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    if market.submission_duration_slots == 0 {
        return Err("market submission duration is zero".into());
    }
    let payer = wallet_pubkey(&wallet)?;
    let market_start_slot = market.market_start_slot;
    let submission_duration_slots = market.submission_duration_slots;
    let key_epoch = market.current_key_epoch;
    let oracle_price = market.oracle_price_account;
    let mut targets = HashMap::<u64, InitializationTarget>::new();

    loop {
        let slot = match current_slot_with_timeout(&rpc_url, "2") {
            Ok(slot) => slot,
            Err(error) => {
                coordinator_error(error.as_ref());
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if slot < market_start_slot {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let elapsed = slot - market_start_slot;
        let current_auction_id = elapsed / submission_duration_slots;
        let remaining_slots = submission_duration_slots - elapsed % submission_duration_slots;
        let next_auction_id = current_auction_id
            .checked_add(1)
            .ok_or("auction id overflow")?;
        // Each auction keeps retrying until its own window closes, independently
        // of the next auction becoming a target.
        targets.retain(|auction_id, target| {
            if *auction_id >= current_auction_id {
                return true;
            }
            if !target.confirmed.load(Ordering::Acquire)
                && !target.in_flight.load(Ordering::Acquire)
            {
                coordinator_auction_log(
                    *auction_id,
                    "missed: not initialized before its window closed",
                );
            }
            false
        });
        let mut candidates = vec![current_auction_id];
        if remaining_slots <= beatex::AUCTION_PREINITIALIZATION_SLOTS {
            candidates.push(next_auction_id);
        }
        let mut last_cleared_sequence = None;
        for auction_id in candidates {
            let target = targets.entry(auction_id).or_insert_with(|| {
                coordinator_auction_log(auction_id, "scheduled");
                InitializationTarget::default()
            });
            if target.confirmed.load(Ordering::Acquire)
                || target.in_flight.load(Ordering::Acquire)
                || target
                    .attempt_started
                    .is_some_and(|started| started.elapsed() < Duration::from_millis(750))
            {
                continue;
            }
            // Before the fallback slots, the program only accepts the next
            // auction once auction N-1 has cleared and its price can be used.
            if auction_id == next_auction_id
                && remaining_slots > beatex::AUCTION_FALLBACK_INITIALIZATION_SLOTS
            {
                let sequence = match last_cleared_sequence {
                    Some(sequence) => sequence,
                    None => match fetch_anchor_account_at::<beatex::Market>(
                        &rpc_url,
                        &addresses.market,
                        "processed",
                        "2",
                    ) {
                        Ok(Some(market)) => {
                            last_cleared_sequence = Some(market.last_cleared_auction_sequence);
                            market.last_cleared_auction_sequence
                        }
                        Ok(None) => continue,
                        Err(error) => {
                            coordinator_error(error.as_ref());
                            continue;
                        }
                    },
                };
                if !beatex::previous_clearing_is_recent(sequence, auction_id) {
                    continue;
                }
            }
            target.in_flight.store(true, Ordering::Release);
            target.attempt_started = Some(Instant::now());
            let open_offset = if auction_id == next_auction_id {
                format!("open-{remaining_slots}")
            } else {
                format!("open+{}", submission_duration_slots - remaining_slots)
            };
            let worker_wallet = wallet.clone();
            let worker_rpc = rpc_url.clone();
            let worker_addresses = addresses.clone();
            let worker_in_flight = Arc::clone(&target.in_flight);
            let worker_confirmed = Arc::clone(&target.confirmed);
            thread::spawn(move || {
                let current = auction_addresses(&worker_addresses, auction_id);
                // The target is already eligible. Submit without an account
                // lookup; the final slot check in send_initialization_direct
                // still prevents stale transactions at a phase boundary.
                let result = submit_auction_initialization(
                    &worker_wallet,
                    &worker_rpc,
                    &worker_addresses,
                    key_epoch,
                    oracle_price,
                    payer,
                    auction_id,
                    true,
                    Some((market_start_slot, submission_duration_slots)),
                );
                let confirmed = match result {
                    Ok(()) => match wait_for_account_exists(
                        &worker_rpc,
                        &current.auction,
                        Duration::from_secs(3),
                    ) {
                        Ok(()) => true,
                        Err(error) => {
                            coordinator_error(&error);
                            false
                        }
                    },
                    Err(error) => {
                        match account_exists_result_with_timeout(&worker_rpc, &current.auction, "1")
                        {
                            Ok(true) => {
                                if !is_already_initialized_race(error.as_ref()) {
                                    coordinator_error(error.as_ref());
                                }
                                true
                            }
                            Ok(false) => {
                                coordinator_error(error.as_ref());
                                false
                            }
                            Err(lookup_error) => {
                                coordinator_error(error.as_ref());
                                coordinator_error(lookup_error.as_ref());
                                false
                            }
                        }
                    }
                };
                if confirmed {
                    worker_confirmed.store(true, Ordering::Release);
                    let reference = match fetch_anchor_account_at::<beatex::Auction>(
                        &worker_rpc,
                        &current.auction,
                        "processed",
                        "2",
                    ) {
                        Ok(Some(auction)) if auction.reference_from_oracle => {
                            format!("reference {} from Pyth", auction.reference_price)
                        }
                        Ok(Some(auction)) => {
                            format!(
                                "reference {} from previous clearing",
                                auction.reference_price
                            )
                        }
                        _ => "reference unknown".to_owned(),
                    };
                    coordinator_auction_log(
                        auction_id,
                        &format!("initialized (sent at {open_offset}, {reference})"),
                    );
                }
                worker_in_flight.store(false, Ordering::Release);
            });
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[derive(Default)]
struct InitializationTarget {
    attempt_started: Option<Instant>,
    in_flight: Arc<AtomicBool>,
    confirmed: Arc<AtomicBool>,
}

fn initialize_current_auction(
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    requested_auction_id: Option<u64>,
) -> Result<(), Box<dyn Error>> {
    if rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&common);
    let market = fetch_anchor_account::<beatex::Market>(&rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let payer = wallet_pubkey(&wallet)?;
    let slot = current_slot(&rpc_url)?;
    if slot < market.market_start_slot {
        println!(
            "market {} starts at slot {}; current slot is {}",
            addresses.market, market.market_start_slot, slot
        );
        return Ok(());
    }
    let auction_id = (slot - market.market_start_slot) / market.submission_duration_slots;
    if requested_auction_id.is_some_and(|requested| requested != auction_id) {
        return Err(format!(
            "auction window changed before initialization (requested {}, current {auction_id})",
            requested_auction_id.unwrap()
        )
        .into());
    }
    let current = auction_addresses(&addresses, auction_id);
    if account_exists(&rpc_url, &current.auction) {
        println!(
            "auction {auction_id} already initialized: {}",
            current.auction
        );
        return Ok(());
    }
    submit_auction_initialization(
        &wallet,
        &rpc_url,
        &addresses,
        market.current_key_epoch,
        market.oracle_price_account,
        payer,
        auction_id,
        false,
        None,
    )?;
    println!("auction {auction_id} initialization submitted");
    Ok(())
}

fn proof_input(
    proof_a: [u8; 64],
    proof_b: [u8; 128],
    proof_c: [u8; 64],
    public_input: [u8; 32],
) -> Groth16ProofInput {
    Groth16ProofInput {
        proof_a,
        proof_b,
        proof_c,
        public_input,
    }
}

fn identity_ciphertext() -> [u8; 64] {
    let mut result = [0u8; 64];
    result[0] = 1;
    result[32] = 1;
    result
}

fn ciphertext_array(
    bytes: &[u8],
) -> Result<[[u8; 64]; queue::QUEUED_CIPHERTEXT_COUNT], Box<dyn Error>> {
    if bytes.len() != queue::QUEUED_CIPHERTEXT_COUNT * queue::ELGAMAL_CIPHERTEXT_BYTES {
        return Err("queued ciphertext vector has the wrong length".into());
    }
    bytes
        .chunks_exact(queue::ELGAMAL_CIPHERTEXT_BYTES)
        .map(|chunk| {
            chunk
                .try_into()
                .map_err(|_| "invalid queued ciphertext".into())
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?
        .try_into()
        .map_err(|_| "queued order must contain 34 ciphertexts".into())
}

fn trader_state_path(state_dir: &Path, owner: &Pubkey) -> PathBuf {
    let market = fs::read_to_string(state_dir.join("market.tsv"))
        .ok()
        .and_then(|contents| {
            contents.lines().skip(1).find_map(|line| {
                let mut fields = line.split('\t');
                if fields.next() == Some("market") {
                    fields.next().map(str::to_owned)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "legacy".to_owned());
    state_dir
        .join("traders")
        .join(market)
        .join(format!("{owner}.json"))
}

fn read_trader_state(
    state_dir: &Path,
    owner: &Pubkey,
) -> Result<Option<TraderPrivateState>, Box<dyn Error>> {
    let path = trader_state_path(state_dir, owner);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice::<TraderPrivateState>(&fs::read(path)?)?.normalize(),
    ))
}

fn write_trader_state(
    state_dir: &Path,
    owner: &Pubkey,
    state: &TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let path = trader_state_path(state_dir, owner);
    let directory = path.parent().ok_or("trader state path has no parent")?;
    fs::create_dir_all(&directory)?;
    let temporary = path.with_extension("json.pending");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    restrict_permissions(&temporary)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn private_balance_address(market: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"private-balance", market.as_ref(), owner.as_ref()],
        &beatex::ID,
    )
    .0
}

fn initialize_private_balance(
    config: &OrderConfig,
    addresses: &Addresses,
    owner: Pubkey,
    private_balance: Pubkey,
) -> Result<(), Box<dyn Error>> {
    if account_exists(&config.rpc_url, &private_balance) {
        return Ok(());
    }
    let identity = identity_ciphertext();
    send(
        &config.wallet,
        &config.rpc_url,
        "initialize private balance",
        vec![
            meta(owner, true, true),
            meta(addresses.market, false, false),
            meta(addresses.key_epoch, false, false),
            meta(private_balance, true, false),
            meta(anchor_lang::system_program::ID, false, false),
        ],
        beatex::instruction::InitializePrivateBalance {
            args: InitializePrivateBalanceArgs {
                encrypted_base: identity,
                encrypted_quote: identity,
            },
        }
        .data(),
        vec![],
    )
}

fn deposit_balance(
    config: &OrderConfig,
    addresses: &Addresses,
    market: &beatex::Market,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    state: &mut TraderPrivateState,
    base_amount: u64,
    quote_amount: u64,
) -> Result<(), Box<dyn Error>> {
    if base_amount == 0 && quote_amount == 0 {
        return Ok(());
    }
    let mut rng = ChaCha20Rng::from_entropy();
    let output = with_deposit_prover(|prover| {
        prover.prove(
            DepositProofRequest {
                market: addresses.market.to_bytes(),
                owner: owner.to_bytes(),
                key_epoch: addresses.key_epoch.to_bytes(),
                committee_key: key_epoch.babyjub_encryption_pubkey,
                old_revision: state.revision,
                base_amount,
                quote_amount,
                old_balance: state.balance.opening(),
            },
            &mut rng,
        )
    })?;
    let deposit_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::BalanceDeposit,
        queue::BALANCE_DEPOSIT_CIRCUIT_VERSION,
    );
    send(
        &config.wallet,
        &config.rpc_url,
        "deposit private collateral",
        vec![
            meta(owner, true, true),
            meta(addresses.market, false, false),
            meta(addresses.key_epoch, false, false),
            meta(deposit_key, false, false),
            meta(private_balance, true, false),
            meta(market.base_mint, false, false),
            meta(market.quote_mint, false, false),
            meta(config.base_source, true, false),
            meta(config.quote_source, true, false),
            meta(market.base_vault, true, false),
            meta(market.quote_vault, true, false),
            meta(addresses.vault_authority, false, false),
            meta(Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?, false, false),
        ],
        beatex::instruction::DepositCollateral {
            args: DepositCollateralArgs {
                base_amount,
                quote_amount,
                new_encrypted_base: output.new_encrypted_base,
                new_encrypted_quote: output.new_encrypted_quote,
                proof: proof_input(
                    output.proof_a,
                    output.proof_b,
                    output.proof_c,
                    output.public_input,
                ),
            },
        }
        .data(),
        vec![],
    )?;
    state.balance = output.new_balance.into();
    state.revision += 1;
    write_trader_state(&config.common.state_dir, &owner, state)
}

fn flatten_ciphertexts(
    ciphertexts: [[u8; 64]; beatex_babyjubjub_groth16_spike::ORDER_CIPHERTEXTS],
) -> Vec<u8> {
    ciphertexts.into_iter().flatten().collect()
}

#[allow(clippy::too_many_arguments)]
fn submit_order(
    config: &OrderConfig,
    addresses: &Addresses,
    auction_addresses: &AuctionAddresses,
    auction: &beatex::Auction,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    intent: &NativeOrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let mut rng = ChaCha20Rng::from_entropy();
    let output = with_order_prover(auction.order_circuit_version, |prover| {
        prover.prove(
            OrderProofRequest {
                auction: auction_addresses.auction.to_bytes(),
                owner: owner.to_bytes(),
                client_order_nonce: intent.client_order_nonce,
                reference_price: auction.reference_price,
                grid_step_bps: auction.grid_step_bps,
                committee_key: key_epoch.babyjub_encryption_pubkey,
                buy: intent.buy,
                sell: intent.sell,
                old_balance: state.balance.opening(),
            },
            &mut rng,
        )
    })?;
    let (event, _) = Pubkey::find_program_address(
        &[
            b"queued-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
            &intent.client_order_nonce.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let (active_order, _) = Pubkey::find_program_address(
        &[
            b"active-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    );
    let order_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderSubmission,
        auction.order_circuit_version,
    );
    ensure_order_submission_time(&config.rpc_url, auction)?;
    send(
        &config.wallet,
        &config.rpc_url,
        "submit encrypted order",
        vec![
            meta(owner, true, true),
            meta(addresses.market, false, false),
            meta(auction_addresses.auction, false, false),
            meta(addresses.key_epoch, false, false),
            meta(order_key, false, false),
            meta(auction_addresses.submission_counter, true, false),
            meta(private_balance, true, false),
            meta(event, true, false),
            meta(active_order, true, false),
            meta(anchor_lang::system_program::ID, false, false),
        ],
        beatex::instruction::SubmitQueuedOrder {
            args: SubmitQueuedOrderArgs {
                client_order_nonce: intent.client_order_nonce,
                order_commitment: output.order_commitment,
                ciphertexts: flatten_ciphertexts(output.ciphertexts),
                new_encrypted_base: output.new_encrypted_base,
                new_encrypted_quote: output.new_encrypted_quote,
                proof: proof_input(
                    output.proof_a,
                    output.proof_b,
                    output.proof_c,
                    output.public_input,
                ),
            },
        }
        .data(),
        vec![],
    )?;
    state.balance = output.new_balance.into();
    state.revision += 1;
    state.upsert_order(StoredOrder {
        auction_id: auction_addresses.auction_id,
        auction: auction_addresses.auction.to_string(),
        event: event.to_string(),
        client_order_nonce: intent.client_order_nonce,
        buy: intent.buy,
        sell: intent.sell,
        order_salt: output.order_salt,
        order_commitment: output.order_commitment,
        base_reserve: output.base_reserve,
        quote_reserve: output.quote_reserve,
    });
    write_trader_state(&config.common.state_dir, &owner, state)?;
    emit_order_event(NativeOrderEvent::Placed {
        auction_id: auction_addresses.auction_id,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn replace_order(
    config: &OrderConfig,
    addresses: &Addresses,
    auction_addresses: &AuctionAddresses,
    auction: &beatex::Auction,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    intent: &NativeOrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let previous = state
        .order(auction_addresses.auction_id)
        .cloned()
        .ok_or("missing private active-order state")?;
    let previous_event = Pubkey::from_str(&previous.event)?;
    let (active_order, _) = Pubkey::find_program_address(
        &[
            b"active-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    );
    let active = fetch_anchor_account::<beatex::ActiveOrder>(&config.rpc_url, &active_order)?
        .ok_or("active-order account is missing")?;
    if !active.active || active.latest_event != previous_event {
        return Err("local active-order state does not match the chain".into());
    }
    let (event, _) = Pubkey::find_program_address(
        &[
            b"queued-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
            &intent.client_order_nonce.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let mut rng = ChaCha20Rng::from_entropy();
    let output = with_replacement_prover(auction.order_circuit_version, |prover| {
        prover.prove(
            CollateralReleaseProofRequest {
                market: addresses.market.to_bytes(),
                auction: auction_addresses.auction.to_bytes(),
                active_order: active_order.to_bytes(),
                owner: owner.to_bytes(),
                balance_revision: state.revision,
                client_order_nonce: previous.client_order_nonce,
                buy: previous.buy,
                sell: previous.sell,
                base_reserve: previous.base_reserve,
                quote_reserve: previous.quote_reserve,
                order_salt: previous.order_salt,
                expected_order_commitment: previous.order_commitment,
                committee_key: key_epoch.babyjub_encryption_pubkey,
                old_balance: state.balance.opening(),
            },
            OrderProofRequest {
                auction: auction_addresses.auction.to_bytes(),
                owner: owner.to_bytes(),
                client_order_nonce: intent.client_order_nonce,
                reference_price: auction.reference_price,
                grid_step_bps: auction.grid_step_bps,
                committee_key: key_epoch.babyjub_encryption_pubkey,
                buy: intent.buy,
                sell: intent.sell,
                old_balance: state.balance.opening(),
            },
            &mut rng,
        )
    })?;
    let release_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderModification,
        queue::ORDER_MODIFICATION_CIRCUIT_VERSION,
    );
    let order_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderSubmission,
        auction.order_circuit_version,
    );
    ensure_order_submission_time(&config.rpc_url, auction)?;
    send(
        &config.wallet,
        &config.rpc_url,
        "replace encrypted order",
        vec![
            meta(owner, true, true),
            meta(addresses.market, false, false),
            meta(auction_addresses.auction, false, false),
            meta(addresses.key_epoch, false, false),
            meta(release_key, false, false),
            meta(auction_addresses.submission_counter, true, false),
            meta(private_balance, true, false),
            meta(active_order, true, false),
            meta(previous_event, false, false),
            meta(order_key, false, false),
            meta(event, true, false),
            meta(anchor_lang::system_program::ID, false, false),
        ],
        beatex::instruction::ReplaceQueuedOrder {
            args: ReplaceQueuedOrderArgs {
                release: ReleaseOrderCollateralArgs {
                    released_encrypted_base: output.release.released_encrypted_base,
                    released_encrypted_quote: output.release.released_encrypted_quote,
                    proof: proof_input(
                        output.release.proof_a,
                        output.release.proof_b,
                        output.release.proof_c,
                        output.release.public_input,
                    ),
                },
                replacement: SubmitQueuedOrderArgs {
                    client_order_nonce: intent.client_order_nonce,
                    order_commitment: output.replacement.order_commitment,
                    ciphertexts: flatten_ciphertexts(output.replacement.ciphertexts),
                    new_encrypted_base: output.replacement.new_encrypted_base,
                    new_encrypted_quote: output.replacement.new_encrypted_quote,
                    proof: proof_input(
                        output.replacement.proof_a,
                        output.replacement.proof_b,
                        output.replacement.proof_c,
                        output.replacement.public_input,
                    ),
                },
            },
        }
        .data(),
        vec![],
    )?;
    state.balance = output.replacement.new_balance.into();
    state.revision += 1;
    state.upsert_order(StoredOrder {
        auction_id: auction_addresses.auction_id,
        auction: auction_addresses.auction.to_string(),
        event: event.to_string(),
        client_order_nonce: intent.client_order_nonce,
        buy: intent.buy,
        sell: intent.sell,
        order_salt: output.replacement.order_salt,
        order_commitment: output.replacement.order_commitment,
        base_reserve: output.replacement.base_reserve,
        quote_reserve: output.replacement.quote_reserve,
    });
    write_trader_state(&config.common.state_dir, &owner, state)?;
    emit_order_event(NativeOrderEvent::Replaced {
        auction_id: auction_addresses.auction_id,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rollover_order(
    config: &OrderConfig,
    addresses: &Addresses,
    next_addresses: &AuctionAddresses,
    next_auction: &beatex::Auction,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    intent: &NativeOrderIntent,
    previous: StoredOrder,
    old_auction: beatex::AuctionResult,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let old_addresses = auction_addresses(addresses, previous.auction_id);
    if previous.auction != old_addresses.auction.to_string() {
        return Err("the local previous-auction address is inconsistent".into());
    }
    if old_auction.state != beatex::AuctionState::Settled {
        return Err(match old_auction.state {
            beatex::AuctionState::Cancelled => {
                "the previous auction was cancelled; refund it before placing a new order".into()
            }
            _ => "the previous auction has not settled yet".into(),
        });
    }
    if old_auction.key_epoch != next_auction.key_epoch
        || old_auction.key_epoch != addresses.key_epoch
    {
        return Err("rollover requires both auctions to use the same key epoch".into());
    }
    let old_order_address = Pubkey::from_str(&previous.event)?;
    let old_active_order = Pubkey::find_program_address(
        &[
            b"active-order",
            old_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let mut previous_accounts =
        fetch_multiple_account_bytes(&config.rpc_url, &[old_order_address, old_active_order])?
            .into_iter();
    let old_order = deserialize_anchor_account::<QueuedOrder>(
        previous_accounts.next().flatten(),
        "previous queued order is missing",
    )?;
    let old_active = deserialize_anchor_account::<beatex::ActiveOrder>(
        previous_accounts.next().flatten(),
        "previous active-order account is missing",
    )?;
    if old_order.state != QueuedOrderState::Folded
        || old_order.withdrawal_state != WithdrawalState::Pending
    {
        return Err("previous queued order is not rollover-eligible".into());
    }
    if !old_active.active || old_active.latest_event != old_order_address {
        return Err("previous queued order is not the active revision".into());
    }
    let next_order_address = Pubkey::find_program_address(
        &[
            b"rollover-order",
            next_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let next_active_order = Pubkey::find_program_address(
        &[
            b"active-order",
            next_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let mut rng = ChaCha20Rng::from_entropy();
    let settlement_request = RolloverProofRequest {
        market: addresses.market.to_bytes(),
        auction: old_addresses.auction.to_bytes(),
        order: old_order_address.to_bytes(),
        active_order: old_active_order.to_bytes(),
        owner: owner.to_bytes(),
        key_epoch: addresses.key_epoch.to_bytes(),
        client_order_nonce: old_order.client_order_nonce,
        sequence: old_order.sequence,
        allocation_rule_version: old_auction.allocation_rule_version,
        order_circuit_version: old_auction.order_circuit_version,
        claim_circuit_version: old_auction.claim_circuit_version,
        buy: previous.buy,
        sell: previous.sell,
        base_reserve: previous.base_reserve,
        quote_reserve: previous.quote_reserve,
        order_salt: previous.order_salt,
        expected_order_commitment: old_order.order_commitment,
        clearing_index: old_auction.clearing_index,
        clearing_price: old_auction.clearing_price,
        demand: old_auction.demand_at_clearing,
        supply: old_auction.supply_at_clearing,
        clearing_volume: old_auction.clearing_volume,
        balance_revision: state.revision,
        committee_key: key_epoch.babyjub_encryption_pubkey,
        old_balance: state.balance.opening(),
    };
    let mut next_order_request = OrderProofRequest {
        auction: next_addresses.auction.to_bytes(),
        owner: owner.to_bytes(),
        client_order_nonce: intent.client_order_nonce,
        reference_price: next_auction.reference_price,
        grid_step_bps: next_auction.grid_step_bps,
        committee_key: key_epoch.babyjub_encryption_pubkey,
        buy: intent.buy,
        sell: intent.sell,
        old_balance: state.balance.opening(),
    };
    let settlement = with_rollover_prover(|prover| prover.prove(settlement_request, &mut rng))?;
    next_order_request.old_balance = settlement.credited_balance.clone();
    let next_order = with_order_prover(next_auction.order_circuit_version, |prover| {
        prover.prove(next_order_request, &mut rng)
    })?;
    let output = MarketMakerRolloverProofOutput {
        settlement,
        next_order,
    };
    let rollover_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::RolloverSettlement,
        queue::ROLLOVER_SETTLEMENT_CIRCUIT_VERSION,
    );
    let order_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderSubmission,
        next_auction.order_circuit_version,
    );
    let mut settlement_expected_input = output.settlement.public_input;
    settlement_expected_input.reverse();
    let mut order_expected_input = output.next_order.public_input;
    order_expected_input.reverse();
    ensure_order_submission_time(&config.rpc_url, next_auction)?;
    send_bundle(
        &config.wallet,
        &config.rpc_url,
        "claim previous auction and place next order",
        vec![
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(owner, true, true),
                    meta(addresses.market, false, false),
                    meta(next_addresses.auction, false, false),
                    meta(next_addresses.submission_counter, false, false),
                    meta(next_order_address, true, false),
                    meta(next_active_order, true, false),
                    meta(solana_instructions_sysvar::ID, false, false),
                    meta(anchor_lang::system_program::ID, false, false),
                ],
                data_base64: BASE64.encode(beatex::instruction::PrepareRollover {}.data()),
            },
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(addresses.market, false, false),
                    meta(rollover_key, false, false),
                ],
                data_base64: BASE64.encode(
                    beatex::instruction::VerifyRolloverProof {
                        args: VerifyRolloverProofArgs {
                            circuit_kind: QueueCircuitKind::RolloverSettlement,
                            expected_public_input: settlement_expected_input,
                            proof: proof_input(
                                output.settlement.proof_a,
                                output.settlement.proof_b,
                                output.settlement.proof_c,
                                output.settlement.public_input,
                            ),
                        },
                    }
                    .data(),
                ),
            },
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(owner, false, true),
                    meta(addresses.market, false, false),
                    meta(old_addresses.auction, false, false),
                    meta(addresses.key_epoch, false, false),
                    meta(old_order_address, false, false),
                    meta(old_active_order, false, false),
                    meta(private_balance, false, false),
                    meta(rollover_key, false, false),
                    meta(solana_instructions_sysvar::ID, false, false),
                ],
                data_base64: BASE64.encode(
                    beatex::instruction::BindRolloverSettlement {
                        args: BindRolloverSettlementArgs {
                            settlement: RolloverSettlementArgs {
                                terminal_nullifier: output.settlement.terminal_nullifier,
                                credited_encrypted_base: output.settlement.credited_encrypted_base,
                                credited_encrypted_quote: output
                                    .settlement
                                    .credited_encrypted_quote,
                            },
                            expected_public_input: settlement_expected_input,
                        },
                    }
                    .data(),
                ),
            },
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(addresses.market, false, false),
                    meta(order_key, false, false),
                ],
                data_base64: BASE64.encode(
                    beatex::instruction::VerifyRolloverProof {
                        args: VerifyRolloverProofArgs {
                            circuit_kind: QueueCircuitKind::OrderSubmission,
                            expected_public_input: order_expected_input,
                            proof: proof_input(
                                output.next_order.proof_a,
                                output.next_order.proof_b,
                                output.next_order.proof_c,
                                output.next_order.public_input,
                            ),
                        },
                    }
                    .data(),
                ),
            },
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(owner, false, true),
                    meta(addresses.market, false, false),
                    meta(next_addresses.auction, false, false),
                    meta(addresses.key_epoch, false, false),
                    meta(order_key, false, false),
                    meta(solana_instructions_sysvar::ID, false, false),
                ],
                data_base64: BASE64.encode(
                    beatex::instruction::BindRolloverOrder {
                        args: BindRolloverOrderArgs {
                            next_order: RolloverNextOrderArgs {
                                client_order_nonce: intent.client_order_nonce,
                                order_commitment: output.next_order.order_commitment,
                                ciphertexts: flatten_ciphertexts(output.next_order.ciphertexts),
                                new_encrypted_base: output.next_order.new_encrypted_base,
                                new_encrypted_quote: output.next_order.new_encrypted_quote,
                            },
                            credited_encrypted_base: output.settlement.credited_encrypted_base,
                            credited_encrypted_quote: output.settlement.credited_encrypted_quote,
                            expected_public_input: order_expected_input,
                        },
                    }
                    .data(),
                ),
            },
            BundleInstruction {
                program_id: beatex::ID.to_string(),
                accounts: vec![
                    meta(owner, true, true),
                    meta(addresses.market, false, false),
                    meta(old_addresses.auction, false, false),
                    meta(next_addresses.auction, false, false),
                    meta(addresses.key_epoch, false, false),
                    meta(old_order_address, true, false),
                    meta(old_active_order, true, false),
                    meta(next_addresses.submission_counter, true, false),
                    meta(private_balance, true, false),
                    meta(rollover_key, false, false),
                    meta(order_key, false, false),
                    meta(next_order_address, true, false),
                    meta(next_active_order, true, false),
                    meta(solana_instructions_sysvar::ID, false, false),
                ],
                data_base64: BASE64.encode(beatex::instruction::RolloverOrder {}.data()),
            },
        ],
        vec![],
    )?;
    state.balance = output.next_order.new_balance.into();
    state.revision += 1;
    state.remove_order(previous.auction_id);
    state.upsert_order(StoredOrder {
        auction_id: next_addresses.auction_id,
        auction: next_addresses.auction.to_string(),
        event: next_order_address.to_string(),
        client_order_nonce: intent.client_order_nonce,
        buy: intent.buy,
        sell: intent.sell,
        order_salt: output.next_order.order_salt,
        order_commitment: output.next_order.order_commitment,
        base_reserve: output.next_order.base_reserve,
        quote_reserve: output.next_order.quote_reserve,
    });
    write_trader_state(&config.common.state_dir, &owner, state)?;
    emit_order_event(NativeOrderEvent::Rollover {
        previous_auction_id: previous.auction_id,
        next_auction_id: next_addresses.auction_id,
    });
    if let Some(fill) = fill_event(&old_auction, &previous)? {
        emit_order_event(NativeOrderEvent::Fill {
            auction_id: previous.auction_id,
            side: fill.side.to_owned(),
            base_amount: fill.base_amount,
            quote_amount: fill.quote_amount,
        });
    }
    Ok(())
}

fn order_reserves(
    market: &beatex::Market,
    auction: &beatex::Auction,
    intent: &NativeOrderIntent,
) -> Result<(u64, u64), Box<dyn Error>> {
    let center = i64::from(auction.grid_size / 2);
    let mut max_quote = 0u128;
    for index in 0..usize::from(auction.grid_size) {
        let offset = i64::try_from(index)? - center;
        let multiplier = 10_000i64
            .checked_add(offset * i64::from(auction.grid_step_bps))
            .ok_or("grid multiplier overflow")?;
        if multiplier <= 0 {
            return Err("grid contains a non-positive multiplier".into());
        }
        let price = u128::from(auction.reference_price)
            .checked_mul(multiplier as u128)
            .and_then(|value| value.checked_add(5_000))
            .ok_or("grid price overflow")?
            / 10_000;
        max_quote = max_quote.max(u128::from(intent.buy[index]) * price);
    }
    let max_sell = intent.sell[..usize::from(auction.grid_size)]
        .iter()
        .copied()
        .max()
        .unwrap_or_default();
    let base = u128::from(max_sell)
        .checked_mul(u128::from(market.base_unit_size))
        .ok_or("base reserve overflow")?;
    Ok((u64::try_from(base)?, u64::try_from(max_quote)?))
}

fn top_up_private_balance_if_needed(
    config: &OrderConfig,
    addresses: &Addresses,
    market: &beatex::Market,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    auction: &beatex::Auction,
    intent: &NativeOrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    // Maintain a configured working balance. A deposit only adds to the
    // encrypted available balance and advances its revision; live orders keep
    // their own reserved collateral and are settled against whatever balance
    // is current, so this is safe while orders are outstanding.
    if config.target_base > 0 || config.target_quote > 0 {
        let low = state.balance.base_value < config.target_base / 2
            || state.balance.quote_value < config.target_quote / 2;
        if !low {
            return Ok(());
        }
        let deposit_base = config.target_base.saturating_sub(state.balance.base_value);
        let deposit_quote = config
            .target_quote
            .saturating_sub(state.balance.quote_value);
        println!(
            "private balance top-up to target: {deposit_base} base units and {deposit_quote} quote units"
        );
        return deposit_balance(
            config,
            addresses,
            market,
            key_epoch,
            owner,
            private_balance,
            state,
            deposit_base,
            deposit_quote,
        );
    }
    // Without a target, only cover the first order's reserves: a live order's
    // replacement or rollover already accounts for its reserved collateral.
    if state.has_active_orders() {
        return Ok(());
    }
    let (required_base, required_quote) = order_reserves(market, auction, intent)?;
    let missing_base = required_base.saturating_sub(state.balance.base_value);
    let missing_quote = required_quote.saturating_sub(state.balance.quote_value);
    if missing_base == 0 && missing_quote == 0 {
        return Ok(());
    }
    // Keep the first-order defaults generous enough for the configured maker
    // curve, while also recovering cleanly if an earlier attempt deposited the
    // old, too-small defaults and failed before submitting an order.
    let deposit_base = missing_base.max(config.deposit_base);
    let deposit_quote = missing_quote.max(config.deposit_quote);
    println!(
        "private balance top-up: {} base units and {} quote units",
        deposit_base, deposit_quote
    );
    deposit_balance(
        config,
        addresses,
        market,
        key_epoch,
        owner,
        private_balance,
        state,
        deposit_base,
        deposit_quote,
    )
}

fn execute_order(config: OrderConfig) -> Result<(), Box<dyn Error>> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let intent: NativeOrderIntent = serde_json::from_slice(&input)?;
    execute_order_intent(config, intent)
}

fn execute_order_intent(
    config: OrderConfig,
    intent: NativeOrderIntent,
) -> Result<(), Box<dyn Error>> {
    execute_order_intent_once(config, intent)
}

fn execute_order_intent_once(
    config: OrderConfig,
    intent: NativeOrderIntent,
) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let owner = wallet_pubkey(&config.wallet)?;
    let addresses = addresses(&config.common);
    let private_balance = private_balance_address(&addresses.market, &owner);
    let (market, key_epoch, auction_addresses, auction, onchain_before) = if let Some(auction_id) =
        intent.auction_id
    {
        let auction_addresses = auction_addresses(&addresses, auction_id);
        let mut accounts = fetch_multiple_account_bytes(
            &config.rpc_url,
            &[
                addresses.market,
                addresses.key_epoch,
                auction_addresses.auction,
                private_balance,
            ],
        )?
        .into_iter();
        let market = deserialize_anchor_account::<beatex::Market>(
            accounts.next().flatten(),
            "market has not been initialized",
        )?;
        let key_epoch = deserialize_anchor_account::<beatex::KeyEpoch>(
            accounts.next().flatten(),
            "key epoch has not been initialized",
        )?;
        let auction = deserialize_anchor_account::<beatex::Auction>(
            accounts.next().flatten(),
            "requested auction has not been initialized",
        )?;
        let onchain_before = accounts
            .next()
            .flatten()
            .map(|bytes| beatex::EncryptedPrivateBalance::try_deserialize(&mut bytes.as_slice()))
            .transpose()?;
        (
            market,
            key_epoch,
            auction_addresses,
            auction,
            onchain_before,
        )
    } else {
        let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
            .ok_or("market has not been initialized")?;
        let auction_addresses = current_auction_addresses(&config.rpc_url, &addresses, &market)?
            .ok_or("market has not reached its start slot")?;
        let mut accounts = fetch_multiple_account_bytes(
            &config.rpc_url,
            &[
                addresses.key_epoch,
                auction_addresses.auction,
                private_balance,
            ],
        )?
        .into_iter();
        let key_epoch = deserialize_anchor_account::<beatex::KeyEpoch>(
            accounts.next().flatten(),
            "key epoch has not been initialized",
        )?;
        let auction = deserialize_anchor_account::<beatex::Auction>(
            accounts.next().flatten(),
            "current auction has not been initialized",
        )?;
        let onchain_before = accounts
            .next()
            .flatten()
            .map(|bytes| beatex::EncryptedPrivateBalance::try_deserialize(&mut bytes.as_slice()))
            .transpose()?;
        (
            market,
            key_epoch,
            auction_addresses,
            auction,
            onchain_before,
        )
    };
    if intent.grid_step_bps != market.grid_step_bps {
        return Err(format!(
            "intent grid step {} bps does not match market grid step {} bps",
            intent.grid_step_bps, market.grid_step_bps
        )
        .into());
    }
    if auction.market != addresses.market || auction.auction_id != auction_addresses.auction_id {
        return Err("auction account does not match the requested market and auction".into());
    }
    if intent.reference_price != auction.reference_price {
        return Err(format!(
            "intent reference {} does not match auction reference {}",
            intent.reference_price, auction.reference_price
        )
        .into());
    }
    let mut state =
        read_trader_state(&config.common.state_dir, &owner)?.unwrap_or(TraderPrivateState {
            revision: 0,
            balance: StoredBalance {
                base_value: 0,
                quote_value: 0,
                base_randomness: [0; 32],
                quote_randomness: [0; 32],
            },
            active_orders: Vec::new(),
            active_order: None,
        });
    if let Some(onchain) = &onchain_before {
        if onchain.revision != state.revision {
            return Err(format!(
                "local private-balance revision {} does not match on-chain revision {}",
                state.revision, onchain.revision
            )
            .into());
        }
    } else if state.revision != 0 {
        return Err("local private balance exists but its on-chain account is missing".into());
    }
    if onchain_before.is_none() {
        initialize_private_balance(&config, &addresses, owner, private_balance)?;
    }
    if state.revision == 0 {
        deposit_balance(
            &config,
            &addresses,
            &market,
            &key_epoch,
            owner,
            private_balance,
            &mut state,
            config.deposit_base,
            config.deposit_quote,
        )?;
    }
    top_up_private_balance_if_needed(
        &config,
        &addresses,
        &market,
        &key_epoch,
        owner,
        private_balance,
        &auction,
        &intent,
        &mut state,
    )?;
    if state.order(auction_addresses.auction_id).is_some() {
        replace_order(
            &config,
            &addresses,
            &auction_addresses,
            &auction,
            &key_epoch,
            owner,
            private_balance,
            &intent,
            &mut state,
        )
    } else {
        let previous_orders = state
            .active_orders
            .iter()
            .filter(|order| order.auction_id < auction_addresses.auction_id)
            .collect::<Vec<_>>();
        let mut terminal_order = None;
        for chunk in previous_orders.chunks(100) {
            let previous_addresses = chunk
                .iter()
                .map(|order| crate::auction_addresses(&addresses, order.auction_id).auction)
                .collect::<Vec<_>>();
            let previous_accounts =
                fetch_multiple_account_bytes(&config.rpc_url, &previous_addresses)?;
            for (previous, account_bytes) in chunk.iter().zip(previous_accounts) {
                // Finished auctions are compacted to an `AuctionResult`; any
                // other form is still pending.
                let Some(account_bytes) = account_bytes.filter(|bytes| {
                    bytes.starts_with(
                        <beatex::AuctionResult as anchor_lang::Discriminator>::DISCRIMINATOR,
                    )
                }) else {
                    continue;
                };
                let previous_auction =
                    beatex::AuctionResult::try_deserialize(&mut account_bytes.as_slice())?;
                if matches!(
                    previous_auction.state,
                    beatex::AuctionState::Settled | beatex::AuctionState::Cancelled
                ) {
                    terminal_order = Some(((*previous).clone(), previous_auction));
                    break;
                }
            }
            if terminal_order.is_some() {
                break;
            }
        }
        match terminal_order {
            Some((previous, previous_auction))
                if previous_auction.state == beatex::AuctionState::Cancelled =>
            {
                execute_balance_settlement(BalanceSettlementConfig {
                    common: config.common.clone(),
                    wallet: config.wallet.clone(),
                    rpc_url: config.rpc_url.clone(),
                    auction_id: previous.auction_id,
                    kind: PrivateWithdrawalKind::Refund,
                })?;
                state = read_trader_state(&config.common.state_dir, &owner)?
                    .ok_or("private state disappeared after internal refund")?;
                submit_order(
                    &config,
                    &addresses,
                    &auction_addresses,
                    &auction,
                    &key_epoch,
                    owner,
                    private_balance,
                    &intent,
                    &mut state,
                )
            }
            Some((previous, previous_auction))
                if previous_auction.state == beatex::AuctionState::Settled =>
            {
                rollover_order(
                    &config,
                    &addresses,
                    &auction_addresses,
                    &auction,
                    &key_epoch,
                    owner,
                    private_balance,
                    &intent,
                    previous,
                    previous_auction,
                    &mut state,
                )
            }
            _ => submit_order(
                &config,
                &addresses,
                &auction_addresses,
                &auction,
                &key_epoch,
                owner,
                private_balance,
                &intent,
                &mut state,
            ),
        }
    }
}

/// Execute one order directly in this process and return the order lifecycle
/// events emitted by the transaction/prover path. This is the native API used
/// by long-lived Rust clients such as the market maker.
pub fn execute_native_order(
    config: NativeOrderConfig,
    intent: NativeOrderIntent,
) -> Result<Vec<NativeOrderEvent>, String> {
    if wallet_file_is_browser_session(&config.wallet).map_err(|error| error.to_string())? {
        return Err(
            "native order execution requires a Solana JSON keypair, not a browser-wallet session"
                .to_owned(),
        );
    }
    read_v1_keypair_file(&config.wallet)
        .map_err(|error| format!("native order wallet is not a valid Solana keypair: {error}"))?;
    let order_config = OrderConfig {
        common: CommonConfig {
            base_mint: config.base_mint,
            quote_mint: config.quote_mint,
            state_dir: config.state_dir,
        },
        wallet: config.wallet,
        base_source: config.base_source,
        quote_source: config.quote_source,
        rpc_url: config.rpc_url,
        deposit_base: config.deposit_base,
        deposit_quote: config.deposit_quote,
        target_base: config.target_base,
        target_quote: config.target_quote,
    };
    let previous_capture = NATIVE_ORDER_EVENTS
        .with(|capture| std::mem::replace(&mut *capture.borrow_mut(), Some(Vec::new())));
    let result = execute_order_intent(order_config, intent).map_err(|error| error.to_string());
    let events = NATIVE_ORDER_EVENTS.with(|capture| {
        let mut capture = capture.borrow_mut();
        let events = capture.take().unwrap_or_default();
        *capture = previous_capture;
        events
    });
    result.map(|()| events)
}

fn execute_market_status(config: MarketStatusConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let slot = current_slot(&config.rpc_url)?;
    println!("current-slot\t{slot}");
    println!("market-start-slot\t{}", market.market_start_slot);
    println!(
        "submission-duration-slots\t{}",
        market.submission_duration_slots
    );
    println!(
        "aggregation-duration-slots\t{}",
        market.aggregation_duration_slots
    );
    println!(
        "decryption-duration-slots\t{}",
        market.decryption_duration_slots
    );
    println!("order-circuit-version\t{}", market.order_circuit_version);
    if slot < market.market_start_slot {
        println!("started\tfalse");
    } else {
        println!("started\ttrue");
        println!(
            "current-auction-id\t{}",
            (slot - market.market_start_slot) / market.submission_duration_slots
        );
    }
    Ok(())
}

fn execute_pipeline_status(config: MarketStatusConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let slot = current_slot(&config.rpc_url)?;
    println!("current-slot\t{slot}");
    println!("market-start-slot\t{}", market.market_start_slot);
    println!(
        "submission-duration-slots\t{}",
        market.submission_duration_slots
    );
    println!(
        "aggregation-duration-slots\t{}",
        market.aggregation_duration_slots
    );
    println!(
        "decryption-duration-slots\t{}",
        market.decryption_duration_slots
    );
    println!("order-circuit-version\t{}", market.order_circuit_version);
    if slot < market.market_start_slot {
        println!("started\tfalse");
        return Ok(());
    }
    let current_id = (slot - market.market_start_slot) / market.submission_duration_slots;
    println!("started\ttrue");
    println!("current-auction-id\t{current_id}");

    let lifecycle_span = market
        .submission_duration_slots
        .checked_add(market.aggregation_duration_slots)
        .and_then(|value| value.checked_add(market.decryption_duration_slots))
        .ok_or("pipeline duration overflow")?;
    let pipeline_depth = lifecycle_span
        .checked_add(market.submission_duration_slots - 1)
        .ok_or("pipeline depth overflow")?
        / market.submission_duration_slots;
    let first_id = current_id.saturating_sub(pipeline_depth);
    let auction_ids = (first_id..=current_id).collect::<Vec<_>>();
    let mut account_addresses = Vec::with_capacity(auction_ids.len() * 3);
    for auction_id in &auction_ids {
        let derived = auction_addresses(&addresses, *auction_id);
        account_addresses.extend([
            derived.auction,
            derived.submission_counter,
            derived.accumulator,
        ]);
    }
    let account_bytes =
        fetch_multiple_account_bytes(&config.rpc_url, account_addresses.as_slice())?;
    for (auction_id, accounts) in auction_ids.iter().zip(account_bytes.chunks_exact(3)) {
        let Some(auction_bytes) = accounts[0].as_deref() else {
            continue;
        };
        // Empty-auction reclamation closes all three accounts in one
        // transaction. Different RPC nodes can briefly expose a mixed view
        // while that transaction propagates; skip that snapshot and let the
        // next poll observe the consistent state.
        let Some(counter_bytes) = accounts[1].as_deref() else {
            continue;
        };
        let Some(accumulator_bytes) = accounts[2].as_deref() else {
            continue;
        };
        let auction = beatex::Auction::try_deserialize(&mut auction_bytes.as_ref())?;
        let counter = SubmissionCounter::try_deserialize(&mut counter_bytes.as_ref())?;
        let accumulator = AggregateAccumulator::try_deserialize(&mut accumulator_bytes.as_ref())?;
        let state = match auction.state {
            beatex::AuctionState::Pending => "pending",
            beatex::AuctionState::Settled => "settled",
            beatex::AuctionState::Cancelled => "cancelled",
        };
        println!(
            "auction\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            auction_id,
            state,
            auction.open_slot,
            auction.submission_end_slot,
            auction.aggregation_deadline_slot,
            auction.decryption_deadline_slot,
            counter.accepted_order_count,
            accumulator.folded_order_count,
            counter
                .accepted_order_count
                .saturating_sub(accumulator.folded_order_count),
        );
    }
    Ok(())
}

fn fetch_pipeline_auctions(
    rpc_url: &str,
    addresses: &Addresses,
    market: &beatex::Market,
    current_id: u64,
) -> Result<Vec<PipelineAuction>, Box<dyn Error>> {
    let lifecycle_span = market
        .submission_duration_slots
        .checked_add(market.aggregation_duration_slots)
        .and_then(|value| value.checked_add(market.decryption_duration_slots))
        .ok_or("pipeline duration overflow")?;
    let pipeline_depth = lifecycle_span
        .checked_add(market.submission_duration_slots - 1)
        .ok_or("pipeline depth overflow")?
        / market.submission_duration_slots;
    let first_id = current_id.saturating_sub(pipeline_depth);
    let auction_ids = (first_id..=current_id).collect::<Vec<_>>();
    let mut account_addresses = Vec::with_capacity(auction_ids.len() * 3);
    for auction_id in &auction_ids {
        let derived = auction_addresses(addresses, *auction_id);
        account_addresses.extend([
            derived.auction,
            derived.submission_counter,
            derived.accumulator,
        ]);
    }
    let account_bytes = fetch_multiple_account_bytes(rpc_url, &account_addresses)?;
    let mut snapshots = Vec::new();
    for (auction_id, accounts) in auction_ids.iter().zip(account_bytes.chunks_exact(3)) {
        let Some(auction_bytes) = accounts[0].as_deref() else {
            continue;
        };
        let (Some(counter_bytes), Some(accumulator_bytes)) =
            (accounts[1].as_deref(), accounts[2].as_deref())
        else {
            continue;
        };
        let auction = beatex::Auction::try_deserialize(&mut auction_bytes.as_ref())?;
        let counter = SubmissionCounter::try_deserialize(&mut counter_bytes.as_ref())?;
        let accumulator = AggregateAccumulator::try_deserialize(&mut accumulator_bytes.as_ref())?;
        snapshots.push(PipelineAuction {
            auction_id: *auction_id,
            state: auction.state,
            submission_end_slot: auction.submission_end_slot,
            aggregation_deadline_slot: auction.aggregation_deadline_slot,
            decryption_deadline_slot: auction.decryption_deadline_slot,
            accepted_orders: counter.accepted_order_count,
            folded_orders: accumulator.folded_order_count,
        });
    }
    Ok(snapshots)
}

fn committee_action_needed(snapshot: &PipelineAuction, current_slot: u64) -> bool {
    if snapshot.state != beatex::AuctionState::Pending {
        return false;
    }
    let queued = snapshot
        .accepted_orders
        .saturating_sub(snapshot.folded_orders);
    (queued > 0
        && current_slot < snapshot.aggregation_deadline_slot
        && (queued >= queue_hash::FOLD_BATCH_SIZE as u64
            || current_slot.saturating_add(FINAL_FOLD_LEAD_SLOTS) >= snapshot.submission_end_slot))
        || (snapshot.accepted_orders > 0 && current_slot >= snapshot.decryption_deadline_slot)
        || (snapshot.accepted_orders > 0
            && current_slot >= snapshot.aggregation_deadline_slot
            && snapshot.accepted_orders != snapshot.folded_orders)
        || (current_slot >= snapshot.submission_end_slot
            && snapshot.accepted_orders > 0
            && snapshot.accepted_orders == snapshot.folded_orders)
        || (current_slot >= snapshot.submission_end_slot
            && snapshot.accepted_orders == 0
            && snapshot.folded_orders == 0)
}

fn process_committee_auction(
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
) -> Result<(), Box<dyn Error>> {
    let job_started = Instant::now();
    let addresses = addresses(&common);
    let auction_addresses = auction_addresses(&addresses, auction_id);
    // One worker carries an auction from its final fold straight into
    // publication, so settlement never waits for the dispatcher's next pass.
    loop {
        let Some((auction, counter, accumulator)) =
            fetch_committee_auction_accounts(&rpc_url, &auction_addresses)?
        else {
            // The pipeline may have queued this worker immediately before another
            // worker reclaimed an empty auction.
            return Ok(());
        };
        if auction.state != beatex::AuctionState::Pending {
            return Ok(());
        }
        let slot = current_slot(&rpc_url)?;
        let queued = counter
            .accepted_order_count
            .saturating_sub(accumulator.folded_order_count);
        if queued > 0
            && slot < auction.aggregation_deadline_slot
            && (queued >= queue_hash::FOLD_BATCH_SIZE as u64
                || slot.saturating_add(FINAL_FOLD_LEAD_SLOTS) >= auction.submission_end_slot)
        {
            execute_fold_with_snapshot(
                FoldConfig {
                    common: common.clone(),
                    wallet: wallet.clone(),
                    rpc_url: rpc_url.clone(),
                    auction_id,
                    max_batches: 1,
                    coordinator_logs: true,
                },
                Some((auction, accumulator)),
                Some(job_started),
            )?;
            continue;
        }
        if counter.accepted_order_count > 0
            && counter.accepted_order_count == accumulator.folded_order_count
            && slot < auction.submission_end_slot
            && slot.saturating_add(FINAL_FOLD_LEAD_SLOTS) >= auction.submission_end_slot
        {
            // Fully folded just before the close: wait for it, then publish.
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        if slot >= auction.submission_end_slot
            && counter.accepted_order_count == 0
            && accumulator.folded_order_count == 0
        {
            let market = fetch_anchor_account::<beatex::Market>(&rpc_url, &addresses.market)?
                .ok_or("market has not been initialized")?;
            send(
                &wallet,
                &rpc_url,
                &format!("close empty auction {auction_id}"),
                vec![
                    meta(addresses.market, false, false),
                    meta(auction_addresses.auction, true, false),
                    meta(auction_addresses.submission_counter, true, false),
                    meta(auction_addresses.accumulator, true, false),
                    meta(market.authority, true, false),
                ]
                .into_iter()
                .chain(event_cpi_metas())
                .collect(),
                beatex::instruction::CloseEmptyAuction {}.data(),
                vec![],
            )?;
            coordinator_auction_log(
                auction_id,
                &format!(
                    "empty auction storage reclaimed, took {:.1}s",
                    job_started.elapsed().as_secs_f64()
                ),
            );
            return Ok(());
        }
        if counter.accepted_order_count > 0
            && (slot >= auction.decryption_deadline_slot
                || (slot >= auction.aggregation_deadline_slot
                    && counter.accepted_order_count != accumulator.folded_order_count))
        {
            execute_cancel(FoldConfig {
                common: common.clone(),
                wallet: wallet.clone(),
                rpc_url: rpc_url.clone(),
                auction_id,
                max_batches: 1,
                coordinator_logs: true,
            })?;
        } else if slot >= auction.submission_end_slot
            && counter.accepted_order_count > 0
            && counter.accepted_order_count == accumulator.folded_order_count
        {
            execute_publish_with_started(
                PublishConfig {
                    common: common.clone(),
                    wallet: wallet.clone(),
                    rpc_url: rpc_url.clone(),
                    auction_id,
                    coordinator_logs: true,
                },
                job_started,
            )?;
        }
        return Ok(());
    }
}

fn run_committee_pipeline(common: CommonConfig, wallet: PathBuf, rpc_url: String) {
    let addresses = addresses(&common);
    let market = loop {
        match fetch_anchor_account::<beatex::Market>(&rpc_url, &addresses.market) {
            Ok(Some(market)) => break market,
            Ok(None) => coordinator_error(&"market has not been initialized"),
            Err(error) => coordinator_error(error.as_ref()),
        }
        thread::sleep(Duration::from_millis(500));
    };
    if let Err(error) = devnet_fast_fold_prover().and_then(|_| {
        devnet_fast_publication_prover()
            .and_then(|prover| prover.prepare_decryption_bound(market.max_aggregate_quantity))
    }) {
        coordinator_error(&format!("committee prover initialization failed: {error}"));
        return;
    }
    let (completion_sender, completion_receiver) = mpsc::channel::<u64>();
    let mut in_flight = HashSet::new();
    let mut retry_after = HashMap::<u64, Instant>::new();
    let mut last_snapshot_slot = None;
    loop {
        let mut snapshot_dirty = false;
        while let Ok(auction_id) = completion_receiver.try_recv() {
            in_flight.remove(&auction_id);
            retry_after.insert(auction_id, Instant::now() + Duration::from_secs(3));
            snapshot_dirty = true;
        }
        let now = Instant::now();
        retry_after.retain(|_, deadline| *deadline > now);
        let slot = match current_slot(&rpc_url) {
            Ok(slot) => slot,
            Err(error) => {
                coordinator_error(error.as_ref());
                thread::sleep(Duration::from_millis(250));
                continue;
            }
        };
        if slot < market.market_start_slot {
            thread::sleep(Duration::from_millis(250));
            continue;
        }
        if last_snapshot_slot == Some(slot) && !snapshot_dirty {
            thread::sleep(Duration::from_millis(250));
            continue;
        }
        let current_id = (slot - market.market_start_slot) / market.submission_duration_slots;
        match fetch_pipeline_auctions(&rpc_url, &addresses, &market, current_id) {
            Ok(snapshots) => {
                last_snapshot_slot = Some(slot);
                for snapshot in snapshots {
                    if in_flight.len() >= MAX_COMMITTEE_AUCTION_WORKERS {
                        break;
                    }
                    if committee_action_needed(&snapshot, slot)
                        && !retry_after.contains_key(&snapshot.auction_id)
                        && in_flight.insert(snapshot.auction_id)
                    {
                        let worker_common = common.clone();
                        let worker_wallet = wallet.clone();
                        let worker_rpc = rpc_url.clone();
                        let worker_sender = completion_sender.clone();
                        thread::spawn(move || {
                            if let Err(error) = process_committee_auction(
                                worker_common,
                                worker_wallet,
                                worker_rpc,
                                snapshot.auction_id,
                            ) {
                                coordinator_error(&format!(
                                    "auction {} processing failed: {error}",
                                    snapshot.auction_id
                                ));
                            }
                            let _ = worker_sender.send(snapshot.auction_id);
                        });
                    }
                }
            }
            Err(error) => coordinator_error(error.as_ref()),
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn run_committee_bot(
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
) -> Result<(), Box<dyn Error>> {
    // Bot execution is deliberately keypair-only. Browser-wallet sessions use
    // the interactive Node bridge elsewhere in the CLI and must never enter a
    // long-running committee process.
    read_v1_keypair_file(&wallet)?;
    let payer = wallet_pubkey(&wallet)?;
    ensure_committee_payer_can_initialize(&rpc_url, &payer)?;
    let pipeline_common = common.clone();
    let pipeline_wallet = wallet.clone();
    let pipeline_rpc = rpc_url.clone();
    thread::spawn(move || run_committee_pipeline(pipeline_common, pipeline_wallet, pipeline_rpc));
    watch_and_initialize_auctions(common, wallet, rpc_url)
}

fn execute_auction_status(config: AuctionStatusConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let Some(auction_bytes) =
        fetch_multiple_account_bytes(&config.rpc_url, &[auction_addresses.auction])?
            .into_iter()
            .next()
            .flatten()
    else {
        println!("exists\tfalse");
        println!("auction-id\t{}", config.auction_id);
        return Ok(());
    };
    if auction_bytes
        .starts_with(<beatex::AuctionResult as anchor_lang::Discriminator>::DISCRIMINATOR)
    {
        // Finished auctions are compacted; their queue accounts are closed.
        let result = beatex::AuctionResult::try_deserialize(&mut auction_bytes.as_slice())?;
        println!("exists\ttrue");
        println!("auction-id\t{}", config.auction_id);
        println!(
            "state\t{}",
            if result.state == beatex::AuctionState::Settled {
                "settled"
            } else {
                "cancelled"
            }
        );
        println!("reference-price\t{}", result.reference_price);
        println!("clearing-price\t{}", result.clearing_price);
        println!("clearing-volume\t{}", result.clearing_volume);
        return Ok(());
    }
    let auction = beatex::Auction::try_deserialize(&mut auction_bytes.as_slice())?;
    let current_slot = current_slot(&config.rpc_url)?;
    let counter = fetch_anchor_account::<SubmissionCounter>(
        &config.rpc_url,
        &auction_addresses.submission_counter,
    )?
    .ok_or("submission counter is missing")?;
    let accumulator = fetch_anchor_account::<AggregateAccumulator>(
        &config.rpc_url,
        &auction_addresses.accumulator,
    )?
    .ok_or("aggregate accumulator is missing")?;
    let state = match auction.state {
        beatex::AuctionState::Pending => "pending",
        beatex::AuctionState::Settled => "settled",
        beatex::AuctionState::Cancelled => "cancelled",
    };
    println!("exists\ttrue");
    println!("auction-id\t{}", config.auction_id);
    println!("state\t{state}");
    println!("current-slot\t{current_slot}");
    println!("open-slot\t{}", auction.open_slot);
    println!("submission-end-slot\t{}", auction.submission_end_slot);
    println!(
        "aggregation-deadline-slot\t{}",
        auction.aggregation_deadline_slot
    );
    println!(
        "decryption-deadline-slot\t{}",
        auction.decryption_deadline_slot
    );
    println!("accepted-orders\t{}", counter.accepted_order_count);
    println!("folded-orders\t{}", accumulator.folded_order_count);
    println!(
        "queued-orders\t{}",
        counter
            .accepted_order_count
            .saturating_sub(accumulator.folded_order_count)
    );
    Ok(())
}

fn execute_fold(config: FoldConfig) -> Result<(), Box<dyn Error>> {
    execute_fold_with_snapshot(config, None, None)
}

fn execute_fold_with_snapshot(
    config: FoldConfig,
    initial_snapshot: Option<(beatex::Auction, AggregateAccumulator)>,
    job_started: Option<Instant>,
) -> Result<(), Box<dyn Error>> {
    let started = job_started.unwrap_or_else(Instant::now);
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    if config.max_batches == 0 {
        return Err("max-batches must be positive".into());
    }
    let aggregator = wallet_pubkey(&config.wallet)?;
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let (auction, initial_accumulator) = if let Some(snapshot) = initial_snapshot {
        snapshot
    } else {
        let (auction, _, accumulator) =
            fetch_committee_auction_accounts(&config.rpc_url, &auction_addresses)?
                .ok_or("auction has not been initialized")?;
        (auction, accumulator)
    };
    if auction.market != addresses.market {
        return Err("auction belongs to a different market".into());
    }
    let fold_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::AggregateFold,
        queue::AGGREGATE_FOLD_CIRCUIT_VERSION,
    );
    let mut rng = ChaCha20Rng::from_entropy();
    let mut total_folded = 0usize;
    let mut completed_batches = 0usize;
    let mut cached_accumulator = Some(initial_accumulator);

    loop {
        if completed_batches >= config.max_batches {
            break;
        }
        let accumulator = if let Some(accumulator) = cached_accumulator.take() {
            accumulator
        } else {
            fetch_committee_auction_accounts(&config.rpc_url, &auction_addresses)?
                .ok_or("aggregate accumulator is missing")?
                .2
        };
        let batch = fetch_pending_orders(
            &config.rpc_url,
            &auction_addresses.submission_counter,
            accumulator.folded_order_count,
            queue_hash::FOLD_BATCH_SIZE,
        )?;
        if batch.is_empty() {
            break;
        }
        let old_aggregate = ciphertext_array(&accumulator.ciphertexts)?;
        let previous_addresses = batch
            .iter()
            .filter_map(|(_, event)| {
                (event.previous_event != Pubkey::default()).then_some(event.previous_event)
            })
            .collect::<Vec<_>>();
        let previous_accounts = if previous_addresses.is_empty() {
            Vec::new()
        } else {
            fetch_multiple_account_bytes(&config.rpc_url, &previous_addresses)?
        };
        let mut previous_accounts = previous_accounts.into_iter();
        let mut events = Vec::with_capacity(batch.len());
        for (event_address, event) in &batch {
            let previous_ciphertexts = if event.previous_event == Pubkey::default() {
                [identity_ciphertext(); queue::QUEUED_CIPHERTEXT_COUNT]
            } else {
                let bytes = previous_accounts
                    .next()
                    .flatten()
                    .ok_or("replacement event references a missing previous event")?;
                let previous = QueuedOrder::try_deserialize(&mut bytes.as_slice())?;
                ciphertext_array(&previous.ciphertexts)?
            };
            events.push(FoldEventInput {
                event: event_address.to_bytes(),
                previous_ciphertexts,
                ciphertexts: ciphertext_array(&event.ciphertexts)?,
            });
        }
        let output = devnet_fast_fold_prover()?.prove(
            FoldProofRequest {
                auction: auction_addresses.auction.to_bytes(),
                old_folded_count: accumulator.folded_order_count,
                old_aggregate,
                events,
            },
            &mut rng,
        )?;
        let mut accounts = vec![
            meta(aggregator, false, true),
            meta(addresses.market, false, false),
            meta(auction_addresses.auction, false, false),
            meta(auction_addresses.submission_counter, true, false),
            meta(auction_addresses.accumulator, true, false),
            meta(fold_key, false, false),
        ];
        accounts.extend(
            batch
                .iter()
                .map(|(event_address, _)| meta(*event_address, true, false)),
        );
        let label = format!("fold {} encrypted queue event(s)", batch.len());
        let data = beatex::instruction::FoldQueuedOrders {
            args: FoldQueuedOrdersArgs {
                new_aggregate: flatten_ciphertexts(output.new_aggregate),
                proof: proof_input(
                    output.proof_a,
                    output.proof_b,
                    output.proof_c,
                    output.public_input,
                ),
            },
        }
        .data();
        if config.coordinator_logs {
            send_committee(&config.wallet, &config.rpc_url, &label, accounts, data)?;
        } else {
            send(
                &config.wallet,
                &config.rpc_url,
                &label,
                accounts,
                data,
                vec![],
            )?;
        }
        total_folded += batch.len();
        completed_batches += 1;
    }
    if total_folded > 0 {
        if config.coordinator_logs {
            coordinator_auction_log(
                config.auction_id,
                &format!(
                    "folding [{total_folded}] orders... folded, took {:.1}s",
                    started.elapsed().as_secs_f64()
                ),
            );
        } else {
            println!(
                "BEATEX_EVENT\tauction\t{}\tfolded\t{}\t{:.1}",
                config.auction_id,
                total_folded,
                started.elapsed().as_secs_f64()
            );
        }
    }
    Ok(())
}

fn execute_publish(config: PublishConfig) -> Result<(), Box<dyn Error>> {
    execute_publish_with_started(config, Instant::now())
}

fn execute_publish_with_started(
    config: PublishConfig,
    started: Instant,
) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let mut account_bytes = fetch_multiple_account_bytes(
        &config.rpc_url,
        &[
            addresses.market,
            auction_addresses.auction,
            auction_addresses.submission_counter,
            auction_addresses.accumulator,
            addresses.key_epoch,
        ],
    )?
    .into_iter();
    let market = deserialize_anchor_account::<beatex::Market>(
        account_bytes.next().flatten(),
        "market has not been initialized",
    )?;
    let auction = deserialize_anchor_account::<beatex::Auction>(
        account_bytes.next().flatten(),
        "auction has not been initialized",
    )?;
    let counter = deserialize_anchor_account::<SubmissionCounter>(
        account_bytes.next().flatten(),
        "submission counter is missing",
    )?;
    let accumulator = deserialize_anchor_account::<AggregateAccumulator>(
        account_bytes.next().flatten(),
        "aggregate accumulator is missing",
    )?;
    let key_epoch = deserialize_anchor_account::<beatex::KeyEpoch>(
        account_bytes.next().flatten(),
        "key epoch has not been initialized",
    )?;
    let slot = current_slot(&config.rpc_url)?;
    if slot < auction.submission_end_slot {
        return Err(format!(
            "auction collection remains open for {} slot(s)",
            auction.submission_end_slot - slot
        )
        .into());
    }
    if slot >= auction.decryption_deadline_slot {
        return Err("auction publication deadline has passed".into());
    }
    if counter.accepted_order_count != accumulator.folded_order_count {
        return Err(format!(
            "aggregation is incomplete: accepted {}, folded {}",
            counter.accepted_order_count, accumulator.folded_order_count
        )
        .into());
    }
    if counter.accepted_order_count == 0 {
        return Err("cannot publish an empty auction aggregate".into());
    }
    let aggregate = ciphertext_array(&accumulator.ciphertexts)?;
    let participant_ids = [1, 4, 7];
    let prover = devnet_fast_publication_prover()?;
    if prover.committee().encryption_key != key_epoch.babyjub_encryption_pubkey
        || prover.committee().participant_public_shares
            != key_epoch.babyjub_participant_public_shares
    {
        return Err("local committee material does not match the active key epoch".into());
    }
    let discrete_log_bound = counter
        .accepted_order_count
        .checked_mul(market.max_order_quantity)
        .ok_or("aggregate plaintext bound overflow")?
        .min(market.max_aggregate_quantity);
    let (demand, supply) =
        prover.decrypt_aggregate(aggregate, participant_ids, discrete_log_bound)?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove(
        AggregatePublicationRequest {
            auction: auction_addresses.auction.to_bytes(),
            accepted_order_count: counter.accepted_order_count,
            participant_ids,
            aggregate,
            demand,
            supply,
        },
        &mut rng,
    )?;
    let publication_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::AggregatePublication,
        queue::AGGREGATE_PUBLICATION_CIRCUIT_VERSION,
    );
    let grid_size = auction.active_grid_size();
    let grid = beatex::auction::PriceGrid {
        reference_price: auction.reference_price,
        grid_step_bps: auction.grid_step_bps,
        level_count: auction.grid_size as u16,
    };
    let tie_break_price = if market.last_clearing_price == 0 {
        auction.reference_price
    } else {
        market.last_clearing_price
    };
    let clearing = beatex::auction::clear_auction_with_tiebreak(
        &grid,
        &output.demand[..grid_size],
        &output.supply[..grid_size],
        tie_break_price,
    )
    .map_err(|error| format!("cannot calculate aggregate clearing result: {error:?}"))?;
    let accounts = vec![
        meta(addresses.market, true, false),
        meta(auction_addresses.auction, true, false),
        meta(addresses.key_epoch, false, false),
        meta(auction_addresses.submission_counter, true, false),
        meta(auction_addresses.accumulator, true, false),
        meta(publication_key, false, false),
        meta(market.authority, true, false),
    ]
    .into_iter()
    .chain(event_cpi_metas())
    .collect::<Vec<_>>();
    let data = beatex::instruction::PublishAndClear {
        args: PublishAggregateCurvesArgs {
            participant_ids: output.participant_ids,
            demand: output.demand,
            supply: output.supply,
            proof: proof_input(
                output.proof_a,
                output.proof_b,
                output.proof_c,
                output.public_input,
            ),
        },
    }
    .data();
    if config.coordinator_logs {
        send_committee(
            &config.wallet,
            &config.rpc_url,
            "publish aggregate curves and clear auction",
            accounts,
            data,
        )?;
    } else {
        send(
            &config.wallet,
            &config.rpc_url,
            "publish aggregate curves and clear auction",
            accounts,
            data,
            vec![],
        )?;
    }
    if config.coordinator_logs {
        // The auction two ahead can be initialized early with this price only
        // if it lands by close + (submission duration - preinitialization) slots.
        let settled_after = current_slot_with_timeout(&config.rpc_url, "2")
            .map(|slot| {
                format!(
                    "close+{} slots",
                    slot.saturating_sub(auction.submission_end_slot)
                )
            })
            .unwrap_or_else(|_| "close+? slots".to_owned());
        coordinator_auction_log(
            config.auction_id,
            &format!(
                "aggregate published at {settled_after}, took {:.1}s (price {}, volume {})",
                started.elapsed().as_secs_f64(),
                clearing.price,
                clearing.executable_volume
            ),
        );
    } else {
        println!(
            "auction {} settled at price {} with volume {} (level {})",
            config.auction_id, clearing.price, clearing.executable_volume, clearing.index
        );
        println!(
            "BEATEX_EVENT\tauction\t{}\tpublished\t{:.1}",
            config.auction_id,
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn execute_cancel(config: FoldConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction = auction_addresses(&addresses, config.auction_id);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    send(
        &config.wallet,
        &config.rpc_url,
        &format!("cancel expired auction {}", config.auction_id),
        vec![
            meta(addresses.market, false, false),
            meta(auction.auction, true, false),
            meta(auction.submission_counter, true, false),
            meta(auction.accumulator, true, false),
            meta(market.authority, true, false),
        ]
        .into_iter()
        .chain(event_cpi_metas())
        .collect(),
        beatex::instruction::CancelPendingAuction {}.data(),
        vec![],
    )?;
    let cancelled =
        fetch_anchor_account::<beatex::AuctionResult>(&config.rpc_url, &auction.auction)?
            .ok_or("cancelled auction account disappeared")?;
    if cancelled.state != beatex::AuctionState::Cancelled {
        return Err("auction did not enter the cancelled state".into());
    }
    if config.coordinator_logs {
        coordinator_auction_log(config.auction_id, "cancelled after deadline");
    } else {
        println!("auction {} cancelled", config.auction_id);
        println!("BEATEX_EVENT\tauction\t{}\tcancelled", config.auction_id);
    }
    Ok(())
}

fn apply_one_vault(
    config: &ApplyVaultConfig,
    addresses: &Addresses,
    market: &beatex::Market,
    authority: Pubkey,
    vault_keypair: &solana_zk_sdk::encryption::elgamal::ElGamalKeypair,
    vault_ae_key: &solana_zk_sdk::encryption::auth_encryption::AeKey,
    is_base_vault: bool,
) -> Result<(), Box<dyn Error>> {
    let (mint, vault, label) = if is_base_vault {
        (market.base_mint, market.base_vault, "base")
    } else {
        (market.quote_mint, market.quote_vault, "quote")
    };
    let data = fetch_account_bytes(&config.rpc_url, &vault)?;
    let state = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    let confidential = state.get_extension::<ConfidentialTransferAccount>()?;
    let decode = |bytes: &[u8]| {
        ElGamalCiphertext::from_bytes(bytes)
            .ok_or_else(|| -> Box<dyn Error> { "invalid vault ciphertext".into() })
    };
    let available = decode(&confidential.available_balance.0)?
        .decrypt_u32(vault_keypair.secret())
        .ok_or("vault available balance exceeds the supported Devnet bound")?;
    let pending_low = decode(&confidential.pending_balance_lo.0)?
        .decrypt_u32(vault_keypair.secret())
        .ok_or("vault pending-low balance could not be decrypted")?;
    let pending_high = decode(&confidential.pending_balance_hi.0)?
        .decrypt_u32(vault_keypair.secret())
        .ok_or("vault pending-high balance could not be decrypted")?;
    let pending = pending_high
        .checked_shl(16)
        .and_then(|high| high.checked_add(pending_low))
        .ok_or("vault pending balance overflow")?;
    if pending == 0 {
        println!("{label} vault has no pending confidential deposits");
        return Ok(());
    }
    let new_available = available
        .checked_add(pending)
        .ok_or("vault available balance overflow")?;
    let pending_counter = u64::from(confidential.pending_balance_credit_counter);
    send(
        &config.wallet,
        &config.rpc_url,
        &format!("apply {label} vault pending balance"),
        vec![
            meta(authority, false, true),
            meta(addresses.market, false, false),
            meta(addresses.key_epoch, false, false),
            meta(mint, false, false),
            meta(vault, true, false),
            meta(addresses.vault_authority, false, false),
            meta(Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?, false, false),
        ],
        beatex::instruction::ApplyVaultPendingBalance {
            is_base_vault,
            expected_pending_balance_credit_counter: pending_counter,
            new_decryptable_available_balance: vault_ae_key.encrypt(new_available).to_bytes(),
        }
        .data(),
        vec![],
    )?;
    println!(
        "{label} vault applied {pending} pending units; available balance is now {new_available}"
    );
    Ok(())
}

fn execute_apply_vault(config: ApplyVaultConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let authority = wallet_pubkey(&config.wallet)?;
    let vault_owner = read_keypair_file(&config.vault_owner)?;
    let (vault_keypair, vault_ae_key) = derive_confidential_keys(&vault_owner, b"")?;
    let addresses = addresses(&config.common);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let key_epoch =
        fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &addresses.key_epoch)?
            .ok_or("key epoch has not been initialized")?;
    if vault_keypair.pubkey().to_bytes() != key_epoch.encryption_pubkey {
        return Err("vault setup owner does not derive the active epoch key".into());
    }
    apply_one_vault(
        &config,
        &addresses,
        &market,
        authority,
        &vault_keypair,
        &vault_ae_key,
        true,
    )?;
    apply_one_vault(
        &config,
        &addresses,
        &market,
        authority,
        &vault_keypair,
        &vault_ae_key,
        false,
    )
}

fn vault_snapshot(
    rpc_url: &str,
    mint: Pubkey,
    token_account: Pubkey,
) -> Result<VaultSnapshot, Box<dyn Error>> {
    let data = fetch_account_bytes(rpc_url, &token_account)?;
    let state = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    if state.base.mint != mint {
        return Err(format!("vault {token_account} belongs to a different mint").into());
    }
    let confidential = state.get_extension::<ConfidentialTransferAccount>()?;
    Ok(VaultSnapshot {
        token_account: token_account.to_bytes(),
        mint: mint.to_bytes(),
        elgamal_pubkey: confidential.elgamal_pubkey.0,
        available_balance: confidential.available_balance.0,
        decryptable_available_balance: confidential.decryptable_available_balance.0,
        pending_balance_credit_counter: confidential.pending_balance_credit_counter.into(),
        expected_pending_balance_credit_counter: confidential
            .expected_pending_balance_credit_counter
            .into(),
        actual_pending_balance_credit_counter: confidential
            .actual_pending_balance_credit_counter
            .into(),
    })
}

fn confidential_destination(
    rpc_url: &str,
    mint: Pubkey,
    token_account: Pubkey,
) -> Result<ConfidentialDestination, Box<dyn Error>> {
    let data = fetch_account_bytes(rpc_url, &token_account)?;
    let state = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    if state.base.mint != mint {
        return Err(format!("destination {token_account} belongs to a different mint").into());
    }
    let confidential = state.get_extension::<ConfidentialTransferAccount>()?;
    if !bool::from(confidential.approved) {
        return Err(
            format!("destination {token_account} is not confidential-transfer approved").into(),
        );
    }
    Ok(ConfidentialDestination {
        token_account: token_account.to_bytes(),
        mint: mint.to_bytes(),
        owner: state.base.owner.to_bytes(),
        elgamal_pubkey: confidential.elgamal_pubkey.0,
    })
}

fn close_contexts(
    config: &ClaimConfig,
    instructions: &[Instruction],
) -> Result<(), Box<dyn Error>> {
    if instructions.is_empty() {
        return Ok(());
    }
    send_instructions(
        &config.wallet,
        &config.rpc_url,
        "close private-withdrawal proof contexts",
        instructions,
        vec![],
    )
}

fn execute_claim(config: ClaimConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let auction =
        fetch_anchor_account::<beatex::AuctionResult>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not settled or been cancelled")?;
    let (expected_auction_state, withdrawal_kind, circuit_kind, operation) = match config.kind {
        PrivateWithdrawalKind::Claim => (
            beatex::AuctionState::Settled,
            queue::WithdrawalKind::Claim,
            QueueCircuitKind::PrivateClaim,
            "claim",
        ),
        PrivateWithdrawalKind::Refund => (
            beatex::AuctionState::Cancelled,
            queue::WithdrawalKind::Refund,
            QueueCircuitKind::CancellationRefund,
            "refund",
        ),
    };
    if auction.state != expected_auction_state {
        return Err(format!("private {operation} is not allowed in this auction state").into());
    }
    let owner_keypair = read_keypair_file(&config.owner)?;
    let owner = owner_keypair.pubkey();
    let mut trader_state = read_trader_state(&config.common.state_dir, &owner)?
        .ok_or("the trader private-state file is missing")?;
    let stored = trader_state
        .order(config.auction_id)
        .cloned()
        .ok_or("the trader has no locally opened order for this auction")?;
    if stored.auction != auction_addresses.auction.to_string() {
        return Err("the local active order belongs to a different auction".into());
    }
    let order_address = Pubkey::from_str(&stored.event)?;
    let order = fetch_anchor_account::<QueuedOrder>(&config.rpc_url, &order_address)?
        .ok_or("queued order account is missing")?;
    let order_state_is_eligible = match config.kind {
        PrivateWithdrawalKind::Claim => order.state == QueuedOrderState::Folded,
        PrivateWithdrawalKind::Refund => matches!(
            order.state,
            QueuedOrderState::Accepted | QueuedOrderState::Folded
        ),
    };
    if !order_state_is_eligible || order.withdrawal_state != WithdrawalState::Pending {
        return Err(format!("queued order is not eligible for private {operation}").into());
    }
    let active_order_address = Pubkey::find_program_address(
        &[
            b"active-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let active_order =
        fetch_anchor_account::<beatex::ActiveOrder>(&config.rpc_url, &active_order_address)?
            .ok_or("active-order account is missing")?;
    if !active_order.active || active_order.latest_event != order_address {
        return Err("the queued order is not the wallet's active revision".into());
    }

    let opening = PrivateOrderOpening {
        buy: stored.buy,
        sell: stored.sell,
        base_reserve: stored.base_reserve,
        quote_reserve: stored.quote_reserve,
        order_salt: stored.order_salt,
    };
    let (order_commitment, terminal_nullifier) = derive_private_order_identifiers(
        &opening,
        &auction_addresses.auction,
        &owner,
        stored.client_order_nonce,
    );
    if order_commitment != stored.order_commitment || order_commitment != order.order_commitment {
        return Err("the local order opening does not match the on-chain commitment".into());
    }
    let clearing_index = usize::from(auction.clearing_index);
    let demand = auction.demand_at_clearing;
    let supply = auction.supply_at_clearing;
    let (base_payout, quote_payout) = match config.kind {
        PrivateWithdrawalKind::Claim => {
            let allocation = allocation::allocate(allocation::AllocationInput {
                buy: opening.buy[clearing_index],
                sell: opening.sell[clearing_index],
                demand,
                supply,
                clearing_price: auction.clearing_price,
                base_reserve: opening.base_reserve,
                quote_reserve: opening.quote_reserve,
            })
            .map_err(|error| format!("private allocation failed: {error:?}"))?;
            (allocation.base_payout, allocation.quote_payout)
        }
        PrivateWithdrawalKind::Refund => (opening.base_reserve, opening.quote_reserve),
    };
    eprintln!("{operation} payouts: {base_payout} base units and {quote_payout} quote units");

    let setup = threshold_setup_for_owner(&config.vault_owner, &addresses)?;
    let source_key = VaultElGamalKeypair::reconstruct_3_of_7(
        &setup.public,
        [
            &setup.secret_shares[0],
            &setup.secret_shares[3],
            &setup.secret_shares[6],
        ],
    )?;
    let vault_owner = read_keypair_file(&config.vault_owner)?;
    let (_, vault_ae_key) = derive_confidential_keys(&vault_owner, b"")?;
    let source_ae_key = VaultAeKey::from_bytes(<[u8; 16]>::from(&vault_ae_key));
    let base_source = vault_snapshot(&config.rpc_url, market.base_mint, market.base_vault)?;
    let quote_source = vault_snapshot(&config.rpc_url, market.quote_mint, market.quote_vault)?;
    if base_source.elgamal_pubkey != source_key.pubkey_bytes()
        || quote_source.elgamal_pubkey != source_key.pubkey_bytes()
    {
        return Err("the reconstructed committee key does not match both vaults".into());
    }
    let base_destination =
        confidential_destination(&config.rpc_url, market.base_mint, config.base_destination)?;
    let quote_destination =
        confidential_destination(&config.rpc_url, market.quote_mint, config.quote_destination)?;
    let (destination_key, _) = derive_confidential_keys(&owner_keypair, b"")?;
    let expected_destination_key = destination_key.pubkey().to_bytes();
    if base_destination.owner != owner.to_bytes()
        || quote_destination.owner != owner.to_bytes()
        || base_destination.elgamal_pubkey != expected_destination_key
        || quote_destination.elgamal_pubkey != expected_destination_key
    {
        return Err("destination ownership or confidential encryption key is incorrect".into());
    }

    eprintln!("generating and host-verifying Token-2022 transfer proofs");
    let bundle = generate_withdrawal_proof_bundle(
        &TransferRequest {
            source: base_source,
            destination: base_destination,
            amount: base_payout,
            auditor_elgamal_pubkey: None,
        },
        &TransferRequest {
            source: quote_source,
            destination: quote_destination,
            amount: quote_payout,
            auditor_elgamal_pubkey: None,
        },
        &source_key,
        &source_key,
        &source_ae_key,
        &source_ae_key,
    )?;
    let statement = WithdrawalStatementContext {
        kind: withdrawal_kind,
        market: addresses.market,
        auction: auction_addresses.auction,
        order: order_address,
        owner,
        key_epoch: auction.key_epoch,
        client_order_nonce: order.client_order_nonce,
        sequence: order.sequence,
        allocation_rule_version: auction.allocation_rule_version,
        order_circuit_version: auction.order_circuit_version,
        claim_circuit_version: auction.claim_circuit_version,
        order_commitment,
        clearing_index: auction.clearing_index,
        clearing_price: auction.clearing_price,
        demand_at_clearing: demand,
        supply_at_clearing: supply,
        clearing_volume: auction.clearing_volume,
        terminal_nullifier,
        base_mint: market.base_mint,
        quote_mint: market.quote_mint,
        base_vault: market.base_vault,
        quote_vault: market.quote_vault,
        base_destination: config.base_destination,
        quote_destination: config.quote_destination,
    };
    let witness = prepare_private_withdrawal_witness(&opening, &statement, &bundle)?;
    eprintln!("generating deterministic Devnet {operation} proving key");
    let setup_seed = match config.kind {
        PrivateWithdrawalKind::Claim => [0xc1; 32],
        PrivateWithdrawalKind::Refund => [0xc2; 32],
    };
    let mut setup_rng = ChaCha20Rng::from_seed(setup_seed);
    let (proving_key, _) = Groth16::<Bn254>::circuit_specific_setup(
        PrivateWithdrawalCircuit::blank(config.kind),
        &mut setup_rng,
    )?;
    eprintln!("generating private-{operation} Groth16 proof");
    let mut proof_rng = ChaCha20Rng::from_entropy();
    let proof = generate_private_withdrawal_groth16_proof(&proving_key, witness, &mut proof_rng)?;

    let payer = wallet_pubkey(&config.wallet)?;
    let context_keypairs: [Keypair; 6] = std::array::from_fn(|_| Keypair::new());
    let context_directory = config
        .common
        .state_dir
        .join("proof-contexts")
        .join(format!("auction-{}-{operation}", config.auction_id));
    fs::create_dir_all(&context_directory)?;
    let context_paths: [PathBuf; 6] = std::array::from_fn(|index| {
        context_directory.join(format!("{}.json", context_keypairs[index].pubkey()))
    });
    for (keypair, path) in context_keypairs.iter().zip(&context_paths) {
        write_keypair_file(keypair, path)?;
        restrict_permissions(path)?;
    }
    let context_addresses = WithdrawalContextAddresses {
        base: TransferContextAddresses {
            equality: context_keypairs[0].pubkey(),
            validity: context_keypairs[1].pubkey(),
            range: context_keypairs[2].pubkey(),
        },
        quote: TransferContextAddresses {
            equality: context_keypairs[3].pubkey(),
            validity: context_keypairs[4].pubkey(),
            range: context_keypairs[5].pubkey(),
        },
    };
    let preliminary =
        build_withdrawal_context_plan(&bundle, context_addresses, payer, payer, payer, [0; 3]);
    let rent_lamports = [
        minimum_balance_for_rent_exemption(&config.rpc_url, preliminary.base.accounts[0].space)?,
        minimum_balance_for_rent_exemption(&config.rpc_url, preliminary.base.accounts[1].space)?,
        minimum_balance_for_rent_exemption(&config.rpc_url, preliminary.base.accounts[2].space)?,
    ];
    let plan = build_withdrawal_context_plan(
        &bundle,
        context_addresses,
        payer,
        payer,
        payer,
        rent_lamports,
    );
    let create_verify_close = vec![
        (
            plan.base.create_instructions[0].clone(),
            plan.base.verify_instructions[0].clone(),
            plan.base.close_instructions[0].clone(),
        ),
        (
            plan.base.create_instructions[1].clone(),
            plan.base.verify_instructions[1].clone(),
            plan.base.close_instructions[1].clone(),
        ),
        (
            plan.base.create_instructions[2].clone(),
            plan.base.verify_instructions[2].clone(),
            plan.base.close_instructions[2].clone(),
        ),
        (
            plan.quote.create_instructions[0].clone(),
            plan.quote.verify_instructions[0].clone(),
            plan.quote.close_instructions[0].clone(),
        ),
        (
            plan.quote.create_instructions[1].clone(),
            plan.quote.verify_instructions[1].clone(),
            plan.quote.close_instructions[1].clone(),
        ),
        (
            plan.quote.create_instructions[2].clone(),
            plan.quote.verify_instructions[2].clone(),
            plan.quote.close_instructions[2].clone(),
        ),
    ];
    let mut created_close_instructions = Vec::new();
    for (index, (create, verify, close)) in create_verify_close.iter().enumerate() {
        if let Err(error) = send_instructions(
            &config.wallet,
            &config.rpc_url,
            &format!(
                "create and verify private-{operation} context {}",
                index + 1
            ),
            &[create.clone(), verify.clone()],
            vec![context_paths[index].clone()],
        ) {
            if let Err(close_error) = close_contexts(&config, &created_close_instructions) {
                eprintln!("warning: failed to reclaim proof-context rent: {close_error}");
            }
            return Err(error);
        }
        created_close_instructions.push(close.clone());
    }

    let (base_transfer, quote_transfer) = bundle.transfer_args();
    let claim_key = verification_key_address(
        &addresses.market,
        circuit_kind,
        queue::circuit_version(circuit_kind),
    );
    let withdrawal_args = PrivateWithdrawalArgs {
        terminal_nullifier,
        base_transfer,
        quote_transfer,
        proof,
    };
    let instruction_data = match config.kind {
        PrivateWithdrawalKind::Claim => beatex::instruction::ClaimQueuedOrder {
            args: withdrawal_args,
        }
        .data(),
        PrivateWithdrawalKind::Refund => beatex::instruction::RefundQueuedOrder {
            args: withdrawal_args,
        }
        .data(),
    };
    let claim_instruction = Instruction {
        program_id: beatex::ID,
        accounts: vec![
            AccountMeta::new_readonly(addresses.market, false),
            AccountMeta::new_readonly(auction_addresses.auction, false),
            AccountMeta::new_readonly(auction.key_epoch, false),
            AccountMeta::new(order_address, false),
            AccountMeta::new(active_order_address, false),
            AccountMeta::new_readonly(claim_key, false),
            AccountMeta::new_readonly(market.base_mint, false),
            AccountMeta::new(market.base_vault, false),
            AccountMeta::new(config.base_destination, false),
            AccountMeta::new_readonly(context_addresses.base.equality, false),
            AccountMeta::new_readonly(context_addresses.base.validity, false),
            AccountMeta::new_readonly(context_addresses.base.range, false),
            AccountMeta::new_readonly(market.quote_mint, false),
            AccountMeta::new(market.quote_vault, false),
            AccountMeta::new(config.quote_destination, false),
            AccountMeta::new_readonly(context_addresses.quote.equality, false),
            AccountMeta::new_readonly(context_addresses.quote.validity, false),
            AccountMeta::new_readonly(context_addresses.quote.range, false),
            AccountMeta::new_readonly(addresses.vault_authority, false),
            AccountMeta::new_readonly(Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?, false),
        ],
        data: instruction_data,
    };
    let claim_result = send_instructions(
        &config.wallet,
        &config.rpc_url,
        &format!("execute private {operation}"),
        &[claim_instruction],
        vec![],
    );
    let cleanup_result = close_contexts(&config, &created_close_instructions);
    if let Err(error) = claim_result {
        if let Err(close_error) = cleanup_result {
            eprintln!("warning: failed to reclaim proof-context rent: {close_error}");
        }
        return Err(error);
    }
    if let Err(error) = cleanup_result {
        eprintln!("warning: claim succeeded but proof-context rent cleanup failed: {error}");
    }
    trader_state.remove_order(config.auction_id);
    write_trader_state(&config.common.state_dir, &owner, &trader_state)?;
    println!(
        "auction {} private {} completed: {} base units and {} quote units",
        config.auction_id, operation, base_payout, quote_payout
    );
    Ok(())
}

fn execute_balance_settlement(config: BalanceSettlementConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let auction =
        fetch_anchor_account::<beatex::AuctionResult>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not settled or been cancelled")?;
    let expected_state = match config.kind {
        PrivateWithdrawalKind::Claim => beatex::AuctionState::Settled,
        PrivateWithdrawalKind::Refund => beatex::AuctionState::Cancelled,
    };
    if auction.state != expected_state {
        return Err("auction is not ready for the requested internal settlement".into());
    }
    let owner = wallet_pubkey(&config.wallet)?;
    let mut state = read_trader_state(&config.common.state_dir, &owner)?
        .ok_or("the trader private-state file is missing")?;
    let stored = state
        .order(config.auction_id)
        .cloned()
        .ok_or("the trader has no locally opened order for this auction")?;
    if stored.auction != auction_addresses.auction.to_string() {
        return Err("the local active order belongs to a different auction".into());
    }
    let order_address = Pubkey::from_str(&stored.event)?;
    let order = fetch_anchor_account::<QueuedOrder>(&config.rpc_url, &order_address)?
        .ok_or("queued order account is missing")?;
    if order.withdrawal_state != WithdrawalState::Pending
        || (config.kind == PrivateWithdrawalKind::Claim && order.state != QueuedOrderState::Folded)
        || (config.kind == PrivateWithdrawalKind::Refund
            && !matches!(
                order.state,
                QueuedOrderState::Accepted | QueuedOrderState::Folded
            ))
    {
        return Err("queued order is not eligible for internal settlement".into());
    }
    let active_order = Pubkey::find_program_address(
        &[
            b"active-order",
            auction_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let active = fetch_anchor_account::<beatex::ActiveOrder>(&config.rpc_url, &active_order)?
        .ok_or("active-order account is missing")?;
    if !active.active || active.latest_event != order_address {
        return Err("the queued order is not the active revision".into());
    }
    let fill = if config.kind == PrivateWithdrawalKind::Claim {
        fill_event(&auction, &stored)?
    } else {
        None
    };
    let private_balance = private_balance_address(&addresses.market, &owner);
    let key_epoch = fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &auction.key_epoch)?
        .ok_or("key epoch account is missing")?;

    match config.kind {
        PrivateWithdrawalKind::Claim => {
            let mut rng = ChaCha20Rng::from_entropy();
            let output = with_rollover_prover(|prover| {
                prover.prove(
                    RolloverProofRequest {
                        market: addresses.market.to_bytes(),
                        auction: auction_addresses.auction.to_bytes(),
                        order: order_address.to_bytes(),
                        active_order: active_order.to_bytes(),
                        owner: owner.to_bytes(),
                        key_epoch: auction.key_epoch.to_bytes(),
                        client_order_nonce: order.client_order_nonce,
                        sequence: order.sequence,
                        allocation_rule_version: auction.allocation_rule_version,
                        order_circuit_version: auction.order_circuit_version,
                        claim_circuit_version: auction.claim_circuit_version,
                        buy: stored.buy,
                        sell: stored.sell,
                        base_reserve: stored.base_reserve,
                        quote_reserve: stored.quote_reserve,
                        order_salt: stored.order_salt,
                        expected_order_commitment: order.order_commitment,
                        clearing_index: auction.clearing_index,
                        clearing_price: auction.clearing_price,
                        demand: auction.demand_at_clearing,
                        supply: auction.supply_at_clearing,
                        clearing_volume: auction.clearing_volume,
                        balance_revision: state.revision,
                        committee_key: key_epoch.babyjub_encryption_pubkey,
                        old_balance: state.balance.opening(),
                    },
                    &mut rng,
                )
            })?;
            let rollover_key = verification_key_address(
                &addresses.market,
                QueueCircuitKind::RolloverSettlement,
                queue::ROLLOVER_SETTLEMENT_CIRCUIT_VERSION,
            );
            let mut expected_public_input = output.public_input;
            expected_public_input.reverse();
            let binding = BindRolloverSettlementArgs {
                settlement: RolloverSettlementArgs {
                    terminal_nullifier: output.terminal_nullifier,
                    credited_encrypted_base: output.credited_encrypted_base,
                    credited_encrypted_quote: output.credited_encrypted_quote,
                },
                expected_public_input,
            };
            let binding_accounts = vec![
                meta(owner, false, true),
                meta(addresses.market, false, false),
                meta(auction_addresses.auction, false, false),
                meta(auction.key_epoch, false, false),
                meta(order_address, false, false),
                meta(active_order, false, false),
                meta(private_balance, false, false),
                meta(rollover_key, false, false),
                meta(solana_instructions_sysvar::ID, false, false),
            ];
            let mut settlement_accounts = binding_accounts.clone();
            settlement_accounts[0].writable = true;
            settlement_accounts[4].writable = true;
            settlement_accounts[5].writable = true;
            settlement_accounts[6].writable = true;
            send_bundle(
                &config.wallet,
                &config.rpc_url,
                "settle order into encrypted balance",
                vec![
                    BundleInstruction {
                        program_id: beatex::ID.to_string(),
                        accounts: vec![
                            meta(addresses.market, false, false),
                            meta(rollover_key, false, false),
                        ],
                        data_base64: BASE64.encode(
                            beatex::instruction::VerifyRolloverProof {
                                args: VerifyRolloverProofArgs {
                                    circuit_kind: QueueCircuitKind::RolloverSettlement,
                                    expected_public_input,
                                    proof: proof_input(
                                        output.proof_a,
                                        output.proof_b,
                                        output.proof_c,
                                        output.public_input,
                                    ),
                                },
                            }
                            .data(),
                        ),
                    },
                    BundleInstruction {
                        program_id: beatex::ID.to_string(),
                        accounts: binding_accounts,
                        data_base64: BASE64.encode(
                            beatex::instruction::BindRolloverSettlement { args: binding }.data(),
                        ),
                    },
                    BundleInstruction {
                        program_id: beatex::ID.to_string(),
                        accounts: settlement_accounts,
                        data_base64: BASE64
                            .encode(beatex::instruction::SettleOrderToBalance {}.data()),
                    },
                ],
                vec![],
            )?;
            state.balance = output.credited_balance.into();
            if let Some(fill) = fill {
                emit_order_event(NativeOrderEvent::Fill {
                    auction_id: config.auction_id,
                    side: fill.side.to_owned(),
                    base_amount: fill.base_amount,
                    quote_amount: fill.quote_amount,
                });
            }
        }
        PrivateWithdrawalKind::Refund => {
            let mut rng = ChaCha20Rng::from_entropy();
            let output = with_collateral_release_prover(|prover| {
                prover.prove(
                    CollateralReleaseProofRequest {
                        market: addresses.market.to_bytes(),
                        auction: auction_addresses.auction.to_bytes(),
                        active_order: active_order.to_bytes(),
                        owner: owner.to_bytes(),
                        balance_revision: state.revision,
                        client_order_nonce: stored.client_order_nonce,
                        buy: stored.buy,
                        sell: stored.sell,
                        base_reserve: stored.base_reserve,
                        quote_reserve: stored.quote_reserve,
                        order_salt: stored.order_salt,
                        expected_order_commitment: stored.order_commitment,
                        committee_key: key_epoch.babyjub_encryption_pubkey,
                        old_balance: state.balance.opening(),
                    },
                    &mut rng,
                )
            })?;
            let release_key = verification_key_address(
                &addresses.market,
                QueueCircuitKind::OrderModification,
                queue::ORDER_MODIFICATION_CIRCUIT_VERSION,
            );
            send(
                &config.wallet,
                &config.rpc_url,
                "refund cancelled order into encrypted balance",
                vec![
                    meta(owner, true, true),
                    meta(addresses.market, false, false),
                    meta(auction_addresses.auction, false, false),
                    meta(auction.key_epoch, false, false),
                    meta(release_key, false, false),
                    meta(private_balance, true, false),
                    meta(active_order, true, false),
                    meta(order_address, true, false),
                ],
                beatex::instruction::RefundOrderToBalance {
                    args: ReleaseOrderCollateralArgs {
                        released_encrypted_base: output.released_encrypted_base,
                        released_encrypted_quote: output.released_encrypted_quote,
                        proof: proof_input(
                            output.proof_a,
                            output.proof_b,
                            output.proof_c,
                            output.public_input,
                        ),
                    },
                }
                .data(),
                vec![],
            )?;
            let released_base = output.released_balance.base_value;
            let released_quote = output.released_balance.quote_value;
            state.balance = output.released_balance.into();
            emit_order_event(NativeOrderEvent::Refund {
                auction_id: config.auction_id,
                base_amount: released_base,
                quote_amount: released_quote,
            });
        }
    }
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or("private balance revision overflow")?;
    state.remove_order(config.auction_id);
    write_trader_state(&config.common.state_dir, &owner, &state)
}

fn execute_public_balance_withdrawal(
    config: PublicBalanceWithdrawalConfig,
) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let key_epoch =
        fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &addresses.key_epoch)?
            .ok_or("key epoch account is missing")?;
    let owner = wallet_pubkey(&config.wallet)?;
    let mut state = read_trader_state(&config.common.state_dir, &owner)?
        .ok_or("the trader private-state file is missing")?;
    if state.has_active_orders() {
        return Err("settle or cancel all active orders before withdrawing".into());
    }
    let private_balance = private_balance_address(&addresses.market, &owner);
    let onchain_balance =
        fetch_anchor_account::<beatex::EncryptedPrivateBalance>(&config.rpc_url, &private_balance)?
            .ok_or("on-chain private balance is missing")?;
    if onchain_balance.market != addresses.market
        || onchain_balance.owner != owner
        || onchain_balance.key_epoch != addresses.key_epoch
    {
        return Err("on-chain private balance does not match this trader and market".into());
    }
    if onchain_balance.revision != state.revision {
        return Err(format!(
            "local private-balance revision {} does not match on-chain revision {}",
            state.revision, onchain_balance.revision
        )
        .into());
    }
    let mut rng = ChaCha20Rng::from_entropy();
    let output = with_deposit_prover(|prover| {
        prover.prove_public_withdrawal(
            PublicWithdrawalProofRequest {
                market: addresses.market.to_bytes(),
                owner: owner.to_bytes(),
                key_epoch: addresses.key_epoch.to_bytes(),
                committee_key: key_epoch.babyjub_encryption_pubkey,
                current_revision: state.revision,
                base_amount: config.base_amount,
                quote_amount: config.quote_amount,
                current_balance: state.balance.opening(),
            },
            &mut rng,
        )
    })?;
    let deposit_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::BalanceDeposit,
        queue::BALANCE_DEPOSIT_CIRCUIT_VERSION,
    );
    send(
        &config.wallet,
        &config.rpc_url,
        "withdraw public tokens from encrypted balance",
        vec![
            meta(owner, true, true),
            meta(addresses.market, false, false),
            meta(addresses.key_epoch, false, false),
            meta(deposit_key, false, false),
            meta(private_balance, true, false),
            meta(market.base_mint, false, false),
            meta(market.quote_mint, false, false),
            meta(market.base_vault, true, false),
            meta(market.quote_vault, true, false),
            meta(config.base_destination, true, false),
            meta(config.quote_destination, true, false),
            meta(addresses.vault_authority, false, false),
            meta(Pubkey::from_str(TOKEN_2022_PROGRAM_ID)?, false, false),
        ],
        beatex::instruction::WithdrawPublicBalance {
            args: WithdrawPublicBalanceArgs {
                base_amount: config.base_amount,
                quote_amount: config.quote_amount,
                remaining_encrypted_base: output.remaining_encrypted_base,
                remaining_encrypted_quote: output.remaining_encrypted_quote,
                proof: proof_input(
                    output.proof_a,
                    output.proof_b,
                    output.proof_c,
                    output.public_input,
                ),
            },
        }
        .data(),
        vec![],
    )?;
    state.balance = output.remaining_balance.into();
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or("private balance revision overflow")?;
    write_trader_state(&config.common.state_dir, &owner, &state)?;
    println!(
        "withdrew {} base units and {} quote units",
        config.base_amount, config.quote_amount
    );
    Ok(())
}

fn write_manifest(config: &InitializeConfig, addresses: &Addresses) -> Result<(), Box<dyn Error>> {
    let path = config.common.state_dir.join("market.tsv");
    let mut contents = format!(
        "name\taddress\nmarket\t{}\nvault-authority\t{}\nkey-epoch\t{}\nbase-vault\t{}\nquote-vault\t{}\n",
        addresses.market,
        addresses.vault_authority,
        addresses.key_epoch,
        config.base_vault,
        config.quote_vault
    );
    let kinds = [
        QueueCircuitKind::OrderSubmission,
        QueueCircuitKind::AggregateFold,
        QueueCircuitKind::BalanceDeposit,
        QueueCircuitKind::AggregatePublication,
        QueueCircuitKind::PrivateClaim,
        QueueCircuitKind::CancellationRefund,
        QueueCircuitKind::OrderModification,
        QueueCircuitKind::RolloverSettlement,
    ];
    for kind in kinds {
        let version = if kind == QueueCircuitKind::OrderSubmission {
            queue::DEVNET_FAST_ORDER_CIRCUIT_VERSION
        } else {
            queue::circuit_version(kind)
        };
        contents.push_str(&format!(
            "vk-{:?}-v{}\t{}\n",
            kind,
            version,
            verification_key_address(&addresses.market, kind, version)
        ));
    }
    fs::write(path, contents)?;
    Ok(())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 {
        return Err("expected 64 hexadecimal characters".into());
    }
    let mut bytes = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(bytes)
}

fn initialize(config: InitializeConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    fs::create_dir_all(&config.common.state_dir)?;
    let authority = wallet_pubkey(&config.wallet)?;
    let addresses = addresses(&config.common);
    let participants = ensure_participants(&config.common.state_dir)?;
    initialize_market(&config, &addresses, authority)?;
    initialize_epoch(&config, &addresses, authority, &participants)?;
    acknowledge_and_activate(&config, &addresses, authority, &participants)?;
    initialize_verification_keys(&config, &addresses, authority)?;
    write_manifest(&config, &addresses)?;
    println!("market initialized: {}", addresses.market);
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        usage();
        return Ok(());
    }
    match args[0].as_str() {
        "addresses" => print_addresses(&common(&args[1..])?),
        "committee" => run_committee_bot(
            common(&args[1..])?,
            PathBuf::from(value(&args[1..], "--wallet")?),
            optional(&args[1..], "--rpc-url").unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        ),
        "watch-auctions" => watch_and_initialize_auctions(
            common(&args[1..])?,
            PathBuf::from(value(&args[1..], "--wallet")?),
            optional(&args[1..], "--rpc-url").unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        ),
        "auction" => initialize_current_auction(
            common(&args[1..])?,
            PathBuf::from(value(&args[1..], "--wallet")?),
            optional(&args[1..], "--rpc-url").unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            optional(&args[1..], "--auction-id")
                .map(|value| value.parse())
                .transpose()?,
        ),
        "market-status" => execute_market_status(MarketStatusConfig {
            common: common(&args[1..])?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        }),
        "pipeline-status" => execute_pipeline_status(MarketStatusConfig {
            common: common(&args[1..])?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        }),
        "auction-status" => execute_auction_status(AuctionStatusConfig {
            common: common(&args[1..])?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
        }),
        "order" => execute_order(OrderConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            base_source: Pubkey::from_str(&value(&args[1..], "--base-source")?)?,
            quote_source: Pubkey::from_str(&value(&args[1..], "--quote-source")?)?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            deposit_base: optional(&args[1..], "--deposit-base")
                .unwrap_or_else(|| "100000".to_owned())
                .parse()?,
            deposit_quote: optional(&args[1..], "--deposit-quote")
                .unwrap_or_else(|| "1000000000".to_owned())
                .parse()?,
            target_base: 0,
            target_quote: 0,
        }),
        "fold" => execute_fold(FoldConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            max_batches: optional(&args[1..], "--max-batches")
                .unwrap_or_else(|| usize::MAX.to_string())
                .parse()?,
            coordinator_logs: false,
        }),
        "publish" => execute_publish(PublishConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            coordinator_logs: false,
        }),
        "cancel" => execute_cancel(FoldConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            max_batches: usize::MAX,
            coordinator_logs: false,
        }),
        "settle-balance" => execute_balance_settlement(BalanceSettlementConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            kind: PrivateWithdrawalKind::Claim,
        }),
        "refund-balance" => execute_balance_settlement(BalanceSettlementConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            kind: PrivateWithdrawalKind::Refund,
        }),
        "withdraw" => execute_public_balance_withdrawal(PublicBalanceWithdrawalConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            base_destination: Pubkey::from_str(&value(&args[1..], "--base-destination")?)?,
            quote_destination: Pubkey::from_str(&value(&args[1..], "--quote-destination")?)?,
            base_amount: value(&args[1..], "--base-amount")?.parse()?,
            quote_amount: value(&args[1..], "--quote-amount")?.parse()?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        }),
        "apply-vault" => execute_apply_vault(ApplyVaultConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            vault_owner: PathBuf::from(value(&args[1..], "--vault-owner")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        }),
        "claim" => execute_claim(ClaimConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            owner: PathBuf::from(value(&args[1..], "--owner")?),
            vault_owner: PathBuf::from(value(&args[1..], "--vault-owner")?),
            base_destination: Pubkey::from_str(&value(&args[1..], "--base-destination")?)?,
            quote_destination: Pubkey::from_str(&value(&args[1..], "--quote-destination")?)?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            kind: PrivateWithdrawalKind::Claim,
        }),
        "refund" => execute_claim(ClaimConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            owner: PathBuf::from(value(&args[1..], "--owner")?),
            vault_owner: PathBuf::from(value(&args[1..], "--vault-owner")?),
            base_destination: Pubkey::from_str(&value(&args[1..], "--base-destination")?)?,
            quote_destination: Pubkey::from_str(&value(&args[1..], "--quote-destination")?)?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
            kind: PrivateWithdrawalKind::Refund,
        }),
        "initialize" => {
            let common = common(&args[1..])?;
            initialize(InitializeConfig {
                common,
                wallet: PathBuf::from(value(&args[1..], "--wallet")?),
                vault_owner: PathBuf::from(value(&args[1..], "--vault-owner")?),
                base_vault: Pubkey::from_str(&value(&args[1..], "--base-vault")?)?,
                quote_vault: Pubkey::from_str(&value(&args[1..], "--quote-vault")?)?,
                rpc_url: optional(&args[1..], "--rpc-url")
                    .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
                market_start_slot: value(&args[1..], "--market-start-slot")?.parse()?,
                initial_price: optional(&args[1..], "--initial-price")
                    .map(|price| price.parse())
                    .transpose()?,
            })
        }
        command => Err(format!("unknown command {command}").into()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Devnet client failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_order(auction_id: u64) -> StoredOrder {
        StoredOrder {
            auction_id,
            auction: format!("auction-{auction_id}"),
            event: format!("event-{auction_id}"),
            client_order_nonce: auction_id,
            buy: [0; beatex_babyjubjub_groth16_spike::LEVELS],
            sell: [0; beatex_babyjubjub_groth16_spike::LEVELS],
            order_salt: [0; 32],
            order_commitment: [0; 32],
            base_reserve: 0,
            quote_reserve: 0,
        }
    }

    #[test]
    fn address_derivation_is_stable() {
        let config = CommonConfig {
            base_mint: Pubkey::new_from_array([1; 32]),
            quote_mint: Pubkey::new_from_array([2; 32]),
            state_dir: PathBuf::from("unused"),
        };
        assert_eq!(addresses(&config), addresses(&config));
    }

    #[test]
    fn feed_id_decodes_exactly() {
        assert_eq!(decode_hex_32(PYTH_BTC_USD_FEED).unwrap()[0], 0xe6);
        assert!(decode_hex_32("ff").is_err());
    }

    #[test]
    fn only_system_allocate_races_are_benign() {
        assert!(is_already_initialized_race(
            "returned deterministic JSON-RPC error: Allocate: account Address { address: auction } already in use"
        ));
        assert!(!is_already_initialized_race(
            "AnchorError: the queued-order state transition violates an invariant"
        ));
        assert!(!is_already_initialized_race(
            "account already in use by an unrelated instruction"
        ));
    }

    #[test]
    fn auction_target_preinitializes_at_phase_end_and_rejects_stale_workers() {
        let start = 1_000;
        let duration = 100;
        assert!(auction_target_is_fresh(start, duration, start, 0));
        assert!(!auction_target_is_fresh(start, duration, start, 1));
        assert!(!auction_target_is_fresh(start, duration, start + 87, 1));
        assert!(auction_target_is_fresh(start, duration, start + 88, 1));
        assert!(auction_target_is_fresh(start, duration, start + 99, 1));
        assert!(!auction_target_is_fresh(start, duration, start + 100, 0));
        assert!(auction_target_is_fresh(start, duration, start + 100, 1));
    }

    #[test]
    fn trader_state_migrates_and_tracks_multiple_auctions() {
        let mut state = TraderPrivateState {
            revision: 2,
            balance: StoredBalance {
                base_value: 100,
                quote_value: 200,
                base_randomness: [0; 32],
                quote_randomness: [0; 32],
            },
            active_orders: vec![stored_order(4)],
            active_order: Some(stored_order(3)),
        }
        .normalize();
        assert_eq!(
            state
                .active_orders
                .iter()
                .map(|order| order.auction_id)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        state.upsert_order(stored_order(5));
        state.remove_order(4);
        assert!(state.order(3).is_some());
        assert!(state.order(4).is_none());
        assert!(state.order(5).is_some());
    }

    fn pipeline_auction(accepted_orders: u64, folded_orders: u64) -> PipelineAuction {
        PipelineAuction {
            auction_id: 1,
            state: beatex::AuctionState::Pending,
            submission_end_slot: 20,
            aggregation_deadline_slot: 30,
            decryption_deadline_slot: 40,
            accepted_orders,
            folded_orders,
        }
    }

    #[test]
    fn committee_reclaims_empty_auctions_after_collection() {
        let auction = pipeline_auction(0, 0);
        assert!(!committee_action_needed(&auction, 19));
        assert!(committee_action_needed(&auction, 20));
        assert!(committee_action_needed(&auction, u64::MAX));
    }

    #[test]
    fn committee_handles_nonempty_auction_deadlines() {
        let complete = pipeline_auction(1, 1);
        assert!(committee_action_needed(&complete, 20));
        assert!(committee_action_needed(&complete, 40));

        let stalled = pipeline_auction(1, 0);
        assert!(committee_action_needed(&stalled, 30));
    }

    #[test]
    fn committee_starts_the_final_fold_before_the_close() {
        let queued = pipeline_auction(1, 0);
        assert!(!committee_action_needed(
            &queued,
            20 - FINAL_FOLD_LEAD_SLOTS - 1
        ));
        assert!(committee_action_needed(&queued, 20 - FINAL_FOLD_LEAD_SLOTS));
    }
}

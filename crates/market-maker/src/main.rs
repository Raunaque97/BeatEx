use std::{
    env,
    error::Error,
    fs,
    path::Path,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender, TryRecvError},
        Arc, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use {
    anchor_lang::{prelude::Pubkey, AccountDeserialize},
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    serde::{Deserialize, Serialize},
    solana_keypair::read_keypair_file,
    solana_signer::Signer,
    time::{
        format_description::BorrowedFormatItem, macros::format_description, OffsetDateTime,
        UtcOffset,
    },
};

const LEVELS: usize = 17;
const BPS_DENOMINATOR: u128 = 10_000;
const DEFAULT_BASE_TOKEN_DECIMALS: u32 = 8;
const DEFAULT_QUOTE_TOKEN_DECIMALS: u32 = 6;
const EXPECTED_BASE_UNIT_SIZE: u64 = 100;
const EXPECTED_QUOTE_UNIT_SIZE: u64 = 1;
const CIRCUIT_QUANTITY_MAX: u64 = (1 << 28) - 1;
const DEFAULT_ORDER_BUDGET_BPS: u16 = 1_000;
const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
// Hand work to the next preinitialized auction as soon as the committee makes
// it available. This keeps proof generation and RPC confirmation away from the
// tail of the current collection window.
const MIN_ORDER_START_REMAINING_SLOTS: u64 = 12;
const NEXT_AUCTION_PREPARE_REMAINING_SLOTS: u64 = 12;
const ORDER_OPEN_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Longer than a blockhash lifetime (150 blocks), after which an unconfirmed
/// transaction can no longer land.
const UNCERTAIN_ORDER_SETTLE_WAIT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriceSource {
    Pyth,
    Binance,
    /// No external feed: an exponential moving average of the market's own
    /// clearing prices, for markets without an oracle.
    Ema,
}

#[derive(Clone, Debug)]
struct Config {
    source: PriceSource,
    /// Whether this maker buys in even auctions (and sells in odd ones). Two
    /// makers must use opposite parities so their orders cross.
    buy_on_even: bool,
    rpc_urls: Vec<String>,
    market: Pubkey,
    base_quantity: String,
    slope_quantity: String,
    max_quantity: String,
    /// Working private tBTC balance; the tUSD target is its value at the
    /// expected price, so both sides hold roughly equal value.
    target_base: String,
    /// Base-token decimals and display symbols, so one binary serves any market.
    base_decimals: u32,
    base_symbol: String,
    quote_decimals: u32,
    quote_symbol: String,
    /// Weight of the newest clearing price in the `ema` source.
    ema_alpha: f64,
    interval: Duration,
    state_dir: Option<String>,
    wallet: Option<String>,
    wallet_owner: Option<Pubkey>,
    base_source: Option<Pubkey>,
    quote_source: Option<Pubkey>,
    market_start_slot: u64,
    submission_duration_slots: u64,
    balance_every_auctions: u64,
    order_budget_bps: u16,
    once: bool,
}

#[derive(Clone, Debug, Serialize)]
struct OrderIntent {
    source: &'static str,
    observed_price: String,
    expected_price: u64,
    reference_price: u64,
    grid_step_bps: u16,
    grid_size: u8,
    base_unit_size: u64,
    quote_unit_size: u64,
    client_order_nonce: u64,
    buy: [u64; LEVELS],
    sell: [u64; LEVELS],
}

struct OrderJob {
    auction_id: u64,
    operation: &'static str,
    side: Side,
    target_base: u64,
    target_quote: u64,
    max_tusd: String,
    max_tbtc: String,
    intent: OrderIntent,
    base_mint: Pubkey,
    quote_mint: Pubkey,
}

enum OrderWorkerResult {
    Submitted(u64),
    Skipped(u64),
    Failed(u64),
    Uncertain(String),
}

#[derive(Deserialize)]
struct BinanceTicker {
    price: String,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: serde_json::Value,
    message: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RpcAccountResult {
    value: Option<RpcAccount>,
}

#[derive(Deserialize)]
struct RpcAccount {
    data: (String, String),
}

#[derive(Deserialize)]
struct RpcTokenAccountsResult {
    value: Vec<RpcTokenAccount>,
}

#[derive(Deserialize)]
struct RpcTokenAccount {
    pubkey: String,
    account: RpcTokenAccountInfo,
}

#[derive(Deserialize)]
struct RpcTokenAccountInfo {
    data: RpcParsedData,
}

#[derive(Deserialize)]
struct RpcParsedData {
    parsed: RpcParsedToken,
}

#[derive(Deserialize)]
struct RpcParsedToken {
    info: RpcParsedTokenInfo,
}

#[derive(Deserialize)]
struct RpcParsedTokenInfo {
    #[serde(rename = "tokenAmount")]
    token_amount: RpcTokenAmount,
}

#[derive(Deserialize)]
struct RpcTokenAmount {
    amount: String,
    decimals: u32,
}

fn usage() {
    eprintln!(
        "Usage: beatex-market-maker --source pyth|binance --market ADDRESS \
         [--rpc-url URL] [--base-quantity TBTC] [--slope-quantity TBTC] \
         [--max-quantity TBTC] [--interval-ms N] \
         [--state-dir DIR] [--wallet KEYPAIR] [--balance-every-auctions N] \
         [--budget-bps N] [--target-tbtc TBTC] [--buy-on even|odd] [--once]\n\n\
         Quantities are human tBTC decimal strings. The bot reads the current Market and \
         Auction before every order and uses the auction's reference price, basis-point \
         grid step, and grid size. Orders are submitted directly through the native Rust \
         client and the call returns only after the transaction is confirmed. When state-dir and \
         wallet are supplied, the bot prints the encrypted ledger balance every N auctions. \
         Each order is capped at budget-bps of the private balance (default 1000 = 10%). \
         The private balance is kept near target-tbtc (default 0.25) plus the same value \
         in tUSD, topping both up from the wallet when either falls below half."
    );
}

fn parse_config() -> Result<Config, Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        usage();
        std::process::exit(0);
    }
    let value = |name: &str| -> Result<String, Box<dyn Error>> {
        let position = args
            .iter()
            .position(|arg| arg == name)
            .ok_or_else(|| format!("missing {name}"))?;
        args.get(position + 1)
            .cloned()
            .ok_or_else(|| format!("{name} requires a value").into())
    };
    let optional = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|position| args.get(position + 1).cloned())
    };
    let source = match value("--source")?.as_str() {
        "pyth" => PriceSource::Pyth,
        "binance" => PriceSource::Binance,
        "ema" => PriceSource::Ema,
        other => return Err(format!("unsupported price source {other}").into()),
    };
    let buy_on_even = match optional("--buy-on").as_deref() {
        Some("even") => true,
        Some("odd") => false,
        Some(other) => return Err(format!("--buy-on must be even or odd, not {other}").into()),
        // Historical pairing: the Pyth maker buys in even auctions.
        None => source == PriceSource::Pyth,
    };
    let rpc_url = optional("--rpc-url")
        .or_else(|| env::var("BEATEX_RPC_URL").ok())
        .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned());
    let mut rpc_urls = vec![rpc_url];
    if let Ok(fallback_urls) = env::var("BEATEX_RPC_FALLBACK_URLS") {
        for fallback_url in fallback_urls
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            if !rpc_urls.iter().any(|url| url == fallback_url) {
                rpc_urls.push(fallback_url.to_owned());
            }
        }
    }
    let wallet = optional("--wallet").or_else(|| env::var("BEATEX_MARKET_MAKER_KEYPAIR").ok());
    let wallet_owner = match wallet.as_deref() {
        Some(path) => {
            let keypair = read_keypair_file(path)?;
            Some(keypair.pubkey())
        }
        None => None,
    };
    Ok(Config {
        source,
        buy_on_even,
        rpc_urls,
        market: Pubkey::from_str(&value("--market")?)?,
        base_quantity: optional("--base-quantity").unwrap_or_else(|| "0.005".to_owned()),
        slope_quantity: optional("--slope-quantity").unwrap_or_else(|| "0.001".to_owned()),
        max_quantity: optional("--max-quantity").unwrap_or_else(|| "0.01".to_owned()),
        target_base: optional("--target-base")
            .or_else(|| optional("--target-tbtc"))
            .unwrap_or_else(|| "0.25".to_owned()),
        base_decimals: optional("--base-decimals")
            .map_or(Ok(DEFAULT_BASE_TOKEN_DECIMALS), |value| value.parse())?,
        base_symbol: optional("--base-symbol").unwrap_or_else(|| "tBTC".to_owned()),
        quote_decimals: optional("--quote-decimals")
            .map_or(Ok(DEFAULT_QUOTE_TOKEN_DECIMALS), |value| value.parse())?,
        quote_symbol: optional("--quote-symbol").unwrap_or_else(|| "tUSD".to_owned()),
        ema_alpha: optional("--ema-alpha").map_or(Ok(0.2), |value| value.parse())?,
        interval: match optional("--interval-ms") {
            Some(milliseconds) => Duration::from_millis(milliseconds.parse()?),
            None => match optional("--interval-seconds") {
                Some(seconds) => Duration::from_secs(seconds.parse()?),
                None => Duration::from_millis(200),
            },
        },
        state_dir: optional("--state-dir").or_else(|| env::var("BEATEX_STATE_DIR").ok()),
        wallet,
        wallet_owner,
        base_source: None,
        quote_source: None,
        market_start_slot: 0,
        submission_duration_slots: 0,
        balance_every_auctions: optional("--balance-every-auctions")
            .or_else(|| env::var("BEATEX_BALANCE_EVERY_AUCTIONS").ok())
            .unwrap_or_else(|| "10".to_owned())
            .parse::<u64>()
            .map_err(|_| "balance-every-auctions must be a positive integer")?,
        order_budget_bps: optional("--budget-bps")
            .or_else(|| env::var("BEATEX_MARKET_MAKER_BUDGET_BPS").ok())
            .unwrap_or_else(|| DEFAULT_ORDER_BUDGET_BPS.to_string())
            .parse::<u16>()
            .map_err(|_| "budget-bps must be an integer")?,
        once: args.iter().any(|arg| arg == "--once"),
    })
}

fn report_balance(config: &Config, auction_id: u64) {
    let (Some(state_dir), Some(owner)) = (&config.state_dir, config.wallet_owner) else {
        return;
    };
    let path = Path::new(state_dir)
        .join("traders")
        .join(config.market.to_string())
        .join(format!("{owner}.json"));
    let Ok(bytes) = fs::read(&path) else {
        eprintln!(
            "[{}] balance report unavailable for auction {auction_id}: {} does not exist yet",
            timestamp(),
            path.display()
        );
        return;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        eprintln!(
            "[{}] balance report unavailable: invalid {}",
            timestamp(),
            path.display()
        );
        return;
    };
    let revision = value
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let base = value
        .pointer("/balance/base_value")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let quote = value
        .pointer("/balance/quote_value")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    println!(
        "[{}] auction {auction_id} private balance: {} {} | {} {} (ledger revision {revision})",
        timestamp(),
        config.base_symbol,
        format_units(u128::from(base), config.base_decimals),
        config.quote_symbol,
        format_units(u128::from(quote), config.quote_decimals),
    );
}

fn timestamp() -> String {
    static FORMAT: &[BorrowedFormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let now = OffsetDateTime::now_utc();
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    now.to_offset(offset)
        .format(FORMAT)
        .unwrap_or_else(|_| "unknown-time".to_owned())
}

fn format_units(value: u128, decimals: u32) -> String {
    if decimals == 0 {
        return value.to_string();
    }
    let scale = 10u128.pow(decimals);
    let whole = value / scale;
    let fraction = value % scale;
    if fraction == 0 {
        return whole.to_string();
    }
    format!("{whole}.{:0width$}", fraction, width = decimals as usize)
        .trim_end_matches('0')
        .to_owned()
}

fn order_operation(config: &Config, auction_id: u64) -> &'static str {
    let (Some(state_dir), Some(owner)) = (&config.state_dir, config.wallet_owner) else {
        return "new";
    };
    let path = Path::new(state_dir)
        .join("traders")
        .join(config.market.to_string())
        .join(format!("{owner}.json"));
    let Ok(bytes) = fs::read(path) else {
        return "new";
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return "new";
    };
    let mut previous_ids = value
        .get("active_orders")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|order| order.get("auction_id")?.as_u64())
        .collect::<Vec<_>>();
    if let Some(legacy_id) = value
        .pointer("/active_order/auction_id")
        .and_then(serde_json::Value::as_u64)
    {
        previous_ids.push(legacy_id);
    }
    if previous_ids.contains(&auction_id) {
        "replace"
    } else if previous_ids
        .iter()
        .any(|previous_id| *previous_id < auction_id)
    {
        "rollover/place"
    } else {
        "new"
    }
}

fn private_balance(config: &Config) -> Option<(u64, u64)> {
    let (Some(state_dir), Some(owner)) = (&config.state_dir, config.wallet_owner) else {
        return None;
    };
    let path = Path::new(state_dir)
        .join("traders")
        .join(config.market.to_string())
        .join(format!("{owner}.json"));
    let value = serde_json::from_slice::<serde_json::Value>(&fs::read(path).ok()?).ok()?;
    Some((
        value.pointer("/balance/base_value")?.as_u64()?,
        value.pointer("/balance/quote_value")?.as_u64()?,
    ))
}

/// Confirm that an order whose confirmation was lost never landed: its
/// signature is unknown even in transaction history, and the on-chain private
/// balance revision still equals the local one.
fn verify_unconfirmed_order(config: &Config, error: &str) -> Result<(), String> {
    let signature = error
        .split_whitespace()
        .skip_while(|word| *word != "for")
        .nth(1)
        .ok_or_else(|| format!("no signature in {error:?}"))?;
    let statuses: serde_json::Value = rpc_request(
        &config.rpc_urls,
        "getSignatureStatuses",
        serde_json::json!([[signature], {"searchTransactionHistory": true}]),
    )
    .map_err(|error| error.to_string())?;
    if !statuses
        .pointer("/value/0")
        .is_none_or(serde_json::Value::is_null)
    {
        return Err(format!("transaction {signature} landed"));
    }
    let (Some(state_dir), Some(owner)) = (&config.state_dir, config.wallet_owner) else {
        return Err("no local private state to compare".to_owned());
    };
    let local = Path::new(state_dir)
        .join("traders")
        .join(config.market.to_string())
        .join(format!("{owner}.json"));
    let local_revision = fs::read(&local)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("revision").and_then(serde_json::Value::as_u64))
        .ok_or_else(|| format!("cannot read {}", local.display()))?;
    let (balance, _) = Pubkey::find_program_address(
        &[b"private-balance", config.market.as_ref(), owner.as_ref()],
        &beatex::ID,
    );
    let onchain =
        fetch_anchor_account::<beatex::EncryptedPrivateBalance>(&config.rpc_urls, &balance)
            .map_err(|error| error.to_string())?
            .ok_or("private balance account is missing")?;
    if onchain.revision != local_revision {
        return Err(format!(
            "on-chain revision {} differs from local revision {local_revision}",
            onchain.revision
        ));
    }
    Ok(())
}

fn cap_order_budget(
    config: &Config,
    market: &beatex::Market,
    auction: &beatex::Auction,
    buy: &mut [u64; LEVELS],
    sell: &mut [u64; LEVELS],
) -> Result<(), Box<dyn Error>> {
    let Some((base_balance, quote_balance)) = private_balance(config) else {
        // The native order client creates the first private balance. Until a local
        // opening exists, leave the configured curve unchanged; subsequent
        // auctions are strictly capped once the balance is available.
        return Ok(());
    };
    let budget_bps = u128::from(config.order_budget_bps);
    let base_cap = u128::from(base_balance) * budget_bps / 10_000;
    let quote_cap = u128::from(quote_balance) * budget_bps / 10_000;
    let prices = grid_prices(auction)?;
    let levels = usize::from(auction.grid_size);
    let max_base = sell[..levels]
        .iter()
        .map(|quantity| u128::from(*quantity) * u128::from(market.base_unit_size))
        .max()
        .unwrap_or_default();
    let max_quote = buy[..levels]
        .iter()
        .enumerate()
        .map(|(index, quantity)| u128::from(*quantity) * u128::from(prices[index]))
        .max()
        .unwrap_or_default();
    if max_base > base_cap {
        if base_cap == 0 {
            sell[..levels].fill(0);
        } else {
            for quantity in &mut sell[..levels] {
                *quantity = u64::try_from(
                    u128::from(*quantity)
                        .saturating_mul(base_cap)
                        .checked_div(max_base)
                        .ok_or("base budget scaling failed")?,
                )?;
            }
        }
    }
    if max_quote > quote_cap {
        if quote_cap == 0 {
            buy[..levels].fill(0);
        } else {
            for quantity in &mut buy[..levels] {
                *quantity = u64::try_from(
                    u128::from(*quantity)
                        .saturating_mul(quote_cap)
                        .checked_div(max_quote)
                        .ok_or("quote budget scaling failed")?,
                )?;
            }
        }
    }
    Ok(())
}

fn order_maxima(
    config: &Config,
    auction: &beatex::Auction,
    intent: &OrderIntent,
) -> Result<(String, String), Box<dyn Error>> {
    let prices = grid_prices(auction)?;
    let levels = usize::from(auction.grid_size);
    let max_base_atoms = intent.sell[..levels]
        .iter()
        .copied()
        .max()
        .unwrap_or_default() as u128
        * u128::from(EXPECTED_BASE_UNIT_SIZE);
    let max_quote_units = intent.buy[..levels]
        .iter()
        .enumerate()
        .map(|(index, quantity)| u128::from(*quantity) * u128::from(prices[index]))
        .max()
        .unwrap_or_default();
    Ok((
        format_units(max_quote_units, config.quote_decimals),
        format_units(max_base_atoms, config.base_decimals),
    ))
}

fn fetch_json(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::blocking::Client::new);
    let response = client.get(url).timeout(Duration::from_secs(10)).send()?;
    let status = response.status();
    let bytes = response.bytes()?;
    if !status.is_success() {
        return Err(format!(
            "price request failed with HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        )
        .into());
    }
    Ok(bytes.to_vec())
}

fn rpc_request<T: for<'de> Deserialize<'de>>(
    rpc_urls: &[String],
    method: &str,
    params: serde_json::Value,
) -> Result<T, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
    .to_string();
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(reqwest::blocking::Client::new);
    let mut last_error = "no RPC endpoints configured".to_owned();
    if rpc_urls.is_empty() {
        return Err(format!("all Solana RPC endpoints failed for {method}: {last_error}").into());
    }
    for index in 0..rpc_urls.len() {
        let rpc_url = &rpc_urls[index];
        let response = match client
            .post(rpc_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(Duration::from_secs(10))
            .body(request.clone())
            .send()
        {
            Ok(response) => response,
            Err(error) => {
                last_error = format!("endpoint {} request failed: {error}", index + 1);
                continue;
            }
        };
        let status = response.status();
        let body = match response.bytes() {
            Ok(body) => body,
            Err(error) => {
                last_error = format!("endpoint {} response body failed: {error}", index + 1);
                continue;
            }
        };
        if !status.is_success() {
            last_error = format!(
                "endpoint {} returned HTTP {}: {}",
                index + 1,
                status,
                String::from_utf8_lossy(&body).trim()
            );
            continue;
        }
        match serde_json::from_slice::<RpcResponse<T>>(&body) {
            Ok(response) if response.error.is_none() => {
                if let Some(result) = response.result {
                    return Ok(result);
                }
                last_error = format!("endpoint {} returned no JSON-RPC result", index + 1);
            }
            Ok(response) => {
                let error = response.error.expect("error is present");
                last_error = format!(
                    "endpoint {} returned JSON-RPC error code={} message={} data={:?}",
                    index + 1,
                    error.code,
                    error.message,
                    error.data
                );
            }
            Err(error) => {
                last_error = format!("endpoint {} returned invalid JSON-RPC: {error}", index + 1);
            }
        }
    }
    Err(format!("all Solana RPC endpoints failed for {method}: {last_error}").into())
}

fn fetch_account_bytes(
    rpc_urls: &[String],
    address: &Pubkey,
) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    let result: RpcAccountResult = rpc_request(
        rpc_urls,
        "getAccountInfo",
        serde_json::json!([address.to_string(), {"encoding": "base64", "commitment": "processed"}]),
    )?;
    let Some(account) = result.value else {
        return Ok(None);
    };
    if account.data.1 != "base64" {
        return Err(format!("unexpected Solana account encoding {}", account.data.1).into());
    }
    Ok(Some(BASE64.decode(account.data.0)?))
}

fn fetch_anchor_account<T: AccountDeserialize>(
    rpc_urls: &[String],
    address: &Pubkey,
) -> Result<Option<T>, Box<dyn Error>> {
    fetch_account_bytes(rpc_urls, address)?
        .map(|bytes| T::try_deserialize(&mut bytes.as_slice()).map_err(Into::into))
        .transpose()
}

fn current_slot(rpc_urls: &[String]) -> Result<u64, Box<dyn Error>> {
    rpc_request(
        rpc_urls,
        "getSlot",
        serde_json::json!([{"commitment": "processed"}]),
    )
}

fn auction_window(
    market_start_slot: u64,
    submission_duration_slots: u64,
    slot: u64,
) -> Result<(u64, u64), Box<dyn Error>> {
    let elapsed = slot
        .checked_sub(market_start_slot)
        .ok_or("market has not reached its start slot")?;
    if submission_duration_slots == 0 {
        return Err("market submission duration is zero".into());
    }
    Ok((
        elapsed / submission_duration_slots,
        submission_duration_slots - elapsed % submission_duration_slots,
    ))
}

fn current_submission_window(config: &Config) -> Result<(u64, u64), Box<dyn Error>> {
    auction_window(
        config.market_start_slot,
        config.submission_duration_slots,
        current_slot(&config.rpc_urls)?,
    )
}

fn order_start_is_safe(remaining_slots: u64) -> bool {
    remaining_slots > MIN_ORDER_START_REMAINING_SLOTS
}

fn auction_grid(
    config: &Config,
    market: &beatex::Market,
    auction_id: u64,
) -> Result<beatex::Auction, Box<dyn Error>> {
    if market.base_unit_size != EXPECTED_BASE_UNIT_SIZE
        || market.quote_unit_size != EXPECTED_QUOTE_UNIT_SIZE
    {
        return Err(format!(
            "unsupported token unit sizes: expected base={} and quote={}, found base={} and quote={}",
            EXPECTED_BASE_UNIT_SIZE,
            EXPECTED_QUOTE_UNIT_SIZE,
            market.base_unit_size,
            market.quote_unit_size
        )
        .into());
    }
    let (auction_address, _) = Pubkey::find_program_address(
        &[
            b"auction",
            config.market.as_ref(),
            &auction_id.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let auction = fetch_anchor_account::<beatex::Auction>(&config.rpc_urls, &auction_address)?
        .ok_or("current auction is not initialized")?;
    if auction.market != config.market || auction.auction_id != auction_id {
        return Err("current auction account does not match the market schedule".into());
    }
    if usize::from(auction.grid_size) > LEVELS
        || auction.grid_size == 0
        || auction.grid_size % 2 == 0
    {
        return Err("current auction has an unsupported grid size".into());
    }
    Ok(auction)
}

fn sol_balance(rpc_urls: &[String], owner: Pubkey) -> Result<u64, Box<dyn Error>> {
    #[derive(Deserialize)]
    struct Balance {
        value: u64,
    }
    rpc_request(
        rpc_urls,
        "getBalance",
        serde_json::json!([owner.to_string(), {"commitment": "processed"}]),
    )
    .map(|value: Balance| value.value)
}

fn token_balance(
    rpc_urls: &[String],
    owner: Pubkey,
    mint: Pubkey,
) -> Result<(u128, u32), Box<dyn Error>> {
    let result: RpcTokenAccountsResult = rpc_request(
        rpc_urls,
        "getTokenAccountsByOwner",
        serde_json::json!([
            owner.to_string(),
            {"mint": mint.to_string()},
            {"encoding": "jsonParsed", "commitment": "processed"}
        ]),
    )?;
    let mut total = 0u128;
    let mut decimals = 0u32;
    for account in result.value {
        total = total
            .checked_add(
                account
                    .account
                    .data
                    .parsed
                    .info
                    .token_amount
                    .amount
                    .parse::<u128>()?,
            )
            .ok_or("token balance overflow")?;
        decimals = account.account.data.parsed.info.token_amount.decimals;
    }
    Ok((total, decimals))
}

fn token_source(
    rpc_urls: &[String],
    owner: Pubkey,
    mint: Pubkey,
) -> Result<Pubkey, Box<dyn Error>> {
    let result: RpcTokenAccountsResult = rpc_request(
        rpc_urls,
        "getTokenAccountsByOwner",
        serde_json::json!([
            owner.to_string(),
            {"mint": mint.to_string()},
            {"encoding": "jsonParsed", "commitment": "processed"}
        ]),
    )?;
    result
        .value
        .first()
        .ok_or_else(|| format!("wallet has no Token-2022 account for mint {mint}").into())
        .and_then(|account| Ok(Pubkey::from_str(&account.pubkey)?))
}

fn report_wallet_identity(config: &Config, market: &beatex::Market) {
    let Some(owner) = config.wallet_owner else {
        return;
    };
    let sol = sol_balance(&config.rpc_urls, owner)
        .map(|lamports| format_units(u128::from(lamports), 9))
        .unwrap_or_else(|error| format!("error: {error}"));
    let base = token_balance(&config.rpc_urls, owner, market.base_mint)
        .map(|(amount, decimals)| format_units(amount, decimals))
        .unwrap_or_else(|error| format!("error: {error}"));
    let quote = token_balance(&config.rpc_urls, owner, market.quote_mint)
        .map(|(amount, decimals)| format_units(amount, decimals))
        .unwrap_or_else(|error| format!("error: {error}"));
    println!(
        "[{}] wallet {owner} balances: SOL {sol} | {} {base} | {} {quote}",
        timestamp(),
        config.base_symbol,
        config.quote_symbol,
    );
}

fn checked_pow10(exponent: u32) -> Result<u128, Box<dyn Error>> {
    10u128
        .checked_pow(exponent)
        .ok_or_else(|| "decimal exponent is too large".into())
}

fn scaled_integer_price(
    digits: u128,
    exponent: i32,
    exponent_adjustment: i8,
) -> Result<u64, Box<dyn Error>> {
    let exponent = exponent
        .checked_add(i32::from(exponent_adjustment))
        .ok_or("price exponent overflow")?;
    let magnitude = exponent.unsigned_abs();
    let scale = checked_pow10(magnitude)?;
    let value = if exponent >= 0 {
        digits.checked_mul(scale).ok_or("price overflow")?
    } else {
        digits.checked_add(scale / 2).ok_or("price overflow")? / scale
    };
    Ok(u64::try_from(value)?)
}

fn decimal_parts(value: &str) -> Result<(u128, i32), Box<dyn Error>> {
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    if whole.starts_with('-')
        || whole.is_empty()
        || !whole.chars().all(|character| character.is_ascii_digit())
        || !fractional
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err("price must be a positive decimal".into());
    }
    let digits = format!("{whole}{fractional}").parse::<u128>()?;
    Ok((digits, -(i32::try_from(fractional.len())?)))
}

fn decimal_token_units(value: &str, decimals: u32, unit_size: u64) -> Result<u64, Box<dyn Error>> {
    let (digits, exponent) = decimal_parts(value)?;
    let effective_exponent = exponent
        .checked_add(i32::try_from(decimals)?)
        .ok_or("quantity exponent overflow")?;
    if effective_exponent < 0 {
        return Err("quantity has too many decimal places".into());
    }
    let atoms = digits
        .checked_mul(checked_pow10(effective_exponent as u32)?)
        .ok_or("quantity overflow")?;
    if atoms % u128::from(unit_size) != 0 {
        return Err("quantity is not an exact multiple of the base unit size".into());
    }
    Ok(u64::try_from(atoms / u128::from(unit_size))?)
}

fn expected_price(
    config: &Config,
    market: &beatex::Market,
) -> Result<(u64, String), Box<dyn Error>> {
    match config.source {
        PriceSource::Pyth => {
            let bytes = fetch_account_bytes(&config.rpc_urls, &market.oracle_price_account)?
                .ok_or("configured Pyth account does not exist")?;
            let update = PriceUpdateV2::try_deserialize(&mut bytes.as_slice())?;
            let price = update.price_message;
            if price.feed_id != market.oracle_feed_id || price.price <= 0 {
                return Err("Pyth account contains the wrong feed or a non-positive price".into());
            }
            Ok((
                scaled_integer_price(
                    u128::from(price.price as u64),
                    price.exponent,
                    market.oracle_exponent_adjustment,
                )?,
                format!("{}e{}@{}", price.price, price.exponent, price.publish_time),
            ))
        }
        PriceSource::Binance => {
            let response: BinanceTicker = serde_json::from_slice(&fetch_json(
                "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT",
            )?)?;
            let (digits, exponent) = decimal_parts(&response.price)?;
            Ok((
                scaled_integer_price(digits, exponent, market.oracle_exponent_adjustment)?,
                response.price,
            ))
        }
        PriceSource::Ema => Err("the ema source is maintained by the quoting loop".into()),
    }
}

fn grid_prices(auction: &beatex::Auction) -> Result<[u64; LEVELS], Box<dyn Error>> {
    let mut prices = [0u64; LEVELS];
    let center = i64::from(auction.grid_size / 2);
    let mut previous = None;
    for (index, price) in prices
        .iter_mut()
        .enumerate()
        .take(usize::from(auction.grid_size))
    {
        let offset = i64::try_from(index)? - center;
        let multiplier = i64::try_from(BPS_DENOMINATOR)?
            .checked_add(
                offset
                    .checked_mul(i64::from(auction.grid_step_bps))
                    .ok_or("grid multiplier overflow")?,
            )
            .ok_or("grid multiplier overflow")?;
        if multiplier <= 0 {
            return Err("grid contains a non-positive multiplier".into());
        }
        let numerator = u128::from(auction.reference_price)
            .checked_mul(multiplier as u128)
            .and_then(|value| value.checked_add(BPS_DENOMINATOR / 2))
            .ok_or("grid price overflow")?;
        *price = u64::try_from(numerator / BPS_DENOMINATOR)?;
        if previous.is_some_and(|previous| *price <= previous) {
            return Err("grid contains duplicate or unordered integer prices".into());
        }
        previous = Some(*price);
    }
    Ok(prices)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Buy,
    Sell,
    Both,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
            Side::Both => "two-sided",
        }
    }
}

/// The two makers quote opposite sides and swap every auction so neither
/// accumulates inventory: one buys in even auctions and sells in odd ones, the
/// other does the reverse.
fn quoting_side(buy_on_even: bool, auction_id: u64) -> Side {
    if (auction_id % 2 == 0) == buy_on_even {
        Side::Buy
    } else {
        Side::Sell
    }
}

/// A single-sided curve over every grid level. The quantity is `base` at the
/// expected price, grows by `slope` per grid step in the side's favourable
/// direction (lower prices for a buyer, higher for a seller), and shrinks by
/// `slope` per step the other way, never below one `slope`. The other maker's
/// mirrored curve crosses it near the expected price, so the auction clears
/// there with nonzero volume even without user orders.
fn one_sided_curve(
    auction: &beatex::Auction,
    expected: u64,
    side: Side,
    base_quantity: u64,
    slope_quantity: u64,
    max_quantity: u64,
) -> Result<([u64; LEVELS], [u64; LEVELS]), Box<dyn Error>> {
    let prices = grid_prices(auction)?;
    let denominator = u128::from(auction.reference_price)
        .checked_mul(u128::from(auction.grid_step_bps))
        .ok_or("distance overflow")?;
    if denominator == 0 {
        return Err("grid distance denominator is zero".into());
    }
    let mut buy = [0u64; LEVELS];
    let mut sell = [0u64; LEVELS];
    for index in 0..usize::from(auction.grid_size) {
        let price = prices[index];
        let steps = u64::try_from(
            u128::from(price.abs_diff(expected))
                .checked_mul(BPS_DENOMINATOR)
                .and_then(|value| value.checked_add(denominator / 2))
                .ok_or("distance overflow")?
                / denominator,
        )?;
        let favourable = match side {
            Side::Buy => price <= expected,
            Side::Sell => price >= expected,
            Side::Both => return Err("one-sided curves take a single side".into()),
        };
        let shift = slope_quantity.saturating_mul(steps);
        let quantity = if favourable {
            base_quantity.saturating_add(shift)
        } else {
            base_quantity.saturating_sub(shift)
        }
        .max(slope_quantity)
        .min(max_quantity);
        match side {
            Side::Buy => buy[index] = quantity,
            Side::Sell => sell[index] = quantity,
            Side::Both => unreachable!("one-sided curves take a single side"),
        }
    }
    Ok((buy, sell))
}

/// Advance the running EMA once per auction with the market's latest
/// clearing price and return it as an integer grid price. The program keeps
/// `last_clearing_price` unchanged across zero-volume auctions, so those
/// auctions repeat the last price; an oracle-free market starts at its initial
/// price.
fn update_ema(
    config: &Config,
    ema: &mut Option<(u64, f64)>,
    auction_id: u64,
) -> Result<u64, Box<dyn Error>> {
    if let Some((seen, value)) = *ema {
        if seen == auction_id {
            return Ok(value.round() as u64);
        }
    }
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_urls, &config.market)?
        .ok_or("market account does not exist")?;
    let price = market.last_clearing_price as f64;
    if price <= 0.0 {
        return Err("the market has no clearing price to average yet".into());
    }
    let value = match *ema {
        Some((_, previous)) => config.ema_alpha * price + (1.0 - config.ema_alpha) * previous,
        None => price,
    };
    *ema = Some((auction_id, value));
    Ok(value.round() as u64)
}

/// A two-sided V around `expected` with one grid level of spread on each side:
/// bids start at the nearest level below `expected` and asks at the nearest
/// level above, each growing by `slope` per further level up to `max`. A level
/// exactly at `expected` is left empty.
fn two_sided_curve(
    auction: &beatex::Auction,
    expected: u64,
    base_quantity: u64,
    slope_quantity: u64,
    max_quantity: u64,
) -> Result<([u64; LEVELS], [u64; LEVELS]), Box<dyn Error>> {
    let prices = grid_prices(auction)?;
    let denominator = u128::from(auction.reference_price)
        .checked_mul(u128::from(auction.grid_step_bps))
        .ok_or("distance overflow")?;
    if denominator == 0 {
        return Err("grid distance denominator is zero".into());
    }
    let mut buy = [0u64; LEVELS];
    let mut sell = [0u64; LEVELS];
    for index in 0..usize::from(auction.grid_size) {
        let price = prices[index];
        if price == expected {
            continue;
        }
        // Levels away from `expected`, rounded up so the nearest level on each
        // side is level one.
        let steps = u64::try_from(
            u128::from(price.abs_diff(expected))
                .checked_mul(BPS_DENOMINATOR)
                .and_then(|value| value.checked_add(denominator - 1))
                .ok_or("distance overflow")?
                / denominator,
        )?
        .max(1);
        let quantity = base_quantity
            .saturating_add(slope_quantity.saturating_mul(steps - 1))
            .min(max_quantity);
        if price < expected {
            buy[index] = quantity;
        } else {
            sell[index] = quantity;
        }
    }
    Ok((buy, sell))
}

fn dispatch(config: &Config, job: &OrderJob) -> Result<(), Box<dyn Error>> {
    let (Some(wallet), Some(state_dir), Some(_owner)) = (
        config.wallet.clone(),
        config.state_dir.clone(),
        config.wallet_owner,
    ) else {
        if config.once {
            println!("{}", serde_json::to_string(&job.intent)?);
            return Ok(());
        }
        return Err("--wallet and --state-dir are required for native order submission".into());
    };
    let base_source = config
        .base_source
        .ok_or("base token source was not initialized")?;
    let quote_source = config
        .quote_source
        .ok_or("quote token source was not initialized")?;
    let rpc_url = config
        .rpc_urls
        .first()
        .cloned()
        .ok_or("no Solana RPC endpoint configured")?;
    let events = beatex_devnet_client::execute_native_order(
        beatex_devnet_client::NativeOrderConfig {
            base_mint: job.base_mint,
            quote_mint: job.quote_mint,
            wallet: wallet.into(),
            state_dir: state_dir.into(),
            rpc_url,
            base_source,
            quote_source,
            deposit_base: 0,
            deposit_quote: 0,
            target_base: job.target_base,
            target_quote: job.target_quote,
        },
        beatex_devnet_client::NativeOrderIntent {
            auction_id: Some(job.auction_id),
            reference_price: job.intent.reference_price,
            grid_step_bps: job.intent.grid_step_bps,
            client_order_nonce: job.intent.client_order_nonce,
            buy: job.intent.buy,
            sell: job.intent.sell,
        },
    )?;
    for event in events {
        match event {
            beatex_devnet_client::NativeOrderEvent::Placed { auction_id } => {
                println!(
                    "[{}] auction {} placeOrder accepted",
                    timestamp(),
                    auction_id
                )
            }
            beatex_devnet_client::NativeOrderEvent::Replaced { auction_id } => println!(
                "[{}] auction {} replaceOrder accepted",
                timestamp(),
                auction_id
            ),
            beatex_devnet_client::NativeOrderEvent::Rollover {
                previous_auction_id,
                next_auction_id,
            } => println!(
                "[{}] auction {} rollover from auction {} accepted",
                timestamp(),
                next_auction_id,
                previous_auction_id
            ),
            beatex_devnet_client::NativeOrderEvent::Fill {
                auction_id,
                side,
                base_amount,
                quote_amount,
            } if base_amount > 0 && quote_amount > 0 => println!(
                "[{}] auction {} {} filled {} {} for {} {}",
                timestamp(),
                auction_id,
                side,
                config.base_symbol,
                format_units(u128::from(base_amount), config.base_decimals),
                config.quote_symbol,
                format_units(u128::from(quote_amount), config.quote_decimals)
            ),
            beatex_devnet_client::NativeOrderEvent::Refund {
                auction_id,
                base_amount,
                quote_amount,
            } if base_amount > 0 || quote_amount > 0 => println!(
                "[{}] auction {} refund completed ({} {}, {} {})",
                timestamp(),
                auction_id,
                config.base_symbol,
                format_units(u128::from(base_amount), config.base_decimals),
                config.quote_symbol,
                format_units(u128::from(quote_amount), config.quote_decimals)
            ),
            beatex_devnet_client::NativeOrderEvent::Fill { .. }
            | beatex_devnet_client::NativeOrderEvent::Refund { .. } => {}
        }
    }
    Ok(())
}

fn spawn_order_worker(
    config: Config,
) -> (
    SyncSender<OrderJob>,
    Receiver<OrderWorkerResult>,
    Receiver<Result<Duration, String>>,
) {
    let (job_sender, job_receiver) = sync_channel::<OrderJob>(1);
    let (result_sender, result_receiver) = sync_channel::<OrderWorkerResult>(1);
    let (ready_sender, ready_receiver) = sync_channel::<Result<Duration, String>>(1);
    thread::spawn(move || {
        let warm_started = Instant::now();
        let warm_result = if config.wallet.is_some() {
            beatex_devnet_client::warm_native_market_maker_provers()
                .map(|()| warm_started.elapsed())
        } else {
            Ok(Duration::ZERO)
        };
        if ready_sender.send(warm_result.clone()).is_err() || warm_result.is_err() {
            return;
        }
        while let Ok(job) = job_receiver.recv() {
            let ready = loop {
                match current_submission_window(&config) {
                    Ok((auction_id, _remaining_slots)) if auction_id < job.auction_id => {
                        thread::sleep(ORDER_OPEN_POLL_INTERVAL);
                    }
                    Ok((auction_id, remaining_slots)) if auction_id == job.auction_id => {
                        break order_start_is_safe(remaining_slots);
                    }
                    Ok(_) => break false,
                    Err(error) => {
                        eprintln!(
                            "[{}] auction {} pre-submit check failed: {error}",
                            timestamp(),
                            job.auction_id
                        );
                        break false;
                    }
                }
            };
            if !ready {
                if result_sender
                    .send(OrderWorkerResult::Skipped(job.auction_id))
                    .is_err()
                {
                    break;
                }
                continue;
            }
            let started = Instant::now();
            let result =
                match dispatch(&config, &job) {
                    Ok(()) => {
                        println!(
                        "[{}] auction {} {} {} order completed in {:.2}s (max {}: {}, max {}: {})",
                        timestamp(), job.auction_id, job.operation, job.side.label(),
                        started.elapsed().as_secs_f64(), config.quote_symbol, job.max_tusd,
                        config.base_symbol, job.max_tbtc
                    );
                        OrderWorkerResult::Submitted(job.auction_id)
                    }
                    Err(error) => {
                        eprintln!(
                            "[{}] auction {} {} order failed after {:.2}s: {error}",
                            timestamp(),
                            job.auction_id,
                            job.operation,
                            started.elapsed().as_secs_f64()
                        );
                        if error
                            .to_string()
                            .contains("transaction confirmation outcome unknown")
                            || error
                                .to_string()
                                .contains("transaction submission outcome unknown")
                        {
                            OrderWorkerResult::Uncertain(error.to_string())
                        } else {
                            OrderWorkerResult::Failed(job.auction_id)
                        }
                    }
                };
            if result_sender.send(result).is_err() {
                break;
            }
        }
    });
    (job_sender, result_receiver, ready_receiver)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut config = parse_config()?;
    if config.balance_every_auctions == 0 {
        return Err("balance-every-auctions must be positive".into());
    }
    if config.order_budget_bps == 0 || config.order_budget_bps > 10_000 {
        return Err("budget-bps must be between 1 and 10000".into());
    }
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_urls, &config.market)?
        .ok_or("market account does not exist")?;
    config.market_start_slot = market.market_start_slot;
    config.submission_duration_slots = market.submission_duration_slots;
    if let Some(owner) = config.wallet_owner {
        config.base_source = Some(token_source(&config.rpc_urls, owner, market.base_mint)?);
        config.quote_source = Some(token_source(&config.rpc_urls, owner, market.quote_mint)?);
        report_wallet_identity(&config, &market);
    } else if !config.once {
        return Err("--wallet is required for native order submission".into());
    }
    println!("[{}] warming native prover cache", timestamp());
    let (job_sender, result_receiver, ready_receiver) = spawn_order_worker(config.clone());
    match ready_receiver.recv() {
        Ok(Ok(elapsed)) => println!(
            "[{}] native prover cache ready in {:.2}s",
            timestamp(),
            elapsed.as_secs_f64()
        ),
        Ok(Err(error)) => {
            eprintln!(
                "[{}] native prover cache initialization failed",
                timestamp()
            );
            return Err(format!("native prover initialization failed: {error}").into());
        }
        Err(_) => {
            eprintln!(
                "[{}] native prover worker stopped during initialization",
                timestamp()
            );
            return Err("market-maker prover worker stopped during initialization".into());
        }
    }
    let mut last_balance_report = None;
    let mut last_submitted_auction = None;
    let mut in_flight_auction = None;
    // Running EMA of the market's clearing price for the `ema` source, updated
    // once per auction. A zero-volume auction repeats the last price.
    let mut ema: Option<(u64, f64)> = None;
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_signal = Arc::clone(&shutdown);
    ctrlc::set_handler(move || shutdown_signal.store(true, Ordering::Release))?;
    let mut shutdown_logged = false;
    loop {
        match result_receiver.try_recv() {
            Ok(OrderWorkerResult::Submitted(auction_id)) => {
                in_flight_auction = None;
                last_submitted_auction = Some(auction_id);
                if auction_id % config.balance_every_auctions == 0
                    && last_balance_report != Some(auction_id)
                {
                    report_balance(&config, auction_id);
                    last_balance_report = Some(auction_id);
                }
                if config.once {
                    return Ok(());
                }
            }
            Ok(OrderWorkerResult::Skipped(auction_id))
            | Ok(OrderWorkerResult::Failed(auction_id)) => {
                if in_flight_auction == Some(auction_id) {
                    in_flight_auction = None;
                }
            }
            Ok(OrderWorkerResult::Uncertain(error)) => {
                // An RPC can lose a transaction without confirming it. Once its
                // blockhash has expired it can no longer land, so the local
                // private state is still current if it did not land and the
                // on-chain balance revision still matches.
                eprintln!(
                    "[{}] order outcome unknown ({error}); verifying after the blockhash expires",
                    timestamp()
                );
                thread::sleep(UNCERTAIN_ORDER_SETTLE_WAIT);
                match verify_unconfirmed_order(&config, &error) {
                    Ok(()) => {
                        eprintln!("[{}] unconfirmed order did not land; resuming", timestamp());
                        in_flight_auction = None;
                    }
                    Err(reason) => {
                        return Err(format!(
                            "stopping market maker because a submitted order may have settled: {reason}"
                        )
                        .into());
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                return Err("market-maker order worker stopped unexpectedly".into());
            }
        }
        if shutdown.load(Ordering::Acquire) {
            if in_flight_auction.is_none() {
                return Ok(());
            }
            if !shutdown_logged {
                eprintln!(
                    "[{}] shutdown requested; finishing the active order",
                    timestamp()
                );
                shutdown_logged = true;
            }
            thread::sleep(config.interval);
            continue;
        }
        let (scheduled_auction_id, remaining_slots) = match current_submission_window(&config) {
            Ok(window) => window,
            Err(error) if error.to_string() == "market has not reached its start slot" => {
                thread::sleep(config.interval);
                continue;
            }
            Err(error) => {
                eprintln!(
                    "[{}] market maker iteration failed: {error}; retrying",
                    timestamp()
                );
                thread::sleep(config.interval);
                continue;
            }
        };
        if in_flight_auction.is_some() {
            thread::sleep(config.interval);
            continue;
        }
        let target_auction_id = if remaining_slots <= NEXT_AUCTION_PREPARE_REMAINING_SLOTS {
            scheduled_auction_id
                .checked_add(1)
                .ok_or("auction id overflow")?
        } else {
            scheduled_auction_id
        };
        if target_auction_id == scheduled_auction_id && !order_start_is_safe(remaining_slots) {
            thread::sleep(config.interval);
            continue;
        }
        if last_submitted_auction == Some(target_auction_id) {
            thread::sleep(config.interval);
            continue;
        }
        let auction = match auction_grid(&config, &market, target_auction_id) {
            Ok(grid) => grid,
            Err(error) if error.to_string() == "current auction is not initialized" => {
                thread::sleep(config.interval);
                continue;
            }
            Err(error) => {
                eprintln!(
                    "[{}] market maker iteration failed: {error}; retrying",
                    timestamp()
                );
                thread::sleep(config.interval);
                continue;
            }
        };
        let base_quantity = decimal_token_units(
            &config.base_quantity,
            config.base_decimals,
            market.base_unit_size,
        )?;
        let slope_quantity = decimal_token_units(
            &config.slope_quantity,
            config.base_decimals,
            market.base_unit_size,
        )?;
        let max_quantity = decimal_token_units(
            &config.max_quantity,
            config.base_decimals,
            market.base_unit_size,
        )?;
        // The private ledger counts tBTC in token atoms (satoshis), not in
        // protocol base units, and tUSD in quote atoms.
        let target_base = decimal_token_units(&config.target_base, config.base_decimals, 1)?;
        let configured_max = market.max_order_quantity.min(CIRCUIT_QUANTITY_MAX);
        if base_quantity > max_quantity || max_quantity > configured_max {
            return Err(format!(
                "quantities must satisfy base <= max <= {configured_max} base units"
            )
            .into());
        }
        let expected_quote = if config.source == PriceSource::Ema {
            update_ema(&config, &mut ema, auction.auction_id)
                .map(|price| (price, format!("ema {price}")))
        } else {
            expected_price(&config, &market)
        };
        let (expected, observed) = match expected_quote {
            Ok(price) => price,
            Err(error) => {
                eprintln!(
                    "[{}] market maker iteration failed: {error}; retrying",
                    timestamp()
                );
                thread::sleep(config.interval);
                continue;
            }
        };
        // Quote atoms per protocol base unit times base units: equal value on both sides.
        let target_quote = (target_base / market.base_unit_size).saturating_mul(expected);
        // External feeds alternate one-sided quotes between two makers; the EMA
        // maker is the only liquidity in its market and quotes both sides.
        let (side, (mut buy, mut sell)) = if config.source == PriceSource::Ema {
            (
                Side::Both,
                two_sided_curve(
                    &auction,
                    expected,
                    base_quantity,
                    slope_quantity,
                    max_quantity,
                )?,
            )
        } else {
            let side = quoting_side(config.buy_on_even, auction.auction_id);
            (
                side,
                one_sided_curve(
                    &auction,
                    expected,
                    side,
                    base_quantity,
                    slope_quantity,
                    max_quantity,
                )?,
            )
        };
        cap_order_budget(&config, &market, &auction, &mut buy, &mut sell)?;
        let nonce = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
        let intent = OrderIntent {
            source: match config.source {
                PriceSource::Pyth => "pyth",
                PriceSource::Binance => "binance",
                PriceSource::Ema => "ema",
            },
            observed_price: observed,
            expected_price: expected,
            reference_price: auction.reference_price,
            grid_step_bps: auction.grid_step_bps,
            grid_size: auction.grid_size,
            base_unit_size: market.base_unit_size,
            quote_unit_size: market.quote_unit_size,
            client_order_nonce: nonce,
            buy,
            sell,
        };
        let operation = order_operation(&config, auction.auction_id);
        let (max_tusd, max_tbtc) = order_maxima(&config, &auction, &intent)?;
        match current_submission_window(&config) {
            Ok((auction_id, remaining_slots))
                if auction_id == auction.auction_id && order_start_is_safe(remaining_slots) => {}
            Ok((auction_id, remaining_slots))
                if auction_id.checked_add(1) == Some(auction.auction_id)
                    && remaining_slots <= NEXT_AUCTION_PREPARE_REMAINING_SLOTS => {}
            Ok((_auction_id, _remaining_slots)) => {
                thread::sleep(config.interval);
                continue;
            }
            Err(error) => {
                eprintln!(
                    "[{}] market maker iteration failed: {error}; retrying",
                    timestamp()
                );
                thread::sleep(config.interval);
                continue;
            }
        }
        job_sender
            .send(OrderJob {
                auction_id: auction.auction_id,
                operation,
                side,
                target_base,
                target_quote,
                max_tusd,
                max_tbtc,
                intent,
                base_mint: market.base_mint,
                quote_mint: market.quote_mint,
            })
            .map_err(|_| "market-maker order worker is unavailable")?;
        in_flight_auction = Some(auction.auction_id);
        thread::sleep(config.interval);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("[{}] market maker failed: {error}", timestamp());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auction(reference_price: u64, grid_step_bps: u16) -> beatex::Auction {
        beatex::Auction {
            version: beatex::state::PROTOCOL_VERSION,
            market: Pubkey::new_unique(),
            auction_id: 0,
            key_epoch: Pubkey::new_unique(),
            state: beatex::AuctionState::Pending,
            open_slot: 0,
            submission_end_slot: 1,
            aggregation_deadline_slot: 2,
            decryption_deadline_slot: 3,
            reference_price,
            reference_from_oracle: true,
            oracle_publish_time: 0,
            grid_step_bps,
            grid_size: 17,
            allocation_rule_version: 2,
            order_circuit_version: 2,
            claim_circuit_version: 2,
            finalized_order_count: 0,
            aggregate_buy: [[0; 64]; LEVELS],
            aggregate_sell: [[0; 64]; LEVELS],
            decrypted_demand: [0; LEVELS],
            decrypted_supply: [0; LEVELS],
            decrypted_entry_bitmap: 0,
            clearing_index: 0,
            clearing_price: 0,
            clearing_volume: 0,
            base_residual: 0,
            quote_residual: 0,
            bump: 0,
        }
    }

    #[test]
    fn one_sided_curves_cross_at_the_expected_price() {
        let auction = auction(100_000, 25);
        let (buy, no_sell) = one_sided_curve(&auction, 100_000, Side::Buy, 5, 1, 15).unwrap();
        let (no_buy, sell) = one_sided_curve(&auction, 100_000, Side::Sell, 5, 1, 15).unwrap();
        assert!(no_sell.iter().chain(&no_buy).all(|quantity| *quantity == 0));
        assert_eq!((buy[8], buy[7], buy[9], buy[16]), (5, 6, 4, 1));
        assert_eq!((sell[8], sell[9], sell[7], sell[0]), (5, 6, 4, 1));
        assert!(buy.iter().all(|quantity| *quantity > 0));
        let volumes: Vec<u64> = buy.iter().zip(&sell).map(|(b, s)| *b.min(s)).collect();
        let best = volumes.iter().copied().max().unwrap();
        assert_eq!(volumes.iter().position(|volume| *volume == best), Some(8));
    }

    #[test]
    fn two_sided_curve_leaves_one_level_of_spread() {
        let auction = auction(100_000, 25);
        let prices = grid_prices(&auction).unwrap();
        // EMA exactly on the centre level: that level stays empty.
        let (buy, sell) = two_sided_curve(&auction, prices[8], 5, 1, 15).unwrap();
        assert_eq!((buy[8], sell[8]), (0, 0));
        assert_eq!((buy[7], buy[6], buy[0]), (5, 6, 12));
        assert_eq!((sell[9], sell[10], sell[16]), (5, 6, 12));
        assert!(buy[9..]
            .iter()
            .chain(&sell[..9])
            .all(|quantity| *quantity == 0));
        // EMA between two levels: the nearest level on each side starts the V.
        let (buy, sell) = two_sided_curve(&auction, prices[8] + 1, 5, 1, 15).unwrap();
        assert_eq!((buy[8], sell[9], sell[8]), (5, 5, 0));
    }

    #[test]
    fn makers_take_opposite_sides_and_alternate() {
        assert_eq!(quoting_side(true, 10), Side::Buy);
        assert_eq!(quoting_side(false, 10), Side::Sell);
        assert_eq!(quoting_side(true, 11), Side::Sell);
        assert_eq!(quoting_side(false, 11), Side::Buy);
    }

    #[test]
    fn grid_uses_half_up_basis_point_prices() {
        let prices = grid_prices(&auction(99_950, 25)).unwrap();
        assert_eq!(prices[8], 99_950);
        assert_eq!(prices[9], 100_200);
    }

    #[test]
    fn duplicate_integer_prices_are_rejected() {
        assert!(grid_prices(&auction(100, 1)).is_err());
    }

    #[test]
    fn human_base_quantity_uses_one_hundred_satoshi_units() {
        assert_eq!(decimal_token_units("0.000001", 8, 100).unwrap(), 1);
        assert_eq!(decimal_token_units("1.25", 8, 100).unwrap(), 1_250_000);
        assert!(decimal_token_units("0.00000001", 8, 100).is_err());
    }

    #[test]
    fn price_normalization_matches_equal_micro_units() {
        assert_eq!(
            scaled_integer_price(10_000_000_000_000, -8, 0).unwrap(),
            100_000
        );
        let (digits, exponent) = decimal_parts("100000.25").unwrap();
        assert_eq!(scaled_integer_price(digits, exponent, 0).unwrap(), 100_000);
    }

    #[test]
    fn auction_window_tracks_twenty_slot_boundaries() {
        assert_eq!(auction_window(100, 20, 100).unwrap(), (0, 20));
        assert_eq!(auction_window(100, 20, 115).unwrap(), (0, 5));
        assert_eq!(auction_window(100, 20, 116).unwrap(), (0, 4));
        assert_eq!(auction_window(100, 20, 120).unwrap(), (1, 20));
        assert!(auction_window(100, 20, 99).is_err());
        assert!(auction_window(100, 0, 100).is_err());
    }

    #[test]
    fn order_start_requires_proof_budget_before_collection_deadline() {
        assert!(order_start_is_safe(MIN_ORDER_START_REMAINING_SLOTS + 1));
        assert!(!order_start_is_safe(MIN_ORDER_START_REMAINING_SLOTS));
        assert!(!order_start_is_safe(1));
    }
}

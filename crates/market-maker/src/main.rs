use std::{
    env,
    error::Error,
    io::Write,
    process::{Command, Stdio},
    str::FromStr,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use {
    anchor_lang::{prelude::Pubkey, AccountDeserialize},
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    serde::{Deserialize, Serialize},
};

const LEVELS: usize = 17;
const BPS_DENOMINATOR: u128 = 10_000;
const BASE_TOKEN_DECIMALS: u32 = 8;
const EXPECTED_BASE_UNIT_SIZE: u64 = 100;
const EXPECTED_QUOTE_UNIT_SIZE: u64 = 1;
const CIRCUIT_QUANTITY_MAX: u64 = (1 << 28) - 1;
const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriceSource {
    Pyth,
    Binance,
}

#[derive(Debug)]
struct Config {
    source: PriceSource,
    rpc_url: String,
    market: Pubkey,
    base_quantity: String,
    slope_quantity: String,
    max_quantity: String,
    interval: Duration,
    executor: Option<String>,
    once: bool,
}

#[derive(Debug, Serialize)]
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

#[derive(Deserialize)]
struct BinanceTicker {
    price: String,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: T,
}

#[derive(Deserialize)]
struct RpcAccountResult {
    value: Option<RpcAccount>,
}

#[derive(Deserialize)]
struct RpcAccount {
    data: (String, String),
}

fn usage() {
    eprintln!(
        "Usage: beatex-market-maker --source pyth|binance --market ADDRESS \
         [--rpc-url URL] [--base-quantity TBTC] [--slope-quantity TBTC] \
         [--max-quantity TBTC] [--interval-seconds N] [--executor COMMAND] [--once]\n\n\
         Quantities are human tBTC decimal strings. The bot reads the current Market and \
         Auction before every order and uses the auction's reference price, basis-point \
         grid step, and grid size. The executor receives JSON on stdin and must return only \
         after the submit, replace, or rollover transaction is confirmed."
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
        other => return Err(format!("unsupported price source {other}").into()),
    };
    Ok(Config {
        source,
        rpc_url: optional("--rpc-url")
            .or_else(|| env::var("BEATEX_RPC_URL").ok())
            .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        market: Pubkey::from_str(&value("--market")?)?,
        base_quantity: optional("--base-quantity").unwrap_or_else(|| "0.0001".to_owned()),
        slope_quantity: optional("--slope-quantity").unwrap_or_else(|| "0.0001".to_owned()),
        max_quantity: optional("--max-quantity").unwrap_or_else(|| "0.001".to_owned()),
        interval: Duration::from_secs(
            optional("--interval-seconds")
                .unwrap_or_else(|| "10".to_owned())
                .parse()?,
        ),
        executor: optional("--executor"),
        once: args.iter().any(|arg| arg == "--once"),
    })
}

fn fetch_json(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("price request failed with {}", output.status).into());
    }
    Ok(output.stdout)
}

fn rpc_request<T: for<'de> Deserialize<'de>>(
    rpc_url: &str,
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
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("Solana RPC request failed with {}", output.status).into());
    }
    Ok(serde_json::from_slice::<RpcResponse<T>>(&output.stdout)?.result)
}

fn fetch_account_bytes(rpc_url: &str, address: &Pubkey) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    let result: RpcAccountResult = rpc_request(
        rpc_url,
        "getAccountInfo",
        serde_json::json!([address.to_string(), {"encoding": "base64"}]),
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
    rpc_url: &str,
    address: &Pubkey,
) -> Result<Option<T>, Box<dyn Error>> {
    fetch_account_bytes(rpc_url, address)?
        .map(|bytes| T::try_deserialize(&mut bytes.as_slice()).map_err(Into::into))
        .transpose()
}

fn prepare_current_auction(executor: Option<&str>) -> Result<(), Box<dyn Error>> {
    let Some(executor) = executor else {
        return Ok(());
    };
    let status = Command::new(executor)
        .env("BEATEX_MARKET_MAKER_ACTION", "initialize-auction")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(format!("auction initializer failed with {status}").into());
    }
    Ok(())
}

fn current_grid(config: &Config) -> Result<(beatex::Market, beatex::Auction), Box<dyn Error>> {
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &config.market)?
        .ok_or("market account does not exist")?;
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
    let slot: u64 = rpc_request(&config.rpc_url, "getSlot", serde_json::json!([]))?;
    if slot < market.market_start_slot {
        return Err("market has not reached its start slot".into());
    }
    let auction_id = (slot - market.market_start_slot) / market.submission_duration_slots;
    let (auction_address, _) = Pubkey::find_program_address(
        &[
            b"auction",
            config.market.as_ref(),
            &auction_id.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let auction = fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_address)?
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
    Ok((market, auction))
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
            let bytes = fetch_account_bytes(&config.rpc_url, &market.oracle_price_account)?
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

fn v_shape(
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
        let distance = u64::try_from(
            u128::from(price.abs_diff(expected))
                .checked_mul(BPS_DENOMINATOR)
                .and_then(|value| value.checked_add(denominator / 2))
                .ok_or("distance overflow")?
                / denominator,
        )?;
        let quantity = base_quantity
            .saturating_add(slope_quantity.saturating_mul(distance))
            .min(max_quantity);
        if price < expected {
            buy[index] = quantity;
        } else if price > expected {
            sell[index] = quantity;
        }
    }
    Ok((buy, sell))
}

fn dispatch(intent: &OrderIntent, executor: Option<&str>) -> Result<(), Box<dyn Error>> {
    let encoded = serde_json::to_vec(intent)?;
    if let Some(executor) = executor {
        let mut child = Command::new(executor)
            .env("BEATEX_MARKET_MAKER_ACTION", "order")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("executor stdin is unavailable")?
            .write_all(&encoded)?;
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("order executor failed with {status}").into());
        }
    } else {
        println!("{}", String::from_utf8(encoded)?);
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let config = parse_config()?;
    loop {
        prepare_current_auction(config.executor.as_deref())?;
        let (market, auction) = current_grid(&config)?;
        let base_quantity = decimal_token_units(
            &config.base_quantity,
            BASE_TOKEN_DECIMALS,
            market.base_unit_size,
        )?;
        let slope_quantity = decimal_token_units(
            &config.slope_quantity,
            BASE_TOKEN_DECIMALS,
            market.base_unit_size,
        )?;
        let max_quantity = decimal_token_units(
            &config.max_quantity,
            BASE_TOKEN_DECIMALS,
            market.base_unit_size,
        )?;
        let configured_max = market.max_order_quantity.min(CIRCUIT_QUANTITY_MAX);
        if base_quantity > max_quantity || max_quantity > configured_max {
            return Err(format!(
                "quantities must satisfy base <= max <= {configured_max} base units"
            )
            .into());
        }
        let (expected, observed) = expected_price(&config, &market)?;
        let (buy, sell) = v_shape(
            &auction,
            expected,
            base_quantity,
            slope_quantity,
            max_quantity,
        )?;
        let nonce = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
        let intent = OrderIntent {
            source: match config.source {
                PriceSource::Pyth => "pyth",
                PriceSource::Binance => "binance",
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
        dispatch(&intent, config.executor.as_deref())?;
        if config.once {
            return Ok(());
        }
        thread::sleep(config.interval);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("market maker failed: {error}");
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
    fn curve_is_two_sided_v_without_crossed_level() {
        let auction = auction(100_000, 25);
        let (buy, sell) = v_shape(&auction, 100_000, 1, 2, 15).unwrap();
        assert_eq!(buy[8], 0);
        assert_eq!(sell[8], 0);
        assert!(buy[..8].iter().all(|quantity| *quantity > 0));
        assert!(sell[9..].iter().all(|quantity| *quantity > 0));
        assert!(buy
            .iter()
            .zip(sell)
            .all(|(buy, sell)| *buy == 0 || sell == 0));
        assert!(buy[0] > buy[7]);
        assert!(sell[16] > sell[9]);
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
}

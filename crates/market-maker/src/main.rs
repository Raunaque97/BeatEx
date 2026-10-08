use std::{
    env,
    error::Error,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use {
    anchor_lang::AccountDeserialize,
    base64::{engine::general_purpose::STANDARD as BASE64, Engine},
    pyth_solana_receiver_sdk::price_update::PriceUpdateV2,
    serde::{Deserialize, Serialize},
};

const LEVELS: usize = 17;
const PYTH_BTC_USD_FEED: &str = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";
const PYTH_DEVNET_BTC_USD_ACCOUNT: &str = "4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo";
const DEVNET_RPC_URL: &str = "https://api.devnet.solana.com";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriceSource {
    Pyth,
    Binance,
}

#[derive(Debug)]
struct Config {
    source: PriceSource,
    reference_price: u64,
    tick_size: u64,
    oracle_exponent_adjustment: i8,
    base_quantity: u64,
    slope_quantity: u64,
    max_quantity: u64,
    interval: Duration,
    executor: Option<String>,
    once: bool,
}

#[derive(Debug, Serialize)]
struct OrderIntent {
    source: &'static str,
    observed_price: String,
    expected_price_lots: u64,
    reference_price: u64,
    tick_size: u64,
    client_order_nonce: u64,
    buy: [u64; LEVELS],
    sell: [u64; LEVELS],
}

#[derive(Deserialize)]
struct BinanceTicker {
    price: String,
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

fn usage() {
    eprintln!(
        "Usage: beatex-market-maker --source pyth|binance --reference-price N --tick-size N \
         [--oracle-exponent-adjustment N] [--base-quantity N] [--slope-quantity N] \
         [--max-quantity N] [--interval-seconds N] [--executor COMMAND] [--once]\n\n\
         The executor receives one JSON order intent on stdin and must exit successfully only \
         after the corresponding submit/replace/rollover transaction is confirmed. Without an \
         executor, intents are printed as JSON lines."
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
        reference_price: value("--reference-price")?.parse()?,
        tick_size: value("--tick-size")?.parse()?,
        oracle_exponent_adjustment: optional("--oracle-exponent-adjustment")
            .unwrap_or_else(|| "-2".to_owned())
            .parse()?,
        base_quantity: optional("--base-quantity")
            .unwrap_or_else(|| "1".to_owned())
            .parse()?,
        slope_quantity: optional("--slope-quantity")
            .unwrap_or_else(|| "1".to_owned())
            .parse()?,
        max_quantity: optional("--max-quantity")
            .unwrap_or_else(|| "15".to_owned())
            .parse()?,
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

fn fetch_pyth_account() -> Result<PriceUpdateV2, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [PYTH_DEVNET_BTC_USD_ACCOUNT, {"encoding": "base64"}],
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
            DEVNET_RPC_URL,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("Solana RPC request failed with {}", output.status).into());
    }
    let response: RpcResponse = serde_json::from_slice(&output.stdout)?;
    let account = response.result.value.ok_or("Pyth account does not exist")?;
    if account.data.1 != "base64" {
        return Err(format!("unexpected Solana account encoding {}", account.data.1).into());
    }
    let bytes = BASE64.decode(account.data.0)?;
    PriceUpdateV2::try_deserialize(&mut bytes.as_slice()).map_err(Into::into)
}

fn checked_pow10(exponent: u32) -> Result<u128, Box<dyn Error>> {
    10u128
        .checked_pow(exponent)
        .ok_or_else(|| "decimal exponent is too large".into())
}

fn round_to_tick(value: u128, tick_size: u64) -> Result<u64, Box<dyn Error>> {
    if tick_size == 0 {
        return Err("tick size must be positive".into());
    }
    let tick = u128::from(tick_size);
    let rounded = value
        .checked_add(tick / 2)
        .and_then(|value| value.checked_div(tick))
        .and_then(|ticks| ticks.checked_mul(tick))
        .ok_or("price rounding overflow")?;
    Ok(u64::try_from(rounded)?)
}

fn scaled_integer_price(
    digits: u128,
    exponent: i32,
    exponent_adjustment: i8,
    tick_size: u64,
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
    round_to_tick(value, tick_size)
}

fn decimal_parts(value: &str) -> Result<(u128, i32), Box<dyn Error>> {
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    if whole.starts_with('-') || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err("price must be a positive decimal".into());
    }
    if !fractional.chars().all(|c| c.is_ascii_digit()) {
        return Err("price must be a positive decimal".into());
    }
    let digits = format!("{whole}{fractional}").parse::<u128>()?;
    Ok((digits, -(i32::try_from(fractional.len())?)))
}

fn expected_price(config: &Config) -> Result<(u64, String), Box<dyn Error>> {
    match config.source {
        PriceSource::Pyth => {
            let update = fetch_pyth_account()?;
            let price = update.price_message;
            let expected_feed = hex_to_32(PYTH_BTC_USD_FEED)?;
            if price.feed_id != expected_feed || price.price <= 0 {
                return Err("Pyth account contains the wrong feed or a non-positive price".into());
            }
            Ok((
                scaled_integer_price(
                    u128::from(price.price as u64),
                    price.exponent,
                    config.oracle_exponent_adjustment,
                    config.tick_size,
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
                scaled_integer_price(
                    digits,
                    exponent,
                    config.oracle_exponent_adjustment,
                    config.tick_size,
                )?,
                response.price,
            ))
        }
    }
}

fn hex_to_32(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    if value.len() != 64 {
        return Err("feed id must contain 64 hex digits".into());
    }
    let mut bytes = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(bytes)
}

fn v_shape(
    config: &Config,
    expected: u64,
) -> Result<([u64; LEVELS], [u64; LEVELS]), Box<dyn Error>> {
    let radius = (LEVELS / 2) as u64;
    let lower = config
        .reference_price
        .checked_sub(
            radius
                .checked_mul(config.tick_size)
                .ok_or("grid overflow")?,
        )
        .ok_or("grid underflow")?;
    let mut buy = [0u64; LEVELS];
    let mut sell = [0u64; LEVELS];
    for index in 0..LEVELS {
        let price = lower
            .checked_add(
                (index as u64)
                    .checked_mul(config.tick_size)
                    .ok_or("grid overflow")?,
            )
            .ok_or("grid overflow")?;
        let distance = price.abs_diff(expected) / config.tick_size;
        let quantity = config
            .base_quantity
            .saturating_add(config.slope_quantity.saturating_mul(distance))
            .min(config.max_quantity);
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
    if config.base_quantity > config.max_quantity || config.max_quantity > 15 {
        return Err("quantities must satisfy base <= max <= 15".into());
    }
    loop {
        let (expected, observed) = expected_price(&config)?;
        let (buy, sell) = v_shape(&config, expected)?;
        let nonce = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
        let intent = OrderIntent {
            source: match config.source {
                PriceSource::Pyth => "pyth",
                PriceSource::Binance => "binance",
            },
            observed_price: observed,
            expected_price_lots: expected,
            reference_price: config.reference_price,
            tick_size: config.tick_size,
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

    fn config() -> Config {
        Config {
            source: PriceSource::Pyth,
            reference_price: 100,
            tick_size: 5,
            oracle_exponent_adjustment: 0,
            base_quantity: 1,
            slope_quantity: 2,
            max_quantity: 15,
            interval: Duration::from_secs(1),
            executor: None,
            once: true,
        }
    }

    #[test]
    fn curve_is_two_sided_v_without_crossed_level() {
        let (buy, sell) = v_shape(&config(), 100).unwrap();
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
    fn price_normalization_matches_program_policy() {
        assert_eq!(
            scaled_integer_price(10_000_000_000_000, -8, -2, 1).unwrap(),
            1000
        );
        let (digits, exponent) = decimal_parts("100000.25").unwrap();
        assert_eq!(scaled_integer_price(digits, exponent, -2, 1).unwrap(), 1000);
    }
}

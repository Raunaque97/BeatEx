//! Off-chain indexer for BeatEx auction outcomes.
//!
//! The program emits `AuctionCleared`, `AuctionCancelled` and
//! `EmptyAuctionClosed` through Anchor's event CPI. Those instructions are the
//! only ones that reference the event-authority PDA, so following that one
//! address with `getSignaturesForAddress` yields exactly the transactions to
//! decode. Records are kept in memory, appended to a JSONL file, and served as
//! per-market auction history for the web app's price chart.

use {
    anchor_lang::{event::EVENT_IX_TAG_LE, prelude::Pubkey, AnchorDeserialize, Discriminator},
    axum::{
        extract::{Path, Query, State},
        http::{header, HeaderValue, StatusCode},
        routing::get,
        Json, Router,
    },
    beatex::{AuctionCancelled, AuctionCleared, EmptyAuctionClosed},
    serde::{Deserialize, Serialize},
    std::{
        collections::BTreeMap,
        env,
        error::Error,
        fs::{self, File, OpenOptions},
        io::{BufRead, BufReader, Write},
        net::SocketAddr,
        path::PathBuf,
        sync::{Arc, RwLock},
        thread,
        time::Duration,
    },
    tower_http::set_header::SetResponseHeaderLayer,
};

const SIGNATURE_PAGE: usize = 1_000;
const MAX_HISTORY_RESPONSE: usize = 500;

struct Config {
    rpc_url: String,
    listen: SocketAddr,
    data_dir: PathBuf,
    poll_interval: Duration,
}

fn parse_config() -> Result<Config, Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|arg| arg == flag)
            .and_then(|index| args.get(index + 1).cloned())
    };
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "Usage: beatex-indexer --rpc-url URL --data-dir DIR [--listen ADDR] [--poll-ms N]\n\n\
             Follows BeatEx auction events and serves GET /v1/markets/{{market}}/auctions."
        );
        std::process::exit(0);
    }
    let rpc_url = value("--rpc-url").ok_or("--rpc-url is required")?;
    if rpc_url.contains("mainnet") {
        return Err("the indexer is configured for Devnet and localnet only".into());
    }
    Ok(Config {
        rpc_url,
        listen: value("--listen")
            .unwrap_or_else(|| "127.0.0.1:8787".to_owned())
            .parse()?,
        data_dir: PathBuf::from(value("--data-dir").ok_or("--data-dir is required")?),
        poll_interval: Duration::from_millis(
            value("--poll-ms").map_or(Ok(2_000), |ms| ms.parse())?,
        ),
    })
}

/// One auction's final outcome. Quantities are strings because they are u64
/// values that JavaScript clients parse with `BigInt`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuctionRecord {
    market: String,
    auction_id: String,
    outcome: Outcome,
    reference_price: String,
    reference_from_oracle: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    grid_step_bps: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grid_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearing_index: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearing_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearing_volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accepted_order_count: Option<String>,
    /// Decrypted aggregate demand and supply per price level, settled auctions only.
    #[serde(skip_serializing_if = "Option::is_none")]
    demand: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supply: Option<Vec<String>>,
    slot: String,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_time: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Outcome {
    Settled,
    Cancelled,
    Empty,
}

impl AuctionRecord {
    fn auction_number(&self) -> u64 {
        self.auction_id.parse().unwrap_or_default()
    }
}

/// Decode the BeatEx events carried by one self-CPI inner instruction.
fn decode_event(
    data: &[u8],
    signature: &str,
    block_time: Option<i64>,
) -> Result<Option<AuctionRecord>, Box<dyn Error>> {
    if data.len() < 16 || &data[..8] != EVENT_IX_TAG_LE {
        return Ok(None);
    }
    let (discriminator, mut body) = (&data[8..16], &data[16..]);
    let record = |market: Pubkey,
                  auction_id: u64,
                  outcome: Outcome,
                  reference_price: u64,
                  reference_from_oracle: bool,
                  slot: u64| AuctionRecord {
        market: market.to_string(),
        auction_id: auction_id.to_string(),
        outcome,
        reference_price: reference_price.to_string(),
        reference_from_oracle,
        grid_step_bps: None,
        grid_size: None,
        clearing_index: None,
        clearing_price: None,
        clearing_volume: None,
        accepted_order_count: None,
        demand: None,
        supply: None,
        slot: slot.to_string(),
        signature: signature.to_owned(),
        block_time,
    };
    Ok(Some(if discriminator == AuctionCleared::DISCRIMINATOR {
        let event = AuctionCleared::deserialize(&mut body)?;
        let quantities = |levels: &[u64]| {
            Some(
                levels[..usize::from(event.grid_size).min(levels.len())]
                    .iter()
                    .map(u64::to_string)
                    .collect(),
            )
        };
        AuctionRecord {
            grid_step_bps: Some(event.grid_step_bps),
            grid_size: Some(event.grid_size),
            clearing_index: Some(event.clearing_index),
            clearing_price: Some(event.clearing_price.to_string()),
            clearing_volume: Some(event.clearing_volume.to_string()),
            accepted_order_count: Some(event.accepted_order_count.to_string()),
            demand: quantities(&event.demand),
            supply: quantities(&event.supply),
            ..record(
                event.market,
                event.auction_id,
                Outcome::Settled,
                event.reference_price,
                event.reference_from_oracle,
                event.slot,
            )
        }
    } else if discriminator == AuctionCancelled::DISCRIMINATOR {
        let event = AuctionCancelled::deserialize(&mut body)?;
        AuctionRecord {
            accepted_order_count: Some(event.accepted_order_count.to_string()),
            ..record(
                event.market,
                event.auction_id,
                Outcome::Cancelled,
                event.reference_price,
                event.reference_from_oracle,
                event.slot,
            )
        }
    } else if discriminator == EmptyAuctionClosed::DISCRIMINATOR {
        let event = EmptyAuctionClosed::deserialize(&mut body)?;
        record(
            event.market,
            event.auction_id,
            Outcome::Empty,
            event.reference_price,
            event.reference_from_oracle,
            event.slot,
        )
    } else {
        return Ok(None);
    }))
}

/// Indexed records plus the newest processed signature, persisted as an
/// append-only JSONL log and a cursor file.
struct Store {
    records: BTreeMap<(String, u64), AuctionRecord>,
    cursor: Option<String>,
    data_dir: PathBuf,
}

impl Store {
    fn open(data_dir: PathBuf) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(&data_dir)?;
        let mut store = Self {
            records: BTreeMap::new(),
            cursor: fs::read_to_string(data_dir.join("cursor"))
                .ok()
                .map(|cursor| cursor.trim().to_owned())
                .filter(|cursor| !cursor.is_empty()),
            data_dir,
        };
        if let Ok(file) = File::open(store.log_path()) {
            for line in BufReader::new(file).lines() {
                let line = line?;
                if !line.trim().is_empty() {
                    store.insert(serde_json::from_str(&line)?);
                }
            }
        }
        Ok(store)
    }

    fn log_path(&self) -> PathBuf {
        self.data_dir.join("auctions.jsonl")
    }

    fn insert(&mut self, record: AuctionRecord) {
        self.records
            .insert((record.market.clone(), record.auction_number()), record);
    }

    /// Record one processed transaction: its events, then the cursor, so a
    /// restart resumes after the last fully stored transaction.
    fn commit(
        &mut self,
        records: Vec<AuctionRecord>,
        signature: &str,
    ) -> Result<(), Box<dyn Error>> {
        if !records.is_empty() {
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.log_path())?;
            for record in &records {
                writeln!(log, "{}", serde_json::to_string(record)?)?;
            }
            log.sync_data()?;
        }
        for record in records {
            self.insert(record);
        }
        fs::write(self.data_dir.join("cursor"), signature)?;
        self.cursor = Some(signature.to_owned());
        Ok(())
    }

    /// Newest-first history for one market.
    fn history(&self, market: &str, before: Option<u64>, limit: usize) -> Vec<AuctionRecord> {
        let upper = before.unwrap_or(u64::MAX);
        self.records
            .range((market.to_owned(), 0)..(market.to_owned(), upper))
            .rev()
            .take(limit)
            .map(|(_, record)| record.clone())
            .collect()
    }
}

struct Rpc {
    client: reqwest::blocking::Client,
    url: String,
}

impl Rpc {
    fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let response: serde_json::Value = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .body(
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
                    .to_string(),
            )
            .send()?
            .error_for_status()?
            .json_value()?;
        if let Some(error) = response.get("error") {
            return Err(format!("{method} failed: {error}").into());
        }
        Ok(response["result"].clone())
    }

    /// Successful signatures that referenced `address` after `until`, oldest first.
    fn new_signatures(
        &self,
        address: &Pubkey,
        until: Option<&str>,
    ) -> Result<Vec<String>, Box<dyn Error>> {
        let mut signatures = Vec::new();
        let mut before: Option<String> = None;
        loop {
            let mut options =
                serde_json::json!({"limit": SIGNATURE_PAGE, "commitment": "confirmed"});
            if let Some(until) = until {
                options["until"] = until.into();
            }
            if let Some(before) = &before {
                options["before"] = before.clone().into();
            }
            let page = self.call(
                "getSignaturesForAddress",
                serde_json::json!([address.to_string(), options]),
            )?;
            let page = page.as_array().ok_or("signature page is not an array")?;
            for entry in page {
                let signature = entry["signature"]
                    .as_str()
                    .ok_or("signature entry has no signature")?
                    .to_owned();
                if entry["err"].is_null() {
                    signatures.push(signature.clone());
                }
                before = Some(signature);
            }
            if page.len() < SIGNATURE_PAGE {
                break;
            }
        }
        signatures.reverse();
        Ok(signatures)
    }

    fn events(&self, signature: &str) -> Result<Vec<AuctionRecord>, Box<dyn Error>> {
        let transaction = self.call(
            "getTransaction",
            serde_json::json!([signature, {
                "encoding": "jsonParsed",
                "commitment": "confirmed",
                "maxSupportedTransactionVersion": 1,
            }]),
        )?;
        if transaction.is_null() {
            return Err(format!("transaction {signature} is not available yet").into());
        }
        let block_time = transaction["blockTime"].as_i64();
        let program = beatex::ID.to_string();
        let mut records = Vec::new();
        for group in transaction["meta"]["innerInstructions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            for instruction in group["instructions"].as_array().into_iter().flatten() {
                if instruction["programId"].as_str() != Some(program.as_str()) {
                    continue;
                }
                let Some(data) = instruction["data"].as_str() else {
                    continue;
                };
                let data = bs58::decode(data).into_vec()?;
                if let Some(record) = decode_event(&data, signature, block_time)? {
                    records.push(record);
                }
            }
        }
        Ok(records)
    }
}

trait JsonValue {
    fn json_value(self) -> Result<serde_json::Value, Box<dyn Error>>;
}

impl JsonValue for reqwest::blocking::Response {
    fn json_value(self) -> Result<serde_json::Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(&self.bytes()?)?)
    }
}

fn event_authority() -> Pubkey {
    Pubkey::find_program_address(&[b"__event_authority"], &beatex::ID).0
}

fn poll_once(rpc: &Rpc, store: &RwLock<Store>) -> Result<usize, Box<dyn Error>> {
    let cursor = store
        .read()
        .map_err(|_| "store lock poisoned")?
        .cursor
        .clone();
    let signatures = rpc.new_signatures(&event_authority(), cursor.as_deref())?;
    let mut indexed = 0;
    for signature in signatures {
        let records = rpc.events(&signature)?;
        indexed += records.len();
        store
            .write()
            .map_err(|_| "store lock poisoned")?
            .commit(records, &signature)?;
    }
    Ok(indexed)
}

fn run_poller(config: &Config, store: Arc<RwLock<Store>>) {
    let rpc = Rpc {
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP client configuration is static"),
        url: config.rpc_url.clone(),
    };
    let interval = config.poll_interval;
    thread::spawn(move || loop {
        match poll_once(&rpc, &store) {
            Ok(0) => {}
            Ok(count) => println!("indexed {count} auction event(s)"),
            Err(error) => eprintln!("indexer poll failed: {error}"),
        }
        thread::sleep(interval);
    });
}

#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
    before: Option<u64>,
}

async fn history(
    State(store): State<Arc<RwLock<Store>>>,
    Path(market): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<AuctionRecord>>, StatusCode> {
    let limit = query.limit.unwrap_or(60).clamp(1, MAX_HISTORY_RESPONSE);
    let store = store
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(store.history(&market, query.before, limit)))
}

async fn health(
    State(store): State<Arc<RwLock<Store>>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = store
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "records": store.records.len(),
        "cursor": store.cursor,
    })))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = parse_config()?;
    let store = Arc::new(RwLock::new(Store::open(config.data_dir.clone())?));
    println!(
        "loaded {} auction record(s); following event authority {}",
        store
            .read()
            .map_err(|_| "store lock poisoned")?
            .records
            .len(),
        event_authority()
    );
    run_poller(&config, Arc::clone(&store));
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/markets/{market}/auctions", get(history))
        .with_state(store)
        .layer(SetResponseHeaderLayer::overriding(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        ));
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    println!("serving auction history on http://{}", config.listen);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use {super::*, anchor_lang::AnchorSerialize};

    fn event_bytes<T: AnchorSerialize + Discriminator>(event: &T) -> Vec<u8> {
        let mut data = EVENT_IX_TAG_LE.to_vec();
        data.extend_from_slice(T::DISCRIMINATOR);
        event.serialize(&mut data).unwrap();
        data
    }

    #[test]
    fn decodes_every_auction_event() {
        let market = Pubkey::new_unique();
        let cleared = decode_event(
            &event_bytes(&AuctionCleared {
                market,
                auction_id: 7,
                reference_price: 100,
                reference_from_oracle: false,
                grid_step_bps: 10,
                grid_size: 17,
                clearing_index: 8,
                clearing_price: 101,
                clearing_volume: 5,
                accepted_order_count: 2,
                demand: [3; 17],
                supply: [4; 17],
                slot: 9,
            }),
            "sig",
            Some(1),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cleared.outcome, Outcome::Settled);
        assert_eq!(cleared.clearing_price.as_deref(), Some("101"));
        assert_eq!(cleared.market, market.to_string());
        assert_eq!(cleared.demand.as_ref().map(Vec::len), Some(17));
        assert_eq!(cleared.supply.as_ref().unwrap()[0], "4");

        let cancelled = decode_event(
            &event_bytes(&AuctionCancelled {
                market,
                auction_id: 8,
                reference_price: 100,
                reference_from_oracle: true,
                accepted_order_count: 3,
                slot: 10,
            }),
            "sig",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(cancelled.outcome, Outcome::Cancelled);
        assert_eq!(cancelled.clearing_price, None);

        let empty = decode_event(
            &event_bytes(&EmptyAuctionClosed {
                market,
                auction_id: 9,
                reference_price: 100,
                reference_from_oracle: true,
                slot: 11,
            }),
            "sig",
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(empty.outcome, Outcome::Empty);

        // Ordinary instruction data and unknown events are ignored.
        assert!(decode_event(&[1; 40], "sig", None).unwrap().is_none());
        let mut unknown = EVENT_IX_TAG_LE.to_vec();
        unknown.extend_from_slice(&[0; 16]);
        assert!(decode_event(&unknown, "sig", None).unwrap().is_none());
    }

    #[test]
    fn store_persists_and_serves_newest_first() {
        let dir = env::temp_dir().join(format!("beatex-indexer-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let market = Pubkey::new_unique().to_string();
        let record = |auction_id: u64| AuctionRecord {
            market: market.clone(),
            auction_id: auction_id.to_string(),
            outcome: Outcome::Settled,
            reference_price: "100".to_owned(),
            reference_from_oracle: false,
            grid_step_bps: Some(10),
            grid_size: Some(17),
            clearing_index: Some(8),
            clearing_price: Some("101".to_owned()),
            clearing_volume: Some("1".to_owned()),
            accepted_order_count: Some("2".to_owned()),
            demand: None,
            supply: None,
            slot: auction_id.to_string(),
            signature: format!("sig-{auction_id}"),
            block_time: None,
        };
        {
            let mut store = Store::open(dir.clone()).unwrap();
            store.commit(vec![record(1), record(2)], "sig-2").unwrap();
            store.commit(Vec::new(), "sig-3").unwrap();
            store.commit(vec![record(10)], "sig-10").unwrap();
        }
        let store = Store::open(dir.clone()).unwrap();
        assert_eq!(store.cursor.as_deref(), Some("sig-10"));
        let ids: Vec<_> = store
            .history(&market, None, 60)
            .iter()
            .map(AuctionRecord::auction_number)
            .collect();
        assert_eq!(ids, [10, 2, 1]);
        let before: Vec<_> = store
            .history(&market, Some(10), 1)
            .iter()
            .map(AuctionRecord::auction_number)
            .collect();
        assert_eq!(before, [2]);
        assert!(store.history("other", None, 60).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}

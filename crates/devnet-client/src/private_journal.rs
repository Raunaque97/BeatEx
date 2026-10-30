//! Persist proof openings before a signed transaction can leave this process.
//! Recovery uses finalized chain state and the actual block-height expiry.
use super::*;

thread_local! {
    static SIGNING_JOURNAL: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Deserialize, Serialize)]
struct PendingTransition {
    market: Pubkey,
    owner: Pubkey,
    before: TraderPrivateState,
    after: TraderPrivateState,
    encrypted_base: Vec<u8>,
    encrypted_quote: Vec<u8>,
    signed: Option<SignedTransaction>,
}

#[derive(Deserialize, Serialize)]
struct SignedTransaction {
    signature: String,
    last_valid_block_height: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NativeRecovery {
    Clean,
    Committed,
    Discarded,
    Waiting,
}

struct SigningGuard;

impl Drop for SigningGuard {
    fn drop(&mut self) {
        SIGNING_JOURNAL.with(|path| *path.borrow_mut() = None);
    }
}

fn journal_path(state_dir: &Path, owner: &Pubkey) -> PathBuf {
    trader_state_path(state_dir, owner).with_extension("transaction.json")
}

pub(super) fn atomic_private_write<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), Box<dyn Error>> {
    let parent = path.parent().ok_or("private state path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    restrict_permissions(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn remove_journal(path: &Path) -> Result<(), Box<dyn Error>> {
    fs::remove_file(path)?;
    sync_directory(path.parent().ok_or("journal has no parent")?)
}

pub(super) fn lock_state(state_dir: &Path, owner: &Pubkey) -> Result<fs::File, Box<dyn Error>> {
    let path = trader_state_path(state_dir, owner).with_extension("lock");
    fs::create_dir_all(path.parent().ok_or("lock has no parent")?)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    restrict_permissions(&path)?;
    file.try_lock()
        .map_err(|_| "another process is using this maker's private state")?;
    Ok(file)
}

pub(super) fn send_private_transition(
    wallet: &Path,
    state_dir: &Path,
    owner: Pubkey,
    state: &mut TraderPrivateState,
    next: TraderPrivateState,
    ciphertexts: [[u8; 64]; 2],
    send: impl FnOnce() -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    // Interactive browser sessions use a separate SDK recovery protocol.
    // Only the native signer can durably record its signature before broadcast.
    if wallet_file_is_browser_session(wallet)? {
        send()?;
        write_trader_state(state_dir, &owner, &next)?;
        *state = next;
        return Ok(());
    }
    if next.revision != state.revision.checked_add(1).ok_or("revision overflow")? {
        return Err("private transition must advance exactly one revision".into());
    }
    let path = journal_path(state_dir, &owner);
    if path.exists() {
        return Err(
            "pending private transaction must be reconciled before another submission".into(),
        );
    }
    let market = fs::read_to_string(state_dir.join("market.tsv"))?
        .lines()
        .find_map(|line| line.strip_prefix("market\t"))
        .ok_or("market manifest is missing its market")?
        .parse()?;
    let pending = PendingTransition {
        market,
        owner,
        before: state.clone(),
        after: next.clone(),
        encrypted_base: ciphertexts[0].to_vec(),
        encrypted_quote: ciphertexts[1].to_vec(),
        signed: None,
    };
    atomic_private_write(&path, &pending)?;
    SIGNING_JOURNAL.with(|slot| *slot.borrow_mut() = Some(path.clone()));
    let _guard = SigningGuard;
    // Even a deterministic failure leaves the journal for verified rollback.
    // An error alone cannot prove that no transaction reached a leader.
    send()?;
    write_trader_state(state_dir, &owner, &next)?;
    *state = next;
    remove_journal(&path)
}

pub(super) fn record_signed_transaction(
    signature: String,
    last_valid_block_height: u64,
) -> Result<(), Box<dyn Error>> {
    SIGNING_JOURNAL.with(|slot| {
        if let Some(path) = slot.borrow().as_ref() {
            let mut pending: PendingTransition = serde_json::from_slice(&fs::read(path)?)?;
            pending.signed = Some(SignedTransaction {
                signature,
                last_valid_block_height,
            });
            atomic_private_write(path, &pending)?;
        }
        Ok(())
    })
}

pub fn has_pending_native_transaction(state_dir: &Path, wallet: &Path) -> bool {
    wallet_pubkey(wallet).is_ok_and(|owner| journal_path(state_dir, &owner).exists())
}

fn recovery_decision(
    pending: &PendingTransition,
    balance: Option<&beatex::EncryptedPrivateBalance>,
    status: Option<&serde_json::Value>,
    block_height: u64,
) -> Result<NativeRecovery, Box<dyn Error>> {
    let balance = balance.ok_or("private balance account disappeared during recovery")?;
    if balance.market != pending.market || balance.owner != pending.owner {
        return Err("journal identity differs from the on-chain private balance".into());
    }
    let landed = status.is_some_and(|value| value["confirmationStatus"] == "finalized");
    let failed = landed && status.is_some_and(|value| !value["err"].is_null());
    let after_matches = balance.revision == pending.after.revision
        && balance.encrypted_base.as_slice() == pending.encrypted_base
        && balance.encrypted_quote.as_slice() == pending.encrypted_quote;
    if landed && !failed {
        return if after_matches {
            Ok(NativeRecovery::Committed)
        } else {
            Err("finalized transaction does not match the journal's balance and ciphertexts; trading remains paused".into())
        };
    }
    let expired = pending
        .signed
        .as_ref()
        .is_none_or(|signed| block_height > signed.last_valid_block_height);
    if failed || (status.is_none() && expired) {
        return if balance.revision == pending.before.revision {
            Ok(NativeRecovery::Discarded)
        } else {
            Err("cannot discard private transaction: on-chain revision changed; trading remains paused".into())
        };
    }
    Ok(NativeRecovery::Waiting)
}

pub(super) fn recover(
    state_dir: &Path,
    owner: &Pubkey,
    rpc_url: &str,
) -> Result<NativeRecovery, Box<dyn Error>> {
    let path = journal_path(state_dir, owner);
    if !path.exists() {
        return Ok(NativeRecovery::Clean);
    }
    let pending: PendingTransition = serde_json::from_slice(&fs::read(&path)?)?;
    if pending.owner != *owner
        || pending.after.revision
            != pending
                .before
                .revision
                .checked_add(1)
                .ok_or("revision overflow")?
    {
        return Err("invalid private transaction journal".into());
    }
    if let Some(local) = read_trader_state(state_dir, owner)? {
        if local != pending.before && local != pending.after {
            return Err(
                "local private state differs from both journal snapshots; trading remains paused"
                    .into(),
            );
        }
    } else if pending.before.revision != 0 {
        return Err("local private state disappeared; trading remains paused".into());
    }
    let mut status = None;
    let mut block_height = 0;
    let mut min_context_slot = 0;
    if let Some(signed) = &pending.signed {
        let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getSignatureStatuses", "params":[[signed.signature], {"searchTransactionHistory":true}]}).to_string();
        let response: serde_json::Value =
            serde_json::from_slice(&rpc_request(rpc_url, &request, "5")?)?;
        status = response
            .pointer("/result/value/0")
            .filter(|value| !value.is_null())
            .cloned();
        if status.is_none() {
            // Epoch info binds block height and slot to one finalized snapshot.
            // Subsequent account reads must be at least this fresh, even on failover.
            let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getEpochInfo","params":[{"commitment":"finalized"}]}).to_string();
            let response: serde_json::Value =
                serde_json::from_slice(&rpc_request(rpc_url, &request, "5")?)?;
            block_height = response["result"]["blockHeight"]
                .as_u64()
                .ok_or("RPC block height is missing")?;
            min_context_slot = response["result"]["absoluteSlot"]
                .as_u64()
                .ok_or("RPC finalized slot is missing")?;
        } else if let Some(status) = &status {
            if status["confirmationStatus"] != "finalized" {
                return Ok(NativeRecovery::Waiting);
            }
            min_context_slot = status["slot"]
                .as_u64()
                .ok_or("RPC finalized transaction slot is missing")?;
        }
    }
    let balance_address = private_balance_address(&pending.market, owner);
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getAccountInfo","params":[balance_address.to_string(), {"encoding":"base64","commitment":"finalized","minContextSlot":min_context_slot}]}).to_string();
    let response: RpcResponse = serde_json::from_slice(&rpc_request(rpc_url, &request, "5")?)?;
    let balance = response
        .result
        .value
        .map(|account| {
            if account.data.1 != "base64" {
                return Err("unexpected account encoding".into());
            }
            let bytes = BASE64.decode(account.data.0)?;
            Ok(beatex::EncryptedPrivateBalance::try_deserialize(
                &mut bytes.as_slice(),
            )?)
        })
        .transpose()
        .map_err(|error: Box<dyn Error>| error)?;
    let outcome = recovery_decision(&pending, balance.as_ref(), status.as_ref(), block_height)?;
    match outcome {
        NativeRecovery::Committed => {
            write_trader_state(state_dir, owner, &pending.after)?;
            remove_journal(&path)?;
        }
        NativeRecovery::Discarded => {
            write_trader_state(state_dir, owner, &pending.before)?;
            remove_journal(&path)?;
        }
        _ => {}
    }
    Ok(outcome)
}

pub fn recover_native_private_state(
    state_dir: &Path,
    wallet: &Path,
    rpc_url: &str,
) -> Result<NativeRecovery, String> {
    (|| {
        let owner = wallet_pubkey(wallet)?;
        let _lock = lock_state(state_dir, &owner)?;
        recover(state_dir, &owner, rpc_url)
    })()
    .map_err(|error: Box<dyn Error>| error.to_string())
}

pub fn acquire_native_maker_lock(state_dir: &Path, wallet: &Path) -> Result<fs::File, String> {
    (|| {
        let owner = wallet_pubkey(wallet)?;
        let path = trader_state_path(state_dir, &owner).with_extension("maker.lock");
        fs::create_dir_all(path.parent().ok_or("maker lock has no parent")?)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        restrict_permissions(&path)?;
        file.try_lock()
            .map_err(|_| "another market maker is already running with this wallet")?;
        Ok(file)
    })()
    .map_err(|error: Box<dyn Error>| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::AccountSerialize;
    use std::net::TcpListener;

    fn state(revision: u64) -> TraderPrivateState {
        TraderPrivateState {
            revision,
            balance: StoredBalance {
                base_value: 10,
                quote_value: 20,
                base_randomness: [3; 32],
                quote_randomness: [4; 32],
            },
            active_orders: vec![],
            active_order: None,
        }
    }

    fn pending() -> PendingTransition {
        PendingTransition {
            market: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            before: state(7),
            after: state(8),
            encrypted_base: vec![5; 64],
            encrypted_quote: vec![6; 64],
            signed: Some(SignedTransaction {
                signature: "signature".into(),
                last_valid_block_height: 100,
            }),
        }
    }

    fn balance(pending: &PendingTransition, revision: u64) -> beatex::EncryptedPrivateBalance {
        beatex::EncryptedPrivateBalance {
            version: 1,
            market: pending.market,
            owner: pending.owner,
            key_epoch: Pubkey::new_unique(),
            encrypted_base: [5; 64],
            encrypted_quote: [6; 64],
            deposited_base: 0,
            deposited_quote: 0,
            balance_digest: [0; 32],
            revision,
            bump: 0,
        }
    }

    #[test]
    fn only_finalized_matching_ciphertexts_can_commit() {
        let pending = pending();
        let mut chain = balance(&pending, 8);
        let confirmed = serde_json::json!({"confirmationStatus":"confirmed","err":null});
        assert_eq!(
            recovery_decision(&pending, Some(&chain), Some(&confirmed), 200).unwrap(),
            NativeRecovery::Waiting
        );
        let finalized = serde_json::json!({"confirmationStatus":"finalized","err":null});
        assert_eq!(
            recovery_decision(&pending, Some(&chain), Some(&finalized), 200).unwrap(),
            NativeRecovery::Committed
        );
        chain.encrypted_base[0] ^= 1;
        assert!(recovery_decision(&pending, Some(&chain), Some(&finalized), 200).is_err());
    }

    #[test]
    fn rollback_requires_expiry_or_finalized_failure_and_unchanged_revision() {
        let pending = pending();
        let chain = balance(&pending, 7);
        assert_eq!(
            recovery_decision(&pending, Some(&chain), None, 100).unwrap(),
            NativeRecovery::Waiting
        );
        assert_eq!(
            recovery_decision(&pending, Some(&chain), None, 101).unwrap(),
            NativeRecovery::Discarded
        );
        let failed = serde_json::json!({"confirmationStatus":"finalized","err":{"InstructionError":[0,"failure"]}});
        assert_eq!(
            recovery_decision(&pending, Some(&chain), Some(&failed), 0).unwrap(),
            NativeRecovery::Discarded
        );
        let changed = balance(&pending, 8);
        assert!(recovery_decision(&pending, Some(&changed), None, 101).is_err());
    }

    fn serve_recovery(pending: &PendingTransition) -> (String, thread::JoinHandle<()>) {
        serve_snapshot(
            pending,
            pending.after.revision,
            Some(serde_json::json!({"confirmationStatus":"finalized","err":null,"slot":55})),
            200,
        )
    }

    fn serve_snapshot(
        pending: &PendingTransition,
        revision: u64,
        status: Option<serde_json::Value>,
        height: u64,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut bytes = vec![];
        balance(pending, revision)
            .try_serialize(&mut bytes)
            .unwrap();
        let data = BASE64.encode(bytes);
        let handle = thread::spawn(move || {
            for _ in 0..if status.is_some() { 2 } else { 3 } {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let body_start = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..body_start]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                while bytes.len() < body_start + length {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "getSignatureStatuses" => serde_json::json!({"value":[status]}),
                    "getEpochInfo" => serde_json::json!({"blockHeight":height,"absoluteSlot":55}),
                    "getAccountInfo" => {
                        assert_eq!(request["params"][1]["minContextSlot"], 55);
                        serde_json::json!({"value":{"data":[data,"base64"]}})
                    }
                    method => panic!("unexpected recovery RPC {method}"),
                };
                let body = serde_json::json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        (url, handle)
    }

    #[test]
    fn lost_confirmation_survives_restart_and_commits_exact_saved_openings() {
        let dir = env::temp_dir().join(format!("beatex-recovery-{}", Pubkey::new_unique()));
        fs::create_dir_all(&dir).unwrap();
        let wallet = dir.join("wallet.json");
        let keypair = Keypair::new();
        write_keypair_file(&keypair, &wallet).unwrap();
        let mut pending = pending();
        pending.owner = keypair.pubkey();
        fs::write(
            dir.join("market.tsv"),
            format!("name\taddress\nmarket\t{}\n", pending.market),
        )
        .unwrap();
        write_trader_state(&dir, &pending.owner, &pending.before).unwrap();
        let mut local = pending.before.clone();
        let result = send_private_transition(
            &wallet,
            &dir,
            pending.owner,
            &mut local,
            pending.after.clone(),
            [[5; 64], [6; 64]],
            || {
                // This runs before broadcast: the proof openings already exist on disk.
                let saved: PendingTransition =
                    serde_json::from_slice(&fs::read(journal_path(&dir, &pending.owner))?).unwrap();
                assert_eq!(saved.after, pending.after);
                record_signed_transaction("signature".into(), 100)?;
                Err("transaction confirmation outcome unknown".into())
            },
        );
        assert!(result.is_err());
        assert_eq!(local, pending.before);
        assert!(journal_path(&dir, &pending.owner).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(journal_path(&dir, &pending.owner))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(recover_native_private_state(&dir, &wallet, "http://127.0.0.1:1").is_err());
        assert!(journal_path(&dir, &pending.owner).exists());
        let (url, server) = serve_recovery(&pending);
        assert_eq!(
            recover_native_private_state(&dir, &wallet, &url).unwrap(),
            NativeRecovery::Committed
        );
        server.join().unwrap();
        assert_eq!(
            read_trader_state(&dir, &pending.owner).unwrap().unwrap(),
            pending.after
        );
        assert!(!journal_path(&dir, &pending.owner).exists());
        assert_eq!(
            recover_native_private_state(&dir, &wallet, &url).unwrap(),
            NativeRecovery::Clean
        );
        // Simulate a crash after committing the state, but before removing the journal.
        atomic_private_write(&journal_path(&dir, &pending.owner), &pending).unwrap();
        let (url, server) = serve_recovery(&pending);
        assert_eq!(
            recover_native_private_state(&dir, &wallet, &url).unwrap(),
            NativeRecovery::Committed
        );
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_transaction_blocks_replacement_until_actual_block_height_expiry() {
        let dir = env::temp_dir().join(format!("beatex-expiry-{}", Pubkey::new_unique()));
        fs::create_dir_all(&dir).unwrap();
        let wallet = dir.join("wallet.json");
        let keypair = Keypair::new();
        write_keypair_file(&keypair, &wallet).unwrap();
        let mut pending = pending();
        pending.owner = keypair.pubkey();
        fs::write(
            dir.join("market.tsv"),
            format!("market\t{}\n", pending.market),
        )
        .unwrap();
        write_trader_state(&dir, &pending.owner, &pending.before).unwrap();
        atomic_private_write(&journal_path(&dir, &pending.owner), &pending).unwrap();
        let mut local = pending.before.clone();
        assert!(send_private_transition(
            &wallet,
            &dir,
            pending.owner,
            &mut local,
            pending.after.clone(),
            [[5; 64], [6; 64]],
            || panic!("must not broadcast over an unresolved transaction")
        )
        .is_err());
        let (url, server) = serve_snapshot(&pending, 7, None, 100);
        assert_eq!(
            recover_native_private_state(&dir, &wallet, &url).unwrap(),
            NativeRecovery::Waiting
        );
        server.join().unwrap();
        assert!(journal_path(&dir, &pending.owner).exists());
        let (url, server) = serve_snapshot(&pending, 7, None, 101);
        assert_eq!(
            recover_native_private_state(&dir, &wallet, &url).unwrap(),
            NativeRecovery::Discarded
        );
        server.join().unwrap();
        assert_eq!(
            read_trader_state(&dir, &pending.owner).unwrap().unwrap(),
            pending.before
        );
        assert!(!journal_path(&dir, &pending.owner).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_maker_lock_is_rejected_and_released_on_exit() {
        let dir = env::temp_dir().join(format!("beatex-lock-{}", Pubkey::new_unique()));
        fs::create_dir_all(&dir).unwrap();
        let wallet = dir.join("wallet.json");
        write_keypair_file(&Keypair::new(), &wallet).unwrap();
        let lock = acquire_native_maker_lock(&dir, &wallet).unwrap();
        assert!(acquire_native_maker_lock(&dir, &wallet).is_err());
        drop(lock);
        assert!(acquire_native_maker_lock(&dir, &wallet).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
}

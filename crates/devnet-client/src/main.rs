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
        rollover::{MarketMakerRolloverProver, RolloverProofRequest, RolloverProver},
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
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::{read_keypair_file, write_keypair_file, Keypair},
    solana_signer::Signer,
    solana_zk_sdk::encryption::{derivation::derive_confidential_keys, elgamal::ElGamalCiphertext},
    spl_token_2022_interface::{
        extension::{
            confidential_transfer::ConfidentialTransferAccount, BaseStateWithExtensions,
            StateWithExtensions,
        },
        state::Account as TokenAccount,
    },
    std::{
        env,
        error::Error,
        fs::{self, OpenOptions},
        io::{Read, Write},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        str::FromStr,
    },
};

const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
const PYTH_BTC_USD_ACCOUNT: &str = "4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo";
const PYTH_BTC_USD_FEED: &str = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";
const MARKET_NONCE: u64 = 3;
const KEY_EPOCH: u64 = 1;
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
struct ProgramAccountsResponse {
    result: Vec<KeyedRpcAccount>,
}

#[derive(Deserialize)]
struct KeyedRpcAccount {
    pubkey: String,
    account: RpcAccount,
}

#[derive(Clone, Debug, Deserialize)]
struct OrderIntent {
    reference_price: u64,
    grid_step_bps: u16,
    client_order_nonce: u64,
    buy: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
    sell: [u64; beatex_babyjubjub_groth16_spike::LEVELS],
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
    active_order: Option<StoredOrder>,
}

#[derive(Debug)]
struct OrderConfig {
    common: CommonConfig,
    wallet: PathBuf,
    base_source: Pubkey,
    quote_source: Pubkey,
    rpc_url: String,
    deposit_base: u64,
    deposit_quote: u64,
}

#[derive(Debug)]
struct FoldConfig {
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
}

#[derive(Debug)]
struct PublishConfig {
    common: CommonConfig,
    wallet: PathBuf,
    rpc_url: String,
    auction_id: u64,
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
        "Usage:\n  beatex-devnet-client addresses --base-mint PUBKEY --quote-mint PUBKEY --state-dir DIR\n\n  beatex-devnet-client initialize --base-mint PUBKEY --quote-mint PUBKEY --base-vault PUBKEY --quote-vault PUBKEY --wallet KEYPAIR --vault-owner KEYPAIR --state-dir DIR --market-start-slot SLOT [--rpc-url URL]\n\n  beatex-devnet-client order --base-mint PUBKEY --quote-mint PUBKEY --base-source PUBKEY --quote-source PUBKEY --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client fold --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client publish --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client cancel --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client settle-balance|refund-balance --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet TRADER_KEYPAIR --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client withdraw --base-mint PUBKEY --quote-mint PUBKEY --wallet TRADER_KEYPAIR --base-destination PUBKEY --quote-destination PUBKEY --base-amount N --quote-amount N --state-dir DIR [--rpc-url URL]\n\n  beatex-devnet-client claim|refund --base-mint PUBKEY --quote-mint PUBKEY --auction-id N --wallet FEE_PAYER_KEYPAIR --owner TRADER_KEYPAIR --vault-owner KEYPAIR --base-destination PUBKEY --quote-destination PUBKEY --state-dir DIR [--rpc-url URL]\n\nThe default fast path keeps custody in the vault's public Token-2022 balance while all trading balances remain encrypted. settle-balance and refund-balance credit the internal ledger; withdraw debits it and performs one public token withdrawal. Legacy claim and refund retain the slower confidential Token-2022 exit. The client refuses mainnet URLs."
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
    Command::new("solana")
        .args([
            "--url",
            rpc_url,
            "account",
            &address.to_string(),
            "--output",
            "json-compact",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn fetch_anchor_account<T: AccountDeserialize>(
    rpc_url: &str,
    address: &Pubkey,
) -> Result<Option<T>, Box<dyn Error>> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [address.to_string(), {"encoding": "base64", "commitment": "confirmed"}],
    })
    .to_string();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("RPC request failed with {}", output.status).into());
    }
    let response: RpcResponse = serde_json::from_slice(&output.stdout)?;
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
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("RPC request failed with {}", output.status).into());
    }
    let response: RpcResponse = serde_json::from_slice(&output.stdout)?;
    let account = response.result.value.ok_or("account does not exist")?;
    if account.data.1 != "base64" {
        return Err(format!("unexpected account encoding {}", account.data.1).into());
    }
    Ok(BASE64.decode(account.data.0)?)
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
        "params": [space, {"commitment": "confirmed"}],
    })
    .to_string();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("RPC rent request failed with {}", output.status).into());
    }
    Ok(serde_json::from_slice::<RentResponse>(&output.stdout)?.result)
}

fn fetch_accepted_orders(
    rpc_url: &str,
    auction: &Pubkey,
) -> Result<Vec<(Pubkey, QueuedOrder)>, Box<dyn Error>> {
    // QueuedOrder layout: discriminator (8), version (1), market (32), auction.
    const AUCTION_OFFSET: usize = 8 + 1 + 32;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getProgramAccounts",
        "params": [
            beatex::ID.to_string(),
            {
                "encoding": "base64",
                "commitment": "confirmed",
                "filters": [
                    {"dataSize": 8 + QueuedOrder::INIT_SPACE},
                    {"memcmp": {"offset": AUCTION_OFFSET, "bytes": auction.to_string()}}
                ]
            }
        ]
    })
    .to_string();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("RPC request failed with {}", output.status).into());
    }
    let response: ProgramAccountsResponse = serde_json::from_slice(&output.stdout)?;
    let mut orders = Vec::new();
    for keyed in response.result {
        if keyed.account.data.1 != "base64" {
            return Err(format!("unexpected account encoding {}", keyed.account.data.1).into());
        }
        let bytes = BASE64.decode(keyed.account.data.0)?;
        let order = QueuedOrder::try_deserialize(&mut bytes.as_slice())?;
        if order.state == QueuedOrderState::Accepted {
            orders.push((Pubkey::from_str(&keyed.pubkey)?, order));
        }
    }
    orders.sort_by_key(|(address, _)| *address);
    Ok(orders)
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
    send_bundle(
        wallet,
        rpc_url,
        label,
        vec![BundleInstruction {
            program_id: beatex::ID.to_string(),
            accounts,
            data_base64: BASE64.encode(data),
        }],
        additional_signers,
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
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sender = project_root.join("clients/v1-transaction/send.mjs");
    let bundle = InstructionBundle {
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
        priority_fee_lamports: 0,
    };
    eprintln!("sending {label}");
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
    let oracle = Pubkey::from_str(PYTH_BTC_USD_ACCOUNT)?;
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
        oracle_max_age_seconds: 600,
        oracle_max_confidence_bps: 200,
        oracle_exponent_adjustment: 0,
        min_reference_price: 1_000,
        max_reference_price: 1_000_000,
        submission_duration_slots: 400,
        aggregation_duration_slots: 200,
        decryption_duration_slots: 200,
        allocation_rule_version: allocation::ALLOCATION_RULE_VERSION,
        order_circuit_version: queue::ORDER_SUBMISSION_CIRCUIT_VERSION,
        claim_circuit_version: queue::PRIVATE_CLAIM_CIRCUIT_VERSION,
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
) -> Result<SolanaVerificationKey, Box<dyn Error>> {
    eprintln!("generating {:?} verification key", kind);
    match kind {
        QueueCircuitKind::OrderSubmission => {
            OrderProver::deterministic_devnet()?.verification_key()
        }
        QueueCircuitKind::AggregateFold => transition::deterministic_fold_verification_key(),
        QueueCircuitKind::BalanceDeposit => deposit::deterministic_verification_key(),
        QueueCircuitKind::AggregatePublication => {
            AggregatePublicationProver::deterministic_devnet()?.verification_key()
        }
        QueueCircuitKind::PrivateClaim => {
            deterministic_private_withdrawal_verification_key(PrivateWithdrawalKind::Claim)
        }
        QueueCircuitKind::CancellationRefund => {
            deterministic_private_withdrawal_verification_key(PrivateWithdrawalKind::Refund)
        }
        QueueCircuitKind::OrderModification => {
            CollateralReleaseProver::deterministic_devnet()?.verification_key()
        }
        QueueCircuitKind::RolloverSettlement => {
            RolloverProver::deterministic_devnet()?.verification_key()
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
        let version = queue::circuit_version(kind);
        let address = verification_key_address(&addresses.market, kind, version);
        if account_exists(&config.rpc_url, &address) {
            eprintln!("{:?} key {} already exists", kind, address);
            continue;
        }
        let key = generate_verification_key(kind)?;
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
    #[derive(Deserialize)]
    struct SlotResponse {
        result: u64,
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot",
        "params": [{"commitment": "confirmed"}],
    })
    .to_string();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--header",
            "Content-Type: application/json",
            "--data",
            &request,
            rpc_url,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!("RPC request failed with {}", output.status).into());
    }
    Ok(serde_json::from_slice::<SlotResponse>(&output.stdout)?.result)
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

fn initialize_current_auction(
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
    let Some(current) = current_auction_addresses(&rpc_url, &addresses, &market)? else {
        let slot = current_slot(&rpc_url)?;
        println!(
            "market {} starts at slot {}; current slot is {}",
            addresses.market, market.market_start_slot, slot
        );
        return Ok(());
    };
    if !account_exists(&rpc_url, &current.auction) {
        let payer = wallet_pubkey(&wallet)?;
        send(
            &wallet,
            &rpc_url,
            &format!("initialize auction {}", current.auction_id),
            vec![
                meta(payer, true, true),
                meta(addresses.market, false, false),
                meta(addresses.key_epoch, false, false),
                meta(market.oracle_price_account, false, false),
                meta(current.auction, true, false),
                meta(current.submission_counter, true, false),
                meta(current.accumulator, true, false),
                meta(anchor_lang::system_program::ID, false, false),
            ],
            beatex::instruction::InitializeAuction {
                auction_id: current.auction_id,
            }
            .data(),
            vec![],
        )?;
    }
    let path = common.state_dir.join("current-auction.tsv");
    fs::write(
        path,
        format!(
            "name\tvalue\nauction-id\t{}\nauction\t{}\nsubmission-counter\t{}\naccumulator\t{}\n",
            current.auction_id, current.auction, current.submission_counter, current.accumulator,
        ),
    )?;
    println!("auction {}: {}", current.auction_id, current.auction);
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
    state_dir.join("traders").join(format!("{owner}.json"))
}

fn read_trader_state(
    state_dir: &Path,
    owner: &Pubkey,
) -> Result<Option<TraderPrivateState>, Box<dyn Error>> {
    let path = trader_state_path(state_dir, owner);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

fn write_trader_state(
    state_dir: &Path,
    owner: &Pubkey,
    state: &TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let directory = state_dir.join("traders");
    fs::create_dir_all(&directory)?;
    let path = trader_state_path(state_dir, owner);
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

fn deposit_initial_balance(
    config: &OrderConfig,
    addresses: &Addresses,
    market: &beatex::Market,
    key_epoch: &beatex::KeyEpoch,
    owner: Pubkey,
    private_balance: Pubkey,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    if state.revision != 0 {
        return Ok(());
    }
    let prover = DepositProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove(
        DepositProofRequest {
            market: addresses.market.to_bytes(),
            owner: owner.to_bytes(),
            key_epoch: addresses.key_epoch.to_bytes(),
            committee_key: key_epoch.babyjub_encryption_pubkey,
            old_revision: state.revision,
            base_amount: config.deposit_base,
            quote_amount: config.deposit_quote,
            old_balance: state.balance.opening(),
        },
        &mut rng,
    )?;
    let deposit_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::BalanceDeposit,
        queue::BALANCE_DEPOSIT_CIRCUIT_VERSION,
    );
    send(
        &config.wallet,
        &config.rpc_url,
        "deposit market-maker collateral",
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
                base_amount: config.deposit_base,
                quote_amount: config.deposit_quote,
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
    counter: &beatex::SubmissionCounter,
    intent: &OrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let prover = OrderProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove(
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
    )?;
    let (event, _) = Pubkey::find_program_address(
        &[
            b"queued-order",
            auction_addresses.auction.as_ref(),
            &counter.accepted_order_count.to_le_bytes(),
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
        queue::ORDER_SUBMISSION_CIRCUIT_VERSION,
    );
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
    state.active_order = Some(StoredOrder {
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
    write_trader_state(&config.common.state_dir, &owner, state)
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
    counter: &beatex::SubmissionCounter,
    intent: &OrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let previous = state
        .active_order
        .clone()
        .ok_or("missing private active-order state")?;
    if previous.auction_id != auction_addresses.auction_id {
        return Err(
            "the previous auction order must be settled or refunded before a new order".into(),
        );
    }
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
            &counter.accepted_order_count.to_le_bytes(),
        ],
        &beatex::ID,
    );
    let prover = ReplacementProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove(
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
    )?;
    let release_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderModification,
        queue::ORDER_MODIFICATION_CIRCUIT_VERSION,
    );
    let order_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderSubmission,
        queue::ORDER_SUBMISSION_CIRCUIT_VERSION,
    );
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
    state.active_order = Some(StoredOrder {
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
    write_trader_state(&config.common.state_dir, &owner, state)
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
    next_counter: &beatex::SubmissionCounter,
    intent: &OrderIntent,
    state: &mut TraderPrivateState,
) -> Result<(), Box<dyn Error>> {
    let previous = state
        .active_order
        .clone()
        .ok_or("missing private active-order state")?;
    let old_addresses = auction_addresses(addresses, previous.auction_id);
    if previous.auction != old_addresses.auction.to_string() {
        return Err("the local previous-auction address is inconsistent".into());
    }
    let old_auction =
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &old_addresses.auction)?
            .ok_or("previous auction account is missing")?;
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
    let old_order = fetch_anchor_account::<QueuedOrder>(&config.rpc_url, &old_order_address)?
        .ok_or("previous queued order is missing")?;
    if old_order.state != QueuedOrderState::Folded
        || old_order.withdrawal_state != WithdrawalState::Pending
    {
        return Err("previous queued order is not rollover-eligible".into());
    }
    let old_active_order = Pubkey::find_program_address(
        &[
            b"active-order",
            old_addresses.auction.as_ref(),
            owner.as_ref(),
        ],
        &beatex::ID,
    )
    .0;
    let old_active =
        fetch_anchor_account::<beatex::ActiveOrder>(&config.rpc_url, &old_active_order)?
            .ok_or("previous active-order account is missing")?;
    if !old_active.active || old_active.latest_event != old_order_address {
        return Err("previous queued order is not the active revision".into());
    }
    let next_order_address = Pubkey::find_program_address(
        &[
            b"queued-order",
            next_addresses.auction.as_ref(),
            &next_counter.accepted_order_count.to_le_bytes(),
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
    let clearing_index = usize::from(old_auction.clearing_index);
    let prover = MarketMakerRolloverProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove(
        RolloverProofRequest {
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
            demand: old_auction.decrypted_demand[clearing_index],
            supply: old_auction.decrypted_supply[clearing_index],
            clearing_volume: old_auction.clearing_volume,
            balance_revision: state.revision,
            committee_key: key_epoch.babyjub_encryption_pubkey,
            old_balance: state.balance.opening(),
        },
        OrderProofRequest {
            auction: next_addresses.auction.to_bytes(),
            owner: owner.to_bytes(),
            client_order_nonce: intent.client_order_nonce,
            reference_price: next_auction.reference_price,
            grid_step_bps: next_auction.grid_step_bps,
            committee_key: key_epoch.babyjub_encryption_pubkey,
            buy: intent.buy,
            sell: intent.sell,
            old_balance: state.balance.opening(),
        },
        &mut rng,
    )?;
    let rollover_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::RolloverSettlement,
        queue::ROLLOVER_SETTLEMENT_CIRCUIT_VERSION,
    );
    let order_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::OrderSubmission,
        queue::ORDER_SUBMISSION_CIRCUIT_VERSION,
    );
    let mut settlement_expected_input = output.settlement.public_input;
    settlement_expected_input.reverse();
    let mut order_expected_input = output.next_order.public_input;
    order_expected_input.reverse();
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
    state.active_order = Some(StoredOrder {
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
    println!(
        "rolled auction {} into {} with payouts {} base and {} quote units",
        previous.auction_id,
        next_addresses.auction_id,
        output.settlement.base_payout,
        output.settlement.quote_payout
    );
    Ok(())
}

fn execute_order(config: OrderConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let intent: OrderIntent = serde_json::from_slice(&input)?;
    let owner = wallet_pubkey(&config.wallet)?;
    let addresses = addresses(&config.common);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let key_epoch =
        fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &addresses.key_epoch)?
            .ok_or("key epoch has not been initialized")?;
    if intent.grid_step_bps != market.grid_step_bps {
        return Err(format!(
            "intent grid step {} bps does not match market grid step {} bps",
            intent.grid_step_bps, market.grid_step_bps
        )
        .into());
    }
    initialize_current_auction(
        config.common.clone(),
        config.wallet.clone(),
        config.rpc_url.clone(),
    )?;
    let auction_addresses = current_auction_addresses(&config.rpc_url, &addresses, &market)?
        .ok_or("market has not reached its start slot")?;
    let auction =
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("current auction has not been initialized")?;
    if intent.reference_price != auction.reference_price {
        return Err(format!(
            "intent reference {} does not match auction reference {}",
            intent.reference_price, auction.reference_price
        )
        .into());
    }
    let private_balance = private_balance_address(&addresses.market, &owner);
    let onchain_before =
        fetch_anchor_account::<beatex::EncryptedPrivateBalance>(&config.rpc_url, &private_balance)?;
    let mut state =
        read_trader_state(&config.common.state_dir, &owner)?.unwrap_or(TraderPrivateState {
            revision: 0,
            balance: StoredBalance {
                base_value: 0,
                quote_value: 0,
                base_randomness: [0; 32],
                quote_randomness: [0; 32],
            },
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
    initialize_private_balance(&config, &addresses, owner, private_balance)?;
    deposit_initial_balance(
        &config,
        &addresses,
        &market,
        &key_epoch,
        owner,
        private_balance,
        &mut state,
    )?;
    let counter = fetch_anchor_account::<beatex::SubmissionCounter>(
        &config.rpc_url,
        &auction_addresses.submission_counter,
    )?
    .ok_or("submission counter is missing")?;
    if let Some(previous) = &state.active_order {
        if previous.auction_id == auction_addresses.auction_id {
            replace_order(
                &config,
                &addresses,
                &auction_addresses,
                &auction,
                &key_epoch,
                owner,
                private_balance,
                &counter,
                &intent,
                &mut state,
            )
        } else {
            let old_addresses = crate::auction_addresses(&addresses, previous.auction_id);
            let old_auction =
                fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &old_addresses.auction)?
                    .ok_or("previous auction account is missing")?;
            if old_auction.state == beatex::AuctionState::Cancelled {
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
                    &counter,
                    &intent,
                    &mut state,
                )
            } else {
                rollover_order(
                    &config,
                    &addresses,
                    &auction_addresses,
                    &auction,
                    &key_epoch,
                    owner,
                    private_balance,
                    &counter,
                    &intent,
                    &mut state,
                )
            }
        }
    } else {
        submit_order(
            &config,
            &addresses,
            &auction_addresses,
            &auction,
            &key_epoch,
            owner,
            private_balance,
            &counter,
            &intent,
            &mut state,
        )
    }
}

fn execute_fold(config: FoldConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let aggregator = wallet_pubkey(&config.wallet)?;
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let auction =
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not been initialized")?;
    if auction.market != addresses.market {
        return Err("auction belongs to a different market".into());
    }
    let fold_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::AggregateFold,
        queue::AGGREGATE_FOLD_CIRCUIT_VERSION,
    );
    let prover = FoldProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let mut total_folded = 0usize;

    loop {
        let accepted = fetch_accepted_orders(&config.rpc_url, &auction_addresses.auction)?;
        if accepted.is_empty() {
            println!(
                "auction {} queue is drained; folded {} event(s)",
                config.auction_id, total_folded
            );
            return Ok(());
        }
        let batch = accepted
            .into_iter()
            .take(queue_hash::FOLD_BATCH_SIZE)
            .collect::<Vec<_>>();
        let accumulator = fetch_anchor_account::<AggregateAccumulator>(
            &config.rpc_url,
            &auction_addresses.accumulator,
        )?
        .ok_or("aggregate accumulator is missing")?;
        let old_aggregate = ciphertext_array(&accumulator.ciphertexts)?;
        let mut events = Vec::with_capacity(batch.len());
        for (event_address, event) in &batch {
            let previous_ciphertexts = if event.previous_event == Pubkey::default() {
                [identity_ciphertext(); queue::QUEUED_CIPHERTEXT_COUNT]
            } else {
                let previous =
                    fetch_anchor_account::<QueuedOrder>(&config.rpc_url, &event.previous_event)?
                        .ok_or("replacement event references a missing previous event")?;
                ciphertext_array(&previous.ciphertexts)?
            };
            events.push(FoldEventInput {
                event: event_address.to_bytes(),
                previous_ciphertexts,
                ciphertexts: ciphertext_array(&event.ciphertexts)?,
            });
        }
        let output = prover.prove(
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
            meta(auction_addresses.submission_counter, false, false),
            meta(auction_addresses.accumulator, true, false),
            meta(fold_key, false, false),
        ];
        accounts.extend(
            batch
                .iter()
                .map(|(event_address, _)| meta(*event_address, true, false)),
        );
        send(
            &config.wallet,
            &config.rpc_url,
            &format!("fold {} encrypted queue event(s)", batch.len()),
            accounts,
            beatex::instruction::FoldQueuedOrders {
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
            .data(),
            vec![],
        )?;
        total_folded += batch.len();
    }
}

fn execute_publish(config: PublishConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction_addresses = auction_addresses(&addresses, config.auction_id);
    let market = fetch_anchor_account::<beatex::Market>(&config.rpc_url, &addresses.market)?
        .ok_or("market has not been initialized")?;
    let auction =
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not been initialized")?;
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
    let key_epoch =
        fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &addresses.key_epoch)?
            .ok_or("key epoch has not been initialized")?;
    let aggregate = ciphertext_array(&accumulator.ciphertexts)?;
    let participant_ids = [1, 4, 7];
    let prover = AggregatePublicationProver::deterministic_devnet()?;
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
    send(
        &config.wallet,
        &config.rpc_url,
        "publish aggregate curves and clear auction",
        vec![
            meta(addresses.market, true, false),
            meta(auction_addresses.auction, true, false),
            meta(addresses.key_epoch, false, false),
            meta(auction_addresses.submission_counter, false, false),
            meta(auction_addresses.accumulator, false, false),
            meta(publication_key, false, false),
        ],
        beatex::instruction::PublishAndClear {
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
        .data(),
        vec![],
    )?;
    let settled =
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("settled auction account disappeared")?;
    println!(
        "auction {} settled at price {} with volume {} (level {})",
        config.auction_id, settled.clearing_price, settled.clearing_volume, settled.clearing_index
    );
    Ok(())
}

fn execute_cancel(config: FoldConfig) -> Result<(), Box<dyn Error>> {
    if config.rpc_url.contains("mainnet") {
        return Err("mainnet is not supported by the Devnet client".into());
    }
    let addresses = addresses(&config.common);
    let auction = auction_addresses(&addresses, config.auction_id);
    send(
        &config.wallet,
        &config.rpc_url,
        &format!("cancel expired auction {}", config.auction_id),
        vec![
            meta(addresses.market, false, false),
            meta(auction.auction, true, false),
            meta(auction.submission_counter, false, false),
            meta(auction.accumulator, false, false),
        ],
        beatex::instruction::CancelPendingAuction {}.data(),
        vec![],
    )?;
    let cancelled = fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction.auction)?
        .ok_or("cancelled auction account disappeared")?;
    if cancelled.state != beatex::AuctionState::Cancelled {
        return Err("auction did not enter the cancelled state".into());
    }
    println!("auction {} cancelled", config.auction_id);
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
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not been initialized")?;
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
        .active_order
        .clone()
        .ok_or("the trader has no locally opened active order")?;
    if stored.auction_id != config.auction_id
        || stored.auction != auction_addresses.auction.to_string()
    {
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
    let demand = auction.decrypted_demand[clearing_index];
    let supply = auction.decrypted_supply[clearing_index];
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
    trader_state.active_order = None;
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
        fetch_anchor_account::<beatex::Auction>(&config.rpc_url, &auction_addresses.auction)?
            .ok_or("auction has not been initialized")?;
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
        .active_order
        .clone()
        .ok_or("the trader has no locally opened active order")?;
    if stored.auction_id != config.auction_id
        || stored.auction != auction_addresses.auction.to_string()
    {
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
    let private_balance = private_balance_address(&addresses.market, &owner);
    let key_epoch = fetch_anchor_account::<beatex::KeyEpoch>(&config.rpc_url, &auction.key_epoch)?
        .ok_or("key epoch account is missing")?;

    match config.kind {
        PrivateWithdrawalKind::Claim => {
            let clearing_index = usize::from(auction.clearing_index);
            let prover = RolloverProver::deterministic_devnet()?;
            let mut rng = ChaCha20Rng::from_entropy();
            let output = prover.prove(
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
                    demand: auction.decrypted_demand[clearing_index],
                    supply: auction.decrypted_supply[clearing_index],
                    clearing_volume: auction.clearing_volume,
                    balance_revision: state.revision,
                    committee_key: key_epoch.babyjub_encryption_pubkey,
                    old_balance: state.balance.opening(),
                },
                &mut rng,
            )?;
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
            println!(
                "auction {} settled internally: {} base units and {} quote units",
                config.auction_id, output.base_payout, output.quote_payout
            );
        }
        PrivateWithdrawalKind::Refund => {
            let prover = CollateralReleaseProver::deterministic_devnet()?;
            let mut rng = ChaCha20Rng::from_entropy();
            let output = prover.prove(
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
            )?;
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
            state.balance = output.released_balance.into();
            println!("auction {} refunded internally", config.auction_id);
        }
    }
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or("private balance revision overflow")?;
    state.active_order = None;
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
    if state.active_order.is_some() {
        return Err("settle or cancel the active order before withdrawing".into());
    }
    let prover = DepositProver::deterministic_devnet()?;
    let mut rng = ChaCha20Rng::from_entropy();
    let output = prover.prove_public_withdrawal(
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
    )?;
    let deposit_key = verification_key_address(
        &addresses.market,
        QueueCircuitKind::BalanceDeposit,
        queue::BALANCE_DEPOSIT_CIRCUIT_VERSION,
    );
    let private_balance = private_balance_address(&addresses.market, &owner);
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
        let version = queue::circuit_version(kind);
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
        "auction" => initialize_current_auction(
            common(&args[1..])?,
            PathBuf::from(value(&args[1..], "--wallet")?),
            optional(&args[1..], "--rpc-url").unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
        ),
        "order" => execute_order(OrderConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            base_source: Pubkey::from_str(&value(&args[1..], "--base-source")?)?,
            quote_source: Pubkey::from_str(&value(&args[1..], "--quote-source")?)?,
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            deposit_base: optional(&args[1..], "--deposit-base")
                .unwrap_or_else(|| "100".to_owned())
                .parse()?,
            deposit_quote: optional(&args[1..], "--deposit-quote")
                .unwrap_or_else(|| "10000000".to_owned())
                .parse()?,
        }),
        "fold" => execute_fold(FoldConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
        }),
        "publish" => execute_publish(PublishConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
        }),
        "cancel" => execute_cancel(FoldConfig {
            common: common(&args[1..])?,
            wallet: PathBuf::from(value(&args[1..], "--wallet")?),
            rpc_url: optional(&args[1..], "--rpc-url")
                .unwrap_or_else(|| DEFAULT_RPC_URL.to_owned()),
            auction_id: value(&args[1..], "--auction-id")?.parse()?,
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
}

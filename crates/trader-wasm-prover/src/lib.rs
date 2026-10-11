//! Production-shaped WebAssembly boundary for BeatEx trader proofs.
//!
//! Proving keys are supplied by the caller and decoded once by [`TraderProver`].
//! Trusted setup is deliberately unavailable from the WebAssembly API. Queue
//! folding, aggregate publication, and confidential Token-2022 claims are
//! infrastructure circuits and are deliberately not exposed here.

use {
    beatex_babyjubjub_groth16_spike::{
        deposit::{
            DepositProofOutput as CoreDepositOutput, DepositProofRequest as CoreDepositRequest,
            DepositProver, PublicWithdrawalProofOutput as CoreWithdrawalOutput,
            PublicWithdrawalProofRequest as CoreWithdrawalRequest,
        },
        release::{
            CollateralReleaseProofOutput as CoreReleaseOutput,
            CollateralReleaseProofRequest as CoreReleaseRequest, CollateralReleaseProver,
        },
        rollover::{
            RolloverProofOutput as CoreSettlementOutput,
            RolloverProofRequest as CoreSettlementRequest, RolloverProver,
        },
        transition::{
            OrderProofOutput as CoreOrderOutput, OrderProofRequest as CoreOrderRequest,
            OrderProver, PrivateBalanceOpening as CoreBalance,
        },
        BALANCE_DEPOSIT_CIRCUIT_VERSION, DEVNET_FAST_ORDER_CIRCUIT_VERSION, LEVELS,
        ORDER_MODIFICATION_CIRCUIT_VERSION, ORDER_SUBMISSION_CIRCUIT_VERSION,
        ROLLOVER_SETTLEMENT_CIRCUIT_VERSION,
    },
    rand::rngs::OsRng,
    serde::{Deserialize, Serialize},
    serde_bytes::ByteBuf,
    wasm_bindgen::prelude::*,
};

const API_VERSION: u32 = 2;
const BACKEND: &str = "beatex-arkworks-groth16-wasm-v2";

type Bytes32 = [u8; 32];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BalanceOpening {
    base_value: u64,
    quote_value: u64,
    base_randomness: Bytes32,
    quote_randomness: Bytes32,
}

impl From<BalanceOpening> for CoreBalance {
    fn from(value: BalanceOpening) -> Self {
        Self {
            base_value: value.base_value,
            quote_value: value.quote_value,
            base_randomness: value.base_randomness,
            quote_randomness: value.quote_randomness,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceOpeningOutput {
    base_value: u64,
    quote_value: u64,
    base_randomness: ByteBuf,
    quote_randomness: ByteBuf,
}

impl From<CoreBalance> for BalanceOpeningOutput {
    fn from(value: CoreBalance) -> Self {
        Self {
            base_value: value.base_value,
            quote_value: value.quote_value,
            base_randomness: ByteBuf::from(value.base_randomness.to_vec()),
            quote_randomness: ByteBuf::from(value.quote_randomness.to_vec()),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderCurve {
    buy: Vec<u64>,
    sell: Vec<u64>,
}

impl OrderCurve {
    fn into_arrays(self) -> Result<([u64; LEVELS], [u64; LEVELS]), String> {
        let buy = self
            .buy
            .try_into()
            .map_err(|values: Vec<u64>| length_error("curve.buy", values.len(), LEVELS))?;
        let sell = self
            .sell
            .try_into()
            .map_err(|values: Vec<u64>| length_error("curve.sell", values.len(), LEVELS))?;
        Ok((buy, sell))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DepositRequest {
    market: Bytes32,
    owner: Bytes32,
    key_epoch: Bytes32,
    committee_key: Bytes32,
    old_revision: u64,
    base_amount: u64,
    quote_amount: u64,
    old_balance: BalanceOpening,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawalRequest {
    market: Bytes32,
    owner: Bytes32,
    key_epoch: Bytes32,
    committee_key: Bytes32,
    current_revision: u64,
    base_amount: u64,
    quote_amount: u64,
    current_balance: BalanceOpening,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OrderRequest {
    auction: Bytes32,
    owner: Bytes32,
    client_order_nonce: u64,
    reference_price: u64,
    grid_step_bps: u16,
    committee_key: Bytes32,
    curve: OrderCurve,
    old_balance: BalanceOpening,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NextOrderRequest {
    auction: Bytes32,
    owner: Bytes32,
    client_order_nonce: u64,
    reference_price: u64,
    grid_step_bps: u16,
    committee_key: Bytes32,
    curve: OrderCurve,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseRequest {
    market: Bytes32,
    auction: Bytes32,
    active_order: Bytes32,
    owner: Bytes32,
    balance_revision: u64,
    client_order_nonce: u64,
    curve: OrderCurve,
    base_reserve: u64,
    quote_reserve: u64,
    order_salt: Bytes32,
    expected_order_commitment: Bytes32,
    committee_key: Bytes32,
    old_balance: BalanceOpening,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplacementRequest {
    release: ReleaseRequest,
    replacement: NextOrderRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettlementRequest {
    market: Bytes32,
    auction: Bytes32,
    order: Bytes32,
    active_order: Bytes32,
    owner: Bytes32,
    key_epoch: Bytes32,
    client_order_nonce: u64,
    auction_sequence: u64,
    allocation_rule_version: u16,
    order_circuit_version: u16,
    claim_circuit_version: u16,
    curve: OrderCurve,
    base_reserve: u64,
    quote_reserve: u64,
    order_salt: Bytes32,
    expected_order_commitment: Bytes32,
    clearing_index: u8,
    clearing_price: u64,
    demand_at_clearing: u64,
    supply_at_clearing: u64,
    clearing_volume: u64,
    balance_revision: u64,
    committee_key: Bytes32,
    old_balance: BalanceOpening,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RolloverRequest {
    settlement: SettlementRequest,
    next_order: NextOrderRequest,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofOutput {
    proof_a: ByteBuf,
    proof_b: ByteBuf,
    proof_c: ByteBuf,
    public_input: ByteBuf,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProofMetadata {
    backend: &'static str,
    proving_time_ms: f64,
    circuit_version: u16,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DepositOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    proof: ProofOutput,
    new_encrypted_base: ByteBuf,
    new_encrypted_quote: ByteBuf,
    new_balance: BalanceOpeningOutput,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WithdrawalOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    proof: ProofOutput,
    remaining_encrypted_base: ByteBuf,
    remaining_encrypted_quote: ByteBuf,
    remaining_balance: BalanceOpeningOutput,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrderOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    proof: ProofOutput,
    order_commitment: ByteBuf,
    ciphertexts: Vec<ByteBuf>,
    new_encrypted_base: ByteBuf,
    new_encrypted_quote: ByteBuf,
    new_balance: BalanceOpeningOutput,
    order_salt: ByteBuf,
    base_reserve: u64,
    quote_reserve: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    proof: ProofOutput,
    released_encrypted_base: ByteBuf,
    released_encrypted_quote: ByteBuf,
    released_balance: BalanceOpeningOutput,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettlementOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    proof: ProofOutput,
    terminal_nullifier: ByteBuf,
    credited_encrypted_base: ByteBuf,
    credited_encrypted_quote: ByteBuf,
    credited_balance: BalanceOpeningOutput,
    base_payout: u64,
    quote_payout: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplacementOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    release: ReleaseOutput,
    replacement: OrderOutput,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RolloverOutput {
    #[serde(flatten)]
    metadata: ProofMetadata,
    settlement: SettlementOutput,
    next_order: OrderOutput,
}

/// One in-memory runtime for every proof needed by a trader frontend.
///
/// Key bytes must be SHA-256-authenticated Arkworks canonical-uncompressed
/// `ProvingKey<Bn254>` artifacts for circuit version 2. Each artifact is
/// decoded exactly once. The official TypeScript loader performs the required
/// digest and byte-length checks before calling this lower-level API.
#[wasm_bindgen]
#[derive(Default)]
pub struct TraderProver {
    deposit: Option<DepositProver>,
    order: Option<OrderProver>,
    release: Option<CollateralReleaseProver>,
    settlement: Option<RolloverProver>,
    order_version: u16,
}

#[wasm_bindgen]
impl TraderProver {
    #[wasm_bindgen(constructor)]
    pub fn new() -> TraderProver {
        Self::default()
    }

    /// Decode and retain one already-authenticated proving key. Setup never
    /// runs in the browser.
    #[wasm_bindgen(js_name = loadProvingKey)]
    pub fn load_proving_key(&mut self, circuit: &str, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_proving_key_for_version(circuit, ORDER_SUBMISSION_CIRCUIT_VERSION, bytes)
    }

    #[wasm_bindgen(js_name = loadProvingKeyForVersion)]
    pub fn load_proving_key_for_version(
        &mut self,
        circuit: &str,
        circuit_version: u16,
        bytes: &[u8],
    ) -> Result<(), JsValue> {
        match circuit {
            "balance" => {
                self.deposit = Some(
                    DepositProver::from_trusted_uncompressed_bytes(bytes)
                        .map_err(|error| key_error("balance", error))?,
                );
            }
            "order" => {
                if !matches!(
                    circuit_version,
                    ORDER_SUBMISSION_CIRCUIT_VERSION | DEVNET_FAST_ORDER_CIRCUIT_VERSION
                ) {
                    return Err(JsValue::from_str("unsupported order circuit version"));
                }
                self.order = Some(
                    OrderProver::from_trusted_uncompressed_bytes_for_version(bytes, circuit_version)
                        .map_err(|error| key_error("order", error))?,
                );
                self.order_version = circuit_version;
            }
            "release" => {
                self.release = Some(
                    CollateralReleaseProver::from_trusted_uncompressed_bytes(bytes)
                        .map_err(|error| key_error("release", error))?,
                );
            }
            "settlement" => {
                if circuit_version != ROLLOVER_SETTLEMENT_CIRCUIT_VERSION {
                    return Err(JsValue::from_str("unsupported settlement circuit version"));
                }
                self.settlement = Some(
                    RolloverProver::from_trusted_uncompressed_bytes_for_version(
                        bytes,
                        circuit_version,
                    )
                        .map_err(|error| key_error("settlement", error))?,
                );
            }
            _ => return Err(JsValue::from_str("unknown trader proving circuit")),
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = isProvingKeyLoaded)]
    pub fn is_proving_key_loaded(&self, circuit: &str) -> bool {
        match circuit {
            "balance" => self.deposit.is_some(),
            "order" => self.order.is_some(),
            "release" => self.release.is_some(),
            "settlement" => self.settlement.is_some(),
            _ => false,
        }
    }

    #[wasm_bindgen(js_name = proveDeposit)]
    pub fn prove_deposit(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: DepositRequest = from_js(request)?;
        let started = now_ms();
        let output = self
            .deposit
            .as_ref()
            .ok_or_else(|| unloaded_key("balance"))?
            .prove(
                CoreDepositRequest {
                    market: request.market,
                    owner: request.owner,
                    key_epoch: request.key_epoch,
                    committee_key: request.committee_key,
                    old_revision: request.old_revision,
                    base_amount: request.base_amount,
                    quote_amount: request.quote_amount,
                    old_balance: request.old_balance.into(),
                },
                &mut OsRng,
            )
            .map_err(proof_error)?;
        to_js(&deposit_output(output, elapsed_ms(started)))
    }

    #[wasm_bindgen(js_name = proveWithdrawal)]
    pub fn prove_withdrawal(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: WithdrawalRequest = from_js(request)?;
        let started = now_ms();
        let output = self
            .deposit
            .as_ref()
            .ok_or_else(|| unloaded_key("balance"))?
            .prove_public_withdrawal(
                CoreWithdrawalRequest {
                    market: request.market,
                    owner: request.owner,
                    key_epoch: request.key_epoch,
                    committee_key: request.committee_key,
                    current_revision: request.current_revision,
                    base_amount: request.base_amount,
                    quote_amount: request.quote_amount,
                    current_balance: request.current_balance.into(),
                },
                &mut OsRng,
            )
            .map_err(proof_error)?;
        to_js(&withdrawal_output(output, elapsed_ms(started)))
    }

    #[wasm_bindgen(js_name = proveOrder)]
    pub fn prove_order(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: OrderRequest = from_js(request)?;
        let started = now_ms();
        let output = self
            .order
            .as_ref()
            .ok_or_else(|| unloaded_key("order"))?
            .prove(order_request(request)?, &mut OsRng)
            .map_err(proof_error)?;
        to_js(&order_output(
            output,
            elapsed_ms(started),
            self.order_version,
        ))
    }

    #[wasm_bindgen(js_name = proveCancellation)]
    pub fn prove_cancellation(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: ReleaseRequest = from_js(request)?;
        let started = now_ms();
        let output = self
            .release
            .as_ref()
            .ok_or_else(|| unloaded_key("release"))?
            .prove(release_request(request)?, &mut OsRng)
            .map_err(proof_error)?;
        to_js(&release_output(output, elapsed_ms(started)))
    }

    #[wasm_bindgen(js_name = proveSettlement)]
    pub fn prove_settlement(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: SettlementRequest = from_js(request)?;
        let started = now_ms();
        let output = self
            .settlement
            .as_ref()
            .ok_or_else(|| unloaded_key("settlement"))?
            .prove(settlement_request(request)?, &mut OsRng)
            .map_err(proof_error)?;
        to_js(&settlement_output(output, elapsed_ms(started)))
    }

    #[wasm_bindgen(js_name = proveReplacement)]
    pub fn prove_replacement(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: ReplacementRequest = from_js(request)?;
        if request.release.auction != request.replacement.auction
            || request.release.owner != request.replacement.owner
            || request.release.committee_key != request.replacement.committee_key
        {
            return Err(JsValue::from_str(
                "release and replacement must share auction, owner, and committee key",
            ));
        }
        let started = now_ms();
        let release_started = now_ms();
        let release = self
            .release
            .as_ref()
            .ok_or_else(|| unloaded_key("release"))?
            .prove(release_request(request.release)?, &mut OsRng)
            .map_err(proof_error)?;
        let release_ms = elapsed_ms(release_started);
        let replacement_started = now_ms();
        let replacement = self
            .order
            .as_ref()
            .ok_or_else(|| unloaded_key("order"))?
            .prove(
                next_order_request(request.replacement, release.released_balance.clone())?,
                &mut OsRng,
            )
            .map_err(proof_error)?;
        let replacement_ms = elapsed_ms(replacement_started);
        to_js(&ReplacementOutput {
            metadata: metadata(elapsed_ms(started), self.order_version),
            release: release_output(release, release_ms),
            replacement: order_output(replacement, replacement_ms, self.order_version),
        })
    }

    #[wasm_bindgen(js_name = proveRollover)]
    pub fn prove_rollover(&self, request: JsValue) -> Result<JsValue, JsValue> {
        let request: RolloverRequest = from_js(request)?;
        if request.settlement.owner != request.next_order.owner
            || request.settlement.committee_key != request.next_order.committee_key
        {
            return Err(JsValue::from_str(
                "settlement and next order must share owner and committee key",
            ));
        }
        let started = now_ms();
        let settlement_started = now_ms();
        let settlement = self
            .settlement
            .as_ref()
            .ok_or_else(|| unloaded_key("settlement"))?
            .prove(settlement_request(request.settlement)?, &mut OsRng)
            .map_err(proof_error)?;
        let settlement_ms = elapsed_ms(settlement_started);
        let next_order_started = now_ms();
        let next_order = self
            .order
            .as_ref()
            .ok_or_else(|| unloaded_key("order"))?
            .prove(
                next_order_request(request.next_order, settlement.credited_balance.clone())?,
                &mut OsRng,
            )
            .map_err(proof_error)?;
        let next_order_ms = elapsed_ms(next_order_started);
        to_js(&RolloverOutput {
            metadata: metadata(elapsed_ms(started), ROLLOVER_SETTLEMENT_CIRCUIT_VERSION),
            settlement: settlement_output(settlement, settlement_ms),
            next_order: order_output(next_order, next_order_ms, self.order_version),
        })
    }
}

/// ABI version for worker/package compatibility checks.
#[wasm_bindgen(js_name = traderProverApiVersion)]
pub fn trader_prover_api_version() -> u32 {
    API_VERSION
}

fn order_request(request: OrderRequest) -> Result<CoreOrderRequest, JsValue> {
    let (buy, sell) = request.curve.into_arrays().map_err(request_error)?;
    Ok(CoreOrderRequest {
        auction: request.auction,
        owner: request.owner,
        client_order_nonce: request.client_order_nonce,
        reference_price: request.reference_price,
        grid_step_bps: request.grid_step_bps,
        committee_key: request.committee_key,
        buy,
        sell,
        old_balance: request.old_balance.into(),
    })
}

fn next_order_request(
    request: NextOrderRequest,
    old_balance: CoreBalance,
) -> Result<CoreOrderRequest, JsValue> {
    let (buy, sell) = request.curve.into_arrays().map_err(request_error)?;
    Ok(CoreOrderRequest {
        auction: request.auction,
        owner: request.owner,
        client_order_nonce: request.client_order_nonce,
        reference_price: request.reference_price,
        grid_step_bps: request.grid_step_bps,
        committee_key: request.committee_key,
        buy,
        sell,
        old_balance,
    })
}

fn release_request(request: ReleaseRequest) -> Result<CoreReleaseRequest, JsValue> {
    let (buy, sell) = request.curve.into_arrays().map_err(request_error)?;
    Ok(CoreReleaseRequest {
        market: request.market,
        auction: request.auction,
        active_order: request.active_order,
        owner: request.owner,
        balance_revision: request.balance_revision,
        client_order_nonce: request.client_order_nonce,
        buy,
        sell,
        base_reserve: request.base_reserve,
        quote_reserve: request.quote_reserve,
        order_salt: request.order_salt,
        expected_order_commitment: request.expected_order_commitment,
        committee_key: request.committee_key,
        old_balance: request.old_balance.into(),
    })
}

fn settlement_request(request: SettlementRequest) -> Result<CoreSettlementRequest, JsValue> {
    let (buy, sell) = request.curve.into_arrays().map_err(request_error)?;
    Ok(CoreSettlementRequest {
        market: request.market,
        auction: request.auction,
        order: request.order,
        active_order: request.active_order,
        owner: request.owner,
        key_epoch: request.key_epoch,
        client_order_nonce: request.client_order_nonce,
        sequence: request.auction_sequence,
        allocation_rule_version: request.allocation_rule_version,
        order_circuit_version: request.order_circuit_version,
        claim_circuit_version: request.claim_circuit_version,
        buy,
        sell,
        base_reserve: request.base_reserve,
        quote_reserve: request.quote_reserve,
        order_salt: request.order_salt,
        expected_order_commitment: request.expected_order_commitment,
        clearing_index: request.clearing_index,
        clearing_price: request.clearing_price,
        demand: request.demand_at_clearing,
        supply: request.supply_at_clearing,
        clearing_volume: request.clearing_volume,
        balance_revision: request.balance_revision,
        committee_key: request.committee_key,
        old_balance: request.old_balance.into(),
    })
}

fn proof(a: [u8; 64], b: [u8; 128], c: [u8; 64], input: [u8; 32]) -> ProofOutput {
    ProofOutput {
        proof_a: ByteBuf::from(a.to_vec()),
        proof_b: ByteBuf::from(b.to_vec()),
        proof_c: ByteBuf::from(c.to_vec()),
        public_input: ByteBuf::from(input.to_vec()),
    }
}

fn metadata(proving_time_ms: f64, circuit_version: u16) -> ProofMetadata {
    ProofMetadata {
        backend: BACKEND,
        proving_time_ms,
        circuit_version,
    }
}

fn deposit_output(value: CoreDepositOutput, proving_time_ms: f64) -> DepositOutput {
    DepositOutput {
        metadata: metadata(proving_time_ms, BALANCE_DEPOSIT_CIRCUIT_VERSION),
        proof: proof(
            value.proof_a,
            value.proof_b,
            value.proof_c,
            value.public_input,
        ),
        new_encrypted_base: ByteBuf::from(value.new_encrypted_base.to_vec()),
        new_encrypted_quote: ByteBuf::from(value.new_encrypted_quote.to_vec()),
        new_balance: value.new_balance.into(),
    }
}

fn withdrawal_output(value: CoreWithdrawalOutput, proving_time_ms: f64) -> WithdrawalOutput {
    WithdrawalOutput {
        metadata: metadata(proving_time_ms, BALANCE_DEPOSIT_CIRCUIT_VERSION),
        proof: proof(
            value.proof_a,
            value.proof_b,
            value.proof_c,
            value.public_input,
        ),
        remaining_encrypted_base: ByteBuf::from(value.remaining_encrypted_base.to_vec()),
        remaining_encrypted_quote: ByteBuf::from(value.remaining_encrypted_quote.to_vec()),
        remaining_balance: value.remaining_balance.into(),
    }
}

fn order_output(value: CoreOrderOutput, proving_time_ms: f64, circuit_version: u16) -> OrderOutput {
    OrderOutput {
        metadata: metadata(proving_time_ms, circuit_version),
        proof: proof(
            value.proof_a,
            value.proof_b,
            value.proof_c,
            value.public_input,
        ),
        order_commitment: ByteBuf::from(value.order_commitment.to_vec()),
        ciphertexts: value
            .ciphertexts
            .into_iter()
            .map(|bytes| ByteBuf::from(bytes.to_vec()))
            .collect(),
        new_encrypted_base: ByteBuf::from(value.new_encrypted_base.to_vec()),
        new_encrypted_quote: ByteBuf::from(value.new_encrypted_quote.to_vec()),
        new_balance: value.new_balance.into(),
        order_salt: ByteBuf::from(value.order_salt.to_vec()),
        base_reserve: value.base_reserve,
        quote_reserve: value.quote_reserve,
    }
}

fn release_output(value: CoreReleaseOutput, proving_time_ms: f64) -> ReleaseOutput {
    ReleaseOutput {
        metadata: metadata(proving_time_ms, ORDER_MODIFICATION_CIRCUIT_VERSION),
        proof: proof(
            value.proof_a,
            value.proof_b,
            value.proof_c,
            value.public_input,
        ),
        released_encrypted_base: ByteBuf::from(value.released_encrypted_base.to_vec()),
        released_encrypted_quote: ByteBuf::from(value.released_encrypted_quote.to_vec()),
        released_balance: value.released_balance.into(),
    }
}

fn settlement_output(value: CoreSettlementOutput, proving_time_ms: f64) -> SettlementOutput {
    SettlementOutput {
        metadata: metadata(proving_time_ms, ROLLOVER_SETTLEMENT_CIRCUIT_VERSION),
        proof: proof(
            value.proof_a,
            value.proof_b,
            value.proof_c,
            value.public_input,
        ),
        terminal_nullifier: ByteBuf::from(value.terminal_nullifier.to_vec()),
        credited_encrypted_base: ByteBuf::from(value.credited_encrypted_base.to_vec()),
        credited_encrypted_quote: ByteBuf::from(value.credited_encrypted_quote.to_vec()),
        credited_balance: value.credited_balance.into(),
        base_payout: value.base_payout,
        quote_payout: value.quote_payout,
    }
}

fn from_js<T: for<'de> Deserialize<'de>>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| JsValue::from_str(&format!("invalid proof request: {error}")))
}

fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::new()
        .serialize_maps_as_objects(true)
        .serialize_large_number_types_as_bigints(true);
    value
        .serialize(&serializer)
        .map_err(|error| JsValue::from_str(&format!("failed to serialize proof result: {error}")))
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

fn elapsed_ms(started: f64) -> f64 {
    now_ms() - started
}

fn length_error(name: &str, actual: usize, expected: usize) -> String {
    format!("{name} must contain exactly {expected} quantities; received {actual}")
}

fn request_error(error: String) -> JsValue {
    JsValue::from_str(&error)
}

fn key_error(name: &str, error: Box<dyn std::error::Error>) -> JsValue {
    JsValue::from_str(&format!("invalid {name} proving key: {error}"))
}

fn unloaded_key(name: &str) -> JsValue {
    JsValue::from_str(&format!("{name} proving key is not loaded"))
}

fn proof_error(error: Box<dyn std::error::Error>) -> JsValue {
    JsValue::from_str(&format!("proof generation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_requires_protocol_level_count() {
        let error = OrderCurve {
            buy: vec![0; LEVELS - 1],
            sell: vec![0; LEVELS],
        }
        .into_arrays()
        .unwrap_err();
        assert!(error.contains("curve.buy"));
    }

    #[test]
    fn output_metadata_is_versioned() {
        let value = metadata(12.5, DEVNET_FAST_ORDER_CIRCUIT_VERSION);
        assert_eq!(value.backend, BACKEND);
        assert_eq!(value.circuit_version, DEVNET_FAST_ORDER_CIRCUIT_VERSION);
        assert_eq!(value.proving_time_ms, 12.5);
    }
}

use std::{
    collections::{HashMap, VecDeque},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::{
    net::TcpListener,
    sync::{oneshot, Notify},
    task::JoinHandle,
    time::timeout,
};
use tower_http::set_header::SetResponseHeaderLayer;

pub mod tui;

const LOGIN_TTL: Duration = Duration::from_secs(120);
const REQUEST_WAIT: Duration = Duration::from_secs(20);
const MAX_BODY_BYTES: usize = 128 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("wallet bridge timed out")]
    Timeout,
    #[error("wallet bridge closed before the operation completed")]
    Closed,
    #[error("invalid wallet address")]
    InvalidAddress,
    #[error("invalid wallet signature")]
    InvalidSignature,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SupportedTransactionVersion {
    Legacy(String),
    Number(u8),
}

impl SupportedTransactionVersion {
    fn is_known(&self) -> bool {
        matches!(self, Self::Legacy(value) if value == "legacy")
            || matches!(self, Self::Number(0 | 1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletProfile {
    pub version: u8,
    pub wallet_name: String,
    pub address: String,
    pub supported_transaction_versions: Vec<SupportedTransactionVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserWalletSessionDescriptor {
    pub version: u8,
    #[serde(rename = "type")]
    pub session_type: String,
    pub address: String,
    pub bridge_url: String,
    pub session_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SigningRequest {
    pub id: String,
    pub kind: SigningRequestKind,
    pub payload_base64: String,
    pub chain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SigningRequestKind {
    Message,
    Transaction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeginLoginRequest {
    wallet_name: String,
    address: String,
    #[serde(default)]
    supported_transaction_versions: Vec<SupportedTransactionVersion>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeResponse {
    message_base64: String,
    expires_at_unix_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteLoginRequest {
    address: String,
    signature_base64: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SigningResponse {
    id: String,
    #[serde(default)]
    signed_payload_base64: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignTransactionRequest {
    transaction_base64: String,
    chain: String,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SignTransactionResponse {
    signed_transaction_base64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiError {
    error: String,
}

#[derive(Debug, Clone)]
struct PendingLogin {
    profile: WalletProfile,
    challenge: Vec<u8>,
    expires_at_unix_ms: u64,
}

#[derive(Debug)]
struct PendingSignature {
    tx: oneshot::Sender<Result<Vec<u8>, String>>,
}

#[derive(Debug)]
struct SharedState {
    origin: String,
    token: String,
    pending_login: Mutex<Option<PendingLogin>>,
    connected_profile: Mutex<Option<WalletProfile>>,
    login_tx: Mutex<Option<oneshot::Sender<WalletProfile>>>,
    requests: Mutex<VecDeque<SigningRequest>>,
    signatures: Mutex<HashMap<String, PendingSignature>>,
    request_notify: Notify,
}

pub struct BrowserWalletBridge {
    address: SocketAddr,
    state: Arc<SharedState>,
    login_rx: Option<oneshot::Receiver<WalletProfile>>,
    server: JoinHandle<()>,
}

impl BrowserWalletBridge {
    pub async fn start() -> Result<Self, BridgeError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let origin = format!("http://{address}");
        let token = random_token(32);
        let (login_tx, login_rx) = oneshot::channel();
        let state = Arc::new(SharedState {
            origin,
            token,
            pending_login: Mutex::new(None),
            connected_profile: Mutex::new(None),
            login_tx: Mutex::new(Some(login_tx)),
            requests: Mutex::new(VecDeque::new()),
            signatures: Mutex::new(HashMap::new()),
            request_notify: Notify::new(),
        });

        let app = Router::new()
            .route("/", get(index))
            .route("/api/challenge", post(begin_login))
            .route("/api/connect", post(complete_login))
            .route("/api/request", get(next_signing_request))
            .route("/api/response", post(complete_signing_request))
            .route("/api/sign-transaction", post(sign_transaction))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ))
            .with_state(state.clone());

        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Ok(Self {
            address,
            state,
            login_rx: Some(login_rx),
            server,
        })
    }

    pub fn browser_url(&self) -> String {
        format!("{}/#token={}", self.state.origin, self.state.token)
    }

    pub async fn wait_for_login(&mut self, wait: Duration) -> Result<WalletProfile, BridgeError> {
        let login_rx = self.login_rx.take().ok_or(BridgeError::Closed)?;
        let result = timeout(wait, login_rx).await;
        match result {
            Ok(Ok(profile)) => Ok(profile),
            Ok(Err(_)) => Err(BridgeError::Closed),
            Err(_) => Err(BridgeError::Timeout),
        }
    }

    pub fn session_descriptor(&self, address: impl Into<String>) -> BrowserWalletSessionDescriptor {
        BrowserWalletSessionDescriptor {
            version: 1,
            session_type: "browser-wallet".to_owned(),
            address: address.into(),
            bridge_url: self.state.origin.clone(),
            session_token: self.state.token.clone(),
        }
    }

    pub async fn request_signature(
        &self,
        kind: SigningRequestKind,
        payload: &[u8],
        chain: impl Into<String>,
        wait: Duration,
    ) -> Result<Vec<u8>, BridgeError> {
        let id = random_token(18);
        let (tx, rx) = oneshot::channel();
        self.state
            .signatures
            .lock()
            .expect("signatures mutex poisoned")
            .insert(id.clone(), PendingSignature { tx });
        self.state
            .requests
            .lock()
            .expect("requests mutex poisoned")
            .push_back(SigningRequest {
                id: id.clone(),
                kind,
                payload_base64: BASE64.encode(payload),
                chain: chain.into(),
                label: None,
            });
        self.state.request_notify.notify_one();

        match timeout(wait, rx).await {
            Ok(Ok(Ok(bytes))) => Ok(bytes),
            Ok(Ok(Err(_))) | Ok(Err(_)) => Err(BridgeError::Closed),
            Err(_) => {
                self.state
                    .signatures
                    .lock()
                    .expect("signatures mutex poisoned")
                    .remove(&id);
                Err(BridgeError::Timeout)
            }
        }
    }

    pub fn socket_address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for BrowserWalletBridge {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub fn default_profile_path() -> PathBuf {
    if let Some(path) = std::env::var_os("BEATEX_STATE_DIR") {
        return PathBuf::from(path).join("wallet.json");
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("beatex/wallet.json");
    }
    if let Some(user_home) = std::env::var_os("HOME") {
        return PathBuf::from(user_home).join(".config/beatex/wallet.json");
    }
    PathBuf::from(".beatex/wallet.json")
}

pub fn load_profile(path: &Path) -> Result<WalletProfile, BridgeError> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn save_profile(path: &Path, profile: &WalletProfile) -> Result<(), BridgeError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".wallet-{}.tmp", random_token(12)));
    let bytes = serde_json::to_vec_pretty(profile)?;
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn random_token(byte_len: usize) -> String {
    let mut bytes = vec![0_u8; byte_len];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn login_challenge(
    origin: &str,
    address: &str,
    nonce: &str,
    issued_at_unix_ms: u64,
    expires_at_unix_ms: u64,
) -> Vec<u8> {
    format!(
        "BeatEx browser wallet login\nVersion: 1\nOrigin: {origin}\nAddress: {address}\nNonce: {nonce}\nIssued At: {issued_at_unix_ms}\nExpiration Time: {expires_at_unix_ms}"
    )
    .into_bytes()
}

pub fn verify_wallet_signature(
    address: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<(), BridgeError> {
    let public_key = bs58::decode(address)
        .into_vec()
        .map_err(|_| BridgeError::InvalidAddress)?;
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| BridgeError::InvalidAddress)?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| BridgeError::InvalidSignature)?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).map_err(|_| BridgeError::InvalidAddress)?;
    verifying_key
        .verify_strict(message, &Signature::from_bytes(&signature))
        .map_err(|_| BridgeError::InvalidSignature)
}

async fn index(State(state): State<Arc<SharedState>>, request: Request<Body>) -> Response {
    if !valid_host(request.headers(), &state.origin) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut response = Html(include_str!("wallet.html")).into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

async fn begin_login(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    Json(input): Json<BeginLoginRequest>,
) -> Response {
    if let Some(response) = authorization_error(&state, &headers) {
        return response;
    }
    if input.wallet_name.trim().is_empty()
        || input.wallet_name.len() > 80
        || input.address.len() > 64
        || bs58::decode(&input.address)
            .into_vec()
            .map(|bytes| bytes.len() != 32)
            .unwrap_or(true)
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid wallet metadata");
    }

    let now = now_unix_ms();
    let expires_at = now.saturating_add(LOGIN_TTL.as_millis() as u64);
    let challenge = login_challenge(
        &state.origin,
        &input.address,
        &random_token(24),
        now,
        expires_at,
    );
    let mut versions = Vec::new();
    for version in input.supported_transaction_versions {
        if version.is_known() && !versions.contains(&version) {
            versions.push(version);
        }
    }
    *state.pending_login.lock().expect("login mutex poisoned") = Some(PendingLogin {
        profile: WalletProfile {
            version: 1,
            wallet_name: input.wallet_name,
            address: input.address,
            supported_transaction_versions: versions,
        },
        challenge: challenge.clone(),
        expires_at_unix_ms: expires_at,
    });

    Json(ChallengeResponse {
        message_base64: BASE64.encode(challenge),
        expires_at_unix_ms: expires_at,
    })
    .into_response()
}

async fn complete_login(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    Json(input): Json<CompleteLoginRequest>,
) -> Response {
    if let Some(response) = authorization_error(&state, &headers) {
        return response;
    }
    let pending = state
        .pending_login
        .lock()
        .expect("login mutex poisoned")
        .take();
    let Some(pending) = pending else {
        return api_error(StatusCode::CONFLICT, "no active login challenge");
    };
    if pending.expires_at_unix_ms < now_unix_ms() {
        return api_error(StatusCode::UNAUTHORIZED, "login challenge expired");
    }
    if pending.profile.address != input.address {
        return api_error(StatusCode::UNAUTHORIZED, "wallet address changed");
    }
    let signature = match BASE64.decode(input.signature_base64) {
        Ok(signature) => signature,
        Err(_) => return api_error(StatusCode::BAD_REQUEST, "invalid signature encoding"),
    };
    if verify_wallet_signature(&input.address, &pending.challenge, &signature).is_err() {
        return api_error(StatusCode::UNAUTHORIZED, "signature verification failed");
    }

    if let Some(tx) = state
        .login_tx
        .lock()
        .expect("login sender mutex poisoned")
        .take()
    {
        *state
            .connected_profile
            .lock()
            .expect("connected profile mutex poisoned") = Some(pending.profile.clone());
        let _ = tx.send(pending.profile);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn next_signing_request(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = authorization_error(&state, &headers) {
        return response;
    }

    for _ in 0..2 {
        if let Some(request) = state
            .requests
            .lock()
            .expect("requests mutex poisoned")
            .pop_front()
        {
            return Json(request).into_response();
        }
        if timeout(REQUEST_WAIT, state.request_notify.notified())
            .await
            .is_err()
        {
            break;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn complete_signing_request(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    Json(input): Json<SigningResponse>,
) -> Response {
    if let Some(response) = authorization_error(&state, &headers) {
        return response;
    }
    let pending = state
        .signatures
        .lock()
        .expect("signatures mutex poisoned")
        .remove(&input.id);
    let Some(pending) = pending else {
        return api_error(StatusCode::NOT_FOUND, "unknown signing request");
    };
    let result = if let Some(error) = input.error {
        Err(error.chars().take(200).collect())
    } else if let Some(payload) = input.signed_payload_base64 {
        BASE64
            .decode(payload)
            .map_err(|_| "invalid signed payload encoding".to_owned())
    } else {
        Err("missing signed payload".to_owned())
    };
    let _ = pending.tx.send(result);
    StatusCode::NO_CONTENT.into_response()
}

async fn sign_transaction(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    Json(input): Json<SignTransactionRequest>,
) -> Response {
    if let Some(response) = loopback_authorization_error(&state, &headers) {
        return response;
    }
    if input.chain.is_empty()
        || input.chain.len() > 80
        || input.label.is_empty()
        || input.label.len() > 120
    {
        return api_error(StatusCode::BAD_REQUEST, "invalid signing metadata");
    }
    let supports_v1 = state
        .connected_profile
        .lock()
        .expect("connected profile mutex poisoned")
        .as_ref()
        .map(|profile| {
            profile
                .supported_transaction_versions
                .contains(&SupportedTransactionVersion::Number(1))
        })
        .unwrap_or(false);
    if !supports_v1 {
        return api_error(
            StatusCode::PRECONDITION_FAILED,
            "connected wallet does not advertise transaction version 1",
        );
    }
    let transaction = match BASE64.decode(input.transaction_base64) {
        Ok(transaction) if !transaction.is_empty() && transaction.len() <= 4096 => transaction,
        _ => return api_error(StatusCode::BAD_REQUEST, "invalid transaction payload"),
    };

    let id = random_token(18);
    let (tx, rx) = oneshot::channel();
    state
        .signatures
        .lock()
        .expect("signatures mutex poisoned")
        .insert(id.clone(), PendingSignature { tx });
    state
        .requests
        .lock()
        .expect("requests mutex poisoned")
        .push_back(SigningRequest {
            id: id.clone(),
            kind: SigningRequestKind::Transaction,
            payload_base64: BASE64.encode(transaction),
            chain: input.chain,
            label: Some(input.label),
        });
    state.request_notify.notify_one();

    match timeout(Duration::from_secs(180), rx).await {
        Ok(Ok(Ok(signed))) => Json(SignTransactionResponse {
            signed_transaction_base64: BASE64.encode(signed),
        })
        .into_response(),
        Ok(Ok(Err(message))) => api_error(StatusCode::BAD_GATEWAY, &message),
        Ok(Err(_)) => api_error(StatusCode::BAD_GATEWAY, "wallet signing request closed"),
        Err(_) => {
            state
                .signatures
                .lock()
                .expect("signatures mutex poisoned")
                .remove(&id);
            api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "wallet signing request timed out",
            )
        }
    }
}

fn authorization_error(state: &SharedState, headers: &HeaderMap) -> Option<Response> {
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        if origin != state.origin {
            return Some(api_error(StatusCode::FORBIDDEN, "invalid origin"));
        }
    }
    let expected = format!("Bearer {}", state.token);
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(expected.as_str())
    {
        return Some(api_error(StatusCode::UNAUTHORIZED, "invalid session"));
    }
    None
}

// The server itself only listens on 127.0.0.1. A non-browser sender has no
// Origin header, so possession of the per-process bearer token authenticates it.
fn loopback_authorization_error(state: &SharedState, headers: &HeaderMap) -> Option<Response> {
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        if origin != state.origin {
            return Some(api_error(StatusCode::FORBIDDEN, "invalid origin"));
        }
    }
    let expected = format!("Bearer {}", state.token);
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(expected.as_str())
    {
        return Some(api_error(StatusCode::UNAUTHORIZED, "invalid session"));
    }
    None
}

fn valid_host(headers: &HeaderMap, origin: &str) -> bool {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|host| origin == format!("http://{host}"))
        .unwrap_or(false)
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(ApiError {
            error: message.to_owned(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn challenge_binds_origin_address_nonce_and_expiry() {
        let challenge = login_challenge(
            "http://127.0.0.1:4123",
            "wallet-address",
            "random-nonce",
            1_000,
            121_000,
        );
        assert_eq!(
            String::from_utf8(challenge).unwrap(),
            "BeatEx browser wallet login\nVersion: 1\nOrigin: http://127.0.0.1:4123\nAddress: wallet-address\nNonce: random-nonce\nIssued At: 1000\nExpiration Time: 121000"
        );
    }

    #[test]
    fn verifies_a_valid_ed25519_wallet_signature() {
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let address = bs58::encode(signing_key.verifying_key().to_bytes()).into_string();
        let message = b"BeatEx browser wallet login";
        let signature = signing_key.sign(message);
        verify_wallet_signature(&address, message, &signature.to_bytes()).unwrap();
    }

    #[test]
    fn rejects_signature_for_a_different_message() {
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        let address = bs58::encode(signing_key.verifying_key().to_bytes()).into_string();
        let signature = signing_key.sign(b"expected");
        assert!(matches!(
            verify_wallet_signature(&address, b"changed", &signature.to_bytes()),
            Err(BridgeError::InvalidSignature)
        ));
    }

    #[test]
    fn profile_json_has_the_stable_public_shape() {
        let profile = WalletProfile {
            version: 1,
            wallet_name: "Phantom".to_owned(),
            address: "abc".to_owned(),
            supported_transaction_versions: vec![
                SupportedTransactionVersion::Legacy("legacy".to_owned()),
                SupportedTransactionVersion::Number(0),
                SupportedTransactionVersion::Number(1),
            ],
        };
        let value = serde_json::to_value(profile).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["walletName"], "Phantom");
        assert_eq!(
            value["supportedTransactionVersions"],
            serde_json::json!(["legacy", 0, 1])
        );
    }
}

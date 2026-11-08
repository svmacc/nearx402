//! x402 facilitator for the `near-policy` scheme.
//!
//! A vendor that sells over x402 does not need to read NEAR itself. It forwards
//! the client's `X-PAYMENT` and its own payment requirements to this service:
//!
//! - `GET  /supported`: schemes, networks, the token and the trusted policy code.
//! - `POST /verify`: checks the payment on chain, changes nothing.
//! - `POST /settle`: verifies again and redeems the payment, so the same policy
//!   log entry can never pay for a second request.
//!
//! In standard x402 schemes, `settle` is where the facilitator broadcasts the
//! transfer. Here the money has already moved: the agent's `pay` call made the
//! policy contract transfer the tokens itself. Settling only redeems the proof.
//!
//! The policy log alone is not proof of payment. On NEAR, redeploying a
//! contract keeps its storage, so an account could run other code to write a
//! fake `Paid` entry and then redeploy the trusted code. The facilitator
//! therefore also reads the payment transaction and requires the trusted token
//! contract's NEP-141 `ft_transfer` event, from the policy to the vendor, for
//! the logged amount, with memo `nearx402:<log id>`.
//!
//! `/verify` and `/settle` require `Authorization: Bearer <api key>` when API
//! keys are configured. Each key may only redeem payments to its own vendor
//! accounts, so nobody else can settle (and thereby burn) a vendor's payment
//! after seeing its proof on chain.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use near_api::types::{AccountId, CryptoHash};
use near_api::NetworkConfig;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::policy::{self, PolicyClient, Status};
use crate::x402::{
    self, FacilitatorRequest, PaymentPayload, PaymentRequirements, SettlementResponse, Supported,
    SupportedExtra, SupportedKind, VerifyResponse,
};

#[derive(Clone)]
pub struct FacilitatorConfig {
    pub network: NetworkConfig,
    /// JSON-RPC URL of the same network, used to read transaction outcomes.
    pub rpc_url: String,
    /// x402 network name, for example `near-sandbox` or `near-testnet`.
    pub network_name: String,
    /// The only NEP-141 token accepted.
    pub token_id: AccountId,
    /// Payments are accepted only from contracts running exactly this code.
    pub trusted_policy_code_hash: CryptoHash,
    /// Who may call `/verify` and `/settle`. Empty means anyone (local use only).
    pub api_keys: Vec<ApiKey>,
    /// Payments whose policy log entry is older than this are refused. Must
    /// exceed the vendors' nonce lifetime (5 minutes). It also bounds how long
    /// a redeemed payment must be remembered.
    pub max_payment_age: Duration,
    /// File that keeps redeemed payments across restarts. `None` keeps them
    /// in memory only, which is fine for the demo.
    pub redeemed_file: Option<PathBuf>,
}

/// Default for [`FacilitatorConfig::max_payment_age`].
pub const DEFAULT_MAX_PAYMENT_AGE: Duration = Duration::from_secs(600);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

type PaymentKey = (String, u32);

/// Redeemed payments, optionally backed by an append-only file with one
/// `redeemed_at<TAB>policy<TAB>log_id` line per payment. Entries older than
/// the maximum payment age are forgotten: such payments are refused anyway.
pub struct RedeemedStore {
    max_age: u64,
    entries: HashMap<PaymentKey, u64>,
    file: Option<File>,
}

impl RedeemedStore {
    pub fn in_memory(max_age: Duration) -> Self {
        Self {
            max_age: max_age.as_secs(),
            entries: HashMap::new(),
            file: None,
        }
    }

    /// Loads `path` (if it exists), drops expired lines, rewrites it compacted
    /// and keeps it open for appending.
    pub fn open(path: &Path, max_age: Duration, now: u64) -> Result<Self> {
        let mut store = Self::in_memory(max_age);
        if path.exists() {
            let f = File::open(path).with_context(|| format!("read {}", path.display()))?;
            for (n, line) in BufReader::new(f).lines().enumerate() {
                let line = line?;
                let mut parts = line.split('\t');
                let parsed = (|| {
                    let at: u64 = parts.next()?.parse().ok()?;
                    let policy = parts.next()?.to_string();
                    let id: u32 = parts.next()?.parse().ok()?;
                    Some((at, policy, id))
                })();
                let Some((at, policy, id)) = parsed else {
                    bail!("{}:{}: malformed line", path.display(), n + 1);
                };
                store.entries.insert((policy, id), at);
            }
        }
        store.prune(now);
        let tmp = path.with_extension("tmp");
        {
            let mut out = File::create(&tmp)?;
            for ((policy, id), at) in &store.entries {
                writeln!(out, "{at}\t{policy}\t{id}")?;
            }
            out.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        store.file = Some(OpenOptions::new().append(true).open(path)?);
        Ok(store)
    }

    pub fn contains(&self, key: &PaymentKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Records a redemption. `Ok(false)` if it was already redeemed. The line
    /// is flushed to disk before this returns, so a crash cannot forget it.
    pub fn insert(&mut self, key: PaymentKey, now: u64) -> Result<bool> {
        self.prune(now);
        if self.entries.contains_key(&key) {
            return Ok(false);
        }
        if let Some(f) = &mut self.file {
            writeln!(f, "{now}\t{}\t{}", key.0, key.1)?;
            f.sync_data()?;
        }
        self.entries.insert(key, now);
        Ok(true)
    }

    fn prune(&mut self, now: u64) {
        let max_age = self.max_age;
        self.entries
            .retain(|_, at| now.saturating_sub(*at) <= max_age);
    }
}

/// A vendor's credential and the accounts it may receive payments for.
#[derive(Clone, Debug)]
pub struct ApiKey {
    pub key: String,
    pub vendors: Vec<AccountId>,
}

impl ApiKey {
    /// Parses `KEY:vendor1,vendor2`, the format of `--api-key`.
    pub fn parse(s: &str) -> Result<Self> {
        let (key, vendors) = s
            .split_once(':')
            .ok_or_else(|| anyhow!("API key must look like KEY:vendor.near[,other.near]"))?;
        if key.len() < 16 {
            bail!("API key must be at least 16 characters");
        }
        let vendors = vendors
            .split(',')
            .map(|v| v.trim().parse::<AccountId>())
            .collect::<Result<Vec<_>, _>>()
            .context("API key vendor is not a NEAR account")?;
        if vendors.is_empty() {
            bail!("API key needs at least one vendor account");
        }
        Ok(Self {
            key: key.to_string(),
            vendors,
        })
    }
}

/// Random 32-byte key, hex encoded.
pub fn generate_api_key() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::rng().random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Credential {
    key_hash: [u8; 32],
    vendors: HashSet<AccountId>,
}

pub struct Facilitator {
    cfg: FacilitatorConfig,
    http: reqwest::Client,
    /// API keys by SHA-256, so a lookup does not compare secrets byte by byte.
    credentials: Vec<Credential>,
    /// `(policy contract, log id)` pairs that already paid for a request.
    redeemed: Mutex<RedeemedStore>,
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

impl Facilitator {
    pub fn new(cfg: FacilitatorConfig) -> Result<Self> {
        let redeemed = match &cfg.redeemed_file {
            Some(path) => RedeemedStore::open(path, cfg.max_payment_age, unix_now())?,
            None => RedeemedStore::in_memory(cfg.max_payment_age),
        };
        let credentials = cfg
            .api_keys
            .iter()
            .map(|k| Credential {
                key_hash: sha256(k.key.as_bytes()),
                vendors: k.vendors.iter().cloned().collect(),
            })
            .collect();
        Ok(Self {
            cfg,
            http: reqwest::Client::new(),
            credentials,
            redeemed: Mutex::new(redeemed),
        })
    }

    pub fn requires_auth(&self) -> bool {
        !self.credentials.is_empty()
    }

    /// Checks the bearer key and that it may redeem payments to `pay_to`.
    fn authorize(&self, headers: &HeaderMap, pay_to: &str) -> Result<(), (StatusCode, String)> {
        if !self.requires_auth() {
            return Ok(());
        }
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or((
                StatusCode::UNAUTHORIZED,
                "missing Authorization: Bearer <api key>".to_string(),
            ))?;
        let hash = sha256(token.as_bytes());
        let cred = self
            .credentials
            .iter()
            .find(|c| c.key_hash == hash)
            .ok_or((StatusCode::UNAUTHORIZED, "unknown API key".to_string()))?;
        let allowed = pay_to
            .parse::<AccountId>()
            .map(|a| cred.vendors.contains(&a))
            .unwrap_or(false);
        if !allowed {
            return Err((
                StatusCode::FORBIDDEN,
                format!("this API key may not redeem payments to {pay_to}"),
            ));
        }
        Ok(())
    }

    pub fn supported(&self) -> Supported {
        Supported {
            kinds: vec![SupportedKind {
                x402_version: x402::VERSION,
                scheme: x402::SCHEME.into(),
                network: self.cfg.network_name.clone(),
                extra: SupportedExtra {
                    asset: self.cfg.token_id.to_string(),
                    policy_code_hash: self.cfg.trusted_policy_code_hash.to_string(),
                },
            }],
        }
    }

    pub async fn verify(&self, req: &FacilitatorRequest) -> VerifyResponse {
        let payer = Some(req.payment_payload.payload.policy_contract.clone());
        match self
            .check(&req.payment_payload, &req.payment_requirements)
            .await
        {
            Ok(()) => VerifyResponse {
                is_valid: true,
                invalid_reason: None,
                payer,
            },
            Err(e) => VerifyResponse {
                is_valid: false,
                invalid_reason: Some(public_reason(&e, req)),
                payer,
            },
        }
    }

    pub async fn settle(&self, req: &FacilitatorRequest) -> SettlementResponse {
        let proof = &req.payment_payload.payload;
        let mut resp = SettlementResponse {
            success: false,
            error_reason: None,
            network: self.cfg.network_name.clone(),
            transaction: proof.tx_hash.clone(),
            payer: proof.policy_contract.clone(),
        };
        if let Err(e) = self
            .check(&req.payment_payload, &req.payment_requirements)
            .await
        {
            resp.error_reason = Some(public_reason(&e, req));
            return resp;
        }
        // Two concurrent settles of the same proof: only the first insert wins.
        let key = (proof.policy_contract.clone(), proof.log_id);
        match self.redeemed.lock().await.insert(key, unix_now()) {
            Ok(true) => resp.success = true,
            Ok(false) => resp.error_reason = Some("payment already redeemed".into()),
            Err(e) => {
                resp.error_reason = Some(public_reason(
                    &e.context("could not record redemption"),
                    req,
                ))
            }
        }
        resp
    }

    /// All checks for one payment. Cheap local checks run before any RPC call.
    async fn check(&self, payment: &PaymentPayload, req: &PaymentRequirements) -> Result<()> {
        let cfg = &self.cfg;
        if payment.scheme != x402::SCHEME || req.scheme != x402::SCHEME {
            bail!(
                "unsupported scheme, this facilitator handles {}",
                x402::SCHEME
            );
        }
        if payment.network != cfg.network_name || req.network != cfg.network_name {
            bail!(
                "unsupported network, this facilitator handles {}",
                cfg.network_name
            );
        }
        if req.asset != cfg.token_id.as_str() {
            bail!("unsupported asset {}", req.asset);
        }
        if req.extra.policy_code_hash != cfg.trusted_policy_code_hash.to_string() {
            bail!("requirements name a policy code hash this facilitator does not trust");
        }
        let proof = &payment.payload;
        if proof.nonce != req.extra.nonce {
            bail!("payment is for a different nonce");
        }
        let memo_expected = x402::payment_memo(&proof.nonce, &proof.receipt_secret)?;
        let pay_to: AccountId = req.pay_to.parse().context("payTo is not a NEAR account")?;
        let price: u128 = req
            .max_amount_required
            .parse()
            .context("maxAmountRequired is not an integer")?;
        if self
            .redeemed
            .lock()
            .await
            .contains(&(proof.policy_contract.clone(), proof.log_id))
        {
            bail!("payment already redeemed");
        }

        let policy_id: AccountId = proof
            .policy_contract
            .parse()
            .context("policyContract is not a NEAR account")?;
        let client = PolicyClient::new(cfg.network.clone(), policy_id);
        if client.code_hash().await? != cfg.trusted_policy_code_hash {
            bail!("payer contract does not run the trusted policy code");
        }
        if client.token_id().await? != cfg.token_id {
            bail!("payer contract pays in a different token");
        }
        let Some(entry) = client.log_entry(proof.log_id).await? else {
            bail!("no such policy log entry");
        };
        if entry.status != Status::Paid {
            bail!("policy log entry is {:?}, not Paid", entry.status);
        }
        let paid_at = entry.timestamp / 1_000_000_000;
        if unix_now().saturating_sub(paid_at) > cfg.max_payment_age.as_secs() {
            bail!(
                "payment is older than {} s and can no longer be redeemed",
                cfg.max_payment_age.as_secs()
            );
        }
        if entry.vendor != pay_to {
            bail!("payment went to {}, not {pay_to}", entry.vendor);
        }
        if entry.amount < price {
            bail!(
                "paid {} but the price is {}",
                policy::usd(entry.amount),
                policy::usd(price)
            );
        }
        // Everything else in the proof can be read from the chain. Only the
        // payer knows the secret behind the memo's commitment.
        if entry.memo.as_deref() != Some(memo_expected.as_str()) {
            bail!("policy log entry does not carry this nonce and receipt secret");
        }

        // The log says Paid; now require the token contract to say so too.
        let outcome = self
            .tx_outcome(&proof.tx_hash, &entry.agent_id)
            .await
            .context("read payment transaction")?;
        let memo = format!("nearx402:{}", proof.log_id);
        if !has_ft_transfer(
            &outcome,
            &cfg.token_id,
            &client.contract_id,
            &pay_to,
            entry.amount,
            &memo,
        ) {
            bail!(
                "transaction {} has no successful {} ft_transfer from {} to {pay_to} with memo {memo}",
                proof.tx_hash,
                cfg.token_id,
                client.contract_id
            );
        }
        Ok(())
    }

    /// Final outcome of transaction `tx_hash` signed by `sender`, as returned by
    /// the NEAR JSON-RPC `tx` method.
    async fn tx_outcome(&self, tx_hash: &str, sender: &AccountId) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "nearx402-facilitator",
            "method": "tx",
            "params": { "tx_hash": tx_hash, "sender_account_id": sender, "wait_until": "FINAL" },
        });
        let resp: Value = self
            .http
            .post(&self.cfg.rpc_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            bail!("RPC error: {err}");
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| anyhow!("RPC response has no result"))
    }
}

/// A refusal reason as it leaves the facilitator. The vendor passes it on in its
/// 402 body to whoever asked, so it must not carry the RPC URL's path, query or
/// credentials, which can hold an API key, nor the claim's receipt secret. It is
/// also bounded. Only the text is shortened; no check changes.
fn public_reason(e: &anyhow::Error, req: &FacilitatorRequest) -> String {
    use crate::rejection::{classify, redact, Cause};
    let full = format!("{e:#}");
    let line = redact(&full, &[&req.payment_payload.payload.receipt_secret]);
    // Said first, so that the cut to a bounded length cannot drop it.
    if classify(&full) == Cause::RpcRateLimited {
        format!("RPC rate limit, too many requests: {line}")
    } else {
        line
    }
}

/// True if a receipt executed by `token` succeeded and logged a NEP-141
/// `ft_transfer` event moving exactly `amount` from `from` to `to` with `memo`.
fn has_ft_transfer(
    tx: &Value,
    token: &AccountId,
    from: &AccountId,
    to: &AccountId,
    amount: u128,
    memo: &str,
) -> bool {
    let Some(receipts) = tx.get("receipts_outcome").and_then(Value::as_array) else {
        return false;
    };
    receipts.iter().any(|r| {
        let o = &r["outcome"];
        let succeeded = o["status"].get("SuccessValue").is_some()
            || o["status"].get("SuccessReceiptId").is_some();
        if o["executor_id"].as_str() != Some(token.as_str()) || !succeeded {
            return false;
        }
        let Some(logs) = o["logs"].as_array() else {
            return false;
        };
        logs.iter()
            .filter_map(|l| l.as_str()?.strip_prefix("EVENT_JSON:"))
            .filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .filter(|e| e["standard"] == "nep141" && e["event"] == "ft_transfer")
            .filter_map(|e| e["data"].as_array().cloned())
            .flatten()
            .any(|d| {
                d["old_owner_id"].as_str() == Some(from.as_str())
                    && d["new_owner_id"].as_str() == Some(to.as_str())
                    && d["amount"].as_str() == Some(amount.to_string().as_str())
                    && d["memo"].as_str() == Some(memo)
            })
    })
}

/// HTTP routes: `/supported`, `/verify`, `/settle`. Mount at `/` for a
/// standalone facilitator or nest under a prefix.
pub fn router(facilitator: Arc<Facilitator>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { "nearx402 x402 facilitator (near-policy). See /supported." }),
        )
        .route("/supported", get(supported))
        .route("/verify", post(verify))
        .route("/settle", post(settle))
        .with_state(facilitator)
}

async fn supported(State(f): State<Arc<Facilitator>>) -> Json<Supported> {
    Json(f.supported())
}

async fn verify(
    State(f): State<Arc<Facilitator>>,
    headers: HeaderMap,
    Json(req): Json<FacilitatorRequest>,
) -> Response {
    if let Err(e) = f.authorize(&headers, &req.payment_requirements.pay_to) {
        return e.into_response();
    }
    Json(f.verify(&req).await).into_response()
}

async fn settle(
    State(f): State<Arc<Facilitator>>,
    headers: HeaderMap,
    Json(req): Json<FacilitatorRequest>,
) -> Response {
    if let Err(e) = f.authorize(&headers, &req.payment_requirements.pay_to) {
        return e.into_response();
    }
    Json(f.settle(&req).await).into_response()
}

/// What a vendor uses to talk to a facilitator over HTTP.
#[derive(Clone)]
pub struct FacilitatorClient {
    base: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl FacilitatorClient {
    /// `base` is the facilitator URL, for example `http://127.0.0.1:8403`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            api_key: None,
            http: reqwest::Client::new(),
        }
    }

    /// Sends `Authorization: Bearer <key>` on `/verify` and `/settle`.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    pub async fn supported(&self) -> Result<Supported> {
        let r = self
            .http
            .get(format!("{}/supported", self.base))
            .send()
            .await
            .context("facilitator /supported")?;
        Ok(r.error_for_status()?.json().await?)
    }

    pub async fn verify(&self, req: &FacilitatorRequest) -> Result<VerifyResponse> {
        self.post("verify", req).await
    }

    pub async fn settle(&self, req: &FacilitatorRequest) -> Result<SettlementResponse> {
        self.post("settle", req).await
    }

    async fn post<T: for<'de> serde::Deserialize<'de>>(
        &self,
        path: &str,
        req: &FacilitatorRequest,
    ) -> Result<T> {
        let mut r = self.http.post(format!("{}/{path}", self.base)).json(req);
        if let Some(key) = &self.api_key {
            r = r.bearer_auth(key);
        }
        let r = r
            .send()
            .await
            .with_context(|| format!("facilitator /{path}"))?;
        let status = r.status();
        if !status.is_success() {
            let text = r.text().await.unwrap_or_default();
            return Err(anyhow!("facilitator /{path} returned {status}: {text}"));
        }
        Ok(r.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::{NearPolicyProof, RequirementsExtra};

    fn facilitator() -> Facilitator {
        Facilitator::new(FacilitatorConfig {
            // Never contacted: every case below fails before any RPC call.
            network: NetworkConfig::testnet(),
            rpc_url: "http://127.0.0.1:9".into(),
            network_name: "near-testnet".into(),
            token_id: "usdc.testnet".parse().unwrap(),
            trusted_policy_code_hash: CryptoHash::hash(b"policy"),
            api_keys: vec![],
            max_payment_age: DEFAULT_MAX_PAYMENT_AGE,
            redeemed_file: None,
        })
        .unwrap()
    }

    #[test]
    fn redeemed_payments_survive_a_restart_until_they_expire() {
        let path =
            std::env::temp_dir().join(format!("nearx402-redeemed-{}.tsv", rand::random::<u64>()));
        let age = Duration::from_secs(600);
        let t = 1_800_000_000;
        let a = ("policy.testnet".to_string(), 1);
        let b = ("policy.testnet".to_string(), 2);
        {
            let mut s = RedeemedStore::open(&path, age, t).unwrap();
            assert!(s.insert(a.clone(), t).unwrap());
            assert!(!s.insert(a.clone(), t).unwrap());
            assert!(s.insert(b.clone(), t + 500).unwrap());
        }
        // Restarted: both still known.
        let s = RedeemedStore::open(&path, age, t + 550).unwrap();
        assert!(s.contains(&a) && s.contains(&b));
        drop(s);
        // Later: `a` is past the maximum payment age and is compacted away.
        let s = RedeemedStore::open(&path, age, t + 700).unwrap();
        assert!(!s.contains(&a) && s.contains(&b));
        drop(s);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);

        std::fs::write(&path, "garbage\n").unwrap();
        assert!(RedeemedStore::open(&path, age, t).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn api_keys_are_checked_and_bound_to_vendors() {
        let mut cfg = facilitator().cfg;
        cfg.api_keys = vec![ApiKey::parse("0123456789abcdef-data:data-api.testnet").unwrap()];
        let f = Facilitator::new(cfg).unwrap();
        let with = |key: Option<&str>| {
            let mut h = HeaderMap::new();
            if let Some(k) = key {
                h.insert(
                    header::AUTHORIZATION,
                    format!("Bearer {k}").parse().unwrap(),
                );
            }
            h
        };
        assert!(f
            .authorize(&with(Some("0123456789abcdef-data")), "data-api.testnet")
            .is_ok());
        let code = |r: Result<(), (StatusCode, String)>| r.unwrap_err().0;
        assert_eq!(
            code(f.authorize(&with(None), "data-api.testnet")),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            code(f.authorize(&with(Some("wrong-key-wrong-key")), "data-api.testnet")),
            StatusCode::UNAUTHORIZED
        );
        // A valid key cannot redeem another vendor's payments.
        assert_eq!(
            code(f.authorize(&with(Some("0123456789abcdef-data")), "gpu-cloud.testnet")),
            StatusCode::FORBIDDEN
        );
        assert!(ApiKey::parse("short:data-api.testnet").is_err());
        assert!(ApiKey::parse("0123456789abcdef-no-vendors").is_err());
    }

    fn request(f: &Facilitator) -> FacilitatorRequest {
        let requirements = PaymentRequirements {
            scheme: x402::SCHEME.into(),
            network: "near-testnet".into(),
            max_amount_required: "50000".into(),
            resource: "/data-api/market-data".into(),
            description: "test".into(),
            mime_type: "application/json".into(),
            pay_to: "data-api.testnet".into(),
            asset: "usdc.testnet".into(),
            max_timeout_seconds: 300,
            extra: RequirementsExtra {
                nonce: "n1".into(),
                policy_code_hash: f.cfg.trusted_policy_code_hash.to_string(),
            },
        };
        FacilitatorRequest {
            x402_version: x402::VERSION,
            payment_payload: PaymentPayload {
                x402_version: x402::VERSION,
                scheme: x402::SCHEME.into(),
                network: "near-testnet".into(),
                payload: NearPolicyProof {
                    policy_contract: "policy.testnet".into(),
                    log_id: 0,
                    nonce: "n1".into(),
                    tx_hash: "hash".into(),
                    receipt_secret: "ab".repeat(32),
                },
            },
            payment_requirements: requirements,
        }
    }

    async fn reason(f: &Facilitator, req: &FacilitatorRequest) -> String {
        let r = f.verify(req).await;
        assert!(!r.is_valid);
        r.invalid_reason.unwrap()
    }

    /// The failure seen on testnet on 8 Nov 2026: the RPC answered 429. The
    /// refusal must say so, must not read as a refusal of the payment, and must
    /// not carry the RPC URL's credentials or the receipt secret.
    #[tokio::test]
    async fn an_rpc_that_rate_limits_is_reported_as_such() {
        use crate::rejection::{explain, Cause};
        use crate::x402::PaymentRequired;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new().fallback(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests")
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let url = format!("http://user:pw@{addr}/v1/key/SECRETPATH?apiKey=SECRETKEY");
        let mut cfg = facilitator().cfg;
        cfg.network = NetworkConfig::from_rpc_url("custom", url.parse().unwrap());
        cfg.rpc_url = url;
        let f = Facilitator::new(cfg).unwrap();
        let req = request(&f);
        let secret = req.payment_payload.payload.receipt_secret.clone();

        let why = reason(&f, &req).await;
        assert!(hits.load(Ordering::SeqCst) >= 1, "the RPC was never asked");
        // What leaves the facilitator, and so reaches anyone the vendor answers.
        for leak in ["SECRETKEY", "SECRETPATH", "apiKey", "pw@", secret.as_str()] {
            assert!(!why.contains(leak), "{leak} leaves the facilitator: {why}");
        }
        let settled = f.settle(&req).await;
        assert!(!settled.success);
        let settle_why = settled.error_reason.unwrap();
        for leak in ["SECRETKEY", "SECRETPATH", "apiKey", "pw@", secret.as_str()] {
            assert!(
                !settle_why.contains(leak),
                "{leak} leaves settle: {settle_why}"
            );
        }
        let body = serde_json::to_string(&PaymentRequired {
            x402_version: x402::VERSION,
            error: Some(why.clone()),
            accepts: vec![],
        })
        .unwrap();
        let r = explain(402, &body, &[&secret]);
        eprintln!("facilitator's reason: {why}\nprinted as: {r}");
        assert_eq!(r.cause, Cause::RpcRateLimited, "reason was: {why}");
        assert!(!r.cause.is_about_the_payment());
        for leak in ["SECRETKEY", "SECRETPATH", "apiKey", "pw@", secret.as_str()] {
            assert!(!r.detail.contains(leak), "{leak} in {}", r.detail);
        }
        // A refusal for a rate limit redeems nothing.
        assert!(!f
            .redeemed
            .lock()
            .await
            .contains(&("policy.testnet".into(), 0)));
        server.abort();
    }

    #[tokio::test]
    async fn rejects_requests_it_cannot_handle_without_touching_the_chain() {
        let f = facilitator();

        let mut r = request(&f);
        r.payment_payload.scheme = "exact".into();
        assert!(reason(&f, &r).await.contains("unsupported scheme"));

        let mut r = request(&f);
        r.payment_payload.payload.receipt_secret = "not-a-secret".into();
        assert!(reason(&f, &r).await.contains("receipt secret"));

        let mut r = request(&f);
        r.payment_requirements.network = "near-mainnet".into();
        assert!(reason(&f, &r).await.contains("unsupported network"));

        let mut r = request(&f);
        r.payment_requirements.asset = "evil-usdc.testnet".into();
        assert!(reason(&f, &r).await.contains("unsupported asset"));

        let mut r = request(&f);
        r.payment_requirements.extra.policy_code_hash = CryptoHash::hash(b"other").to_string();
        assert!(reason(&f, &r).await.contains("does not trust"));

        let mut r = request(&f);
        r.payment_payload.payload.nonce = "n2".into();
        assert!(reason(&f, &r).await.contains("different nonce"));
    }

    fn tx_with(executor: &str, status: Value, event: Value) -> Value {
        json!({ "receipts_outcome": [
            { "outcome": { "executor_id": "policy.testnet", "logs": [], "status": { "SuccessValue": "" } } },
            { "outcome": {
                "executor_id": executor,
                "logs": [format!("EVENT_JSON:{event}")],
                "status": status,
            } },
        ] })
    }

    fn transfer_event(amount: &str, memo: &str) -> Value {
        json!({ "standard": "nep141", "version": "1.0.0", "event": "ft_transfer", "data": [
            { "old_owner_id": "policy.testnet", "new_owner_id": "data-api.testnet",
              "amount": amount, "memo": memo }
        ] })
    }

    fn matches(tx: &Value) -> bool {
        has_ft_transfer(
            tx,
            &"usdc.testnet".parse().unwrap(),
            &"policy.testnet".parse().unwrap(),
            &"data-api.testnet".parse().unwrap(),
            50_000,
            "nearx402:3",
        )
    }

    #[test]
    fn finds_the_token_transfer_event() {
        let ok = json!({ "SuccessValue": "" });
        assert!(matches(&tx_with(
            "usdc.testnet",
            ok.clone(),
            transfer_event("50000", "nearx402:3")
        )));
        // Same event logged by a contract that is not the trusted token.
        assert!(!matches(&tx_with(
            "fake-usdc.testnet",
            ok.clone(),
            transfer_event("50000", "nearx402:3")
        )));
        // Wrong amount, or a transfer made for another log entry.
        assert!(!matches(&tx_with(
            "usdc.testnet",
            ok.clone(),
            transfer_event("1", "nearx402:3")
        )));
        assert!(!matches(&tx_with(
            "usdc.testnet",
            ok,
            transfer_event("50000", "nearx402:4")
        )));
        // The token receipt failed.
        assert!(!matches(&tx_with(
            "usdc.testnet",
            json!({ "Failure": {} }),
            transfer_event("50000", "nearx402:3")
        )));
        assert!(!matches(&json!({})));
    }

    #[test]
    fn supported_advertises_token_and_code_hash() {
        let f = facilitator();
        let s = f.supported();
        assert_eq!(s.kinds.len(), 1);
        assert_eq!(s.kinds[0].scheme, x402::SCHEME);
        assert_eq!(s.kinds[0].extra.asset, "usdc.testnet");
    }
}

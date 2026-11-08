//! The demo's paid-but-not-served path, end to end: the real `demo-zk` binary
//! against a local sandbox, with the facilitator's RPC rate limiting the one
//! read that only the facilitator makes. The 8 Nov testnet run is assumed to
//! have failed this way, and its 402 body was lost. Here the run must stop with
//! the reason, the payment's log id and transaction, and retry nothing.
//!
//! The RPC proxy answers 429 to any `query` that calls `get_log_entry`. The demo
//! itself never calls it, so everything the demo reads still works, and only
//! the facilitator's `/verify` fails. The proxy's URL carries credentials, a
//! path and a query, as a paid RPC's URL does, and none of them may appear in
//! what the demo prints.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use nearx402::deployment::{self, PolicyParams};
use nearx402::policy::Status;
use nearx402::world::{res_dir, World};
use nearx402::zk::chain_agent;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use zx402_zk::store::{Reconciled, WalletFile};

/// What the demo pays for the market data.
const PRICE: u128 = 50_000;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sits between everyone and the sandbox's RPC.
#[derive(Clone)]
struct Proxy {
    upstream: String,
    http: reqwest::Client,
    /// Calls of `get_log_entry`, each answered 429.
    refused: Arc<AtomicUsize>,
    /// Every other call, forwarded as it is.
    forwarded: Arc<AtomicUsize>,
}

async fn proxy(State(p): State<Proxy>, body: Bytes) -> Response {
    if String::from_utf8_lossy(&body).contains("get_log_entry") {
        p.refused.fetch_add(1, Ordering::SeqCst);
        return (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
    }
    p.forwarded.fetch_add(1, Ordering::SeqCst);
    let upstream = p
        .http
        .post(&p.upstream)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    match upstream {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = r.bytes().await.unwrap_or_default();
            (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

/// Returns the proxy and its URL. The URL has credentials, a path and a query,
/// which the proxy ignores: they exist to show up if the demo prints them.
async fn start_proxy(upstream: String) -> (Proxy, String) {
    let p = Proxy {
        upstream,
        http: reqwest::Client::new(),
        refused: Arc::default(),
        forwarded: Arc::default(),
    };
    let app = Router::new().fallback(proxy).with_state(p.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        p,
        format!("http://user:pw@{addr}/v1/key/SECRETPATH?apiKey=SECRETKEY"),
    )
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// The longest run of hex digits in `s`. A receipt secret is 64.
fn longest_hex_run(s: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// The value after `label` on the demo's own output line for it.
fn printed(out: &str, label: &str) -> Option<String> {
    out.lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix(label))
        .map(|rest| rest.trim().to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_refused_for_a_rate_limit_stops_the_run_and_pays_nothing_again() -> Result<()> {
    let w = World::start_bare().await?;
    let dir = Temp(std::env::temp_dir().join(format!("zx402-unserved-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0)?;
    let d = &dir.0;

    // The agent, its key file, and finance's setup, as on testnet.
    let (agent_id, agent_key) = w.create_zk_agent_with_key("zk-agent").await?;
    let key_file = d.join("agent-key.json");
    std::fs::write(
        &key_file,
        json!({
            "account_id": agent_id,
            "public_key": agent_key.public_key().to_string(),
            "private_key": agent_key.to_string(),
        })
        .to_string(),
    )?;
    let data = w.vendor("data-api").account_id.clone();
    let gpu = w.vendor("gpu-cloud").account_id.clone();
    let setup = d.join("setup");
    let (paths, _pk) = deployment::init(
        &setup,
        &PolicyParams {
            contract: w.policy_id.clone(),
            agent: agent_id.clone(),
            cap: 1_000_000,
            per_call: 500_000,
            vendors: vec![data.clone(), gpu.clone()],
        },
    )?;
    w.owner_call(
        "set_zk_verifying_key",
        read_json(&paths.verifying_key_args)?,
    )
    .await?;
    w.owner_call("set_zk_agent", read_json(&paths.agent_args)?)
        .await?;

    let (proxy, rpc_url) = start_proxy(w.rpc_url.clone()).await;
    let config = d.join("config.json");
    std::fs::write(
        &config,
        json!({
            "rpc_url": rpc_url,
            "network_name": "near-sandbox",
            "policy": w.policy_id,
            "token": w.token_id,
            "agent": agent_id,
            "agent_key_file": key_file,
            "vendor": data,
            "outside_recipient": w.attacker_id,
            "agent_dir": setup.join("agent"),
            "policy_wasm": res_dir().join("policy.wasm"),
        })
        .to_string(),
    )?;

    let client = w.policy();
    let vendor_before = w.token_balance(&data).await?;
    let treasury_before = client.balance().await?;
    let log_before = client.log(0, 100).await?.len();

    let started = Instant::now();
    let o = Command::new(env!("CARGO_BIN_EXE_demo-zk"))
        .args(["--network", "testnet", "--port", "0", "--config"])
        .arg(&config)
        .output()
        .context("run demo-zk")?;
    let took = started.elapsed();
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    let refused = proxy.refused.load(Ordering::SeqCst);
    let forwarded = proxy.forwarded.load(Ordering::SeqCst);
    println!(
        "---------- demo-zk, exit {:?} after {:.1} s; RPC proxy refused {refused} get_log_entry \
         call(s) with 429 and forwarded {forwarded} ----------\n{stdout}\n{stderr}",
        o.status.code(),
        took.as_secs_f32()
    );

    // ---------- what the run says ----------
    assert_eq!(o.status.code(), Some(1), "the run must stop with an error");
    assert!(
        stdout.contains("PROOF VERIFIED"),
        "the payment was not made"
    );
    let tx_hash = printed(&stdout, "transaction").context("the transaction line")?;
    assert!(
        stdout.contains("the contract checked the proof, then paid: log #0"),
        "{stdout}"
    );
    // The observer's refusal is read for its reason, and not taken as the check.
    assert!(
        stdout.contains(
            "refused, but not for the secret: HTTP 402, the facilitator's RPC is rate limiting (429)"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("This does not show the secret's protection"),
        "{stdout}"
    );
    // The agent's own claim: the run stops, naming the payment and the cause.
    let expected = format!(
        "paid but not served: paid on chain as log #0, transaction {tx_hash}, and the vendor \
         refused the claim: HTTP 402, the facilitator's RPC is rate limiting (429)"
    );
    assert!(
        stderr.contains(&expected),
        "expected {expected:?} in: {stderr}"
    );
    assert!(
        stderr.contains("Do not pay again; nothing was retried"),
        "{stderr}"
    );
    for not_reached in ["HTTP 200", "replay", "[2]", "REJECTED", "[3]"] {
        assert!(!stdout.contains(not_reached), "{not_reached} in: {stdout}");
    }

    // ---------- what the run must not say ----------
    let all = format!("{stdout}\n{stderr}");
    for leak in ["SECRETKEY", "SECRETPATH", "apiKey", "pw@", "user:"] {
        assert!(!all.contains(leak), "{leak} printed: {all}");
    }
    assert!(
        longest_hex_run(&all) < 64,
        "a 64-hex value, the length of a receipt secret or nonce, was printed: {all}"
    );
    for private in [gpu.as_str(), "1.00 mUSDC", "0.50 mUSDC"] {
        assert!(!all.contains(private), "{private} printed: {all}");
    }
    // The RPC is still named, by scheme and host only.
    let chain = printed(&stdout, "chain").context("the chain line")?;
    assert!(chain.contains("http://127.0.0.1:"), "{chain}");

    // ---------- what the chain says: one payment, nothing more ----------
    let log = client.log(0, 100).await?;
    assert_eq!(log.len(), log_before + 1, "exactly one new log entry");
    let entry = log.last().context("the payment")?;
    assert_eq!(entry.id, 0);
    assert_eq!(entry.status, Status::Paid);
    assert_eq!(entry.vendor, data);
    assert_eq!(entry.amount, PRICE);
    assert!(entry.memo.is_some(), "the memo commits to the secret");
    assert_eq!(w.token_balance(&data).await?, vendor_before + PRICE);
    assert_eq!(client.balance().await?, treasury_before - PRICE);
    assert_eq!(w.token_balance(&w.attacker_id).await?, 0);
    let view = client
        .zk_agent(&agent_id)
        .await?
        .context("the agent is registered")?;
    assert_eq!(view.blocked_today, 0, "the run stopped before scene 2");
    assert!(!view.frozen);
    assert_eq!(view.pending_log_id, None, "nothing is in flight");

    // The wallet file agrees with the chain, and counted the payment once.
    let mut file = WalletFile::open(&paths.wallet)?;
    assert_eq!(file.reconcile(&chain_agent(&view)?)?, Reconciled::Current);
    assert_eq!(file.wallet.state.spent, PRICE);

    // Two claims reached the facilitator, each refused at the log entry read.
    // near-api retries a read on its own, so the count is at least two.
    assert!(
        refused >= 2,
        "the facilitator read the log entry {refused} time(s)"
    );
    assert!(forwarded > 0, "the demo's own reads went through");
    Ok(())
}

//! Captures real `EXPERIMENTAL_tx_status` responses from a local sandbox, as the
//! base fixtures for the reconcile checker's tests. Run it by hand:
//!
//! ```text
//! cargo test -p nearx402 --test reconcile_capture -- --ignored
//! ```
//!
//! It writes `tests/fixtures/reconcile/*.json`. The `pay_zk` action's arguments
//! are cut down to `vendor`, `amount` and `memo`: the proof and commitments are
//! not needed by the checker and are not kept.

mod common;

use std::path::PathBuf;

use ark_std::rand::rngs::OsRng;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use common::rpc;
use nearx402::deployment::{self, PolicyParams};
use nearx402::policy::{PayZkResult, Status};
use nearx402::world::World;
use nearx402::zk::pay_args;
use serde_json::{json, Value};
use zx402_zk::store::WalletFile;

const AMOUNT: u128 = 50_000;

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reconcile")
}

async fn tx_status(http: &reqwest::Client, w: &World, hash: &str, sender: &str) -> Value {
    rpc(
        http,
        &w.rpc_url,
        "EXPERIMENTAL_tx_status",
        json!({ "tx_hash": hash, "sender_account_id": sender, "wait_until": "FINAL" }),
    )
    .await
}

/// Drops the proof and commitments from `pay_zk` actions, keeping what the
/// checker reads. The transaction's signature no longer matches its arguments,
/// which the checker does not check.
fn sanitize_pay_zk(status: &mut Value) {
    fn cut(actions: Option<&mut Vec<Value>>) {
        for a in actions.into_iter().flatten() {
            let Some(call) = a.get_mut("FunctionCall").filter(|c| c.is_object()) else {
                continue;
            };
            if call["method_name"] != "pay_zk" {
                continue;
            }
            let args: Value =
                serde_json::from_slice(&B64.decode(call["args"].as_str().unwrap()).unwrap())
                    .unwrap();
            let kept =
                json!({ "vendor": args["vendor"], "amount": args["amount"], "memo": args["memo"] });
            call["args"] = json!(B64.encode(kept.to_string()));
        }
    }
    cut(status["transaction"]["actions"].as_array_mut());
    for r in status["receipts"].as_array_mut().unwrap() {
        if let Some(action) = r.get_mut("receipt").and_then(|x| x.get_mut("Action")) {
            cut(action.get_mut("actions").and_then(Value::as_array_mut));
        }
    }
}

async fn save(w: &World, name: &str, sender: &str, mut status: Value, log_id: u32) {
    if status["transaction"]["actions"]
        .to_string()
        .contains("pay_zk")
    {
        sanitize_pay_zk(&mut status);
    }
    let entry = w.policy().log_entry(log_id).await.unwrap().unwrap();
    let fixture = json!({
        "policy": w.policy_id,
        "token": w.token_id,
        "sender": sender,
        "log_id": log_id,
        "entry": entry,
        "tx_status": status,
    });
    std::fs::create_dir_all(out_dir()).unwrap();
    std::fs::write(
        out_dir().join(format!("{name}.json")),
        serde_json::to_string_pretty(&fixture).unwrap() + "\n",
    )
    .unwrap();
    println!("wrote {name}.json");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes fixtures; run by hand"]
async fn capture_fixtures() {
    let w = World::start().await.expect("sandbox world");
    let http = reqwest::Client::new();
    let agent = w.agent_id.to_string();

    // 1. A payment whose transfer succeeded.
    let vendor = w.vendor("data-api").account_id.clone();
    let r = w
        .policy()
        .pay(&w.agent_id, w.agent_signer(), &vendor, AMOUNT, None)
        .await
        .unwrap();
    assert_eq!(r.outcome.status, Status::Paid);
    let s = tx_status(&http, &w, &r.tx_hash, &agent).await;
    save(&w, "pay_transfer_ok", &agent, s, r.outcome.log_id).await;

    // 2. A payment whose transfer failed: the vendor is not registered with the token.
    let unregistered = w.create_unregistered_account("no-token").await.unwrap();
    w.approve_vendor(&unregistered).await.unwrap();
    let r = w
        .policy()
        .pay(&w.agent_id, w.agent_signer(), &unregistered, AMOUNT, None)
        .await
        .unwrap();
    assert_eq!(r.outcome.status, Status::Failed);
    let s = tx_status(&http, &w, &r.tx_hash, &agent).await;
    save(&w, "pay_transfer_failed", &agent, s, r.outcome.log_id).await;

    // 3. A private payment whose transfer succeeded.
    let (zk_agent, signer) = w.create_zk_agent("zk-agent").await.unwrap();
    let dir = std::env::temp_dir().join(format!("zx402-capture-{}", std::process::id()));
    let (paths, pk) = deployment::init(
        &dir,
        &PolicyParams {
            contract: w.policy_id.clone(),
            agent: zk_agent.clone(),
            cap: 1_000_000,
            per_call: 500_000,
            vendors: vec![vendor.clone()],
        },
    )
    .unwrap();
    let read = |p: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    w.owner_call("set_zk_verifying_key", read(&paths.verifying_key_args))
        .await
        .unwrap();
    w.owner_call("set_zk_agent", read(&paths.agent_args))
        .await
        .unwrap();
    let mut file = WalletFile::open(&paths.wallet).unwrap();
    let day = w.policy().day().await.unwrap();
    let prepared = file
        .wallet
        .prepare(&pk, vendor.as_str(), AMOUNT, "capture", day, &mut OsRng)
        .unwrap()
        .unwrap();
    file.begin(prepared.next).unwrap();
    let args = pay_args(&vendor, AMOUNT, "capture", &prepared, day);
    let PayZkResult::Executed { receipt, .. } =
        w.policy().pay_zk(&zk_agent, signer, &args).await.unwrap()
    else {
        panic!("pay_zk reverted");
    };
    assert_eq!(receipt.outcome.status, Status::Paid);
    let s = tx_status(&http, &w, &receipt.tx_hash, zk_agent.as_str()).await;
    save(
        &w,
        "pay_zk_transfer_ok",
        zk_agent.as_str(),
        s,
        receipt.outcome.log_id,
    )
    .await;
    drop(file);
    let _ = std::fs::remove_dir_all(&dir);
}

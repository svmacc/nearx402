//! `reconcile_payment` on a local NEAR sandbox, with the real contracts.
//!
//! A callback cannot be made to fail on demand, so the test builds the state a
//! lost callback would leave, by patching storage: the entry goes back to
//! `Pending`, is aged, and its amount goes back into `reserved`. Everything after
//! that runs for real, including the read of the token balance and the callback.
//! Needs `scripts/build-contracts.sh` to have run.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use near_api::types::{AccountId, NearGas};
use near_api::{Contract, Signer};
use near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY;
use nearx402::policy::{LogEntry, Status};
use nearx402::world::World;
use serde_json::{json, Value};

const DAY_NANOS: u64 = 86_400 * 1_000_000_000;
const AMOUNT: u128 = 50_000;

async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: Value) -> Value {
    let r: Value = http
        .post(url)
        .json(&json!({ "jsonrpc": "2.0", "id": "t", "method": method, "params": params }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(r.get("error").is_none(), "{method} failed: {r}");
    r["result"].clone()
}

async fn patch(http: &reqwest::Client, w: &World, key: &[u8], value: &[u8]) {
    let params = json!({ "records": [ { "Data": {
        "account_id": w.policy_id, "data_key": B64.encode(key), "value": B64.encode(value),
    } } ] });
    // Sent twice, as near-sandbox's own patch helper does, for reliability.
    for _ in 0..2 {
        rpc(http, &w.rpc_url, "sandbox_patch_state", params.clone()).await;
    }
}

/// The contract's root state, and where `balance` starts in it. The fields before
/// it are the owner, an empty `pending_owner`, and the token.
async fn root_state(http: &reqwest::Client, w: &World) -> (Vec<u8>, usize) {
    let result = rpc(
        http,
        &w.rpc_url,
        "query",
        json!({
            "request_type": "view_state", "finality": "optimistic",
            "account_id": w.policy_id, "prefix_base64": B64.encode("STATE"),
        }),
    )
    .await;
    let value = result["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == B64.encode("STATE"))
        .expect("root state");
    let raw = B64.decode(value["value"].as_str().unwrap()).unwrap();
    let skip_string =
        |at: usize| at + 4 + u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
    let after_owner = skip_string(0);
    assert_eq!(raw[after_owner], 0, "pending_owner should be empty");
    let at = skip_string(after_owner + 1);
    (raw, at)
}

async fn books(http: &reqwest::Client, w: &World) -> (u128, u128) {
    let (raw, at) = root_state(http, w).await;
    (
        u128::from_le_bytes(raw[at..at + 16].try_into().unwrap()),
        u128::from_le_bytes(raw[at + 16..at + 32].try_into().unwrap()),
    )
}

/// The Borsh form of a `Pending` log entry.
fn pending_entry(e: &LogEntry, timestamp: u64) -> Vec<u8> {
    let s = |out: &mut Vec<u8>, v: &str| {
        out.extend((v.len() as u32).to_le_bytes());
        out.extend(v.as_bytes());
    };
    let mut out = Vec::new();
    out.extend(e.id.to_le_bytes());
    out.extend(timestamp.to_le_bytes());
    s(&mut out, e.agent_id.as_str());
    s(&mut out, e.vendor.as_str());
    out.extend(e.amount.to_le_bytes());
    out.push(0); // Status::Pending
    out.push(0); // reason: None
    out.push(0); // memo: None
    out.push(0); // settled_at: None
    out
}

/// Leaves the state a lost callback would leave for a payment that just settled.
/// `moved` says whether the token really has the transfer behind it.
async fn lose_callback(http: &reqwest::Client, w: &World, e: &LogEntry, age: u64, moved: bool) {
    let (mut raw, at) = root_state(http, w).await;
    let (balance, reserved) = books(http, w).await;
    assert_eq!(reserved, 0);
    let balance = if moved { balance } else { balance - e.amount };
    raw[at..at + 16].copy_from_slice(&balance.to_le_bytes());
    raw[at + 16..at + 32].copy_from_slice(&e.amount.to_le_bytes());
    patch(http, w, b"STATE", &raw).await;

    let key = [vec![2u8], e.id.to_le_bytes().to_vec()].concat(); // StorageKey::Log, index
    patch(http, w, &key, &pending_entry(e, e.timestamp - age)).await;

    for _ in 0..40 {
        let status = w.policy().log_entry(e.id).await.unwrap().unwrap().status;
        if status == Status::Pending && books(http, w).await == (balance, e.amount) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("storage patch did not take effect");
}

async fn pay(w: &World, vendor: &AccountId) -> LogEntry {
    let receipt = w
        .policy()
        .pay(&w.agent_id, w.agent_signer(), vendor, AMOUNT, None)
        .await
        .unwrap();
    assert_eq!(receipt.outcome.status, Status::Paid);
    w.policy()
        .log_entry(receipt.outcome.log_id)
        .await
        .unwrap()
        .unwrap()
}

async fn claim(w: &World, id: u32, tokens_moved: bool) -> anyhow::Result<()> {
    w.owner_call(
        "reconcile_payment",
        json!({ "id": id, "tokens_moved": tokens_moved }),
    )
    .await
}

#[tokio::test]
async fn reconcile_after_a_lost_callback() {
    let w = World::start().await.expect("sandbox world");
    let http = reqwest::Client::new();
    let vendor = w.vendor("data-api").account_id.clone();

    // 1. The tokens moved, and the callback was lost.
    let first = pay(&w, &vendor).await;
    let held = w.token_balance(&w.policy_id).await.unwrap();
    let (balance, _) = books(&http, &w).await;
    lose_callback(&http, &w, &first, DAY_NANOS, true).await;

    // A false claim fails against the real token balance, and changes nothing.
    let refusal = claim(&w, first.id, false).await.unwrap_err().to_string();
    assert!(refusal.contains("does not cover"), "{refusal}");
    assert_eq!(books(&http, &w).await, (balance, AMOUNT));
    let entry = w.policy().log_entry(first.id).await.unwrap().unwrap();
    assert_eq!(entry.status, Status::Pending);

    // The true claim settles it.
    claim(&w, first.id, true).await.unwrap();
    assert_eq!(books(&http, &w).await, (balance, 0));
    let entry = w.policy().log_entry(first.id).await.unwrap().unwrap();
    assert_eq!(entry.status, Status::Paid);
    assert!(entry.reason.unwrap().contains("reconciled by the owner"));
    assert_eq!(w.token_balance(&w.policy_id).await.unwrap(), held);

    // A settled entry cannot be claimed again, by either claim.
    for tokens_moved in [true, false] {
        let refusal = claim(&w, first.id, tokens_moved)
            .await
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("payment is not pending"), "{refusal}");
    }
    assert_eq!(books(&http, &w).await, (balance, 0));

    // 2. The tokens did not move, and the callback was lost: the real read of the
    // token balance covers what the contract counts, so the funds come back.
    let second = pay(&w, &vendor).await;
    let held = w.token_balance(&w.policy_id).await.unwrap();
    let (balance, _) = books(&http, &w).await;
    lose_callback(&http, &w, &second, DAY_NANOS, false).await;
    assert_eq!(books(&http, &w).await, (balance - AMOUNT, AMOUNT));

    claim(&w, second.id, false).await.unwrap();

    assert_eq!(books(&http, &w).await, (balance, 0));
    let entry = w.policy().log_entry(second.id).await.unwrap().unwrap();
    assert_eq!(entry.status, Status::Failed);
    assert!(entry.reason.unwrap().contains("reconciled by the owner"));
    assert_eq!(w.token_balance(&w.policy_id).await.unwrap(), held);

    // 3. An entry that is not old enough is refused, with the real block time.
    let third = pay(&w, &vendor).await;
    let (balance, _) = books(&http, &w).await;
    lose_callback(&http, &w, &third, 0, true).await;
    let refusal = claim(&w, third.id, true).await.unwrap_err().to_string();
    assert!(refusal.contains("too recent"), "{refusal}");
    assert_eq!(books(&http, &w).await, (balance, AMOUNT));

    // 4. Nobody but the contract can run the callback.
    let root_signer =
        Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap()).unwrap();
    let forged = Contract(w.policy_id.clone())
        .call_function("on_reconcile_unmoved", json!({ "id": third.id, "seq": 0 }))
        .transaction()
        .gas(NearGas::from_tgas(30))
        .with_signer(w.root.clone(), root_signer)
        .send_to(&w.network)
        .await
        .unwrap();
    assert!(
        !forged.is_success(),
        "private callback must not be callable"
    );
}

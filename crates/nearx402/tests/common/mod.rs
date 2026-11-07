//! Helpers for sandbox tests that read or patch the policy contract's storage.
#![allow(dead_code)]

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nearx402::policy::{LogEntry, Status};
use nearx402::world::World;
use serde_json::{json, Value};

pub const DAY_NANOS: u64 = 86_400 * 1_000_000_000;

pub async fn rpc(http: &reqwest::Client, url: &str, method: &str, params: Value) -> Value {
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

/// Applies state records to the sandbox. Sent twice, as near-sandbox's own patch
/// helper does, for reliability.
pub async fn patch_records(http: &reqwest::Client, w: &World, records: Value) {
    for _ in 0..2 {
        rpc(
            http,
            &w.rpc_url,
            "sandbox_patch_state",
            json!({ "records": records }),
        )
        .await;
    }
}

pub async fn patch(http: &reqwest::Client, w: &World, key: &[u8], value: &[u8]) {
    let records = json!([ { "Data": {
        "account_id": w.policy_id, "data_key": B64.encode(key), "value": B64.encode(value),
    } } ]);
    patch_records(http, w, records).await;
}

/// The contract's storage under a key prefix, as base64 key and value pairs.
pub async fn storage(http: &reqwest::Client, w: &World, prefix: &str) -> Vec<(String, String)> {
    let result = rpc(
        http,
        &w.rpc_url,
        "query",
        json!({
            "request_type": "view_state", "finality": "optimistic",
            "account_id": w.policy_id, "prefix_base64": B64.encode(prefix),
        }),
    )
    .await;
    let mut pairs: Vec<(String, String)> = result["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["key"].as_str().unwrap().to_string(),
                v["value"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The contract's root state, and where `balance` starts in it. The fields before
/// it are the owner, an empty `pending_owner`, and the token.
pub async fn root_state(http: &reqwest::Client, w: &World) -> (Vec<u8>, usize) {
    let pairs = storage(http, w, "STATE").await;
    let (_, value) = pairs
        .iter()
        .find(|(k, _)| *k == B64.encode("STATE"))
        .expect("root state");
    let raw = B64.decode(value).unwrap();
    let skip_string =
        |at: usize| at + 4 + u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
    let after_owner = skip_string(0);
    assert_eq!(raw[after_owner], 0, "pending_owner should be empty");
    let at = skip_string(after_owner + 1);
    (raw, at)
}

/// `balance` and `reserved`, read from storage.
pub async fn books(http: &reqwest::Client, w: &World) -> (u128, u128) {
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
pub async fn lose_callback(http: &reqwest::Client, w: &World, e: &LogEntry, age: u64, moved: bool) {
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

/// Sends the owner's claim for a log entry.
pub async fn claim(w: &World, id: u32, tokens_moved: bool) -> anyhow::Result<()> {
    w.owner_call(
        "reconcile_payment",
        json!({ "id": id, "tokens_moved": tokens_moved }),
    )
    .await
}

//! End-to-end checks on a local NEAR sandbox: the real contracts, the vendor
//! server and the agent. Needs `scripts/build-contracts.sh` to have run.
//!
//! One test function shares one sandbox, because starting one takes ~20 s.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use near_api::types::{AccountId, NearGas};
use near_api::{Contract, Signer};
use near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY;
use nearx402::agent::Fetch;
use nearx402::facilitator::{Facilitator, FacilitatorClient};
use nearx402::policy::Status;
use nearx402::server;
use nearx402::world::{World, USDC, WALLET_FUNDING};
use nearx402::x402::{self, FacilitatorRequest, NearPolicyProof, PaymentPayload, PaymentRequired};
use serde_json::json;

/// Borsh encoding of the policy contract's `LogEntry` for a `Paid` entry with
/// no reason, field by field, so the test can write it straight into storage.
fn borsh_paid_entry(
    id: u32,
    ts: u64,
    agent: &str,
    vendor: &str,
    amount: u128,
    memo: &str,
) -> Vec<u8> {
    let s = |out: &mut Vec<u8>, v: &str| {
        out.extend((v.len() as u32).to_le_bytes());
        out.extend(v.as_bytes());
    };
    let mut out = Vec::new();
    out.extend(id.to_le_bytes());
    out.extend(ts.to_le_bytes());
    s(&mut out, agent);
    s(&mut out, vendor);
    out.extend(amount.to_le_bytes());
    out.push(1); // Status::Paid
    out.push(0); // reason: None
    out.push(1); // memo: Some
    s(&mut out, memo);
    out.push(1); // settled_at: Some
    out.extend(1u64.to_le_bytes());
    out
}

/// Overwrites one key of `account`'s contract storage on the sandbox. Sent
/// twice, as near-sandbox's own patch helper does, for reliability.
async fn patch_storage(http: &reqwest::Client, rpc: &str, account: &str, key: &[u8], value: &[u8]) {
    let body = json!({
        "jsonrpc": "2.0", "id": "patch", "method": "sandbox_patch_state",
        "params": { "records": [ { "Data": {
            "account_id": account, "data_key": B64.encode(key), "value": B64.encode(value),
        } } ] },
    });
    for _ in 0..2 {
        let r: serde_json::Value = http
            .post(rpc)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(r.get("error").is_none(), "patch failed: {r}");
    }
}

async fn requirements(http: &reqwest::Client, url: &str) -> PaymentRequired {
    let r = http.get(url).send().await.unwrap();
    assert_eq!(r.status(), 402);
    r.json().await.unwrap()
}

/// A receipt secret and the memo that commits to it, for one payment.
fn new_receipt(nonce: &str) -> (String, String) {
    let secret = x402::new_receipt_secret();
    let memo = x402::payment_memo(nonce, &secret).unwrap();
    (secret, memo)
}

async fn with_proof(http: &reqwest::Client, url: &str, proof: &PaymentPayload) -> (u16, String) {
    let r = http
        .get(url)
        .header(x402::PAYMENT_HEADER, x402::encode(proof).unwrap())
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.text().await.unwrap())
}

// Several worker threads, so the redemption race in step 3c runs in parallel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn policy_vendor_and_agent_end_to_end() {
    let w = World::start().await.expect("sandbox world");
    let served = server::serve(
        0,
        w.vendor_config(),
        w.facilitator_config(),
        w.rpc_url.clone(),
        w.policy_id.to_string(),
    )
    .await
    .unwrap();
    let facilitator = FacilitatorClient::new(served.facilitator_url.clone())
        .with_api_key(served.facilitator_api_key.clone());
    let anonymous = FacilitatorClient::new(served.facilitator_url.clone());
    let base = format!("http://{}", served.addr);
    let data_url = format!("{base}/data-api/market-data");
    let agent = w.agent();
    let policy = w.policy();
    let http = reqwest::Client::new();

    // 1. Happy path: 402 → pay → 200, vendor actually receives the tokens.
    let Fetch::Paid { receipt, price, .. } = agent.fetch(&data_url).await.unwrap() else {
        panic!("expected a paid fetch");
    };
    let data_api: AccountId = w.vendor("data-api").account_id.clone();
    assert_eq!(w.token_balance(&data_api).await.unwrap(), price);
    assert_eq!(policy.balance().await.unwrap(), WALLET_FUNDING - price);
    let entry = policy
        .log_entry(receipt.outcome.log_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.status, Status::Paid);

    // 2. A valid proof cannot be replayed against a fresh nonce or reused.
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let req_template = req.clone();
    let replay = PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: receipt.outcome.log_id,
            nonce: req.extra.nonce.clone(),
            tx_hash: receipt.tx_hash.clone(),
            receipt_secret: x402::new_receipt_secret(),
        },
    };
    let (status, body) = with_proof(&http, &data_url, &replay).await;
    assert_eq!(status, 402, "replayed log entry must be rejected: {body}");
    // Caught first by the facilitator's redeemed set; the memo/nonce binding
    // would reject it next ("does not carry this nonce").
    assert!(body.contains("payment already redeemed"), "{body}");

    // 3. A contract that is not the trusted policy code is rejected.
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let mut fake = replay.clone();
    fake.payload.policy_contract = w.token_id.to_string();
    fake.payload.nonce = req.extra.nonce;
    let (status, body) = with_proof(&http, &data_url, &fake).await;
    assert_eq!(status, 402);
    assert!(body.contains("trusted policy code"), "{body}");

    // 3a. A nonce issued for another resource does not fit this one.
    let gpu_url = format!("{base}/gpu-cloud/gpu-minute");
    let gpu_req = requirements(&http, &gpu_url).await.accepts.remove(0);
    let mut wrong = replay.clone();
    wrong.payload.nonce = gpu_req.extra.nonce;
    let (status, body) = with_proof(&http, &data_url, &wrong).await;
    assert_eq!(status, 402);
    assert!(body.contains("invalid nonce"), "{body}");

    // The dashboard's RPC proxy only forwards read-only `query` calls.
    let r = http
        .post(format!("{base}/rpc"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "sandbox_patch_state", "params": {} }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 3b. The facilitator advertises what vendors must ask for, and a payment
    // the vendor already settled cannot be settled again, even directly.
    let supported = facilitator.supported().await.unwrap();
    assert_eq!(
        supported.kinds[0].extra.policy_code_hash,
        w.policy_code_hash.to_string()
    );
    assert_eq!(supported.kinds[0].extra.asset, w.token_id.to_string());
    let original_memo = entry.memo.clone().unwrap();
    let original_nonce = original_memo.split(':').next().unwrap().to_string();
    let mut original_req = req_template.clone();
    original_req.extra.nonce = original_nonce.clone();
    let mut original = replay.clone();
    original.payload.nonce = original_nonce;
    let again = FacilitatorRequest {
        x402_version: x402::VERSION,
        payment_payload: original,
        payment_requirements: original_req,
    };
    // Without the vendors' API key, /verify and /settle refuse to answer.
    let err = anonymous.settle(&again).await.unwrap_err().to_string();
    assert!(err.contains("401"), "{err}");
    let v = facilitator.verify(&again).await.unwrap();
    assert!(!v.is_valid);
    assert_eq!(
        v.invalid_reason.as_deref(),
        Some("payment already redeemed")
    );
    let s = facilitator.settle(&again).await.unwrap();
    assert!(!s.success);

    // 3c. Many simultaneous redemptions of one fresh payment: exactly one wins.
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let (secret, memo) = new_receipt(&req.extra.nonce);
    let paid = agent.pay(&data_api, price, Some(&memo)).await.unwrap();
    assert_eq!(paid.outcome.status, Status::Paid);
    let fresh = PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: paid.outcome.log_id,
            nonce: req.extra.nonce.clone(),
            tx_hash: paid.tx_hash.clone(),
            receipt_secret: secret,
        },
    };
    let mut racers = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let (http, url, proof) = (http.clone(), data_url.clone(), fresh.clone());
        racers.spawn(async move { with_proof(&http, &url, &proof).await });
    }
    let mut served_count = 0;
    while let Some(result) = racers.join_next().await {
        let (status, body) = result.unwrap();
        match status {
            200 => served_count += 1,
            402 => assert!(body.contains("already redeemed"), "{body}"),
            other => panic!("unexpected status {other}: {body}"),
        }
    }
    assert_eq!(served_count, 1, "one payment must buy exactly one response");

    // 3d. A real payment still has to match the invoice it is presented for.
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let (secret, memo) = new_receipt(&req.extra.nonce);
    let short = agent.pay(&data_api, price - 1, Some(&memo)).await.unwrap();
    assert_eq!(short.outcome.status, Status::Paid);
    let underpaid = PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: short.outcome.log_id,
            nonce: req.extra.nonce.clone(),
            tx_hash: short.tx_hash.clone(),
            receipt_secret: secret,
        },
    };
    let (status, body) = with_proof(&http, &data_url, &underpaid).await;
    assert_eq!(status, 402, "{body}");
    assert!(body.contains("but the price is"), "{body}");

    let gpu_cloud = w.vendor("gpu-cloud").account_id.clone();
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let (secret, memo) = new_receipt(&req.extra.nonce);
    let elsewhere = agent.pay(&gpu_cloud, price, Some(&memo)).await.unwrap();
    assert_eq!(elsewhere.outcome.status, Status::Paid);
    let wrong_vendor = PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: elsewhere.outcome.log_id,
            nonce: req.extra.nonce.clone(),
            tx_hash: elsewhere.tx_hash.clone(),
            receipt_secret: secret,
        },
    };
    let (status, body) = with_proof(&http, &data_url, &wrong_vendor).await;
    assert_eq!(status, 402, "{body}");
    assert!(body.contains("payment went to"), "{body}");

    // 3e. A facilitator configured for another token rejects this contract,
    // even when the vendor's requirements name that other token.
    let mut other_cfg = w.facilitator_config();
    other_cfg.token_id = "other-usdc.sandbox".parse().unwrap();
    let other = Facilitator::new(other_cfg).unwrap();
    let mut other_req = again.clone();
    other_req.payment_requirements.asset = "other-usdc.sandbox".into();
    let v = other.verify(&other_req).await;
    assert!(!v.is_valid);
    assert!(
        v.invalid_reason
            .as_deref()
            .is_some_and(|r| r.contains("different token")),
        "{:?}",
        v.invalid_reason
    );

    // 4. Blocked attempts are logged and move no money.
    let attacker = w.attacker_id.clone();
    let r = agent.pay(&attacker, 100_000, None).await.unwrap();
    assert_eq!(r.outcome.status, Status::Blocked);
    assert_eq!(
        r.outcome.reason.as_deref(),
        Some("recipient not in allowlist")
    );
    assert_eq!(w.token_balance(&attacker).await.unwrap(), 0);

    // 5. Nobody but the contract can settle a payment (callbacks are private).
    let root_signer =
        Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap()).unwrap();
    let forged = Contract(w.policy_id.clone())
        .call_function("on_pay_transfer", json!({ "id": r.outcome.log_id }))
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

    // 5b. A log entry that says Paid is not enough. Storage survives redeploys,
    // so a policy account could hold a Paid entry that its trusted code never
    // wrote. Simulate that by rewriting the blocked entry from step 4 as "Paid
    // $0.05 to data-api with a fresh nonce", then try to spend it.
    let req = requirements(&http, &data_url).await.accepts.remove(0);
    let blocked_id = r.outcome.log_id;
    let key = [vec![2u8], blocked_id.to_le_bytes().to_vec()].concat(); // StorageKey::Log, index
    let (secret, memo) = new_receipt(&req.extra.nonce);
    let value = borsh_paid_entry(
        blocked_id,
        entry.timestamp,
        w.agent_id.as_str(),
        data_api.as_str(),
        price,
        &memo,
    );
    patch_storage(&http, &w.rpc_url, w.policy_id.as_str(), &key, &value).await;
    let mut patched = None;
    for _ in 0..20 {
        let e = policy.log_entry(blocked_id).await.unwrap().unwrap();
        if e.status == Status::Paid {
            patched = Some(e);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let patched = patched.expect("storage patch did not take effect");
    assert_eq!(patched.vendor, data_api);
    assert_eq!(patched.memo.as_deref(), Some(memo.as_str()));
    let forged_proof = PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: blocked_id,
            nonce: req.extra.nonce.clone(),
            tx_hash: r.tx_hash.clone(),
            receipt_secret: secret,
        },
    };
    let (status, body) = with_proof(&http, &data_url, &forged_proof).await;
    assert_eq!(
        status, 402,
        "a forged Paid entry must not buy anything: {body}"
    );
    assert!(body.contains("no successful"), "{body}");
    // Pointing the forged entry at the real payment's transaction fails too:
    // that transfer carries the memo of a different log entry.
    let mut borrowed = forged_proof.clone();
    borrowed.payload.tx_hash = receipt.tx_hash.clone();
    let (status, body) = with_proof(&http, &data_url, &borrowed).await;
    assert_eq!(status, 402, "{body}");
    assert!(body.contains("no successful"), "{body}");

    // 6. The agent's key cannot call anything but policy.pay.
    let sneaky = Contract(w.policy_id.clone())
        .call_function("approve_vendor", json!({ "vendor": attacker }))
        .transaction()
        .with_signer(w.agent_id.clone(), w.agent_signer())
        .send_to(&w.network)
        .await;
    assert!(sneaky.is_err() || !sneaky.unwrap().is_success());
    assert!(!policy.log(0, 100).await.unwrap().is_empty());

    // 7. Tokens sent with a plain ft_transfer are invisible until finance
    // runs sync_balance, which credits at most the amount it names.
    let before = policy.balance().await.unwrap();
    w.owner_plain_transfer(&w.policy_id, USDC).await.unwrap();
    assert_eq!(policy.balance().await.unwrap(), before);
    w.owner_call(
        "sync_balance",
        json!({ "max_amount": (5 * USDC).to_string() }),
    )
    .await
    .unwrap();
    assert_eq!(policy.balance().await.unwrap(), before + USDC);

    // 8. Circuit breaker: the 5th blocked attempt of the day freezes the
    // agent, and later attempts fail without adding to the log.
    let blocked_so_far = policy
        .agent(&w.agent_id)
        .await
        .unwrap()
        .unwrap()
        .blocked_today;
    for _ in blocked_so_far..5 {
        let r = agent.pay(&attacker, 100_000, None).await.unwrap();
        assert_eq!(r.outcome.status, Status::Blocked);
    }
    let a = policy.agent(&w.agent_id).await.unwrap().unwrap();
    assert!(a.policy.frozen);
    assert_eq!(a.blocked_today, 5);
    let len = policy.log(0, 100).await.unwrap().len();
    // The contract panics ("too many blocked attempts today"). With the plain
    // cargo build the message surfaces only as a wasm trap, so check effects.
    let err = agent.pay(&data_api, 50_000, None).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("pay transaction failed"),
        "{err:#}"
    );
    assert_eq!(policy.log(0, 100).await.unwrap().len(), len);
    assert_eq!(
        policy
            .agent(&w.agent_id)
            .await
            .unwrap()
            .unwrap()
            .blocked_today,
        5
    );

    served.handle.abort();
}

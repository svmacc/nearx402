//! Can someone who only reads the chain claim what an agent paid for? Every field
//! of a payment proof but the secret is public once the payment is on chain, so
//! an observer can try to redeem it before the agent does.
//!
//! Before the receipt secret existed, the rebuilt proof here got HTTP 200: the
//! observer received the data, and the agent had paid for nothing.

use nearx402::policy::{PayReceipt, Status};
use nearx402::server;
use nearx402::world::World;
use nearx402::x402::{self, NearPolicyProof, PaymentPayload, PaymentRequired, PaymentRequirements};

async fn quote(http: &reqwest::Client, url: &str) -> PaymentRequirements {
    let r = http.get(url).send().await.unwrap();
    assert_eq!(r.status(), 402);
    let mut body: PaymentRequired = r.json().await.unwrap();
    body.accepts.remove(0)
}

async fn present(http: &reqwest::Client, url: &str, header: &str) -> (u16, String) {
    let r = http
        .get(url)
        .header(x402::PAYMENT_HEADER, header)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.text().await.unwrap())
}

fn proof(
    w: &World,
    req: &PaymentRequirements,
    paid: &PayReceipt,
    nonce: &str,
    secret: &str,
) -> String {
    x402::encode(&PaymentPayload {
        x402_version: x402::VERSION,
        scheme: req.scheme.clone(),
        network: req.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: paid.outcome.log_id,
            nonce: nonce.into(),
            tx_hash: paid.tx_hash.clone(),
            receipt_secret: secret.into(),
        },
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_rebuilt_from_public_data_buys_nothing() {
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
    let url = format!("http://{}/data-api/market-data", served.addr);
    let http = reqwest::Client::new();
    let data_api = w.vendor("data-api").account_id.clone();
    let agent = w.agent();

    // The agent pays, and has not claimed yet.
    let req = quote(&http, &url).await;
    let price: u128 = req.max_amount_required.parse().unwrap();
    let secret = x402::new_receipt_secret();
    let memo = x402::payment_memo(&req.extra.nonce, &secret).unwrap();
    let paid = agent.pay(&data_api, price, Some(&memo)).await.unwrap();
    assert_eq!(paid.outcome.status, Status::Paid);

    // The observer knows only the chain: the log entry, and the transaction
    // hash, which is public from the block.
    let entry = w
        .policy()
        .log_entry(paid.outcome.log_id)
        .await
        .unwrap()
        .unwrap();
    let public_memo = entry.memo.unwrap();
    let (public_nonce, public_commitment) = public_memo.split_once(':').unwrap();
    assert_eq!(public_nonce, req.extra.nonce);

    // A guessed secret.
    let guess = proof(&w, &req, &paid, public_nonce, &x402::new_receipt_secret());
    let (status, body) = present(&http, &url, &guess).await;
    assert_eq!(status, 402, "a guess claimed the payment: {body}");
    assert!(body.contains("receipt secret"), "{body}");

    // The commitment itself, which is public.
    let commitment = proof(&w, &req, &paid, public_nonce, public_commitment);
    let (status, body) = present(&http, &url, &commitment).await;
    assert_eq!(status, 402, "the public commitment claimed it: {body}");
    assert!(body.contains("receipt secret"), "{body}");

    // No secret at all: a proof from before this field existed.
    let mut old_format: serde_json::Value = serde_json::from_slice(
        &base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            proof(&w, &req, &paid, public_nonce, &secret),
        )
        .unwrap(),
    )
    .unwrap();
    old_format["payload"]
        .as_object_mut()
        .unwrap()
        .remove("receiptSecret");
    let (status, body) = present(&http, &url, &x402::encode(&old_format).unwrap()).await;
    assert_eq!(status, 402, "a proof with no secret claimed it: {body}");

    // Another payment's secret does not open this one. Its owner has shown it to
    // the vendor, so a dishonest vendor or a leak must not reach further.
    let other_req = quote(&http, &url).await;
    let other_secret = x402::new_receipt_secret();
    let other_memo = x402::payment_memo(&other_req.extra.nonce, &other_secret).unwrap();
    let other = agent
        .pay(&data_api, price, Some(&other_memo))
        .await
        .unwrap();
    assert_eq!(other.outcome.status, Status::Paid);
    let crossed = proof(&w, &req, &paid, public_nonce, &other_secret);
    let (status, body) = present(&http, &url, &crossed).await;
    assert_eq!(status, 402, "another payment's secret claimed it: {body}");
    assert!(body.contains("receipt secret"), "{body}");

    // None of those attempts cost the agent its claim.
    let claim = proof(&w, &req, &paid, public_nonce, &secret);
    let (status, body) = present(&http, &url, &claim).await;
    assert_eq!(status, 200, "the payer's own claim failed: {body}");

    // And it works once, even with the secret.
    let (status, body) = present(&http, &url, &claim).await;
    assert_eq!(status, 402, "{body}");
    assert!(body.contains("already redeemed"), "{body}");

    // The whole flow through the agent's own client.
    let fetched = agent.fetch(&url).await.unwrap();
    assert!(matches!(fetched, nearx402::agent::Fetch::Paid { .. }));

    served.handle.abort();
}

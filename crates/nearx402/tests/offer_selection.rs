//! What the agent does with a vendor it should not take at its word (CX-01): a
//! 402 can name an approved vendor and an in-budget price, and still be wrong in
//! ways that only show after the money has moved.
//!
//! A controlled server sits in front of the real vendor. It rewrites the offers
//! in a 402, and decides what happens to a paid retry.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ark_std::rand::rngs::OsRng;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use nearx402::agent::{payment_http_client, quote, Fetch};
use nearx402::deployment::{self, PolicyParams};
use nearx402::policy::{PayZkResult, PolicyClient, Status};
use nearx402::server;
use nearx402::world::{World, NETWORK_NAME};
use nearx402::x402::{self, NearPolicyProof, PaymentPayload, PaymentRequired, PaymentRequirements};
use nearx402::zk::pay_args;
use tokio::net::TcpListener;
use zx402_zk::store::WalletFile;

type Rewrite = Arc<dyn Fn(PaymentRequirements) -> Vec<PaymentRequirements> + Send + Sync>;

#[derive(Clone)]
enum Retry {
    /// Pass it to the real vendor.
    Forward,
    /// Refuse it, as a vendor would when the terms it advertised were wrong.
    Refuse,
    /// Send the client somewhere else.
    Redirect(String),
    /// Let the real vendor settle and answer, then drop the connection.
    LoseResponse,
}

#[derive(Clone)]
struct Control {
    upstream: String,
    http: reqwest::Client,
    rewrite: Arc<Mutex<Rewrite>>,
    first: Arc<Mutex<Option<String>>>,
    retry: Arc<Mutex<Retry>>,
    retries: Arc<AtomicUsize>,
}

async fn control(State(c): State<Control>, req: Request) -> Response {
    let url = format!("{}{}", c.upstream, req.uri().path());
    let Some(header) = req.headers().get(x402::PAYMENT_HEADER).cloned() else {
        let redirect = c.first.lock().unwrap().clone();
        if let Some(to) = redirect {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, to)],
            )
                .into_response();
        }
        let mut body: PaymentRequired =
            c.http.get(&url).send().await.unwrap().json().await.unwrap();
        let rewrite = c.rewrite.lock().unwrap().clone();
        body.accepts = body.accepts.into_iter().flat_map(|o| rewrite(o)).collect();
        return (StatusCode::PAYMENT_REQUIRED, Json(body)).into_response();
    };
    c.retries.fetch_add(1, Ordering::SeqCst);
    let retry = c.retry.lock().unwrap().clone();
    match retry {
        Retry::Refuse => (StatusCode::PAYMENT_REQUIRED, "terms were wrong").into_response(),
        Retry::Redirect(to) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, to)],
        )
            .into_response(),
        Retry::Forward | Retry::LoseResponse => {
            let r = c
                .http
                .get(&url)
                .header(x402::PAYMENT_HEADER, header)
                .send()
                .await
                .unwrap();
            let status = r.status().as_u16();
            let text = r.text().await.unwrap();
            if matches!(retry, Retry::LoseResponse) {
                assert_eq!(status, 200, "the vendor should have settled: {text}");
                panic!("dropping the connection on purpose, after settlement");
            }
            (StatusCode::from_u16(status).unwrap(), text).into_response()
        }
    }
}

async fn start_control(upstream: String) -> (Control, String) {
    let keep: Rewrite = Arc::new(|o| vec![o]);
    let c = Control {
        upstream,
        http: reqwest::Client::new(),
        rewrite: Arc::new(Mutex::new(keep)),
        first: Arc::default(),
        retry: Arc::new(Mutex::new(Retry::Forward)),
        retries: Arc::default(),
    };
    let app = Router::new()
        .route("/{*path}", get(control))
        .with_state(c.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (c, format!("http://{addr}"))
}

/// A server on another origin that records the proof it is handed.
async fn start_other_origin() -> (Arc<Mutex<Vec<String>>>, String) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = seen.clone();
    let app = Router::new().route(
        "/{*path}",
        get(move |req: Request| {
            let record = record.clone();
            async move {
                if let Some(h) = req.headers().get(x402::PAYMENT_HEADER) {
                    record.lock().unwrap().push(h.to_str().unwrap().to_string());
                }
                Json(serde_json::json!({ "other": "origin" }))
            }
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (seen, format!("http://{addr}"))
}

/// Everything that a payment would change.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Ledger {
    wallet: u128,
    vendor: u128,
    log: usize,
    retries: usize,
}

async fn ledger(w: &World, policy: &PolicyClient, c: &Control) -> Ledger {
    let vendor = w.vendor("data-api").account_id.clone();
    Ledger {
        wallet: policy.balance().await.unwrap(),
        vendor: w.token_balance(&vendor).await.unwrap(),
        log: policy.log(0, 200).await.unwrap().len(),
        retries: c.retries.load(Ordering::SeqCst),
    }
}

async fn start_world() -> (World, Control, String, server::Served) {
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
    let (c, base) = start_control(format!("http://{}", served.addr)).await;
    (w, c, format!("{base}/data-api/market-data"), served)
}

fn set_rewrite(
    c: &Control,
    f: impl Fn(PaymentRequirements) -> Vec<PaymentRequirements> + Send + Sync + 'static,
) {
    *c.rewrite.lock().unwrap() = Arc::new(f);
}

fn bad_asset(mut o: PaymentRequirements) -> PaymentRequirements {
    o.asset = "other-token.sandbox".into();
    o
}

fn bad_network(mut o: PaymentRequirements) -> PaymentRequirements {
    o.network = "near-mainnet".into();
    o
}

fn bad_hash(mut o: PaymentRequirements) -> PaymentRequirements {
    o.extra.policy_code_hash = "11111111111111111111111111111111".into();
    o
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_offers_move_nothing_and_a_valid_one_is_still_chosen() {
    let (w, c, url, served) = start_world().await;
    let policy = w.policy();
    let agent = w.agent();
    *c.retry.lock().unwrap() = Retry::Refuse;

    // The vendor is approved and the price is within budget, so the contract
    // would pay. Each of these is wrong only in its terms.
    type Break = fn(PaymentRequirements) -> PaymentRequirements;
    let cases: [(&str, Break); 3] = [
        ("asset", bad_asset),
        ("network", bad_network),
        ("policy hash", bad_hash),
    ];
    for (name, breaks) in cases {
        set_rewrite(&c, move |o| vec![breaks(o)]);
        let before = ledger(&w, &policy, &c).await;
        let result = agent.fetch(&url).await;
        let after = ledger(&w, &policy, &c).await;
        println!("{name}: {before:?} -> {after:?}, result {result:?}");
        assert_eq!(before, after, "{name}: the mismatch moved something");
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains(name), "{name}: {err}");
    }

    // An offer for another resource than the one requested.
    set_rewrite(&c, |mut o| {
        o.resource = "/gpu-cloud/gpu-minute".into();
        vec![o]
    });
    let before = ledger(&w, &policy, &c).await;
    let err = format!("{:#}", agent.fetch(&url).await.unwrap_err());
    assert_eq!(before, ledger(&w, &policy, &c).await);
    assert!(err.contains("resource"), "{err}");

    // No offers at all.
    set_rewrite(&c, |_| vec![]);
    let before = ledger(&w, &policy, &c).await;
    let err = format!("{:#}", agent.fetch(&url).await.unwrap_err());
    assert_eq!(before, ledger(&w, &policy, &c).await);
    assert!(err.contains("no payment offer"), "{err}");

    // A valid offer is still chosen, wherever it is in the list.
    *c.retry.lock().unwrap() = Retry::Forward;
    set_rewrite(&c, |o| {
        vec![bad_asset(o.clone()), bad_network(o.clone()), o]
    });
    let before = ledger(&w, &policy, &c).await;
    let fetched = agent.fetch(&url).await.unwrap();
    let Fetch::Paid { price, receipt, .. } = fetched else {
        panic!("expected a paid fetch, got {fetched:?}");
    };
    assert_eq!(receipt.outcome.status, Status::Paid);
    let after = ledger(&w, &policy, &c).await;
    assert_eq!(after.wallet, before.wallet - price);
    assert_eq!(after.vendor, before.vendor + price);
    assert_eq!(after.log, before.log + 1);

    served.handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paid_retry_does_not_follow_a_redirect_to_another_origin() {
    let (w, c, url, served) = start_world().await;
    let (seen, other) = start_other_origin().await;
    let policy = w.policy();
    let agent = w.agent();

    *c.retry.lock().unwrap() = Retry::Redirect(format!("{other}/collect"));
    let before = ledger(&w, &policy, &c).await;
    let result = agent.fetch(&url).await;
    let after = ledger(&w, &policy, &c).await;
    let delivered = seen.lock().unwrap().clone();
    println!(
        "redirect on retry: other origin received {} proof(s); {before:?} -> {after:?}, result {result:?}",
        delivered.len()
    );
    assert!(
        delivered.is_empty(),
        "the proof and its secret reached another origin"
    );
    // The payment had already happened, and that must be reported.
    assert_eq!(after.log, before.log + 1);
    let err = format!("{:#}", result.unwrap_err());
    assert!(err.contains("redirect"), "{err}");
    assert!(err.contains("log #"), "{err}");

    served.handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_before_paying_stops_the_agent_with_nothing_spent() {
    let (w, c, url, served) = start_world().await;
    let (seen, other) = start_other_origin().await;
    let policy = w.policy();

    *c.first.lock().unwrap() = Some(format!("{other}/collect"));
    let before = ledger(&w, &policy, &c).await;
    let err = format!("{:#}", w.agent().fetch(&url).await.unwrap_err());
    assert_eq!(before, ledger(&w, &policy, &c).await);
    assert!(err.contains("redirect"), "{err}");
    assert!(seen.lock().unwrap().is_empty());

    served.handle.abort();
}

/// The private-policy path has no `Agent`: it quotes, proves and sends by itself,
/// as `demo-zk` does, through the same `quote`.
#[tokio::test(flavor = "multi_thread")]
async fn the_zk_client_checks_an_offer_before_proving() {
    let (w, c, url, served) = start_world().await;
    let policy = w.policy();
    let (seen, other) = start_other_origin().await;
    let http = payment_http_client();
    let data_api = w.vendor("data-api").account_id.clone();

    let dir = std::env::temp_dir().join(format!("zx402-offers-{}", std::process::id()));
    let (agent_id, signer) = w.create_zk_agent("zk-agent").await.unwrap();
    let (paths, pk) = deployment::init(
        &dir,
        &PolicyParams {
            contract: w.policy_id.clone(),
            agent: agent_id.clone(),
            cap: 1_000_000,
            per_call: 500_000,
            vendors: vec![data_api.clone()],
        },
    )
    .unwrap();
    let read = |p: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    w.owner_call("set_zk_verifying_key", read(&paths.verifying_key_args))
        .await
        .unwrap();
    w.owner_call("set_zk_agent", read(&paths.agent_args))
        .await
        .unwrap();
    let mut wallet = WalletFile::open(&paths.wallet).unwrap();
    let terms = policy.accepted_terms(NETWORK_NAME).await.unwrap();

    // A mismatch stops at the quote, so there is nothing to prove or send.
    type Break = fn(PaymentRequirements) -> PaymentRequirements;
    let cases: [(&str, Break); 3] = [
        ("asset", bad_asset),
        ("network", bad_network),
        ("policy hash", bad_hash),
    ];
    for (name, breaks) in cases {
        set_rewrite(&c, move |o| vec![breaks(o)]);
        let before = ledger(&w, &policy, &c).await;
        let err = format!("{:#}", quote(&http, &url, &terms).await.unwrap_err());
        assert_eq!(before, ledger(&w, &policy, &c).await, "{name}");
        assert!(err.contains(name), "{name}: {err}");
    }

    // A valid second offer is chosen, and the payment goes through.
    set_rewrite(&c, |o| vec![bad_asset(o.clone()), o]);
    let offer = quote(&http, &url, &terms).await.unwrap();
    assert_eq!(offer.requirements.asset, w.token_id.as_str());
    let secret = x402::new_receipt_secret();
    let memo = x402::payment_memo(&offer.requirements.extra.nonce, &secret).unwrap();
    let day = policy.day().await.unwrap();
    let payment = wallet
        .wallet
        .prepare(&pk, data_api.as_str(), offer.price, &memo, day, &mut OsRng)
        .unwrap()
        .unwrap();
    wallet.begin(payment.next).unwrap();
    let args = pay_args(&data_api, offer.price, &memo, &payment, day);
    let before = ledger(&w, &policy, &c).await;
    let PayZkResult::Executed { receipt, .. } = policy
        .pay_zk(&agent_id, signer.clone(), &args)
        .await
        .unwrap()
    else {
        panic!("pay_zk reverted");
    };
    assert_eq!(receipt.outcome.status, Status::Paid);
    let after = ledger(&w, &policy, &c).await;
    assert_eq!(after.wallet, before.wallet - offer.price);

    // Its client follows no redirect either, so the proof stays with the agent.
    let header = x402::encode(&PaymentPayload {
        x402_version: x402::VERSION,
        scheme: offer.requirements.scheme.clone(),
        network: offer.requirements.network.clone(),
        payload: NearPolicyProof {
            policy_contract: w.policy_id.to_string(),
            log_id: receipt.outcome.log_id,
            nonce: offer.requirements.extra.nonce.clone(),
            tx_hash: receipt.tx_hash.clone(),
            receipt_secret: secret,
        },
    })
    .unwrap();
    *c.retry.lock().unwrap() = Retry::Redirect(format!("{other}/collect"));
    let r = http
        .get(&url)
        .header(x402::PAYMENT_HEADER, &header)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 307);
    assert!(seen.lock().unwrap().is_empty());

    // The claim still works at the real vendor.
    *c.retry.lock().unwrap() = Retry::Forward;
    let r = http
        .get(&url)
        .header(x402::PAYMENT_HEADER, &header)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let _ = std::fs::remove_dir_all(&dir);
    served.handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_response_after_settlement_is_reported_with_the_payment() {
    let (w, c, url, served) = start_world().await;
    let policy = w.policy();
    let agent = w.agent();

    *c.retry.lock().unwrap() = Retry::LoseResponse;
    let before = ledger(&w, &policy, &c).await;
    let result = agent.fetch(&url).await;
    let after = ledger(&w, &policy, &c).await;
    println!("lost response: {before:?} -> {after:?}, result {result:?}");

    // The money moved, and the facilitator has redeemed the payment.
    let price = before.wallet - after.wallet;
    assert_eq!(after.vendor, before.vendor + price);
    assert!(price > 0);
    let err = format!("{:#}", result.unwrap_err());
    // The caller must be able to tell this from a payment that never happened,
    // or it may pay again.
    assert!(err.contains("log #"), "{err}");
    assert!(err.contains("do not pay again"), "{err}");

    served.handle.abort();
}

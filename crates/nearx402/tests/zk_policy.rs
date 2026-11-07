//! Option C on a local NEAR sandbox: real contracts, vendor, facilitator, and
//! Groth16 proofs, with an agent whose only key can call `pay_zk`. Covers the
//! honest flow and what a compromised agent can try. Needs
//! `scripts/build-contracts.sh` to have run.
//!
//! Prints gas and timings. Run with `--nocapture --profile prover` for timings
//! that mean anything: the dev profile proves much more slowly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ark_std::rand::rngs::OsRng;
use ark_std::UniformRand;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use near_api::types::AccountId;
use near_api::Signer;
use nearx402::policy::{GasReport, PayReceipt, PayZkArgs, PayZkResult, PolicyClient, Status};
use nearx402::server;
use nearx402::world::{World, DAILY_CAP, PER_CALL_MAX, WALLET_FUNDING};
use nearx402::x402::{self, NearPolicyProof, PaymentPayload, PaymentRequired};
use nearx402::zk::{b64, pay_args as args};
use serde_json::json;
use zx402_zk::ark_bn254::{Bn254, Fr};
use zx402_zk::ark_groth16::ProvingKey;
use zx402_zk::policy::{self, PolicyOpening, PreparedPayment, SpendState, ZkWallet};
use zx402_zk::{g2_bytes, g2_outside_subgroup, near_vk, proof_bytes};

/// Proves anything the wallet is asked to, as a compromised agent would.
fn force(
    wallet: &ZkWallet,
    pk: &ProvingKey<Bn254>,
    vendor: &AccountId,
    amount: u128,
    memo: &str,
    day: u64,
) -> PreparedPayment {
    let (public, witness, next) = wallet
        .statement(vendor.as_str(), amount, memo, day, &mut OsRng)
        .unwrap();
    PreparedPayment {
        proof: policy::prove(pk, public, witness, &mut OsRng).unwrap(),
        public,
        next,
    }
}

struct ZkAgent {
    id: AccountId,
    signer: Arc<Signer>,
    policy: PolicyClient,
}

/// Everything a rejected attempt must leave alone.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    funds: u128,
    state: String,
    blocked_today: u32,
    log_len: usize,
    frozen: bool,
    pending: Option<u32>,
}

impl ZkAgent {
    async fn executed(&self, a: &PayZkArgs) -> (PayReceipt, GasReport) {
        match self
            .policy
            .pay_zk(&self.id, self.signer.clone(), a)
            .await
            .unwrap()
        {
            PayZkResult::Executed { receipt, gas } => (receipt, gas),
            PayZkResult::Reverted { reason, .. } => panic!("unexpected revert: {reason}"),
        }
    }

    async fn reverted(&self, a: &PayZkArgs) -> (String, GasReport) {
        match self
            .policy
            .pay_zk(&self.id, self.signer.clone(), a)
            .await
            .unwrap()
        {
            PayZkResult::Reverted { reason, gas } => (reason, gas),
            PayZkResult::Executed { receipt, .. } => {
                panic!("expected a revert, got {:?}", receipt.outcome)
            }
        }
    }

    async fn snapshot(&self) -> Snapshot {
        let view = self.policy.zk_agent(&self.id).await.unwrap().unwrap();
        Snapshot {
            funds: self.policy.balance().await.unwrap(),
            state: view.state_commitment,
            blocked_today: view.blocked_today,
            log_len: self.policy.log(0, 100).await.unwrap().len(),
            frozen: view.frozen,
            pending: view.pending_log_id,
        }
    }
}

async fn requirements(http: &reqwest::Client, url: &str) -> x402::PaymentRequirements {
    let r = http.get(url).send().await.unwrap();
    assert_eq!(r.status(), 402);
    let mut body: PaymentRequired = r.json().await.unwrap();
    body.accepts.remove(0)
}

fn median_ms(samples: &[Duration]) -> u128 {
    let mut v: Vec<u128> = samples.iter().map(Duration::as_millis).collect();
    v.sort_unstable();
    v[v.len() / 2]
}

fn worst_ms(samples: &[Duration]) -> u128 {
    samples.iter().map(Duration::as_millis).max().unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_policy_end_to_end() {
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
    let data_url = format!("http://{}/data-api/market-data", served.addr);
    let http = reqwest::Client::new();
    let policy = w.policy();
    let data_api = w.vendor("data-api").account_id.clone();
    let attacker = w.attacker_id.clone();
    let unregistered = w
        .create_unregistered_account("no-token-account")
        .await
        .unwrap();

    // Finance: a fresh setup, then commitments only. The agent gets the proving
    // key and the policy opening, never the setup randomness.
    let (pk, vk) = policy::setup(&mut OsRng).unwrap();
    let near = near_vk(&vk);
    w.owner_call(
        "set_zk_verifying_key",
        json!({
            "alpha": B64.encode(&near.alpha),
            "neg_beta": B64.encode(&near.neg_beta),
            "neg_gamma": B64.encode(&near.neg_gamma),
            "neg_delta": B64.encode(&near.neg_delta),
            "ic": B64.encode(&near.ic),
        }),
    )
    .await
    .unwrap();

    let (agent_id, signer) = w.create_zk_agent("zk-agent").await.unwrap();
    let mut wallet = ZkWallet {
        contract: w.policy_id.to_string(),
        agent: agent_id.to_string(),
        opening: PolicyOpening {
            cap: DAILY_CAP,
            per_call: PER_CALL_MAX,
            vendors: vec![data_api.to_string(), unregistered.to_string()],
            salt: Fr::rand(&mut OsRng),
        },
        state: SpendState::fresh(&mut OsRng),
    };
    w.owner_call(
        "set_zk_agent",
        json!({
            "agent_id": agent_id,
            "policy_commitment": b64(wallet.opening.commitment().unwrap()),
            "state_commitment": b64(wallet.state.commitment()),
            // High enough to run every attack below; step 14 lowers it.
            "max_blocked_per_day": 30,
        }),
    )
    .await
    .unwrap();
    let agent = ZkAgent {
        id: agent_id.clone(),
        signer: signer.clone(),
        policy: policy.clone(),
    };
    let mut funds = WALLET_FUNDING;
    let day = policy.day().await.unwrap();

    // The chain knows only commitments for this agent.
    assert!(policy.agent(&agent_id).await.unwrap().is_none());

    // 1. Full flow: 402, prove, pay_zk, retry, 200, through the real facilitator.
    // Run several times, so timings are not one sample. Network time here is a
    // local sandbox's, not testnet's.
    let mut t_quote = Vec::new();
    let mut t_prove = Vec::new();
    let mut t_tx = Vec::new();
    let mut t_redeem = Vec::new();
    let mut t_total = Vec::new();
    let mut last = None;
    let mut received = 0;
    for run in 0..5 {
        let start = Instant::now();
        let req = requirements(&http, &data_url).await;
        t_quote.push(start.elapsed());
        let price: u128 = req.max_amount_required.parse().unwrap();
        let nonce = req.extra.nonce.clone();
        let receipt_secret = x402::new_receipt_secret();
        let memo = x402::payment_memo(&nonce, &receipt_secret).unwrap();

        let t = Instant::now();
        let paid = wallet
            .prepare(&pk, data_api.as_str(), price, &memo, day, &mut OsRng)
            .unwrap()
            .unwrap();
        t_prove.push(t.elapsed());

        let t = Instant::now();
        let (receipt, gas) = agent
            .executed(&args(&data_api, price, &memo, &paid, day))
            .await;
        t_tx.push(t.elapsed());
        assert_eq!(receipt.outcome.status, Status::Paid);

        // The reported total is the sum of every step, in gas units.
        let sum: u64 = gas.steps.iter().map(|(_, g)| g).sum();
        assert_eq!(sum, gas.total, "total must cover every step");
        if run == 0 {
            println!(
                "GAS pay_zk paid: {:.2} Tgas ({} gas), all steps:",
                gas.tgas(),
                gas.total
            );
            for (who, g) in &gas.steps {
                println!("  {:>7.3} Tgas  {who}", *g as f64 / 1e12);
            }
        }

        funds -= price;
        received += price;
        assert_eq!(policy.balance().await.unwrap(), funds);
        assert_eq!(w.token_balance(&data_api).await.unwrap(), received);
        assert_eq!(
            agent.snapshot().await.state,
            b64(paid.public.new_state),
            "state must advance"
        );
        wallet.state = paid.next;

        let t = Instant::now();
        let proof = PaymentPayload {
            x402_version: x402::VERSION,
            scheme: req.scheme.clone(),
            network: req.network.clone(),
            payload: NearPolicyProof {
                policy_contract: w.policy_id.to_string(),
                log_id: receipt.outcome.log_id,
                nonce: nonce.clone(),
                tx_hash: receipt.tx_hash.clone(),
                receipt_secret,
            },
        };
        let header = x402::encode(&proof).unwrap();
        let r = http
            .get(&data_url)
            .header(x402::PAYMENT_HEADER, &header)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        t_redeem.push(t.elapsed());
        t_total.push(start.elapsed());

        let r = http
            .get(&data_url)
            .header(x402::PAYMENT_HEADER, &header)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 402, "a receipt buys one response");
        last = Some((paid, memo, price));
    }
    let (used, nonce, price) = last.unwrap();
    for (name, samples) in [
        ("GET returns 402", &t_quote),
        ("prove", &t_prove),
        ("pay_zk transaction, local sandbox", &t_tx),
        ("retry through facilitator, 200", &t_redeem),
        ("whole flow", &t_total),
    ] {
        println!(
            "TIME {name}: median {} ms, worst {} ms ({} runs)",
            median_ms(samples),
            worst_ms(samples),
            samples.len()
        );
    }

    // 2. An honest wallet refuses off-policy payments before touching the chain.
    assert!(wallet
        .prepare(&pk, attacker.as_str(), 100_000, "n", day, &mut OsRng)
        .unwrap()
        .is_err());

    // From here on, a compromised agent runs the prover anyway.
    //
    // Three different results, which must not be confused:
    //   counted   Blocked: logged, counted by the breaker, nothing moved
    //   reverted  aborted: not logged, not counted, nothing moved, gas burned
    //   failed    Failed: a valid payment the token refused; see step 11

    macro_rules! counted {
        ($what:expr, $args:expr) => {{
            let before = agent.snapshot().await;
            let (r, gas) = agent.executed(&$args).await;
            println!("COUNTED {}: {:.2} Tgas", $what, gas.tgas());
            assert_eq!(r.outcome.status, Status::Blocked, "{}", $what);
            assert_eq!(
                r.outcome.reason.as_deref(),
                Some("invalid proof"),
                "{}",
                $what
            );
            let expected = Snapshot {
                blocked_today: before.blocked_today + 1,
                log_len: before.log_len + 1,
                ..before
            };
            assert_eq!(agent.snapshot().await, expected, "{}", $what);
            assert_eq!(w.token_balance(&attacker).await.unwrap(), 0, "{}", $what);
        }};
    }

    macro_rules! reverted {
        ($what:expr, $args:expr, $why:expr) => {{
            let before = agent.snapshot().await;
            let (reason, gas) = agent.reverted(&$args).await;
            println!("REVERTED {}: {:.2} Tgas", $what, gas.tgas());
            assert!(reason.contains($why), "{}: {reason}", $what);
            assert_eq!(agent.snapshot().await, before, "{}", $what);
            assert_eq!(w.token_balance(&attacker).await.unwrap(), 0, "{}", $what);
        }};
    }

    // 3. Prompt injection: pay an address outside the private allowlist.
    let forged = force(&wallet, &pk, &attacker, 100_000, "n", day);
    counted!(
        "attacker not on allowlist",
        args(&attacker, 100_000, "n", &forged, day)
    );

    // 4. A valid proof for an allowlisted vendor, redirected to the attacker.
    let good = force(&wallet, &pk, &data_api, 100_000, "n", day);
    counted!(
        "redirect to attacker",
        args(&attacker, 100_000, "n", &good, day)
    );

    // 5. The same proof with a larger transfer.
    counted!("amount changed", args(&data_api, 100_001, "n", &good, day));

    // 6. Over the private per-call limit.
    let forged = force(&wallet, &pk, &data_api, PER_CALL_MAX + 1, "n", day);
    counted!(
        "over per-call limit",
        args(&data_api, PER_CALL_MAX + 1, "n", &forged, day)
    );

    // 7. A fabricated new day, to reset the budget. Named honestly it is refused
    // as out of date; hidden behind the real day it fails the proof.
    let tomorrow = force(&wallet, &pk, &data_api, 100_000, "n", day + 1);
    reverted!(
        "claims tomorrow",
        args(&data_api, 100_000, "n", &tomorrow, day + 1),
        "the day has changed"
    );
    counted!(
        "tomorrow's proof named as today",
        args(&data_api, 100_000, "n", &tomorrow, day)
    );

    // 8. Proofs of the right length with invalid coordinates. A malformed point
    // aborts inside the host function, before the contract can react.
    let valid_bytes = proof_bytes(&good.proof);
    assert_eq!(valid_bytes.len(), 256);
    let with = |edit: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = valid_bytes.clone();
        edit(&mut bytes);
        assert_eq!(bytes.len(), 256, "every case stays 256 bytes");
        let mut a = args(&data_api, 100_000, "n", &good, day);
        a.proof = B64.encode(&bytes);
        a
    };
    let cases: Vec<(&str, PayZkArgs)> = vec![
        (
            "A.x above the field modulus",
            with(&|b| b[0..32].fill(0xff)),
        ),
        ("A off curve", with(&|b| b[32] ^= 1)),
        (
            "B.x.c0 above the field modulus",
            with(&|b| b[64..96].fill(0xff)),
        ),
        ("B off curve", with(&|b| b[160] ^= 1)),
        (
            "B on curve, outside the subgroup",
            with(&|b| b[64..192].copy_from_slice(&g2_bytes(&g2_outside_subgroup()))),
        ),
        (
            "C.x above the field modulus",
            with(&|b| b[192..224].fill(0xff)),
        ),
        ("C off curve", with(&|b| b[224] ^= 1)),
    ];
    for (name, a) in &cases {
        reverted!(*name, a, "AltBn128");
    }

    // An all-zero proof is well-formed (points at infinity), so it reaches the
    // pairing, fails there, and is counted: the opposite of the cases above.
    counted!("all-zero proof", with(&|b| b.fill(0)));

    // 9. A spent proof cannot be replayed once the state has moved.
    reverted!(
        "replay of a settled proof",
        args(&data_api, price, &nonce, &used, day),
        "the state has changed"
    );

    // 10. The key cannot call anything else, including finance's methods.
    let sneaky = near_api::Contract(w.policy_id.clone())
        .call_function(
            "set_zk_agent",
            json!({
                "agent_id": agent_id,
                "policy_commitment": b64(wallet.opening.commitment().unwrap()),
                "state_commitment": b64(SpendState::fresh(&mut OsRng).commitment()),
            }),
        )
        .transaction()
        .with_signer(agent_id.clone(), signer.clone())
        .send_to(&w.network)
        .await;
    assert!(sneaky.is_err() || !sneaky.unwrap().is_success());

    // 11. A failed payment: the proof is valid and the token refuses. Funds and
    // state come back, it is logged as Failed, and the breaker does not count it.
    let before = agent.snapshot().await;
    let doomed = wallet
        .prepare(&pk, unregistered.as_str(), 100_000, "n", day, &mut OsRng)
        .unwrap()
        .unwrap();
    let (r, gas) = agent
        .executed(&args(&unregistered, 100_000, "n", &doomed, day))
        .await;
    println!(
        "FAILED transfer refused by the token: {:.2} Tgas",
        gas.tgas()
    );
    assert_eq!(r.outcome.status, Status::Failed);
    let expected = Snapshot {
        log_len: before.log_len + 1,
        ..before
    };
    assert_eq!(agent.snapshot().await, expected);

    // 12. The wallet kept its state, so it pays normally afterwards, up to the cap.
    let mut spent = wallet.state.spent;
    while spent + PER_CALL_MAX <= DAILY_CAP {
        let p = wallet
            .prepare(&pk, data_api.as_str(), PER_CALL_MAX, "n", day, &mut OsRng)
            .unwrap()
            .unwrap();
        let (r, _) = agent
            .executed(&args(&data_api, PER_CALL_MAX, "n", &p, day))
            .await;
        assert_eq!(r.outcome.status, Status::Paid);
        wallet.state = p.next;
        spent += PER_CALL_MAX;
        funds -= PER_CALL_MAX;
    }
    assert_eq!(policy.balance().await.unwrap(), funds);

    // 13. Past the cap: refused locally, and rejected on chain if forced.
    let over = DAILY_CAP - spent + 1;
    assert!(wallet
        .prepare(&pk, data_api.as_str(), over, "n", day, &mut OsRng)
        .unwrap()
        .is_err());
    let forged = force(&wallet, &pk, &data_api, over, "n", day);
    counted!(
        "past the daily cap",
        args(&data_api, over, "n", &forged, day)
    );
    assert_eq!(policy.balance().await.unwrap(), funds);

    // 14. Lowering the breaker limit to today's count freezes the agent, and
    // then even a valid proof moves nothing and writes nothing.
    let now = agent.snapshot().await;
    w.owner_call(
        "set_zk_agent",
        json!({
            "agent_id": agent_id,
            "policy_commitment": b64(wallet.opening.commitment().unwrap()),
            "state_commitment": now.state,
            "max_blocked_per_day": now.blocked_today,
        }),
    )
    .await
    .unwrap();
    assert!(agent.snapshot().await.frozen);
    let small = wallet
        .prepare(&pk, data_api.as_str(), 1, "n", day, &mut OsRng)
        .unwrap()
        .unwrap();
    reverted!(
        "valid proof after the breaker tripped",
        args(&data_api, 1, "n", &small, day),
        "too many blocked"
    );

    served.handle.abort();
}

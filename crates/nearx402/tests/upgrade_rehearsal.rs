//! Rehearses replacing the policy's code in place, and going back, on a local
//! NEAR sandbox: the build on testnet, then the current build, then the testnet
//! build again, each by a real deploy action with no init call, over a contract
//! holding funds, a log, a public agent, a ZK agent and a verifying key.
//!
//! Ignored by default, because it needs the old build. Run it with
//! `scripts/upgrade-rehearsal.sh`.
//!
//! Limits: `World` initialises the contract with the current build, and the test
//! swaps to the old build before anything else is written. A sandbox is not
//! testnet, and no payment is in flight during a deploy.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ark_std::rand::rngs::OsRng;
use ark_std::UniformRand;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use common::{books, claim, lose_callback, patch_records, rpc, storage, DAY_NANOS};
use near_api::types::{AccountId, CryptoHash, NearGas};
use near_api::{Contract, Signer};
use near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY;
use nearx402::policy::{PayZkResult, Status};
use nearx402::world::{res_dir, World, DAILY_CAP, PER_CALL_MAX};
use nearx402::zk::{b64, pay_args};
use serde_json::{json, Value};
use zx402_zk::ark_bn254::{Bn254, Fr};
use zx402_zk::ark_groth16::ProvingKey;
use zx402_zk::near_vk;
use zx402_zk::policy::{self, PolicyOpening, PreparedPayment, SpendState, ZkWallet};

/// The policy build on testnet, which the old file must match.
const DEPLOYED_HASH: &str = "CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU";
const AMOUNT: u128 = 50_000;

/// What must survive a deploy: every storage key and value, and what the view
/// methods return, which also shows the new code can decode the old state.
#[derive(Debug, PartialEq)]
struct Contents {
    storage: Vec<(String, String)>,
    views: BTreeMap<&'static str, String>,
}

struct Scene {
    w: World,
    http: reqwest::Client,
    vendor: AccountId,
    attacker: AccountId,
    zk_id: AccountId,
    zk_signer: Arc<Signer>,
    pk: ProvingKey<Bn254>,
    wallet: ZkWallet,
    runs: u32,
}

impl Scene {
    async fn view(&self, method: &str, args: Value) -> String {
        let result = rpc(
            &self.http,
            &self.w.rpc_url,
            "query",
            json!({
                "request_type": "call_function", "finality": "optimistic",
                "account_id": self.w.policy_id, "method_name": method,
                "args_base64": B64.encode(args.to_string()),
            }),
        )
        .await;
        let bytes = result["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{method} did not return: {result}"))
            .iter()
            .map(|b| b.as_u64().unwrap() as u8)
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    async fn contents(&self) -> Contents {
        let mut views = BTreeMap::new();
        for method in [
            "get_owner",
            "get_token_id",
            "get_balance",
            "get_agents",
            "get_vendors",
            "get_zk_agents",
            "get_zk_verifying_key",
            "get_log",
            "get_log_length",
        ] {
            views.insert(method, self.view(method, json!({})).await);
        }
        Contents {
            storage: storage(&self.http, &self.w, "").await,
            views,
        }
    }

    /// One public payment, one private payment and one blocked attempt, with the
    /// balances checked against the token.
    async fn pay_all(&mut self) {
        self.runs += 1;
        let policy = self.w.policy();
        let before = self.w.token_balance(&self.vendor).await.unwrap();

        let public = policy
            .pay(
                &self.w.agent_id,
                self.w.agent_signer(),
                &self.vendor,
                AMOUNT,
                None,
            )
            .await
            .unwrap();
        assert_eq!(public.outcome.status, Status::Paid);

        let day = policy.day().await.unwrap();
        let memo = format!("rehearsal-{}", self.runs);
        let paid = self
            .wallet
            .prepare(
                &self.pk,
                self.vendor.as_str(),
                AMOUNT,
                &memo,
                day,
                &mut OsRng,
            )
            .unwrap()
            .unwrap();
        self.send_zk(&self.vendor.clone(), &memo, &paid, day, Status::Paid)
            .await;
        self.wallet.state = paid.next;

        // A compromised agent proves anyway, for an account outside the policy.
        let memo = format!("rehearsal-attack-{}", self.runs);
        let (public, witness, next) = self
            .wallet
            .statement(self.attacker.as_str(), AMOUNT, &memo, day, &mut OsRng)
            .unwrap();
        let forced = PreparedPayment {
            proof: policy::prove(&self.pk, public, witness, &mut OsRng).unwrap(),
            public,
            next,
        };
        self.send_zk(&self.attacker.clone(), &memo, &forced, day, Status::Blocked)
            .await;

        assert_eq!(
            self.w.token_balance(&self.vendor).await.unwrap(),
            before + 2 * AMOUNT
        );
        self.assert_books_match_the_token().await;
    }

    async fn send_zk(
        &self,
        to: &AccountId,
        memo: &str,
        p: &PreparedPayment,
        day: u64,
        want: Status,
    ) {
        let result = self
            .w
            .policy()
            .pay_zk(
                &self.zk_id,
                self.zk_signer.clone(),
                &pay_args(to, AMOUNT, memo, p, day),
            )
            .await
            .unwrap();
        match result {
            PayZkResult::Executed { receipt, .. } => assert_eq!(receipt.outcome.status, want),
            PayZkResult::Reverted { reason, .. } => panic!("unexpected revert: {reason}"),
        }
    }

    async fn assert_books_match_the_token(&self) {
        let held = self.w.token_balance(&self.w.policy_id).await.unwrap();
        assert_eq!(books(&self.http, &self.w).await, (held, 0));
    }
}

/// `World` locks the policy, but testnet's still has a key. Puts one back.
async fn restore_policy_key(w: &World, http: &reqwest::Client) -> Arc<Signer> {
    let signer =
        Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse().unwrap()).unwrap();
    let public = signer.get_public_key().await.unwrap().to_string();
    let records = json!([ { "AccessKey": {
        "account_id": w.policy_id,
        "public_key": public,
        "access_key": { "nonce": 0, "permission": "FullAccess" },
    } } ]);
    patch_records(http, w, records).await;
    for _ in 0..40 {
        let keys = rpc(
            http,
            &w.rpc_url,
            "query",
            json!({
                "request_type": "view_access_key_list", "finality": "optimistic",
                "account_id": w.policy_id,
            }),
        )
        .await;
        if !keys["keys"].as_array().unwrap().is_empty() {
            return signer;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the key patch did not take effect");
}

/// A real deploy action with no init call.
async fn deploy(w: &World, signer: &Arc<Signer>, code: &[u8]) {
    Contract::deploy(w.policy_id.clone())
        .use_code(code.to_vec())
        .without_init_call()
        .with_signer(signer.clone())
        .send_to(&w.network)
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let want = CryptoHash::hash(code);
    assert_eq!(w.policy().code_hash().await.unwrap(), want);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the old build: run scripts/upgrade-rehearsal.sh"]
async fn old_to_new_to_old_without_an_init_call() {
    let old = std::fs::read(std::env::var("OLD_POLICY_WASM").unwrap()).unwrap();
    // Another build can stand in as a control: one that changes the stored layout
    // must make this test fail.
    let new_path = std::env::var("NEW_POLICY_WASM")
        .map(Into::into)
        .unwrap_or_else(|_| res_dir().join("policy.wasm"));
    let new = std::fs::read(new_path).unwrap();
    assert_eq!(CryptoHash::hash(&old).to_string(), DEPLOYED_HASH);
    assert_ne!(CryptoHash::hash(&new).to_string(), DEPLOYED_HASH);

    let w = World::start().await.expect("sandbox world");
    let http = reqwest::Client::new();
    let signer = restore_policy_key(&w, &http).await;

    // 1. The old build, as on testnet, writes everything from here on.
    deploy(&w, &signer, &old).await;

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
    let vendor = w.vendor("data-api").account_id.clone();
    let unpaid = w.vendor("gpu-cloud").account_id.clone();
    let (zk_id, zk_signer) = w.create_zk_agent("zk-agent").await.unwrap();
    let wallet = ZkWallet {
        contract: w.policy_id.to_string(),
        agent: zk_id.to_string(),
        opening: PolicyOpening {
            cap: DAILY_CAP,
            per_call: PER_CALL_MAX,
            vendors: vec![vendor.to_string(), unpaid.to_string()],
            salt: Fr::rand(&mut OsRng),
        },
        state: SpendState::fresh(&mut OsRng),
    };
    w.owner_call(
        "set_zk_agent",
        json!({
            "agent_id": zk_id,
            "policy_commitment": b64(wallet.opening.commitment().unwrap()),
            "state_commitment": b64(wallet.state.commitment()),
        }),
    )
    .await
    .unwrap();

    let attacker = w.attacker_id.clone();
    let mut s = Scene {
        w,
        http,
        vendor,
        attacker,
        zk_id,
        zk_signer,
        pk,
        wallet,
        runs: 0,
    };
    s.pay_all().await;
    let written_by_old = s.contents().await;

    // 2. Old to new: nothing in storage changes, and the new code reads it all.
    deploy(&s.w, &signer, &new).await;
    assert_eq!(s.contents().await, written_by_old);

    // The contract cannot be initialised a second time.
    let again = Contract(s.w.policy_id.clone())
        .call_function(
            "new",
            json!({ "owner_id": s.w.root, "token_id": s.w.token_id }),
        )
        .transaction()
        .gas(NearGas::from_tgas(30))
        .with_signer(s.w.root.clone(), signer.clone())
        .send_to(&s.w.network)
        .await
        .unwrap();
    assert!(!again.is_success(), "a second init must fail");
    assert_eq!(s.contents().await, written_by_old);

    // 3. The new build pays, and its new method runs, in both forms.
    s.pay_all().await;
    let refusal = claim(&s.w, 0, true).await.unwrap_err().to_string();
    assert!(refusal.contains("payment is not pending"), "{refusal}");
    for tokens_moved in [true, false] {
        let receipt =
            s.w.policy()
                .pay(&s.w.agent_id, s.w.agent_signer(), &s.vendor, AMOUNT, None)
                .await
                .unwrap();
        let entry =
            s.w.policy()
                .log_entry(receipt.outcome.log_id)
                .await
                .unwrap()
                .unwrap();
        lose_callback(&s.http, &s.w, &entry, DAY_NANOS, tokens_moved).await;
        claim(&s.w, entry.id, tokens_moved).await.unwrap();
    }
    s.assert_books_match_the_token().await;
    let written_by_new = s.contents().await;
    assert!(written_by_new.views["get_log"].contains("reconciled by the owner"));

    // 4. New to old: nothing changes, and the old code reads entries the new one
    // wrote, including reconciled ones.
    deploy(&s.w, &signer, &old).await;
    assert_eq!(s.contents().await, written_by_new);

    // 5. The old build pays again, and no longer has the new method.
    s.pay_all().await;
    let missing = claim(&s.w, 0, true).await.unwrap_err().to_string();
    assert!(missing.contains("MethodNotFound"), "{missing}");

    println!(
        "rehearsal passed: old {} -> new {} -> old, {} storage keys at the end",
        CryptoHash::hash(&old),
        CryptoHash::hash(&new),
        s.contents().await.storage.len()
    );
}

//! `reconcile_payment` on a local NEAR sandbox, with the real contracts.
//!
//! A callback cannot be made to fail on demand, so the test builds the state a
//! lost callback would leave, by patching storage: the entry goes back to
//! `Pending`, is aged, and its amount goes back into `reserved`. Everything after
//! that runs for real, including the read of the token balance and the callback.
//! Needs `scripts/build-contracts.sh` to have run.

mod common;

use common::{books, claim, lose_callback, DAY_NANOS};
use near_api::types::{AccountId, NearGas};
use near_api::{Contract, Signer};
use near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY;
use nearx402::policy::{LogEntry, Status};
use nearx402::world::World;
use serde_json::json;

const AMOUNT: u128 = 50_000;

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

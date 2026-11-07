//! Two valid proofs built on one spending state, sent at the same moment to a real
//! contract on a local sandbox. Either fits the daily cap alone, and together they
//! would break it. In the mocked VM calls run one after another by construction,
//! so only a real chain shows this.

use ark_std::rand::rngs::OsRng;
use nearx402::deployment::{self, PolicyParams};
use nearx402::policy::{PayZkArgs, PayZkResult, Status};
use nearx402::world::World;
use nearx402::zk::{chain_agent, pay_args};
use zx402_zk::store::{Reconciled, WalletFile};

const CAP: u128 = 1_000_000;
const PER_CALL: u128 = 500_000;

#[tokio::test(flavor = "multi_thread")]
async fn two_proofs_sent_together_cannot_both_spend_one_state() {
    let w = World::start_bare().await.expect("sandbox world");
    let policy = w.policy();
    let data_api = w.vendor("data-api").account_id.clone();
    let gpu_cloud = w.vendor("gpu-cloud").account_id.clone();
    let (agent, signer) = w.create_zk_agent("zk-agent").await.unwrap();

    let dir = std::env::temp_dir().join(format!("zx402-race-{}", std::process::id()));
    let (paths, pk) = deployment::init(
        &dir,
        &PolicyParams {
            contract: w.policy_id.clone(),
            agent: agent.clone(),
            cap: CAP,
            per_call: PER_CALL,
            vendors: vec![data_api.clone(), gpu_cloud.clone()],
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
    let mut file = WalletFile::open(&paths.wallet).unwrap();
    let day = policy.day().await.unwrap();

    // Half the cap goes first.
    let first = file
        .wallet
        .prepare(&pk, data_api.as_str(), PER_CALL, "first", day, &mut OsRng)
        .unwrap()
        .unwrap();
    file.begin(first.next).unwrap();
    let args = pay_args(&data_api, PER_CALL, "first", &first, day);
    let PayZkResult::Executed { receipt, .. } =
        policy.pay_zk(&agent, signer.clone(), &args).await.unwrap()
    else {
        panic!("the first payment reverted");
    };
    assert_eq!(receipt.outcome.status, Status::Paid);
    let view = policy.zk_agent(&agent).await.unwrap().unwrap();
    assert_eq!(
        file.reconcile(&chain_agent(&view).unwrap()).unwrap(),
        Reconciled::Adopted
    );
    let treasury = policy.balance().await.unwrap();
    let (data_before, gpu_before) = (
        w.token_balance(&data_api).await.unwrap(),
        w.token_balance(&gpu_cloud).await.unwrap(),
    );

    // Two proofs from that one state, for the half that is left.
    let a = file
        .wallet
        .prepare(&pk, data_api.as_str(), PER_CALL, "race-a", day, &mut OsRng)
        .unwrap()
        .unwrap();
    let b = file
        .wallet
        .prepare(&pk, gpu_cloud.as_str(), PER_CALL, "race-b", day, &mut OsRng)
        .unwrap()
        .unwrap();
    file.begin(a.next).unwrap();
    file.begin(b.next).unwrap();
    let args_a: PayZkArgs = pay_args(&data_api, PER_CALL, "race-a", &a, day);
    let args_b: PayZkArgs = pay_args(&gpu_cloud, PER_CALL, "race-b", &b, day);
    let (result_a, result_b) = tokio::join!(
        policy.pay_zk(&agent, signer.clone(), &args_a),
        policy.pay_zk(&agent, signer.clone(), &args_b),
    );

    let mut paid = 0;
    for (name, result) in [("a", result_a), ("b", result_b)] {
        match result {
            Ok(PayZkResult::Executed { receipt, .. }) => {
                println!("{name}: {:?}", receipt.outcome);
                if receipt.outcome.status == Status::Paid {
                    paid += 1;
                }
            }
            Ok(PayZkResult::Reverted { reason, .. }) => {
                println!("{name}: reverted: {reason}");
                assert!(
                    reason.contains("in flight") || reason.contains("stale proof"),
                    "{name}: an unexpected reason: {reason}"
                );
            }
            Err(e) => panic!("{name} did not reach the contract, so nothing raced: {e:#}"),
        }
    }
    assert_eq!(paid, 1, "exactly one of the two may be paid");

    // The money moved once, the cap holds, and it went to the winner's vendor.
    assert_eq!(policy.balance().await.unwrap(), treasury - PER_CALL);
    let data_delta = w.token_balance(&data_api).await.unwrap() - data_before;
    let gpu_delta = w.token_balance(&gpu_cloud).await.unwrap() - gpu_before;
    assert_eq!(data_delta + gpu_delta, PER_CALL);

    // The wallet learns from the chain which one won.
    let view = policy.zk_agent(&agent).await.unwrap().unwrap();
    assert_eq!(view.pending_log_id, None);
    assert_eq!(
        file.reconcile(&chain_agent(&view).unwrap()).unwrap(),
        Reconciled::Adopted
    );
    assert_eq!(file.wallet.state.spent, CAP);

    // The budget is used up, and forcing one more proof gets nowhere.
    let (public, witness, next) = file
        .wallet
        .statement(data_api.as_str(), 1, "over", day, &mut OsRng)
        .unwrap();
    let forced = zx402_zk::policy::PreparedPayment {
        proof: zx402_zk::policy::prove(&pk, public, witness, &mut OsRng).unwrap(),
        public,
        next,
    };
    let args = pay_args(&data_api, 1, "over", &forced, day);
    let PayZkResult::Executed { receipt, .. } =
        policy.pay_zk(&agent, signer.clone(), &args).await.unwrap()
    else {
        panic!("a forced proof should be logged, not reverted");
    };
    assert_eq!(receipt.outcome.status, Status::Blocked);
    assert_eq!(receipt.outcome.reason.as_deref(), Some("invalid proof"));
    assert_eq!(policy.balance().await.unwrap(), treasury - PER_CALL);

    drop(file);
    let _ = std::fs::remove_dir_all(&dir);
}

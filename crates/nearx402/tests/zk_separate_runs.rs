//! A rehearsal of the testnet steps on a local sandbox: finance and the agent are
//! separate processes that share only files and the chain, through the same
//! entry points `docs/testnet.md` uses. The chain here is local, so this says
//! nothing about testnet's timing, gas, or RPC behaviour.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use ark_std::rand::rngs::OsRng;
use near_api::types::AccountId;
use near_api::Signer;
use nearx402::policy::{PayZkResult, PolicyClient, Status, ZkAgentView};
use nearx402::world::{res_dir, World};
use nearx402::zk::{chain_agent, pay_args};
use serde_json::{json, Value};
use zx402_zk::ark_bn254::Bn254;
use zx402_zk::ark_groth16::ProvingKey;
use zx402_zk::store::{load_proving_key, Reconciled, WalletFile};

const CAP: &str = "1000000";
const PER_CALL: &str = "500000";
const PRICE: u128 = 50_000;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn zk_setup(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zk-setup"))
        .args(args)
        .output()
        .expect("run zk-setup")
}

fn demo(config: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_demo-zk"))
        .args(["--network", "testnet", "--port", "0", "--config"])
        .arg(config)
        .output()
        .expect("run demo-zk")
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// Runs the demo, expecting it to stop before spending anything.
async fn refused(
    client: &PolicyClient,
    agent: &AccountId,
    config: &Path,
    expected: &str,
) -> Result<()> {
    let before = client.log(0, 100).await?.len();
    let o = demo(config);
    let out = text(&o);
    assert!(!o.status.success(), "expected a refusal: {out}");
    assert!(out.contains(expected), "expected {expected:?} in: {out}");
    assert_eq!(client.log(0, 100).await?.len(), before, "it sent something");
    let view = client.zk_agent(agent).await?;
    assert!(!view.is_some_and(|v| v.frozen), "it froze the agent");
    Ok(())
}

struct Agent {
    client: PolicyClient,
    id: AccountId,
    signer: Arc<Signer>,
    pk: ProvingKey<Bn254>,
    wallet_path: PathBuf,
}

impl Agent {
    async fn view(&self) -> Result<ZkAgentView> {
        self.client
            .zk_agent(&self.id)
            .await?
            .context("the agent is registered")
    }

    /// Sends a payment, recording its intent first. Returns before reconciling,
    /// and drops the wallet file, as a crash right after sending would.
    async fn pay_and_crash(&self, vendor: &AccountId, memo: &str) -> Result<Status> {
        let mut file = WalletFile::open(&self.wallet_path)?;
        let day = self.client.day().await?;
        let payment =
            match file
                .wallet
                .prepare(&self.pk, vendor.as_str(), PRICE, memo, day, &mut OsRng)?
            {
                Ok(p) => p,
                Err(refusal) => bail!("the wallet refused: {refusal}"),
            };
        file.begin(payment.next)?;
        let args = pay_args(vendor, PRICE, memo, &payment, day);
        match self
            .client
            .pay_zk(&self.id, self.signer.clone(), &args)
            .await?
        {
            PayZkResult::Executed { receipt, .. } => Ok(receipt.outcome.status),
            PayZkResult::Reverted { reason, .. } => bail!("reverted: {reason}"),
        }
    }

    /// What the next run does first.
    async fn reopen(&self) -> Result<(Reconciled, u128)> {
        let view = self.view().await?;
        let mut file = WalletFile::open(&self.wallet_path)?;
        let r = file.reconcile(&chain_agent(&view)?)?;
        Ok((r, file.wallet.state.spent))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn finance_and_agent_are_separate_runs_that_survive_a_crash() -> Result<()> {
    let w = World::start_bare().await?;
    let dir = Temp(std::env::temp_dir().join(format!("zx402-runs-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0)?;
    let d = &dir.0;

    let (agent_id, agent_key) = w.create_zk_agent_with_key("zk-agent").await?;
    let key_file = d.join("agent-key.json");
    std::fs::write(
        &key_file,
        json!({
            "account_id": agent_id,
            "public_key": agent_key.public_key().to_string(),
            "private_key": agent_key.to_string(),
        })
        .to_string(),
    )?;
    let data = w.vendor("data-api").account_id.clone();
    let gpu = w.vendor("gpu-cloud").account_id.clone();
    let no_token = w.create_unregistered_account("no-token").await?;
    let client = w.policy();

    // ---------- finance's setup, as its own process ----------
    let setup = d.join("setup");
    let init = |out: &Path| {
        zk_setup(&[
            "init",
            "--contract",
            w.policy_id.as_str(),
            "--agent",
            agent_id.as_str(),
            "--cap",
            CAP,
            "--per-call",
            PER_CALL,
            "--vendor",
            data.as_str(),
            "--vendor",
            gpu.as_str(),
            "--vendor",
            no_token.as_str(),
            "--out",
            out.to_str().expect("utf-8 path"),
        ])
    };
    let o = init(&setup);
    assert!(o.status.success(), "{}", text(&o));
    let setup_output = text(&o);
    assert!(setup_output.contains("file-args"), "{setup_output}");

    // Nothing private reaches the output, or what finance puts on chain.
    let vk_args = read_json(&setup.join("finance/set_zk_verifying_key.json"))?;
    let agent_args = read_json(&setup.join("finance/set_zk_agent.json"))?;
    for public in [
        setup_output.clone(),
        vk_args.to_string(),
        agent_args.to_string(),
    ] {
        for private in [gpu.as_str(), CAP, PER_CALL] {
            assert!(!public.contains(private), "{private} leaked into: {public}");
        }
    }
    let wallet_path = setup.join("agent/wallet.json");
    let wallet_text = std::fs::read_to_string(&wallet_path)?;
    let wallet_salts = read_json(&wallet_path)?;
    let salt = wallet_salts["policy_salt"].as_str().context("salt")?;
    assert!(!agent_args.to_string().contains(salt), "a salt leaked");
    assert!(!setup_output.contains(salt), "a salt leaked");

    // A second setup cannot replace the first, whose salts are not recreatable.
    let o = init(&setup);
    assert!(!o.status.success());
    assert!(text(&o).contains("already exists"), "{}", text(&o));
    assert_eq!(std::fs::read_to_string(&wallet_path)?, wallet_text);

    // ---------- the agent's configuration ----------
    let base_config = json!({
        "rpc_url": w.rpc_url,
        "network_name": "near-sandbox",
        "policy": w.policy_id,
        "token": w.token_id,
        "agent": agent_id,
        "agent_key_file": key_file,
        "vendor": data,
        "outside_recipient": w.attacker_id,
        "agent_dir": setup.join("agent"),
        "policy_wasm": res_dir().join("policy.wasm"),
    });
    let config = d.join("config.json");
    std::fs::write(&config, base_config.to_string())?;
    let variant = |name: &str, field: &str, value: Value| -> Result<PathBuf> {
        let mut c = base_config.clone();
        c[field] = value;
        let path = d.join(name);
        std::fs::write(&path, c.to_string())?;
        Ok(path)
    };

    // ---------- the demo refuses a bad deployment, before spending anything ----------
    refused(&client, &agent_id, &config, "no verifying key").await?;
    w.owner_call("set_zk_verifying_key", vk_args).await?;
    refused(&client, &agent_id, &config, "not registered").await?;
    w.owner_call("set_zk_agent", agent_args).await?;

    let wrong_code = variant(
        "wrong-code.json",
        "policy_wasm",
        json!(res_dir().join("mock_usdc.wasm")),
    )?;
    refused(&client, &agent_id, &wrong_code, "code hash").await?;

    let wrong_token = variant("wrong-token.json", "token", json!(data))?;
    refused(&client, &agent_id, &wrong_token, "pays in").await?;

    // The owner holds a full-access key, which the demo must not call limited.
    let full_key = variant("full-key.json", "agent", json!(w.owner_id))?;
    refused(&client, &agent_id, &full_key, "not limited").await?;

    let inside = variant("inside.json", "outside_recipient", json!(gpu))?;
    refused(&client, &agent_id, &inside, "not outside the policy").await?;

    // A second setup's files: another proving key, then another policy.
    let other = d.join("other-setup");
    let o = init(&other);
    assert!(o.status.success(), "{}", text(&o));
    let mixed = d.join("mixed-agent");
    std::fs::create_dir_all(&mixed)?;
    std::fs::copy(setup.join("agent/wallet.json"), mixed.join("wallet.json"))?;
    std::fs::copy(other.join("agent/proving.key"), mixed.join("proving.key"))?;
    let wrong_pk = variant("wrong-pk.json", "agent_dir", json!(mixed))?;
    refused(&client, &agent_id, &wrong_pk, "another setup").await?;

    std::fs::copy(
        setup.join("agent/proving.key"),
        mixed.join("proving.key.ok"),
    )?;
    std::fs::rename(mixed.join("proving.key.ok"), mixed.join("proving.key"))?;
    std::fs::copy(other.join("agent/wallet.json"), mixed.join("wallet.json"))?;
    refused(&client, &agent_id, &wrong_pk, "different policy").await?;

    let view = client.zk_agent(&agent_id).await?.context("registered")?;
    assert_eq!(
        view.blocked_today, 0,
        "a refusal must not touch the breaker"
    );

    // ---------- two runs, as separate processes, on the same wallet ----------
    let mut paid = 0;
    for run in 1..=2u32 {
        let o = demo(&config);
        let out = text(&o);
        assert!(o.status.success(), "run {run}: {out}");
        println!("---------- run {run}, its own process ----------\n{out}");
        paid += PRICE;
        for expected in ["HTTP 402", "PROOF VERIFIED", "HTTP 200", "REJECTED"] {
            assert!(out.contains(expected), "run {run}, no {expected}: {out}");
        }
        // A recording of this output must not reveal the policy.
        for private in [gpu.as_str(), no_token.as_str(), "1.00 mUSDC", "0.50 mUSDC"] {
            assert!(!out.contains(private), "run {run} printed {private}: {out}");
        }
        // Nor how many vendors are allowed: two are unpaid here, on one line.
        assert_eq!(out.matches("not yet paid").count(), 1, "{out}");
        assert_eq!(w.token_balance(&data).await?, paid);
        assert_eq!(w.token_balance(&w.attacker_id).await?, 0);

        let view = client.zk_agent(&agent_id).await?.context("registered")?;
        assert_eq!(view.blocked_today, 2 * run);
        let mut file = WalletFile::open(&wallet_path)?;
        assert_eq!(
            file.reconcile(&chain_agent(&view)?)?,
            Reconciled::Current,
            "run {run} did not leave the file in step with the chain"
        );
        assert_eq!(file.wallet.state.spent, paid, "spending must accumulate");
    }

    // A third run would freeze the agent, so it stops first.
    refused(&client, &agent_id, &config, "circuit breaker").await?;
    let view = client.zk_agent(&agent_id).await?.context("registered")?;
    assert_eq!(view.blocked_today, 4);

    // ---------- crashes ----------
    let agent = Agent {
        client: client.clone(),
        id: agent_id.clone(),
        signer: Signer::from_secret_key(agent_key)?,
        pk: load_proving_key(&setup.join("agent/proving.key"))?,
        wallet_path: wallet_path.clone(),
    };

    // A backup from now, for the lost-state case below.
    let backup = std::fs::read(&wallet_path)?;

    // Crash after the transaction landed: the next run adopts the chain's state.
    assert_eq!(agent.pay_and_crash(&data, "crash-1").await?, Status::Paid);
    paid += PRICE;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, paid));
    assert_eq!(w.token_balance(&data).await?, paid);

    // And a payment works from there.
    assert_eq!(agent.pay_and_crash(&data, "crash-2").await?, Status::Paid);
    paid += PRICE;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, paid));

    // Crash after the intent, before sending: nothing happened, and it still pays.
    {
        let mut file = WalletFile::open(&wallet_path)?;
        let day = client.day().await?;
        let unsent = file
            .wallet
            .prepare(&agent.pk, data.as_str(), PRICE, "unsent", day, &mut OsRng)?
            .map_err(|r| anyhow::anyhow!("{r}"))?;
        file.begin(unsent.next)?;
    }
    assert_eq!(agent.reopen().await?, (Reconciled::Current, paid));
    assert_eq!(agent.pay_and_crash(&data, "crash-3").await?, Status::Paid);
    paid += PRICE;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, paid));

    // The token refuses the transfer: the contract restores the state, and the
    // file agrees, without counting against the breaker.
    let treasury = client.balance().await?;
    assert_eq!(
        agent.pay_and_crash(&no_token, "no-token").await?,
        Status::Failed
    );
    assert_eq!(agent.reopen().await?, (Reconciled::Current, paid));
    assert_eq!(client.balance().await?, treasury);
    assert_eq!(agent.view().await?.blocked_today, 4);
    assert_eq!(
        agent.pay_and_crash(&data, "after-fail").await?,
        Status::Paid
    );
    paid += PRICE;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, paid));

    // ---------- a wallet restored from an old backup ----------
    std::fs::write(&wallet_path, &backup)?;
    let view = agent.view().await?;
    let err = WalletFile::open(&wallet_path)?
        .reconcile(&chain_agent(&view)?)
        .unwrap_err()
        .to_string();
    assert!(err.contains("zk-setup recover"), "{err}");
    refused(&client, &agent_id, &config, "zk-setup recover").await?;

    // Recovery changes nothing until finance acts.
    let setup_arg = setup.to_str().expect("utf-8 path");
    let o = zk_setup(&["recover", "--dir", setup_arg]);
    assert!(o.status.success(), "{}", text(&o));
    let recovered = read_json(&setup.join("finance/set_zk_agent.json"))?;
    assert_eq!(
        agent.view().await?.state_commitment,
        view.state_commitment,
        "recovery must not touch the chain by itself"
    );
    assert!(!recovered.to_string().contains(gpu.as_str()));

    w.owner_call("set_zk_agent", recovered).await?;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, 0));
    assert_eq!(agent.pay_and_crash(&data, "recovered").await?, Status::Paid);
    paid += PRICE;
    assert_eq!(agent.reopen().await?, (Reconciled::Adopted, PRICE));
    assert_eq!(w.token_balance(&data).await?, paid);
    assert_eq!(w.token_balance(&w.attacker_id).await?, 0);
    Ok(())
}

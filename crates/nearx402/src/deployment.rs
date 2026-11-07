//! What it takes to run the private-policy flow against contracts that already
//! exist: the setup output finance produces, and the demo's configuration.
//!
//! Setup output, in a directory that must not be committed:
//!
//!   finance/set_zk_verifying_key.json   arguments, public
//!   finance/set_zk_agent.json           arguments, commitments only
//!   agent/proving.key                   for the agent, not secret
//!   agent/wallet.json                   for the agent, holds the private policy

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use ark_std::rand::rngs::OsRng;
use ark_std::UniformRand;
use near_api::types::AccountId;
use serde::Deserialize;
use serde_json::{json, Value};
use zx402_zk::ark_bn254::{Bn254, Fr};
use zx402_zk::ark_groth16::ProvingKey;
use zx402_zk::policy::{self, PolicyOpening, SpendState, ZkWallet};
use zx402_zk::store::{save_proving_key, WalletFile};

use crate::zk::{b64, verifying_key_args};

/// The allowlist's capacity: a Merkle tree of depth 8.
const MAX_VENDORS: usize = 1 << policy::MERKLE_DEPTH;

/// Where setup puts each file.
#[derive(Clone, Debug)]
pub struct SetupPaths {
    pub verifying_key_args: PathBuf,
    pub agent_args: PathBuf,
    pub proving_key: PathBuf,
    pub wallet: PathBuf,
}

impl SetupPaths {
    pub fn new(dir: &Path) -> Self {
        Self {
            verifying_key_args: dir.join("finance/set_zk_verifying_key.json"),
            agent_args: dir.join("finance/set_zk_agent.json"),
            proving_key: dir.join("agent/proving.key"),
            wallet: dir.join("agent/wallet.json"),
        }
    }
}

/// The private policy finance sets.
#[derive(Clone, Debug)]
pub struct PolicyParams {
    pub contract: AccountId,
    pub agent: AccountId,
    /// Daily cap, in token base units.
    pub cap: u128,
    /// Limit per call, in token base units.
    pub per_call: u128,
    pub vendors: Vec<AccountId>,
}

impl PolicyParams {
    fn check(&self) -> Result<()> {
        ensure!(self.per_call > 0, "the per-call limit must be positive");
        ensure!(
            self.per_call <= self.cap,
            "the per-call limit cannot exceed the daily cap"
        );
        ensure!(!self.vendors.is_empty(), "name at least one vendor");
        ensure!(
            self.vendors.len() <= MAX_VENDORS,
            "the allowlist holds at most {MAX_VENDORS} vendors"
        );
        let mut sorted: Vec<&AccountId> = self.vendors.iter().collect();
        sorted.sort();
        sorted.dedup();
        ensure!(
            sorted.len() == self.vendors.len(),
            "a vendor is listed twice"
        );
        ensure!(
            !self.vendors.contains(&self.agent),
            "the agent cannot be its own vendor"
        );
        ensure!(
            !self.vendors.contains(&self.contract),
            "the policy contract cannot be a vendor"
        );
        Ok(())
    }
}

fn write_args(path: &Path, args: &Value) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(args)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

fn agent_args(agent: &str, opening: &PolicyOpening, state: &SpendState) -> Result<Value> {
    Ok(json!({
        "agent_id": agent,
        "policy_commitment": b64(opening.commitment()?),
        "state_commitment": b64(state.commitment()),
    }))
}

/// Runs the trusted setup, and saves everything finance and the agent need.
/// Whoever ran this could have kept the randomness and could forge proofs, so
/// it must not run on the agent's machine (SD-007). Refuses to replace an earlier
/// setup, whose salts cannot be recreated.
pub fn init(dir: &Path, params: &PolicyParams) -> Result<(SetupPaths, ProvingKey<Bn254>)> {
    params.check()?;
    let paths = SetupPaths::new(dir);
    for p in [&paths.wallet, &paths.proving_key, &paths.agent_args] {
        ensure!(
            !p.exists(),
            "{} already exists; choose another directory, or `recover` an existing setup",
            p.display()
        );
    }
    for sub in ["finance", "agent"] {
        fs::create_dir_all(dir.join(sub))
            .with_context(|| format!("create {}", dir.join(sub).display()))?;
    }

    let (pk, vk) = policy::setup(&mut OsRng)?;
    let wallet = ZkWallet {
        contract: params.contract.to_string(),
        agent: params.agent.to_string(),
        opening: PolicyOpening {
            cap: params.cap,
            per_call: params.per_call,
            vendors: params.vendors.iter().map(ToString::to_string).collect(),
            salt: Fr::rand(&mut OsRng),
        },
        state: SpendState::fresh(&mut OsRng),
    };
    let args = agent_args(&wallet.agent, &wallet.opening, &wallet.state)?;
    save_proving_key(&paths.proving_key, &pk)?;
    drop(WalletFile::create(&paths.wallet, wallet)?);
    write_args(&paths.verifying_key_args, &verifying_key_args(&vk))?;
    write_args(&paths.agent_args, &args)?;
    Ok((paths, pk))
}

/// Starts over from a lost or unknown spending state: a fresh state under the
/// same policy. The wallet adopts it once finance has sent `set_zk_agent`, and
/// until then nothing changes. This also resets the day's spending, which is
/// finance's call to make.
pub fn recover(dir: &Path) -> Result<SetupPaths> {
    let paths = SetupPaths::new(dir);
    let mut file = WalletFile::open(&paths.wallet)?;
    let fresh = SpendState::fresh(&mut OsRng);
    let args = agent_args(&file.wallet.agent, &file.wallet.opening, &fresh)?;
    file.begin(fresh)?;
    write_args(&paths.agent_args, &args)?;
    Ok(paths)
}

/// The deployment `demo-zk --network testnet` runs against. It holds names and
/// paths only: signing keys stay in their own files.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoConfig {
    pub rpc_url: String,
    /// x402 network name, for example `near-testnet`.
    pub network_name: String,
    pub policy: AccountId,
    pub token: AccountId,
    pub agent: AccountId,
    /// JSON key file of the agent's limited key, in near-cli's legacy format.
    pub agent_key_file: PathBuf,
    /// The vendor the agent buys from.
    pub vendor: AccountId,
    /// The account the compromised agent tries to pay.
    pub outside_recipient: AccountId,
    /// The `agent` directory that `zk-setup init` wrote.
    pub agent_dir: PathBuf,
    /// The wasm you deployed, so the facilitator trusts exactly that code.
    pub policy_wasm: PathBuf,
    /// Transaction link for the output, with `{hash}` where the hash goes.
    #[serde(default)]
    pub explorer_tx_url: Option<String>,
}

impl DemoConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
        let mut cfg: Self = serde_json::from_str(&text)
            .with_context(|| format!("parse config {}", path.display()))?;
        cfg.agent_key_file = expand_home(&cfg.agent_key_file)?;
        cfg.agent_dir = expand_home(&cfg.agent_dir)?;
        cfg.policy_wasm = expand_home(&cfg.policy_wasm)?;
        ensure!(
            !cfg.network_name.contains("mainnet"),
            "this demo uses a mock token with an open faucet, and must never run on mainnet"
        );
        Ok(cfg)
    }
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            Ok(PathBuf::from(home).join(rest))
        }
        Err(_) => Ok(path.to_path_buf()),
    }
}

/// Checks a `view_access_key_list` result: every key must be limited to `method`
/// on `contract`, so the account cannot do anything else with what it holds.
pub fn check_limited_keys(keys: &Value, contract: &AccountId, method: &str) -> Result<usize> {
    let list = keys["keys"]
        .as_array()
        .context("access key list has no keys")?;
    ensure!(!list.is_empty(), "the account has no access keys");
    for key in list {
        let id = key["public_key"].as_str().unwrap_or("?");
        let permission = &key["access_key"]["permission"];
        let Some(call) = permission.get("FunctionCall") else {
            bail!(
                "key {id} is not limited ({permission}). Delete it, so the agent can only call \
                 {method}"
            );
        };
        ensure!(
            call["receiver_id"].as_str() == Some(contract.as_str()),
            "key {id} can call {} instead of {contract}",
            call["receiver_id"]
        );
        let methods = call["method_names"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        ensure!(
            methods.len() == 1 && methods[0].as_str() == Some(method),
            "key {id} must allow only {method}, and allows {methods:?}"
        );
    }
    Ok(list.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> AccountId {
        s.parse().unwrap()
    }

    fn params() -> PolicyParams {
        PolicyParams {
            contract: id("policy.testnet"),
            agent: id("agent.testnet"),
            cap: 1_000_000,
            per_call: 500_000,
            vendors: vec![id("data.testnet"), id("gpu.testnet")],
        }
    }

    #[test]
    fn nonsense_policies_are_refused() {
        type Change = fn(&mut PolicyParams);
        let breaks: [(Change, &str); 6] = [
            (|p| p.per_call = 0, "positive"),
            (|p| p.per_call = p.cap + 1, "exceed"),
            (|p| p.vendors.clear(), "at least one"),
            (|p| p.vendors.push(id("data.testnet")), "twice"),
            (|p| p.vendors.push(id("agent.testnet")), "its own vendor"),
            (|p| p.vendors.push(id("policy.testnet")), "contract"),
        ];
        for (change, expected) in breaks {
            let mut p = params();
            change(&mut p);
            let err = p.check().unwrap_err().to_string();
            assert!(err.contains(expected), "{err}");
        }
        assert!(params().check().is_ok());
    }

    #[test]
    fn too_many_vendors_are_refused() {
        let mut p = params();
        p.vendors = (0..=MAX_VENDORS)
            .map(|i| id(&format!("v{i}.testnet")))
            .collect();
        assert!(p.check().unwrap_err().to_string().contains("at most"));
    }

    fn key(permission: Value) -> Value {
        json!({ "public_key": "ed25519:k", "access_key": { "nonce": 1, "permission": permission } })
    }

    fn limited(receiver: &str, methods: &[&str]) -> Value {
        key(json!({ "FunctionCall": {
            "allowance": "1", "receiver_id": receiver, "method_names": methods,
        } }))
    }

    #[test]
    fn only_keys_limited_to_the_one_method_pass() {
        let policy = id("policy.testnet");
        let check =
            |keys: Vec<Value>| check_limited_keys(&json!({ "keys": keys }), &policy, "pay_zk");

        assert_eq!(
            check(vec![limited("policy.testnet", &["pay_zk"])]).unwrap(),
            1
        );
        for (bad, expected) in [
            (key(json!("FullAccess")), "not limited"),
            (limited("usdc.testnet", &["pay_zk"]), "instead of"),
            (limited("policy.testnet", &["pay_zk", "pay"]), "only pay_zk"),
            (limited("policy.testnet", &[]), "only pay_zk"),
        ] {
            let err = check(vec![limited("policy.testnet", &["pay_zk"]), bad])
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{err}");
        }
        assert!(check(vec![]).unwrap_err().to_string().contains("no access"));
    }

    #[test]
    fn a_config_for_mainnet_is_refused() {
        let dir = std::env::temp_dir().join(format!("zx402-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.json");
        let config = |network: &str| {
            json!({
                "rpc_url": "http://127.0.0.1:1", "network_name": network,
                "policy": "p.testnet", "token": "t.testnet", "agent": "a.testnet",
                "agent_key_file": "~/k.json", "vendor": "v.testnet",
                "outside_recipient": "o.testnet", "agent_dir": "d", "policy_wasm": "w",
            })
        };
        fs::write(&path, config("near-testnet").to_string()).unwrap();
        let cfg = DemoConfig::load(&path).unwrap();
        assert!(!cfg.agent_key_file.starts_with("~"));

        fs::write(&path, config("near-mainnet").to_string()).unwrap();
        assert!(DemoConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("mainnet"));
        let _ = fs::remove_dir_all(&dir);
    }
}

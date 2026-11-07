//! Sets up a complete demo world on a local NEAR sandbox: mock USDC, a locked
//! policy contract, a finance owner, vendors, an attacker account, and an agent
//! whose only key is a function-call key limited to `policy.pay`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use near_api::signer::generate_secret_key;
use near_api::types::transaction::actions::FunctionCallPermission;
use near_api::types::{AccessKeyPermission, AccountId, CryptoHash, NearGas, NearToken};
use near_api::{Account, Contract, NetworkConfig, Signer};
use near_sandbox::config::{DEFAULT_GENESIS_ACCOUNT, DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY};
use near_sandbox::Sandbox;
use serde_json::json;

use crate::agent::Agent;
use crate::facilitator::FacilitatorConfig;
use crate::policy::PolicyClient;
use crate::vendor::{demo_vendors, Vendor, VendorConfig};

/// x402 network name used on the local sandbox.
pub const NETWORK_NAME: &str = "near-sandbox";
pub const USDC: u128 = 1_000_000;
pub const DAILY_CAP: u128 = USDC; // $1.00
pub const PER_CALL_MAX: u128 = USDC / 2; // $0.50
pub const WALLET_FUNDING: u128 = 10 * USDC;

pub struct World {
    /// Keeps the sandbox process alive; it is killed on drop.
    _sandbox: Sandbox,
    pub rpc_url: String,
    pub network: NetworkConfig,
    pub root: AccountId,
    root_signer: Arc<Signer>,
    pub token_id: AccountId,
    pub policy_id: AccountId,
    pub policy_code_hash: CryptoHash,
    pub owner_id: AccountId,
    owner_signer: Arc<Signer>,
    pub agent_id: AccountId,
    agent_signer: Arc<Signer>,
    pub attacker_id: AccountId,
    pub vendors: Vec<Vendor>,
}

/// Directory holding the built contract wasm files (`scripts/build-contracts.sh`).
pub fn res_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../res")
}

fn read_wasm(name: &str) -> Result<Vec<u8>> {
    let path = res_dir().join(name);
    std::fs::read(&path).with_context(|| {
        format!(
            "missing {}; run scripts/build-contracts.sh first",
            path.display()
        )
    })
}

async fn send(
    tx: near_api::advanced::ExecuteSignedTransaction,
    net: &NetworkConfig,
    what: &str,
) -> Result<()> {
    tx.send_to(net)
        .await
        .with_context(|| format!("{what}: send"))?
        .into_result()
        .map_err(|e| anyhow!("{what}: {e}"))?;
    Ok(())
}

impl World {
    /// The full v0.2 demo world, with a public agent and approved vendors.
    pub async fn start() -> Result<Self> {
        let mut w = Self::start_bare().await?;
        w.register_public_agent().await?;
        Ok(w)
    }

    /// Contracts, people, and a funded wallet, with **no policy on chain**: no
    /// public agent and no approved vendors. `agent_id` is not created here.
    pub async fn start_bare() -> Result<Self> {
        let policy_wasm = read_wasm("policy.wasm")?;
        let token_wasm = read_wasm("mock_usdc.wasm")?;

        let sandbox = Sandbox::start_sandbox()
            .await
            .map_err(|e| anyhow!("start NEAR sandbox: {e}"))?;
        let rpc_url = sandbox.rpc_addr.clone();
        let network = NetworkConfig::from_rpc_url("sandbox", rpc_url.parse()?);
        let root: AccountId = DEFAULT_GENESIS_ACCOUNT.as_str().parse()?;
        let root_signer = Signer::from_secret_key(DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?)?;

        let sub =
            |name: &str| -> AccountId { format!("{name}.{root}").parse().expect("account id") };
        let token_id = sub("usdc");
        let policy_id = sub("policy");
        let owner_id = sub("finance");
        let agent_id = sub("agent");
        let attacker_id = sub("attacker");
        let vendors = demo_vendors(&root);

        let mut w = World {
            _sandbox: sandbox,
            rpc_url,
            network,
            root: root.clone(),
            root_signer: root_signer.clone(),
            token_id: token_id.clone(),
            policy_id: policy_id.clone(),
            policy_code_hash: CryptoHash::hash(&policy_wasm),
            owner_id: owner_id.clone(),
            owner_signer: root_signer.clone(),
            agent_id: agent_id.clone(),
            agent_signer: root_signer.clone(),
            attacker_id: attacker_id.clone(),
            vendors,
        };

        // Token and policy contracts.
        let token_signer = w.create_account(&token_id, 10).await?;
        send(
            Contract::deploy(token_id.clone())
                .use_code(token_wasm)
                .with_init_call("new", json!({}))?
                .with_signer(token_signer),
            &w.network,
            "deploy mock USDC",
        )
        .await?;

        let (policy_sk, policy_signer) = w.create_account_with_key(&policy_id, 10).await?;
        send(
            Contract::deploy(policy_id.clone())
                .use_code(policy_wasm)
                .with_init_call("new", json!({ "owner_id": owner_id, "token_id": token_id }))?
                .with_signer(policy_signer.clone()),
            &w.network,
            "deploy policy",
        )
        .await?;
        // Lock the policy: with no access keys left, nobody can change its code.
        send(
            Account(policy_id.clone())
                .delete_key(policy_sk.public_key())
                .with_signer(policy_signer),
            &w.network,
            "lock policy contract",
        )
        .await?;

        // People: finance owner, vendors, attacker.
        w.owner_signer = w.create_account(&owner_id, 10).await?;
        let mut payees: Vec<AccountId> = w.vendors.iter().map(|v| v.account_id.clone()).collect();
        payees.push(attacker_id.clone());
        for id in &payees {
            w.create_account(id, 2).await?;
        }

        // Token registrations and funding.
        for id in payees.iter().chain([&policy_id]) {
            w.faucet(id, 0).await?;
        }
        w.faucet(&owner_id, 2 * WALLET_FUNDING).await?;
        send(
            Contract(token_id.clone())
                .call_function(
                    "ft_transfer_call",
                    json!({
                        "receiver_id": policy_id,
                        "amount": WALLET_FUNDING.to_string(),
                        "msg": "",
                    }),
                )
                .transaction()
                .gas(NearGas::from_tgas(100))
                .deposit(NearToken::from_yoctonear(1))
                .with_signer(owner_id.clone(), w.owner_signer.clone()),
            &w.network,
            "fund policy wallet",
        )
        .await?;

        Ok(w)
    }

    /// The v0.2 agent with public limits, and its first two vendors.
    async fn register_public_agent(&mut self) -> Result<()> {
        let agent_id = self.agent_id.clone();
        let policy_id = self.policy_id.clone();
        self.owner_call(
            "set_agent",
            json!({
                "agent_id": agent_id,
                "daily_cap": DAILY_CAP.to_string(),
                "per_call_max": PER_CALL_MAX.to_string(),
            }),
        )
        .await?;
        for slug in ["data-api", "gpu-cloud"] {
            let vendor = self.vendor(slug).account_id.clone();
            self.approve_vendor(&vendor).await?;
        }

        // Agent: swap its full-access key for a key that can only call policy.pay.
        let (full_sk, full_signer) = self.create_account_with_key(&agent_id, 5).await?;
        let pay_sk = generate_secret_key()?;
        send(
            Account(agent_id.clone())
                .add_key(
                    AccessKeyPermission::FunctionCall(FunctionCallPermission {
                        allowance: Some(NearToken::from_near(1)),
                        receiver_id: policy_id.to_string(),
                        method_names: vec!["pay".into()],
                    }),
                    pay_sk.public_key(),
                )
                .with_signer(full_signer.clone()),
            &self.network,
            "add agent pay key",
        )
        .await?;
        send(
            Account(agent_id.clone())
                .delete_key(full_sk.public_key())
                .with_signer(full_signer),
            &self.network,
            "remove agent full-access key",
        )
        .await?;
        self.agent_signer = Signer::from_secret_key(pay_sk)?;
        Ok(())
    }

    /// An agent account whose only key can call `policy.pay_zk`. The caller
    /// registers it with `set_zk_agent`.
    pub async fn create_zk_agent(&self, name: &str) -> Result<(AccountId, Arc<Signer>)> {
        let (id, key) = self.create_zk_agent_with_key(name).await?;
        Ok((id, Signer::from_secret_key(key)?))
    }

    /// As [`Self::create_zk_agent`], returning the limited key itself, so that a
    /// test can hand it to another process in a key file.
    pub async fn create_zk_agent_with_key(
        &self,
        name: &str,
    ) -> Result<(AccountId, near_api::types::SecretKey)> {
        let id: AccountId = format!("{name}.{}", self.root).parse()?;
        let (full_sk, full_signer) = self.create_account_with_key(&id, 5).await?;
        let pay_sk = generate_secret_key()?;
        send(
            Account(id.clone())
                .add_key(
                    AccessKeyPermission::FunctionCall(FunctionCallPermission {
                        allowance: Some(NearToken::from_near(1)),
                        receiver_id: self.policy_id.to_string(),
                        method_names: vec!["pay_zk".into()],
                    }),
                    pay_sk.public_key(),
                )
                .with_signer(full_signer.clone()),
            &self.network,
            "add ZK agent key",
        )
        .await?;
        send(
            Account(id.clone())
                .delete_key(full_sk.public_key())
                .with_signer(full_signer),
            &self.network,
            "remove ZK agent full-access key",
        )
        .await?;
        Ok((id, pay_sk))
    }

    /// An account with no token registration, so transfers to it fail.
    pub async fn create_unregistered_account(&self, name: &str) -> Result<AccountId> {
        let id: AccountId = format!("{name}.{}", self.root).parse()?;
        self.create_account(&id, 2).await?;
        Ok(id)
    }

    pub fn vendor(&self, slug: &str) -> &Vendor {
        self.vendors
            .iter()
            .find(|v| v.slug == slug)
            .expect("demo vendor")
    }

    pub fn policy(&self) -> PolicyClient {
        PolicyClient::new(self.network.clone(), self.policy_id.clone())
    }

    pub fn agent(&self) -> Agent {
        Agent::new(
            self.agent_id.clone(),
            self.agent_signer.clone(),
            self.policy(),
            NETWORK_NAME,
        )
    }

    /// The agent's own signer, for demonstrating what its key cannot do.
    pub fn agent_signer(&self) -> Arc<Signer> {
        self.agent_signer.clone()
    }

    pub fn vendor_config(&self) -> VendorConfig {
        VendorConfig {
            network_name: NETWORK_NAME.into(),
            token_id: self.token_id.clone(),
            trusted_policy_code_hash: self.policy_code_hash,
            vendors: self.vendors.clone(),
            nonce_secret: rand::random(),
        }
    }

    pub fn facilitator_config(&self) -> FacilitatorConfig {
        FacilitatorConfig {
            network: self.network.clone(),
            rpc_url: self.rpc_url.clone(),
            network_name: NETWORK_NAME.into(),
            token_id: self.token_id.clone(),
            trusted_policy_code_hash: self.policy_code_hash,
            api_keys: vec![],
            max_payment_age: crate::facilitator::DEFAULT_MAX_PAYMENT_AGE,
            redeemed_file: None,
        }
    }

    /// Calls a policy method as the finance owner.
    pub async fn owner_call(&self, method: &str, args: serde_json::Value) -> Result<()> {
        self.owner_call_inner(method, args).await
    }

    /// Sends tokens from the owner with a plain `ft_transfer`, the mistake
    /// that `sync_balance` recovers from.
    pub async fn owner_plain_transfer(&self, receiver: &AccountId, amount: u128) -> Result<()> {
        send(
            Contract(self.token_id.clone())
                .call_function(
                    "ft_transfer",
                    json!({ "receiver_id": receiver, "amount": amount.to_string() }),
                )
                .transaction()
                .gas(NearGas::from_tgas(30))
                .deposit(NearToken::from_yoctonear(1))
                .with_signer(self.owner_id.clone(), self.owner_signer.clone()),
            &self.network,
            "plain ft_transfer",
        )
        .await
    }

    pub async fn approve_vendor(&self, vendor: &AccountId) -> Result<()> {
        self.owner_call("approve_vendor", json!({ "vendor": vendor }))
            .await
    }

    pub async fn token_balance(&self, account: &AccountId) -> Result<u128> {
        let v: near_api::Data<String> = Contract(self.token_id.clone())
            .call_function("ft_balance_of", json!({ "account_id": account }))
            .read_only()
            .fetch_from(&self.network)
            .await?;
        Ok(v.data.parse()?)
    }

    async fn owner_call_inner(&self, method: &str, args: serde_json::Value) -> Result<()> {
        send(
            Contract(self.policy_id.clone())
                .call_function(method, args)
                .transaction()
                .gas(NearGas::from_tgas(100))
                .with_signer(self.owner_id.clone(), self.owner_signer.clone()),
            &self.network,
            method,
        )
        .await
    }

    async fn faucet(&self, account: &AccountId, amount: u128) -> Result<()> {
        send(
            Contract(self.token_id.clone())
                .call_function(
                    "faucet",
                    json!({ "account_id": account, "amount": amount.to_string() }),
                )
                .transaction()
                .with_signer(self.root.clone(), self.root_signer.clone()),
            &self.network,
            "faucet",
        )
        .await
    }

    async fn create_account(&self, id: &AccountId, near: u128) -> Result<Arc<Signer>> {
        Ok(self.create_account_with_key(id, near).await?.1)
    }

    async fn create_account_with_key(
        &self,
        id: &AccountId,
        near: u128,
    ) -> Result<(near_api::types::SecretKey, Arc<Signer>)> {
        let sk = generate_secret_key()?;
        send(
            Account::create_account(id.clone())
                .fund_myself(self.root.clone(), NearToken::from_near(near))
                .with_public_key(sk.public_key())
                .with_signer(self.root_signer.clone()),
            &self.network,
            &format!("create {id}"),
        )
        .await?;
        let signer = Signer::from_secret_key(sk.clone())?;
        Ok((sk, signer))
    }
}

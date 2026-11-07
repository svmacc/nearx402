//! Typed client for the policy contract, plus helpers shared by agent and vendor.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use near_api::types::transaction::result::TransactionResultError;
use near_api::types::{AccountId, CryptoHash, NearGas};
use near_api::{Account, Contract, NetworkConfig, Signer};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::AcceptedTerms;

/// Gas the agent attaches to `pay` (the contract requires at least 50 Tgas).
pub const PAY_GAS: NearGas = NearGas::from_tgas(60);
/// Gas the agent attaches to `pay_zk` (the contract requires at least 90 Tgas).
pub const PAY_ZK_GAS: NearGas = NearGas::from_tgas(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pending,
    Paid,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayOutcome {
    pub log_id: u32,
    pub status: Status,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: u32,
    pub timestamp: u64,
    pub agent_id: AccountId,
    pub vendor: AccountId,
    #[serde(with = "u128_string")]
    pub amount: u128,
    pub status: Status,
    pub reason: Option<String>,
    pub memo: Option<String>,
    pub settled_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentPolicy {
    #[serde(with = "u128_string")]
    pub daily_cap: u128,
    #[serde(with = "u128_string")]
    pub per_call_max: u128,
    pub frozen: bool,
    pub max_blocked_per_day: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentView {
    pub agent_id: AccountId,
    pub policy: AgentPolicy,
    #[serde(with = "u128_string")]
    pub spent_today: u128,
    pub blocked_today: u32,
}

/// Result of an agent's `pay` transaction.
#[derive(Clone, Debug)]
pub struct PayReceipt {
    pub outcome: PayOutcome,
    pub tx_hash: String,
}

/// A private-policy agent: the chain holds only commitments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZkAgentView {
    pub agent_id: AccountId,
    /// Base64, 32 bytes.
    pub policy_commitment: String,
    /// Base64, 32 bytes.
    pub state_commitment: String,
    pub frozen: bool,
    pub max_blocked_per_day: u32,
    pub blocked_today: u32,
    pub pending_log_id: Option<u32>,
}

/// Gas burned by one transaction, in gas units (1 Tgas = 10^12 gas): the total
/// the node reports, and the amount each step of the transaction burned.
#[derive(Clone, Debug)]
pub struct GasReport {
    pub total: u64,
    pub steps: Vec<(String, u64)>,
}

impl GasReport {
    fn new(total: u64, steps: impl Iterator<Item = (String, u64)>) -> Self {
        Self {
            total,
            steps: steps.collect(),
        }
    }

    pub fn tgas(&self) -> f64 {
        self.total as f64 / 1e12
    }
}

/// How a `pay_zk` transaction ended.
#[derive(Clone, Debug)]
pub enum PayZkResult {
    /// The contract ran to the end. The outcome says `Paid`, `Failed` (the
    /// token transfer failed; funds and state were restored; not counted by
    /// the breaker) or `Blocked` (logged and counted by the breaker).
    Executed { receipt: PayReceipt, gas: GasReport },
    /// The call aborted: nothing was written, so no log entry and no breaker
    /// count. This covers malformed proof points and out-of-date proofs.
    Reverted { reason: String, gas: GasReport },
}

/// Arguments of `pay_zk`, with all binary values already base64-encoded.
#[derive(Clone, Debug, Serialize)]
pub struct PayZkArgs {
    pub vendor: AccountId,
    #[serde(with = "u128_string")]
    pub amount: u128,
    pub memo: String,
    pub proof: String,
    pub new_state: String,
    pub expected_old_state: String,
    pub expected_policy: String,
    pub expected_day: String,
}

#[derive(Clone)]
pub struct PolicyClient {
    pub network: NetworkConfig,
    pub contract_id: AccountId,
}

impl PolicyClient {
    pub fn new(network: NetworkConfig, contract_id: AccountId) -> Self {
        Self {
            network,
            contract_id,
        }
    }

    fn contract(&self) -> Contract {
        Contract(self.contract_id.clone())
    }

    async fn view<T: for<'de> Deserialize<'de> + Send + Sync>(
        &self,
        method: &str,
        args: serde_json::Value,
    ) -> Result<T> {
        let data = self
            .contract()
            .call_function(method, args)
            .read_only::<T>()
            .fetch_from(&self.network)
            .await
            .with_context(|| format!("view {method} on {}", self.contract_id))?;
        Ok(data.data)
    }

    pub async fn log_entry(&self, id: u32) -> Result<Option<LogEntry>> {
        self.view("get_log_entry", json!({ "id": id })).await
    }

    pub async fn log(&self, from: u32, limit: u32) -> Result<Vec<LogEntry>> {
        self.view("get_log", json!({ "from_index": from, "limit": limit }))
            .await
    }

    pub async fn token_id(&self) -> Result<AccountId> {
        self.view("get_token_id", json!({})).await
    }

    pub async fn balance(&self) -> Result<u128> {
        let s: String = self.view("get_balance", json!({})).await?;
        Ok(s.parse()?)
    }

    pub async fn agent(&self, agent_id: &AccountId) -> Result<Option<AgentView>> {
        self.view("get_agent", json!({ "agent_id": agent_id }))
            .await
    }

    /// Calls `pay` as the agent. Only the `pay` method is needed, so a
    /// function-call access key restricted to this contract is enough.
    pub async fn pay(
        &self,
        agent_id: &AccountId,
        signer: Arc<Signer>,
        vendor: &AccountId,
        amount: u128,
        memo: Option<&str>,
    ) -> Result<PayReceipt> {
        let result = self
            .contract()
            .call_function(
                "pay",
                json!({ "vendor": vendor, "amount": amount.to_string(), "memo": memo }),
            )
            .transaction()
            .gas(PAY_GAS)
            .with_signer(agent_id.clone(), signer)
            .send_to(&self.network)
            .await
            .context("send pay transaction")?;
        let success = result
            .into_result()
            .map_err(|e| anyhow!("pay transaction failed: {e}"))?;
        let tx_hash = success.outcome().transaction_hash.to_string();
        let outcome: PayOutcome = success.json().context("decode PayOutcome")?;
        Ok(PayReceipt { outcome, tx_hash })
    }

    /// The UTC day number the contract will use, for building a `pay_zk` proof.
    pub async fn day(&self) -> Result<u64> {
        let s: String = self.view("get_day", json!({})).await?;
        Ok(s.parse()?)
    }

    /// The verifying key the contract trusts, in the form of
    /// [`crate::zk::verifying_key_args`], so the two can be compared.
    pub async fn zk_verifying_key(&self) -> Result<Option<serde_json::Value>> {
        self.view("get_zk_verifying_key", json!({})).await
    }

    pub async fn zk_agent(&self, agent_id: &AccountId) -> Result<Option<ZkAgentView>> {
        self.view("get_zk_agent", json!({ "agent_id": agent_id }))
            .await
    }

    /// Calls `pay_zk` as the agent. `Err` means the transaction could not be
    /// sent or did not finish; a contract or host abort is a normal
    /// [`PayZkResult::Reverted`], which still burns gas.
    pub async fn pay_zk(
        &self,
        agent_id: &AccountId,
        signer: Arc<Signer>,
        args: &PayZkArgs,
    ) -> Result<PayZkResult> {
        let result = self
            .contract()
            .call_function("pay_zk", args)
            .transaction()
            .gas(PAY_ZK_GAS)
            .with_signer(agent_id.clone(), signer)
            .send_to(&self.network)
            .await
            .context("send pay_zk transaction")?;
        match result.into_result() {
            Ok(success) => {
                let gas = GasReport::new(
                    success.total_gas_burnt.as_gas(),
                    success
                        .outcomes()
                        .iter()
                        .map(|o| (o.executor_id.to_string(), o.gas_burnt.as_gas())),
                );
                let tx_hash = success.outcome().transaction_hash.to_string();
                let outcome: PayOutcome = success.json().context("decode PayOutcome")?;
                Ok(PayZkResult::Executed {
                    receipt: PayReceipt { outcome, tx_hash },
                    gas,
                })
            }
            Err(TransactionResultError::Failure(f)) => Ok(PayZkResult::Reverted {
                reason: f.to_string(),
                gas: GasReport::new(
                    f.total_gas_burnt.as_gas(),
                    f.outcomes()
                        .iter()
                        .map(|o| (o.executor_id.to_string(), o.gas_burnt.as_gas())),
                ),
            }),
            Err(e) => bail!("pay_zk transaction did not finish: {e}"),
        }
    }

    /// What this contract can pay with, read from the chain, for comparing with
    /// a vendor's offer before signing. `network` is the x402 name of this chain,
    /// which the chain itself does not state.
    pub async fn accepted_terms(&self, network: &str) -> Result<AcceptedTerms> {
        Ok(AcceptedTerms {
            network: network.to_string(),
            asset: self.token_id().await?.to_string(),
            policy_code_hash: self.code_hash().await?.to_string(),
        })
    }

    /// Code hash of the deployed policy contract, so a vendor can check it
    /// runs the published code before trusting its log.
    pub async fn code_hash(&self) -> Result<CryptoHash> {
        code_hash(&self.network, &self.contract_id).await
    }
}

pub async fn code_hash(network: &NetworkConfig, account: &AccountId) -> Result<CryptoHash> {
    let view = Account(account.clone())
        .view()
        .fetch_from(network)
        .await
        .with_context(|| format!("view account {account}"))?;
    match view.data.contract_state {
        near_api::types::account::ContractState::LocalHash(h) => Ok(h),
        other => bail!("{account} has no locally deployed contract ({other:?})"),
    }
}

/// Formats USDC base units (6 decimals) as `$1.234567`, trimming trailing zeros.
pub fn usd(amount: u128) -> String {
    let whole = amount / 1_000_000;
    let frac = amount % 1_000_000;
    let mut digits = format!("{frac:06}");
    while digits.len() > 2 && digits.ends_with('0') {
        digits.pop();
    }
    format!("${whole}.{digits}")
}

mod u128_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_usdc() {
        assert_eq!(usd(50_000), "$0.05");
        assert_eq!(usd(500_000), "$0.50");
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(1_000_000), "$1.00");
        assert_eq!(usd(1_234_567), "$1.234567");
    }

    #[test]
    fn parses_contract_log_entry() {
        let raw = r#"{"id":3,"timestamp":1,"agent_id":"agent.sandbox","vendor":"data-api.sandbox",
            "amount":"50000","status":"Paid","reason":null,"memo":"n1","settled_at":42}"#;
        let e: LogEntry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.amount, 50_000);
        assert_eq!(e.status, Status::Paid);
    }
}

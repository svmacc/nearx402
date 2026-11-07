//! Helpers for `pay_zk`: call arguments from a prepared payment, and reading
//! back what really happened on chain, so a demo reports evidence rather than
//! its own expectations.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use near_api::types::AccountId;
use near_api::NetworkConfig;
use serde_json::{json, Value};
use zx402_zk::ark_bn254::{Bn254, Fr};
use zx402_zk::ark_groth16::VerifyingKey;
use zx402_zk::policy::PreparedPayment;
use zx402_zk::store::{fr_from_base64, ChainAgent};
use zx402_zk::{fr_le, near_vk, proof_bytes};

use crate::policy::{PayZkArgs, ZkAgentView};

/// A field element as the contract's base64 arguments expect it.
pub fn b64(fr: Fr) -> String {
    B64.encode(fr_le(&fr))
}

/// `pay_zk` arguments for a prepared payment. `vendor`, `amount`, `memo` and
/// `day` are separate so that a test or demo can send values that differ from
/// what the proof covers, as a cheating agent would.
pub fn pay_args(
    vendor: &AccountId,
    amount: u128,
    memo: &str,
    p: &PreparedPayment,
    day: u64,
) -> PayZkArgs {
    PayZkArgs {
        vendor: vendor.clone(),
        amount,
        memo: memo.into(),
        proof: B64.encode(proof_bytes(&p.proof)),
        new_state: b64(p.public.new_state),
        expected_old_state: b64(p.public.old_state),
        expected_policy: b64(p.public.policy_commitment),
        expected_day: day.to_string(),
    }
}

/// Arguments of `set_zk_verifying_key`.
pub fn verifying_key_args(vk: &VerifyingKey<Bn254>) -> Value {
    let near = near_vk(vk);
    json!({
        "alpha": B64.encode(&near.alpha),
        "neg_beta": B64.encode(&near.neg_beta),
        "neg_gamma": B64.encode(&near.neg_gamma),
        "neg_delta": B64.encode(&near.neg_delta),
        "ic": B64.encode(&near.ic),
    })
}

/// One execution step of a transaction, as the node reports it.
#[derive(Clone, Debug)]
pub struct TxStep {
    pub executor: String,
    pub gas_burnt: u64,
    /// NEP-297 events this step logged.
    pub events: Vec<Value>,
}

impl TxStep {
    pub fn has_event(&self, name: &str) -> bool {
        self.events.iter().any(|e| e["event"] == name)
    }
}

/// A transaction read back from the node by its hash.
#[derive(Clone, Debug)]
pub struct TxEvidence {
    pub hash: String,
    pub signer: String,
    pub receiver: String,
    pub method: String,
    /// Gas to convert the signed transaction into its first receipt.
    pub conversion_gas: u64,
    pub steps: Vec<TxStep>,
}

impl TxEvidence {
    pub fn has_event(&self, name: &str) -> bool {
        self.steps.iter().any(|s| s.has_event(name))
    }

    /// The `(to, amount)` of each NEP-141 transfer logged by `token`.
    pub fn transfers(&self, token: &AccountId) -> Vec<(String, u128)> {
        self.steps
            .iter()
            .filter(|s| s.executor == token.as_str())
            .flat_map(|s| s.events.iter())
            .filter(|e| e["standard"] == "nep141" && e["event"] == "ft_transfer")
            .flat_map(|e| e["data"].as_array().cloned().unwrap_or_default())
            .filter_map(|d| {
                Some((
                    d["new_owner_id"].as_str()?.to_string(),
                    d["amount"].as_str()?.parse().ok()?,
                ))
            })
            .collect()
    }

    /// Everything the transaction burned, including its conversion.
    pub fn total_gas(&self) -> u64 {
        self.conversion_gas + self.steps.iter().map(|s| s.gas_burnt).sum::<u64>()
    }
}

async fn rpc(rpc_url: &str, method: &str, params: Value) -> Result<Value> {
    let body =
        json!({ "jsonrpc": "2.0", "id": "nearx402-demo", "method": method, "params": params });
    let resp: Value = reqwest::Client::new()
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if let Some(err) = resp.get("error") {
        bail!("RPC error from {method}: {err}");
    }
    Ok(resp["result"].clone())
}

/// Fetches a finished transaction from the node.
pub async fn tx_evidence(rpc_url: &str, hash: &str, sender: &AccountId) -> Result<TxEvidence> {
    let tx = rpc(
        rpc_url,
        "tx",
        json!({ "tx_hash": hash, "sender_account_id": sender, "wait_until": "FINAL" }),
    )
    .await
    .with_context(|| format!("read transaction {hash}"))?;
    let steps = tx["receipts_outcome"]
        .as_array()
        .context("transaction has no receipts")?
        .iter()
        .map(|r| {
            let o = &r["outcome"];
            TxStep {
                executor: o["executor_id"].as_str().unwrap_or("?").to_string(),
                gas_burnt: o["gas_burnt"].as_u64().unwrap_or(0),
                events: o["logs"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|l| l.as_str()?.strip_prefix("EVENT_JSON:"))
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect(),
            }
        })
        .collect();
    let t = &tx["transaction"];
    Ok(TxEvidence {
        conversion_gas: tx["transaction_outcome"]["outcome"]["gas_burnt"]
            .as_u64()
            .unwrap_or(0),
        hash: hash.to_string(),
        signer: t["signer_id"].as_str().unwrap_or("?").to_string(),
        receiver: t["receiver_id"].as_str().unwrap_or("?").to_string(),
        method: t["actions"][0]["FunctionCall"]["method_name"]
            .as_str()
            .unwrap_or("?")
            .to_string(),
        steps,
    })
}

/// The id of the chain behind an RPC endpoint, for example `testnet`.
pub async fn chain_id(rpc_url: &str) -> Result<String> {
    let status = rpc(rpc_url, "status", json!([])).await?;
    Ok(status["chain_id"]
        .as_str()
        .context("status has no chain_id")?
        .to_string())
}

/// An account's access keys, as `view_access_key_list` reports them.
pub async fn access_keys(rpc_url: &str, account: &AccountId) -> Result<Value> {
    rpc(
        rpc_url,
        "query",
        json!({
            "request_type": "view_access_key_list",
            "finality": "final",
            "account_id": account,
        }),
    )
    .await
}

/// An account's balance of a NEP-141 token.
pub async fn token_balance(
    network: &NetworkConfig,
    token: &AccountId,
    account: &AccountId,
) -> Result<u128> {
    let v: near_api::Data<String> = near_api::Contract(token.clone())
        .call_function("ft_balance_of", json!({ "account_id": account }))
        .read_only()
        .fetch_from(network)
        .await
        .with_context(|| format!("read the balance of {account}"))?;
    Ok(v.data.parse()?)
}

/// What a wallet file needs from the contract's view of the agent.
pub fn chain_agent(view: &ZkAgentView) -> Result<ChainAgent> {
    Ok(ChainAgent {
        policy_commitment: fr_from_base64(&view.policy_commitment)?,
        state_commitment: fr_from_base64(&view.state_commitment)?,
        in_flight: view.pending_log_id.is_some(),
    })
}

/// Every key and value in a contract's storage, concatenated, for checking
/// what is and is not on chain.
pub async fn contract_storage(rpc_url: &str, account: &AccountId) -> Result<Vec<u8>> {
    let result = rpc(
        rpc_url,
        "query",
        json!({
            "request_type": "view_state",
            "finality": "final",
            "account_id": account,
            "prefix_base64": "",
        }),
    )
    .await?;
    let mut out = Vec::new();
    for item in result["values"].as_array().context("no storage values")? {
        for field in ["key", "value"] {
            let s = item[field].as_str().context("storage entry field")?;
            out.extend(B64.decode(s).context("storage entry is not base64")?);
        }
    }
    Ok(out)
}

/// True if `needle` occurs anywhere in `haystack`.
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bytes_only_where_they_occur() {
        assert!(contains(b"abc-gpu-cloud", b"gpu-cloud"));
        assert!(!contains(b"abc", b"gpu-cloud"));
        assert!(!contains(b"abc", b""));
    }

    #[test]
    fn evidence_reads_transfers_from_the_token_only() {
        let event = json!({ "standard": "nep141", "event": "ft_transfer", "data": [
            { "new_owner_id": "data-api.near", "amount": "50000" }
        ] });
        let step = |executor: &str| TxStep {
            executor: executor.into(),
            gas_burnt: 5,
            events: vec![event.clone()],
        };
        let tx = TxEvidence {
            hash: "h".into(),
            signer: "a".into(),
            receiver: "p".into(),
            method: "pay_zk".into(),
            conversion_gas: 1,
            steps: vec![step("fake-token.near"), step("usdc.near")],
        };
        assert_eq!(
            tx.transfers(&"usdc.near".parse().unwrap()),
            vec![("data-api.near".to_string(), 50_000)]
        );
        assert_eq!(tx.total_gas(), 11);
    }
}

//! Checks, read-only, whether the runbook allows `reconcile_payment` for one log
//! entry, and which claim. It has no signer and sends no transaction: it calls
//! only the RPC methods `query`, `block` and `EXPERIMENTAL_tx_status`.
//!
//!   reconcile-check --policy nearx402-policy.testnet --log-id 3 \
//!       --tx-hash <hash> --sender nearx402-agent.testnet
//!
//! Exit 0 with a recommended claim, 3 with a refusal. A usage error exits 2.
//!
//! An operational control: the contract does not verify that the transaction is
//! FINAL. See docs/reserved-recovery.md.

use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use clap::Parser;
use nearx402::policy::LogEntry;
use nearx402::reconcile_check::{check, rpc_error, Expected, Refusal};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(about = "Checks the reconcile_payment precondition, read-only")]
struct Args {
    /// RPC endpoint. An archival one may be needed for an old transaction.
    #[arg(
        long,
        env = "NEAR_RPC_URL",
        default_value = "https://rpc.testnet.near.org"
    )]
    rpc: String,
    /// The policy contract.
    #[arg(long)]
    policy: String,
    /// The log entry to reconcile.
    #[arg(long)]
    log_id: u32,
    /// Hash of the agent's `pay` or `pay_zk` transaction.
    #[arg(long)]
    tx_hash: String,
    /// The agent that signed it.
    #[arg(long)]
    sender: String,
}

/// The JSON-RPC `result`, or the `error` object.
async fn rpc(
    http: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Result<Value, Value>> {
    let r: Value = http
        .post(url)
        .json(&json!({ "jsonrpc": "2.0", "id": "reconcile-check", "method": method, "params": params }))
        .send()
        .await
        .with_context(|| format!("{method}: request failed"))?
        .json()
        .await
        .with_context(|| format!("{method}: response is not JSON"))?;
    Ok(match r.get("error") {
        Some(e) => Err(e.clone()),
        None => Ok(r["result"].clone()),
    })
}

async fn view(http: &reqwest::Client, a: &Args, method: &str, args: Value) -> Result<Value> {
    let result = rpc(
        http,
        &a.rpc,
        "query",
        json!({
            "request_type": "call_function", "finality": "final",
            "account_id": a.policy, "method_name": method,
            "args_base64": B64.encode(args.to_string()),
        }),
    )
    .await?
    .map_err(|e| anyhow!("{method}: {e}"))?;
    let bytes: Vec<u8> = serde_json::from_value(result["result"].clone())
        .with_context(|| format!("{method}: no result bytes"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn run(a: &Args) -> Result<Result<(Expected, nearx402::reconcile_check::Evidence), Refusal>> {
    let http = reqwest::Client::new();
    let token: String = serde_json::from_value(view(&http, a, "get_token_id", json!({})).await?)?;
    let entry: Option<LogEntry> =
        serde_json::from_value(view(&http, a, "get_log_entry", json!({ "id": a.log_id })).await?)?;
    let block = rpc(&http, &a.rpc, "block", json!({ "finality": "final" }))
        .await?
        .map_err(|e| anyhow!("block: {e}"))?;
    let now: u64 = block["header"]["timestamp_nanosec"]
        .as_str()
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| anyhow!("block: no timestamp"))?;
    let expected = Expected {
        policy: a.policy.clone(),
        token,
        sender: a.sender.clone(),
        log_id: a.log_id,
    };
    let status = rpc(
        &http,
        &a.rpc,
        "EXPERIMENTAL_tx_status",
        json!({ "tx_hash": a.tx_hash, "sender_account_id": a.sender, "wait_until": "FINAL" }),
    )
    .await?;
    let status = match status {
        Ok(s) => s,
        Err(e) => return Ok(Err(rpc_error(&e))),
    };
    if status["transaction"]["hash"].as_str() != Some(a.tx_hash.as_str()) {
        return Ok(Err(Refusal::Mismatch(
            "the RPC returned a different transaction".into(),
        )));
    }
    Ok(check(&expected, entry.as_ref(), &status, now).map(|ev| (expected, ev)))
}

#[tokio::main]
async fn main() -> ExitCode {
    let a = Args::parse();
    println!("reconcile-check: read-only. It sends nothing.");
    println!("The contract does not verify FINAL; this check is the operator's control.");
    match run(&a).await {
        Err(e) => {
            println!("REFUSED: could not read the chain: {e:#}");
            println!("Do not call reconcile_payment.");
            ExitCode::from(3)
        }
        Ok(Err(refusal)) => {
            println!("REFUSED: {refusal}");
            println!("Do not call reconcile_payment.");
            ExitCode::from(3)
        }
        Ok(Ok((expected, ev))) => {
            println!("transaction      {} ({}), FINAL", ev.tx_hash, ev.method);
            println!("pay receipt      {}", ev.pay_receipt);
            println!(
                "ft_transfer      {} in block {}: {}",
                ev.transfer_receipt,
                ev.transfer_block,
                match &ev.transfer_failure {
                    None => "SuccessValue, the tokens moved".to_string(),
                    Some(f) => format!("Failure, the tokens did not move: {f}"),
                }
            );
            println!(
                "on_pay_transfer  {} in block {}: Failure {}",
                ev.callback_receipt, ev.callback_block, ev.callback_failure
            );
            println!(
                "RECOMMENDED, not sent: {} reconcile_payment {}",
                expected.policy,
                ev.claim_args(expected.log_id)
            );
            ExitCode::SUCCESS
        }
    }
}

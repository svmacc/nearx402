//! Read-only check of the precondition for `reconcile_payment`.
//!
//! **An operational control, not a contract guarantee.** The contract cannot read
//! an old receipt's outcome and does not check that a transaction is `FINAL`. This
//! module reads the payment's transaction from an RPC node and says whether the
//! runbook in docs/reserved-recovery.md allows a claim, and which one. It never
//! sends a transaction.
//!
//! It fails closed: anything missing, ambiguous or mismatched is a refusal, and so
//! is a callback that succeeded. Events are corroboration only where the receipt
//! that emits them succeeded: a failed callback emits nothing.

use std::collections::{HashMap, HashSet};
use std::fmt;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::Value;

use crate::policy::{LogEntry, Status};

/// What the operator says the payment is.
#[derive(Clone, Debug)]
pub struct Expected {
    pub policy: String,
    pub token: String,
    pub sender: String,
    pub log_id: u32,
}

/// Why the checker will not recommend a claim. The operator must not call
/// `reconcile_payment` while any of these holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The RPC did not return the transaction at `FINAL`: a timeout, an unknown or
    /// pruned transaction, or another error. A timeout does not mean a failure.
    Rpc(String),
    NotFinal(String),
    Entry(String),
    Transaction(String),
    Incomplete(String),
    Ambiguous(String),
    Mismatch(String),
    /// The callback ran, so the entry should not be `Pending`. Investigate.
    CallbackSucceeded,
    /// The transfer's outcome is not `SuccessValue` or `Failure`.
    TransferUnclear(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Rpc(m) => write!(f, "rpc: {m}"),
            Refusal::NotFinal(m) => write!(f, "not FINAL: {m}"),
            Refusal::Entry(m) => write!(f, "log entry: {m}"),
            Refusal::Transaction(m) => write!(f, "transaction: {m}"),
            Refusal::Incomplete(m) => write!(f, "incomplete: {m}"),
            Refusal::Ambiguous(m) => write!(f, "ambiguous: {m}"),
            Refusal::Mismatch(m) => write!(f, "mismatch: {m}"),
            Refusal::CallbackSucceeded => write!(
                f,
                "on_pay_transfer succeeded, so the entry should not be Pending: investigate, do not reconcile"
            ),
            Refusal::TransferUnclear(m) => write!(f, "transfer outcome unclear: {m}"),
        }
    }
}

/// What the checker saw, for the operator's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub tx_hash: String,
    pub method: String,
    pub pay_receipt: String,
    pub transfer_receipt: String,
    pub transfer_block: String,
    /// `true` if `ft_transfer` returned `SuccessValue`, `false` if it failed.
    pub tokens_moved: bool,
    pub transfer_failure: Option<String>,
    pub callback_receipt: String,
    pub callback_block: String,
    pub callback_failure: String,
}

impl Evidence {
    /// The arguments for `reconcile_payment`. Printed, never sent.
    pub fn claim_args(&self, log_id: u32) -> String {
        format!("{{\"id\":{log_id},\"tokens_moved\":{}}}", self.tokens_moved)
    }
}

type Check<T> = Result<T, Refusal>;

fn s<'a>(v: &'a Value, key: &str) -> Check<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::Incomplete(format!("missing `{key}`")))
}

/// A receipt's single function call: method, decoded arguments, deposit.
fn single_call(actions: &Value, what: &str) -> Check<(String, Value, String)> {
    let actions = actions
        .as_array()
        .ok_or_else(|| Refusal::Incomplete(format!("{what}: no actions")))?;
    if actions.len() != 1 {
        return Err(Refusal::Ambiguous(format!(
            "{what}: {} actions, expected one function call",
            actions.len()
        )));
    }
    let call = actions[0]
        .get("FunctionCall")
        .filter(|c| c.is_object())
        .ok_or_else(|| Refusal::Mismatch(format!("{what}: not a function call")))?;
    let method = s(call, "method_name")?.to_string();
    let raw = B64
        .decode(s(call, "args")?)
        .map_err(|_| Refusal::Incomplete(format!("{what}: arguments are not base64")))?;
    let args: Value = serde_json::from_slice(&raw)
        .map_err(|_| Refusal::Mismatch(format!("{what}: arguments are not JSON")))?;
    let deposit = call
        .get("deposit")
        .map(|d| {
            d.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| d.to_string())
        })
        .unwrap_or_default();
    Ok((method, args, deposit))
}

fn calls(r: &Value) -> Option<&Value> {
    r.get("receipt")?.get("Action")?.get("actions")
}

fn is_call_to(r: &Value, method: &str) -> bool {
    calls(r).and_then(Value::as_array).is_some_and(|a| {
        a.iter().any(|x| {
            x.get("FunctionCall")
                .and_then(|c| c.get("method_name"))
                .and_then(Value::as_str)
                == Some(method)
        })
    })
}

fn want(field: &str, got: &Value, expected: &str) -> Check<()> {
    if got.as_str() == Some(expected) {
        Ok(())
    } else {
        Err(Refusal::Mismatch(format!(
            "{field} is {got}, expected \"{expected}\""
        )))
    }
}

fn failure_text(status: &Value) -> String {
    let text = status["Failure"].to_string();
    if text.chars().count() > 300 {
        format!("{}...", text.chars().take(300).collect::<String>())
    } else {
        text
    }
}

/// The age the contract requires before a claim, in nanoseconds.
pub const RECONCILE_AFTER_NANOS: u64 = 86_400 * 1_000_000_000;

/// Checks `entry`, read at `final` from the policy, and `tx_status`, the `result`
/// of `EXPERIMENTAL_tx_status` with `wait_until: "FINAL"`. `now` is the timestamp
/// of a final block, in nanoseconds.
pub fn check(
    expected: &Expected,
    entry: Option<&LogEntry>,
    tx_status: &Value,
    now: u64,
) -> Check<Evidence> {
    let id = expected.log_id;

    // The entry, as the contract holds it now.
    let entry = entry.ok_or_else(|| Refusal::Entry(format!("no log entry {id}")))?;
    if entry.id != id {
        return Err(Refusal::Entry(format!("entry id {} is not {id}", entry.id)));
    }
    if entry.status != Status::Pending {
        return Err(Refusal::Entry(format!(
            "entry {id} is {:?}, not Pending",
            entry.status
        )));
    }
    if now.saturating_sub(entry.timestamp) < RECONCILE_AFTER_NANOS {
        return Err(Refusal::Entry(format!(
            "entry {id} is younger than 24 hours, so the contract would refuse it"
        )));
    }
    if entry.agent_id.as_str() != expected.sender {
        return Err(Refusal::Entry(format!(
            "entry {id} belongs to {}, not {}",
            entry.agent_id, expected.sender
        )));
    }

    // FINAL: every receipt, including refunds, has finished.
    let level = tx_status
        .get("final_execution_status")
        .and_then(Value::as_str)
        .unwrap_or("missing");
    if level != "FINAL" {
        return Err(Refusal::NotFinal(format!(
            "final_execution_status is {level}"
        )));
    }

    // The transaction the agent signed.
    let tx = tx_status
        .get("transaction")
        .ok_or_else(|| Refusal::Incomplete("no transaction".into()))?;
    let tx_hash = s(tx, "hash")?.to_string();
    want("transaction signer", &tx["signer_id"], &expected.sender)?;
    want("transaction receiver", &tx["receiver_id"], &expected.policy)?;
    let (method, args, _) = single_call(&tx["actions"], "transaction")?;
    if method != "pay" && method != "pay_zk" {
        return Err(Refusal::Transaction(format!(
            "calls `{method}`, not `pay` or `pay_zk`"
        )));
    }
    want("payment vendor", &args["vendor"], entry.vendor.as_str())?;
    want("payment amount", &args["amount"], &entry.amount.to_string())?;
    let memo_matches = match (&args["memo"], &entry.memo) {
        (Value::Null, None) => true,
        (Value::String(a), Some(b)) => a == b,
        _ => false,
    };
    if !memo_matches {
        return Err(Refusal::Mismatch(format!(
            "payment memo is {}, the entry has {:?}",
            args["memo"], entry.memo
        )));
    }

    // Every receipt, and every outcome, once.
    let receipts = tx_status
        .get("receipts")
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal::Incomplete("no receipts".into()))?;
    let outcomes = tx_status
        .get("receipts_outcome")
        .and_then(Value::as_array)
        .ok_or_else(|| Refusal::Incomplete("no receipt outcomes".into()))?;
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    for r in receipts {
        if by_id.insert(s(r, "receipt_id")?, r).is_some() {
            return Err(Refusal::Ambiguous("a receipt appears twice".into()));
        }
    }
    let mut outcome_of: HashMap<&str, &Value> = HashMap::new();
    for o in outcomes {
        if outcome_of.insert(s(o, "id")?, o).is_some() {
            return Err(Refusal::Ambiguous("an outcome appears twice".into()));
        }
    }
    let ids: HashSet<&str> = by_id.keys().copied().collect();
    if ids != outcome_of.keys().copied().collect() {
        return Err(Refusal::Incomplete(
            "receipts and outcomes do not match one to one".into(),
        ));
    }
    let children = |o: &Value| -> Check<Vec<String>> {
        o["outcome"]["receipt_ids"]
            .as_array()
            .ok_or_else(|| Refusal::Incomplete("an outcome lists no receipt_ids".into()))?
            .iter()
            .map(|c| {
                c.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Refusal::Incomplete("bad receipt id".into()))
            })
            .collect()
    };
    for o in outcomes {
        for c in children(o)? {
            if !ids.contains(c.as_str()) {
                return Err(Refusal::Incomplete(format!("receipt {c} is missing")));
            }
        }
    }

    // The receipt the transaction became.
    let first = children(&tx_status["transaction_outcome"])?;
    let [pay_id] = first.as_slice() else {
        return Err(Refusal::Ambiguous(format!(
            "the transaction produced {} receipts, expected one",
            first.len()
        )));
    };
    let pay = by_id[pay_id.as_str()];
    want(
        "pay receipt predecessor",
        &pay["predecessor_id"],
        &expected.sender,
    )?;
    want(
        "pay receipt receiver",
        &pay["receiver_id"],
        &expected.policy,
    )?;
    let pay_outcome = &outcome_of[pay_id.as_str()]["outcome"];
    if pay_outcome["status"].get("Failure").is_some() {
        return Err(Refusal::Transaction(
            "the pay receipt failed, so this transaction left nothing Pending".into(),
        ));
    }
    want(
        "pay receipt executor",
        &pay_outcome["executor_id"],
        &expected.policy,
    )?;

    // The pay receipt succeeded, so it emitted `payment_approved` for this entry.
    let approved: Vec<Value> = pay_outcome["logs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e["standard"] == "nearx402_policy" && e["event"] == "payment_approved")
        .collect();
    let [event] = approved.as_slice() else {
        return Err(Refusal::Ambiguous(format!(
            "{} payment_approved events in the pay receipt, expected one",
            approved.len()
        )));
    };
    let data = &event["data"][0];
    if data["id"].as_u64() != Some(u64::from(id))
        || data["timestamp"].as_u64() != Some(entry.timestamp)
        || data["agent_id"].as_str() != Some(entry.agent_id.as_str())
        || data["vendor"].as_str() != Some(entry.vendor.as_str())
        || data["amount"].as_str() != Some(entry.amount.to_string().as_str())
    {
        return Err(Refusal::Mismatch(format!(
            "payment_approved {data} does not match entry {id}"
        )));
    }

    // Exactly one transfer and one callback, in the whole graph.
    let only = |method: &str| -> Check<&Value> {
        let found: Vec<&Value> = receipts.iter().filter(|r| is_call_to(r, method)).collect();
        match found.as_slice() {
            [one] => Ok(*one),
            [] => Err(Refusal::Incomplete(format!("no `{method}` receipt"))),
            many => Err(Refusal::Ambiguous(format!(
                "{} `{method}` receipts",
                many.len()
            ))),
        }
    };
    let transfer = only("ft_transfer")?;
    let callback = only("on_pay_transfer")?;
    let transfer_id = s(transfer, "receipt_id")?.to_string();
    let callback_id = s(callback, "receipt_id")?.to_string();
    let pay_children = children(outcome_of[pay_id.as_str()])?;
    for (what, rid) in [
        ("ft_transfer", &transfer_id),
        ("on_pay_transfer", &callback_id),
    ] {
        if !pay_children.contains(rid) {
            return Err(Refusal::Mismatch(format!(
                "the {what} receipt was not created by the pay receipt"
            )));
        }
    }
    for c in &pay_children {
        if c != &transfer_id && c != &callback_id && by_id[c.as_str()]["predecessor_id"] != "system"
        {
            return Err(Refusal::Ambiguous(format!(
                "the pay receipt created another receipt, {c}"
            )));
        }
    }

    // The transfer.
    want(
        "ft_transfer predecessor",
        &transfer["predecessor_id"],
        &expected.policy,
    )?;
    want(
        "ft_transfer receiver",
        &transfer["receiver_id"],
        &expected.token,
    )?;
    let (_, targs, deposit) = single_call(calls(transfer).unwrap_or(&Value::Null), "ft_transfer")?;
    want(
        "ft_transfer receiver_id",
        &targs["receiver_id"],
        entry.vendor.as_str(),
    )?;
    want(
        "ft_transfer amount",
        &targs["amount"],
        &entry.amount.to_string(),
    )?;
    want(
        "ft_transfer memo",
        &targs["memo"],
        &format!("nearx402:{id}"),
    )?;
    if deposit != "1" {
        return Err(Refusal::Mismatch(format!(
            "ft_transfer deposit is {deposit}, expected 1 yoctoNEAR"
        )));
    }
    let toutcome = &outcome_of[transfer_id.as_str()];
    want(
        "ft_transfer executor",
        &toutcome["outcome"]["executor_id"],
        &expected.token,
    )?;

    // The callback, and that it waits on this transfer's result.
    want(
        "callback predecessor",
        &callback["predecessor_id"],
        &expected.policy,
    )?;
    want(
        "callback receiver",
        &callback["receiver_id"],
        &expected.policy,
    )?;
    let (_, cargs, _) = single_call(calls(callback).unwrap_or(&Value::Null), "on_pay_transfer")?;
    if cargs != serde_json::json!({ "id": id }) {
        return Err(Refusal::Mismatch(format!(
            "on_pay_transfer arguments are {cargs}, expected {{\"id\":{id}}}"
        )));
    }
    let receivers = transfer["receipt"]["Action"]["output_data_receivers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let inputs = callback["receipt"]["Action"]["input_data_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let linked = match (receivers.as_slice(), inputs.as_slice()) {
        ([out], [input]) => {
            out["data_id"] == *input && out["receiver_id"].as_str() == Some(&expected.policy)
        }
        _ => false,
    };
    if !linked {
        return Err(Refusal::Mismatch(
            "the callback does not take its input from this ft_transfer".into(),
        ));
    }

    let coutcome = &outcome_of[callback_id.as_str()];
    let cstatus = &coutcome["outcome"]["status"];
    if cstatus.get("Failure").is_none() {
        if cstatus.get("SuccessValue").is_some() || cstatus.get("SuccessReceiptId").is_some() {
            return Err(Refusal::CallbackSucceeded);
        }
        return Err(Refusal::Incomplete(format!(
            "on_pay_transfer status is {cstatus}"
        )));
    }

    let tstatus = &toutcome["outcome"]["status"];
    let (tokens_moved, transfer_failure) = if tstatus.get("SuccessValue").is_some() {
        (true, None)
    } else if tstatus.get("Failure").is_some() {
        (false, Some(failure_text(tstatus)))
    } else {
        return Err(Refusal::TransferUnclear(tstatus.to_string()));
    };

    Ok(Evidence {
        tx_hash,
        method,
        pay_receipt: pay_id.clone(),
        transfer_receipt: transfer_id,
        transfer_block: s(toutcome, "block_hash")?.to_string(),
        tokens_moved,
        transfer_failure,
        callback_receipt: callback_id,
        callback_block: s(coutcome, "block_hash")?.to_string(),
        callback_failure: failure_text(cstatus),
    })
}

/// Turns an RPC error object into a refusal. A timeout is reported as such: it
/// does not mean the transfer failed.
pub fn rpc_error(error: &Value) -> Refusal {
    let name = error["cause"]["name"].as_str().unwrap_or("");
    match name {
        "TIMEOUT_ERROR" => Refusal::Rpc(
            "timed out before FINAL. This does not mean the transfer failed. Stop, and try later"
                .into(),
        ),
        "UNKNOWN_TRANSACTION" => Refusal::Rpc(
            "transaction not found. It may be pruned: try an archival RPC. Do not reconcile".into(),
        ),
        _ => Refusal::Rpc(format!("error {error}")),
    }
}

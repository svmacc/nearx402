//! The reconcile checker against sanitized fixtures, and live against a sandbox.
//!
//! The fixtures in `tests/fixtures/reconcile` are real `EXPERIMENTAL_tx_status`
//! responses at `FINAL`, captured by `reconcile_capture.rs`; `pay_zk` arguments
//! are cut down to vendor, amount and memo. In each, the callback succeeded, as
//! it does on a healthy chain. No callback could be made to fail by itself, so the
//! cases the checker accepts are those responses with exactly one edit: the
//! callback's status set to a `Failure`, and the entry set back to `Pending`, as
//! a lost callback would leave them. Every other case is one further edit.

mod common;

use std::process::Command;

use common::{lose_callback, DAY_NANOS};
use nearx402::policy::{LogEntry, Status};
use nearx402::reconcile_check::{check, rpc_error, Evidence, Expected, Refusal};
use nearx402::world::World;
use serde_json::{json, Value};

struct Case {
    expected: Expected,
    entry: Option<LogEntry>,
    status: Value,
    now: u64,
}

impl Case {
    /// The response as captured, with the entry `Pending` and a day old.
    fn real(name: &str) -> Case {
        let path = format!(
            "{}/tests/fixtures/reconcile/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let f: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut entry: LogEntry = serde_json::from_value(f["entry"].clone()).unwrap();
        entry.status = Status::Pending;
        entry.reason = None;
        entry.settled_at = None;
        let now = entry.timestamp + DAY_NANOS;
        Case {
            expected: Expected {
                policy: f["policy"].as_str().unwrap().into(),
                token: f["token"].as_str().unwrap().into(),
                sender: f["sender"].as_str().unwrap().into(),
                log_id: f["log_id"].as_u64().unwrap() as u32,
            },
            entry: Some(entry),
            status: f["tx_status"].clone(),
            now,
        }
    }

    /// As a lost callback leaves it: the callback's receipt failed.
    fn lost(name: &str) -> Case {
        let mut c = Case::real(name);
        *c.outcome_status("on_pay_transfer") = json!({ "Failure": { "ActionError": {
            "index": 0,
            "kind": { "FunctionCallError": { "ExecutionError": "Exceeded the prepaid gas." } }
        } } });
        c
    }

    fn run(&self) -> Result<Evidence, Refusal> {
        check(&self.expected, self.entry.as_ref(), &self.status, self.now)
    }

    fn receipt_index(&self, method: &str) -> usize {
        self.status["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .position(|r| {
                r["receipt"]["Action"]["actions"][0]["FunctionCall"]["method_name"] == method
            })
            .unwrap_or_else(|| panic!("no {method} receipt"))
    }

    fn receipt(&mut self, method: &str) -> &mut Value {
        let i = self.receipt_index(method);
        &mut self.status["receipts"][i]
    }

    fn outcome(&mut self, method: &str) -> &mut Value {
        let i = self.receipt_index(method);
        let id = self.status["receipts"][i]["receipt_id"].clone();
        self.status["receipts_outcome"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|o| o["id"] == id)
            .unwrap()
    }

    fn outcome_status(&mut self, method: &str) -> &mut Value {
        &mut self.outcome(method)["outcome"]["status"]
    }

    /// Rewrites one argument of a receipt's (or the transaction's) call.
    fn set_arg(call: &mut Value, key: &str, value: Value) {
        use base64::Engine;
        let b64 = &base64::engine::general_purpose::STANDARD;
        let mut args: Value =
            serde_json::from_slice(&b64.decode(call["args"].as_str().unwrap()).unwrap()).unwrap();
        args[key] = value;
        call["args"] = json!(b64.encode(args.to_string()));
    }

    fn set_receipt_arg(&mut self, method: &str, key: &str, value: Value) {
        let r = self.receipt(method);
        Self::set_arg(
            &mut r["receipt"]["Action"]["actions"][0]["FunctionCall"],
            key,
            value,
        );
    }

    fn set_tx_arg(&mut self, key: &str, value: Value) {
        Self::set_arg(
            &mut self.status["transaction"]["actions"][0]["FunctionCall"],
            key,
            value,
        );
    }

    fn remove_receipt(&mut self, method: &str, outcome_too: bool) {
        let i = self.receipt_index(method);
        let id = self.status["receipts"][i]["receipt_id"].clone();
        self.status["receipts"].as_array_mut().unwrap().remove(i);
        if outcome_too {
            self.status["receipts_outcome"]
                .as_array_mut()
                .unwrap()
                .retain(|o| o["id"] != id);
        }
    }
}

const FIXTURES: [&str; 3] = [
    "pay_transfer_ok",
    "pay_transfer_failed",
    "pay_zk_transfer_ok",
];

fn refused(c: &Case) -> Refusal {
    c.run().expect_err("must refuse")
}

#[test]
fn a_real_response_with_its_callback_succeeded_is_refused() {
    for name in FIXTURES {
        assert_eq!(
            refused(&Case::real(name)),
            Refusal::CallbackSucceeded,
            "{name}"
        );
    }
}

#[test]
fn a_lost_callback_after_a_transfer_that_moved_recommends_true() {
    for name in ["pay_transfer_ok", "pay_zk_transfer_ok"] {
        let ev = Case::lost(name).run().unwrap();
        assert!(ev.tokens_moved, "{name}");
        assert_eq!(ev.transfer_failure, None);
        assert!(ev.callback_failure.contains("Exceeded the prepaid gas"));
        assert!(ev
            .claim_args(Case::real(name).expected.log_id)
            .ends_with("\"tokens_moved\":true}"));
    }
    assert_eq!(
        Case::lost("pay_zk_transfer_ok").run().unwrap().method,
        "pay_zk"
    );
}

#[test]
fn a_lost_callback_after_a_transfer_that_failed_recommends_false() {
    let ev = Case::lost("pay_transfer_failed").run().unwrap();
    assert!(!ev.tokens_moved);
    assert_eq!(ev.claim_args(1), "{\"id\":1,\"tokens_moved\":false}");
    assert!(ev.transfer_failure.unwrap().contains("is not registered"));
}

/// Every edit below is to an otherwise accepted case, so the refusal is caused by
/// that edit alone.
fn each_lost(edit: impl Fn(&mut Case), want: impl Fn(&Refusal) -> bool, what: &str) {
    for name in FIXTURES {
        let mut c = Case::lost(name);
        assert!(c.run().is_ok(), "{name}: the base case must pass");
        edit(&mut c);
        let r = refused(&c);
        assert!(want(&r), "{what}, {name}: got {r:?}");
    }
}

#[test]
fn not_final_is_refused() {
    for level in [
        json!("EXECUTED_OPTIMISTIC"),
        json!("EXECUTED"),
        json!("INCLUDED_FINAL"),
        Value::Null,
    ] {
        each_lost(
            |c| c.status["final_execution_status"] = level.clone(),
            |r| matches!(r, Refusal::NotFinal(_)),
            "not final",
        );
    }
    each_lost(
        |c| {
            c.status
                .as_object_mut()
                .unwrap()
                .remove("final_execution_status");
        },
        |r| matches!(r, Refusal::NotFinal(_)),
        "no level",
    );
}

#[test]
fn rpc_errors_are_refusals_and_a_timeout_is_not_a_failure() {
    let timeout =
        rpc_error(&json!({ "name": "HANDLER_ERROR", "cause": { "name": "TIMEOUT_ERROR" } }));
    assert!(
        timeout
            .to_string()
            .contains("does not mean the transfer failed"),
        "{timeout}"
    );
    let unknown = rpc_error(&json!({ "cause": { "name": "UNKNOWN_TRANSACTION", "info": {} } }));
    assert!(unknown.to_string().contains("archival"), "{unknown}");
    assert!(matches!(
        rpc_error(&json!({ "code": -32000 })),
        Refusal::Rpc(_)
    ));
}

#[test]
fn the_entry_must_be_the_pending_one_of_this_agent_and_old_enough() {
    let entry = |c: &mut Case| c.entry.as_mut().unwrap().clone();
    each_lost(
        |c| c.entry = None,
        |r| matches!(r, Refusal::Entry(_)),
        "no entry",
    );
    for s in [Status::Paid, Status::Failed, Status::Blocked] {
        each_lost(
            |c| c.entry.as_mut().unwrap().status = s,
            |r| matches!(r, Refusal::Entry(_)),
            "not pending",
        );
    }
    each_lost(
        |c| c.entry.as_mut().unwrap().id += 1,
        |r| matches!(r, Refusal::Entry(_)),
        "other id",
    );
    each_lost(
        |c| c.entry.as_mut().unwrap().agent_id = "someone.sandbox".parse().unwrap(),
        |r| matches!(r, Refusal::Entry(_)),
        "other agent",
    );
    each_lost(
        |c| c.now = entry(c).timestamp + DAY_NANOS - 1,
        |r| matches!(r, Refusal::Entry(m) if m.contains("24 hours")),
        "too young",
    );
}

#[test]
fn the_transaction_must_be_this_agents_payment_to_this_policy() {
    each_lost(
        |c| c.status["transaction"]["signer_id"] = json!("someone.sandbox"),
        |r| matches!(r, Refusal::Mismatch(_)),
        "signer",
    );
    each_lost(
        |c| c.status["transaction"]["receiver_id"] = json!("other.sandbox"),
        |r| matches!(r, Refusal::Mismatch(_)),
        "receiver",
    );
    each_lost(
        |c| {
            c.status["transaction"]["actions"][0]["FunctionCall"]["method_name"] = json!("withdraw")
        },
        |r| matches!(r, Refusal::Transaction(_)),
        "method",
    );
    each_lost(
        |c| {
            let a = c.status["transaction"]["actions"][0].clone();
            c.status["transaction"]["actions"]
                .as_array_mut()
                .unwrap()
                .push(a);
        },
        |r| matches!(r, Refusal::Ambiguous(_)),
        "two actions",
    );
    each_lost(
        |c| c.set_tx_arg("vendor", json!("other.sandbox")),
        |r| matches!(r, Refusal::Mismatch(_)),
        "vendor",
    );
    each_lost(
        |c| c.set_tx_arg("amount", json!("49999")),
        |r| matches!(r, Refusal::Mismatch(_)),
        "amount",
    );
    each_lost(
        |c| c.set_tx_arg("memo", json!("other")),
        |r| matches!(r, Refusal::Mismatch(_)),
        "memo",
    );
    each_lost(
        |c| c.expected.policy = "other.sandbox".into(),
        |r| matches!(r, Refusal::Mismatch(_)),
        "another policy",
    );
}

#[test]
fn the_pay_receipt_must_have_succeeded_and_announced_this_entry() {
    each_lost(
        |c| {
            let m = c.status["transaction"]["actions"][0]["FunctionCall"]["method_name"].clone();
            let m = m.as_str().unwrap().to_string();
            *c.outcome_status(&m) =
                json!({ "Failure": { "ActionError": { "index": 0, "kind": "x" } } });
        },
        |r| matches!(r, Refusal::Transaction(_)),
        "pay failed",
    );
    fn pay_logs(c: &mut Case) -> &mut Value {
        let m = c.status["transaction"]["actions"][0]["FunctionCall"]["method_name"]
            .as_str()
            .unwrap()
            .to_string();
        &mut c.outcome(&m)["outcome"]["logs"]
    }
    each_lost(
        |c| {
            pay_logs(c)
                .as_array_mut()
                .unwrap()
                .retain(|l| !l.as_str().unwrap().contains("payment_approved"))
        },
        |r| matches!(r, Refusal::Ambiguous(_)),
        "no event",
    );
    each_lost(
        |c| {
            let logs = pay_logs(c).as_array_mut().unwrap();
            let e = logs
                .iter()
                .find(|l| l.as_str().unwrap().contains("payment_approved"))
                .unwrap()
                .clone();
            logs.push(e);
        },
        |r| matches!(r, Refusal::Ambiguous(_)),
        "two events",
    );
    each_lost(
        |c| c.entry.as_mut().unwrap().timestamp -= 1,
        |r| matches!(r, Refusal::Mismatch(m) if m.contains("payment_approved")),
        "event timestamp",
    );
    each_lost(
        |c| {
            let extra = c.status["transaction_outcome"]["outcome"]["receipt_ids"][0].clone();
            c.status["transaction_outcome"]["outcome"]["receipt_ids"]
                .as_array_mut()
                .unwrap()
                .push(extra);
        },
        |r| matches!(r, Refusal::Ambiguous(_)),
        "two first receipts",
    );
}

#[test]
fn a_missing_or_duplicated_receipt_is_refused() {
    for method in ["ft_transfer", "on_pay_transfer"] {
        each_lost(
            |c| c.remove_receipt(method, true),
            |r| matches!(r, Refusal::Incomplete(_)),
            "removed",
        );
        each_lost(
            |c| c.remove_receipt(method, false),
            |r| matches!(r, Refusal::Incomplete(_)),
            "receipt only",
        );
        each_lost(
            |c| {
                let mut copy = c.receipt(method).clone();
                let mut out = c.outcome(method).clone();
                copy["receipt_id"] = json!("11111111111111111111111111111111");
                out["id"] = json!("11111111111111111111111111111111");
                c.status["receipts"].as_array_mut().unwrap().push(copy);
                c.status["receipts_outcome"]
                    .as_array_mut()
                    .unwrap()
                    .push(out);
            },
            |r| matches!(r, Refusal::Ambiguous(_)),
            "duplicated",
        );
    }
    each_lost(
        |c| {
            let o = c.status["receipts_outcome"][0].clone();
            c.status["receipts_outcome"].as_array_mut().unwrap().push(o);
        },
        |r| matches!(r, Refusal::Ambiguous(_)),
        "outcome twice",
    );
    each_lost(
        |c| {
            c.outcome("ft_transfer")["outcome"]["receipt_ids"] =
                json!(["22222222222222222222222222222222"])
        },
        |r| matches!(r, Refusal::Incomplete(_)),
        "unknown child",
    );
}

#[test]
fn the_transfer_must_be_this_payment() {
    let m = |r: &Refusal| matches!(r, Refusal::Mismatch(_));
    each_lost(
        |c| c.set_receipt_arg("ft_transfer", "memo", json!("nearx402:99")),
        m,
        "memo id",
    );
    each_lost(
        |c| c.set_receipt_arg("ft_transfer", "memo", Value::Null),
        m,
        "no memo",
    );
    each_lost(
        |c| c.set_receipt_arg("ft_transfer", "amount", json!("1")),
        m,
        "amount",
    );
    each_lost(
        |c| c.set_receipt_arg("ft_transfer", "receiver_id", json!("other.sandbox")),
        m,
        "vendor",
    );
    each_lost(
        |c| c.receipt("ft_transfer")["receiver_id"] = json!("fake-usdc.sandbox"),
        m,
        "token",
    );
    each_lost(
        |c| c.expected.token = "fake-usdc.sandbox".into(),
        m,
        "expected token",
    );
    each_lost(
        |c| c.receipt("ft_transfer")["predecessor_id"] = json!("other.sandbox"),
        m,
        "predecessor",
    );
    each_lost(
        |c| {
            c.receipt("ft_transfer")["receipt"]["Action"]["actions"][0]["FunctionCall"]["deposit"] =
                json!("0")
        },
        m,
        "deposit",
    );
    each_lost(
        |c| c.outcome("ft_transfer")["outcome"]["executor_id"] = json!("other.sandbox"),
        m,
        "executor",
    );
    for status in [json!("Unknown"), json!({ "SuccessReceiptId": "x" })] {
        each_lost(
            |c| *c.outcome_status("ft_transfer") = status.clone(),
            |r| matches!(r, Refusal::TransferUnclear(_)),
            "unclear",
        );
    }
}

#[test]
fn the_callback_must_be_this_payments_and_have_failed() {
    let m = |r: &Refusal| matches!(r, Refusal::Mismatch(_));
    each_lost(
        |c| c.set_receipt_arg("on_pay_transfer", "id", json!(99)),
        m,
        "id",
    );
    each_lost(
        |c| {
            c.receipt("on_pay_transfer")["receipt"]["Action"]["input_data_ids"] =
                json!(["33333333333333333333333333333333"])
        },
        m,
        "not linked",
    );
    each_lost(
        |c| c.receipt("ft_transfer")["receipt"]["Action"]["output_data_receivers"] = json!([]),
        m,
        "no data receiver",
    );
    each_lost(
        |c| c.receipt("on_pay_transfer")["predecessor_id"] = json!("other.sandbox"),
        m,
        "predecessor",
    );
    each_lost(
        |c| *c.outcome_status("on_pay_transfer") = json!({ "SuccessValue": "" }),
        |r| *r == Refusal::CallbackSucceeded,
        "succeeded",
    );
    each_lost(
        |c| *c.outcome_status("on_pay_transfer") = json!("Unknown"),
        |r| matches!(r, Refusal::Incomplete(_)),
        "unknown",
    );
}

/// Added after a mutation run of the checker showed these three checks untested.
#[test]
fn the_graph_must_be_one_to_one_and_hang_from_the_pay_receipt() {
    for method in ["ft_transfer", "on_pay_transfer"] {
        // An outcome missing while its receipt is there.
        each_lost(
            |c| {
                let id = c.receipt(method)["receipt_id"].clone();
                c.status["receipts_outcome"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|o| o["id"] != id);
            },
            |r| matches!(r, Refusal::Incomplete(m) if m.contains("one to one")),
            "outcome only",
        );
        // A transfer or callback the pay receipt did not create.
        each_lost(
            |c| {
                let id = c.receipt(method)["receipt_id"].clone();
                let first = c.status["transaction_outcome"]["outcome"]["receipt_ids"][0].clone();
                let pay = c.status["receipts_outcome"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|o| o["id"] == first)
                    .unwrap();
                pay["outcome"]["receipt_ids"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|x| *x != id);
            },
            |r| matches!(r, Refusal::Mismatch(m) if m.contains("not created by the pay receipt")),
            "not a child",
        );
    }
    // The same receipt listed twice. A refund is used, so no other check sees it.
    each_lost(
        |c| {
            let refund = c.status["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["predecessor_id"] == "system")
                .unwrap()
                .clone();
            c.status["receipts"].as_array_mut().unwrap().push(refund);
        },
        |r| matches!(r, Refusal::Ambiguous(m) if m.contains("receipt appears twice")),
        "receipt twice",
    );
}

#[test]
fn the_fixtures_hold_no_proof() {
    for name in FIXTURES {
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/reconcile/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        // Every function call's arguments, decoded. (The RPC's own `proof` field,
        // a Merkle proof of each outcome, is public and kept.)
        let f: Value = serde_json::from_str(&raw).unwrap();
        let s = &f["tx_status"];
        let mut calls: Vec<&Value> = s["transaction"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .collect();
        for r in s["receipts"].as_array().unwrap() {
            calls.extend(r["receipt"]["Action"]["actions"].as_array().unwrap().iter());
        }
        for a in calls.into_iter().filter_map(|a| a.get("FunctionCall")) {
            use base64::Engine;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(a["args"].as_str().unwrap())
                .unwrap();
            let args: Value = serde_json::from_slice(&raw).unwrap();
            if a["method_name"] == "pay_zk" {
                let mut keys: Vec<&String> = args.as_object().unwrap().keys().collect();
                keys.sort();
                assert_eq!(keys, ["amount", "memo", "vendor"], "{name}");
            }
        }
        assert!(!raw.contains("\"FunctionCall\": null"), "{name}");
    }
}

/// The binary, against a real sandbox: it refuses what a healthy chain shows,
/// including a lost callback faked by patching storage, because the transaction
/// says otherwise.
#[tokio::test(flavor = "multi_thread")]
async fn the_binary_refuses_on_a_live_sandbox() {
    let w = World::start().await.expect("sandbox world");
    let http = reqwest::Client::new();
    let vendor = w.vendor("data-api").account_id.clone();
    let r = w
        .policy()
        .pay(&w.agent_id, w.agent_signer(), &vendor, 50_000, None)
        .await
        .unwrap();
    let run = |hash: &str, id: u32| {
        let out = Command::new(env!("CARGO_BIN_EXE_reconcile-check"))
            .args(["--rpc", &w.rpc_url, "--policy", w.policy_id.as_str()])
            .args([
                "--log-id",
                &id.to_string(),
                "--tx-hash",
                hash,
                "--sender",
                w.agent_id.as_str(),
            ])
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };

    let (code, out) = run(&r.tx_hash, r.outcome.log_id);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("is Paid, not Pending"), "{out}");
    assert!(out.contains("sends nothing"), "{out}");

    let (code, out) = run("11111111111111111111111111111111", r.outcome.log_id);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("REFUSED"), "{out}");

    // Storage says Pending and a day old; the transaction says the callback ran.
    let entry = w
        .policy()
        .log_entry(r.outcome.log_id)
        .await
        .unwrap()
        .unwrap();
    lose_callback(&http, &w, &entry, DAY_NANOS, true).await;
    let (code, out) = run(&r.tx_hash, r.outcome.log_id);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("payment_approved"), "{out}");
    assert!(!out.contains("RECOMMENDED"), "{out}");
}

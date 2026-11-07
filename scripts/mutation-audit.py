#!/usr/bin/env python3
"""Mutation audit of the policy circuit, pay_zk, the wallet file and the demo's
checks of a deployment.

Breaks one security check at a time and confirms that a test fails. A mutant
that survives is a constraint or check no test depends on, which is how an
under-constrained circuit stays hidden.

    python3 -I scripts/mutation-audit.py            # all, about an hour
    python3 -I scripts/mutation-audit.py memo day   # names containing a word

The script rewrites source files while it runs, and restores them afterward, so
do not review or edit them meanwhile, or run it in a copy of the repository. Run
scripts/build-contracts.sh first: the deployment mutants use a local sandbox.
Exits 0 only if every mutant was killed or is on the EQUIVALENT list.
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CIRCUIT = ROOT / "crates/zx402-zk/src/policy.rs"
CONTRACT = ROOT / "contracts/policy/src/lib.rs"
CIRCUIT_TEST = ["cargo", "test", "-p", "zx402-zk", "--lib", "policy"]
CONTRACT_TEST = ["cargo", "test", "-p", "policy", "--lib", "zk_tests"]
RECONCILE_TEST = ["cargo", "test", "-p", "policy", "--lib", "reconcile"]
STORE = ROOT / "crates/zx402-zk/src/store.rs"
STORE_TEST = ["cargo", "test", "-p", "zx402-zk", "--lib", "store"]
DEPLOYMENT = ROOT / "crates/nearx402/src/deployment.rs"
DEPLOYMENT_TEST = ["cargo", "test", "-p", "nearx402", "--lib", "deployment"]
DEMO = ROOT / "crates/nearx402/src/bin/demo-zk.rs"
REHEARSAL_TEST = ["cargo", "test", "-p", "nearx402", "--test", "zk_separate_runs"]
X402 = ROOT / "crates/nearx402/src/x402.rs"
X402_TEST = ["cargo", "test", "-p", "nearx402", "--lib", "x402"]
FACILITATOR = ROOT / "crates/nearx402/src/facilitator.rs"
THEFT_TEST = ["cargo", "test", "-p", "nearx402", "--test", "receipt_theft"]
POSEIDON_TEST = ["cargo", "test", "-p", "zx402-zk", "--lib", "poseidon_parameters"]
CONTRACT_ZK = ROOT / "contracts/policy/src/zk.rs"
INPUTS_TEST = ["cargo", "test", "-p", "policy", "--lib", "both_sides_derive"]
AGENT = ROOT / "crates/nearx402/src/agent.rs"
AGENT_TEST = ["cargo", "test", "-p", "nearx402", "--lib", "agent"]
OFFER_TEST = ["cargo", "test", "-p", "nearx402", "--test", "offer_selection"]
TRUST_THE_OFFER = ("AcceptedTerms {\n            network: required.accepts[0].network.clone(),\n"
                   "            asset: required.accepts[0].asset.clone(),\n"
                   "            policy_code_hash: required.accepts[0].extra.policy_code_hash.clone(),\n"
                   "        }")
TIMEOUT_SECONDS = 400

# Survivors with a reason they cannot change behaviour. Challenge these.
EQUIVALENT = {
    # Groth16 in arkworks binds every public input by itself:
    # real_proof_binds_every_public_input still passes without these constraints.
    # They stay as defence in depth, for 2 constraints, so that binding does not
    # rest on a library internal.
    "context unbound": "arkworks binds public inputs without this constraint",
    "invoice unbound": "arkworks binds public inputs without this constraint",
    # The check just above forces expected_day == day, so the two values match.
    # "agent controls the day" removes both, and is killed.
    "agent's day as input": "the stale-day check makes the values equal",
}

STALE_DAY = 'require!(expected_day.0 == day, "stale proof: the day has changed");\n'
STALE_STATE = """require!(
            expected_old_state.0 == old_state,
            "stale proof: the state has changed"
        );
"""
STALE_POLICY = """require!(
            expected_policy.0 == policy,
            "stale proof: the policy has changed"
        );
"""
INPUT_ORDER = "            policy,\n            old_state,\n            new_state,\n"
SELECT_SPENT = "FpVar::conditionally_select(&same_day, &(&spent + &amount), &amount)?"
CONTEXT = "zk::context_field(env::current_account_id().as_str(), agent_id.as_str()),"


def circuit(name, *edits):
    return name, CIRCUIT, CIRCUIT_TEST, list(edits)


def contract(name, *edits):
    return name, CONTRACT, CONTRACT_TEST, list(edits)


def reconcile(name, *edits):
    return "reconcile: " + name, CONTRACT, RECONCILE_TEST, list(edits)


def store(name, *edits):
    return name, STORE, STORE_TEST, list(edits)


def deployment(name, *edits):
    return name, DEPLOYMENT, DEPLOYMENT_TEST, list(edits)


def rehearsal(name, path, *edits):
    return name, path, REHEARSAL_TEST, list(edits)


def always(check):
    """A check that can no longer fail."""
    return check, "true || " + check


MUTANTS = [
    circuit("range: amount", ("range_check(&cs, &amount, AMOUNT_BITS)?;", "")),
    circuit("range: cap", ("range_check(&cs, &cap, AMOUNT_BITS)?;", "")),
    circuit("range: per_call", ("range_check(&cs, &per_call, AMOUNT_BITS)?;", "")),
    circuit("range: spent", ("range_check(&cs, &spent, AMOUNT_BITS)?;", "")),
    circuit("range: day", ("range_check(&cs, &day, DAY_BITS)?;", "")),
    circuit("range: old_day", ("range_check(&cs, &old_day, DAY_BITS)?;", "")),
    circuit("range bits not tied to the value",
            ("Boolean::le_bits_to_fp(&vars)?.enforce_equal(x)",
             "let _ = Boolean::le_bits_to_fp(&vars)?; Ok(())")),
    circuit("policy commitment", (".enforce_equal(&policy)?;", ";")),
    circuit("old state commitment", (".enforce_equal(&old_state)?;", ";")),
    circuit("new state commitment", (".enforce_equal(&new_state)?;", ";")),
    circuit("allowlist root", ("node.enforce_equal(&root)?;", "")),
    circuit("leaf not from the public vendor",
            ("poseidon_var(&cs, DOM_LEAF, &[&vendor])?",
             'poseidon_var(&cs, DOM_LEAF, &[&FpVar::new_witness(cs.clone(), || Ok(vendor_field("data-api.near")))?])?')),
    circuit("per-call limit", ("enforce_le(&cs, &amount, &per_call, AMOUNT_BITS)?;", "")),
    circuit("day may go backward", ("enforce_le(&cs, &old_day, &day, DAY_BITS)?;", "")),
    circuit("daily cap", ("enforce_le(&cs, &new_spent, &cap, SUM_BITS)?;", "")),
    circuit("spend never accumulates", (SELECT_SPENT, "amount.clone()")),
    circuit("budget never resets", (SELECT_SPENT, "&spent + &amount")),
    circuit("new state skips the day",
            ("poseidon_var(&cs, DOM_STATE, &[&day, &new_spent, &new_salt])?",
             "poseidon_var(&cs, DOM_STATE, &[&old_day, &new_spent, &new_salt])?")),
    circuit("context unbound", ("let _ = context.square()?;", "let _ = &context;")),
    circuit("invoice unbound", ("let _ = invoice.square()?;", "let _ = &invoice;")),

    contract("proof result ignored", ("if !valid {", "if false && !valid {")),
    contract("stale-day check gone", (STALE_DAY, "")),
    contract("agent's day as input", ("zk::fr_from_u64(day),", "zk::fr_from_u64(expected_day.0),")),
    contract("agent controls the day",
             (STALE_DAY, ""), ("zk::fr_from_u64(day),", "zk::fr_from_u64(expected_day.0),")),
    contract("agent controls old state",
             (STALE_STATE, ""),
             (INPUT_ORDER, "            policy,\n            expected_old_state.0.as_slice().try_into().unwrap(),\n            new_state,\n")),
    contract("agent controls policy",
             (STALE_POLICY, ""),
             (INPUT_ORDER, "            expected_policy.0.as_slice().try_into().unwrap(),\n            old_state,\n            new_state,\n")),
    contract("vendor is not the transfer target",
             ("zk::vendor_field(vendor.as_str()),", 'zk::vendor_field("data-api.near"),')),
    contract("amount is not the transfer amount",
             ("zk::fr_from_u128(amount.0),", "zk::fr_from_u128(100_000),")),
    contract("invoice is not the memo", ("zk::invoice_field(&memo),", 'zk::invoice_field("n"),')),
    contract("context ignores the caller",
             (CONTEXT, 'zk::context_field(env::current_account_id().as_str(), "bob.near"),')),
    contract("context ignores the contract",
             (CONTEXT, 'zk::context_field("policy.near", agent_id.as_str()),')),
    contract("overlapping payments allowed",
             ('require!(a.pending.is_none(), "a payment is already in flight");\n', "")),
    contract("frozen agent can pay", ("if frozen {", "if false && frozen {")),
    contract("failed transfer keeps the new state",
             ("a.state_commitment = p.old_state;", "let _ = p.old_state;")),
    contract("failed transfer counted by breaker",
             ("if !ok {\n                        a.state_commitment = p.old_state;",
              "if !ok {\n                        a.blocked_today += 1;\n                        a.state_commitment = p.old_state;")),
    contract("late callback touches a newer payment",
             ("a.pending.as_ref().is_some_and(|p| p.log_id == id)", "a.pending.is_some()")),
    contract("rotation leaves the in-flight marker",
             ("a.max_blocked_per_day = max_blocked;\n            a.pending = None;",
              "a.max_blocked_per_day = max_blocked;")),
    contract("breaker never trips",
             ("let tripped = a.blocked_today == a.max_blocked_per_day && !a.frozen;", "let tripped = false;")),
    contract("balance not checked", ("if amount.0 > self.balance {", "if false && amount.0 > self.balance {")),
    contract("new_state not checked canonical",
             ('let new_state = zk::field_arg(&new_state.0, "new_state");',
              "let new_state: zk::Fr = new_state.0.as_slice().try_into().unwrap();")),
    contract("zero amount allowed", ('require!(amount.0 > 0, "amount must be positive");\n', "")),
    contract("memo length unchecked", ("!memo.is_empty() && memo.len() <= MAX_MEMO_LEN", "true || memo.is_empty()")),
    contract("owner check gone from set_zk_agent",
             ("max_blocked_per_day: Option<u32>,\n    ) {\n        self.assert_owner();\n        require!(\n            !self.agents.contains_key",
              "max_blocked_per_day: Option<u32>,\n    ) {\n        require!(\n            !self.agents.contains_key")),
    contract("owner check gone from set_zk_verifying_key",
             ("ic: Base64VecU8,\n    ) {\n        self.assert_owner();", "ic: Base64VecU8,\n    ) {")),
    contract("verifying key view swaps a field",
             ("neg_beta: vk.neg_beta.clone().into(),", "neg_beta: vk.neg_gamma.clone().into(),")),

    reconcile("owner check gone",
              ("tokens_moved: bool) -> PromiseOrValue<PayOutcome> {\n        self.assert_owner();\n",
               "tokens_moved: bool) -> PromiseOrValue<PayOutcome> {\n")),
    reconcile("claim skips the pending check",
              ('require!(entry.status == Status::Pending, "payment is not pending");\n'
               "        require!(\n            env::block_timestamp()",
               "require!(\n            env::block_timestamp()")),
    reconcile("no age limit", always("env::block_timestamp().saturating_sub(entry.timestamp) >= RECONCILE_AFTER,")),
    reconcile("age limit one early", (">= RECONCILE_AFTER,", "+ 1 >= RECONCILE_AFTER,")),
    reconcile("age limit one late", ("entry.timestamp) >= RECONCILE_AFTER", "entry.timestamp) > RECONCILE_AFTER")),
    reconcile("age limit shorter", ("const RECONCILE_AFTER: u64 = NANOS_PER_DAY;", "const RECONCILE_AFTER: u64 = NANOS_PER_DAY / 2;")),
    reconcile("unmoved claim skips the read",
              ("if tokens_moved {\n            return PromiseOrValue::Value", "if true {\n            return PromiseOrValue::Value")),
    reconcile("callback skips the pending check",
              ('require!(entry.status == Status::Pending, "payment is not pending");\n'
               "        require!(\n            seq == self.settle_seq,",
               "require!(\n            seq == self.settle_seq,")),
    reconcile("callback ignores a transfer that resolved", always("seq == self.settle_seq,")),
    reconcile("token balance not compared", always("held.0 >= self.balance + self.reserved,")),
    reconcile("token balance ignores what is reserved",
              ("held.0 >= self.balance + self.reserved,", "held.0 + self.reserved >= self.balance + self.reserved,")),
    reconcile("token balance must exceed what is counted",
              ("held.0 >= self.balance + self.reserved,", "held.0 > self.balance + self.reserved,")),
    reconcile("failed read accepted",
              ('.unwrap_or_else(|| env::panic_str("could not read token balance"));', ".unwrap_or(U128(u128::MAX));")),
    reconcile("statuses swapped",
              ("entry.status = if tokens_moved {\n            Status::Paid\n        } else {\n            Status::Failed\n",
               "entry.status = if tokens_moved {\n            Status::Failed\n        } else {\n            Status::Paid\n")),
    reconcile("moved claim not labelled", ('"reconciled by the owner: tokens moved"', '"tokens moved"')),
    reconcile("unmoved claim not labelled", ('"reconciled by the owner: tokens did not move"', '"tokens did not move"')),
    reconcile("reservation not dropped",
              ("self.reserved -= entry.amount.0;\n        self.settle_seq += 1;\n        if !tokens_moved {",
               "self.settle_seq += 1;\n        if !tokens_moved {")),
    reconcile("settle_seq not bumped",
              ("self.reserved -= entry.amount.0;\n        self.settle_seq += 1;\n        if !tokens_moved {",
               "self.reserved -= entry.amount.0;\n        if !tokens_moved {")),
    reconcile("funds not returned",
              ("if !tokens_moved {\n            self.balance += entry.amount.0;\n        }\n", "")),
    reconcile("funds returned on a moved claim",
              ("if !tokens_moved {\n            self.balance += entry.amount.0;", "if true {\n            self.balance += entry.amount.0;")),
    reconcile("budget refunded on an unmoved claim",
              ("if !tokens_moved {\n            self.balance += entry.amount.0;\n",
               "if !tokens_moved {\n            self.balance += entry.amount.0;\n"
               "            if let Some(state) = self.agents.get_mut(&entry.agent_id) {\n"
               "                state.spent_today = 0;\n            }\n")),
    reconcile("budget refunded on a moved claim",
              ("if !tokens_moved {\n            self.balance += entry.amount.0;\n",
               "if let Some(state) = self.agents.get_mut(&entry.agent_id).filter(|_| tokens_moved) {\n"
               "            state.spent_today = 0;\n        }\n"
               "        if !tokens_moved {\n            self.balance += entry.amount.0;\n")),
    reconcile("another payment's marker cleared",
              ("if a.pending.as_ref().is_some_and(|p| p.log_id == id) {\n                a.pending = None;",
               "if true {\n                a.pending = None;")),
    reconcile("marker never cleared",
              ('a.pending = None;\n            }\n        }\n        emit_entry("payment_reconciled"',
               'let _ = &a;\n            }\n        }\n        emit_entry("payment_reconciled"')),
    reconcile("state rolled back on a claim",
              ('a.pending = None;\n            }\n        }\n        emit_entry("payment_reconciled"',
               "if let Some(p) = a.pending.take() {\n                    a.state_commitment = p.old_state;\n                }\n"
               '            }\n        }\n        emit_entry("payment_reconciled"')),

    store("intent not saved",
          ("        self.save()\n    }\n\n    /// Makes the file agree",
           "        Ok(())\n    }\n\n    /// Makes the file agree")),
    store("in-flight payment ignored", always("!chain.in_flight,")),
    store("another policy accepted", always("self.wallet.opening.commitment()? == chain.policy_commitment,")),
    store("adopted state not saved", ("self.intents.clear();\n        self.save()?;", "self.intents.clear();")),
    store("a lost-looking send is forgotten",
          ("return Ok(Reconciled::Current);", "self.intents.clear();\n            return Ok(Reconciled::Current);")),
    store("create replaces a wallet", always("!path.exists(),")),
    store("no lock on the wallet", ("match lock.try_lock() {", "match Ok::<(), TryLockError>(()) {")),
    store("non-canonical field accepted", always("fr_le(&fr).as_slice() == bytes,")),
    store("wallet readable by others", ("options.mode(0o600);", "options.mode(0o644);")),
    store("intents unbounded", ("if self.intents.len() > MAX_INTENTS {", "if false && self.intents.len() > MAX_INTENTS {")),
    store("file version ignored", always("j.version == VERSION,")),
    store("wallet temp: reuses a leftover",
          ("options.create_new(true).write(true);", "options.create(true).truncate(true).write(true);")),
    store("wallet temp: gives up on a leftover",
          ("Err(e) if e.kind() == ErrorKind::AlreadyExists => {}\n", "")),
    store("wallet temp: left behind on failure", ("let _ = fs::remove_file(&tmp);", "")),
    store("wallet symlink: accepted", always("!meta.file_type().is_symlink(),")),
    store("wallet symlink: open does not check",
          ("ensure_not_a_symlink(path)?;\n        let text", "let text")),
    store("wallet symlink: create does not check",
          ("ensure_not_a_symlink(path)?;\n        ensure!(", "ensure!(")),

    deployment("full-access key passes",
               ('let Some(call) = permission.get("FunctionCall") else {',
                'let Some(call) = permission.get("FunctionCall") else {\n            continue;')),
    deployment("key may call another contract", always('call["receiver_id"].as_str() == Some(contract.as_str()),')),
    deployment("key may call other methods", always("methods.len() == 1 && methods[0].as_str() == Some(method),")),
    deployment("mainnet config accepted", always('!cfg.network_name.contains("mainnet"),')),
    deployment("wallet temp: arguments follow a leftover",
               ("write_private(path, &serde_json::to_vec_pretty(args)?)",
                'let tmp = path.with_extension("json.tmp");\n'
                "    fs::write(&tmp, serde_json::to_vec_pretty(args)?)?;\n"
                "    write_private(path, &[])?;\n"
                "    Ok(fs::rename(&tmp, path)?)")),

    rehearsal("recovery overwrites the current state", DEPLOYMENT,
              ("file.begin(fresh)?;", "file.wallet.state = fresh;\n    file.begin(fresh)?;")),
    rehearsal("intent not recorded before sending", DEMO, ("wallet.begin(payment.next)?;", "")),
    rehearsal("deployed code not checked", DEMO, always("deployed == trusted,")),
    rehearsal("token not checked", DEMO, always("token == cfg.token,")),
    rehearsal("agent's keys not checked", DEMO,
              ('let count = check_limited_keys(&keys, &cfg.policy, "pay_zk")?;',
               "let count = {\n        let _ = &keys;\n        1\n    };")),
    rehearsal("proving key not compared", DEMO, always("on_chain == verifying_key_args(&pk.vk),")),
    rehearsal("breaker headroom not checked", DEMO, always("view.blocked_today + REJECTED_ATTEMPTS < view.max_blocked_per_day,")),
    rehearsal("outside recipient not checked", DEMO,
              (".any(|v| v == cfg.outside_recipient.as_str()),", ".any(|v| false && v == cfg.outside_recipient.as_str()),")),
    rehearsal("a recorded run reveals the policy", DEMO, ("reveal: false,", "reveal: true,")),

    ("receipt secret not checked", FACILITATOR, THEFT_TEST,
     [("if entry.memo.as_deref() != Some(memo_expected.as_str()) {",
       "if false && entry.memo.as_deref() != Some(memo_expected.as_str()) {")]),
    ("receipt check accepts the nonce alone", FACILITATOR, THEFT_TEST,
     [("if entry.memo.as_deref() != Some(memo_expected.as_str()) {",
       "if !entry.memo.as_deref().is_some_and(|m| m.starts_with(proof.nonce.as_str())) {")]),
    ("receipt memo ignores the secret", X402, X402_TEST,
     [(".chain_update(receipt_secret.as_bytes())", ".chain_update(b\"fixed\")")]),
    ("receipt secret format not checked", X402, X402_TEST,
     [("receipt_secret.len() == 64\n            && receipt_secret",
       "true || receipt_secret.len() == 64\n            && receipt_secret")]),
    ("receipt secret reused between payments", X402, X402_TEST,
     [("rand::random::<[u8; 32]>()", "[7u8; 32]")]),

    ("offer scheme not checked", AGENT, AGENT_TEST, [("if o.scheme != x402::SCHEME {", "if false && o.scheme != x402::SCHEME {")]),
    ("offer network not checked", AGENT, AGENT_TEST, [("if o.network != self.network {", "if false && o.network != self.network {")]),
    ("offer asset not checked", AGENT, AGENT_TEST, [("if o.asset != self.asset {", "if false && o.asset != self.asset {")]),
    ("offer hash not checked", AGENT, AGENT_TEST,
     [("if o.extra.policy_code_hash != self.policy_code_hash {", "if false && o.extra.policy_code_hash != self.policy_code_hash {")]),
    ("offer resource not checked", AGENT, AGENT_TEST, [("if o.resource != path && o.resource != url {", "if false && o.resource != path {")]),
    ("offer of zero price accepted", AGENT, AGENT_TEST, [("if price == 0 {", "if false && price == 0 {")]),
    ("offer nonce too long for the memo", AGENT, AGENT_TEST,
     [("o.extra.nonce.is_empty() || o.extra.nonce.len() > x402::MAX_NONCE_LEN", "o.extra.nonce.is_empty()")]),
    ("offer list stops at the first", AGENT, AGENT_TEST,
     [("Err(reason) => reasons.push(reason),", "Err(reason) => {\n                    reasons.push(reason);\n                    break;\n                }")]),

    ("poseidon: too few partial rounds", ROOT / "crates/zx402-zk/src/lib.rs", POSEIDON_TEST,
     [("(2usize, 8usize, 57usize, 5u64)", "(2usize, 8usize, 56usize, 5u64)")]),
    ("inputs: contract drops the length prefix", CONTRACT_ZK, INPUTS_TEST,
     [("data.extend_from_slice(&(part.len() as u32).to_le_bytes());\n", "")]),
    ("inputs: contract skips the truncation", CONTRACT_ZK, INPUTS_TEST,
     [("digest[31] = 0;\n", "")]),
    ("inputs: contract tags vendors as invoices", CONTRACT_ZK, INPUTS_TEST,
     [('const VENDOR_DOMAIN: &[u8] = b"zx402:vendor:v1";', 'const VENDOR_DOMAIN: &[u8] = b"zx402:invoice:v1";')]),
    ("offer: agent trusts the vendor's terms", AGENT, OFFER_TEST,
     [("terms.choose(&required.accepts, url)?", TRUST_THE_OFFER + ".choose(&required.accepts, url)?"),
      ("let terms = self.policy.accepted_terms(&self.network).await?;\n", "")]),
    ("offer: zk quote trusts the vendor's terms", AGENT, OFFER_TEST,
     [("terms.choose(&body.accepts, url)",
       "{\n        let _ = terms;\n        AcceptedTerms {\n            network: body.accepts[0].network.clone(),\n"
       "            asset: body.accepts[0].asset.clone(),\n"
       "            policy_code_hash: body.accepts[0].extra.policy_code_hash.clone(),\n"
       "        }\n        .choose(&body.accepts, url)\n    }")]),
    ("offer: client follows redirects", AGENT, OFFER_TEST,
     [(".redirect(reqwest::redirect::Policy::none())", "")]),
    ("offer: lost response not reported as paid", AGENT, OFFER_TEST,
     [('.with_context(|| format!("the response to a paid request was lost; {moved}"))?',
       '.context("GET resource with payment")?')]),
]


def main():
    words = sys.argv[1:]
    chosen = [m for m in MUTANTS if not words or any(w in m[0] for w in words)]
    killed, survived, equivalent, timed_out, not_run = [], [], [], [], []
    for name, path, test, edits in chosen:
        original = path.read_text()
        mutated = original
        missing = [old for old, _ in edits if old not in mutated]
        if missing:
            not_run.append(name)
            print(f"NOMATCH    {name}: {missing[0][:60]!r}", flush=True)
            continue
        for old, new in edits:
            mutated = mutated.replace(old, new, 1)
        path.write_text(mutated)
        try:
            run = subprocess.run(test, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                 text=True, timeout=TIMEOUT_SECONDS)
        except subprocess.TimeoutExpired:
            timed_out.append(name)
            print(f"TIMEOUT    {name}", flush=True)
            continue
        finally:
            path.write_text(original)
        if "could not compile" in run.stdout:
            not_run.append(name)
            print(f"NOBUILD    {name}\n{run.stdout[-700:]}", flush=True)
        elif run.returncode != 0:
            killed.append(name)
            failed = re.findall(r"^test (\S+)(?: - should panic)? \.\.\. FAILED", run.stdout, re.M)
            short = sorted({f.split("::")[-1] for f in failed})
            print(f"killed     {name}  <- {len(short)}: {', '.join(short[:3])}", flush=True)
        elif name in EQUIVALENT:
            equivalent.append(name)
            print(f"EQUIVALENT {name}: {EQUIVALENT[name]}", flush=True)
        else:
            survived.append(name)
            print(f"SURVIVED   {name}", flush=True)
    print(f"\ntotal {len(chosen)}: killed {len(killed)}, equivalent {len(equivalent)}, "
          f"survived {len(survived)}, timed out {len(timed_out)}, not run {len(not_run)}")
    sys.exit(1 if survived or timed_out or not_run else 0)


main()

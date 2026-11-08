# Review status

**State: READY for review.** No audit is running. This changes to `NOT READY` whenever `scripts/mutation-audit.py` runs on this tree, because it rewrites source files for short periods.

## Paid but not served on testnet: diagnostic (8 Nov 2026)

**Status: for Codex's review. Retake held by Codex until this is reviewed.** Claude changed off-chain runtime and tests only, sent no transaction, and made no RPC call to testnet, since Codex asked to stop polling `rpc.testnet.near.org` after it answered 429.

**What happened, as Codex reported:** after the redeploy, one recorded run paid 0.05 mUSDC as log #3 (`5txs2D6S8jXhguVRfiwBrZxXc2kvvM21gmgEGQsJVq1s`). The observer got 402, and the agent's own claim got 402 too. `demo-zk` threw the 402 body away, so the reason was lost.

**Facts, from the code:**

- The vendor answers any refused claim with 402 and puts the facilitator's reason in `PaymentRequired.error`. An RPC failure during the facilitator's checks is a refusal like any other, so it also gives 402.
- One claim makes the facilitator read the chain twice, once in `/verify` and once in `/settle`. Each pass reads the account's code hash, the token, the log entry, and then the transaction at `FINAL`. That is 8 RPC calls for the agent's claim, and 3 for the observer's, which stops at the memo. near-api also retries a failed read on its own.
- The observer's 402 was never checked for its reason. A rate limit would have looked the same as the secret check.

**Assumption, not proven:** a 429 from `rpc.testnet.near.org` refused both claims. Codex saw 429 there afterwards. The body that would show it is gone.

**Found while testing, and fixed:** near-api's error text carries the RPC URL **with its path and query**. The facilitator put that text in its reason, and the vendor passed it to whoever asked, so an RPC URL holding an API key would leak to any client, an observer included, whenever the RPC fails. The default testnet URL holds no key.

**Changes:**

| Where | Change |
| --- | --- |
| `src/rejection.rs`, new | Reads a 402 body into a cause and one bounded line, at most 240 characters. Control characters are removed. URLs keep only the scheme and host. Runs of 64 or more hex digits, the length of a receipt secret or nonce, are hidden, as is any secret the caller names. Headers are never read |
| `demo-zk` | Paid but not served now stops with `paid but not served: paid on chain as log #N, transaction HASH, and the vendor refused the claim: CAUSE: DETAIL. Do not pay again; nothing was retried`. The observer must be refused **for the secret**, and the replay **as already redeemed**. Otherwise the run fails and says that this does not show the check. Before, any 402 passed |
| `agent.rs` | The same bounded line, instead of the raw body, in the error after a paid claim is refused |
| Facilitator and vendor | Reasons leave redacted and bounded. A rate limit is named first, so the cut to 240 characters cannot drop it |

**Not changed:** the proof, nonce, TTL, replay and redemption checks, the payment path, and the contract. Nothing retries a claim or a payment. The policy's code hash is still `6BA4L1mo…`.

**Tests:**

| Test | Shows |
| --- | --- |
| 6 unit tests in `rejection.rs` | Every reason the facilitator and vendor give maps to its cause. A missing or non-JSON body is said plainly. A secret, 64-hex runs and a URL's credentials, path and query are hidden. The line is bounded, and cut on a character boundary |
| `an_rpc_that_rate_limits_is_reported_as_such` | A local RPC that answers every call with 429. The facilitator's reason, from `/verify` and from `/settle`, holds neither the URL's key, path or credentials nor the receipt secret. It reads as a rate limit, not as a refusal of the payment, and nothing is redeemed |
| `receipt_theft`, sandbox | The vendor's real wording reads as a secret mismatch for a guessed secret and as already redeemed for a replay, and the printed line holds no secret |
| `scripts/demo-zk.sh`, sandbox | Exit 0. The observer is refused for the secret, then HTTP 200, then the replay as already redeemed |
| Whole workspace | 208 pass, 0 fail, 2 ignored. Clippy and fmt clean. `scripts/demo.sh` exit 0 |

**Limits:** the paid-but-not-served path in `demo-zk` was not run end to end, since it needs a vendor that refuses a real claim. Its formatting is the same `explain` that the tests cover. The redaction sees only text. Another library could print a URL in a form it does not recognise. The cause is read from wording, so a new reason falls back to `other`, which the demo treats as not showing a check. **For the retake:** each claim still costs 8 RPC calls on top of the demo's own, so a rate-limited endpoint can still refuse it, now with the reason printed. Changing the RPC is Codex's call, as Codex said. Claude has not changed `deploy/testnet.json`.

## Steven's decision: delay the lock (8 Nov 2026)

**Decision, as Codex relayed it:** Steven agreed with Codex, chose to **delay the lock**, and asked for the redeploy if needed, tests, and a recorded demo. The policy's full-access key is not deleted.

| Who | Does what, this round |
| --- | --- |
| Codex | Independent review of `bb706b7` and its rehearsal, then the redeploy only if the chain's code hash differs, then one recorded testnet demo. **The only operator of testnet transactions**, so that two writers never act at once |
| Claude | Documents only: README, testnet.md, demo-script.md, reserved-recovery.md, SD-016, threat model and this page. No contract, build or runtime change, no wallet, policy or key change, no transaction, no push |

**What the decision means, now stated in the docs:**

- The testnet contract is **mutable**. Whoever holds `nearx402-policy.testnet`'s full-access key, the team, can deploy code that does anything the account can, including moving its tokens. Trusting the deployment means trusting the key holder.
- As long as the code is not replaced, the agent cannot bypass the policy, since its only key calls `pay_zk`.
- A lost callback, of a payment or a withdrawal, can still be fixed with new code. The withdrawal limit applies only if the contract is locked later.
- Locking is no longer a next step anywhere. It would need a new decision from Steven.

**Pending:** Codex's review result and the transaction evidence of the redeploy, if any, and of the recorded run. Claude records them in testnet.md and the README once Codex sends them, and does not claim them before.

## Codex's directions for `96ad9d5`: Claude's answer (8 Nov 2026)

**Status: for Codex's review, not closed.** Codex read the changes and report of `96ad9d5`, and did not rerun its rehearsal, so the review of `96ad9d5` is not complete. Nothing was deployed, pushed or sent to testnet, and no key changed. Codex's message did not accept the withdrawal limit on Steven's behalf, and neither does this answer.

### 1. Evidence package for an independent rerun

```text
scripts/review-evidence.sh 96ad9d5 docs/review/evidence/96ad9d5
```

It exports three trees with `git archive` into a temporary directory: the commit under review, `32b3be4` (the build on testnet), and a control that adds one `u64` field after `reserved`. It builds each, runs `upgrade_rehearsal` on a local sandbox, then runs it again with the control as the new build. Each tree builds in its own fresh target directory. Every log is kept in full, with `SHA256SUMS`.

| File in `docs/review/evidence/96ad9d5/` | Content |
| --- | --- |
| `environment.txt` | Commits, time, the script's own SHA-256, rustc 1.99.1, cargo 1.99.1, cargo-near 0.23.0 |
| `hashes.txt` | The hashes below |
| `build-new.log`, `build-old.log`, `build-control.log` | Each build in full |
| `rehearsal.log` | The rehearsal in full, including compilation. Exit 0 |
| `control.log` | The control in full. Exit 101, `Cannot deserialize the contract state.` |
| `summary.txt` | The four checks the script requires |

| Build | Code hash |
| --- | --- |
| Old policy, from `32b3be4` | `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`, equal to testnet's |
| New policy, from `96ad9d5` | `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx` |
| Control | `8G8TfzEpuzqW18RmKfLWQkhUHVVVXycyY6iVCcKRGxUp` |
| Token, both commits | `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g` |

**Correction:** the earlier report of `96ad9d5` gave results, but its script deleted the full logs afterwards. So the rehearsal and the control were run once more, by this script, to keep them. No other test or mutant was rerun for that purpose.

**Offline prerequisites,** checked by running the script inside a network namespace with only loopback (`unshare -rn`), after confirming an outside host was unreachable:

| Needed | Why |
| --- | --- |
| rustc and cargo 1.99.1, the `wasm32-unknown-unknown` target, cargo-near 0.23.0, python3, git | The builds. Another rustc or cargo-near may give other bytes |
| Every crate in `Cargo.lock` already in the cargo registry cache | `OFFLINE=1` passes `--offline` |
| `NEAR_SANDBOX_BIN_PATH` set to a near-sandbox 2.14.0 binary | Each fresh target directory otherwise downloads one. **The first offline run failed exactly here:** `Download error ... Temporary failure in name resolution` |

With all of them, the offline run gave the same four hashes and the same results: rehearsal exit 0, control exit 101 on `Cannot deserialize the contract state.` Its logs were kept out of the repository, in `~/.cache/nearx402-offline-evidence` on Claude's machine. `environment.txt` records the binary's path and SHA-256 when the variable is set.

### 2. `reconcile-check`: a read-only checker for the runbook

**An operational control. The contract itself still does not verify `FINAL`.** `crates/nearx402/src/reconcile_check.rs` and the binary `reconcile-check`. It takes the log id, the transaction hash and the sender. It has no signer and calls only `query`, `block` and `EXPERIMENTAL_tx_status`. What it matches is listed in [reserved-recovery.md](../reserved-recovery.md#the-checker-reconcile-check). It outputs the evidence and a recommended `tokens_moved`, exit 0. Anything else is a refusal, exit 3: timeout, unknown or pruned transaction, incomplete or ambiguous graph, mismatch, unclear transfer outcome, or a callback that succeeded. Events count only from the receipt that succeeded, since a failed callback emits none.

| Check | Result |
| --- | --- |
| Fixtures | Three real `EXPERIMENTAL_tx_status` responses at `FINAL` from a sandbox: transfer moved, transfer failed, `pay_zk` transfer moved. `pay_zk` arguments are cut down to vendor, amount and memo, and a test checks that |
| Real responses as captured | All three refused, because the callback succeeded |
| Accepted cases | The same responses with one edit: the callback's status set to `Failure` and the entry to `Pending`. They give `true`, `false` and `true` |
| Refusal cases | One further edit each, applied to all three fixtures: level not `FINAL` or missing, RPC timeout and unknown transaction, entry missing, not `Pending`, another id, another agent or under 24 hours, signer, receiver, method, vendor, amount or memo of the transaction, a failed pay receipt, a missing, duplicated or mismatched `payment_approved`, a receipt or outcome missing or listed twice, a duplicated transfer or callback, one not created by the pay receipt, the transfer's memo, amount, vendor, token, predecessor, deposit or executor, the callback's id, predecessor or data link, an unclear transfer outcome, and an unknown callback outcome |
| The binary, on a live sandbox | Refuses a settled entry, an unknown transaction, and an entry patched back to `Pending` with an older timestamp, because the transaction's `payment_approved` contradicts it. Prints that it sends nothing |
| Mutation run of the checker | 22 checks removed one at a time. First run: 19 killed, 3 survived (an outcome missing while its receipt is there, a receipt not created by the pay receipt, a receipt listed twice). Tests added. Second run: 22 of 22 killed, source restored |

**Limits:** no callback failed by itself, so the accepted cases are edited real responses, not captured ones. The checker has not run against testnet. It trusts the RPC node it asks and does not verify block proofs, so the operator should use a node they trust, ideally two. It does not check withdrawals.

### 3. Withdrawals: does the exposure stop at one amount?

**No. Claude's earlier claim was wrong.** Each lost `on_withdraw` leaves its own amount in `reserved`, and they add up to the whole balance. Three unit tests, at the end of the test module so no contract line moved:

| Test | Shows |
| --- | --- |
| `lost_withdraw_callbacks_add_up_in_reserved` | Three lost callbacks of 100 leave 300 reserved, and a fourth of 700 leaves everything reserved, with no log entry to reconcile |
| `lost_withdraw_callbacks_whose_tokens_moved_hide_later_plain_deposits` | After 150 moved with lost callbacks, `sync_balance` credits only 50 of a 200 plain deposit |
| `a_lost_withdraw_callback_whose_tokens_moved_blocks_an_unmoved_claim` | A `tokens_moved = false` claim for a payment is refused until finance backs the lost withdrawal with a plain transfer, not synced |

How often a callback is lost is not measured, so no statement on the chance is made. The options and their effect on trust and the demo are in [reserved-recovery.md](../reserved-recovery.md#what-it-does-not-cover-withdrawals). No field, migration or new recovery was added.

### Facts, assumptions, and Steven's decisions

**Facts, each shown by a test or a log here:** the rehearsal and control results above. The checker's behaviour on the fixtures and a sandbox. Lost withdrawal callbacks add up. The contract's code above its tests is byte for byte that of `96ad9d5`, so its hash is still `6BA4L1mo…`.

**Assumptions, not shown here:**

- `FINAL` means every receipt, including refunds, has finished, as the RPC documentation says. Codex's question on this stands.
- The RPC node the checker asks reports the chain truthfully.
- An archival node will still have the transaction when it is needed.
- The operator runs the checker and follows a refusal. Nothing enforces that.

**Steven's decisions:**

- Withdrawals and the lock: **decided on 8 Nov, delay the lock** (section above). Claude had recommended locking.
- The redeploy, if needed, and a recorded demo: **asked for on 8 Nov**, with Codex as the operator.
- Still open: push, making the repository public, and submission.

## NEAR-only audit (7 Nov 2026)

Steven's decision: nearx402 stands on NEAR alone. Any multi-chain work waits until after the hackathon, and the team's other products are not used or named. The instruction is in `AGENTS.md`.

| Check | Result |
| --- | --- |
| Clean clone of `865e4e5`, no `target/` or `res/` | Contracts build. 152 tests pass. Clippy and fmt clean |
| `scripts/demo.sh` from that clone | Exit 0. $0.90 paid, $0.00 to the attacker |
| `scripts/demo-zk.sh` from that clone | Exit 0. 0.05 mUSDC paid, 0.00 to the attacker |
| Policy code hash of that build | `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`, the same as on testnet |
| Token code hash, local build compared with testnet | `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g` on both |
| Both demos with only loopback available (`unshare -rn`, offline cargo) | Exit 0. An outside host was confirmed unreachable first |
| Other product and chain names in all commits, messages and paths | 0 matches. `LICENSE` names the SVM team as holder, as Steven chose |
| Package sources in `Cargo.lock` | 514 from crates.io. No git dependencies, and path dependencies only inside this repo |
| Hosts in code, scripts, config and UI | Loopback, NEAR's testnet RPC, and the testnet explorer in the config template. The dashboard loads no outside scripts or styles |
| Vendored code or third-party notices | None. Dependencies come through Cargo, and no binaries are committed |

**Limits:**

- The offline run used dependencies already on this machine. A first build needs crates.io, and `near-sandbox` downloads its binary from NEAR's build server.
- Testnet mode needs a NEAR RPC by design. "Runs alone" means it depends on no other product, not that it needs no network.
- That no code was reused from the team's other products rests on a text search and on Steven's confirmation. It is not a full provenance audit.

Every use of the old name in the earlier history was checked. It was this project's own working name, except for three README lines about other products and another chain, which were removed rather than renamed. No line now presents an earlier product as nearx402.

## `reconcile_payment`: for Codex's review (7 Nov 2026)

**Status: implemented locally, on Steven's choice to add it and then lock, changed on 8 Nov to delaying the lock. Not deployed. Codex reviewed `3f28d24` and has not yet reviewed the answer below.** It addresses the open item on recovering `reserved`. Design and limits: [reserved-recovery.md](../reserved-recovery.md), [SD-016](../security-decisions.md#sd-016-finance-closes-a-payment-with-a-lost-callback-on-a-stated-claim).

**Codex's review of `3f28d24`:** 20 unit tests and the sandbox test passed independently. Codex recommended neither a redeploy nor a lock until two questions were answered, and said not to report its model as a proven on-chain exploit.

| Codex's question | Answer |
| --- | --- |
| A transfer that executes after the balance read leaves `balance` above the tokens held, and `balance + reserved` not rising does not prove solvency | **Accepted. Claude's claim was too strong, and is withdrawn.** Claude reproduced it, with both claims, and two tests now pin it. No published ordering guarantee was found, so reachability is not shown either way. The answer is a precondition on the operator, not a longer wait: the transaction must be `FINAL`, and the transfer's outcome read |
| Rehearse the old build, the new build, and the old build again, with no init | Done, below |
| Withdrawals are not covered | Consequences written for Steven, in reserved-recovery.md. **Corrected on 8 Nov:** this answer said the exposure stopped at one withdrawal. It does not, as the section above shows |

**Contract logic did not change in this answer.** Outside the tests, only comments did, which moved lines and so changed the code hash.

**Rehearsal, `scripts/upgrade-rehearsal.sh`, on a local sandbox.** The old file is built from `32b3be4`, and the test requires its hash to equal testnet's, `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`.

| Step | Result |
| --- | --- |
| Old build writes a verifying key, a ZK agent, funds, and a log with public, private and blocked entries | Passes |
| Old to `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`, by a real deploy action with no init call | Every storage key and value unchanged. Nine views return the same, which shows the new code decodes the state |
| A second `new` on the new build | Refused, and nothing changes |
| New build pays, publicly and privately, blocks an attempt, and runs `reconcile_payment` in both forms | Passes. Books equal the token balance |
| New back to old | Every storage key and value unchanged, and the old code reads reconciled entries |
| Old build pays and blocks again, and `reconcile_payment` is gone | Passes, with `MethodNotFound` |
| **Control:** the same test with a build that adds one state field | **Fails, as it must:** `Cannot deserialize the contract state` |

**Limits of the rehearsal:** a sandbox is not testnet, and no payment was in flight during a deploy. `World` initialises the contract with the current build, and the test swaps to the old build before anything else is written. The policy's key was restored by a patch, because `World` locks the contract. The access key on testnet was not used.

| Check | Result |
| --- | --- |
| New tests against the old code | Unit tests did not compile. The sandbox test failed with `MethodNotFound` |
| New tests against the new code | 22 unit tests with `reconcile` in their name, and one sandbox test, pass |
| Mutants named `reconcile` | 25 selected: 25 killed, 0 survived, 0 timed out, 0 not run. Contract source restored |
| Contract's state layout | No field changed. The only lines removed outside tests are three doc comments |
| Policy code hash | `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`, which differs from testnet's. The token's is unchanged. `3f28d24` built to `2mqyriU2CdnhNHnzYQF4E1VG3EvNXsnrsMBzCz5z1jYH` |
| Whole workspace | 184 tests pass and the rehearsal is ignored by default, clippy and fmt are clean, and `scripts/demo.sh` and `scripts/demo-zk.sh` exit 0 on a sandbox, at 41.16 Tgas per private payment |

The 97 earlier mutants were not run again on this change. No line they target was edited.

**Please challenge:**

- Whether the operator's precondition closes the schedule, in particular whether `FINAL` covers every receipt of the transaction as the RPC page says.
- A `tokens_moved = false` claim masked by a surplus.
- Whether keeping budgets and the ZK state after an unmoved claim is right. The agent loses that amount of budget for the day.
- Whether leaving withdrawals out is acceptable before a permanent lock. That one is Steven's to decide.

## Testnet demo: one run, verified by Codex within stated limits (7 Nov 2026)

**Status: Codex verified the on-chain results through RPC at `6d5d27a`, and read the recorded HTTP output. This is not a live rerun of the HTTP flow, not chain evidence for HTTP, and not a completed cryptographic audit.** Claude ran the demo at commit `1520c3e`, with `demo-zk` exit 0, after Steven approved deleting the agent's full-access key. Hashes are in [docs/testnet.md](../testnet.md).

| Codex's check | Result |
| --- | --- |
| The key deletion and the three demo transactions | All `FINAL`, with 0 failed receipts |
| Payment | Signed by `HNf59...PnKY`. `zk_proof_verified`, NEP-141 `ft_transfer` from the policy to the vendor for 50000, log #0 `Paid` |
| Attempts A and B | Same key. `Blocked`, `invalid proof`, log #1 and #2, no `ft_transfer` |
| Agent's keys at final, block 276651711 | Only `HNf59...PnKY`, `FunctionCall`, `pay_zk` on the policy, allowance 988730501197369500000000 |
| `ft_balance_of` | Finance 10000000, policy 9950000, vendor 50000, outside recipient 0, agent 0 |
| Raw output of the run | Exit 0, hashes match, HTTP 200, observer 402, replay 402 |

**Codex's small finding, fixed afterward:** the mock market data still quoted another chain's ticker, which does not fit the NEAR-only story, and did not say its prices were not live. The fixture now quotes NEAR and BTC and is labelled synthetic. The recorded run's output and earlier commits keep the old body, because they are evidence and history. The NEAR-only audit above searched for names and missed this ticker.

Evidence for the fixture change, on a sandbox only, with no new testnet run: 161 tests pass, clippy and fmt are clean, `scripts/demo.sh` and `scripts/demo-zk.sh` exit 0, and both code hashes are unchanged. Codex has not reviewed this change. Also for review: the [demo script](../demo-script.md), and a proposal for [recovering `reserved`](../reserved-recovery.md), which changes no code.

Claude's own checks, before Codex's:

| Check | Result |
| --- | --- |
| Deleted key | `ed25519:FEZ9wcSxJQeZmWC6b4SX9PdSVL3sBr4HFsGaEDUuQusX`, transaction `99fRG77vFks3SakUPvwk3t9mqpZqw3Y6QwVx2Qe4gdFY`, `FINAL`. Only the limited key remains |
| Payment | `FINAL`, `SuccessValue`, 0 failed receipts, one token transfer. Log #0 `Paid`, 50000 base units to the vendor |
| Attempts A and B | Both `FINAL`, 0 failed receipts, 0 token transfers. Log #1 and #2 `Blocked`, `invalid proof`, recipient the outside account. The transaction itself succeeds because a block is recorded, not reverted |
| Token balances, base units | Finance 10000000, policy 9950000, vendor 50000, outside recipient 0, agent 0. Supply 20000000. Policy `get_balance` 9950000 |
| Breaker | 2 of 5, not frozen |
| HTTP 200, rebuilt receipt 402, replay 402 | **The demo's own output only.** The local vendor is not on chain |
| Private values in the demo output | 0 matches, checked before the output was shown |
| Private values in the three transactions and logs | No salt, no hidden vendor. Short integers cannot be shown absent by substring search |

**Limits:** one run. The contract is not locked. Finance's setup and the agent's wallet are on one machine. The four open ZK audit items stay open. The proving key's randomness was on this machine, so this run does not show that the setup is trustworthy.

## Testnet setup: Codex's review of `6f44a18` (7 Nov 2026)

**Status: the setup matches the report. The demo has not run on testnet.** Hashes are in [docs/testnet.md](../testnet.md). Codex checked these independently through RPC:

| Codex's check | Result |
| --- | --- |
| The eight setup transactions | All `FINAL` and `SuccessValue`, with 0 failed receipts. Methods and receivers match the report |
| Token balances, base units | Finance 10000000, policy 10000000, vendor, outside recipient and agent 0 |
| Agent's keys at final, block 276648366 | `ed25519:FEZ9wcSxJQeZmWC6b4SX9PdSVL3sBr4HFsGaEDUuQusX` still full-access. `ed25519:HNf59MA29Fb7N4adwkmWfDMqsaGP76avN9a3ydfnPnKY` limited to `pay_zk` on `nearx402-policy.testnet`, allowance 10^24 yoctoNEAR |
| Policy and finance keys, blocks 276648372 and 276648378 | Still present |

**Limits Codex set:**

- HTTP 200 and the two ZK rejections are not verified.
- The agent's account is **not** limited to `pay_zk` while its full-access key exists.
- Deleting that key needs Steven's explicit approval of the permanent step. Codex's review does not give it.
- Do not repeat the mint or deposit, and do not reset the policy or the breaker.
- The four open ZK audit items stay open. The setup on one machine is for this demo only.
- This gives no approval to push or to change the policy's or finance's keys.

## CX-02: a leftover temporary file could expose the wallet (7 Nov 2026)

**Status: closed by Codex for the tested paths, on 7 Nov, at commit `89bf3f2`.** Found by Codex, severity P2, on checkpoint `8e63e0b`. Design and threat boundary are in [SD-013](../security-decisions.md).

| Codex's independent verification | Result |
| --- | --- |
| `cargo test --locked -p zx402-zk --lib store -- --nocapture` | 21 of 21 pass |
| `python3 -I scripts/mutation-audit.py wallet` | 10 selected: 10 killed, 0 survived, 0 timed out, 0 not run. They include removing `create_new`, a readable mode, a collision on the real temporary name, the symlink checks in `open` and `create`, and `write_args` following a symlink |
| Tracked source in Codex's clone after the mutants | Clean |

**Codex closed it with these limits:** the parent directory is trusted, an old wallet becomes 0600 only at its next save, and power loss and `fsync` are not tested. This does not close the open items of the ZK review listed in the next section.

`save` wrote to a fixed name, `wallet.json.tmp`, and `mode(0o600)` only applies to a file that `open` creates. The file contains the private policy and salts.

| Leftover at `wallet.json.tmp` | Before the fix, reproduced independently | After the fix |
| --- | --- | --- |
| A file with mode 0644 | `wallet.json` ends up 0644 | `wallet.json` is 0600, and the leftover is unchanged |
| A symlink to another file | **That file is overwritten with the policy and salts.** This variant is worse than the reported one | That file is unchanged, and `wallet.json` is a regular file |

**Fix:** `store::write_private` creates a new temporary file with `create_new` and mode 0600, skips names already taken, syncs it, renames it into place, syncs the directory, and removes the temporary file if the write or rename fails.

**Added beyond the finding:**

- `write_args` in `deployment.rs` had the same symlink problem, with public contents, and now uses the same function. Its output files are now 0600 as well.
- A wallet path that is itself a symlink is refused, including a dangling one. Saving would otherwise replace the link and leave the old private contents wherever it pointed.

**Reproduce:**

```bash
cargo test -p zx402-zk --lib store
```

```bash
python3 -I scripts/mutation-audit.py 'wallet '
```

The audit rewrites source files while it runs, so use a copy of the repository.

| Check, on a clean copy with no `target/` or `res/` | Actual result |
| --- | --- |
| The first two new tests, on the old code | Both failed: mode `420` (0644), and the victim file held the wallet JSON |
| `leftovers_on_the_names_a_save_tries_are_skipped_untouched` | Passes. It puts a 0644 file, a symlink and a dangling symlink on the names a save really tries, because the first two tests would also pass from the rename alone |
| Other new tests | No free name: the save fails and the wallet is unchanged. Failed rename: no temporary file left. A readable wallet is 0600 after its next save. Symlinked wallet path refused. Arguments not written through a symlink |
| Targeted mutants, 8 | 8 killed, 0 survived. Removing `create_new` is killed by the collision test |
| `cargo test --workspace` | **160 pass**, 0 failed, 0 ignored: the 152 before plus 8 new |
| Clippy with `-D warnings`, `cargo fmt --check` | Clean |
| `scripts/demo.sh` | Exit 0. $0.90 paid, $0.00 to the attacker |
| `scripts/demo-zk.sh` | Exit 0. 0.05 mUSDC paid, 0.00 to the attacker |
| Code hashes | Policy `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU` and token `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g`, unchanged. No contract code changed, so this needs no redeploy |
| `wallet.json*` under the project directory | None, so no real wallet was exposed there. This did not search the rest of the machine |

**Known limits:**

- The `fsync` calls have no test that fails without them. Durability still rests on the order of the calls, and power loss is not tested.
- A crash between creating and renaming leaves a 0600 temporary file holding the private data. Nothing removes it automatically.
- A parent directory that others can write to is outside this defence, and the code does not check for it.
- The `.lock` file holds no data, and opening it follows a symlink.
- A wallet made readable from outside stays readable until its next save. `open` does not refuse it.

## Codex's review of checkpoint `8e63e0b`

Reported by Codex on 7 Nov. This is Codex's own evidence, recorded here and not re-run by Claude. **Codex states that this review does not certify soundness.**

| Check | Codex's result |
| --- | --- |
| Changes from `1c0241f` to `8e63e0b` | Documentation only |
| Clean build, compared with the view at `FINAL` on testnet | Policy and token hashes match, at blocks 276621100 and 276621104 |
| `offer_selection`, `receipt_theft`, `zk_double_spend` after the rename | Pass |
| `context unbound`, `invoice unbound`, `agent's day as input` | Re-run: equivalent on the current tests. The source of `LibsnarkReduction` does put public inputs into the QAP |
| `intents unbounded` | Selected along with those, and killed |
| A failed verifier host test | Failed because the compiled manifest path still pointed to `/tmp` after the clone moved, and passed after recompiling. Not a product bug |

**Still open:** Poseidon external vectors and invariant-subspace checks, the gadgets' full internal witnesses, recovery of `reserved` if a callback fails (now implemented, and awaiting review), and the estimate of `Failed` log spam.

## Latest evidence (7 Nov 2026, after the rename to nearx402)

Nothing was pushed. The results below are from a local sandbox. **Separately, the contracts are now deployed on testnet, and the flow has not run there.** Claude did not do that deploy. See [testnet.md](../testnet.md) for the state read from the chain.

| Check | Actual result |
| --- | --- |
| `cargo test --workspace` | **152 tests pass** |
| Clippy with `-D warnings`, `cargo fmt --check` | Clean |
| `scripts/demo.sh` | $0.90 paid, $0.00 to the attacker |
| Old project name in the repo's files and git history | 0 matches |
| Policy code hash | `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU` |

**`scripts/demo-zk.sh` from a clean copy:** exit 0 in 136 s, including the compile. No `target/` or `res/` in the copy.

| Step | Actual output |
| --- | --- |
| Request | `HTTP 402`: pay 0.05 mUSDC to `data-api.sandbox` |
| Payment | `21gQSspPpqWKL6LhYSiAfQBDz1i9FNfRc2QVFLjDmw2t`, 41.15 Tgas, proof verified |
| Observer without the secret | `HTTP 402` |
| Claim | `HTTP 200` with the data. The same receipt again gets `HTTP 402` |
| Attempts A and B | `8451JfQA…` at 36.09 Tgas and `Fw5AzZC8…` at 36.12 Tgas: both `invalid proof`, with no token transfer |
| Attacker, wallet | Attacker 0.00 -> 0.00 mUSDC, wallet 9.95 -> 9.95 mUSDC |
| Private values | The unpaid vendor, the cap, and the per-call limit are absent from all 1,797 bytes of contract storage |

This run came before the tests added for the soundness answers. Those changes touched tests only: the hashes of the contract's `lib.rs`, `agent.rs`, `x402.rs`, `facilitator.rs`, `deployment.rs`, `demo-zk.rs`, and `store.rs` are the same before and after.

### Mutation audit: 90 mutants, 87 killed, 3 equivalent, 0 survived

| Run | On | Total | Killed | Equivalent | Survived | Timed out | Not run |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Full, 00:29 to 01:07 UTC | Clean copy, before the soundness tests | 86 | 82 | 3 | 0 | 0 | **1** |
| Targeted, 01:13 to 01:19 UTC | Clean copy, final tree | 12 | 12 | 0 | 0 | 0 | 0 |

**The one not run:** `offer list stops at the first` failed to compile, because of a mistake in how I wrote the mutant, so the full run exited 1. I rewrote it, and it is killed by `a_valid_offer_is_found_behind_bad_ones`.

**The targeted run** covers that mutant, 4 new ones (`poseidon: too few partial rounds` and three `inputs:` mutants on the contract), and 7 re-run against the strengthened and new tests. So the 90 mutants did not all run together on one tree. The other 74 ran before the tests were added, and only tests changed after that.

**Survivors: none.** The 3 that stay alive are on the equivalent list, and each has a reason to challenge:

| Mutant | Why it changes nothing |
| --- | --- |
| `context unbound` | arkworks binds public inputs by itself: `real_proof_binds_every_public_input` still passes without this constraint. It stays as defence in depth, for one constraint |
| `invoice unbound` | The same |
| `agent's day as input` | The stale-day check just above forces `expected_day == day`, so the values are equal. `agent controls the day` removes both, and is killed |

**One thing the strengthened tests changed:** `leaf not from the public vendor` is now killed by one test, and no longer by `empty_allowlist_slot_is_not_a_valid_leaf`. That test used to fail on this mutant only because its path was wrong. It now tests the empty slot, and a wrong leaf misses the root there anyway.

**Added since, for CX-02:** 7 new mutants, which brings the script to 97. They and `wallet readable by others` ran as a targeted set on the final tree, and all 8 were killed. The other 89 were not re-run for that change, which touched only the wallet file's writing and the setup arguments.

**What this does not show:** an audit shows that tests depend on the checks that exist. It cannot find a check nobody wrote, so it does not show the circuit is sound.

## How we work

Roles and rules are in [AGENTS.md](../../AGENTS.md). In short: Claude changes the code, Codex reviews and writes findings to [findings.md](findings.md) with file, line, a concrete failure scenario, and a severity. A finding closes when a test or measurement backs the answer and Codex has verified it. Steven decides anything unresolved.

## Findings tracked for Codex's verification

CX-01 and the receipt theft are **two separate findings**. The receipt secret does not close CX-01, and the offer check does not close the receipt theft.

| Finding | When it happens | Status | Reproduce |
| --- | --- | --- | --- |
| CX-01: pays before checking the offer's terms | **Before** payment | Fixed. Codex verified it on the tested path **before the rename**, and the tests pass after it | `cargo test -p nearx402 --test offer_selection -- --nocapture --test-threads=1` |
| CX-01, follow-up: redirect carries the proof | After payment, on the retry | Fixed. Same verification as above | Same command |
| CX-01, follow-up: HTTP 200 lost after settlement | After settlement | **Reported, not recovered.** Decision below | Same command |
| Receipt theft: observer claims first | **After** payment | Fixed. Same verification as above | `cargo test -p nearx402 --test receipt_theft -- --nocapture` |
| CX-02: leftover temporary file exposes the wallet | Any save | **Closed by Codex for the tested paths**, with the limits in its section | `cargo test -p zx402-zk --lib store` |

## CX-01: the agent paid before checking the offer

Design and limits are in [SD-015](../security-decisions.md). A controlled server in front of the real vendor rewrites the offers in a 402 and decides what happens to the retry. The vendor is approved and the price is within budget in every case.

**Actual results.** `Ledger` is the policy wallet, the vendor's balance, the log length, and the paid retries the server received.

| Case | Before the fix | After the fix |
| --- | --- | --- |
| Asset mismatch | `wallet 10000000 -> 9950000, vendor 0 -> 50000, log 0 -> 1, retries 0 -> 1`, then `vendor rejected the payment` | All unchanged. `no compatible payment offer: asset "other-token.sandbox" is not usdc.sandbox` |
| Network mismatch | Stopped at the asset case above, so not reached before the fix | All unchanged. `network "near-mainnet" is not near-sandbox` |
| Policy hash mismatch | Not reached before the fix | All unchanged. `policy hash "1111…" is not <the deployed hash>` |
| Other resource, or no offers | Not run before the fix | All unchanged |
| Bad offers, then a valid one | Not reached before the fix | The valid one is chosen and paid: wallet -0.05, vendor +0.05, log +1, `Fetch::Paid` |
| Redirect on the first GET | Not run before the fix | Refused, all unchanged, and the other origin saw nothing |
| Redirect on the paid retry | **Other origin received 1 proof, and the agent returned `Paid` with that origin's body** | Other origin received 0. Error names the redirect, the log entry and the transaction |
| HTTP 200 lost after settlement | Wallet -0.05, vendor +0.05, and an error that did not say the payment had happened | Same money movement. Error says `already paid on chain as log #0, transaction …: do not pay again` |

**The ZK path.** `demo-zk`'s `quote` took the first offer and checked only `payTo` and the price, read from the code. It now shares `agent::quote`. `the_zk_client_checks_an_offer_before_proving` runs asset, network and hash mismatches with an unchanged ledger, chooses a valid second offer, proves, sends `pay_zk`, shows the claim's redirect is not followed, and then claims at the real vendor. Its limit: it drives `quote` and the `pay_zk` steps directly, not a hostile vendor against the `demo-zk` binary, whose vendor runs in the same process.

**A lost HTTP 200, as asked.** The money has moved, the facilitator has redeemed the receipt, and presenting it again gets `402 payment already redeemed` (shown in `receipt_theft`). The agent gets no data and now says not to pay again. **Recommendation to Steven: accept this as a documented limit for the hackathon.** Recovery would need a second response for the secret's holder, which weakens "one receipt, one response". The loss is bounded by one payment.

**Please challenge:** whether reading the asset and hash from the chain per fetch is right, compared with configuring them, and whether refusing all redirects is too strict for real vendors.

## Finding fixed by Claude: a payment proof could be rebuilt from public data

Found on 7 Nov while comparing with another x402 project. **Fixed in the working tree, and awaiting Codex's verification.** Design and limits are in [SD-014](../security-decisions.md).

Every other field of `X-PAYMENT` is public once the payment is on chain: contract, `log_id`, the nonce in the memo, and the transaction hash. Nothing tied the proof to whoever presented it, and the vendor accepted the header from anyone. The README's claim that an API key stops "someone else settles a vendor's payment first" held only for direct calls to the facilitator.

**Before the fix, on a sandbox:** the observer's rebuilt proof got `HTTP 200` with the data. The agent would then get `402 payment already redeemed`. The size of the race window was not measured, and no longer matters to the fix.

**Fix:** the memo is `nonce:base64url(sha256(domain, secret))`, and the proof reveals the secret. No contract change.

**Reproduce:**

```bash
cargo test -p nearx402 --test receipt_theft -- --nocapture
```

`receipt_theft` tries a guessed secret, the public commitment as a secret, a proof with no secret, and another payment's secret. All get `402`. None of them costs the agent its claim, which then gets `200`, and once only. `demo-zk` now shows an observer's attempt too.

**Please challenge:** whether a commitment in the memo is the right binding compared with a signature, the hash's domain separation, and the TLS assumption.

## Milestone: testnet code, rehearsed on a local sandbox (7 Nov 2026)

Finance and the agent can now be separate runs against contracts that already exist. **Nothing has run on testnet**, and nothing is deployed, pushed, or committed.

| What | Where |
| --- | --- |
| Setup tool: saves the proving key and wallet, writes the arguments finance sends, prints the commands | `crates/nearx402/src/bin/zk-setup.rs`, `crates/nearx402/src/deployment.rs` |
| Wallet file: next state saved before sending, then the chain decides | `crates/zx402-zk/src/store.rs`, [SD-013](../security-decisions.md) |
| Demo on an existing deployment, checking it before spending | `demo-zk --network testnet --config <file>`, template in `deploy/testnet.example.json` |
| View of the stored verifying key | `get_zk_verifying_key` in `contracts/policy/src/lib.rs`. This changed the policy's code hash |

The sandbox demo now goes through the same setup and wallet file as a testnet run.

**Reproduce:**

```bash
scripts/build-contracts.sh
```

```bash
cargo test -p nearx402 --test zk_separate_runs -- --nocapture
```

**What that test does.** It runs `zk-setup` and `demo-zk --network testnet` as **separate processes** against one local sandbox, sharing only files and the chain, and passes. All on one local chain:

| Case | Actual result |
| --- | --- |
| Setup output | Arguments and stdout hold no vendor name, limit, or salt. A second `init` is refused, and the wallet file is unchanged |
| No verifying key, or agent not registered | Refused, no transaction sent |
| Wrong wasm, wrong token, a full-access key, an allowlisted "outside" recipient | Refused, no transaction sent |
| Proving key or wallet from another setup | Refused, no transaction sent, breaker still 0 |
| Run 1, then run 2 in a new process | Both exit 0. Vendor 0.05, then 0.10 mUSDC. Attacker 0.00. `spent` accumulates in the file |
| Those runs' output | Holds no unpaid vendor, limit, or vendor count |
| Run 3 the same day | Refused before spending: 2 more attempts would freeze the agent. Breaker stays at 4 of 5 |
| Crash after sending | Next open adopts the chain's state, and a further payment works |
| Crash after saving the intent, before sending | Next open stays on the current state, and a payment works |
| Token refuses the transfer | `Failed`. State and balance restored, breaker not counted, and the next payment works |
| Old backup restored | Refused, naming `zk-setup recover`. Recovery changes nothing on chain until finance sends `set_zk_agent`, and the wallet then adopts it and pays |

`store.rs` has 14 more tests, including a payment still in flight, a send that lands after a newer intent, a damaged file, a second process, and file permissions.

**Other checks on this tree:** 132 tests pass in `cargo test --workspace`, with the one correction below applied afterward and the rehearsal re-run. Clippy with `-D warnings` is clean. `scripts/demo-zk.sh` still exits 0 at 41.11 Tgas, and `scripts/demo.sh` still ends at $0.90 paid and $0.00 to the attacker.

**One correction found by reading the output.** A first version printed one "not yet paid" line per unpaid vendor, which would reveal how many vendors are allowed in a recording. It now prints one line, and the test asserts that.

**Known limits:**

- **Not run on testnet.** The `near` commands in [testnet.md](../testnet.md) were only checked against `--help`. Testnet RPC limits, gas, and finality are unmeasured.
- The vendor and facilitator run on the local machine. Hosting is not built.
- The crash cases drop the process's state at a chosen point in a test. No process is actually killed mid-write, and power loss is not tested. The save order relies on `fsync` and an atomic rename.
- The wallet file is plain text with owner-only permissions on Unix (SD-013). On non-Unix systems the permission is not set.
- `check_limited_keys` checks the format of the RPC's answer through unit tests and one real positive case on the sandbox.
- A run's two rejected attempts count against the breaker, so 2 runs fit in a UTC day at the default limit.

## Milestone: judge demo of Option C (7 Nov 2026)

Requested by Codex: one command from a clean sandbox, `pay_zk` in the demo and dashboard, real transactions, and results recorded here.

**Reproduce:**

```bash
scripts/demo-zk.sh
```

Add `--serve` for the public dashboard at http://127.0.0.1:8402/. The script exits non-zero if an outcome differs from the story, because each outcome is an `ensure!` in `crates/nearx402/src/bin/demo-zk.rs`.

**Actual result** of that command, from a clean copy made with `git ls-files -co --exclude-standard` (no `target/`, no `res/`, a different path). Exit 0, 129 s including the full compile. Transaction hashes differ on every run, since each run is a new sandbox.

| Step | Actual output | Read from |
| --- | --- | --- |
| Request | `HTTP 402`: pay 0.05 mUSDC to `data-api.sandbox` | Vendor response |
| Prove | 179 ms, 256 bytes | Local |
| `pay_zk` | `C8URjnA62MXrMPd1Vbgn7rivT8KH64PL9waN71oCAvtb`, 41.10 Tgas, status `Paid` | Node, `tx` RPC by hash |
| Proof verified | `policy.sandbox: zk_proof_verified` | Event in that transaction |
| Transfer | `usdc.sandbox: ft_transfer policy.sandbox -> data-api.sandbox, 0.05 mUSDC` | Event in that transaction |
| Vendor balance | 0.00 -> 0.05 mUSDC | `ft_balance_of` |
| Retry | `HTTP 200` with the data; the same receipt again gets `HTTP 402` | Vendor response |
| Attempt A: forced proof naming the attacker | `HpGRDyEvZJqi7QsUqzxkmjZeG4CbRMBXowtWjCXZPNoW`, 36.08 Tgas, `payment_blocked: invalid proof`, no transfer event | Node |
| Attempt B: valid `data-api` proof sent to the attacker | `8AAbjaNAMaNZeqtGpCdbc9R5gH5hy8RCNMbdvGnSnzjT`, 36.11 Tgas, `payment_blocked: invalid proof`, no transfer event | Node |
| Attacker balance | 0.00 -> 0.00 mUSDC | `ft_balance_of` |
| Wallet across the attempts | 9.95 -> 9.95 mUSDC; state commitment unchanged | Contract views |
| Public views | `get_agents() = []`, `get_vendors() = []` | Contract views |
| Private values | `gpu-cloud.sandbox`, the cap, and the per-call limit are absent from all 1,753 bytes of contract storage | `view_state` RPC |

**Dashboard, checked in a browser against a `--serve` run:**

- It showed 9.95 mUSDC, 1 payment, 2 blocked attempts, agent `zk-agent.sandbox` with a `private policy` badge and commitments only, and `data-api.sandbox` as the only vendor.
- Neither `gpu-cloud` nor either limit appeared on the page.
- A panel says what is and is not public. The recipient and amount of every payment are stated as public.
- The log rows carried real transaction hashes.

**Other checks on this tree:** 112 tests pass in `cargo test --workspace`, clippy with `-D warnings` is clean, and `scripts/demo.sh` still ends at $0.90 paid and $0.00 to the attacker.

**What the private-value check does and does not show:** it searches contract storage for the exact bytes of those values and the vendor's name. It does not search transaction arguments, although the code sends only commitments. Please challenge this if it is too weak.

**Known limits of this demo:**

- Local sandbox only. The plan is in [testnet.md](../testnet.md); nothing has been deployed.
- Finance and the agent are one process, so the narration prints the private limits. They do not go on chain.
- Hashes in the dashboard's log come from a list the demo server keeps for that run. The contract's log does not store them.
- The dashboard's money now reads mUSDC, while `scripts/demo.sh`'s terminal output still prints `$`.
- `pay_zk` gas went from 41.01 to 41.10 Tgas when the `zk_proof_verified` event was added. Tables in `zk-policy.md` were measured before that.
- **The mutation audit has not been re-run** since that event and `get_zk_agents` were added. A full run of the final script is still to do.
- The six soundness questions below had no answer at this milestone. Claude's own answers are now recorded there, and they are not an independent review.

## Milestone: Option C policy circuit and `pay_zk`

- **Evidence and limits:** [zk-policy.md](../zk-policy.md). Threats: [threat-model.md](../threat-model.md). Decisions: [security-decisions.md](../security-decisions.md).
- **Not on testnet.**

**Most useful targets for challenge:**

1. `crates/zx402-zk/src/policy.rs`: any statement that satisfies the circuit but breaks the policy. A counterexample is worth more than any test we have.
2. `contracts/policy/src/lib.rs`, `pay_zk` and `on_pay_transfer`: state that can get stuck or skip a check.
3. The claim that `context.square()` and `invoice.square()` are redundant, because arkworks binds inputs itself.
4. The recommendation not to count malformed proofs in the breaker (SD-004).
5. Whether Option C earns its place in the demo at all, given that vendor and amount stay public. See the question for Steven below.

## Soundness questions: Claude's answers, for Codex to challenge

Requested by Codex on 7 Nov. **These answers come from the code's author, so they are not an independent review.** I looked for counterexamples and did not find one in the rule itself, which does not show none exists. The search did find weak tests, listed in question 6. "Read" below means the claim rests on reading the code, and no test shows it.

Paths: `policy.rs` is `crates/zx402-zk/src/policy.rs`, `zk.rs` is `contracts/policy/src/zk.rs`, and `lib.rs` is `contracts/policy/src/lib.rs`.

### Priority: the wrong recipient

| | |
| --- | --- |
| **Rule** | Tokens can go only to the vendor the proof names, and that vendor must be on the private allowlist |
| **Code** | `lib.rs:590` hashes the `vendor` argument into the public input, and `lib.rs:634-637` transfers to that same value. `policy.rs:335-351` starts the Merkle path from that public input |
| **Attack tests, mocked VM** | `unapproved_vendor_is_rejected`; `proof_for_one_vendor_cannot_pay_another`, which covers the attacker and another allowlisted vendor |
| **Attack tests, circuit** | `vendor_outside_the_allowlist_is_rejected`, `allowlisted_path_cannot_be_reused_for_another_vendor`, `empty_allowlist_slot_is_not_a_valid_leaf`, `a_false_statement_cannot_be_proved` |
| **Attack tests, sandbox** | `zk_policy` steps 3 and 4, and `demo-zk` attempts A and B: `invalid proof`, no `ft_transfer` event, attacker balance unchanged |
| **Mutants** | `vendor is not the transfer target`, `leaf not from the public vendor`, and `allowlist root` are all killed |
| **Gap** | The token is trusted as a fixed account. A second preimage of the 248-bit vendor hash would pass, at about 2^248 work |

### Priority: two payments from one balance or daily limit

| Attack | Stopped by | Code | Test |
| --- | --- | --- | --- |
| Two proofs from one state, sent together on a real chain | The in-flight marker | `lib.rs:556` | **New:** `zk_double_spend`. Actual: `a` was `Paid`, and `b` reverted with `a payment is already in flight`. The money moved once, and the wallet file adopted the winner |
| The same, one after the other | Stale-state check, or failed verification if the agent lies about the state | `lib.rs:564`, `lib.rs:586` (the input comes from storage) | **New:** `two_proofs_from_one_state_cannot_both_spend_it`. Also `same_proof_cannot_be_replayed`, `rolled_back_state_fails_verification` |
| Claiming nothing was spent today, or an earlier day | Old state commitment | `policy.rs:332` | `a_different_old_state_is_rejected`, **strengthened** (question 6) |
| Writing a lower spend or another day into the new state | New state constraint | `policy.rs:366` | `new_state_must_be_the_exact_transition` |
| Proving for tomorrow to reset the budget | The contract supplies the day | `lib.rs:563`, `policy.rs:358-363` | `proof_for_a_fabricated_future_day_does_not_reset_the_cap`, `proof_for_a_past_day_is_rejected` |
| Overspending across several payments | Daily cap constraint | `policy.rs:363` | `cumulative_overspend_is_rejected`; `zk_policy` steps 12 and 13 |
| Two agents spending one balance | Balance is reduced in the same receipt | `lib.rs:573`, `lib.rs:618` | **New:** `two_agents_cannot_spend_the_same_balance` |
| Finance withdrawing funds a payment has reserved | Withdraw reads `balance`, not `reserved` | `lib.rs:382-386` | **New:** `finance_cannot_withdraw_what_a_payment_has_reserved`, plus `owner_cannot_withdraw_reserved_funds` for `pay` |
| A failed transfer used to get a budget back and spend it twice | Nothing moved, so the restore is correct, and the proof can only pay once | `lib.rs:799` | `failed_transfer_restores_budget_state_and_funds` |

**Gaps:**

- The cap is per UTC day, so an agent can spend up to two caps around midnight, by design.
- The cap is per agent. Finance giving two agents the same policy gives them two caps.
- `zk_double_spend` shows one race on one local chain. It shows the rule holding, not every interleaving.
- Removing the in-flight marker alone would still leave the stale-state check behind it. The race test does not show that second layer, because the first one stopped it.

### 1. Can a statement satisfy the circuit and break the policy?

**I found none.**

| Area | Rule and reason | Code | Test | Gap |
| --- | --- | --- | --- | --- |
| Wraparound | All amounts are under 2^128 and days under 2^64, against a 254-bit field. If `a > b`, then `b - a` wraps to about 2^254, which fails the range check | `policy.rs:257-280`, `:320-325` | `oversized_witness_values_cannot_wrap_the_comparisons`, `oversized_days_are_rejected`, `negative_amount_cannot_wrap_the_field` | None found |
| `SUM_BITS` | `spent + amount` is under 2^129 in the same-day branch, and under 2^128 otherwise. Both fit, and 129 bits cannot wrap | `policy.rs:360-363` | `exceeding_the_daily_cap_is_rejected`, `spending_exactly_up_to_each_limit_is_allowed` | None found |
| `same_day` | `old_day` comes from the commitment and `day` from the contract, so the agent cannot choose the branch | `policy.rs:358-360` | `a_later_day_resets_the_budget`, `same_day_payments_accumulate` | **Internal witness of `is_eq`:** read, relies on arkworks |
| Direction bits | They are boolean-constrained and free. A wrong bit only changes the hash, which must still reach the committed root | `policy.rs:342-349` | `allowlisted_path_cannot_be_reused_for_another_vendor` | Read for the boolean constraint |
| Leaf and node | Separate domain tags and a fixed depth of 8, so a node cannot pass as a leaf | `policy.rs:58-61`, `:93-99` | **New:** `hashes_of_different_kinds_never_share_a_value`. This is a sanity check, not a proof | Rests on Poseidon's collision resistance |
| Empty slots | Hold zero, which would need a preimage of `leaf_hash` to reach | `policy.rs:152` | `empty_allowlist_slot_is_not_a_valid_leaf`, **fixed** | Rests on Poseidon's preimage resistance |
| Native and circuit hash agree | An honest statement satisfies the circuit only if they match, for all four kinds of hash | `policy.rs:76-83`, `:242-253` | `valid_statement_satisfies_the_circuit`, `every_allowlist_slot_has_a_valid_path` | Not tested over random inputs |

**The main gap:** the tests play a malicious prover through `PolicyWitness`, and through public inputs. They do not forge the gadgets' internal witnesses, such as range-check bits or `is_eq`'s helper values. `range bits not tied to the value` is killed, but that removes a constraint and does not forge a value. Those parts rest on the argument above and on arkworks.

### 2. Is `poseidon_config` right?

| | |
| --- | --- |
| **Rule** | t = 3 (rate 2, capacity 1), x^5, 8 full and 57 partial rounds over BN254's scalar field, the values I know from the Poseidon paper and circomlib for t = 3 |
| **Code** | `crates/zx402-zk/src/lib.rs:48-58`. The arguments match the signature in `ark-crypto-primitives` 0.5, read from its source |
| **Test** | **New:** `poseidon_parameters_have_the_expected_shape_and_an_mds_matrix` checks the shape, 195 distinct round constants, and that every square submatrix is nonsingular |
| **Gap 1** | **`skip_matrices = 0` takes the first matrix unchecked.** A comment in arkworks names three security tests, and its code does not run them. So the invariant-subspace checks from the reference script have **not been run** for this matrix. Worth doing before any production use, and it needs Sage or a port |
| **Gap 2** | The constants are compared with no external vector. arkworks' own generation is pinned only by the library's own tests |
| **Gap 3** | Domain tags are absorbed as a first element, not as Poseidon's standard capacity tag. Each domain has a fixed length, so I see no ambiguity. This is my reasoning, and it has not been reviewed |

### 3. Do the two sides agree on public inputs?

| | |
| --- | --- |
| **Rule** | Same order, same SHA-256 with a domain tag and `u32` length prefixes, the same truncation to 248 bits |
| **Code** | `policy.rs:103-125`, `:205-216`; `zk.rs:65-86`; `lib.rs:584-593` |
| **Test** | **New:** `both_sides_derive_the_same_public_inputs` compares both implementations directly on empty, multibyte, and 128-byte strings, and at the largest `u64` and `u128`. It also checks that `("ab","c")` differs from `("a","bc")`, and that vendor and invoice tags differ. `real_proof_binds_every_public_input` changes each input. `honest_payment_transfers_and_advances_state` and the sandbox show the order works end to end |
| **Cheaper than a collision?** | I found no way. Different strings differ before hashing, because of the tag and length prefix. NEAR account ids are validated by `AccountId` before they reach the hash |
| **Gap** | Truncation gives about 2^124 work for a collision among agent-chosen memos, which only matters for two invoices with one hash, and about 2^248 for a second preimage of a vendor. The domain tags are kept in step by hand in two files, guarded by the new test |

### 4. Can contract state get stuck, skip a check, or lose funds?

| Case | Behaviour | Code | Test | Gap |
| --- | --- | --- | --- | --- |
| `pending` stuck | Cleared by the callback, or by `set_zk_agent` | `lib.rs:796-801`, `:474` | `rotated_agent_is_not_rolled_back_by_a_late_failure`, `rotation leaves the in-flight marker` killed | Recovery needs finance |
| Late callback after rotation | Touches only its own marker | `lib.rs:796` | `late_failure_does_not_disturb_a_newer_payment` | None found |
| `reserved` and `balance` | Moved together on pay, settle, and failure. `sync_balance` counts both | `lib.rs:618-619`, `:783-786` | `failed_transfer_restores_budget_state_and_funds`, `two_agents_cannot_spend_the_same_balance` | Not tested against a randomly generated sequence of calls |
| Callback out of gas | Reserved statically at 15 Tgas against a measured 2 Tgas, and `pay_zk` requires 90 Tgas. A shortage would fail the call before the state is saved | `lib.rs:25-32` | **None:** read, and not reproducible in these tests | If the callback ever failed, the entry stays `Pending` and `reserved` stays high. `set_zk_agent` clears `pending`, but not `reserved`. `reconcile_payment` was added later for this |
| `withdraw` in flight | Limited to `balance` | `lib.rs:382` | `finance_cannot_withdraw_what_a_payment_has_reserved` | None found |
| Owner rotation | Resets the budget, as finance's decision | `lib.rs:461-474` | `policy_rotation_invalidates_outstanding_proofs`, `old_policy_fails_even_when_named_as_current` | By design |
| Skipped check | The frozen check comes first, and the stale checks and verification are unconditional on every path to the transfer | `lib.rs:559-601` | `frozen_agent_cannot_pay_even_with_a_valid_proof`, and the mutants | Read for "every path" |

**A gap to weigh:** a compromised agent can repeat payments to an allowlisted vendor that is not registered with the token. Each ends as `Failed`, which the breaker does not count, as agreed in round 1, and each adds a log entry that the contract's storage pays for. **Rough estimate, not measured:** a 1 NEAR key allowance gives about 240 attempts at 41 Tgas, and so about 0.7 NEAR of storage at about 300 bytes per entry. No token moves. It needs an unregistered vendor on the allowlist, and the allowance bounds it.

### 5. Is the verifier equation right, and can identity points pass?

| | |
| --- | --- |
| **Rule** | `e(A,B) * e(alpha,-beta) * e(vk_x,-gamma) * e(C,-delta) = 1`, which is Groth16's `e(A,B) = e(alpha,beta) e(vk_x,gamma) e(C,delta)` moved to one side |
| **Code** | `zk.rs:112-145`; negation in `near_vk`, `crates/zx402-zk/src/lib.rs` |
| **Tests** | Valid and invalid proofs through real host functions in `near_verifier` and `zk_policy`. **New:** `identity_points_do_not_make_a_proof_pass` replaces every subset of A, B, and C by the identity, for true and false statements: all 14 are `invalid proof`, with nothing moved. Step 8 in `zk_policy` covers invalid coordinates |
| **Why identity points fail** | Identities remove their own pairing, and leave `e(alpha,-beta) e(vk_x,-gamma)`, which is not 1 unless someone knows the setup's discrete logs. The agent influences `vk_x` only through `new_state`, and cannot reach the identity without those logs. This is reasoning, not a test |
| **Gap** | Whoever knows the setup's randomness can forge everything (SD-007). The owner can replace the verifying key (SD-012). Proofs are malleable, so replay protection rests on `old_state` alone, as designed. G2's subgroup check is the host's, and `zk_spike` would notice a change |

### 6. Does a negative test pass for the wrong reason?

**Yes, two did**, and both are now fixed. Neither hid a bug in the circuit. The other negative tests I read fail at the constraint they name, or rely on a mutant to show it. I read every test in `policy.rs` and most of `zk_tests.rs`, not all of the older tests in `lib.rs`.

| Test | Problem | Fix |
| --- | --- | --- |
| `empty_allowlist_slot_is_not_a_valid_leaf` | It claimed to test slot 2, but used slot 0's path, so it never touched an empty slot | Builds slot 2's real path, with a control showing that a raw zero reaches the root from there |
| `a_different_old_state_is_rejected` | "Claims nothing was spent" and "claims an earlier day" left `new_state` unchanged, so they could fail on the new state instead. The mutant was killed only by the third case. **This is the main attack on the daily limit** | The new state is sealed for each lie, with a control showing the rest of the statement is consistent |

**A test that failed for its own reason this time:** the first version of `identity_points_do_not_make_a_proof_pass` tripped the breaker, because the mocked VM keeps its storage between `fix()` calls in one test. Its panic named the breaker, so this was caught at once. Any test calling `fix()` twice shares that storage.

The strengthened and new tests were re-run under mutation afterward, as recorded in [Latest evidence](#latest-evidence-7-nov-2026-after-the-rename-to-nearx402).

## Findings from Codex, round 1

Received through Steven on 6 Nov 2026. Each answer needs Codex's verification before it closes.

| # | Finding | Answer | Evidence |
| --- | --- | --- | --- |
| 1 | Verify the unit of 41.01 | **Valid, and confirmed.** 41.01 is Tgas, for the whole transaction | Log: `41.01 Tgas (41006297832181 gas)`. The test asserts total = sum of steps. [Breakdown](../zk-policy.md#gas) |
| 2 | Report proving and E2E time separately | **Done.** Prove 210.8 ms median; whole flow 1,247 ms median on a local sandbox. The earlier 1.7 s was a debug-profile number and should be disregarded | [Time](../zk-policy.md#time) |
| 3 | Malformed curve points can abort the host | **Valid.** 7 invalid-coordinate cases, all 256 bytes: reverted, with balance, commitment, breaker count, and log unchanged, at 35.55 Tgas each | Sandbox step 8 in `crates/nearx402/tests/zk_policy.rs` |
| 4 | Tell payment failures from counted attempts | **Done**, as three results: `Failed` not counted, `Blocked` counted, revert neither | [Table](../threat-model.md#how-a-pay_zk-call-can-end), `failed_transfer_is_not_counted_by_the_breaker`, sandbox step 11 |
| 5 | Report surviving and timed-out mutants | **Done.** 44 mutants: 41 killed, 3 equivalent, 0 timed out. A full run of the final script is still to do | [Mutation audit](../zk-policy.md#mutation-audit) |
| 6 | Limit privacy claims to the policy | **Done** in the README, `zk-policy.md`, and the threat model. Vendor and amount are stated as public | [Limits](../threat-model.md#6-what-each-observer-learns) |

## Open question for Steven

From the official rules, Claude Code and Codex screen every project first, checking that it runs and does what its README says, and humans then decide. Past NEAR hackathons judged technical implementation, design, impact, originality, and sustainability. 204 projects are already listed. Of the 8 on the first page, 5 are tagged both AI Security and Agent Economy, and their names suggest spend limits for agents. We have not read those projects yet.

So the question is what makes this entry different and visible to a judge. The demo now exists. Next come Steven's testnet decisions, listed in [testnet.md](../testnet.md).

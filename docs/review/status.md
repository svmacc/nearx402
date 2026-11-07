# Review status

**State: READY for review.** No audit is running. This changes to `NOT READY` whenever `scripts/mutation-audit.py` runs on this tree, because it rewrites source files for short periods.

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

## Latest evidence (7 Nov 2026, after the rename to nearx402)

Nothing is committed or pushed. The results below are from a local sandbox. **Separately, the contracts are now deployed on testnet, and the flow has not run there.** Claude did not do that deploy. See [testnet.md](../testnet.md) for the state read from the chain.

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

**What this does not show:** an audit shows that tests depend on the checks that exist. It cannot find a check nobody wrote, so it does not show the circuit is sound.

## How we work

Roles and rules are in [AGENTS.md](../../AGENTS.md). In short: Claude changes the code, Codex reviews and writes findings to [findings.md](findings.md) with file, line, a concrete failure scenario, and a severity. A finding closes when a test or measurement backs the answer and Codex has verified it. Steven decides anything unresolved.

## Findings tracked for Codex's verification

These are **two separate findings**. The receipt secret does not close CX-01, and the offer check does not close the receipt theft.

| Finding | When it happens | Status | Reproduce |
| --- | --- | --- | --- |
| CX-01: pays before checking the offer's terms | **Before** payment | Fixed. Codex verified it on the tested path **before the rename**, and the tests pass after it | `cargo test -p nearx402 --test offer_selection -- --nocapture --test-threads=1` |
| CX-01, follow-up: redirect carries the proof | After payment, on the retry | Fixed. Same verification as above | Same command |
| CX-01, follow-up: HTTP 200 lost after settlement | After settlement | **Reported, not recovered.** Decision below | Same command |
| Receipt theft: observer claims first | **After** payment | Fixed. Same verification as above | `cargo test -p nearx402 --test receipt_theft -- --nocapture` |

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
| Callback out of gas | Reserved statically at 15 Tgas against a measured 2 Tgas, and `pay_zk` requires 90 Tgas. A shortage would fail the call before the state is saved | `lib.rs:25-32` | **None:** read, and not reproducible in these tests | If the callback ever failed, the entry stays `Pending` and `reserved` stays high. `set_zk_agent` clears `pending`, but **nothing clears `reserved`** |
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

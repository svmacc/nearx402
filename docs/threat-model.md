# zx402 threat model

Status: 6 Nov 2026. Covers v0.2 and Option C (`pay_zk`) as built, on a local sandbox only. Neither is on testnet, and nothing here is a third-party audit. Decisions and their reasons are in [security-decisions.md](security-decisions.md); measurements are in [zk-policy.md](zk-policy.md) and [zk-spike.md](zk-spike.md).

## 1. What we promise

1. A prompt-injected agent cannot move funds outside the finance team's policy.
2. The agent never holds treasury funds or a general-purpose key.
3. A vendor receives a receipt that can be verified and redeemed at most once.
4. Privacy claims match the rail that actually ships. A transparent NEP-141 transfer hides neither recipient nor amount.

## 2. Actors and trust

| Actor | Holds | Trusted for | Assumed able to |
| --- | --- | --- | --- |
| Finance (owner) | Owner account key, policy plaintext | Setting policy, funding, recovery | Change policy at will, so it must not be able to redeem or forge a vendor's receipt |
| Agent | Function-call key limited to the policy contract; for Option C also the policy opening and state secrets | Nothing | Be fully compromised: chosen inputs, arbitrary proofs, retries, overlapping calls |
| Vendor | API key for the facilitator, HMAC nonce secret | Its own pricing | Replay receipts, overcharge in its 402 response up to the per-call limit |
| Facilitator | Vendor API keys, redemption store | Honest verification for its vendors | See every payment for its vendors |
| Policy contract | USDC balance, policy, log | Enforcing rules | Locked: no access keys, so code cannot change after deployment |
| Token contract | Balances | Correct NEP-141 behaviour, identified by account id | Fail a transfer |
| Chain observer | Everything public | Nothing | Read all arguments, events, and storage |

## 3. Trust boundaries

```text
             untrusted                 |            trusted by the rule set
                                       |
 Agent runtime (LLM, tools, prover) ---+--> policy.pay / pay_zk --> NEAR runtime
 Vendor 402 response (untrusted input) |        ^ derives day, vendor, amount binding
                                       |        |
 X-PAYMENT receipt (untrusted input) --+--> Facilitator --> NEAR RPC (final outcomes)
```

- Everything the agent submits is untrusted, including its proof's public inputs. The contract must derive or check each one (section 5).
- Everything in a 402 response or a receipt is untrusted until the facilitator has checked it against the chain.
- The local Groth16 setup is trusted by assumption for the hackathon. Whoever holds its randomness can forge proofs (SD-007).

## 4. Threats to v0.2, as built

Test references are to `contracts/policy/src/lib.rs` (unit), `crates/nearx402/src/*.rs` (unit), and `crates/nearx402/tests/sandbox.rs` (end-to-end "step N").

| # | Threat | Control | Evidence |
| --- | --- | --- | --- |
| T1 | Injected instruction pays an unapproved address | Allowlist in contract; entry logged `Blocked`, no transfer | `unknown_vendor_is_blocked_and_logged`, step 4 |
| T2 | Amount above per-call limit | Contract check | `per_call_max_is_enforced` |
| T3 | Repeated calls exceed the daily cap | Contract check | `daily_cap_is_enforced_and_resets_next_day` |
| T4 | Agent fabricates a new day | Not possible: `pay` takes no day argument, and the bucket comes from `env::block_timestamp` | Same test, by construction |
| T5 | Agent escalates beyond `pay` | Function-call key; owner checks | `agent_cannot_approve_vendors`, step 6 |
| T6 | Receipt submitted twice | Redemption store | Steps 2, 3b |
| T7 | Concurrent redemption of one receipt | Mutex around an fsync'd insert, single process only | Step 3c: 16 parallel requests, one served. Treating a lost insert as success makes all 16 succeed, so the test does reach the race |
| T8 | `Paid` entry forged through a redeploy or storage rewrite | Facilitator requires the real `ft_transfer` event | Step 5b, `finds_the_token_transfer_event` |
| T9 | Contract is not the trusted code | Code hash check | Step 3 |
| T10 | Token differs from the trusted deployment | Requirements check and on-chain `token_id` check | `rejects_requests_it_cannot_handle_without_touching_the_chain`, step 3e |
| T11 | Wrong vendor, wrong invoice, underpayment, expired nonce | Facilitator and nonce checks | Steps 3a, 3d, `nonces_are_bound_to_secret_requirements_and_time` |
| T12 | Token transfer fails after initiation | Callback restores budget and balance; not `Paid` | `failed_transfer_restores_budget_and_balance` |
| T13 | Callback forged by an outsider | `#[private]` callback | Step 5, `blocked_entry_cannot_be_settled` |
| T14 | Freeze while an invoice is outstanding | Frozen agent is blocked deterministically | `frozen_agent_is_blocked` (the 402 nonce stays valid but cannot be paid) |
| T15 | Log spam from a compromised agent | Breaker freezes at the limit; later attempts revert without writing | Step 8, `attempts_after_the_breaker_trips_are_not_logged` |
| T16 | Observer rebuilds a receipt from public chain data and claims the resource first | The memo commits to a secret that the agent reveals only in its claim (SD-014) | `crates/nearx402/tests/receipt_theft.rs`: before this control the observer got HTTP 200 |
| T17 | A 402 names an approved vendor and an in-budget price, with a token, network, policy hash, or resource the payment cannot satisfy | Client compares the offer with what the chain says about its own contract, before signing, and skips to a compatible offer (SD-015) | `crates/nearx402/tests/offer_selection.rs`, for both the `pay` and `pay_zk` clients: before this control the wallet paid and the purchase failed |
| T18 | A redirect carries `X-PAYMENT` and its secret to another origin | The paying client follows no redirects (SD-015) | `a_paid_retry_does_not_follow_a_redirect_to_another_origin`: before this control the other origin received the proof, and the agent reported `Paid` |

**Known residual risks in v0.2**

- **A lost response is not recovered.** If settlement succeeds and the HTTP 200 is lost, the money has moved, the receipt is spent, and retrying gets `402 payment already redeemed`. The agent reports the log entry and transaction and says not to pay again, but it does not get the data. See SD-015.
- **The receipt secret is only as private as the claim's transport.** It travels in `X-PAYMENT` to the vendor and on to the facilitator. Without TLS on both hops, someone on the path can read it and race the agent's request. The local demo uses plain HTTP on loopback (SD-014).
- **The agent keeps the secret in memory.** A crash between paying and claiming loses that one response. The vendor has the money, and the payment cannot be claimed by anyone else (SD-014).
- **Redemption store is one process plus one local file.** Two facilitator instances sharing or duplicating that file can double-redeem. Deployment must run exactly one instance until the store moves to a transactional database (SD-006).
- **Finality.** The facilitator accepts a fetched transaction outcome. The wait level has not been reviewed against reorg behaviour.
- **The HMAC is hand-written** on top of `sha2` (`crates/nearx402/src/vendor.rs:174`), against the no-custom-primitives rule. Replacement awaits approval of the RustCrypto `hmac` crate (SD-008).
- **A callback that never runs** can leave an entry `Pending`, with its amount reserved. Finance can close such an entry with `reconcile_payment`, stating whether the tokens moved. The contract cannot check the claim, or that the transaction is `FINAL`, so finance must read that first. An owner's mistake can mislabel the log and leave `balance` above the tokens held, which makes later payments fail. The token still refuses to move more than it holds (SD-016). A withdrawal whose callback never runs stays reserved, because it has no log entry, and several such withdrawals add up.
- **Testnet evidence is one run** of an earlier build. See [testnet.md](testnet.md).

## 5. Proof bindings for Option C

Implemented in `crates/zx402-zk/src/policy.rs` and `contracts/policy/src/lib.rs` (`pay_zk`). Each public input has one source. The agent supplies only the proof and `new_state`. It also sends `expected_*` values, which the contract only compares against its own, to tell an out-of-date proof from an invalid one.

| Public input | Value | Source | Attack it blocks |
| --- | --- | --- | --- |
| `context` | `H(contract id, agent id, circuit version)` | Contract computes | Proof reused on another deployment or agent |
| `policy_commitment` | `Poseidon(cap, per_call, allowlist_root, salt)` | Contract storage, set by finance | Agent substitutes its own policy |
| `old_state` | `Poseidon(day, spent, salt)` | Contract storage | Rollback, forked state |
| `new_state` | `Poseidon(day', spent', salt')` | Agent, constrained by the proof, checked canonical | Arbitrary budget reset |
| `day` | UTC day from block timestamp | Contract computes | Fabricated day |
| `vendor` | `H(account id)` of the actual `ft_transfer` receiver | Contract computes from the same argument it transfers to | Allowlisted vendor differs from the paid one |
| `amount` | Actual `ft_transfer` amount | Contract uses the same argument | Proof for one amount, transfer of another |
| `invoice` | `H(nonce)` | Contract computes from the memo it records | Proof reused for another invoice |

Constraints the circuit must enforce:

1. `policy_commitment` opens to `(cap, per_call, allowlist_root, salt)`.
2. `old_state` opens to `(old_day, spent, salt)`.
3. `vendor` is a leaf under `allowlist_root`.
4. All of `amount`, `spent`, `cap`, and `per_call` are range-checked to 128 bits, so arithmetic cannot wrap in the field.
5. `amount <= per_call`.
6. `old_day <= day`; `spent' = amount` if `day > old_day`, otherwise `spent + amount`; `spent' <= cap`.
7. `new_state` opens to `(day, spent', salt')`.

The contract checks that `old_state` and `policy_commitment` equal the current stored values, and that every field element is canonical (below the BN254 scalar modulus), before it calls the verifier.

**Concurrency and failure.** An agent may have one transfer in flight. While it is pending, further `pay_zk` calls fail, so a retry can never race a state transition. If the transfer fails, the callback restores `old_state`, which is safe because nothing could have advanced it in between.

**Policy updates.** Finance rotating `policy_commitment` or resetting state invalidates any unsubmitted proof, because it no longer matches stored values. The result is deterministic rejection, and the agent reproves.

### How a `pay_zk` call can end

These are three different results. Only the second feeds the circuit breaker.

| Result | When | Log | Breaker | Funds and state | Gas, measured |
| --- | --- | --- | --- | --- | --- |
| **Failed payment** (`Failed`) | Valid proof; the token refuses the transfer | Yes | Not counted | Both restored | 41.08 Tgas |
| **Counted attempt** (`Blocked`) | Pairing check fails, agent frozen, or wallet balance too low | Yes | Counted | Unchanged | 36.08 Tgas for a failed pairing |
| **Revert** | Malformed proof point, out-of-date proof, transfer in flight, or breaker already tripped | No | Not counted | Unchanged | 35.55 Tgas for a malformed point; 1.99 Tgas for an out-of-date proof |

**Limit: malformed points escape the breaker.** A compromised agent can send proofs with invalid coordinates, and each one aborts inside the host function, so nothing is logged or counted. It gains nothing and moves no funds, and it pays gas from its key allowance each time. See SD-004.

**Limit: a low balance counts as an attempt.** An emptied wallet freezes its agent after 5 attempts, as `pay` already did. This is a false positive for the breaker, and finance must unfreeze the agent after refunding.

## 6. What each observer learns

**Option C's privacy covers the spending policy only. Vendor and transfer amount stay public.**

| Observer | v0.2 | Option A | Option C | Option D (not planned for 12 Nov) |
| --- | --- | --- | --- | --- |
| Chain observer | Vendor, amount, time, agent, policy, block reasons, nonce | Vendor, amount, time, agent, policy; purpose hidden | Vendor, amount, time, agent. Hidden: caps and vendors not yet paid. **Sum spent per day is computable from public transfers.** | Deposits and withdrawals; correlation still possible |
| Vendor | Its own payments, policy (public) | Its own payments | Its own payments | Its own payments |
| Facilitator | Every payment for its vendors | Same | Same | Same, unless the vendor verifies alone |
| Compromised agent | Everything public | Everything public | **The full private policy**, because it must hold the opening to prove. Integrity still holds. | Same, plus its own notes |
| Finance | Everything | Everything, plus decrypted log | Everything | Everything through a viewing key |

**Stated limits.**

- Option C does not hide vendor or amount. It hides only policy parameters and the allowlist from outside observers, not from the agent. Option A hides purpose, not payment. Neither offers unlinkability.
- **The policy still leaks through use.** Every paid vendor is visibly on the allowlist. Every payment is a lower bound on the per-call limit, and each day's total is a lower bound on the daily cap.
- **A forced attempt is public.** The rejected transaction's arguments show the vendor and amount that were tried. Only the rule it broke stays hidden, since the log says just `invalid proof`.
- **An honest refusal leaves no trace**, which is good for privacy and bad for audit: finance cannot see from the chain that an agent was prompted to pay someone off-policy.

## 7. Attack test matrix: current coverage

| Attack test | Status |
| --- | --- |
| Injected payment to unapproved address | Covered (T1) |
| Amount above per-call | Covered (T2) |
| Cumulative overspend | Covered (T3) |
| Fabricated new day | v0.2: not applicable (T4). Option C: covered, in the mocked VM and on the sandbox |
| Receipt submitted twice | Covered (T6) |
| Concurrent redemption | Covered (T7) |
| Forged `Paid` | Covered (T8) |
| Wrong code hash or token | Covered (T9, T10) |
| Wrong vendor, invoice, amount, expired nonce | Covered (T11) |
| Invalid proof or public-input order | Covered, including invalid coordinates in each proof point |
| Callback failure | Covered (T12), and under Option C it also restores the state |
| Policy update or freeze with an outstanding invoice | Covered for both `pay` and `pay_zk` |
| Unauthorized recovery | Covered: the agent cannot reset its state or replace the verifying key |
| Allowlist A with recipient B, reused nullifier, corrupt ciphertext | Option D only, not started |

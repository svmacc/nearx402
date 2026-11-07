# Recovering `reserved` after a lost callback

**Status: implemented locally, as `reconcile_payment`. Not deployed, and Codex has not reviewed it.** It answers an open item of the ZK review (`docs/review/status.md`). Steven chose to add it and then lock the contract ([SD-016](security-decisions.md#sd-016-finance-closes-a-payment-with-a-lost-callback-on-a-stated-claim)).

## The problem

A payment moves its amount from `balance` to `reserved`, then calls `ft_transfer`, then `on_pay_transfer` settles it (`contracts/policy/src/lib.rs:618-619`, `:697`, `:783`). If that callback itself fails, nothing settles the payment. The entry stays `Pending`, and nothing lowers `reserved`. `set_zk_agent` clears the ZK agent's in-flight marker (`:474`), but not `reserved`.

**How likely:** low. The callback has a static 15 Tgas against a measured 2 Tgas, and `pay_zk` requires 90 Tgas up front. No test makes a callback fail by itself. A protocol change in gas costs, or a bug in the callback, is what this guards against.

**Why it matters anyway:** once the contract is locked, code cannot be fixed.

| What happened to the transfer | Effect while the callback is lost |
| --- | --- |
| Tokens moved | `reserved` stays too high. Later plain deposits up to that amount cannot be credited by `sync_balance`, because it counts `balance + reserved` |
| Tokens did not move | The tokens sit in the wallet and cannot be spent, withdrawn or synced. If the contract is locked, this is permanent |
| Either, for a ZK agent | `pay_zk` refuses with "a payment is already in flight" until finance sends `set_zk_agent` |
| A withdrawal instead of a payment | Same as the first two rows, but no log entry identifies it |

The loss is bounded by the stuck amount. An attacker gains nothing from it.

## Why the contract cannot decide alone

A contract cannot read the outcome of an old receipt. Its token balance is not enough to infer it: a deposit in flight may already count at the token while `ft_on_transfer` has not run, plain transfers can land at any time, and other payments may be in flight. Anyone could also send tokens to the wallet to make a transfer look failed. So a human must read the result, and the contract must make a wrong claim harmless to solvency.

## The method

```rust
pub fn reconcile_payment(&mut self, id: u32, tokens_moved: bool) -> PromiseOrValue<PayOutcome>
```

Finance reads the token's `ft_transfer` event for memo `nearx402:{id}` in the explorer, then calls this. Rules for both cases:

- Owner only.
- The entry must be `Pending`.
- The entry must be at least 24 hours old, so a slow transfer cannot execute after the decision. The wait costs little for an event this rare. **The 24 hours is an assumption, since NEAR gives no hard bound on receipt delay.**
- Budgets never go back up. `spent_today` and the ZK state stay as they are, unlike a normal failure, because the outcome here is a claim and not a receipt result. Finance can still send `set_zk_agent`, which it can do today.
- The ZK marker clears only if its `log_id` equals `id`, the same check as `on_pay_transfer`.
- It bumps `settle_seq`, and emits `payment_reconciled` with the claim.

| Claim | Steps | If the claim is wrong |
| --- | --- | --- |
| `tokens_moved = true` | `reserved -= amount`. Status `Paid`, with a reason saying finance reconciled it | The tokens become a surplus, and `sync_balance` can credit them. Nothing is lost, but the log is wrong until the event is read |
| `tokens_moved = false` | Read `ft_balance_of`. In the callback, recheck `Pending` and `settle_seq`, and require `held >= balance + reserved`. Then `reserved -= amount`, `balance += amount`. Status `Failed`, with the same kind of reason | Normally the check refuses, since the tokens are gone. A surplus in the wallet can mask it, but then the tokens are held, and finance could have credited them by `sync_balance` anyway |

**Why this holds:** `balance + reserved` either falls or stays the same, so the method can never raise what the contract counts as held.

**A late callback:** `on_pay_transfer` already requires `Pending`, so a callback arriving after this method panics and changes nothing.

**What finance gains:** it can label a stuck entry older than 24 hours. It gains no new way to move tokens, since it already controls `withdraw`, `sync_balance` and `set_zk_agent`. The event makes each use public.

## What it does not cover

**Withdrawals.** They have no log entry, so nothing identifies one. Covering them needs a new field such as `pending_withdrawal`, and a new field changes the stored layout, so the contract would need a state migration. **Recommendation: leave this out, and document it.** Only finance can start a withdrawal, it chooses the amount, and the callback is the same size of risk.

## Deployment effect

A new method keeps the stored layout: no field of the contract's state changed. So the same account can be redeployed in place while it still has a key, and the verifying key, agent and log stay. **This has not been rehearsed against testnet's state.** A failed decode could be undone by deploying the old build again, as long as the contract is not locked.

The code hash changes to `2mqyriU2CdnhNHnzYQF4E1VG3EvNXsnrsMBzCz5z1jYH`, so `demo-zk` refuses to run against testnet until the redeploy, and the testnet demo must run again. The order is in [testnet.md](testnet.md).

## Tests

Written first, and seen to fail on the old code: the unit tests did not compile, and the sandbox test failed with `MethodNotFound`.

**Unit tests, in `contracts/policy/src`.** They simulate a lost callback by not calling `on_pay_transfer`:

| Test | Shows |
| --- | --- |
| Refuses a non-owner, an unknown id, and `Paid`, `Failed` and `Blocked` entries | The rules at the door |
| Refuses at 24 hours less 1 ns, accepts at exactly 24 hours | The age limit |
| `tokens_moved = true` | `reserved` falls, `balance` is unchanged, status `Paid`, budgets unchanged |
| `tokens_moved = false`, `held == balance + reserved` | Funds return to `balance`, status `Failed`, budgets unchanged |
| `tokens_moved = false`, `held` one unit short | Refused, nothing changes |
| `ft_balance_of` fails, or `settle_seq` changed during the read | Refused, nothing changes |
| The entry was settled during the read | Refused, so nothing is released twice |
| `on_pay_transfer` arrives late, for each claim | Panics, and `reserved` and `balance` do not change |
| ZK agent after each claim | It can pay again from the current state, which was not rolled back |
| Agent rotated, or a newer payment in flight | Its marker is left alone |
| Wrong `tokens_moved = true` | `sync_balance` then credits the tokens, and only once |
| `withdraw` after each claim | Limited to the new `balance` |
| Random calls: payments, deposits, withdrawals, settlements, lost callbacks and truthful claims, 8 seeds of 400 steps | After every step, `reserved` equals what is in flight or stuck, and `balance + reserved` equals the token balance plus what stuck payments have moved |

**Sandbox test, `crates/nearx402/tests/reserved_recovery.rs`.** It patches storage to leave the state a lost callback would leave, then runs the real contracts: a false claim refused by the real `ft_balance_of` read, a true claim of each kind, a settled entry, the age limit against real block time, and the private callback refused from outside.

**Mutants:** 25 in `scripts/mutation-audit.py`, all killed by the unit tests. They do not cover `#[private]` on the new callback, which only the sandbox test checks.

**Limits:** no callback was made to fail by itself, and the patched state stands in for one. The random test's claims are all truthful, because a false claim is covered by its own tests. Gas for `reconcile_payment` was not measured, beyond working with 100 Tgas attached. The new method has not run on testnet.

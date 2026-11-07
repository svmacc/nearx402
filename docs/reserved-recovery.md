# Recovering `reserved` after a lost callback

**Status: implemented locally, as `reconcile_payment`. Not deployed.** Codex challenged the first version, and this page records the result. Steven chose to add the method and then lock the contract ([SD-016](security-decisions.md#sd-016-finance-closes-a-payment-with-a-lost-callback-on-a-stated-claim)).

## The problem

A payment moves its amount from `balance` to `reserved`, then calls `ft_transfer`, then `on_pay_transfer` settles it. If that callback itself fails, nothing settles the payment. The entry stays `Pending`, and nothing lowers `reserved`. `set_zk_agent` clears the ZK agent's in-flight marker, but not `reserved`.

**How likely:** low. The callback has a static 15 Tgas against a measured 2 Tgas, and `pay_zk` requires 90 Tgas up front. No test makes a callback fail by itself. A protocol change in gas costs, or a bug in the callback, is what this guards against.

**Why it matters anyway:** once the contract is locked, code cannot be fixed.

| What happened to the transfer | Effect while the callback is lost |
| --- | --- |
| Tokens moved | `reserved` stays too high. Later plain deposits up to that amount cannot be credited by `sync_balance`, because it counts `balance + reserved` |
| Tokens did not move | The tokens sit in the wallet and cannot be spent, withdrawn or synced. If the contract is locked, this is permanent |
| Either, for a ZK agent | `pay_zk` refuses with "a payment is already in flight" until finance sends `set_zk_agent` |

## The method

```rust
pub fn reconcile_payment(&mut self, id: u32, tokens_moved: bool) -> PromiseOrValue<PayOutcome>
```

The contract enforces these:

- Owner only, on a `Pending` entry at least 24 hours old.
- Budgets never go back up. `spent_today` and the ZK state stay as they are, unlike a failure the token reported, because this outcome is a claim.
- The ZK marker clears only if its `log_id` equals `id`.
- It bumps `settle_seq`, emits `payment_reconciled`, and says in `reason` that the owner reconciled the entry.

| Claim | Steps |
| --- | --- |
| `tokens_moved = true` | `reserved -= amount`. Status `Paid` |
| `tokens_moved = false` | Read `ft_balance_of`. In the callback, recheck `Pending` and `settle_seq`, and require `held >= balance + reserved`. Then `reserved -= amount`, `balance += amount`. Status `Failed` |

A late `on_pay_transfer` panics, because the entry is no longer `Pending`, and changes nothing.

## Precondition, which the contract cannot check

**Before calling this, the operator must see every receipt of the payment's transaction finished.** An entry's age or a missing event proves nothing.

1. Find the transaction of log entry `id`, by its agent and time. A node that has pruned old transactions will not have it, so an archival RPC may be needed.
2. Call the `tx` RPC with `wait_until: "FINAL"`, and require `final_execution_status` to be `FINAL`. **A timeout means the level was not reached, not that the transfer failed. Stop, and try later.**
3. Find the `ft_transfer` receipt, whose executor is the token and whose memo is `nearx402:{id}`. `SuccessValue` means the tokens moved, and `Failure` means they did not.
4. Find the `on_pay_transfer` receipt. It must show `Failure`. If it succeeded the entry would not be `Pending`, so stop and investigate.
5. Call `reconcile_payment` with what step 3 showed.

If the transaction cannot be found or is not `FINAL`, do not call it. There is no deadline: the stuck amount stays safely reserved.

## Codex's counterexample

Claude's first version argued that a wrong claim was harmless because `balance + reserved` never rises. **That is not a proof of solvency**, as Codex showed. It says nothing about what the token really holds.

| Step | `balance` | `reserved` | Tokens held |
| --- | --- | --- | --- |
| Pay 25, transfer not yet executed | 975 | 25 | 1000 |
| 24 hours pass. Owner claims `false`, and the read returns 1000 | 975 | 25 | 1000 |
| The transfer executes after the read, and its callback has not run | 975 | 25 | 975 |
| The claim's callback accepts the read | 1000 | 0 | 975 |
| The transfer's callback arrives, and is refused as not pending | 1000 | 0 | 975 |

A `tokens_moved = true` claim has the same gap: if the transfer is still to come, `sync_balance` can credit the tokens before they leave. Two tests pin both schedules: `reconcile_cannot_see_a_transfer_still_in_flight_after_an_unmoved_claim` and `reconcile_cannot_see_a_transfer_still_in_flight_after_a_moved_claim_and_a_sync`.

**Is this reachable on NEAR? Not shown either way.** These tests are a model. They need a receipt to be delayed past another that was issued at least 24 hours later. Claude could not show a guarantee that rules it out. Pages read on 7 Nov 2026, with testnet at nearcore 2.14.0-rc.5, protocol 88:

| Source | What it says |
| --- | --- |
| [RPC transactions](https://docs.near.org/api/rpc/transactions) | `FINAL` is "Finalized + all receipts (including refunds) finalized". Nothing on ordering |
| [Receipts spec](https://nomicon.io/RuntimeSpec/Receipts) | Every action receipt executes eventually. A callback is postponed until its data arrives. "Runtime doesn't require that Receipts come in a particular order", said about matching receipts |
| [Receipt congestion](https://near.github.io/nearcore/architecture/how/receipt-congestion.html) | Receipts to a congested shard wait in a buffer of the sending shard. The order in that buffer is not described |

The nearcore source was not read. An order it happens to keep would still not be a published guarantee. So this design does not rely on order. **The precondition closes the gap instead:** once the transaction is `FINAL`, none of its receipts is still to come. It also matches what a lost callback really is, since a callback runs only after the transfer's result arrives.

## What a mistake costs

**Only the owner can call this, so only an owner's mistake can cause it.** Nobody else can trigger it, and it gives nobody a way to take tokens.

If the owner skips the precondition and a transfer really is still in flight, `balance` can exceed the tokens held by up to that payment's amount. The token still refuses to move more than it holds, so nothing overspends, but later payments can fail, including other agents'. The log also states something false. The owner can restore the backing by sending that amount with a plain `ft_transfer`, and not syncing it.

If the owner claims wrongly with the transaction `FINAL`:

| Wrong claim | Result |
| --- | --- |
| `true`, but the tokens did not move | The tokens become a surplus, and `sync_balance` can credit them. The log is wrong |
| `false`, but the tokens moved | The read normally refuses it. A surplus in the wallet can hide this, and then `balance` takes that surplus, which `sync_balance` could have credited anyway. The log is wrong |

The new power is labelling a stuck entry. The owner already controls `withdraw`, `sync_balance` and `set_zk_agent`, and each use here is public as an event.

## What it does not cover: withdrawals

**Steven should weigh this before locking.** A withdrawal has no log entry, so nothing identifies one. Covering it needs a new field, and so a state migration.

If the callback of a withdrawal is lost after locking, then **that amount stays in `reserved` for good**:

| What happened to the transfer | Consequence after locking |
| --- | --- |
| Tokens reached finance | Finance has its tokens. `sync_balance` stays short by that amount for later plain deposits |
| Tokens did not move | **Those tokens are stuck in the wallet permanently** |

The exposure is limited to one withdrawal's amount, and only finance can start one. Finance can reduce it by withdrawing smaller amounts. The callback has the same static 15 Tgas as a payment's. Recommendation: accept and document this, since the likelihood is low and a migration would add risk just before a permanent step.

## Deployment

No field of the contract's state changed. `scripts/upgrade-rehearsal.sh` rehearses the redeploy on a local sandbox: the build on testnet, the current build, then the testnet build again, each by a real deploy action with no init call. See the [review status](review/status.md) for results.

The current code hash is `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`. **Any edit that moves lines in the contract changes it, even a comment**, so check the hash of the commit being deployed. `demo-zk` refuses to run against testnet until the redeploy, and the testnet demo must run again. The order is in [testnet.md](testnet.md).

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
| Wrong `tokens_moved = true`, with the transaction `FINAL` | `sync_balance` then credits the tokens, and only once |
| `withdraw` after each claim | Limited to the new `balance` |
| A transfer still in flight after each claim | **Pins the known limit above** |
| Random calls: payments, deposits, withdrawals, settlements, lost callbacks and truthful claims, 8 seeds of 400 steps | After every step, `reserved` equals what is in flight or stuck, and `balance + reserved` equals the token balance plus what stuck payments have moved |

**Sandbox test, `crates/nearx402/tests/reserved_recovery.rs`.** It patches storage to leave the state a lost callback would leave, then runs the real contracts: a false claim refused by the real `ft_balance_of` read, a true claim of each kind, a settled entry, the age limit against real block time, and the private callback refused from outside.

**Mutants:** 25 in `scripts/mutation-audit.py`. They do not cover `#[private]` on the new callback, which only the sandbox test checks.

**Limits:** no callback was made to fail by itself, and the patched state stands in for one. The random test's claims are all truthful, because false claims have their own tests. Gas for `reconcile_payment` was not measured, beyond working with 100 Tgas attached. The new method has not run on testnet, and the runbook above has not been walked through on a real transaction.

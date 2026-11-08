# Retake runbook: one recorded testnet run

For Codex, the only operator of testnet transactions. Written by Claude on 8 Nov 2026 from the code and the 8 Nov evidence. It asks for nothing beyond what Steven asked for on 8 Nov: the redeploy if needed, tests, and a recorded demo. Claude sends no transaction and does not change `deploy/testnet.json`.

## What the failed run left

| Item | State, from the 8 Nov recording and Codex's report |
| --- | --- |
| Contract | `nearx402-policy.testnet` at code hash `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`, which the current source still builds. Not locked |
| Log | #3 is `Paid`, 0.05 mUSDC to `nearx402-data.testnet`, transaction `5txs2D6S8jXhguVRfiwBrZxXc2kvvM21gmgEGQsJVq1s`. It was never served. The facilitator runs inside `demo-zk`, so nothing remembers it as redeemed, and its 600 s claim window has passed. Leave it |
| Wallet | 9.90 mUSDC in the policy, 0.10 at the vendor, if nothing moved since. The run reconciled the wallet file after paying, so the file should agree with the chain. `demo-zk` checks this before spending, and catches up if it does not |
| Breaker | The run stopped before scene 2, so it added no blocked attempt. `demo-zk` reads `get_zk_agent` and refuses a run that would freeze the agent. Each run adds 2 of the day's 5, UTC |
| Agent's key | About 0.985 NEAR of allowance left, after four `pay_zk` calls at about 0.0038 each |
| Why it failed | Not known. The 402 body was discarded. A 429 from `rpc.testnet.near.org` fits what Codex saw afterwards, and is not shown |

## What changed in the demo since

- `3e047df`: a refused claim is read for its reason, printed bounded and redacted. The observer must be refused for the secret, and the replay as already redeemed, or the run says it does not show those checks. "Paid but not served" stops the run with the log id, the transaction, and the cause. Nothing retries.
- The commit after it: `demo-zk` prints the RPC by scheme and host only, so a key in the URL does not reach a recording. The paid-but-not-served path has now run end to end on a sandbox against a rate-limiting RPC. See [status.md](status.md).

## Before recording

1. **Both commits reviewed**, or at least read. The run uses the current source for the demo binary. The contract did not change, so the chain needs no redeploy.
2. **Build and hash.** `scripts/build-contracts.sh` must print `6BA4L1mo…` for the policy. `scripts/demo-zk.sh` builds and runs with the prover profile.
3. **Choose the RPC**, which is Codex's call. What one run asks of it, counted from the code and not measured:

   | Part | Reads | Transactions |
   | --- | --- | --- |
   | Scene 0 checks, plus the terms and the day | 11 | 0 |
   | Scene 1: balances, the payment read back, the wallet check | 5 | 1 |
   | Scene 1 claims, through the facilitator: observer 3, agent 8, replay 0 | 11 | 0 |
   | Scene 2: balances, two attempts read back, the agent view | 7 | 2 |
   | Scene 3: `get_agents`, `get_vendors`, `view_state` | 3 | 0 |

   About 37 reads and 3 transactions, not counting how near-api waits for each transaction to finish. near-api retries a failed read 5 times within about 0.3 s, so a 429 draws 5 more calls at once. The facilitator's reads and the demo's own go to the same URL. If the URL carries an API key, `deploy/testnet.json` must stay off screen; the demo and the facilitator's reasons no longer print it.
4. **Check `view_state` on the chosen RPC before spending.** Scene 3 reads the policy's whole storage with `view_state`, prefix empty, at `final`. `rpc.testnet.near.org` allowed it on 7 Nov. A provider that refuses or truncates it fails the run after the payment and both blocked attempts have been made. One read-only `query` with `request_type: view_state` on `nearx402-policy.testnet` settles it, and sends nothing.
5. **Read the breaker and the balance** with `get_zk_agent` and `get_balance`, or let `demo-zk` do it: it refuses before spending if `blocked_today + 2 >= max_blocked_per_day`, if the agent is frozen, or if the wallet holds less than 0.05 mUSDC.
6. **Terminal.** At least 160 columns, as the 8 Nov cast. Nothing that prints `deploy/testnet.json`, `.zx402/`, `wallet.json` or `~/.near-credentials`. `demo-zk` itself prints no private value.

## While it runs

What scene 1 prints when it works, from a sandbox run of the same build:

```text
observer       rebuilds the receipt from public data, without the agent's secret: HTTP 402, the secret does not match
HTTP 200       the vendor served: {...}
replay         the same receipt again: HTTP 402, already redeemed, a receipt buys one response
```

If it stops instead:

| Printed | Meaning | Then |
| --- | --- | --- |
| `paid but not served: paid on chain as log #N, transaction H, and the vendor refused the claim: HTTP 402, the facilitator's RPC is rate limiting (429): …` | The RPC refused the facilitator's read. The payment is on chain and stays unserved. Nothing was retried | Keep the recording. A second take costs another 0.05 mUSDC and leaves another unserved entry, and the breaker is untouched. If it fails the same way twice, stop and report, rather than run again |
| `… the facilitator could not read the chain: …` | Another RPC failure, named in the detail | Same |
| `observer       refused, but not for the secret: …` | The observer's 402 was for an RPC reason. The run then stops after HTTP 200 with `the observer was refused for another reason`, exit 1, because the take does not show the secret's protection | The payment was served. The take is not usable for the secret claim |
| `the circuit breaker is at X of Y today` | Refused before spending | Wait for midnight UTC, or ask Steven about `max_blocked_per_day` |

Do not open the wallet, do not run `zk-setup recover`, and do not claim log #3 by hand.

## After a good run

Send Claude, as on 7 Nov: the cast or log and the exit code, the three transaction hashes with their log ids and gas, the balances, and the breaker reading. Codex's own chain checks, `FINAL` and no failed receipt for each, stay the evidence of record. Claude then records them in `docs/testnet.md` and the README, and checks the narration in `docs/demo-script.md` against the printed numbers. Nothing of that is claimed before.

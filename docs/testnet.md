# Testnet plan

Status: 8 Nov 2026. **The demo has run twice on testnet with every check passing: on 7 Nov on the earlier build, and on 8 Nov on the current build, recorded.** The vendor and facilitator ran on a local machine talking to testnet. The 8 Nov redeploy, the recorded run and its verification were done by Codex, as the reviewer and the only operator of testnet transactions that day; Claude re-read the three transactions from the chain and recorded them here.

**The testnet contract is mutable, by Steven's choice on 8 Nov to delay the lock.** `nearx402-policy.testnet` keeps its full-access key. Whoever holds that key, the team, can deploy new code to the account, and new code can do anything the account can, including moving the tokens it holds. So on testnet, the spend policy holds against the agent, but trusting it also means trusting the key holder not to replace the code. The policy's own rules, such as owner-only methods, do not limit a code change. Locking is not a planned step. It would need a new, separate decision from Steven.

**The chain holds the current build.** On 8 Nov Codex redeployed `nearx402-policy.testnet` in place, with no init call, from `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU` to `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`, the build that adds `reconcile_payment` ([SD-016](security-decisions.md#sd-016-finance-closes-a-payment-with-a-lost-callback-on-a-stated-claim)). See [the 8 Nov section](#8-nov-2026-the-redeploy-and-the-recorded-run). `demo-zk` refuses to run against a chain that holds another build.

Read from the chain at final with view calls, on 7 Nov, before the redeploy:

| Item | State on testnet |
| --- | --- |
| Accounts | All six exist. The agent has one limited key, listed below. The other five each have one full-access key |
| `nearx402-policy.testnet` | Deployed and initialised, code hash `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`, the build of commit `32b3be4`, without `reconcile_payment`. Owner `nearx402-finance.testnet`, token `nearx402-musdc.testnet`. **Not locked** |
| `nearx402-musdc.testnet` | Deployed, code hash `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g`. Total supply 20.00 mUSDC |
| Wallet and log | Policy holds 9.95 mUSDC, finance 10.00, the vendor 0.05, the outside recipient 0. Log: one paid entry, two blocked |
| Private policy | Verifying key and ZK agent are on chain. Breaker 2 of 5 on 7 Nov UTC, not frozen. The values stay in `.zx402/testnet`, which git ignores |
| Agent's key | Only `ed25519:HNf59MA29Fb7N4adwkmWfDMqsaGP76avN9a3ydfnPnKY`, limited to `pay_zk` on the policy. Allowance 0.98873 of 1 NEAR left after three calls. The full-access key was deleted, which cannot be undone |

Demo transactions, signed by the limited key, each `FINAL` with no failed receipt:

| Step | Result | Gas | Transaction |
| --- | --- | --- | --- |
| Pay the vendor 0.05 mUSDC | Paid, log #0, one token transfer | 41.42 Tgas | `ErMc9bfwTJSTQXDtf1c7QM73vUHESQN8DR8KvSucFMw5` |
| Attempt A: prove with the attacker named | Blocked, `invalid proof`, log #1, no token transfer | 36.18 Tgas | `7Jq2vy4wy5qAdn86oWf7PThmi4mu4dbeaJKLSLvK6Nx7` |
| Attempt B: a valid proof sent to the attacker | Blocked, `invalid proof`, log #2, no token transfer | 36.21 Tgas | `4P8DhKWGm46FEvp2SdSR3uuNg8hxU63kVhH5CmFL3Cti` |

Setup transactions, each `FINAL` with no failed receipt:

| Step | Signer | Transaction |
| --- | --- | --- |
| Register policy | finance | `CgxMAsx4uR8LgNhxAuttgsyvVZYpniQ9SJevGKdbaUSo` |
| Register vendor | finance | `hkFGDR59wmj3HVEFF6usAGpJ5vg36fHNPFCyDRYxmXD` |
| Register outside recipient | finance | `7zQQauWmWGV96fM5JSXmQHvJgegpis7pQM6ASU29mXzL` |
| Mint 20 mUSDC to finance | finance | `EhAzotAne9QZHWvtgyeNcyV5zAYwRhiv8zB582Hqxbaa` |
| Deposit 10 mUSDC | finance | `EuKCUcgi7Aq3nQL1tvebs2kCQTgZbctrtR3yJeUXfYod` |
| `set_zk_verifying_key` | finance | `Fy85xJD94NMyrR5gSqWWmvtUu9tMAs5x2R1n3efetbr1` |
| `set_zk_agent` | finance | `2BcKKd88i2vcUJp5P7UBDrw2kzwnCzA4LKNUJ9BYAFig` |
| Add the limited key | agent | `41pFmNZPPd1aBsLDhmkiU2NnWrTfCoVuqWaMu2Dhs54o` |
| Delete the full-access key | agent | `99fRG77vFks3SakUPvwk3t9mqpZqw3Y6QwVx2Qe4gdFY` |

Claude did not make these accounts or this deploy. Claude sent the transactions above on Steven's approval. Pushing and any further transaction wait for Steven's approval.

## 8 Nov 2026: the redeploy and the recorded run

On 8 Nov Steven chose to **delay the lock** and asked for the redeploy, if needed, tests, and a recorded demo. Codex did all three as the only operator of testnet transactions, after its independent review of `bb706b7`, `3e047df`, `0ea1964` and `0f461f0`. Codex's report is [review/codex-2026-11-08-retake.md](review/codex-2026-11-08-retake.md); its `verification.json` and each transaction's JSON are in the recording folder outside the repository. Claude re-read the three demo transactions at `FINAL` through `https://test.rpc.fastnear.com` and recorded them here.

| Step | Result | Transaction |
| --- | --- | --- |
| Redeploy of the policy in place, no init call | Code hash `CXerHAq8…` to `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`. Codex compared every storage key and value, nine views, token balances and the agent's key before and after: identical. The policy's key stays, so the contract is still mutable | `AWpxaYA7xVnw8Y2WXhY1fkQD7Q6f3KNv3cwLayGaDCVU` |
| First take, through `rpc.testnet.near.org` | **Failed, exit 1 after 76.9 s.** The payment settled as log #3 and the vendor went from 0.05 to 0.10 mUSDC, but the observer's claim and the agent's own claim both got 402, and the demo of that build discarded the body. The cause is not known; Codex later saw that RPC answer 429, which fits a rate limit and proves nothing. Log #3 stays paid and unserved, its claim window has passed, and nothing retried it | `5txs2D6S8jXhguVRfiwBrZxXc2kvvM21gmgEGQsJVq1s` |
| Second take, through `https://test.rpc.fastnear.com`, build `0f461f0` | **Exit 0 in 23.1 s.** Every check passed. Below | Below |

The second take, signed by the agent's limited key `ed25519:HNf59MA29Fb7N4adwkmWfDMqsaGP76avN9a3ydfnPnKY`, each `FINAL` with no failed receipt:

| Step | Result | Gas | Transaction |
| --- | --- | --- | --- |
| Pay the vendor 0.05 mUSDC | `zk_proof_verified`, `payment_approved`, one `ft_transfer` of 50000 from the policy to the vendor with memo `nearx402:4`, `payment_paid`. Log #4 `Paid`. 6 receipts | 41.44 Tgas | `G46TzNiacRWn9JtxKr3ucLRfotLLM7FoGNeeeN5eHeL` |
| Attempt A: prove with the attacker named | `payment_blocked`, `invalid proof`, log #5, no token transfer. 2 receipts | 36.19 Tgas | `AWM9KTdm2V9hmPbZyk6fEzbjuv6Up5WRt1V1RdAgtbqV` |
| Attempt B: a valid proof sent to the attacker | `payment_blocked`, `invalid proof`, log #6, no token transfer. 2 receipts | 36.22 Tgas | `248x4Q8gaKTt8aUE36duxFy2FaBLrjBtSxTPxePqwRmS` |

From the run's own output, which no chain read can confirm: the observer's rebuilt receipt got HTTP 402 for the secret, the agent got HTTP 200 with the sample data, and the replay got HTTP 402 as already redeemed. The proof took 183 ms on the agent's machine.

State at Codex's verification block `DTn67YRqdvakLvE6kU8dXveYVo9LBqyW6mdZazdSJo3h`, and read again by Claude afterwards: policy 9.85 mUSDC, finance 10.00, vendor 0.15, outside recipient and agent 0. Log length 7. Breaker 2 of 5 on 8 Nov UTC, not frozen, nothing pending. The agent still has only its `pay_zk` key, with about 0.973 NEAR of allowance. The policy still has one full-access key.

The second take used a per-run configuration with the fastnear RPC; the project's `deploy/testnet.json` was not changed. The recording, its log and a silent terminal video are in `demo-2026-11-08/take-02/`, and the failed take stays in `demo-2026-11-08/`, both outside the repository. The build on the chain matches the current source, so no further redeploy is needed unless the contract changes.

**No lock step follows.** The policy's full-access key stays. Deleting it would be permanent, and would need its own decision from Steven, with the withdrawal limit in [reserved-recovery.md](reserved-recovery.md) in view.

The redeploy command Codex used, for the record:

```bash
near contract deploy nearx402-policy.testnet use-file res/policy.wasm without-init-call network-config testnet sign-with-legacy-keychain send
```

Do not pass an init call: it would fail on an initialised contract, and the state must stay.

Finance's setup and the agent's wallet are on one machine for this demo, so step 6's warning about who ran the setup is not met here.

What has been checked, and how:

| Part | How far it was checked |
| --- | --- |
| Steps 6, 8 and 9: `zk-setup` and `demo-zk --network testnet` | Run as separate processes against a **local sandbox** by `crates/nearx402/tests/zk_separate_runs.rs`, and twice on testnet |
| `near` commands in steps 5, 6 and 7 | **Executed on testnet** |
| `near` command in step 10 | Its subcommand was checked against `near` 0.31.1's `--help`. **Not executed.** Whether the deploy used the commands in steps 2 to 4 is not known |
| The redeploy without an init call | **Executed on testnet** by Codex on 8 Nov, after the sandbox rehearsal |
| The limited key | Signed all six demo transactions on testnet, and the failed take's payment |
| HTTP 200, the rebuilt receipt and the replay | Reported by the demo runs on the local vendor. A chain read cannot confirm these. The payments behind them are on chain |
| Testnet's RPC, gas and timing | Two runs, and one failed take. See the end of this page |

Testnet-only entries are eligible on every track (Q-010 in the official `HACKATHON.md`), and testnet NEAR is free (Q-008).

## Decisions and actions for Steven

| # | Decision | Recommendation | Why it is yours |
| --- | --- | --- | --- |
| 1 | Account names, and creating them | **Done**, with the names below | Accounts and their keys are yours |
| 2 | The private policy: cap, per-call limit, allowed vendors | **Values other than this page's examples**, since those are public and could be guessed. Include one allowed vendor that is not paid in the demo | It stays off chain, and must stay out of the repository |
| 3 | Push the work | Push once you have read the change | Outward-facing |
| 4 | Deploy | **Done before this decision was recorded.** Redeploy after any change to the contract, because `demo-zk` refuses to run if the chain holds another build | Outward-facing |
| 5 | Breaker limit while recording | 20, through `max_blocked_per_day` in step 6. The default of 5 allows 2 runs per UTC day | A higher limit gives a compromised agent more attempts |
| 6 | Locking the contract | **Decided on 8 Nov: delay it.** The testnet contract stays mutable, and the README says whoever holds the policy's key must be trusted | Permanent if done. A bug then means deploying to a new account |
| 7 | Recording or hosting | Record first, and host only if time remains. Hosting needs HTTPS on every hop, or the receipt secret travels in the clear (SD-014) | See "Still missing" |
| 8 | Making the repo public | Last, and by the deadline of 14 Nov 23:59 UTC. The rules require it | Outward-facing |
| 9 | The lost-response limit | Accept and document it (SD-015) | Product tradeoff |

| Role | Suggested account |
| --- | --- |
| Policy contract | `nearx402-policy.testnet` |
| Mock token | `nearx402-musdc.testnet` |
| Finance, owner | `nearx402-finance.testnet` |
| Agent | `nearx402-agent.testnet` |
| Vendor | `nearx402-data.testnet` |
| Outside-policy recipient for the demo | `nearx402-outside.testnet` |

## Before you deploy

The code hash of the policy changed with the rename, and any later change to the contract changes it again. Use the hash that step 1 prints, not one from an older document.

**This circuit has had no independent audit**, and Codex's review of it is still open. That does not block a testnet deploy with a mock token, but the README must keep saying so.

## Still missing

**A hosted vendor and facilitator.** `demo-zk --network testnet` runs them on your machine, talking to testnet. A local-only demo is allowed with a video (Q-012). Hosting is a plus, and must stay up until at least 18 Nov. Recommendation: record a run first, and host only if time remains.

## Steps

Run in order from the repository root. These use near-cli's legacy keychain in `~/.near-credentials`, because `demo-zk` reads the agent's key from a file there.

**1. Build.** Note the printed code hash of `res/policy.wasm`. The facilitator trusts the hash of the build you deploy, and `demo-zk` refuses to run if the chain holds anything else.

```bash
scripts/build-contracts.sh
```

**2. Create accounts**, one per role, with your chosen names. Repeat for each.

```bash
near account create-account sponsor-by-faucet-service nearx402-policy.testnet autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
```

**3. Deploy the mock token.** Its faucet is open to anyone, so it must never go to mainnet. `demo-zk` refuses to run against a chain whose id is `mainnet`.

```bash
near contract deploy nearx402-musdc.testnet use-file res/mock_usdc.wasm with-init-call new json-args '{}' prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-legacy-keychain send
```

**4. Deploy the policy contract.** It holds about 248 KB of code, which needs roughly 2.5 NEAR for storage at the protocol's storage price.

```bash
near contract deploy nearx402-policy.testnet use-file res/policy.wasm with-init-call new json-args '{"owner_id":"nearx402-finance.testnet","token_id":"nearx402-musdc.testnet"}' prepaid-gas '100 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-legacy-keychain send
```

**5. Fund the wallet.** The faucet registers an account with the token. Register the policy contract, the vendor and the outside recipient with an amount of 0, and mint to finance.

```bash
near contract call-function as-transaction nearx402-musdc.testnet faucet json-args '{"account_id":"nearx402-policy.testnet","amount":"0"}' prepaid-gas '30 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

```bash
near contract call-function as-transaction nearx402-musdc.testnet faucet json-args '{"account_id":"nearx402-data.testnet","amount":"0"}' prepaid-gas '30 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

```bash
near contract call-function as-transaction nearx402-musdc.testnet faucet json-args '{"account_id":"nearx402-outside.testnet","amount":"0"}' prepaid-gas '30 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

```bash
near contract call-function as-transaction nearx402-musdc.testnet faucet json-args '{"account_id":"nearx402-finance.testnet","amount":"20000000"}' prepaid-gas '30 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

Then send 10 mUSDC to the policy contract. A plain `ft_transfer` would need `sync_balance` afterward.

```bash
near contract call-function as-transaction nearx402-musdc.testnet ft_transfer_call json-args '{"receiver_id":"nearx402-policy.testnet","amount":"10000000","msg":""}' prepaid-gas '100 Tgas' attached-deposit '1 yoctoNEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

**6. Set up the private policy.** Finance runs this, **not on the agent's machine**: whoever ran the setup could have kept its randomness and could forge proofs. It signs nothing and sends nothing. Amounts are base units, so 1000000 is 1.00 mUSDC. A vendor on the allowlist needs no account until it is paid, and `demo-zk` needs one allowed vendor it does not pay, to show that an unpaid vendor stays hidden. These values are examples: use your own, and keep them out of the repository.

```bash
cargo run -q -p nearx402 --bin zk-setup --profile prover -- init --contract nearx402-policy.testnet --agent nearx402-agent.testnet --cap 1000000 --per-call 500000 --vendor nearx402-data.testnet --vendor nearx402-gpu.testnet --out .zx402/testnet
```

It writes these into `.zx402/testnet`, which git ignores:

| File | Holds | Goes to |
| --- | --- | --- |
| `finance/set_zk_verifying_key.json` | Public key material | The chain |
| `finance/set_zk_agent.json` | Commitments only | The chain |
| `agent/proving.key` | Not secret | The agent's machine |
| `agent/wallet.json` | **The private policy and salts**, readable by its owner only. Keep `agent/` writable by its owner only, and do not symlink the wallet | The agent's machine |

Send the two `finance` files. The tool prints these commands too.

```bash
near contract call-function as-transaction nearx402-policy.testnet set_zk_verifying_key file-args .zx402/testnet/finance/set_zk_verifying_key.json prepaid-gas '100 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

```bash
near contract call-function as-transaction nearx402-policy.testnet set_zk_agent file-args .zx402/testnet/finance/set_zk_agent.json prepaid-gas '100 Tgas' attached-deposit '0 NEAR' sign-as nearx402-finance.testnet network-config testnet sign-with-legacy-keychain send
```

**The circuit breaker limits retakes.** Each run makes 2 rejected attempts on purpose, and the default limit is 5 per UTC day, so 2 runs fit in a day, and `demo-zk` refuses a third instead of freezing the agent. For more takes while recording, add `"max_blocked_per_day": 20` to `finance/set_zk_agent.json` before sending it. Choose this deliberately: it also gives a compromised agent more attempts.

**7. Limit the agent's key.** Add a key that can only call `pay_zk` on the policy contract.

```bash
near account add-key nearx402-agent.testnet grant-function-call-access --allowance '1 NEAR' --contract-account-id nearx402-policy.testnet --function-names pay_zk autogenerate-new-keypair save-to-legacy-keychain network-config testnet sign-with-legacy-keychain send
```

near-cli 0.31.1 saves the new key as `~/.near-credentials/testnet/<agent>/ed25519_<public key>.json`, with an underscore after `ed25519`, and with mode 0644 inside a 0700 directory. Make it private:

```bash
chmod 600 ~/.near-credentials/testnet/nearx402-agent.testnet/ed25519_THE_LIMITED_KEY.json
```

Before deleting anything, check that the key's file is there and holds the public key on chain. Deleting the full-access key cannot be undone: the limited key cannot add a key, raise its allowance, or move the account's NEAR. If that file is lost or the allowance runs out, this account can no longer pay, and finance needs a new agent account and a new setup. The funds stay in the policy contract.

Then delete the agent's full-access key, naming its public key. `demo-zk` reads the agent's keys from the chain, and refuses to run while any key is not limited this way.

```bash
near account delete-keys nearx402-agent.testnet public-keys ed25519:THE_FULL_ACCESS_KEY network-config testnet sign-with-legacy-keychain send
```

**8. Configure the demo.** Copy the template, then set your names, the path of the limited key's file, and an RPC endpoint from https://docs.near.org/api/rpc/providers. The file holds names and paths, no keys, and git ignores it.

```bash
cp deploy/testnet.example.json deploy/testnet.json
```

**9. Run the demo.** Before spending anything, it checks the chain id, the deployed code hash, the token, the agent's keys, the verifying key against the proving key, the wallet file against the chain, the breaker, and the balance. It then tells the same story as the sandbox and never prints the policy. Add `--serve` for the dashboard.

```bash
scripts/demo-zk.sh --network testnet --config deploy/testnet.json
```

Record the real transaction hashes in the README.

**10. Locking: not planned** (decision 6). Steven chose on 8 Nov to keep the policy's key, so this step is not run. The demo prints whether the contract still has an access key. Only if Steven later decides to lock, and it is permanent, the command would be:

```text
near account delete-keys nearx402-policy.testnet public-keys ed25519:THE_CONTRACT_KEY network-config testnet sign-with-legacy-keychain send
```

## If the wallet and the chain disagree

The wallet records each payment's next state **before** sending it, and asks the chain which state won when it next starts. So a crash, a timeout, or a failed transfer needs no action.

If the file holds neither the chain's state nor a recorded one, for example after restoring an old backup, `demo-zk` refuses to prove. Funds are safe. Finance starts from a fresh state:

```bash
cargo run -q -p nearx402 --bin zk-setup --profile prover -- recover --dir .zx402/testnet
```

Then send `finance/set_zk_agent.json` as in step 6. The wallet adopts the new state on its next run. This **resets the day's spending**, and sets the breaker limit back to the default unless you add it to the file again.

## What the testnet runs showed

- Gas per payment: 41.42 Tgas on 7 Nov and 41.44 on 8 Nov, against 41.16 on the sandbox. A blocked attempt: 36.18 to 36.22 Tgas.
- Allowance: three calls used 0.01127 NEAR on 7 Nov, about 0.0038 each. Seven calls in all have used about 0.027 NEAR.
- `https://rpc.testnet.near.org` allowed `view_state` on the policy contract and one run's request rate on 7 Nov. On 8 Nov it refused the facilitator's claims in the first take, cause unrecorded, and answered 429 to reads afterwards. `https://test.rpc.fastnear.com` carried the second take, including `view_state`, in 23.1 s end to end.
- The facilitator's `FINAL` wait worked on testnet in both runs, and each transaction later read back as `FINAL`.
- A substring search of the 7 Nov transactions and their logs found no salt and no hidden vendor. Short integers cannot be shown absent this way, because their digits can appear by chance inside unrelated public numbers. The 8 Nov transactions were not searched this way.

## What must still be verified on testnet

- Time per payment: not measured separately. The 8 Nov run took 23.1 s for the whole story, with the proof in 183 ms. The sandbox's figure is 1.2 s per payment.
- The `delete-keys` command in step 10, which is not planned to run.
- The dashboard: **checked on 8 Nov** in a browser, served as static files with `?policy=nearx402-policy.testnet&rpc=https://test.rpc.fastnear.com&symbol=mUSDC`, and again with the default RPC. It showed 9.85 mUSDC, 0.10 paid that UTC day, 3 payments, 4 blocked attempts, the agent as `private policy` with 2 of 5 blocked, the paid vendor, and log #0 to #6. The transaction column stays empty there, because the hash list only exists in a demo run. The default RPC took a few seconds longer to answer. `demo-zk --serve` itself has not run against testnet. `dashboard/config.json` now holds those parameters, so the page needs no query string, and `.github/workflows/pages.yml` publishes `dashboard/` to GitHub Pages once the repository is public and Pages is switched to "GitHub Actions" in its settings.
- The explorer links: an automated browser got a bot check from the explorer, so they were not opened there. Open them in an ordinary browser before publishing.

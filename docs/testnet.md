# Testnet plan

Status: 7 Nov 2026. **The contracts are deployed on testnet, and nothing else has happened there.** Read from the chain with view calls:

| Item | State on testnet |
| --- | --- |
| Accounts | All six exist, each with one full-access key that matches its local key file |
| `nearx402-policy.testnet` | Deployed and initialised, code hash `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`, the same as the local build. Owner `nearx402-finance.testnet`, token `nearx402-musdc.testnet`. **Not locked** |
| `nearx402-musdc.testnet` | Deployed, code hash `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g`. Total supply 0 |
| Wallet and log | Balance 0, log empty, nothing registered with the token |
| Private policy | No verifying key and no ZK agent on chain, and no setup output on this machine |
| Agent's key | Still full-access, so `demo-zk` refuses to run |

Claude did not make these accounts or this deploy. Done so far: steps 1 to 4 below. **Still to do: steps 5 to 10.** Pushing and any further transaction wait for Steven's approval.

What has been checked, and how:

| Part | How far it was checked |
| --- | --- |
| Steps 6, 8 and 9: `zk-setup` and `demo-zk --network testnet` | Run as separate processes against a **local sandbox** by `crates/nearx402/tests/zk_separate_runs.rs` |
| `near` commands in steps 5, 7 and 10 | Their subcommands were checked against `near` 0.31.1's `--help`. **Claude has not seen them executed.** Whether the deploy used the commands in steps 2 to 4 is not known |
| Testnet's RPC, gas and timing | Not checked. See the end of this page |

Testnet-only entries are eligible on every track (Q-010 in the official `HACKATHON.md`), and testnet NEAR is free (Q-008).

## Decisions and actions for Steven

| # | Decision | Recommendation | Why it is yours |
| --- | --- | --- | --- |
| 1 | Account names, and creating them | **Done**, with the names below | Accounts and their keys are yours |
| 2 | The private policy: cap, per-call limit, allowed vendors | **Values other than this page's examples**, since those are public and could be guessed. Include one allowed vendor that is not paid in the demo | It stays off chain, and must stay out of the repository |
| 3 | Push the work | Push once you have read the change | Outward-facing |
| 4 | Deploy | **Done before this decision was recorded.** Redeploy after any change to the contract, because `demo-zk` refuses to run if the chain holds another build | Outward-facing |
| 5 | Breaker limit while recording | 20, through `max_blocked_per_day` in step 6. The default of 5 allows 2 runs per UTC day | A higher limit gives a compromised agent more attempts |
| 6 | Locking the contract | Lock it last, after a good run and the recording. The README's security model assumes a locked contract | Permanent. A bug then means deploying to a new account |
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
| `agent/wallet.json` | **The private policy and salts**, readable by its owner only | The agent's machine |

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

**10. Decide about locking** (decision 6). The demo prints whether the contract still has an access key. To lock it, delete that key.

```bash
near account delete-keys nearx402-policy.testnet public-keys ed25519:THE_CONTRACT_KEY network-config testnet sign-with-legacy-keychain send
```

## If the wallet and the chain disagree

The wallet records each payment's next state **before** sending it, and asks the chain which state won when it next starts. So a crash, a timeout, or a failed transfer needs no action.

If the file holds neither the chain's state nor a recorded one, for example after restoring an old backup, `demo-zk` refuses to prove. Funds are safe. Finance starts from a fresh state:

```bash
cargo run -q -p nearx402 --bin zk-setup --profile prover -- recover --dir .zx402/testnet
```

Then send `finance/set_zk_agent.json` as in step 6. The wallet adopts the new state on its next run. This **resets the day's spending**, and sets the breaker limit back to the default unless you add it to the file again.

## What must be re-verified on testnet

- Gas per payment: 41 Tgas on the sandbox.
- Time per payment: the sandbox's 1.2 s will be longer.
- That a transaction is final before the facilitator accepts it. Its `FINAL` wait has only run against a sandbox.
- That the RPC endpoint allows `view_state` on the policy contract, which scene 3 reads, and allows the demo's request rate.
- The `near` commands above, including where near-cli saves the limited key's file.
- The dashboard, using `?policy=<id>&rpc=<url>&symbol=mUSDC`. The transaction column stays empty there, because the hash list only exists in a demo run.

# Three-minute demo script

Draft for review. Nothing has been recorded. The narration is about 370 words, which fits three minutes with pauses. Every claim below comes from the testnet run in [testnet.md](testnet.md).

| Time | On screen | Narration |
| --- | --- | --- |
| 0:00–0:20 | Title: nearx402. Subtitle: testnet, mock token | Give an AI agent a wallet today, and you give it the keys to everything. If a prompt injection takes over that agent, the money is gone. nearx402 is a spend-policy wallet for agents that pay x402 APIs, settled on NEAR. |
| 0:20–0:45 | Simple diagram: agent, policy contract, vendor | The money sits in a NEAR contract, not with the agent. The agent's only key can call one method, `pay_zk`. Finance sets a daily cap, a per-call limit and a list of vendors. Those rules stay off chain. The agent proves it follows them, and the contract checks the proof before it pays. |
| 0:45–1:05 | Terminal, step 0: chain, code hash, agent key | This is a real run on NEAR testnet. Before spending anything, the demo reads the chain: the deployed code, and the agent's keys. There is one key, limited to `pay_zk`. |
| 1:05–1:35 | Step 1: HTTP 402, proof, transaction, HTTP 200. Then the explorer, showing the transaction | The agent asks for market data, and the vendor answers 402: pay 0.05 mock USDC. The agent builds a proof on its own machine in about 200 milliseconds. The contract verifies it and transfers to the vendor, for about 41 Tgas. Someone rebuilding the receipt from public data gets a 402. The agent gets a 200, once. |
| 1:35–2:15 | Step 2: attempts A and B, balances, breaker | Now the agent is compromised, and tries to pay an attacker. First it runs the prover anyway, naming the attacker: invalid proof. Then it takes a valid proof for the data vendor, and sends it to the attacker: invalid proof, because the proof is bound to the recipient. No tokens move, and the attacker stays at zero. Both attempts land in the on-chain log, and they count toward a circuit breaker that freezes the agent at its limit. |
| 2:15–2:40 | Step 3: `NOT IN STORAGE` and `STILL PUBLIC` lines | What can anyone read? The cap, the per-call limit, and allowed vendors not yet paid are absent from contract storage. But this hides the policy, not the payments. Vendor and amount stay public, and a compromised agent can read the policy, because it needs it to prove. It just cannot break it. |
| 2:40–3:00 | README status, then the command `scripts/demo-zk.sh` | This is a hackathon build with a mock token. It has had no third-party audit, and its trusted setup is local, so it is not ready for production. The vendor and facilitator ran on my machine, against testnet. You can run the whole flow in one command, with no accounts and no funds. |

## Before recording

| Check | Why |
| --- | --- |
| Steven approves a new testnet run | The verification run is done, and this one is for footage. It spends mock tokens and testNEAR |
| The run uses the current fixture | The first run's output shows the old sample body. Rebuild, and do not reuse that footage |
| Breaker budget for that UTC day | Each run makes 2 blocked attempts, so 2 runs fit in a day at the limit of 5. On 7 Nov, 2 attempts are already used. `demo-zk` refuses a run that would trip the breaker |
| Open the explorer link for the new transaction | The links have not been opened in a browser yet |
| Decide about the dashboard | `--serve` has not run against testnet. If it is added, test it first, or leave it out |
| Which contract is on chain | Record after the redeploy, since `demo-zk` refuses to run while the chain holds an older build. The order is in [testnet.md](testnet.md). Say whether the contract is locked at the time of recording |

## While recording

- Cut the waits for finality, and show that a cut happened. In the first run, about a minute passed between the payment and attempt A.
- Do not open `.zx402/`, `wallet.json` or `~/.near-credentials`.
- Do not run commands that print the wallet or the key files. `demo-zk` itself never prints the policy.
- Say the prices are sample data. The response labels them `synthetic sample, not live prices`.

## Do not say

- That payments are hidden, or that this is private payments.
- That the circuit is audited or proven sound. Four ZK audit items are open.
- That the agent cannot learn the policy.
- That the agent holds nothing. It holds no tokens, but its account holds testNEAR for gas.
- That this is safe for real funds. The setup ran on one machine, and the token is a mock with an open faucet.

The X post and the hackathon submission are Steven's to make.

# Three-minute demo script

Draft, 8 Nov 2026. Codex ran the recorded testnet demo as the only transaction operator. The first take on 8 Nov failed at the agent's claim after the payment; the second take the same day passed every check, exit 0 in 23 s ([testnet.md](testnet.md#8-nov-2026-the-redeploy-and-the-recorded-run)). Its cast, log and a silent terminal video are in `demo-2026-11-08/take-02/`, outside the repository. The narration below was checked against that take's output: 0.05 mUSDC, one key limited to `pay_zk`, 402 then 200 then 402, two blocked attempts, breaker 2 of 5. It is about 330 words, which fits three minutes with pauses. Any further take follows the [retake runbook](review/retake-runbook.md).

## Narration

| Time | On screen | Narration |
| --- | --- | --- |
| 0:00–0:20 | Title: nearx402. Subtitle: NEAR testnet, mock token | Give an AI agent a wallet, and a prompt injection can spend all of it. nearx402 is a spend-policy wallet for agents that pay x402 APIs, settled on NEAR. |
| 0:20–0:45 | Diagram: agent, policy contract, vendor | The money sits in a NEAR contract, not with the agent. The agent's only key can call one method, `pay_zk`. Finance sets a daily cap, a per-call limit and a list of vendors. Those rules stay off chain: the agent proves it follows them, and the contract checks the proof before it pays. |
| 0:45–1:05 | Terminal, step 0: chain, code hash, agent key | This is NEAR testnet, with a mock token that has no value. The vendor and facilitator run on my machine, talking to testnet. Before spending, the demo reads the deployed code and the agent's keys: one key, limited to `pay_zk`. |
| 1:05–1:35 | Step 1: HTTP 402, proof, transaction, HTTP 200, then the explorer | The vendor answers 402: pay 0.05 mock USDC. The agent proves the payment fits its policy, and the contract verifies the proof and transfers to the vendor. A receipt rebuilt from public data gets a 402. The agent gets a 200, once. |
| 1:35–2:15 | Step 2: attempts A and B, balances, breaker | Now the agent is compromised. It proves for an attacker: invalid proof. It sends a valid proof for the data vendor to the attacker: invalid proof, because the proof is bound to the recipient. No tokens move, both attempts are logged on chain, and they count toward a breaker that freezes the agent. |
| 2:15–2:40 | Step 3: `NOT IN STORAGE` and `STILL PUBLIC` lines | The cap, the per-call limit and unpaid vendors are not in contract storage. This is policy privacy, not payment privacy: vendor and amount are public, and the agent itself knows its policy. It just cannot break it. |
| 2:40–3:00 | README status | Limits, plainly. This contract is not locked: our team keeps its key, so you are trusting us not to replace the code. The circuit has no third-party audit, and its setup ran on one machine. It is a hackathon build, not for real funds. The whole flow also runs locally with one command. |

## Before recording

| Check | Why |
| --- | --- |
| The chain holds the build being shown | `demo-zk` refuses to run otherwise. Codex redeploys only if the code hash differs |
| Breaker budget for that UTC day | Each run makes 2 blocked attempts, and the default limit is 5. `demo-zk` refuses a run that would trip the breaker |
| The run uses the current fixture | The 7 Nov run's output shows the old sample body. Do not reuse that footage |
| The RPC can carry the run | About 40 reads and 3 transactions per run, with the facilitator's reads on the same URL. The 8 Nov run was refused at the claim, cause unknown. The runbook lists the reads and what each failure prints |
| Scene 1 says why each 402 happened | The observer's line must end `the secret does not match`, and the replay's `already redeemed`. Any other reason means the take does not show that check, and the run says so |
| Open the explorer link for each new transaction | The links have not been opened in a browser yet |
| Dashboard | `--serve` has not run against testnet. Test it first, or leave it out |

## While recording

- Cut the waits for finality, and show that a cut happened.
- Do not open `.zx402/`, `wallet.json` or `~/.near-credentials`, and do not run commands that print them. `demo-zk` itself never prints the policy.
- Say the prices are sample data. The response labels them `synthetic sample, not live prices`.

## Do not say

- That the contract is locked, immutable, or that nobody can change the rules. The team holds its key.
- That payments are hidden, or that this is private payments.
- That the circuit is audited or proven sound. Four ZK audit items are open.
- That the agent cannot learn the policy.
- That the agent holds nothing. It holds no tokens, but its account holds testNEAR for gas.
- That this is safe for real funds. The setup ran on one machine, and the token is a mock with an open faucet.

The X post and the hackathon submission are Steven's to make.

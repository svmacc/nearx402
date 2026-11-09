# Demo video storyboard

Draft, 8 Nov 2026, for Steven's review. Ten frames, three minutes. Every terminal frame quotes lines that are in the 8 Nov second take (`demo-2026-11-08/take-02/testnet-demo.log`, 23.1 s of real time, exit 0), so no new recording and no new testnet transaction is needed. The narration is the one in [demo-script.md](demo-script.md), reworded so that each line names the role on screen. The "Do not say" list there still applies.

## The cast

The x402 roles, with the one nearx402 adds, and the name each one has in the recording.

| Role | x402 name | In the recording | What it does |
| --- | --- | --- | --- |
| Finance | (added by nearx402) | `nearx402-finance.testnet`, `wallet` | Deposits into the policy contract and sets the policy: daily cap, per-call limit, allowed vendors. The policy stays off chain |
| Agent | buyer, client | `nearx402-agent.testnet`, `agent` | Holds one key that can only call `pay_zk`. Holds no tokens. Proves each payment fits the policy |
| Vendor | seller, resource server | `nearx402-data.testnet`, `vendor`, `data-api` | Answers 402 with a price, serves 200 once per receipt |
| Facilitator | facilitator | a local process, `/verify` and `/settle` | Reads the payment back from the chain before the vendor serves. Runs on the same machine as the vendor in this demo |
| Policy contract | settlement | `nearx402-policy.testnet` | Verifies the Groth16 proof, then transfers the token |
| Attacker | (none) | `observer`, `nearx402-outside.testnet` | Tries to claim the agent's receipt from public data, and to redirect a payment |

## Frames

Timings add up to 3:00. Terminal frames are stills or slow playback of the take-02 footage with a role badge overlaid next to the line being narrated. The footage itself is not edited.

### Frame 1, 0:00–0:15, title card

**On screen.** Dark card. `nearx402`. Below: `A spend-policy wallet for AI agents that pay x402 APIs, settled on NEAR`. Footer: `NEAR testnet, mock token, built at NEAR HACKATHON 2026`.

**Narration.** Give an AI agent a wallet, and one prompt injection can spend all of it. nearx402 puts the money in a NEAR contract that only pays when the agent proves the payment follows a policy it cannot change.

### Frame 2, 0:15–0:40, cast card

**On screen.** The cast table above as a diagram. Left: Finance, with an arrow "deposit, set policy" into the policy contract in the middle. Below the contract: Agent, with one arrow labelled `pay_zk` into the contract. Right: Vendor, with the Facilitator beside it, both on a box labelled "x402 server". A dotted line from the contract to the Facilitator labelled "reads the payment". Each box carries its x402 name in smaller text: buyer, seller, facilitator.

**Narration.** The roles are the x402 ones. The seller is a data API, the buyer is an agent, and a facilitator checks each payment before the seller serves. nearx402 adds a fourth: finance. Finance funds the contract and sets a daily cap, a per-call limit and a list of vendors. Those rules never go on chain. The agent proves it follows them, and the contract checks the proof before it pays.

### Frame 3, 0:40–0:55, flow card

**On screen.** Six numbered steps on one line: 1 vendor answers 402. 2 agent builds a proof. 3 contract verifies and pays. 4 agent claims with its receipt. 5 facilitator reads the chain. 6 vendor serves 200, once.

**Narration.** One purchase in six steps. Keep an eye on the numbers: every line in the terminal maps to one of them.

### Frame 4, 0:55–1:15, terminal scene 0

**On screen.** Lines from `[0] Checking the deployment before spending anything`:

```
chain          testnet, through https://test.rpc.fastnear.com
contract       nearx402-policy.testnet runs code 6BA4L1mo..., paying in nearx402-musdc.testnet
lock           the contract still has an access key, so its owner can change its code
agent key      nearx402-agent.testnet: read from the chain, 1 key(s), each limited to nearx402-policy.testnet.pay_zk
wallet         9.90 mUSDC, with the policy itself held off chain and not printed
```

Badges: `AGENT` on the key line, `FINANCE` on the wallet line.

**Narration.** This is NEAR testnet, with a mock token that has no value. The vendor and facilitator run on my machine. Before spending, the demo reads the deployed code and the agent's keys: one key, limited to one method. The wallet holds the policy off chain, and the demo never prints it. Note the lock line: the contract is not locked, and the demo says so.

### Frame 5, 1:15–1:40, terminal scene 1, the payment

**On screen.** Lines from `[1] The agent buys market data for 0.05 mUSDC`:

```
HTTP 402       GET /data-api/market-data: pay 0.05 mUSDC to nearx402-data.testnet
proof          built on the agent's machine in 183 ms, 256 bytes
transaction    G46TzNiacRWn9JtxKr3ucLRfotLLM7FoGNeeeN5eHeL
on chain       nearx402-policy.testnet: zk_proof_verified: the Groth16 check passed
on chain       nearx402-musdc.testnet: NEP-141 ft_transfer: nearx402-policy.testnet -> nearx402-data.testnet, 0.05 mUSDC
PROOF VERIFIED the contract checked the proof, then paid: log #4
vendor         nearx402-data.testnet: 0.10 mUSDC -> 0.15 mUSDC
```

Badges: `SELLER` on the 402 line, `BUYER` on the proof line, `CONTRACT` on the on-chain lines. Step numbers 1, 2, 3 at the left edge.

**Narration.** Step one: the seller answers 402, pay 0.05 mock USDC. Step two: the buyer proves on its own machine that this payment fits the policy, in under a second. Step three: the contract verifies the proof on chain and transfers to the seller. The seller's balance moves. Nothing in this transaction says what the cap or the limit is.

### Frame 6, 1:40–2:00, terminal scene 1, the claim

**On screen.**

```
observer       rebuilds the receipt from public data, without the agent's secret: HTTP 402, the secret does not match
HTTP 200       the vendor served: {"BTC":98250.0,"NEAR":5.12,"note":"synthetic sample, not live prices","source":"data-api"}
replay         the same receipt again: HTTP 402, already redeemed, a receipt buys one response
```

Badges: `ATTACKER` on the observer line, `FACILITATOR` and then `SELLER` on the 200 line. Steps 4, 5, 6. Optional 3-second cutaway to the explorer page for `G46Tz…` showing the `ft_transfer`, opened in an ordinary browser beforehand.

**Narration.** Everything about the payment is public, so an observer rebuilds the receipt and claims first: 402, the secret does not match. The buyer claims with its secret. The facilitator reads the payment back from the chain and the seller serves, once. The prices are sample data. The same receipt again: 402, already redeemed.

### Frame 7, 2:00–2:30, terminal scene 2

**On screen.** Lines from `[2] A compromised agent tries to redirect a payment outside the policy`:

```
honest wallet  refuses to prove: vendor is not on the private allowlist. Nothing is sent

Attempt A: run the prover anyway, naming the attacker
on chain       nearx402-policy.testnet: payment_blocked: invalid proof

Attempt B: take a valid proof for data-api, send it to the attacker
on chain       nearx402-policy.testnet: payment_blocked: invalid proof

REJECTED       the contract refused both: invalid proof. No token transfer in either transaction
after          nearx402-outside.testnet: 0.00 mUSDC -> 0.00 mUSDC. Wallet: 9.85 mUSDC -> 9.85 mUSDC
breaker        2 of 5 blocked attempts today; at the limit the agent freezes
```

Badges: `COMPROMISED BUYER` on the attempt headers, `ATTACKER` on `nearx402-outside.testnet`.

**Narration.** Now the buyer is compromised. The honest wallet refuses to prove for a vendor that is not on the list, so the attacker runs the prover by hand. Attempt A: a proof naming the attacker. Invalid. Attempt B: a valid proof for the data vendor, sent to the attacker. Invalid, because the proof is bound to the recipient. No tokens moved, both attempts are on chain, and they count toward a breaker that freezes the agent.

### Frame 8, 2:30–2:45, terminal scene 3

**On screen.** Lines from `[3] What anyone can read on chain`:

```
views          get_agents() = [], get_vendors() = []
NOT IN STORAGE allowed vendors not yet paid: absent from all 2573 bytes of contract storage
NOT IN STORAGE the daily cap: absent from all 2573 bytes of contract storage
NOT IN STORAGE the per-call limit: absent from all 2573 bytes of contract storage
STILL PUBLIC   nearx402-data.testnet and the amount paid: every token transfer shows them
agent          the agent must hold the policy to prove it. A compromised agent reads it, but cannot break it
```

Badge: `FINANCE'S POLICY` across the three `NOT IN STORAGE` lines. Optional 3-second cutaway to the dashboard, `dashboard/index.html` against testnet, showing the balance and log #4 to #6.

**Narration.** What finance set is not in contract storage: not the cap, not the limit, not the unpaid vendors. What is public stays public: the vendor and the amount of every payment. This is policy privacy, not payment privacy. The agent knows its own policy. It just cannot break it.

### Frame 9, 2:45–2:57, result and limits card

**On screen.** The last line of the recording:

```
Result: every check passed, read back from chain testnet.
    paid 0.05 mUSDC to nearx402-data.testnet; nearx402-outside.testnet received 0.00 mUSDC; wallet 9.90 mUSDC -> 9.85 mUSDC
```

Below it, a card: `Not locked: the team holds the contract key`. `No third-party audit of the circuit; setup ran on one machine`. `Mock token, testnet only`. `The whole flow runs locally with one command`.

**Narration.** Limits, plainly. The contract is not locked, so you are trusting us not to replace the code. The circuit has no third-party audit. It is a hackathon build, not for real funds.

### Frame 10, 2:57–3:00, end card

**On screen.** `nearx402`, the repository URL, `Built at NEAR HACKATHON 2026`, `@NEARProtocol`.

**Narration.** None, or "The whole flow also runs locally with one command."

## Production notes

- Footage: `take-02/testnet-demo.avi` is silent and already shows the full run. A larger-font render from `testnet-demo.cast` (asciinema `agg`) would read better at 1080p. Either way, the lines are the same.
- The run takes 23 s but the terminal frames cover about 1:55, so each scene is held or slowed. Hold on stills; do not speed the footage up, and do not cut lines out of a scene.
- Badges and step numbers are overlays added in the editor, not changes to the output.
- Cutaways to the explorer and the dashboard are optional. Open the explorer pages in an ordinary browser before recording; the links are in the log.
- Nothing in these frames shows `.zx402/`, `wallet.json`, `~/.near-credentials` or a private policy value. Keep it that way in the cutaways too.
- Everything the narration claims is in the log or in README's status table. Do not add a claim that is not.
- The narration above runs long when read slowly. The trimmed version, about 400 words in ten text files, is in `voiceover-2026-11-08/narration/` outside the repository, next to the synthetic audio auditions.

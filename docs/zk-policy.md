# Option C: ZK proof of policy compliance

Status: 7 Nov 2026. Works end to end on a **local NEAR sandbox**, through `scripts/demo-zk.sh` and the dashboard. Not on testnet, and not audited.

## What it does, and does not

An agent pays a vendor and proves, in zero knowledge, that the payment obeys finance's private limits. The contract holds only commitments to those limits.

**Privacy covers the spending policy only: caps, vendors not yet paid, and which rule a rejected attempt broke. Vendor and transfer amount stay public in the token transfer.** The agent must hold the full policy to prove, so a compromised agent learns it, although it still cannot break it. Full limits are in [threat-model.md](threat-model.md#6-what-each-observer-learns).

The setup is local and not a ceremony (SD-007).

## Reproduce

```bash
./scripts/build-contracts.sh
```

```bash
cargo test --workspace
```

```bash
cargo test -p nearx402 --test zk_policy --profile prover -- --nocapture
```

```bash
cargo run -p zx402-zk --bin zk-bench --profile prover -- policy
```

```bash
python3 -I scripts/mutation-audit.py
```

## Gas

Sandbox, `pay_zk` with 100 Tgas attached. The log prints `41.01 Tgas (41006297832181 gas)`, and 1 Tgas is 10^12 gas. This is the whole transaction, not only the contract call: the test asserts that the total equals the sum of every step below.

| Step | Tgas |
| --- | --- |
| Transaction conversion, agent | 0.341 |
| `pay_zk` on the policy contract, including verification | 36.282 |
| `ft_transfer` on the token | 1.701 |
| Callback on the policy contract | 2.013 |
| Three gas refunds, agent | 0.669 |
| **Total** | **41.01** |

Measured before the `zk_proof_verified` event and the receipt secret's longer memo were added. With both, `scripts/demo-zk.sh` reports 41.15 Tgas for a paid transaction.

## How a call ends, and what it costs

Only `Blocked` feeds the circuit breaker. Details are in [threat-model.md](threat-model.md#how-a-pay_zk-call-can-end).

| Result | Example | Tgas | Log | Breaker | Funds and state |
| --- | --- | --- | --- | --- | --- |
| Paid | Honest payment | 41.01 | Yes | No | Moved and advanced |
| Failed | Token refuses the transfer | 41.08 | Yes | No | Restored |
| Blocked | Failed pairing, including an all-zero proof | 36.08 | Yes | **Yes** | Unchanged |
| Reverted | Invalid coordinate | 35.55 | No | No | Unchanged |
| Reverted | Out-of-date proof, or breaker already tripped | 1.99 | No | No | Unchanged |

**Proofs with invalid coordinates.** All are 256 bytes and were sent on the sandbox: A.x above the field modulus, A off curve, B.x.c0 above the field modulus, B off curve, B on the curve but outside the subgroup, C.x above the field modulus, C off curve. Each one reverts with an `AltBn128` error. The test compares balance, state commitment, breaker count, log length, frozen flag, and in-flight marker before and after, and all are unchanged.

**Consequence.** A compromised agent can repeat this without tripping the breaker or writing a log entry. At the minimum gas price (0.0001 NEAR per Tgas, from protocol documentation, not measured here), 35.55 Tgas is about 0.0036 NEAR, so a 1 NEAR key allowance allows roughly 280 attempts. This is an estimate. No funds can move this way. See SD-004.

## Time

Measured separately, on an AMD Ryzen 5 8600G under WSL, single-threaded, in the `prover` profile.

**Proving, no network.** 6,763 constraints, allowlist full at 256 vendors, 21 runs.

| Step | Median | Worst |
| --- | --- | --- |
| Setup (once) | 164 ms | n/a |
| Build statement: hashes and allowlist tree | 40.0 ms | 41.6 ms |
| Prove | 210.8 ms | 276.0 ms |
| Native verify | 2.4 ms | 2.8 ms |

**End to end, against the local sandbox.** 5 runs, allowlist of 2 vendors.

| Step | Median | Worst |
| --- | --- | --- |
| `GET` returns 402 | under 1 ms | under 1 ms |
| Statement and proof | 208 ms | 221 ms |
| `pay_zk` transaction | 939 ms | 980 ms |
| Retry through facilitator, returns 200 | 49 ms | 54 ms |
| **Whole flow** | **1,247 ms** | **1,250 ms** |

The whole flow also includes the test's own balance and state reads between steps. **The transaction time is a local sandbox's. Testnet will be slower and is not measured.**

## Mutation audit

`scripts/mutation-audit.py` breaks one check at a time and confirms a test fails. It now also covers the wallet file, the demo's deployment checks, the receipt secret, and offer selection. **On 7 Nov: 90 mutants, 87 killed, 3 equivalent, 0 survived, 0 timed out.**

**Caveat.** These were two runs, not one. A full run of 86 had 1 mutant that failed to compile, and a targeted run of 12 covered it, 4 new mutants, and 7 others against stronger tests. Only tests changed between them. Details are in [the review status](review/status.md#latest-evidence-7-nov-2026-after-the-rename-to-nearx402).

**What the first circuit audit's survivors taught us:**

| Mutant | First-pass cause | Resolution |
| --- | --- | --- |
| `range: old_day` | **Weak test**: it failed on the wrong constraint | Test fixed; killed |
| `context ignores the contract` | **Weak test**: the mutant's value equalled the test's contract id | New test at another address; killed |
| `memo length unchecked` | **No test** | Two tests added; killed |
| `balance not checked` | Script pattern did not match | Pattern fixed; killed |
| `agent's day as input` | Equivalent: the check above it makes the values equal | Removing both is killed |
| `context unbound`, `invoice unbound` | Equivalent: arkworks binds all public inputs itself | Kept as defence in depth, 2 constraints |

**The evidence for the last row:** `real_proof_binds_every_public_input` changes each input in turn and requires verification to fail, and it still passes with those constraints removed. Please challenge this one, since it rests on a library internal.

A mutation audit shows that tests depend on the checks that exist. It cannot find a check nobody wrote, so it does not prove the circuit is sound.

## Known limits

- **Not on testnet.** All evidence comes from a local sandbox and the mocked VM.
- **Circuit soundness has no independent audit.** The author's answers to the six open questions are in [the review status](review/status.md), for Codex to challenge.
- **Poseidon's matrix has not been through the reference script's invariant-subspace checks.** arkworks does not run them, and the round constants have not been compared with an external vector.
- **Malformed proofs escape the breaker** (SD-004).
- **One payment at a time per agent** (SD-005).
- **The prover needs the proving key and the private policy on the agent's machine.**
- **The wallet file holds the private policy and salts in plain text**, readable by its owner only, in a directory only its owner can write to (SD-013). Losing it means finance must set a new policy through `set_zk_agent`, unless finance kept a copy. A stale copy recovers with `zk-setup recover`, which also resets the day's spending.

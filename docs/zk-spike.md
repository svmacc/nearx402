# Groth16 verifier spike

Date: 6 Nov 2026. **Result: the gate passes.** A NEAR contract can verify Groth16 proofs from arkworks using only `alt_bn128_*` host functions, and it costs 35.26 Tgas per verification. This covers the verifier only. The policy circuit is not written yet, and nothing has run on testnet.

## What was built

| Path | Contents |
| --- | --- |
| `contracts/zk-spike` | Verifier contract. Does no curve arithmetic of its own |
| `crates/zx402-zk` | Stand-in circuit, prover, NEAR byte encoding |
| `crates/zx402-zk/tests/near_verifier.rs` | Real proofs through a local NEAR sandbox |
| `crates/zx402-zk/src/bin/zk-bench.rs` | Proving benchmarks, test vector generator |
| `crates/zx402-zk/vectors/spike-groth16.json` | Reproducible vectors, checked by the sandbox test |

The circuit is a stand-in. It proves knowledge of a secret whose Poseidon hash with the public inputs gives a public output, and **it enforces no spending rule**. It has 8 public inputs, the number the Option C statement plans to use.

The keys come from public seeds and **offer no security**.

## Reproduce

```bash
./scripts/build-contracts.sh
```

```bash
cargo test -p zx402-zk --test near_verifier --profile prover -- --nocapture
```

```bash
cargo run -p zx402-zk --bin zk-bench --profile prover -- bench
```

## On-chain gas

Measured on the local sandbox as total gas burned by the transaction: verifying key read from storage, proof check, and one counter written back.

| Case | Tgas |
| --- | --- |
| Valid proof, 8 public inputs | **35.26** |
| Share of the 300 Tgas transaction limit | 11.8% |
| Rejected before pairing (bad shape, non-canonical input) | 1.69 |

At NEAR's minimum gas price (0.0001 NEAR per Tgas, taken from protocol documentation rather than measured here) that is about 0.0035 NEAR per verification, before the token transfer and callback. **Compare that against a $0.05 payment before calling this economical.** The fee in USD depends on the NEAR price at the time.

## Proving time

AMD Ryzen 5 8600G, WSL, one thread (arkworks built without its `parallel` feature), 7 runs each. Proving time includes witness generation.

| Poseidon rounds | Constraints | Setup ms | Prove ms, median | Prove ms, worst | Native verify ms |
| --- | --- | --- | --- | --- | --- |
| 1 | 970 | 36 | 42.6 | 43.2 | 2.6 |
| 8 | 2,629 | 61 | 103.4 | 114.7 | 2.5 |
| 32 | 8,317 | 160 | 188.9 | 252.3 | 2.5 |
| 128 | 31,069 | 499 | 508.4 | 789.4 | 2.5 |

One Poseidon permutation costs about 237 constraints. **Estimate, not a measurement:** the policy circuit (three commitments, an allowlist path of depth 8 to 16, 128-bit range checks) should land around 5,000 to 10,000 constraints, which on this machine means well under one second. A small cloud VM will be slower. This has to be re-measured with the real circuit.

## Negative cases

All run through the real host functions in the sandbox. The counter of accepted proofs confirms that none of them counted.

| Case | Result | Reason |
| --- | --- | --- |
| Public inputs swapped | `false` | Pairing fails |
| Public input changed by one | `false` | Pairing fails |
| Proof paired with another statement's inputs | `false` | Pairing fails |
| A replaced by 2A (a valid curve point) | `false` | Pairing fails |
| A taken from another valid proof | `false` | Pairing fails |
| Proof from a different setup | `false` | Pairing fails |
| All-zero proof | `false` | Pairing fails |
| Byte flipped in A | Call fails | `AltBn128 invalid input: invalid g1` |
| Byte flipped in B | Call fails | `AltBn128 invalid input: invalid g2` |
| B on the curve but outside the subgroup | Call fails | `AltBn128 invalid input: invalid g2` |
| Proof too short | Call fails | `proof must be 256 bytes` |
| One public input missing | Call fails | `wrong number of public inputs` |
| Public input + modulus | Call fails | `public input is not canonical` |

## What the spike taught us

1. **The byte format works as first written.** Each field element is 32 bytes little-endian, and G2 is `x.c0, x.c1, y.c0, y.c1`. This answers assumption 2 in the brief.
2. **G2 subgroup checking is done by the host.** The contract does not need its own check. The test will catch it if a protocol upgrade ever changes this.
3. **Proofs are malleable, as expected for Groth16.** The test turns a valid proof into a different valid proof without the witness, and it still verifies. **Replay protection must never rely on proof bytes being unique.** Option C prevents replay through `old_state`, which changes on every payment.
4. **Host errors cannot be caught.** A malformed point aborts the whole call, so it leaves no log entry and is not counted by the breaker, while a failed pairing returns `false` and can be logged. SD-004 accepts this.
5. **The contract's own canonical check matters.** Without it, `x` and `x + r` could be two encodings of one input. Its test rejects the alias at 1.69 Tgas, before any pairing.
6. **With `cargo near build`, panic messages reach clients.** The "unreachable" trap seen earlier applies to the plain `cargo build` wasm only. This closes that open question for builds through `scripts/build-contracts.sh` when cargo-near is installed.

## Gate decision

| Criterion (handoff section 11) | Result |
| --- | --- |
| Valid and invalid proofs are told apart in the sandbox | **Pass** |
| Gas fits the limit | **Pass**, 35.26 of 300 Tgas |
| Serialization is solved | **Pass** |
| Proving time is usable | **Pass for the stand-in circuit**; the real one is still to be measured |

Next is the Option C policy circuit, with its own kill criterion: if it remains unsound or integration fails by 9 Nov, the submission locks to Option A (SD-001).

## Not verified

- Behaviour on testnet. Gas may differ with the protocol version.
- The circuit's soundness for real spending rules, since none are enforced here.
- Proving on the hardware an agent would actually run on.
- `pay_zk` total gas, which adds the token transfer and callback.

**Later finding on code hashes (7 Nov).** The cargo-near build gave the same code hash from two different checkout paths, for all three contracts, on this machine. Plain and cargo-near builds differ from each other, so whichever build gets deployed, the facilitator must trust that build's hash. This is not a reproducible build across machines, which needs Docker.

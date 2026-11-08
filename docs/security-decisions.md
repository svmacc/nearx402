# Security decisions

Each entry records what we chose, why, and which test enforces it. **Accepted** means the team has agreed. **Proposed** means it is the working assumption until someone on the team approves or changes it. **Open** means work that depends on it must not start.

| ID | Decision | Status |
| --- | --- | --- |
| SD-001 | Scope for 12 Nov is A + C; D is a stretch goal behind a gate | Accepted |
| SD-002 | The contract derives or checks every proof input | Accepted |
| SD-003 | One proof statement binds vendor, amount, invoice, and state | Accepted |
| SD-004 | Invalid proofs are logged and count toward the breaker | Proposed |
| SD-005 | One in-flight transfer per agent under Option C | Proposed |
| SD-006 | Redemption store stays single-instance for the hackathon | Proposed |
| SD-007 | Local trusted setup, disclosed as non-production | Proposed |
| SD-008 | Replace the hand-written HMAC with the RustCrypto `hmac` crate | Accepted, awaiting dependency approval |
| SD-009 | Original implementation; foundations only from public libraries | Accepted |
| SD-010 | Withdrawal and recovery roles for Option D | Open |
| SD-011 | Privacy track eligibility | Checked 6 Nov: eligible, not disqualified |
| SD-012 | Verifying key lives in contract storage | Proposed |
| SD-013 | The agent's wallet file is plain text, and the chain decides its state | Proposed |
| SD-014 | A payment's claim is bound by a secret committed in the memo | Proposed |
| SD-015 | A paying client checks an offer before signing, and follows no redirects | Proposed; lost-response recovery is open |
| SD-016 | Finance closes a payment with a lost callback, on a stated claim | Chosen by Steven; Codex's review is pending, and it is not deployed |

## SD-001: Scope is A + C, D is a stretch goal

Steven accepted an external design review's findings and its proposed order on 6 Nov 2026. Option A and Option C are the 12 Nov target. If the verifier spike or the policy circuit fails its gate, the submission locks to Option A. Option D stays the long-term ambition, but it does not enter `main` until its open design questions have answers. This is a scheduling decision, not a rejection of D.

## SD-002: The contract derives or checks every proof input

An agent-chosen `day` could reset the daily cap, so `day`, `vendor`, `amount`, `invoice`, and `context` are computed by the contract from block time and from the same arguments it acts on. `policy_commitment` and `old_state` come from storage. Only `new_state` comes from the agent, and the proof constrains it. All field elements must be canonical. The binding table is in [threat-model.md](threat-model.md#5-proof-bindings-for-option-c).

**Enforced by:** `proof_for_a_fabricated_future_day_does_not_reset_the_cap` and its sandbox equivalent (step 7), `real_proof_binds_every_public_input`, and the mutants that hand each input to the agent ([zk-policy.md](zk-policy.md)).

## SD-003: One proof statement binds vendor, amount, invoice, and state

Allowlist membership must apply to the account that actually receives the transfer. Under Option C the transfer target and the proved vendor are one contract argument, hashed by the contract. Option D needs a stronger recipient binding, which is covered by SD-010.

**Enforced by:** `allowlisted_path_cannot_be_reused_for_another_vendor`, `proof_for_one_vendor_cannot_pay_another`, `proof_for_one_amount_cannot_transfer_another`, and sandbox steps 4 and 5.

## SD-004: Invalid proofs are logged and count toward the breaker

In v0.2, a blocked attempt is written to the log rather than reverted, and that feeds the circuit breaker. Under Option C a failed pairing check returns `false`, so the contract can do the same: write a `Blocked` entry with the reason `invalid proof`, increment `blocked_today`, and freeze at the limit.

This leaks only that an attempt happened, and the target was already public in the transaction arguments. The reason does not say which rule was broken.

Two limits are accepted for now:

- Malformed encodings make the NEAR host function abort, so the transaction reverts with no log entry and no count. Measured on the sandbox for invalid coordinates in A, B, and C: balance, state commitment, breaker count, and log are all unchanged, and each attempt burns 35.55 Tgas. The attacker gains nothing, but the breaker does not see these attempts.
- An honest prover refusing locally produces no on-chain trace, and a compromised agent can also simply stop trying. **Under Option C, the on-chain breaker can only count attempts that reach the chain.**

Counting malformed proofs would take a self-call, so that the abort happens in a child receipt whose failure a callback can see. That adds gas and another in-flight state, and we have not built it. **Recommendation: leave it out for the hackathon**, because the failure is safe and the key's allowance already caps the spam.

**Enforced by:** `invalid_proofs_trip_the_breaker_and_then_write_nothing`, `failed_transfer_is_not_counted_by_the_breaker`, and sandbox step 8.

## SD-005: One in-flight transfer per agent under Option C

`pay_zk` advances `state_commitment` before the cross-contract transfer finishes. If that transfer fails, the budget must come back, but a commitment cannot be decremented without a new proof. So each agent gets a pending lock: further `pay_zk` calls fail while a transfer is in flight, and the callback either clears the lock or restores `old_state`. Throughput for one agent drops to roughly one payment per few blocks. That is acceptable for the demo but would not suit a high-frequency agent.

If a callback runs out of gas, the lock would stick. `set_zk_agent` is the owner-only reset: it replaces the commitments and clears the lock.

**Enforced by:** `overlapping_payments_are_refused`, `failed_transfer_restores_budget_state_and_funds`, `late_failure_does_not_disturb_a_newer_payment`, and sandbox step 11.

## SD-006: Redemption store stays single-instance for the hackathon

The current store is a mutex plus an fsync'd append-only file, which is atomic only within one process. The hackathon deployment runs exactly one facilitator instance, and the README must say so. Production needs a database with a unique key on `(policy, log_id)`.

**Enforced by:** concurrent-redemption sandbox test, step 3c.

## SD-007: Local trusted setup, disclosed as non-production

Groth16 needs a per-circuit setup, and whoever keeps its randomness can forge proofs. The spike uses a fixed test seed, so **its keys offer no security at all**. The demo uses a fresh local setup. Docs and video must state that neither is a ceremony.

The setup must not run where the agent can see it: an agent holding that randomness could forge a proof for any payment. Under this design, finance runs it. Finance can already change the policy, so this gives finance no new power over its own funds.

## SD-008: Replace the hand-written HMAC

`hmac_sha256` at `crates/nearx402/src/vendor.rs:174` is hand-written over `sha2`, and the external design review forbids custom primitives. It passes the RFC 4231 test vector, so this is a policy fix rather than a known bug. The replacement uses `hmac::Hmac<Sha256>` and its `verify_truncated_left` for constant-time comparison. Adding the crate needs dependency approval.

## SD-009: Original implementation

Application logic, contracts, facilitator, circuit constraints, and tests are written by this team. Nothing is copied or ported from `fastnear/x402-facilitator` or any other x402 product. A text search of source and `Cargo.lock` on 6 Nov 2026 found no reference to `fastnear` or `x402-facilitator`; this is not a full provenance audit.

Foundations in use, with licences verified from their crate manifests when they are added:

| Library | Purpose | Licence |
| --- | --- | --- |
| `near-sdk`, `near-api`, `near-sandbox` | Contract and tooling | MIT or Apache-2.0 |
| `ark-*` (arkworks) | Groth16, BN254, R1CS, Poseidon gadgets; off-chain and test only | MIT or Apache-2.0 |

Curve operations on chain use only NEAR's `alt_bn128_*` host functions.

## SD-010: Withdrawal and recovery roles for Option D (open)

The brief contradicted itself: the vendor withdraws in one section, and an "owner key" is required in another. Before any Option D code, the team must define separate agent, vendor, finance recovery, and viewing roles. A viewing key must never be able to spend. Option C needs none of this, because funds go straight to the vendor.

## SD-011: Privacy track eligibility

Checked against the official `HACKATHON.md` on 6 Nov 2026. Integrations are hints, not requirements (Q-011), and a project can enter several tracks. The privacy track asks for "confidential computation, private finance and AI that protects sensitive data", so nothing excludes a ZK policy proof. The rules can change, so check them again before submitting.

Eligible is not the same as competitive. Option C's privacy is narrow (see [threat-model.md](threat-model.md#6-what-each-observer-learns)), so the entry must describe it as hiding the spending policy, not payments. The natural fit is `security` and `agents`, with `privacy` as an addition.

## SD-012: Verifying key lives in contract storage

Vendors trust the contract by its code hash, but the verifying key is state, set by the owner. So the owner can change which proofs the contract accepts without changing the code hash. This does not harm vendors, who rely only on the real token transfer. It does mean a verifier of the policy's integrity must also check the stored key. Compiling the key into the wasm would close this, at the cost of a rebuild per setup. **Status: Proposed**, keep it in storage for the hackathon. The view `get_zk_verifying_key` lets anyone read the stored key, and `demo-zk --network testnet` compares it with the agent's proving key before sending anything.

## SD-013: The agent's wallet file is plain text, and the chain decides its state

The chain holds only a commitment to the spending state, so a wallet that forgets the state behind it can never prove again. `crates/zx402-zk/src/store.rs` therefore writes the next state to disk, synced and renamed into place, **before** the transaction is sent, and on its next start asks the chain which state won. It never trusts the local outcome of a send, because a timeout can still land. Unconfirmed intents are kept, up to 32, so a send that looked lost and lands late is still recognised. If it holds neither the chain's state nor a recorded one, it refuses to prove, and finance recovers through `zk-setup recover` and `set_zk_agent`.

The file holds the private policy and salts unencrypted, with owner-only permissions on Unix. Encrypting it would add little here: the agent must read the policy to prove, so a compromised agent already learns it (see [threat-model.md](threat-model.md#6-what-each-observer-learns)). **Knowing the file does not let anyone break the policy**, since the circuit still enforces it. An advisory lock keeps two processes from overwriting each other's intents. Backups need care, since a restored old copy needs recovery, and recovery resets the day's spending. **Status: Proposed.**

**How a save writes the file.** Each save creates a new temporary file beside the wallet with `create_new` and mode 0600, skips any name already taken, syncs it, renames it into place, and syncs the directory. `create_new` fails on anything already at that name, including a symlink, so a leftover can neither lend its permissions to the wallet nor be written through. A wallet path that is itself a symlink is refused. Reviewer finding CX-02 showed why this matters: an earlier version reused a fixed temporary name, so a stale 0644 file made the wallet world-readable, and a stale symlink received the salts.

**What this does and does not defend against.** It handles leftovers from crashes, earlier runs and mistakes.

- **The parent directory is trusted.** Anyone who can write to it, other than its owner, can swap or delete the wallet whatever the code does. Keep it writable by its owner only. The code does not check this.
- A crash between creating and renaming a temporary file leaves it behind. It holds the same private data at 0600, and nothing removes it automatically.
- A wallet made readable from outside, for example by a copy, stays readable until its next save.
- The `.lock` file holds no data, and opening it does follow a symlink.
- On non-Unix systems the mode is not set.

Enforced by the tests in `store.rs` and by `crates/nearx402/tests/zk_separate_runs.rs`, which crash after and before sending, and restore an old backup, on a local sandbox.

## SD-014: A payment's claim is bound by a secret committed in the memo

Every other field of `X-PAYMENT` is public once the payment is on chain, and vendors accept the header from anyone. So an observer could rebuild it and claim the resource first. **This was demonstrated on the sandbox before the fix: the observer got HTTP 200.** The agent would have paid, and then received `402 payment already redeemed`. The money always reached the right vendor, so this is theft of the purchased response, not of funds.

The agent now picks 256 random bits per payment and puts `nonce:base64url(sha256(domain, secret))` in the memo. It reveals the secret only in its claim, and the facilitator recomputes the memo from the two. The contract does not change, since it treats the memo as opaque, and under Option C the proof binds the whole memo through the `invoice` input.

Alternatives considered: an agent signature over the request would bind the claim to the agent's key, but the facilitator would then have to check that key against the chain, and a function-call key may be rotated. An API key at the vendor would break the x402 model, where any client with a payment can claim.

Limits:

- It needs TLS on the agent-to-vendor and vendor-to-facilitator hops. Otherwise someone on the path can read the secret and race the request. The demo runs on loopback with plain HTTP, and hosting must use HTTPS.
- A dishonest vendor or facilitator sees the secret, but gains nothing it could not already do, since it controls the response anyway.
- The secret lives in memory, so a crash between paying and claiming loses that response.
- The memo grows to 108 of 128 bytes, so each log entry takes about 44 more bytes of contract storage.

**Status: Proposed.** Enforced by `crates/nearx402/tests/receipt_theft.rs` and by the checks of the secret's format in `x402.rs`.

## SD-015: A paying client checks an offer before signing, and follows no redirects

Found by Codex as CX-01. On NEAR the money moves when the contract is called, before anyone checks the payment against the vendor's terms. So a faulty or hostile 402 could name an approved vendor and an in-budget price with a token or network that cannot satisfy the purchase: the wallet paid, and the purchase failed. This stays inside the contract's cap and allowlist, so it is a wasted payment rather than an escape from policy. **Demonstrated on a sandbox before the fix.**

`AcceptedTerms::choose` in `crates/nearx402/src/agent.rs` now picks the first offer whose scheme, network, asset, policy hash, resource, payee, price, and nonce can all be used. Asset and policy hash come from the chain, by reading the payer's own contract. The network name comes from configuration, since the chain does not state it. Both the `pay` client and the `pay_zk` client go through it, so a mismatch stops before anything is signed or proved.

**Redirects.** `reqwest` keeps custom headers across redirects, including to another origin. Before the fix, a vendor's redirect on a retry delivered `X-PAYMENT` and its secret to the other origin, and the agent reported `Paid` with that origin's body. The paying client now follows none, in both directions, and a retry that gets one is reported as paid but unserved. A legitimate vendor behind a redirect must be addressed by its final URL.

**Lost responses, open.** If settlement succeeds and the HTTP 200 is lost, the money has moved and the receipt is spent. The agent now reports the log entry and transaction and says not to pay again, so it does not pay twice by accident, but it does not get the data. Recovering would need the vendor or facilitator to serve a second response to the holder of the secret, which weakens "one receipt, one response" and needs a response cache or an idempotent resource. **Recommendation: keep this as a documented limit for the hackathon.** The loss is bounded by one payment, and needs Steven's decision only if a judge's flow could hit it.

**Limits:** the checks cannot tell whether a compatible offer is honest about what it sells, or whether its price is fair within the budget. The contract's cap and allowlist still bound that. `quote` is shared with `demo-zk`, and the test exercises `quote` and the `pay_zk` flow directly, not by running a hostile vendor against the `demo-zk` binary.

## SD-016: Finance closes a payment with a lost callback, on a stated claim

A locked contract cannot be fixed, so a payment whose callback never ran would stay `Pending` with its amount reserved. Steven chose to add a way out, and to lock afterward. `reconcile_payment` lets the owner state whether the tokens moved, on an entry at least 24 hours old. Budgets never go back up. Design, tests and limits are in [reserved-recovery.md](reserved-recovery.md).

**The contract cannot check the precondition.** The owner must first see the payment's transaction `FINAL`, and read its transfer's outcome. Codex showed that the contract's own checks are not enough: a transfer executing after the balance read would leave `balance` above the tokens held. Whether NEAR allows that schedule is not shown either way, and no published guarantee was found, so this design does not rely on receipt order.

**The new power:** finance can label a stuck entry `Paid` or `Failed`. The log then records a claim, and says so in `reason` and in a `payment_reconciled` event. Finance gains no new way to move tokens, and nobody else can call the method. A mistake by the owner can make later payments fail. The token still refuses to move more than it holds.

**Limits:** withdrawals are not covered, so a lost withdrawal callback after locking leaves that amount reserved for good, and several lost callbacks add up. Whether to accept this before locking is Steven's decision, still open. The precondition is checked by `reconcile-check`, an operational control: the contract does not verify `FINAL`. The 24 hours is a backstop against a hasty call, not evidence.

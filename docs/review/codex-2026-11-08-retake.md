# Codex independent review and testnet retake — 8 November 2026

## Result

Reviewed `3e047df`, `0ea1964`, and the Unicode fix committed as `0f461f0c2de2903cb093e59650818b0b29a50d8e`. The requested testnet redeploy, verification, and recorded demo have completed. No push, key deletion, policy reset, or new setup was performed. The contract remains upgradeable, per Steven's decision.

## Independent review evidence

- Recovery checkpoint: 22 reconcile unit tests, 14 checker tests, and one old -> new -> old sandbox upgrade rehearsal passed. All storage and views were checked by the rehearsal. The initial reviewer attempt incorrectly reused build artifacts between old and new trees; an isolated old build corrected that environment problem and matched the historical deployed hash.
- On `0ea1964`: 7 rejection unit tests, one facilitator rate-limit test, one receipt-theft integration test, and one paid-not-served integration test passed independently.
- Reviewer found a reproducible P2 panic: a multibyte character immediately before a URL made `strip_url` slice inside UTF-8. This is an off-chain error-handling defect, not a demonstrated funds exploit or the proven cause of the earlier HTTP 402.
- Fixed source SHA-256: `192ea202e3f5154a5cecf9297374df5a6f2c47c460a84314db207836b2a90fd7`. This matches `0f461f0` and the source used for the retake. Eleven rejection unit tests, the reviewer's original regression test, and the facilitator rate-limit test passed after the fix. The specific finding is closed for these tested paths; this is not a complete cryptographic audit.
- Logs: `/home/bejohen/.cache/nearx402-review-bb706b7/evidence` and `/home/bejohen/.cache/nearx402-review-0ea1964/evidence`.

## Deployment

- Account: `nearx402-policy.testnet`.
- Transaction: `AWpxaYA7xVnw8Y2WXhY1fkQD7Q6f3KNv3cwLayGaDCVU`.
- Old code hash: `CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU`.
- New code hash: `6BA4L1moNbXF8iUuZTzk4ag93cRwtE5mmqWBu83mQsTx`.
- Deployed without init. Compared every storage key/value, nine views, token balances and agent permissions before/after: unchanged. The policy key remains. Token hash unchanged: `HaXWMvDr4W7b6PZujZGuFcRbhrCjbdaJ7hi4S7U4ad7g`.

## Failed take retained

The first take paid 0.05 mUSDC as log #3, transaction `5txs2D6S8jXhguVRfiwBrZxXc2kvvM21gmgEGQsJVq1s`, then the legitimate data claim returned HTTP 402. The body was not captured. A subsequent read to the original RPC returned HTTP 429, but this does not prove the earlier cause. The payment is FINAL and remains paid/unserved; no refund or replay was attempted. The original cast/log remain in `/home/bejohen/near-hackathon/demo-2026-11-08/`.

## Successful retake

Used a separate per-run configuration with `https://test.rpc.fastnear.com`, a public NEAR testnet provider listed in NEAR's documentation. Checked chain id, deployed hash and `view_state` before spending. The project's `deploy/testnet.json` was not changed. No automated payment retry was added; this was a distinct authorized test take after review.

Retake source: `0f461f0`; executable SHA-256 `148721dc94bd50ba02ee6fa815d0bbc70c456efa364bce2a3ffe5509c3e46bb2`.

| Step | Transaction | Outcome |
| --- | --- | --- |
| Payment, log #4 | `G46TzNiacRWn9JtxKr3ucLRfotLLM7FoGNeeeN5eHeL` | FINAL, Paid, exactly 50000 base units policy -> vendor; 41.44 Tgas reported by demo |
| Attempt A, log #5 | `AWM9KTdm2V9hmPbZyk6fEzbjuv6Up5WRt1V1RdAgtbqV` | FINAL, Blocked / invalid proof, no transfer; 36.19 Tgas |
| Attempt B, log #6 | `248x4Q8gaKTt8aUE36duxFy2FaBLrjBtSxTPxePqwRmS` | FINAL, Blocked / invalid proof, no transfer; 36.22 Tgas |

Each transaction has zero failed receipts. The blocked calls succeed as transactions because they record the refusal. All three use the existing pay_zk-only agent key.

Live local HTTP results in the recording: observer HTTP 402 for secret mismatch, legitimate HTTP 200, replay HTTP 402 for already redeemed. These are recorded HTTP evidence, not claims inferred from chain reads. Run exited 0 in 23.140 seconds; proof creation printed 183 ms. The timing describes this run, not a benchmark or guarantee.

At independent verification block `DTn67YRqdvakLvE6kU8dXveYVo9LBqyW6mdZazdSJo3h`:

- Policy balance 9.85 mUSDC, vendor 0.15 mUSDC, finance 10.00 mUSDC, outside recipient and agent token balances 0.
- Policy still has one access key; agent still has only its pay_zk-limited key. Remaining agent allowance: 973379992603403500000000 yoctoNEAR.
- Recorded breaker: 2 of 5, not frozen.

## Recording and evidence

Folder: `/home/bejohen/near-hackathon/demo-2026-11-08/take-02/`.

- `testnet-demo.cast`: original live terminal recording with timestamps.
- `testnet-demo.log`: original output.
- `testnet-demo.html`: self-contained interactive replay; no external resources.
- `testnet-demo.avi`: silent MJPEG video rendered from the actual timestamped output, with a 5-second intro and 7-second end hold; 35.5 seconds total. This is terminal footage, not a polished narrated submission video.
- `verification.json`: independently read finality, transfer events, balances and access-key permissions; individual transaction JSON files are also saved.
- `recording.json`: exit code and timing.

The HTML script was syntax checked; AVI container lengths were checked; rendered start/end frame images were inspected. No media-player playback or third-party audit is claimed.

## Next developer action

Update README, docs/testnet.md, docs/review/status.md and the demo narration to reflect the verified successful run and the retained failed take. Keep the mutable-contract / mock-token / local trusted setup / policy-only privacy limitations. Do not push, submit, lock, change keys, or send more transactions under this report. Publication and any permanent action remain Steven's decision.

The direct UI message carrying this final evidence could not be sent because application access timed out. This report is the durable reviewer handoff.
#!/usr/bin/env bash
# One-command private-policy demo, from a clean local NEAR sandbox. No accounts
# or funds needed. Exits non-zero if any outcome differs from the story.
# Pass --serve to keep the public dashboard running at http://127.0.0.1:8402/.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-contracts.sh
# The prover profile, because the dev profile proves much more slowly.
cargo run -q -p nearx402 --bin demo-zk --profile prover -- "$@"

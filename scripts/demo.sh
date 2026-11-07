#!/usr/bin/env bash
# One-command demo: builds the contracts and runs the story on a local NEAR sandbox.
# Pass --serve to keep the vendors and dashboard running at http://127.0.0.1:8402/.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-contracts.sh
cargo run -q -p nearx402 --bin demo -- "$@"

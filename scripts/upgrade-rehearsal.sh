#!/usr/bin/env bash
# Rehearses an in-place redeploy of the policy on a local NEAR sandbox: the build on
# testnet, then the current one, then back. Sends nothing to any network but the
# sandbox. Builds the old contract from a commit, in a temporary directory.
#
#     scripts/upgrade-rehearsal.sh [commit]    # default 32b3be4, the build on testnet
set -euo pipefail
cd "$(dirname "$0")/.."
COMMIT="${1:-32b3be4}"

scripts/build-contracts.sh >/dev/null

OLD_DIR="$(mktemp -d)"
trap 'rm -rf "$OLD_DIR"' EXIT
git archive "$COMMIT" | tar -x -C "$OLD_DIR"
(cd "$OLD_DIR" && scripts/build-contracts.sh) | grep 'res/policy.wasm'

OLD_POLICY_WASM="$OLD_DIR/res/policy.wasm" cargo test -p nearx402 --profile prover \
  --test upgrade_rehearsal -- --ignored --nocapture

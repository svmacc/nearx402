#!/usr/bin/env bash
# Evidence for an independent review of the redeploy: builds the commit under
# review, the build on testnet, and a control build with one extra state field,
# each from `git archive` in a temporary directory, then runs the upgrade
# rehearsal and the control on a local NEAR sandbox. Keeps every log in full.
#
#     scripts/review-evidence.sh <commit> <out-dir>    # OLD_COMMIT defaults to 32b3be4
#
# Sends nothing to any network but the local sandbox. With empty caches, cargo
# fetches crates from crates.io and near-sandbox downloads its binary from NEAR's
# build server; with `OFFLINE=1`, cargo is run with --offline and fails instead.
# Offline, also set NEAR_SANDBOX_BIN_PATH to a near-sandbox 2.14.0 binary, such as
# target/prover/build/near-sandbox-*/out/.near/near-sandbox-2.14.0/near-sandbox
# from an earlier build: a fresh target directory has none.
set -euo pipefail
COMMIT="${1:?usage: review-evidence.sh <commit> <out-dir>}"
OUT="${2:?usage: review-evidence.sh <commit> <out-dir>}"
OLD_COMMIT="${OLD_COMMIT:-32b3be4}"
TESTNET_POLICY_HASH="CXerHAq8tAUUtxKUteMJ1TwKoupEnFjRj3zgQF6moGYU"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$REPO/scripts/review-evidence.sh"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
CARGO_FLAGS=()
[ "${OFFLINE:-0}" = 1 ] && CARGO_FLAGS=(--offline)

mkdir -p "$WORK/new" "$WORK/old"
git -C "$REPO" archive "$COMMIT" | tar -x -C "$WORK/new"
git -C "$REPO" archive "$OLD_COMMIT" | tar -x -C "$WORK/old"
cp -a "$WORK/new" "$WORK/control"
python3 -I - "$WORK/control/contracts/policy/src/lib.rs" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
a = "    reserved: u128,\n    /// Bumped whenever"
b = "            reserved: 0,\n"
assert s.count(a) == 1 and s.count(b) == 1, "control patch does not apply"
s = s.replace(a, "    reserved: u128,\n    control_field: u64,\n    /// Bumped whenever", 1)
s = s.replace(b, b + "            control_field: 0,\n", 1)
open(p, "w").write(s)
PY

{
  echo "commit under review: $(git -C "$REPO" rev-parse "$COMMIT")"
  echo "old commit:          $(git -C "$REPO" rev-parse "$OLD_COMMIT")"
  echo "run at (UTC):        $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "this script sha256:  $(sha256sum "$SCRIPT" | cut -d' ' -f1)"
  echo "rustc:               $(rustc -V)"
  echo "cargo:               $(cargo -V)"
  echo "wasm32 target:       $(rustup target list --installed 2>/dev/null | grep -c wasm32-unknown-unknown) installed"
  echo "cargo-near:          $(cargo near --version 2>/dev/null || echo 'not installed (plain cargo build is used)')"
  echo "python3:             $(python3 -V 2>&1)"
  echo "kernel:              $(uname -sr)"
  echo "offline:             ${OFFLINE:-0}"
  if [ -n "${NEAR_SANDBOX_BIN_PATH:-}" ]; then
    echo "near-sandbox binary: $NEAR_SANDBOX_BIN_PATH, sha256 $(sha256sum "$NEAR_SANDBOX_BIN_PATH" | cut -d' ' -f1)"
  else
    echo "near-sandbox binary: downloaded into the fresh target directory (needs network)"
  fi
  echo "control patch:       one u64 field after \`reserved\` in the contract state"
} > "$OUT/environment.txt"

# Each tree builds in its own target directory, as a fresh checkout would.
unset CARGO_TARGET_DIR
for tree in new old control; do
  (cd "$WORK/$tree" && scripts/build-contracts.sh) > "$OUT/build-$tree.log" 2>&1
done

hash_of() { grep "res/$1.wasm" "$OUT/build-$2.log" | sed 's/.*code hash //'; }
{
  echo "old policy (testnet build):  $(hash_of policy old)"
  echo "new policy (under review):   $(hash_of policy new)"
  echo "control policy:              $(hash_of policy control)"
  echo "token, old:                  $(hash_of mock_usdc old)"
  echo "token, new:                  $(hash_of mock_usdc new)"
  echo "testnet policy, expected:    $TESTNET_POLICY_HASH"
} > "$OUT/hashes.txt"

set +e
(cd "$WORK/new" && OLD_POLICY_WASM="$WORK/old/res/policy.wasm" \
  cargo test "${CARGO_FLAGS[@]}" -p nearx402 --profile prover --test upgrade_rehearsal -- --ignored --nocapture) \
  > "$OUT/rehearsal.log" 2>&1
rehearsal=$?
(cd "$WORK/new" && OLD_POLICY_WASM="$WORK/old/res/policy.wasm" NEW_POLICY_WASM="$WORK/control/res/policy.wasm" \
  cargo test "${CARGO_FLAGS[@]}" -p nearx402 --profile prover --test upgrade_rehearsal -- --ignored --nocapture) \
  > "$OUT/control.log" 2>&1
control=$?
set -e

old_ok=no; [ "$(hash_of policy old)" = "$TESTNET_POLICY_HASH" ] && old_ok=yes
deser=no; grep -q 'Cannot deserialize the contract state' "$OUT/control.log" && deser=yes
{
  echo "old build equals testnet's:        $old_ok"
  echo "rehearsal exit:                    $rehearsal (must be 0)"
  echo "control exit:                      $control (must be non-zero)"
  echo "control failed on deserialization: $deser (must be yes)"
} > "$OUT/summary.txt"
(cd "$OUT" && sha256sum environment.txt hashes.txt build-*.log rehearsal.log control.log > SHA256SUMS)
cat "$OUT/hashes.txt" "$OUT/summary.txt"
[ "$old_ok" = yes ] && [ "$rehearsal" = 0 ] && [ "$control" != 0 ] && [ "$deser" = yes ]

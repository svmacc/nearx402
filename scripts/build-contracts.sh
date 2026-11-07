#!/usr/bin/env bash
# Builds the contract wasm files into ./res. Uses cargo-near when installed,
# otherwise a plain cargo build for wasm32-unknown-unknown.
#
# The printed code hash is the base58 SHA-256 that NEAR reports for deployed
# code (and that the facilitator's --policy-code-hash expects).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p res

# Panic messages embed source paths. Remap them so the wasm (and its code
# hash) does not depend on where this checkout or the cargo registry lives.
# The same rustc version is still needed to get the same bytes.
REMAP="--remap-path-prefix=$PWD=/nearx402 --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"

base58() {
  python3 - "$1" <<'PY'
import hashlib, sys
alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
digest = hashlib.sha256(open(sys.argv[1], "rb").read()).digest()
n = int.from_bytes(digest, "big")
out = ""
while n:
    n, r = divmod(n, 58)
    out = alphabet[r] + out
out = "1" * (len(digest) - len(digest.lstrip(b"\0"))) + out
print(out)
PY
}

for pkg in policy mock-usdc zk-spike; do
  dir="contracts/$pkg"
  name="${pkg//-/_}"
  if cargo near --version >/dev/null 2>&1; then
    (cd "$dir" && cargo near build non-reproducible-wasm --no-abi >/dev/null)
    cp "target/near/$name/$name.wasm" "res/$name.wasm"
  else
    RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-s $REMAP" \
      cargo build -q -p "$pkg" --target wasm32-unknown-unknown --release
    cp "target/wasm32-unknown-unknown/release/$name.wasm" "res/$name.wasm"
  fi
  if command -v python3 >/dev/null 2>&1; then
    hash="code hash $(base58 "res/$name.wasm")"
  else
    hash="sha256 $(sha256sum "res/$name.wasm" | cut -d' ' -f1) (hex)"
  fi
  echo "res/$name.wasm  $(wc -c < "res/$name.wasm") bytes  $hash"
done

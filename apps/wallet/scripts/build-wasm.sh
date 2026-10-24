#!/usr/bin/env bash
# Builds the wallet core (crates/mmx-wallet-wasm) as WebAssembly for the web wallet.
#
#   scripts/build-wasm.sh          -> src/backend/web/pkg        (shipped build)
#   scripts/build-wasm.sh --test   -> tests/.wasm-test/pkg       (adds parity-test hooks)
#
# From any shell (cmd.exe and PowerShell too): npm run build:wasm [-- --test]
# (scripts/build-wasm.ts runs this script in bash, on Windows Git for Windows').
#
# Needs the wasm32-unknown-unknown Rust target and wasm-bindgen-cli matching
# the wasm-bindgen crate version below.
set -euo pipefail

BINDGEN_VERSION="0.2.127"
APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_DIR="$(cd "$APP_DIR/../.." && pwd)"
TARGET_DIR="${WALLET_TARGET_DIR:-$ROOT_DIR/target/wallet}"
PROFILE="wasm-release"

FEATURES=()
OUT_DIR="$APP_DIR/src/backend/web/pkg"
case "${1:-}" in
  "") ;;
  --test)
    FEATURES=(--features test-nonce)
    OUT_DIR="$APP_DIR/tests/.wasm-test/pkg"
    ;;
  *)
    echo "usage: $0 [--test]" >&2
    exit 2
    ;;
esac

if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen not found: cargo install wasm-bindgen-cli --version $BINDGEN_VERSION --locked" >&2
  exit 1
fi
installed="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$installed" != "$BINDGEN_VERSION" ]; then
  echo "wasm-bindgen $installed found, $BINDGEN_VERSION required" >&2
  exit 1
fi

# One build at a time: the shipped and test builds share the cargo output file.
mkdir -p "$TARGET_DIR"
if command -v flock >/dev/null 2>&1; then
  exec 9>"$TARGET_DIR/.build-wasm.lock"
  flock 9
fi

# Low CPU and I/O priority, two jobs, no incremental state or debug symbols.
LOW_PRIORITY=()
if command -v nice >/dev/null 2>&1; then LOW_PRIORITY+=(nice -n 19); fi
if command -v ionice >/dev/null 2>&1; then LOW_PRIORITY+=(ionice -c 3); fi

(
  cd "$ROOT_DIR"
  CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_DEV_DEBUG=0 \
  CARGO_PROFILE_TEST_DEBUG=0 \
  CARGO_PROFILE_RELEASE_DEBUG=0 \
  CARGO_TARGET_DIR="$TARGET_DIR" \
    ${LOW_PRIORITY[@]+"${LOW_PRIORITY[@]}"} cargo build --locked -p mmx-wallet-wasm \
      --target wasm32-unknown-unknown --profile "$PROFILE" --jobs 2 \
      ${FEATURES[@]+"${FEATURES[@]}"}
)

WASM="$TARGET_DIR/wasm32-unknown-unknown/$PROFILE/mmx_wallet_wasm.wasm"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
wasm-bindgen --target web --out-dir "$OUT_DIR" \
  --remove-name-section --remove-producers-section "$WASM"

size="$(wc -c < "$OUT_DIR/mmx_wallet_wasm_bg.wasm" | tr -d ' ')"
echo "mmx_wallet_wasm_bg.wasm: $size bytes -> ${OUT_DIR#"$APP_DIR"/}"

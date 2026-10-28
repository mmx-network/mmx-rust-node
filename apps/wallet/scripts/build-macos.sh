#!/usr/bin/env bash
# Builds the MMX Wallet for macOS 11 or later as one universal app (Apple
# silicon and Intel) and packages it as a dmg in artifacts/packages.
# Run on macOS with the Xcode command line tools, rustup and Node.js.
#
# Signing (optional):
#   APPLE_SIGNING_IDENTITY       "Developer ID Application: …" in the keychain,
#   APPLE_CERTIFICATE            or a base64 .p12 to import for this build,
#   APPLE_CERTIFICATE_PASSWORD   with its password.
# Notarization (optional, needs signing): APPLE_API_ISSUER, APPLE_API_KEY and
# APPLE_API_KEY_PATH, or APPLE_ID, APPLE_PASSWORD and APPLE_TEAM_ID.
# Without an identity the app is signed ad hoc: it runs on the build Mac;
# other Macs ask the user to confirm on first open.
set -euo pipefail
[[ $(uname -s) == Darwin ]] || { printf 'Run this build on macOS.\n' >&2; exit 1; }

wallet=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
artifacts=$wallet/artifacts
target_dir=$artifacts/build-macos
packages=$artifacts/packages
target=universal-apple-darwin
json_field() { node -e 'process.stdout.write(String(require(process.argv[1])[process.argv[2]]))' "$1" "$2"; }
version=$(json_field "$wallet/package.json" version)
product=$(json_field "$wallet/src-tauri/tauri.conf.json" productName)

# One baseline for Rust, C dependencies and the bundle.
export MACOSX_DEPLOYMENT_TARGET=11.0
export CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0

[[ -f $wallet/src/backend/web/pkg/mmx_wallet_wasm.js ]] || {
    printf 'The web wallet core is missing (needed to check the shared interface): run npm run build:wasm\n' >&2
    exit 1
}
mkdir -p "$target_dir" "$packages"
rustup target add aarch64-apple-darwin x86_64-apple-darwin

# Expanded with the +-form below: bash 3.2 (macOS) treats an empty array as unset.
signing=()
if [[ -z ${APPLE_SIGNING_IDENTITY:-} && -z ${APPLE_CERTIFICATE:-} ]]; then
    printf '{"bundle":{"macOS":{"signingIdentity":"-"}}}\n' >"$target_dir/adhoc-signing.json"
    signing=(--config "$target_dir/adhoc-signing.json")
fi

cd "$wallet"
npm ci --no-audit --no-fund
CARGO_TARGET_DIR=$target_dir npm run tauri build -- \
    --target "$target" --no-bundle ${signing[@]+"${signing[@]}"} -- --locked
# Tauri creates a universal main executable; combine the additional CLI too.
lipo -create "$target_dir/aarch64-apple-darwin/release/mmxwallet" \
    "$target_dir/x86_64-apple-darwin/release/mmxwallet" \
    -output "$target_dir/$target/release/mmxwallet"
CARGO_TARGET_DIR=$target_dir npm run tauri bundle -- \
    --target "$target" --bundles app,dmg ${signing[@]+"${signing[@]}"}

bundle=$target_dir/$target/release/bundle
app=$bundle/macos/$product.app
dmg=$bundle/dmg/${product}_${version}_universal.dmg
[[ -d $app && -f $dmg ]] || { printf 'Missing app or dmg in %s\n' "$bundle" >&2; exit 1; }
binary=$app/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")
archs=$(lipo -archs "$binary")
[[ $archs == *x86_64* && $archs == *arm64* ]] || { printf 'Not a universal binary: %s\n' "$archs" >&2; exit 1; }
minimum=$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$app/Contents/Info.plist")
[[ $minimum == 11.0 ]] || { printf 'Unexpected minimum macOS version: %s\n' "$minimum" >&2; exit 1; }
codesign --verify --deep --strict "$app"
cli=$app/Contents/MacOS/mmxwallet
cli_archs=$(lipo -archs "$cli")
[[ $cli_archs == *x86_64* && $cli_archs == *arm64* ]] || { printf 'CLI is not universal.\n' >&2; exit 1; }
python3 "$wallet/scripts/check-packaged-cli.py" "$cli"
hdiutil verify "$dmg"

name=mmx-wallet_${version}_macos_universal.dmg
publish=$(mktemp -d "$packages/.publish-XXXXXX")
install -m 0644 "$dmg" "$publish/$name"
(cd "$publish" && shasum -a 256 "$name" >sha256sums-macos-universal)
for file in "$publish"/*; do mv -f -- "$file" "$packages/"; done
rmdir -- "$publish"
ls -l "$packages/$name"

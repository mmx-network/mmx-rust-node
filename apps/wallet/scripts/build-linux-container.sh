#!/usr/bin/env bash
# Builds the Linux desktop packages (deb and AppImage) in a container and
# checks that both start. Packages: apps/wallet/artifacts/packages.
# Arguments are passed to build-linux.sh (e.g. --no-launch-check).
# REQUIRE_CHAIN=1 fails the command check instead of skipping its chain
# server checks when the server cannot be reached.
set -euo pipefail
wallet=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
repository=$(cd -- "$wallet/../.." && pwd)
image=${WALLET_BUILD_IMAGE:-mmx-wallet-linux-build:1}

context=$(mktemp -d)
trap 'rmdir -- "$context"' EXIT
mkdir -p "$wallet/artifacts/cargo" "$wallet/artifacts/cache" "$wallet/artifacts/build"
# The image needs no build context; the repository is mounted instead.
docker build --tag "$image" --file "$wallet/scripts/Dockerfile.linux" "$context"

# The calling user needs an account in the container (the session bus used by
# the launch check looks it up).
accounts=$wallet/artifacts/cache/accounts
mkdir -p "$accounts"
{ docker run --rm "$image" cat /etc/passwd; printf 'builder:x:%s:%s:builder:/tmp:/bin/bash\n' "$(id -u)" "$(id -g)"; } >"$accounts/passwd"
{ docker run --rm "$image" cat /etc/group; printf 'builder:x:%s:\n' "$(id -g)"; } >"$accounts/group"

# The repository is mounted at the same path, so paths are identical inside
# and outside the container. Builds run as the calling user at low priority.
docker run --rm --init \
    --name "mmx-wallet-linux-build-$$" \
    --user "$(id -u):$(id -g)" \
    --volume "$repository:$repository" \
    --volume "$accounts/passwd:/etc/passwd:ro" \
    --volume "$accounts/group:/etc/group:ro" \
    --workdir "$wallet" \
    --env HOME=/tmp \
    --env RUSTUP_TOOLCHAIN=1.97.0 \
    --env CARGO_HOME="$wallet/artifacts/cargo" \
    --env XDG_CACHE_HOME="$wallet/artifacts/cache" \
    --env npm_config_cache="$wallet/artifacts/cache/npm" \
    --env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
    --env REQUIRE_CHAIN \
    "$image" nice -n 19 ionice -c 3 "$wallet/scripts/build-linux.sh" "$@"

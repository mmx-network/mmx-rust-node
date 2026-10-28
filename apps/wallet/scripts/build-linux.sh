#!/usr/bin/env bash
# Builds the MMX Wallet for Linux x86_64 as a deb and an AppImage
# (artifacts/packages), then checks the packages under Xvfb with an empty
# wallet directory: each one starts, shows its window and keeps running
# (screenshots in artifacts/launch-check), and every command of the app
# behaves as the wallet interface expects (src-tauri/tests/ipc_check.py).
# Runs in the build container: use scripts/build-linux-container.sh.
#
#   --no-launch-check   only build and package
set -euo pipefail

launch_check=1
for arg in "$@"; do
    case $arg in
        --no-launch-check) launch_check=0 ;;
        *) printf 'Unknown option: %s\n' "$arg" >&2; exit 2 ;;
    esac
done
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || { printf 'Build on Linux x86_64.\n' >&2; exit 1; }

wallet=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
repository=$(cd -- "$wallet/../.." && pwd)
artifacts=$wallet/artifacts
stage=$artifacts/stage/wallet
target_dir=$artifacts/build/desktop
packages=$artifacts/packages
shots=$artifacts/launch-check
json_field() { node -e 'process.stdout.write(String(require(process.argv[1])[process.argv[2]]))' "$1" "$2"; }
version=$(json_field "$wallet/package.json" version)
product=$(json_field "$wallet/src-tauri/tauri.conf.json" productName)
export CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}

[[ -f $wallet/src/backend/web/pkg/mmx_wallet_wasm.js ]] || {
    printf 'The web wallet core is missing (needed to check the shared interface): run npm run build:wasm\n' >&2
    exit 1
}
# The interface is built from a clean copy with the locked npm dependencies,
# so the working tree's node_modules and dist are left alone.
mkdir -p "$stage" "$packages"
# Of the desktop crate only the shared test fixtures are needed (typecheck).
rsync -a --delete \
    --exclude /node_modules --exclude /dist --exclude /artifacts \
    --exclude /test-results --exclude /playwright-report \
    --include /src-tauri/ --include /src-tauri/tests/ --include /src-tauri/tests/payloads.json \
    --exclude '/src-tauri/**' \
    "$wallet/" "$stage/"
(cd "$stage" && npm ci --no-audit --no-fund && npm run build:desktop)

cd "$wallet"
config=$(printf '{"build":{"beforeBuildCommand":null,"frontendDist":"%s"}}' "$stage/dist/desktop")
CARGO_TARGET_DIR=$target_dir "$stage/node_modules/.bin/tauri" build \
    --bundles deb,appimage --config "$config" -- --locked

bundle=$target_dir/release/bundle
deb=$bundle/deb/${product}_${version}_amd64.deb
appimage=$bundle/appimage/${product}_${version}_amd64.AppImage
for file in "$deb" "$appimage"; do
    [[ -f $file ]] || { printf 'Missing package: %s\n' "$file" >&2; exit 1; }
done

# Publish all files together; a running AppImage keeps its old file.
publish=$(mktemp -d "$packages/.publish-XXXXXX")
deb_name=mmx-wallet_${version}_amd64.deb
appimage_name=mmx-wallet_${version}_amd64.AppImage
install -m 0644 "$deb" "$publish/$deb_name"
install -m 0755 "$appimage" "$publish/$appimage_name"
(cd "$publish" && sha256sum "$deb_name" "$appimage_name" > sha256sums-linux-amd64)
for file in "$publish"/*; do mv -f -- "$file" "$packages/"; done
rmdir -- "$publish"
dpkg-deb --field "$packages/$deb_name" Package Version Architecture Maintainer Section Depends Installed-Size

# A free X display number.
free_display() {
    local n
    for n in $(seq 90 189); do
        [[ -e /tmp/.X11-unix/X$n || -e /tmp/.X$n-lock ]] || { printf ':%s' "$n"; return; }
    done
    return 1
}

# Starts Xvfb on display $1 (PID in xvfb_pid) and waits until it accepts clients.
start_display() {
    Xvfb "$1" -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
    xvfb_pid=$!
    for _ in $(seq 100); do
        DISPLAY=$1 xdotool getdisplaygeometry >/dev/null 2>&1 && break
        sleep 0.1
    done
    DISPLAY=$1 openbox --sm-disable >/dev/null 2>&1 &
    wm_pid=$!
    for _ in $(seq 50); do
        if DISPLAY=$1 xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; then break; fi
        sleep 0.1
    done
}

stop_display() {
    [[ $(cat "/proc/$2/comm" 2>/dev/null) == openbox ]] && kill -TERM "$2"
    wait "$2" 2>/dev/null || true
    [[ $(cat "/proc/$1/comm" 2>/dev/null) == Xvfb ]] && kill -TERM "$1"
    wait "$1" 2>/dev/null || true
}

# An empty home for the app: wallet directory, settings, caches, runtime dir.
app_home() {
    local home
    home=$(mktemp -d)
    mkdir -p "$home/run" && chmod 0700 "$home/run"
    printf '%s' "$home"
}

# Saves a PNG screenshot of display $1 to $2; prints its number of colors.
screenshot() {
    local raw
    raw=$(mktemp)
    DISPLAY=$1 xwd -root -silent >"$raw" 2>/dev/null || true
    python3 - "$raw" "$2" <<'PY' || printf '0'
import struct, sys, zlib
data = open(sys.argv[1], "rb").read()
h = struct.unpack(">25I", data[:100])
width, height, bytes_per_line, colormap = h[4], h[5], h[12], h[19]
pixel, little, masks = h[11] // 8, h[7] == 0, h[14:17]
shifts = [(m & -m).bit_length() - 1 for m in masks]
pixels = data[h[0] + colormap * 12:]
rows, colors = [], set()
for y in range(height):
    line, row = pixels[y * bytes_per_line:(y + 1) * bytes_per_line], bytearray(b"\0")
    for x in range(width):
        v = int.from_bytes(line[x * pixel:(x + 1) * pixel], "little" if little else "big")
        rgb = bytes((v & m) >> s for m, s in zip(masks, shifts))
        row += rgb
        colors.add(rgb)
    rows.append(bytes(row))
chunk = lambda t, b: struct.pack(">I", len(b)) + t + b + struct.pack(">I", zlib.crc32(t + b))
open(sys.argv[2], "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                              + chunk(b"IDAT", zlib.compress(b"".join(rows), 6)) + chunk(b"IEND", b""))
print(len(colors), end="")
PY
    rm -f -- "$raw"
}

# Starts "$@" with an empty home on its own display and session bus, waits for
# its window, takes a screenshot and stops exactly the processes it started.
check_launch() {
    local label=$1; shift
    local home display xvfb wm app owner="" bus="" windows=0 window="" colors=0 status=0
    home=$(app_home)
    display=$(free_display)
    start_display "$display"
    xvfb=$xvfb_pid
    wm=$wm_pid

    env HOME="$home" MMX_HOME="$home/node" XDG_CONFIG_HOME="$home/config" \
        XDG_DATA_HOME="$home/data" XDG_CACHE_HOME="$home/cache" XDG_RUNTIME_DIR="$home/run" \
        DISPLAY="$display" setsid dbus-run-session -- "$@" >"$home/app.log" 2>&1 &
    app=$!
    for _ in $(seq 40); do
        window=$(DISPLAY=$display xdotool search --onlyvisible --name "^${product}\$" 2>/dev/null | head -n 1 || true)
        [[ -n $window ]] && break
        kill -0 "$app" 2>/dev/null || break
        sleep 0.5
    done
    sleep 10

    [[ -n $window ]] || { printf '%s: no visible window\n' "$label" >&2; status=1; }
    if [[ -n $window ]]; then
        owner=$(DISPLAY=$display xdotool getwindowpid "$window" 2>/dev/null || true)
        if [[ -z $owner || $(ps -o pgid= -p "$owner" 2>/dev/null | tr -d ' ') != "$app" ]]; then
            printf '%s: the app is not running\n' "$label" >&2; status=1
        fi
        if [[ $label == appimage ]]; then
            DISPLAY=$display xprop -id "$window" _NET_WM_ICON >"$home/icon.txt"
            if ! grep -q 'Icon (128 x 128)' "$home/icon.txt" || ! grep -q 'Icon (192 x 192)' "$home/icon.txt"; then
                printf 'appimage: the window does not expose its high-resolution icon\n' >&2; status=1
            fi
        fi
    fi
    # A second start hands over to the running app (session bus) and exits.
    bus=""
    [[ -n $owner ]] && bus=$(tr '\0' '\n' <"/proc/$owner/environ" 2>/dev/null | sed -n 's/^DBUS_SESSION_BUS_ADDRESS=//p')
    if [[ -n $bus ]]; then
        if env HOME="$home" MMX_HOME="$home/node" XDG_CONFIG_HOME="$home/config" \
            XDG_DATA_HOME="$home/data" XDG_CACHE_HOME="$home/cache" XDG_RUNTIME_DIR="$home/run" \
            DISPLAY="$display" DBUS_SESSION_BUS_ADDRESS="$bus" timeout 30 "$@" >"$home/second.log" 2>&1; then
            windows=$(DISPLAY=$display xdotool search --onlyvisible --name "^${product}\$" 2>/dev/null | wc -l)
            (( windows == 1 )) || { printf '%s: a second start opened another window\n' "$label" >&2; status=1; }
        else
            printf '%s: a second start did not hand over to the running app\n' "$label" >&2; status=1
        fi
    else
        printf '%s: no session bus to check a second start\n' "$label" >&2; status=1
    fi
    if grep -qiE 'panicked|could not start' "$home/app.log"; then
        printf '%s: error output\n' "$label" >&2; status=1
    fi
    if [[ -e $home/.mmx || -e $home/node ]]; then
        printf '%s: the wallet directory was created without a change\n' "$label" >&2; status=1
    fi
    mkdir -p "$shots"
    colors=$(screenshot "$display" "$shots/$label.png")
    # A window that never painted its page has only a few flat colors.
    (( ${colors:-0} >= 64 )) || { printf '%s: the window shows no content (%s colors)\n' "$label" "${colors:-0}" >&2; status=1; }

    # Stop only the process group started above, then its X server.
    if [[ $(ps -o pgid= -p "$app" 2>/dev/null | tr -d ' ') == "$app" ]]; then
        kill -TERM -- "-$app" 2>/dev/null || true
        for _ in $(seq 50); do kill -0 -- "-$app" 2>/dev/null || break; sleep 0.1; done
        kill -KILL -- "-$app" 2>/dev/null || true
    fi
    wait "$app" 2>/dev/null || true
    stop_display "$xvfb" "$wm"

    if (( status != 0 )); then
        printf -- '--- %s output ---\n' "$label" >&2
        grep -v 'Gtk-CRITICAL' "$home/app.log" | tail -n 40 >&2 || true
    else
        printf '%s: started, window shown (%s colors), second start handed over, screenshot %s\n' \
            "$label" "$colors" "$shots/$label.png"
    fi
    rm -rf -- "$home"
    return "$status"
}

# Prints the program the deb extracted at $1 starts from its menu entry. It
# must be installed there and must not share a name with one of the node's
# programs (binaries.yml), which may come first on PATH.
menu_program() {
    local binary node
    binary=$(sed -n 's/^Exec=//p' "$1/usr/share/applications/$product.desktop" 2>/dev/null | head -n 1)
    node=$(sed -n 's/^ *for bin in \([^;]*\);.*/\1/p' "$repository/.github/workflows/binaries.yml" | head -n 1)
    if [[ -z $binary || ! -x $1/usr/bin/$binary ]]; then
        printf 'deb: the menu entry does not start the installed app (%s)\n' "${binary:-no Exec}" >&2
        return 1
    fi
    if [[ -z $node ]]; then
        printf 'deb: the node programs are not listed in binaries.yml\n' >&2
        return 1
    fi
    if [[ " $node " == *" $binary "* ]]; then
        printf 'deb: %s is also the name of a node program\n' "$binary" >&2
        return 1
    fi
    printf '%s' "$binary"
}

# Calls every command of the app at $1 through WebDriver (see ipc_check.py).
check_commands() {
    local home display xvfb wm status=0
    home=$(app_home)
    display=$(free_display)
    start_display "$display"
    xvfb=$xvfb_pid
    wm=$wm_pid
    env HOME="$home" MMX_HOME="$home/node" XDG_CONFIG_HOME="$home/config" \
        XDG_DATA_HOME="$home/data" XDG_CACHE_HOME="$home/cache" XDG_RUNTIME_DIR="$home/run" \
        DISPLAY="$display" timeout 600 dbus-run-session -- \
        python3 -B "$wallet/src-tauri/tests/ipc_check.py" "$1" "$(command -v tauri-driver)" \
        >"$home/check.log" 2>&1 || status=1
    stop_display "$xvfb" "$wm"
    if (( status != 0 )); then
        printf -- '--- command check ---\n' >&2
        grep -vE 'Gtk-CRITICAL|dbus-daemon|^$' "$home/check.log" | grep -v '^ok ' | tail -n 60 >&2 || true
    else
        printf 'commands: %s checks passed%s\n' "$(grep -c '^ok ' "$home/check.log")" \
            "$(grep -q '^skip ' "$home/check.log" && printf ' (chain checks skipped: no network)')"
    fi
    rm -rf -- "$home"
    return "$status"
}

if (( launch_check )); then
    extract=$(mktemp -d)
    dpkg-deb -x "$packages/$deb_name" "$extract"
    python3 "$wallet/scripts/check-packaged-cli.py" "$extract/usr/bin/mmxwallet"
    python3 "$wallet/scripts/check-packaged-cli.py" "$packages/$appimage_name" --cli
    result=0
    if program=$(menu_program "$extract"); then
        printf 'deb: the menu entry starts %s\n' "$program"
        check_launch deb "$extract/usr/bin/$program" || result=1
    else
        program="" result=1
    fi
    check_launch appimage "$packages/$appimage_name" || result=1
    if [[ -n $program ]]; then
        check_commands "$extract/usr/bin/$program" || result=1
    fi
    rm -rf -- "$extract"
    (( result == 0 )) || exit 1
fi
ls -l "$packages/$deb_name" "$packages/$appimage_name"

#!/usr/bin/env python3
"""Checks the command interface of the built desktop app.

Starts the app through tauri-driver (WebDriver) on the X display in DISPLAY,
with the empty wallet directory in MMX_HOME, and calls the commands the way
the wallet UI does: valid requests, rejected requests, the file dialogs
(answered with xdotool), commands the window must not reach and navigation
away from the app. Chain requests need network access: when neither the app
nor this script can reach the chain server those checks are reported as
skipped, unless REQUIRE_CHAIN=1.

usage: ipc_check.py APP TAURI_DRIVER
"""
import ctypes
import http.client
import json
import os
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from webdriver_client import WebDriverClient

APP, DRIVER = sys.argv[1], sys.argv[2]
PRODUCT = "MMX Wallet"
CHAIN_SERVER = "https://rpc.mmx.network"
HOME = os.environ["MMX_HOME"]
PORT = 4444
ZERO = "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev"
PASSPHRASE = "correct horse"
# Valid 24-word phrases: a test wallet, and the all-zero seed (never accepted).
FIXTURE = ("cactus wedding mansion deal random arrive gate tissue bench party setup captain abstract "
           "mandate crouch lift anxiety embark achieve army expect account amount claim")
FIXTURE_FINGERPRINT = "2138401816"
FIXTURE_PASSPHRASE = "test 世界 🔑"
ZERO_SEED = " ".join(["abandon"] * 23 + ["art"])
ERROR_CODES = {
    "invalid_passphrase", "passphrase_required", "invalid_mnemonic", "wallet_mismatch",
    "wallet_exists", "wallet_not_found", "locked", "invalid_address", "invalid_amount",
    "invalid_memo", "insufficient_funds", "insufficient_fee", "invalid_argument",
    "rpc_unreachable", "rpc_timeout", "rpc_not_synced", "rpc_network_mismatch",
    "rpc_http_error", "rpc_response_invalid", "tx_rejected", "tx_expired", "tx_not_sent",
    "prepared_not_found", "io_error", "internal",
}
INVOKE = """
const done = arguments[arguments.length - 1];
window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(
  (value) => done({ok: true, value: value === undefined ? null : value}),
  (error) => done({ok: false, error: error === undefined ? null : error}));
"""
READ_CLIPBOARD = """
const done = arguments[arguments.length - 1];
navigator.clipboard.readText().then((text) => done({text}), (e) => done({error: String(e)}));
"""
START = """
window.__pending = window.__pending || {};
window.__pending[arguments[2] || 'call'] = window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]);
return true;
"""
FINISH = """
const done = arguments[arguments.length - 1];
window.__pending[arguments[0] || 'call'].then((value) => done({ok: true, value}), (error) => done({ok: false, error}));
"""
# Whether the page keeps the default action of an event, as the user would
# trigger it on the given element.
DEFAULT_KEPT = """
const [type, init, selector] = arguments;
let target = document.body;
if (selector) {
  target = document.createElement(selector);
  document.body.appendChild(target);
}
const event = type === 'keydown'
  ? new KeyboardEvent(type, {bubbles: true, cancelable: true, ...init})
  : new MouseEvent(type, {bubbles: true, cancelable: true, ...init});
target.dispatchEvent(event);
if (selector) target.remove();
return !event.defaultPrevented;
"""

failures = []
secrets = []
client = WebDriverClient("127.0.0.1", PORT)


def check(name, condition, detail=""):
    print(("ok    " if condition else "FAIL  ") + name + (f" ({detail})" if detail and not condition else ""))
    if not condition:
        failures.append(name)


def webdriver(method, path, body=None):
    return client.request(method, path, body)


def xdotool(*args):
    return subprocess.run(["xdotool", *args], capture_output=True, text=True).stdout.strip()


def wait_for_window(title):
    for _ in range(100):
        found = xdotool("search", "--onlyvisible", "--name", title).split()
        if found:
            return found[-1]
        time.sleep(0.1)
    return None


def transient_for(window):
    """The window a dialog belongs to (WM_TRANSIENT_FOR), or None."""
    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XGetTransientForHint.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(ctypes.c_ulong)]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = x11.XOpenDisplay(None)
    if not display:
        return None
    try:
        parent = ctypes.c_ulong()
        return parent.value if x11.XGetTransientForHint(display, int(window), ctypes.byref(parent)) else None
    finally:
        x11.XCloseDisplay(display)


def chain_server_reachable():
    """Whether this host reaches the app's default chain server at all."""
    try:
        with urllib.request.urlopen(f"{CHAIN_SERVER}/chain/info", timeout=15) as response:
            response.read(1)
        return True
    except urllib.error.HTTPError:
        return True  # it answered
    except (OSError, ValueError, http.client.HTTPException):
        return False


driver = subprocess.Popen([DRIVER, "--port", str(PORT)], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          env={**os.environ, "TAURI_WEBVIEW_AUTOMATION": "true"}, start_new_session=True)
session = None
try:
    # The driver accepts connections before its WebKit driver is ready.
    for attempt in range(30):
        try:
            session = webdriver("POST", "/session", {"capabilities": {"alwaysMatch": {
                "tauri:options": {"application": APP}}}})["sessionId"]
            break
        except (OSError, http.client.HTTPException):
            if attempt == 29:
                raise
            time.sleep(1)
    base = f"/session/{session}"
    webdriver("POST", f"{base}/timeouts", {"script": 90000})
    for _ in range(100):
        if webdriver("POST", f"{base}/execute/sync", {
                "script": "return document.readyState === 'complete' && !!window.__TAURI_INTERNALS__", "args": []}):
            break
        time.sleep(0.1)

    def call(command, args=None):
        return webdriver("POST", f"{base}/execute/async", {"script": INVOKE, "args": [command, args or {}]})

    def clipboard():
        result = webdriver("POST", f"{base}/execute/async", {"script": READ_CLIPBOARD, "args": []})
        return result.get("text")

    def ok(name, command, args=None):
        result = call(command, args)
        check(name, result["ok"], json.dumps(result.get("error"))[:200])
        return result.get("value")

    def fails(name, command, args, code):
        result = call(command, args)
        error = result.get("error")
        got = error.get("code") if isinstance(error, dict) else error
        check(name, not result["ok"] and got == code, f"got {json.dumps(result)[:200]}")
        if isinstance(error, dict):
            check(f"{name}: error shape", set(error) == {"code", "message"} and error["code"] in ERROR_CODES)

    def execute(script, *args):
        return webdriver("POST", f"{base}/execute/sync", {"script": script, "args": list(args)})

    def start(command, args, slot="call"):
        execute(START, command, args, slot)

    def finish(slot="call"):
        return webdriver("POST", f"{base}/execute/async", {"script": FINISH, "args": [slot]})

    def default_kept(type, init=None, element=None):
        return execute(DEFAULT_KEPT, type, init or {}, element)

    check("page loaded", execute("return document.title") == PRODUCT)
    rendered = ""
    for _ in range(100):
        rendered = webdriver("POST", f"{base}/execute/sync", {
            "script": "const a = document.getElementById('app'); return a ? a.innerText : ''", "args": []}) or ""
        if rendered.strip():
            break
        time.sleep(0.1)
    check("interface shown", bool(rendered.strip()))
    print("      first screen: " + " | ".join(rendered.split("\n")[:6])[:160])
    check("no wallets", ok("list wallets", "list_wallets") == [])
    state = ok("load app state", "load_app_state")
    check("default settings", state["settings"] == {"theme": "system", "autoLockMinutes": 5, "feeLevel": 1})
    check("nothing written before a change", not os.path.exists(HOME))

    # Wallets: new words first, the wallet is saved once they are confirmed.
    phrases = [ok(f"new recovery words {i + 1}", "new_recovery_phrase") for i in range(3)]
    secrets.extend(p for p in phrases if isinstance(p, str))
    check("24 new words", all(isinstance(p, str) and len(p.split()) == 24 for p in phrases))
    check("different words each time", len(set(phrases)) == 3)
    check("nothing saved before the words are confirmed",
          not os.path.exists(HOME) and ok("list before creating", "list_wallets") == [])
    mnemonic, other, unused = phrases
    main = ok("create wallet", "create_wallet", {"name": "Main", "mnemonic": mnemonic, "addressCount": 10})
    check("new wallet", main["name"] == "Main" and main["addressCount"] == 10 and not main["locked"]
          and not main["withPassphrase"] and main["id"] == f"wallet_{main['fingerprint']}.dat")
    key = os.path.join(HOME, main["id"])
    check("key file saved privately", os.path.isfile(key) and stat.S_IMODE(os.stat(key).st_mode) & 0o077 == 0
          and stat.S_IMODE(os.stat(HOME).st_mode) == 0o700)
    secured = ok("create wallet with passphrase", "create_wallet",
                 {"name": "Savings", "mnemonic": other, "passphrase": PASSPHRASE, "addressCount": 3})
    secrets.append(PASSPHRASE)
    check("passphrase wallet open after creation", secured["withPassphrase"] and not secured["locked"])
    fails("same words again", "create_wallet", {"name": "Again", "mnemonic": mnemonic, "addressCount": 1}, "wallet_exists")
    fails("words required", "create_wallet", {"name": "X", "addressCount": 1}, "invalid_argument")
    fails("invalid new words", "create_wallet", {"name": "X", "mnemonic": "abandon " * 24, "addressCount": 1}, "invalid_mnemonic")
    fails("all-zero seed refused for a new wallet", "create_wallet",
          {"name": "X", "mnemonic": ZERO_SEED, "addressCount": 1}, "invalid_mnemonic")
    fails("misspelled field rejected", "create_wallet",
          {"name": "X", "mnemonic": unused, "passPhrase": "p", "addressCount": 1}, "invalid_argument")
    fails("empty name rejected", "create_wallet", {"name": " ", "mnemonic": unused, "addressCount": 1}, "invalid_argument")
    fails("too many addresses rejected", "create_wallet",
          {"name": "X", "mnemonic": unused, "addressCount": 101}, "invalid_argument")
    check("refused wallets not saved", len(ok("list after refusals", "list_wallets")) == 2)
    fails("unsafe name rejected", "rename_wallet", {"id": main["id"], "name": "a‮b"}, "invalid_argument")
    fails("path as wallet id rejected", "get_addresses", {"id": "../" + main["id"]}, "invalid_argument")
    fails("unknown wallet", "get_addresses", {"id": "wallet_1.dat"}, "wallet_not_found")

    addresses = ok("addresses", "get_addresses", {"id": main["id"]})
    check("10 addresses", len(addresses) == 10 and all(re.fullmatch(r"mmx1[0-9a-z]{58}", a) for a in addresses))
    secured_addresses = ok("passphrase wallet addresses", "get_addresses", {"id": secured["id"]})
    check("recovery words", ok("reveal words", "reveal_mnemonic", {"id": main["id"]}) == mnemonic)

    ok("lock all", "lock_all", {"everywhere": True})
    listed = {w["id"]: w for w in ok("list after lock", "list_wallets")}
    check("passphrase wallet locked and observable, other open",
          listed[secured["id"]]["locked"] and listed[secured["id"]]["canView"] and not listed[main["id"]]["locked"])
    check("locked public addresses retained", ok("locked addresses", "get_addresses", {"id": secured["id"]}) == secured_addresses)
    check("regular wallet remains usable", ok("regular words after lock", "reveal_mnemonic", {"id": main["id"]}) == mnemonic)
    fails("locked words need authorization", "reveal_mnemonic", {"id": secured["id"]}, "passphrase_required")
    fails("locked spending refused", "prepare_send",
          {"walletId": secured["id"], "to": addresses[0], "contract": ZERO, "amount": "1000000", "feeLevel": 1}, "locked")
    fails("missing addresses need authorization", "set_address_count", {"id": secured["id"], "count": 4}, "passphrase_required")
    fails("wrong derivation passphrase", "set_address_count",
          {"id": secured["id"], "count": 4, "passphrase": "wrong"}, "invalid_passphrase")
    derived = ok("derive missing addresses", "set_address_count", {"id": secured["id"], "count": 4, "passphrase": PASSPHRASE})
    check("derived addresses extend the public cache", len(derived) == 4 and derived[:3] == secured_addresses)
    check("restore cached address count", ok("cached addresses need no authorization", "set_address_count",
                                            {"id": secured["id"], "count": 3}) == secured_addresses)
    check("authorized recovery words", ok("reveal authorized words", "reveal_mnemonic",
                                         {"id": secured["id"], "passphrase": PASSPHRASE}) == other)
    listed = {w["id"]: w for w in ok("list after derivation and reveal", "list_wallets")}
    check("derivation and reveal leave spending locked", listed[secured["id"]]["locked"])
    fails("temporary authorization not retained", "reveal_mnemonic", {"id": secured["id"]}, "passphrase_required")
    fails("passphrase required", "unlock_wallet", {"id": secured["id"]}, "passphrase_required")
    fails("wrong passphrase", "unlock_wallet", {"id": secured["id"], "passphrase": "wrong"}, "invalid_passphrase")
    fails("words not accepted on desktop", "unlock_wallet", {"id": secured["id"], "mnemonic": FIXTURE}, "invalid_argument")
    check("unlocked", not ok("unlock", "unlock_wallet", {"id": secured["id"], "passphrase": PASSPHRASE})["locked"])
    check("3 addresses", len(ok("addresses after unlock", "get_addresses", {"id": secured["id"]})) == 3)

    fails("same seed again", "import_wallet", {"name": "Again", "mnemonic": mnemonic.upper(), "addressCount": 10}, "wallet_exists")
    fails("invalid words", "import_wallet", {"name": "Bad", "mnemonic": "abandon " * 24, "addressCount": 10}, "invalid_mnemonic")
    fails("web field rejected", "import_wallet",
          {"name": "X", "mnemonic": FIXTURE, "addressCount": 10, "expectedFingerprint": "1"}, "invalid_argument")
    fails("all-zero seed refused", "import_wallet", {"name": "Zero", "mnemonic": ZERO_SEED, "addressCount": 10}, "invalid_mnemonic")
    imported = ok("import", "import_wallet",
                  {"name": "Imported", "mnemonic": FIXTURE, "passphrase": FIXTURE_PASSPHRASE, "addressCount": 100})
    secrets.append(FIXTURE_PASSPHRASE)
    check("imported with passphrase", imported["withPassphrase"] and not imported["locked"]
          and imported["fingerprint"] == FIXTURE_FINGERPRINT)
    check("100 addresses after import", len(ok("imported addresses", "get_addresses", {"id": imported["id"]})) == 100)

    ok("rename", "rename_wallet", {"id": main["id"], "name": "Daily"})
    more = ok("more addresses", "set_address_count", {"id": main["id"], "count": 12})
    check("12 addresses, same first 10", len(more) == 12 and more[:10] == addresses)
    ok("set active", "set_active_wallet", {"id": secured["id"]})
    names = {w["id"]: w["name"] for w in ok("list after rename", "list_wallets")}
    check("renamed", names.get(main["id"]) == "Daily" and len(names) == 3)

    # Labels, contacts and settings
    contact_address = ok("other addresses", "get_addresses", {"id": secured["id"]})[0]
    saved = {"activeWallet": main["id"], "labels": {main["id"]: {"0": "Rent"}},
             "contacts": [{"id": "c-1", "name": "Alice", "address": contact_address, "note": "lunch"}],
             "settings": {"theme": "dark", "autoLockMinutes": 30, "feeLevel": 2}}
    ok("save app state", "save_app_state", {"state": saved})
    # Only set_active_wallet changes the active wallet.
    check("app state round trip", ok("reload app state", "load_app_state") == dict(saved, activeWallet=secured["id"]))
    bad = dict(saved, contacts=[{"id": "c-2", "name": "Bob", "address": "mmx1bad"}])
    fails("invalid contact rejected", "save_app_state", {"state": bad}, "invalid_argument")
    with open(os.path.join(HOME, "mmxwallet.json")) as f:
        config = json.load(f)
    check("settings file", config.get("active_wallet") == secured["id"] and config["app"]["theme"] == "dark"
          and config["contacts"][0]["name"] == "Alice" and config["wallets"][main["id"]]["name"] == "Daily")

    # Chain data and transfers
    chain = call("rpc_get", {"path": "/chain/info"})
    online = chain["ok"]
    if online:
        check("chain info", isinstance(chain["value"], dict) and "network" in chain["value"])
        check("nothing to send", ok("max sendable", "max_sendable",
                                    {"walletId": main["id"], "to": "", "contract": ZERO, "feeLevel": 1}) == "0")
        fails("no funds", "prepare_send", {"walletId": main["id"], "to": contact_address, "contract": ZERO,
                                           "amount": "1000000", "feeLevel": 1}, "insufficient_funds")
        server = dict(saved, settings=dict(saved["settings"], rpcUrl="https://rpc.mmx.network/"))
        ok("choose a server", "save_app_state", {"state": server})
        check("server saved", ok("reload with server", "load_app_state")["settings"].get("rpcUrl") == "https://rpc.mmx.network")
        ok("default server again", "save_app_state", {"state": saved})
        check("server reset", "rpcUrl" not in ok("reload without server", "load_app_state")["settings"])
    else:
        # Skipped only when the chain server cannot be reached from here
        # either; any other failure is the app's.
        code = chain["error"].get("code") if isinstance(chain["error"], dict) else chain["error"]
        offline = code in ("rpc_unreachable", "rpc_timeout") and not chain_server_reachable()
        check("chain info (or no network)", offline and os.environ.get("REQUIRE_CHAIN") != "1", json.dumps(chain)[:200])
        if offline:
            print("skip  chain checks (no network)")
    unreachable = dict(saved, settings=dict(saved["settings"], rpcUrl="http://127.0.0.1:9"))
    fails("unreachable server not saved", "save_app_state", {"state": unreachable}, "rpc_unreachable")
    fails("server with credentials refused", "save_app_state",
          {"state": dict(saved, settings=dict(saved["settings"], rpcUrl="https://user:pw@rpc.mmx.network"))}, "invalid_argument")
    fails("endpoint not allowed", "rpc_get", {"path": "/wallet/keys"}, "invalid_argument")
    fails("query not allowed", "rpc_get", {"path": "/address?id=a;b"}, "invalid_argument")
    request = {"walletId": main["id"], "to": "mmx1invalid", "contract": ZERO, "amount": "1", "feeLevel": 1}
    fails("invalid recipient", "prepare_send", request, "invalid_address")
    fails("memo too long", "prepare_send", dict(request, to=contact_address, memo="x" * 65), "invalid_memo")
    fails("invalid amount", "prepare_send", dict(request, to=contact_address, amount="1.5"), "invalid_amount")
    fails("invalid fee level", "prepare_send", dict(request, to=contact_address, feeLevel=4), "invalid_argument")
    fails("nothing prepared", "broadcast", {"walletId": main["id"], "txid": "A" * 64}, "prepared_not_found")
    ok("discard", "discard_prepared", {"txid": "a" * 64})

    # System services
    fails("other links refused", "open_external", {"url": "https://example.com/"}, "invalid_argument")
    ok("copy", "copy_text", {"text": addresses[0]})
    check("clipboard holds the address", clipboard() == addresses[0])
    ok("copy words", "copy_text", {"text": mnemonic, "sensitive": True})
    copied_words = time.monotonic()
    check("clipboard holds the words", clipboard() == mnemonic)
    fails("copy too long", "copy_text", {"text": "x" * 20000}, "invalid_argument")

    contents = '{"format":"mmx-wallet-contacts","version":1,"contacts":[]}\n'
    main_window = wait_for_window(f"^{PRODUCT}$")

    def belongs_to_window(dialog):
        parent = transient_for(dialog)
        return parent is not None and xdotool("getwindowname", str(parent)) == PRODUCT

    def cancel(dialog):
        xdotool("windowfocus", "--sync", dialog)
        xdotool("key", "Escape")
        for _ in range(50):
            if dialog not in xdotool("search", "--onlyvisible", "--name", ".").split():
                return
            time.sleep(0.1)

    # A second request while a dialog is open waits for it, then shows its own.
    start("export_file", {"suggestedName": "../contacts.json", "contents": contents}, "first")
    dialog = wait_for_window("Save File")
    check("save dialog", dialog is not None)
    check("save dialog belongs to the window", dialog is not None and belongs_to_window(dialog))
    start("import_file", {}, "second")
    if dialog:
        cancel(dialog)
    result = finish("first")
    check("save cancelled", result == {"ok": True, "value": False}, json.dumps(result))
    dialog = wait_for_window("Open File")
    check("waiting request shows its dialog next", dialog is not None)
    check("open dialog belongs to the window", dialog is not None and belongs_to_window(dialog))
    if dialog:
        cancel(dialog)
    result = finish("second")
    check("open cancelled", result == {"ok": True, "value": None}, json.dumps(result))

    folder = tempfile.mkdtemp()
    target = os.path.join(folder, "contacts.json")
    start("export_file", {"suggestedName": "contacts.json", "contents": contents})
    dialog = wait_for_window("Save File")
    if dialog:
        xdotool("windowfocus", "--sync", dialog)
        xdotool("key", "ctrl+a")
        xdotool("type", "--delay", "15", target)
        xdotool("key", "Return")
    result = finish()
    check("saved", result == {"ok": True, "value": True}, json.dumps(result))
    check("file written privately", os.path.isfile(target) and open(target).read() == contents
          and stat.S_IMODE(os.stat(target).st_mode) & 0o077 == 0)

    start("import_file", {})
    dialog = wait_for_window("Open File")
    check("open dialog", dialog is not None)
    if dialog:
        xdotool("windowfocus", "--sync", dialog)
        xdotool("key", "ctrl+l")
        xdotool("type", "--delay", "15", target)
        # The first Return may only accept the location's completion.
        for _ in range(3):
            time.sleep(0.5)
            if dialog not in xdotool("search", "--onlyvisible", "--name", "Open File").split():
                break
            xdotool("windowfocus", "--sync", dialog)
            xdotool("key", "Return")
    result = finish()
    check("file read", result == {"ok": True, "value": contents}, json.dumps(result)[:200])

    # The web view's browser functions are kept out of the window; text
    # fields keep their context menu, zoom keys stay.
    for name, init in [("F5", {"key": "F5"}), ("Ctrl+R", {"key": "r", "code": "KeyR", "ctrlKey": True}),
                       ("Ctrl+Shift+R", {"key": "R", "code": "KeyR", "ctrlKey": True, "shiftKey": True}),
                       ("Ctrl+P", {"key": "p", "code": "KeyP", "ctrlKey": True}),
                       ("Ctrl+S", {"key": "s", "code": "KeyS", "ctrlKey": True}),
                       ("Ctrl+U", {"key": "u", "code": "KeyU", "ctrlKey": True}),
                       ("Ctrl+R (Cyrillic layout)", {"key": "к", "code": "KeyR", "ctrlKey": True}),
                       ("Alt+Left", {"key": "ArrowLeft", "altKey": True}),
                       ("BrowserBack", {"key": "BrowserBack"})]:
        check(f"{name} blocked", not default_kept("keydown", init))
    for name, init in [("typing", {"key": "r", "code": "KeyR"}), ("Ctrl+C", {"key": "c", "code": "KeyC", "ctrlKey": True}),
                       ("Ctrl+V", {"key": "v", "code": "KeyV", "ctrlKey": True}),
                       ("Ctrl+=", {"key": "=", "code": "Equal", "ctrlKey": True}),
                       ("Ctrl+0", {"key": "0", "code": "Digit0", "ctrlKey": True})]:
        check(f"{name} kept", default_kept("keydown", init))
    # (Option+arrow moves by word in macOS text fields only.)
    check("Alt+Left blocked in a text field", not default_kept("keydown", {"key": "ArrowLeft", "altKey": True}, "input"))
    check("page context menu blocked", not default_kept("contextmenu"))
    check("text field context menu kept", default_kept("contextmenu", None, "input"))
    check("back mouse button blocked", not default_kept("mouseup", {"button": 3}))

    # Ctrl + and Ctrl 0 zoom the page (zoom keys and their permission).
    width = execute("return window.innerWidth")
    if main_window:
        xdotool("windowfocus", "--sync", main_window)
        xdotool("key", "ctrl+equal")
    zoomed = width
    for _ in range(30):
        zoomed = execute("return window.innerWidth")
        if zoomed < width:
            break
        time.sleep(0.1)
    check("Ctrl + zooms in", zoomed < width, f"{width} -> {zoomed}")
    if main_window:
        xdotool("windowfocus", "--sync", main_window)
        xdotool("key", "ctrl+0")
    for _ in range(30):
        zoomed = execute("return window.innerWidth")
        if zoomed == width:
            break
        time.sleep(0.1)
    check("Ctrl 0 resets the zoom", zoomed == width, f"{width} -> {zoomed}")

    # Nothing else is reachable from the window.
    for command in ["plugin:clipboard-manager|read_text", "plugin:clipboard-manager|write_text",
                    "plugin:dialog|open", "plugin:dialog|message", "plugin:opener|open_url",
                    "plugin:window-state|save_window_state", "plugin:window|close", "plugin:webview|print",
                    "plugin:webview|set_webview_size", "plugin:webview|webview_close",
                    "plugin:app|version", "plugin:event|listen", "plugin:path|resolve_directory",
                    "plugin:fs|read_file", "plugin:wallet-session|x", "not_a_command"]:
        result = call(command, {"text": "x", "url": "https://example.com/", "path": "/etc/passwd"})
        check(f"{command} refused", not result["ok"], json.dumps(result)[:200])

    webdriver("POST", f"{base}/execute/sync", {"script": "location.href = 'https://example.com/'; return true", "args": []})
    webdriver("POST", f"{base}/execute/sync", {"script": "window.open('https://example.com/'); return true", "args": []})
    time.sleep(1.5)
    check("stays in the app", webdriver("GET", f"{base}/url").startswith("tauri://localhost"))
    check("no new windows", len(webdriver("GET", f"{base}/window/handles")) == 1)

    time.sleep(max(0.0, copied_words + 62 - time.monotonic()))
    check("words cleared from the clipboard after a minute", clipboard() in ("", None))

    ok("lock", "lock_wallet", {"id": secured["id"]})
    ok("remove", "remove_wallet", {"id": imported["id"]})
    removed = os.listdir(os.path.join(HOME, "removed"))
    check("key file kept aside", len(removed) == 1 and removed[0].startswith(imported["id"] + "."))
    check("two wallets left", len(ok("list after remove", "list_wallets")) == 2)
except Exception as e:
    check("checks completed", False, repr(e)[:300])
finally:
    if session:
        try:
            webdriver("DELETE", f"/session/{session}")
        except Exception as e:  # the app may already be gone
            print(f"note  {e}")
    client.close()
    # Stop only the driver's own process group (driver, WebKitWebDriver, app).
    try:
        os.killpg(driver.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        output, _ = driver.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(driver.pid, signal.SIGKILL)
        output, _ = driver.communicate()

log = output.decode(errors="replace")
check("no panic", "panicked" not in log)
if failures:
    print("--- app and driver output ---\n" + "\n".join(log.splitlines()[-40:]))
for secret in secrets:
    check("no secret in the app output", secret not in log and secret.split()[0] + " " + secret.split()[-1] not in log)
print(f"{len(failures)} failed" if failures else "all checks passed")
sys.exit(1 if failures else 0)

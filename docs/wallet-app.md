# MMX Wallet (desktop + web) — engineering specification

This is the implementation contract for the MMX desktop wallet (Tauri) and the
hosted web wallet. Both share one UI. It is a developer document; nothing here
is shown in the product UI.

## 1. Goals

- Finished end-user product: send/receive with optional memo, multiple wallets
  (switching), multiple addresses per wallet (default 10, configurable 1–100),
  public RPC for chain data, import/export via 24 recovery words, transaction
  history, balance history, address book with labels (send again, identify
  senders), wallet creation, passphrase support with fingerprint-based detection
  of "locked" and "wrong passphrase".
- Desktop: Windows (NSIS installer), Linux (AppImage + deb), macOS (dmg,
  universal). Built on the `mmxwallet` crate (same wallet directory, key file
  format and `mmxwallet.json` as the CLI and the C++ node GUI).
- Web: static site, seed stored in the user's password manager, seed and keys
  never leave the browser, same features and look.
- Consensus-critical code (key derivation, fingerprint, mnemonic, address
  encoding, transaction hashing/signing, fee cost) exists exactly once, in Rust
  (`crates/mmx-wallet`). Desktop links it natively; web runs it as WASM.

## 2. Repository layout

```
crates/mmx-wallet/        Core. Feature `native` (default) = consensus record bridge
                          (native.rs, spend.rs, mmx-consensus/mmx-types/vnx deps).
                          Without it: pure Rust, builds for wasm32-unknown-unknown.
crates/mmx-wallet-wasm/   wasm-bindgen API over mmx-wallet (default-features = false).
crates/mmxwallet/         CLI + library. `mmxwallet::service` is the desktop backend.
apps/wallet/              Shared UI (Preact + TypeScript + Vite).
  src/backend/            types.ts (Backend contract), desktop.ts, web/*, mock.ts (tests only)
  src/chain/              RPC client over a transport, history cache, balances, tx details
  src/lib/                amounts (bigint), address check, formatting, grouping, series, qr
  src/state/              signals store, actions, refresh scheduler, idle lock
  src/ui/                 components/, screens/, icons.ts
  src/styles/             base.css (reset, layout, components), themes/<id>/theme.css
  src-tauri/              Tauri 2 crate (own [workspace]; thin bindings to mmxwallet::service)
  tests/unit/             vitest
  tests/e2e/              Playwright (web build + mock RPC server); gallery screenshots
  deploy/                 static hosting headers (Caddy, nginx, Netlify/Cloudflare `_headers`)
  scripts/                build-wasm.sh, build-linux-container.sh, build-macos.sh, build-windows.ps1
.github/workflows/wallet.yml   CI: tests, web build, desktop bundles for 3 OSes
```

`apps/wallet/src-tauri` is NOT a member of the root workspace (it needs
webkit2gtk system libraries); it is built in a container on Linux and natively
on macOS/Windows CI. All logic that needs testing lives in `mmxwallet::service`
so it is tested by `cargo test -p mmxwallet` on the host.

Build rules (AGENTS.md): `CARGO_INCREMENTAL=0`, debug symbols off, `--jobs 2`,
`nice -n 19 ionice -c 3`. Use a dedicated target dir for wallet work:
`CARGO_TARGET_DIR=target/wallet` (host) to avoid lock contention with node builds.

## 3. Core crate changes (`crates/mmx-wallet`)

1. Feature `native` (default on). Gate `native.rs`, `spend.rs`, the
   `Error::Native` variant, `Balances`(keep the type alias ungated — it is a plain
   BTreeMap) and the mmx-consensus/mmx-types/vnx dependencies behind it.
   `cargo build -p mmx-wallet --no-default-features --target wasm32-unknown-unknown`
   must succeed. Existing tests keep passing with default features.
2. `Wallet::max_transfer_amount(&self, balances, params, currency, memo: Option<&str>, fee_ratio) -> Result<u128>`:
   - Native currency: let `fee(k) = static cost with k inputs, 1 output (with memo), k
     signatures (sender is one of the inputs)` × fee_ratio / 1024. Let `k` = number of
     owned addresses whose native balance ≥ fee(k) (iterate: compute with candidate k,
     shrink until consistent). Result = sum of those k balances − fee(k). Returns 0 if
     k = 0 or the sum ≤ fee. The result MUST be accepted by `transfer` with the same
     params and leave the fee payer (address with the largest remaining native
     balance) holding ≥ fee: prove with tests over random balance sets (incl. balances
     below the fee, ties, a single address, memo of 1, 32, 33, 64 bytes).
   - Token: full token balance across owned addresses; error
     `InsufficientFunds("transaction fee")` when no owned address holds ≥ the fee in
     native currency.
3. `Wallet::transfer` stays consensus-identical (keep `transactions_match_cpp`).
4. Helper `pub fn static_fee(params, inputs, owners, memo) -> u128` used by the above
   and by the UI fee preview, matching `Transaction::calc_cost` exactly.

## 4. WASM API (`crates/mmx-wallet-wasm`)

wasm-bindgen 0.2.127 (matches Cargo.lock and the installed `wasm-bindgen` CLI),
`getrandom 0.2` with `js` feature. Output: `wasm-bindgen --target web` into
`apps/wallet/src/backend/web/pkg/` (generated, gitignored; `npm run build:wasm`).
All errors are thrown as `JsError` whose message is a stable code from §7.

```
generate_mnemonic() -> String                       // 24 words from OS RNG
normalize_mnemonic(words) -> String                 // lowercases, collapses whitespace, validates; throws invalid_mnemonic
fingerprint(words, passphrase: Option<String>) -> String
validate_address(s) -> bool
parse_amount(text, decimals) -> String              // atomic units, throws invalid_amount
format_amount(atomic: String, decimals) -> String
static_fee(params_json, inputs, owners, memo: Option<String>, fee_level) -> String
class Vault {
  // Opens a seed. expected_fingerprint: the wallet's known fingerprint (or None for a
  // first import). Detection rules (§6.3). Seed, passphrase and keys live only inside
  // WASM memory and are zeroized on free()/drop.
  static open(words, passphrase: Option<String>, expected_fingerprint: Option<String>, count) -> Vault
  fingerprint() -> String
  with_passphrase() -> bool
  addresses() -> Vec<String>
  set_count(count)                                  // 1..=100, re-derives
  mnemonic() -> String
  max_amount(balances_json, params_json, currency, memo: Option<String>, fee_level) -> String
  sign_transfer(balances_json, params_json, height, to, currency, amount_atomic, memo: Option<String>, fee_level, expire_delta) -> String  // signed tx JSON (same as mmxwallet)
  free()
}
verify_transaction(tx_json, params_json)            // throws on any failure
```

`balances_json`: `[{"address":"mmx1…","contract":"mmx1…","amount":"123"}]`.
`params_json`: the raw `/chain/info` object. `fee_level` ∈ {1,2,3,5,10} →
`fee_ratio = level × 1024`. Parity test: WASM (run under Node from vitest) and
native Rust produce byte-identical signed JSON for fixed nonce (add a hidden
`sign_transfer_with_nonce` behind a cargo feature `test-nonce` used only by tests)
and identical fingerprints/addresses/mnemonics for the fixtures in
`crates/mmx-wallet/tests/fixtures/reference.json`.

## 5. `mmxwallet::service` (desktop backend)

`pub struct Service` — `Send + Sync`, shared via `Arc`. Holds the wallet directory,
a `Mutex` for config writes, unlocked passphrases (`Zeroizing<String>`, keyed by
wallet id), prepared transactions (id → wallet id, signed bytes, created_at; dropped
after 10 minutes or on use). Blocking (ureq); the Tauri layer calls it from
`spawn_blocking`.

Wallet id = key file name inside the wallet directory (`wallet.dat`,
`wallet_<fp>.dat`, `mmxwallet_*.dat`). Every id received from the UI is validated:
plain file name, no separators, matches the wallet patterns, exists in the listing.

```
Service::open(dir: Option<PathBuf>) -> Result<Service>        // default storage::directory()
wallets() -> Vec<WalletInfo>        // unreadable files are listed with `error` instead of failing the list
create(name, passphrase: Option<Zeroizing<String>>, count) -> (WalletInfo, Zeroizing<String> mnemonic)
import(name, mnemonic: Zeroizing<String>, passphrase, count) -> WalletInfo
unlock(id, passphrase: Zeroizing<String>) -> WalletInfo      // invalid_passphrase on fingerprint mismatch
lock(id) / lock_all()
rename(id, name)                    // 1..=64 chars after trim, no control chars
set_address_count(id, n)            // 1..=100
remove(id)                          // moves the file to <dir>/removed/<id>.<unix-ts> (never deletes)
mnemonic(id) -> Zeroizing<String>
addresses(id) -> Vec<Address>       // locked → error `locked`
set_active(id)
app_state() -> AppState / set_app_state(AppState)            // contacts, labels, settings (validated)
rpc_get(path) -> serde_json::Value  // allowlist: /node/info /chain/info /address /balance
                                    // /address/history /transaction; query chars [A-Za-z0-9=&_-]
set_rpc(url) / rpc() -> String      // validated by Rpc::new + /node/info probe
max_send(id, currency, memo, fee_level) -> String
prepare_send(id, SendRequest) -> PreparedSend   // fresh balances (remote::State), transfer, /transaction/validate
broadcast(id, txid)                 // posts the prepared bytes once; removes it
discard(txid)
```
Desktop file export/import (contacts) uses the Tauri dialog plugin from Rust
commands; web uses a Blob download and a file input.

Empty passphrase handling: a key file whose fingerprint equals
`fingerprint(seed, Some(""))` unlocks automatically (no prompt). New wallets never
store an empty passphrase (empty input = no passphrase).

`mmxwallet.json` (existing file; unknown keys preserved, atomic writes):

```json
{
  "active_wallet": "wallet_123.dat",
  "rpc": "https://rpc.mmx.network",
  "wallets": { "wallet_123.dat": { "name": "Savings", "num_addresses": 10,
                                   "labels": { "0": "Main" } } },
  "contacts": [ { "id": "c-…", "name": "Alice", "address": "mmx1…", "note": "" } ],
  "app": { "theme": "system", "auto_lock_minutes": 15, "fee_level": 1 }
}
```

CLI changes: default `--num-addresses` = the wallet's `num_addresses` from the
config, else 10; max 100 everywhere (`capabilities.max_num_addresses`); default
`--rpc` = config `rpc` if set; `list` shows names; new commands `rename <NAME>` and
`set-addresses <N>` (both honour `--wallet`); a corrupt key file no longer breaks
`list`. Keep JSON output schema backwards compatible (only add fields). Update
`tools/test_wallet_cli.py` and the help text.

## 6. Shared UI

### 6.1 Stack
Preact 10 + @preact/signals, TypeScript strict, Vite 7, vitest, Playwright 1.63
(chromium-1243 is cached locally; do not download browsers). Runtime deps are
limited to: preact, @preact/signals, @tauri-apps/api + the tauri plugins used,
`uqr` (QR), @fontsource/* font packages. Exact versions, `package-lock.json`, `npm ci`.
No CDN, no analytics, no telemetry, no remote fonts, no service worker.

### 6.2 Backend contract (`src/backend/types.ts`)

```ts
type WalletId = string;            // desktop: key file name; web: fingerprint
interface WalletInfo { id; name; fingerprint; withPassphrase: boolean; locked: boolean;
                       addressCount: number; error?: string }
interface Contact { id: string; name: string; address: string; note?: string }
interface AppState { activeWallet?: WalletId;
  labels: Record<WalletId, Record<number, string>>;  contacts: Contact[];
  settings: { theme: 'system'|'light'|'dark'; autoLockMinutes: number /*0=never, desktop only*/;
              feeLevel: 1|2|3|5|10; rpcUrl?: string /*desktop only*/ } }
interface SendRequest { walletId; to: string; contract: string; amount: string /*atomic*/;
                        memo?: string; feeLevel: number }
interface PreparedSend { txid; to; contract; symbol; decimals; amount; fee; expires; height; memo? }
class BackendError extends Error { code: ErrorCode }

interface Backend {
  readonly platform: 'desktop' | 'web';
  loadAppState(): Promise<AppState>; saveAppState(s: AppState): Promise<void>;
  listWallets(): Promise<WalletInfo[]>;
  createWallet(r: {name; passphrase?; addressCount}): Promise<{wallet: WalletInfo; mnemonic: string}>;
  importWallet(r: {name; mnemonic; passphrase?; addressCount}): Promise<WalletInfo>;
  unlockWallet(id, r: {passphrase?: string; mnemonic?: string}): Promise<WalletInfo>;
  lockWallet(id): Promise<void>; lockAll(): Promise<void>;
  renameWallet(id, name): Promise<void>;
  setAddressCount(id, n): Promise<string[]>;
  removeWallet(id): Promise<void>;
  revealMnemonic(id): Promise<string>;
  getAddresses(id): Promise<string[]>;
  setActiveWallet(id): Promise<void>;
  rpcGet(path: string): Promise<unknown>;              // web: fetch; desktop: service.rpc_get
  maxSendable(r: Omit<SendRequest,'amount'>): Promise<string>;
  prepareSend(r: SendRequest): Promise<PreparedSend>;
  broadcast(walletId, txid): Promise<void>;
  discardPrepared(txid): Promise<void>;
  copyText(text: string, sensitive?: boolean): Promise<void>;   // sensitive → cleared after 60 s
  openExternal(url: string): Promise<void>;                     // explorer links only
  exportFile(suggestedName: string, contents: string): Promise<boolean>; // false = cancelled
  importFile(): Promise<string | null>;                          // JSON ≤ 1 MiB, null = cancelled
}
```

### 6.3 Passphrase detection (both platforms)
A wallet's stored fingerprint is `fingerprint(seed, passphrase)` if created with a
passphrase, otherwise `fingerprint(seed, None)`.
- `fingerprint(seed, None) == stored` → no passphrase; never prompt.
- else `fingerprint(seed, Some("")) == stored` → unlock silently.
- else → wallet is locked; prompt for the passphrase; `fingerprint(seed, Some(p)) ==
  stored` → unlocked, otherwise `invalid_passphrase` ("Wrong passphrase").
- Web, unknown fingerprint (first open of a saved credential on a new device): see 6.6.

### 6.4 Chain layer (`src/chain`)
- `Rpc` over `backend.rpcGet`; mirrors mmxwallet validation: `/node/info` must be
  synced and its `name` must equal `/chain/info.network`; amounts are exact strings →
  `bigint`; decimals ≤ 18; native currency = all-zero address
  (`mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev`) only — never by
  symbol. Concurrency limit 4 requests, in-flight dedupe of identical GETs, 20 s
  timeout, exponential backoff when unreachable.
- Balances: `/address?id=…&limit=1000` per address (skip `is_nft`). For addresses
  holding non-native tokens also `/balance?id=…` to read `is_validated`. Total per
  contract plus per-address amounts.
- History cache per wallet: per address, a contiguous fully-fetched height range.
  `until` and `since` are inclusive; the node completes the last block of a page, so
  next `until = minHeight − 1`. Merge pages across addresses: rows above the highest
  "page-full" boundary are complete. First page 25 rows per address, "load more" and
  balance-history coverage extend downward (1000 per request, cap 20 000 rows per
  wallet). Refresh uses `since = topHeight + 1`. The public RPC does not return
  pending entries; broadcast transactions are tracked locally as pending via
  `/transaction?id=` until included (confirmed / failed) or `expires < height`.
- Grouping: one activity item per txid: net amount per contract over the wallet's own
  addresses; `TXFEE` shown as fee; SPEND+RECEIVE between own addresses with zero net
  → "Moved between your addresses"; REWARD → "Farming reward", VDF_REWARD → "Timelord
  reward", PROJECT_REWARD → "Project reward".
- Counterparty: lazily `/transaction?id=` for visible items only (LRU 500, in memory):
  received → `sender` (else first foreign input); sent → first foreign output.
  Shown as contact name, own-address label, or shortened address.
- Balance history: per contract (default MMX), walk confirmed rows newest→oldest from
  the current confirmed balance (RECEIVE/REWARD/VDF_REWARD/PROJECT_REWARD add, SPEND
  and TXFEE subtract) producing step points; ranges 1W, 1M, 3M, 1Y, All.
- Refresh: poll `/node/info` every 10 s while visible (30 s hidden/backgrounded). On
  a new height, at most every 20 s: history `since` for each address; balances for
  addresses with new rows (all addresses every 5 min); pending statuses.

### 6.5 Screens
Shell: wallet switcher (name; list of wallets, add wallet), navigation (Overview,
Send, Receive, Activity, Contacts, Settings), network status, lock (when lockable).
Mobile (<720 px, web): bottom tab bar Overview/Send/Receive/Activity/More.

- Onboarding: Create new wallet / Import with recovery phrase (web adds: Open a wallet
  saved in your password manager).
- Create: name → optional passphrase (confirm twice, warning it cannot be recovered)
  → recovery phrase (24 numbered words, copy with auto-clear) → verify (desktop: pick
  words #a, #b, #c from shuffled options; web: §6.6) → done.
- Import: name, 24 words (per-word validation and suggestions from the BIP39 list,
  paste splits), optional passphrase. After import scan up to 100 addresses
  (`/address/history?limit=1`, concurrency 4) and set the address count to
  max(10, last used index + 1).
- Overview: total MMX (large), pending outgoing, verified token list, "other tokens"
  (unverified, collapsed, warning icon, symbol + short contract), balance chart with
  ranges, recent activity (5), Send/Receive actions.
- Send: To (address or contact search; shows the contact name on match; invalid
  address error inline), Asset (MMX + tokens with balance), Amount (Max button,
  available balance shown), Memo (optional, max 64 UTF-8 bytes; counter appears near
  the limit). Review: recipient (full address, chunked into groups of 4 for checking,
  contact name if known), amount, network fee with priority selector (Standard 1×,
  Faster 2×, 3×, 5×, Fastest 10× from `min_fee_ratio`), total, memo. Locked wallet →
  passphrase prompt first. Confirm → broadcast → result (txid copy, explorer, "Save
  recipient to contacts" when not a contact, Done). Transport failure after send →
  "We couldn't confirm the transfer. Check Activity before sending again." (no
  automatic retry).
- Receive: QR + address + copy, address picker (label, index, balance), label edit,
  table of all addresses.
- Activity: date-grouped list, filter All/Received/Sent/Rewards and asset, load more;
  detail panel: status, confirmations, amount, fee, counterparty (+ save to contacts),
  memo, txid (copy), height, time, explorer link.
- Contacts: search, add/edit/delete (name ≤ 64, valid address, unique address), send
  to, copy; export/import JSON (validated, merge by address).
- Settings: wallet (name, fingerprint, address count 1–100 with warning when hiding
  addresses that hold funds, show recovery phrase, remove wallet); security (auto-lock
  5/15/30/60 min, desktop also Never; Lock now); network (desktop: RPC server URL,
  test + reset to default); appearance (System/Light/Dark); about (version).

Copy rules: short, task-focused, second person. No implementation details, no
"WASM", "Tauri", "RPC" (say "network"), no "fingerprint" jargon outside wallet
details ("Wallet ID"). Errors say what happened and what to do, in one line.

Amounts: bigint only. Display with the user's locale grouping and decimal
separator, trailing zeros trimmed, tabular numerals. Input accepts digits and the
locale's decimal separator only; the other separator or grouping characters → inline
error; scientific notation rejected; more fractional digits than `decimals` → error.

### 6.6 Web wallet: password manager vault
- Credential: username = `<wallet name> (<fingerprint>)`, password = the 24 words
  (lowercase, single spaces). Never stored anywhere else; never in localStorage,
  sessionStorage, IndexedDB, URLs, logs or error messages.
- Save: a real `<form>` with `autocomplete="username"` + `autocomplete="new-password"`
  fields; on submit: `preventDefault`, call `navigator.credentials.store(new
  PasswordCredential(...))` when available (Chromium), then route away and remove the
  form from the DOM (triggers save prompts in Safari and password manager extensions).
- Verify (completes creation): the new wallet opens through the normal unlock form,
  filled by the password manager (or typed words). Until verified the wallet is
  marked unverified on this device and the user is returned to this step.
- Unlock: wallets known on this device are listed; selecting one shows a form with
  the username prefilled (readonly) and `autocomplete="current-password"`; Chromium
  also tries `navigator.credentials.get({password: true, mediation: 'optional'})`.
  "Open another wallet" shows username + password fields empty. Passphrase is asked
  in a separate step (`autocomplete="off"`, `data-1p-ignore`, `data-lpignore`,
  `data-bwignore`) only when §6.3 says locked; on a new device the fingerprint from
  the username is the expected fingerprint; if the username has none, the passphrase
  step is shown as optional.
- After unlock the password input value is cleared; the seed is held only inside the
  WASM `Vault`. Lock / auto-lock (default 15 min idle, also on `pagehide`) frees every
  Vault and clears unlocked UI state.
- localStorage key `mmx-wallet/v1`: `{ wallets: { [fp]: { name, withPassphrase,
  addressCount, verified } }, activeWallet, labels, contacts, settings }`; sync across
  tabs via the `storage` event.
- Hosting: relative base (`./`), CSP via headers and `<meta>`:
  `default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self';
  font-src 'self'; img-src 'self' data:; connect-src https://rpc.mmx.network;
  manifest-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none';
  frame-ancestors 'none'` plus `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `Permissions-Policy` (all off),
  `Cross-Origin-Opener-Policy: same-origin`, HSTS. RPC URL is fixed at build time
  (`VITE_RPC_URL`, default `https://rpc.mmx.network`).

### 6.7 Desktop specifics
Tauri 2 (2.12.x). Plugins: clipboard-manager, opener (explorer URLs only:
`https://explore.mmx.network/#/explore/(transaction|address)/…`), window-state,
single-instance. `withGlobalTauri: false`; CSP `default-src 'self'; script-src
'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src ipc:
http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none';
frame-src 'none'`. Capabilities grant only the app's own commands and the plugin
permissions actually used. Commands map 1:1 to `Service` methods and return
`{code, message}` errors. Identifier `network.mmx.wallet`, product name "MMX Wallet",
min window 900×620.

### 6.8 Security invariants (review checklist)
- No secret (words, passphrase, seed, key) in logs, errors, panics, URLs, storage
  other than the key file (desktop) / password manager (web), or the clipboard
  without auto-clear.
- All RPC-provided text (memo, symbol, names) rendered as text, control and bidi
  override characters stripped, length-capped. No `innerHTML`/`dangerouslySetInnerHTML`.
- Native MMX identified only by the zero contract address.
- Every value from the UI is re-validated in Rust/WASM before signing.
- The transaction shown on the review screen is exactly the one broadcast
  (prepared id bound to wallet, amount, recipient, memo, fee).
- No automatic re-broadcast; no duplicate submission on double click.

## 7. Error codes
`invalid_passphrase, passphrase_required, invalid_mnemonic, wallet_mismatch (web: the
phrase does not belong to the selected wallet), wallet_exists,
wallet_not_found, locked, invalid_address, invalid_amount, invalid_memo,
insufficient_funds, insufficient_fee, invalid_argument, rpc_unreachable, rpc_timeout,
rpc_not_synced, rpc_network_mismatch, rpc_http_error, rpc_response_invalid,
tx_rejected, tx_expired, prepared_not_found, io_error, internal`.

## 8. Themes (10 styles)
Each style lives in `src/styles/themes/<id>/theme.css` (+ its fonts) and defines the
token contract for light and dark (`:root[data-scheme=light|dark]`), and may restyle
any component class. Styles never change markup or behaviour. The active style is
chosen at build time (`VITE_WALLET_STYLE`); in dev builds `?style=<id>` overrides it
for screenshots. Token contract: `--font-ui, --font-display, --font-mono,
--bg, --bg-elevated, --bg-sunken, --surface, --surface-2, --border, --border-strong,
--text, --text-muted, --text-faint, --accent, --accent-contrast, --accent-soft,
--positive, --negative, --warning, --focus, --radius-s/m/l, --shadow-1/2,
--chart-line, --chart-fill, --chart-grid, --space-unit, --nav-width,
--layout (sidebar|rail|topbar)`.

## 9. Testing
- Rust: core max-amount property tests; service tests with a local mock RPC server
  (create/import/unlock/wrong passphrase/remove/config/send/broadcast/allowlist);
  CLI python tests.
- WASM parity under Node (vitest): fixtures + signed tx byte equality.
- TS unit (vitest): amounts/locale parsing, address check vectors, history merge and
  pagination edges, grouping, balance series, persisted-state validation.
- E2E (Playwright, web build + mock RPC): create→save→verify, import with passphrase,
  wrong passphrase, switch wallets, receive, send review/broadcast, pending→confirmed,
  activity/detail/save contact, contacts CRUD, settings (address count, rename,
  remove), auto-lock, mobile viewport.
- Desktop: container build of deb + AppImage; launch smoke test under Xvfb.

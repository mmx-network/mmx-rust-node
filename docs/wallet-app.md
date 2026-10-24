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
2. `Wallet::max_transfer_amount(&self, balances, params, currency, memo: Option<&str>, fee_ratio) -> Result<u128>`
   (`fee.rs`) returns the largest amount that `transfer` accepts while its fee payer
   keeps the fee. It reuses `transfer`'s own input selection (`select`: owned balances
   of the currency, largest first, the last input possibly partial; fee payer = owned
   address with the largest native balance left after its inputs) and fee check
   (`covers_fee`: fee payer's native balance ≥ its own native input +
   `static_fee(inputs, owners, memo) × fee_ratio / 1024`, static cost ≤ u32::MAX).
   Only owned, nonzero balances of the currency and of MMX are considered.
   - Native currency: sort the owned MMX balances descending; for each prefix of the
     j largest (sum `P_j`, next balance `n`, `fee_j` = fee with j inputs and j
     owners) the candidates are `P_j − keep` for keep ∈ {0, 1, fee_j, n, n + 1}
     (all of them, or all minus the smallest remainder that leaves the last input
     able to pay). Candidates are tried from the largest down; the first one that
     selection + `covers_fee` accept is the result, else 0. Property tests over
     random balance sets (balances below the fee, ties, a single address, memos of
     1, 32, 33, 64 bytes) check that `transfer` accepts the result.
   - Token: the whole owned token balance if `covers_fee` accepts it, else
     `InsufficientFunds("transaction fee")`; 0 for no balance.
   - Errors: memo > 64 bytes or fee_ratio < 1024 → `InvalidArgument`.
   The balances passed in are fresh chain balances less the wallet's own pending
   transfers (§6.4 Reservations).
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
new_mnemonic() -> Zeroizing<String>  // 24 words (OsRng via KeyFile::generate); stores nothing
create(name, mnemonic: Zeroizing<String>, passphrase, count) -> WalletInfo
                                    // after the backup check; writes the key file, unlocks it,
                                    // makes it active; wallet_exists if the fingerprint exists
import(name, mnemonic: Zeroizing<String>, passphrase, count) -> WalletInfo
unlock(id, passphrase: Zeroizing<String>) -> WalletInfo      // invalid_passphrase on fingerprint mismatch
lock(id) / lock_all()
rename(id, name)                    // 1..=64 chars after trim, no control chars
set_address_count(id, n)            // 1..=100
remove(id)                          // moves the file to <dir>/removed/<id>.<unix-ts> (never deletes)
mnemonic(id) -> Zeroizing<String>
addresses(id) -> Vec<Address>       // locked → error `locked`
set_active(id)
app_state() -> AppState / set_app_state(AppState)            // contacts (≤ 1000), labels, settings (validated);
                                    // set_app_state never writes active_wallet (only set_active does)
rpc_get(path) -> serde_json::Value  // allowlist: /node/info /chain/info /address
                                    // /address/history /transaction; query chars [A-Za-z0-9=&_-]
set_rpc(url) / rpc() -> String      // validated by Rpc::new + /node/info probe before it is saved
max_send(id, currency, memo, fee_level) -> String   // fresh balances less own pending transfers
prepare_send(id, SendRequest) -> PreparedSend
            // fresh balances less own pending transfers; signed, verified (Transaction::verify)
            // and fee-checked locally (fee = static_cost × fee_ratio / 1024, fee payer covered).
            // Nothing is sent before "Send now".
broadcast(id, txid)
            // Removes the prepared bytes, then checks before sending anything: /node/info
            // (synced, same network, expires <= height → tx_expired), then the node's dry run
            // POST /transaction/validate. The node's broadcast accepts a transfer whose execution
            // fails (it is included and charged its fee), so did_fail → insufficient_funds
            // (error code INSUFFICIENT_FUNDS) / tx_expired (TX_EXPIRED) / tx_rejected, and nothing
            // is sent. A fee above the shown fee, or a check without an answer → tx_not_sent
            // (definite: nothing was sent). Then posts the bytes once to /transaction/broadcast.
            // HTTP 4xx → tx_rejected / insufficient_funds / insufficient_fee / tx_expired
            // (definite, funds released); transport errors, timeouts, 3xx/5xx →
            // rpc_unreachable / rpc_timeout / rpc_http_error (uncertain, funds stay reserved).
discard(txid)
```

Reservations (decision 3, mirrors C++ `ECDSA_Wallet::update_from/update_cache`): per
wallet, in memory, at most 256 broadcast transfers `{txid, network, expires, sender,
sender static fee, own inputs}`, recorded before posting and kept on uncertain outcomes,
removed on a definite refusal. Before Max and Review they are subtracted (saturating)
from fresh balances of their own network only (addresses are the same on every
network, and the chain server may be switched); entries of another network are kept
untouched. An entry is dropped when `expires < height` or `/transaction?id=`
shows a block at or below the height the balances were read at; unknown or failed
lookups keep it. At most one lookup per entry per new block, 4 at a time, only while
entries exist. Not persisted (C++ `pending_tx` is in memory too).
Desktop file export/import (contacts, JSON ≤ 2 MiB) runs from Rust commands: GTK
file dialogs attached to the window on Linux, the Tauri dialog plugin on Windows and
macOS; a second request waits for the open dialog. Web uses a Blob download and a
file input (same 2 MiB cap).

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
`list`. `send` signs and verifies locally and makes no request before the y/N
confirmation (or `--yes`); only then it calls `/transaction/validate` (did_fail →
insufficient_funds / transaction_expired by the node's error code, else wallet_error;
a fee above the shown fee → rpc_response_invalid) and broadcasts. A `send` that is not
broadcast reports `"status": "prepared"` (formerly `"validated"`, although no server
check runs before confirmation).
Node 4xx refusals map to tx_rejected / insufficient_funds / insufficient_fee /
transaction_expired. Keep JSON output schema backwards compatible (only add fields).
Update `tools/test_wallet_cli.py` and the help text.

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
  newRecoveryPhrase(): Promise<string>;                 // 24 fresh words; stores nothing
  createWallet(r: {name; mnemonic; passphrase?; addressCount}): Promise<WalletInfo>;
                       // only after the backup was confirmed; stores, opens, makes active;
                       // wallet_exists if already on this device
  walletFingerprint?(r: {mnemonic; passphrase?}): Promise<string>; // web: username of a new wallet
  importWallet(r: {name; mnemonic; passphrase?; addressCount; expectedFingerprint?}): Promise<WalletInfo>;
  unlockWallet(id, r: {passphrase?: string; mnemonic?: string}): Promise<WalletInfo>;
  lockWallet(id): Promise<void>;
  lockAll(opts?: {everywhere?: boolean}): Promise<void>; // web: everywhere also locks other tabs
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
  copyText(text: string, sensitive?: boolean): Promise<void>;   // sensitive: desktop clears after 60 s,
                                                                // web best effort (§6.8)
  openExternal(url: string): Promise<void>;                     // explorer links only
  exportFile(suggestedName: string, contents: string): Promise<boolean>; // false = cancelled
  importFile(): Promise<string | null>;                          // JSON ≤ 2 MiB, null = cancelled
  onExternalChange?(listener): () => void;   // web: storage event from another tab
  onExternalLock?(listener): () => void;     // web: another tab pressed Lock; keys already freed
}
```

Send contract (decision 1): no signed transaction leaves the device before the user
presses "Send now". `maxSendable`/`prepareSend` read `/chain/info`, `/node/info`,
balances and `/transaction?id=` for own pending transfers, then sign, verify
(`verify_transaction` / `Transaction::verify`) and check the fee payer locally; the
fee shown is the static fee (equals the node's `total_fee` for plain transfers).
`broadcast` checks the confirmed transfer first (`/node/info` expiry check, then the
node's dry run `/transaction/validate`, as in §5) and then posts the prepared bytes
once. Codes as in §5 `broadcast`.

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
- Balances: `/address?id=…&limit=1000` per address (skip `is_nft`). Total per
  contract plus per-address amounts. Tokens are "verified" only by the bundled
  per-network list in `src/chain/tokens.ts` (mainnet: native MMX only, like the C++
  `token_whitelist`); the node's `is_validated` flag is never used.
- Reservations: the backends subtract the wallet's own broadcast-but-unconfirmed
  transfers (§5) before Max and Review. The UI's "Available" and amount checks use
  total − `pendingReservedTotals` (amount + fee of every open send, own moves
  included) for that asset, clamped at 0.
- History cache per wallet: per address, a contiguous fully-fetched height range.
  `until` and `since` are inclusive; the node completes the last block of a page, so
  next `until = minHeight − 1`. Merge pages across addresses: rows above the highest
  "page-full" boundary are complete. First page 25 rows per address, "load more" and
  balance-history coverage extend downward (1000 per request, cap 20 000 rows per
  wallet). A page is full when the node returned `limit` entries (NFT rows
  included). Refresh re-reads the last 6 blocks: `since` = newest height the history
  server returned − 5 (not the `/node/info` head); rows outside the requested
  since/until are rejected. The public RPC does not return
  pending entries; broadcast transactions are tracked locally as pending via
  `/transaction?id=` until included (confirmed / failed) or `expires + 6 < height`
  (a lagging lookup must not mark an included send expired). Lookups run before
  the history and balance reads of a sync; a confirmed send keeps being deducted
  until history shows it. Each local send records its network and is shown and
  deducted only while the server is on that network.
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
  addresses with new rows (all addresses every 5 min); pending statuses. Failed syncs
  back off from 20 s, doubling up to 5 min; a manual refresh skips the wait.

### 6.5 Screens
Shell: wallet switcher (name; list of wallets, add wallet), navigation (Overview,
Send, Receive, Activity, Contacts, Settings), network status, lock (when lockable).
Mobile (<720 px, web): bottom tab bar Overview/Send/Receive/Activity/More.

- Onboarding: Create new wallet / Import with recovery phrase (web adds: Open a wallet
  saved in your password manager).
- Create (decision 2: stored only after the backup is confirmed): name → optional
  passphrase (typed twice, warning it cannot be recovered) → `newRecoveryPhrase()`.
  Desktop: 24 numbered words (copy with auto-clear) → pick words #a, #b, #c from
  shuffled options → `createWallet`. Web: words + save-to-password-manager form →
  fill the phrase in again inside the flow (compared with the in-memory words) →
  `createWallet`. Changing name or passphrase keeps the same words. Leaving the flow,
  a reload or a lock drops the words (nothing to delete); an idle auto-lock during
  setup shows "Wallet setup was cancelled after inactivity." on the welcome screen.
- Import: name, 24 words (per-word validation and suggestions from the BIP39 list,
  paste splits), optional passphrase (typed twice). After import scan up to 100 addresses
  (`/address/history?limit=1`, concurrency 4) and set the address count to
  max(10, last used index + 1).
- Overview: total MMX (large), pending outgoing, verified token list (empty on
  mainnet), "other tokens"
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
- Contacts: search, add/edit/delete (name ≤ 64, valid address, unique address, at
  most 1000 contacts), send to, copy; export/import JSON ≤ 2 MiB (validated, merge by
  address, import stops at the cap and reports what was skipped).
- Settings: wallet (name, fingerprint, address count 1–100 with warning when hiding
  addresses that hold funds, show recovery phrase — a passphrase wallet asks for its
  passphrase first, on both platforms; remove wallet); security (auto-lock
  5/15/30/60 min, desktop also Never; Lock now = Lock everywhere on web); network
  (desktop: server URL — Connect checks the server (`/node/info` synced, network
  matches) before anything is saved; a failure keeps the typed URL with the error
  and the old server; reset to default); appearance (System/Light/Dark); about
  (version).

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
- Verify (completes creation): a credential form inside the create flow, filled by
  the password manager (or typed), compared with the in-memory words; only then
  `createWallet` stores the metadata and opens the Vault. Nothing is stored before.
  The `verified` flag is only read from metadata written by older versions (missing =
  verified; `false` returns the user to a "Finish setting up" step).
- Unlock: wallets known on this device are listed; selecting one shows a form with
  the username prefilled (readonly) and `autocomplete="current-password"`; Chromium
  also tries `navigator.credentials.get({password: true, mediation: 'optional'})`.
  "Open another wallet" shows username + password fields empty. Passphrase is asked
  in a separate step (`autocomplete="one-time-code"`, `data-1p-ignore`, `data-lpignore`,
  `data-bwignore`) only when §6.3 says locked; on a new device the fingerprint from
  the username is the expected fingerprint; if the username has none, the passphrase
  step is shown as optional.
- After unlock the password input value is cleared; the seed is held only inside the
  WASM `Vault` of that tab. Lock / auto-lock (default 15 min idle, also on `pagehide`)
  frees every Vault of the tab and clears unlocked UI state.
- Cross-tab lock (decision 4): explicit Lock (shell button, Settings "Lock now") calls
  `lockAll({everywhere: true})`, which also writes `mmx-wallet/lock` (a unique value
  per write). Other tabs free their Vaults on the `storage` event and run the local
  lock cleanup without signalling again. Idle and `pagehide` locks stay local.
- localStorage key `mmx-wallet/v1`: `{ wallets: { [fp]: { name, withPassphrase,
  addressCount, verified? } }, activeWallet, labels, contacts, settings }`; sync across
  tabs via the `storage` event.
- Hosting: relative base (`./`), CSP via headers and `<meta>` (`deploy/security.ts`
  is the source; `_headers`, Caddyfile and nginx config are checked against it):
  `default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self';
  font-src 'self'; img-src 'self' data:; connect-src 'self' <rpc origin>;
  manifest-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none';
  require-trusted-types-for 'script'; trusted-types 'none'; frame-ancestors 'none'`
  (`connect-src 'self'` fetches the .wasm; the `<meta>` omits `frame-ancestors`)
  plus `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
  `Permissions-Policy` (all off except `clipboard-read=(self)`,
  `clipboard-write=(self)`), `Cross-Origin-Opener-Policy: same-origin`,
  `Cross-Origin-Resource-Policy: same-origin`, `X-Frame-Options: DENY`, HSTS
  (2 years, includeSubDomains). The build fails on inline script/style. RPC URL is
  fixed at build time (`VITE_RPC_URL`, default `https://rpc.mmx.network`; web builds
  use Vite mode `web`, so `.env.web` is read).

### 6.7 Desktop specifics
Tauri 2 (2.12.x). Plugins: clipboard-manager, opener (explorer URLs only:
`https://explore.mmx.network/#/explore/(transaction|address)/…`), window-state,
single-instance. `withGlobalTauri: false`; CSP `default-src 'self'; script-src
'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src ipc:
http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none';
frame-src 'none'`. Capabilities grant only the app's own commands and the plugin
permissions actually used (plus `core:webview:allow-set-webview-zoom` for the zoom
keys). Commands map 1:1 to `Service` methods (`new_recovery_phrase`, `create_wallet
{name, mnemonic, passphrase?, addressCount}`, `lock_all` ignores its body, …) and
return `{code, message}` errors; unknown fields are rejected. The window runs
`page_guard.js` (no reload, history, print, save or view-source; no page context menu
outside text fields). Binary `mmx-wallet-gui`, identifier `network.mmx.wallet`,
product name "MMX Wallet", min window 900×620.

### 6.8 Security invariants (review checklist)
- No secret (words, passphrase, seed, key) in logs, errors, panics, URLs, or storage
  other than the key file (desktop) / password manager (web). A new wallet is stored
  only after its backup was confirmed.
- Clipboard (decision 5): a copied recovery phrase is cleared after 60 s on desktop
  ("Copied. The clipboard clears in 1 minute."). Browsers allow clipboard writes
  only around user actions, so the web clears best effort: 60 s timer, then retried
  on the next trusted pointerdown/pointerup/keydown/focus in the page, on `pagehide`
  and on lock; its toast promises no time ("Copied. Clear your clipboard after
  pasting.").
- All RPC-provided text (memo, symbol, names) rendered as text, control and bidi
  override characters stripped, length-capped. No `innerHTML`/`dangerouslySetInnerHTML`.
- Native MMX identified only by the zero contract address.
- Every value from the UI is re-validated in Rust/WASM before signing.
- The transaction shown on the review screen is exactly the one broadcast
  (prepared id bound to wallet, amount, recipient, memo, fee).
- No request carries a signed transaction before "Send now"; then one dry run
  (`/transaction/validate`) and at most one POST to `/transaction/broadcast`.
- No automatic re-broadcast; no duplicate submission on double click.

## 7. Error codes
`invalid_passphrase, passphrase_required, invalid_mnemonic, wallet_mismatch (web: the
phrase does not belong to the selected wallet), wallet_exists,
wallet_not_found, locked, invalid_address, invalid_amount, invalid_memo,
insufficient_funds, insufficient_fee, invalid_argument, rpc_unreachable, rpc_timeout,
rpc_not_synced, rpc_network_mismatch, rpc_http_error, rpc_response_invalid,
tx_rejected, tx_expired, tx_not_sent, prepared_not_found, io_error, internal`.

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
- E2E (Playwright, web build + mock RPC): create→save→verify, abandoning creation
  (cancel, reload, wrong words, idle) stores nothing, import with passphrase, wrong
  passphrase, switch wallets, receive, send review/broadcast (Review makes no request
  with a body; one POST after "Send now"), a second send while the first is pending
  uses only free funds, node refusal after "Send now", pending→confirmed,
  activity/detail/save contact, contacts CRUD, settings (address count, rename,
  remove), auto-lock, explicit Lock in one tab locks another, mobile viewport.
- Desktop: container build of deb + AppImage; launch smoke test under Xvfb.

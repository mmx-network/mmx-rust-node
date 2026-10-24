# State API for screens

Import everything from `src/state` (`import { … } from '../../../state'`).
Signals are `@preact/signals` signals: read `.value` in render and components
re-render automatically. Never write to a signal from a screen; call the
actions below. Actions that talk to the backend return promises and throw
`BackendError` (`src/backend/types.ts`); show `errorMessage(e)` from
`src/lib/errors.ts` (one user-facing line per error code). Field validation
problems are returned as values, not thrown, where noted.

Formatting helpers live in `src/lib`: `formatAmount(atomic, decimals, opts)`,
`parseAmount`, `toInputString` (amount.ts), `sanitizeText`, `shortenHash`
(text.ts), `formatRelative`, `formatDateTime`,
`formatDayHeading` (time.ts), `FEE_OPTIONS` (fees.ts), `memoState` (memo.ts),
`suggestWords`, `splitWords`, `wordState`, `verificationOptions`,
`pickVerificationPositions` (mnemonic.ts), `transactionUrl`/`addressUrl`
(explorer.ts), `AUTO_LOCK_CHOICES`, `MAX_ADDRESSES`, `LABEL_MAX`,
`CONTACT_NAME_MAX` (appstate.ts). RPC text (memos, token symbols) arrives
already sanitized; always render it as text.

Native MMX is identified only by `NATIVE_CONTRACT` (`src/lib/address.ts`) or
`asset.native === true`, never by symbol. Tokens with `verified === false`
belong in "Other tokens" with a warning and their short contract address — an
unverified token can be called "MMX".

## App and routing

| Export | Type | Notes |
| --- | --- | --- |
| `init(options?: { backend?: Backend })` | `Promise<void>` | Called once by `App`. |
| `initialized` | `Signal<boolean>` | |
| `initError` | `Signal<string \| null>` | Start-up failure message (App shows it). |
| `platform` | `Signal<'desktop' \| 'web'>` | Use for platform-specific UI. |
| `phase` | `Computed<'loading' \| 'onboarding' \| 'unlock' \| 'ready'>` | onboarding = no wallets; unlock = active wallet locked. |
| `route` | `Signal<Route>` | `{ name: RouteName; param?: string }`. `param` is the activity item key for `#/activity/<key>`. |
| `navigate(name, param?, { replace? })` | `void` | Hash navigation. |
| `RouteId`, `FlowRouteId`, `RouteName`, `Route`, `AppPhase` | types | `RouteId` = shell routes. |
| `SHELL_ROUTES`, `FLOW_ROUTES`, `isShellRoute`, `isFlowRoute`, `parseHash`, `formatHash`, `homeRoute`, `routeAllowed` | helpers | The route guard redirects routes not allowed in the current phase (e.g. shell routes while locked go to `#/unlock`). Flow routes (`welcome`, `create`, `import`) stay reachable when ready ("Add wallet"); show a back/cancel action there when `phase.value === 'ready'` or wallets exist. |

## Wallets

| Export | Type | Notes |
| --- | --- | --- |
| `wallets` | `Signal<readonly WalletInfo[]>` | All wallets; entries with `error` cannot be opened. Names are display-safe. |
| `usableWallets` | `Computed<WalletInfo[]>` | Without `error`. |
| `activeWalletId` | `Signal<string \| undefined>` | |
| `activeWallet` | `Computed<WalletInfo \| undefined>` | `withPassphrase`, `locked`, `fingerprint` ("Wallet ID"), `addressCount`, web: `verified`. |
| `sessionWalletId` | `Signal<string \| null>` | Wallet whose data is loaded. |
| `opening` | `Signal<boolean>` | A wallet switch is opening a wallet. |
| `canLock` | `Computed<boolean>` | Web: always when open; desktop: passphrase wallets only. |
| `selectWallet(id)` | `Promise<void>` | Switches wallet and stays on the current page; a locked target leads to `#/unlock`. |
| `unlockWallet(id, { passphrase?, mnemonic? }, { navigateTo? })` | `Promise<WalletInfo>` | Desktop: passphrase. Web: `mnemonic` = words from the password manager (+ passphrase when needed). Opens the wallet and goes to Overview (`navigateTo: null` to stay). Errors: `invalid_passphrase`, `passphrase_required`, `wallet_mismatch`, `invalid_mnemonic`. Web: a successful unlock also marks the wallet verified. |
| `openSavedWallet({ username, words, passphrase? })` | `Promise<WalletInfo>` | Web "Open another wallet" / onboarding "Open a wallet saved in your password manager". Handles known wallets, new devices (fingerprint from the username = expected fingerprint) and usernames without one. On `passphrase_required` show the passphrase step (optional if the username had no fingerprint) and call again. |
| `credentialUsername(name, fingerprint)` | `string` | `"<name> (<fingerprint>)"` for the save/unlock forms. |
| `parseCredentialUsername(username)` | `{ name, fingerprint? }` | |
| `verifyPassphrase(id, passphrase)` | `Promise<void>` | Desktop re-check (e.g. before showing the recovery phrase). |
| `lock()` | `Promise<void>` | Web: frees all keys, goes to `#/unlock`. Desktop: forgets passphrases; goes to `#/unlock` if the active wallet is now locked. |
| `createWallet({ name, passphrase? })` | `Promise<{ wallet, mnemonic }>` | Keep `mnemonic` in component state only while shown; drop it on unmount. Empty passphrase = none. Then confirm the words (desktop: pick words; web: save form + `unlockWallet` with the saved credential) and call `completeCreate`. |
| `completeCreate(id)` | `Promise<void>` | Opens the new wallet (desktop). On web the verifying `unlockWallet` does this. |
| — | — | Web: while `activeWallet.verified === false` (credential saved but never opened) the app starts on `#/unlock`; show the confirm step for that wallet there. |
| `importWallet({ name, mnemonic, passphrase?, expectedFingerprint? })` | `Promise<WalletInfo>` | Accepts pasted phrases in any common format. Scans up to 100 addresses and sets the count to max(10, last used + 1), then opens the wallet. Errors: `invalid_mnemonic`, `wallet_exists`, `invalid_argument` (name). |
| `discovery` | `Signal<{ checked, total } \| null>` | Progress of the address scan during import. |
| `suggestWalletName(base = 'Wallet')` | `string` | First free "Wallet", "Wallet 2", … |
| `renameWallet(id, name)` | `Promise<void>` | 1–64 characters. |
| `setAddressCount(n)` | `Promise<void>` | Active wallet, 1–100. |
| `fundedAddressesFrom(n)` | `number[]` | Indices ≥ n holding funds: warn before hiding them. |
| `removeWallet(id)` | `Promise<void>` | Desktop keeps a backup of the key file. Opens another wallet or returns to onboarding. |
| `revealMnemonic(id)` | `Promise<string>` | Hold only while shown. |

## Chain data of the open wallet

| Export | Type | Notes |
| --- | --- | --- |
| `network` | `Signal<NetworkStatus>` | `{ state: 'online' \| 'connecting' \| 'offline' \| 'syncing'; height?; network?; error?: ErrorCode }`. |
| `chainHeight` | `Computed<number \| undefined>` | |
| `params` | `Signal<ChainParams \| null>` | `decimals`, `minFeeRatio`, `blockIntervalMs`, … |
| `chainError` | `Signal<ErrorCode \| null>` | Last refresh failure (null after success). |
| `clock` | `Signal<number>` | Advances every poll; read it to re-render relative times. |
| `addresses` | `Signal<readonly string[]>` | Index = address number − 1. |
| `addressRows` | `Computed<AddressRow[]>` | `{ index, address, label?, native: bigint, hasFunds }` for Receive. |
| `ownAddressSet` | `Computed<Set<string>>` | |
| `ownAddressIndex(address)` | `number` | −1 if not own. |
| `describeAddress(address)` | `Counterparty` | `{ address, name?, source: 'contact' \| 'own' \| 'address' }` (contact name, own label, or plain). |
| `balancesLoaded` | `Computed<boolean>` | |
| `assets` | `Computed<AssetBalance[] \| null>` | `{ contract, symbol, decimals, native, verified, total: bigint, byAddress: Map }`; order: MMX, verified tokens, other tokens. |
| `nativeAsset` | `Computed<AssetBalance \| null>` | MMX total. |
| `verifiedAssets` / `otherAssets` | `Computed<AssetBalance[]>` | Overview token list / collapsed "Other tokens". |
| `pendingOutgoingTotals` | `Computed<Map<contract, bigint>>` | Pending sends (MMX includes fees). |
| `refreshNow()` | `void` | Poll and refresh immediately. |

### Activity

| Export | Type | Notes |
| --- | --- | --- |
| `historyLoaded` | `Computed<boolean>` | First pages of all addresses loaded. |
| `activity` | `Computed<ActivityItem[]>` | All loaded items, newest first; local sends on top. |
| `recentActivity` | `Computed<ActivityItem[]>` | First 5 (Overview). |
| `activityFilter` | `Signal<{ kind: 'all' \| 'received' \| 'sent' \| 'rewards'; contract: string \| null }>` | |
| `setActivityFilter(partial)` | `void` | Resets the page size. |
| `filteredActivity` / `visibleActivity` | `Computed<ActivityItem[]>` | Render `visibleActivity`; group with `groupByDay(items)`. |
| `activityHasMore` | `Computed<boolean>` | Show "Load more". |
| `loadMoreActivity()` | `Promise<void>` | Fetches older history when needed (errors become a toast). |
| `activityLoadingMore` | `Signal<boolean>` | |
| `ACTIVITY_PAGE` | `50` | |
| `activityItem(key)` | `ActivityItem \| undefined` | For `#/activity/<key>`. |
| `counterpartyFor(item)` | `Counterparty \| null \| undefined` | undefined = loading (lookup started automatically), null = none (rewards, moves). |
| `memoFor(item)` | `string \| undefined` | Uses transaction details for sends. |
| `statusFor(item)` | `'confirmed' \| 'pending' \| 'failed' \| 'expired'` | |
| `confirmationsFor(item)` | `number` | |
| `cachedTx(txid)` / `loadTx(txid, force?)` | `TxInfo \| null \| undefined` / `Promise` | Details for the detail panel (`force` refreshes). Not for reward keys. |

`ActivityItem`: `{ key, txid, kind: 'received' \| 'sent' \| 'moved' \| 'reward' \| 'fee', rewardType?: 'farming' \| 'timelord' \| 'project', status, height?, timeMs, amounts: ActivityAmount[], primary?: ActivityAmount, fee: bigint, memo?, ownAddresses, to?, local? }`.
`ActivityAmount.amount` is signed (negative = sent); for `moved` it is the
moved amount. Labels: `REWARD_LABELS`; `moved` →
"Moved between your addresses". Explorer links only for non-reward items
(`openTransaction(item.txid)`).

### Balance chart

| Export | Type | Notes |
| --- | --- | --- |
| `seriesRange` / `setSeriesRange(range)` | `'1W' \| '1M' \| '3M' \| '1Y' \| 'ALL'` | Older history is fetched automatically for the range. |
| `seriesContract` / `setSeriesContract(contract)` | `string` | Default MMX. |
| `balanceSeries` | `Computed<BalanceSeries \| null>` | `{ points: { t: ms, v: bigint }[], start, end, complete, min, max }` — draw as steps. `complete === false`: history before `start` was not available. |
| `seriesLoading` | `Signal<boolean>` | |

## Send

| Export | Type | Notes |
| --- | --- | --- |
| `sendDraft` | `Signal<{ to, contract, amount, memo }>` | `amount` as typed (locale format). |
| `updateDraft(partial)` | `void` | Bind inputs to this. |
| `startSend(prefill?)` | `Promise<void>` | Fresh form + navigate to Send ("Send to" from contacts). |
| `draftCheck` | `Computed<DraftCheck>` | `{ recipient?, contact?, ownAddress, asset?, amountAtomic?, memo?, errors: { to?, amount?, memo? }, ready }` — errors are user-facing lines; show them once a field was touched. |
| `sendableAssets` | `Computed<AssetBalance[]>` | Asset picker. |
| `fillMaxAmount()` | `Promise<void>` | "Max" button. |
| `sendFlow` | `Signal<SendFlowState>` | `{ step: 'edit' \| 'review' \| 'done' \| 'uncertain', busy: false \| 'preparing' \| 'sending' \| 'max' \| 'unlocking', feeLevel, request?, prepared?: PreparedSend, preparedAt?, needsPassphrase, error?: { code, message, field? } }`. Disable inputs while `busy`. |
| `reviewSend()` | `Promise<boolean>` | Prepares the transaction → `step: 'review'`. |
| `setSendFeeLevel(level)` | `Promise<void>` | Review priority selector (`FEE_OPTIONS`); prepares a new transaction. |
| `reviewTotals(prepared)` | `{ amount, fee, totalNative }` | `totalNative` is null for tokens (fee is separate, in MMX). |
| `confirmSend()` | `Promise<void>` | Broadcasts once (repeated clicks are ignored). → `done`, or `uncertain` ("We couldn't confirm the transfer. Check Activity before sending again."). Other failures return to `edit` with `error`. |
| `backToEdit()` | `Promise<void>` | Leave review without sending. |
| `finishSend()` | `void` | "Done": clears the form. |
| `discardSend()` | `Promise<void>` | Reset everything. |
| `sendNeedsPassphrase` | `Computed<boolean>` | Desktop: show a passphrase field, then `unlockForSend(passphrase)`. |
| `unlockForSend(passphrase)` | `Promise<boolean>` | Continues to review on success; `error.field === 'passphrase'` on failure. |
| `amountErrorMessage`, `checkDraft`, `matchesRequest`, `formatPrepared`, `PREPARED_TTL_MS` | helpers | |

A review that stays open past `PREPARED_TTL_MS` returns to `edit` with an
"expired" error automatically. Show the recipient with `AddressText` (full
address in groups of 4).

## Contacts and labels

| Export | Type | Notes |
| --- | --- | --- |
| `contacts` / `sortedContacts` | `Computed<Contact[]>` | `{ id, name, address, note? }`. |
| `searchContacts(query, limit?)` | `Contact[]` | Name, note or address. |
| `contactForAddress(address)` | `Contact \| undefined` | |
| `contactErrors(input, editingId?)` | `ContactErrors` | Live validation: `name: 'empty' \| 'too_long' \| 'invalid'`, `address: 'empty' \| 'invalid' \| 'duplicate' \| 'own'`, `note: 'too_long' \| 'invalid'`. |
| `addContact(input)` / `updateContact(id, input)` | `Promise<ContactResult>` | `{ ok: true, contact } \| { ok: false, errors }`. |
| `deleteContact(id)` | `Promise<Contact \| undefined>` | Offer undo with `restoreContact(contact)`. |
| `exportContacts()` | `Promise<boolean>` | false = cancelled. |
| `importContacts()` | `Promise<{ status: 'cancelled' } \| { status: 'invalid' } \| { status: 'imported', added, skipped }>` | Merges by address (existing kept). |
| `activeLabels` | `Computed<Record<number, string>>` | Labels by address index. |
| `setAddressLabel(index, label)` | `Promise<void>` | Empty text clears; max `LABEL_MAX` (32). |

## Settings

| Export | Type | Notes |
| --- | --- | --- |
| `settings` | `Computed<Settings>` | `{ theme, autoLockMinutes, feeLevel, rpcUrl? }`. |
| `appState` | `Signal<AppState>` | Read-only for screens. |
| `setTheme(theme)` | `Promise<void>` | `'system' \| 'light' \| 'dark'` (App applies it). |
| `setAutoLockMinutes(m)` | `Promise<void>` | `AUTO_LOCK_CHOICES[platform]`: 5/15/30/60, desktop also 0 = never. |
| `autoLockDelayMinutes()` | `number \| null` | Effective delay now (null = auto-lock does not apply). |
| `setDefaultFeeLevel(level)` | `Promise<void>` | |
| `setRpcUrl(url \| null)` | `Promise<void>` | Desktop only. Checks the server before saving (throws `rpc_*` or `invalid_argument`); null = default. Reloads data. |
| `normalizeRpcUrl(text)` / `DEFAULT_RPC_URL` | helpers | |

## Feedback

| Export | Type | Notes |
| --- | --- | --- |
| `toasts` | `Signal<readonly Toast[]>` | Rendered by App. |
| `notify(message, kind?, { timeout?, action? } \| timeoutMs)` | `number` | kinds: `success`, `error`, `info`, `warning`. |
| `dismissToast(id)` / `clearToasts()` | `void` | |
| `copyToClipboard(text, { sensitive?, message? })` | `Promise<boolean>` | Shows "Copied"; `sensitive` clears the clipboard after 60 s (recovery words). |
| `copyAddress(address)` | `Promise<void>` | "Address copied"; throws when copying failed (for `CopyButton`). |
| `openTransaction(txid)` / `openAddress(address)` | `Promise<void>` | Explorer links. |
| `markActivity()` | `void` | Idle tracking is automatic; only needed for activity outside DOM events. |

## Onboarding (`actions/onboarding.ts`)

| Export | Type | Notes |
| --- | --- | --- |
| `importWallet(request, { navigateTo? })` | `Promise<WalletInfo>` | Second argument: `navigateTo: null` opens the wallet but stays on the page (web import save step). |
| `passwordManagerApi()` | `boolean` | Web and the browser can save/offer passwords from script (Chromium). |
| `saveWalletCredential(username, words)` | `Promise<boolean>` | Web: `navigator.credentials.store`; false when unsupported or declined. |
| `requestWalletCredential()` | `Promise<StoredCredential \| null>` | Web: the browser's saved-wallet chooser; turns off silent sign-in first so a wallet never opens without the user picking it. |
| `discardNewWallet(id)` | `Promise<void>` | Removes a wallet whose creation was abandoned (desktop keeps a key file backup). Never throws. |
| `finishCreate(id)` | `Promise<void>` | `completeCreate` + "Wallet created" toast (desktop). |
| `leaveOnboarding()` | `void` | Back from a welcome/create/import flow: Overview, unlock or welcome by phase. |
| `hasWallets()` | `boolean` | Usable wallets exist on this device. |

## Development data

`npm run dev:mock` serves the mock backend (`src/backend/mock.ts`): wallets
Main, Savings (passphrase `correct horse`, starts locked) and Trading; `?platform=web`
for the web flows (all wallets locked; the words are `MOCK_WALLETS[i].mnemonic`),
`?mock=empty` for onboarding.

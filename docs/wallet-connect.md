# Website connections to MMX Wallet

The hosted web wallet supports a popup API for public account discovery and
custom transaction submission. Users must first enable **Settings → Website
connections → Allow website connections**. It is off by default, including when
upgrading an existing wallet. Connecting shares the selected wallet's public
addresses only after the user clicks **Connect**. Each transaction requires
**Continue** (or **Unlock to review**), then **Send now** in the wallet window.
Connecting and each transaction open their own short-lived popup. The popup
closes after a completed action, rejection, timeout or access revocation.
Unlocking and preparation errors remain visible in the popup for retry; they
do not complete the request or send anything.

Users can choose **Block this website** in the connection window, or enter a
website URL in wallet Settings. Blocking persists for that exact origin
(scheme, hostname and port), disconnects it and cancels outstanding approvals.
Settings lists blocked origins with an **Unblock** action. Turning connections
off cancels outstanding approvals as well. Neither action can recall a
transaction already submitted to the node. Re-enabling or unblocking requires
a new connection from the website; old approvals cannot be reused.

## Website integration

Serve the application over HTTPS; HTTP is allowed only on loopback for local
development. Import the standalone ESM SDK from the deployed wallet:

```html
<button id="connect">Connect MMX Wallet</button>
<button id="send" disabled>Call contract</button>
<script type="module" src="./wallet-integration.js"></script>
```

```js
// wallet-integration.js
import { connect } from 'https://wallet.mmx.network/wallet-sdk.js';

let wallet;
document.querySelector('#connect').onclick = async () => {
  try {
    // Call directly in the click handler so the browser permits the popup.
    wallet = await connect({ network: 'mainnet' });
    console.log(wallet.account.address, wallet.account.addresses);
    document.querySelector('#send').disabled = false;
  } catch (error) {
    console.error(error.code, error.message);
  }
};

document.querySelector('#send').onclick = async () => {
  try {
    // Call directly in the click handler: this opens a fresh approval popup.
    const { txid } = await wallet.sendTransaction({
      execute: [{
        __type: 'mmx.operation.Execute',
        address: CONTRACT_ADDRESS, // application's deployed contract
        method: 'my_method',
        args: ['example', 42],
        user: wallet.account.address,
      }],
    }, { feeLevel: 1, gasLimit: 1_000_000 });
    console.log('Submitted:', txid);
  } catch (error) {
    console.error(error.code, error.message);
  }
};

// To end the session: wallet.disconnect().
```

`/wallet-sdk.js` is a small ES module loader that re-exports the latest build
from `/assets/sdk/wallet-sdk-<hash>.js`. The hash is derived from the emitted
SDK's contents, so unchanged SDK code keeps the same URL across wallet builds.
The loader revalidates on a fresh page load; the hashed SDK is cached for one
year with `immutable`. The loader and the hashed SDK permit cross-origin
imports. There is no HTTP redirect or server relay.

To pin a particular SDK build, import the hashed URL named in the loader
instead. Keep that asset available on the origin across deployments for sites
that pin it. The stable loader automatically follows upgrades without changing
the website's import URL.

A test deployment can pass `walletUrl: 'http://127.0.0.1:5173/'` (or its HTTPS
base URL, ending in `/`) to `connect`. Use a production web build plus Vite
preview for local testing of the SDK and the real response headers. `network`
is optional at connect time; transaction requests are always bound to the
network returned by the wallet. `timeoutMs` defaults to two minutes and is
bounded to 1–300,000 ms; wallet approval itself expires after two minutes.

The calling website must use the default
`Cross-Origin-Opener-Policy: unsafe-none` or
`Cross-Origin-Opener-Policy: same-origin-allow-popups`. A caller using
`same-origin` severs the popup reference and cannot use this API. If the caller
has a CSP, it must allow importing the SDK from the wallet origin, or bundle
`apps/wallet/src/connect/sdk.ts` into its application. The popup is a separate
window or tab according to the browser's UI. It closes after each action; the
public connection stays available for later transaction requests. Both
`connect()`, `signMessage()` and `sendTransaction()` must be called directly from a user click
or tap, before any `await`, so mobile popup blocking does not prevent the new
window. Only one popup may be active per SDK instance.

## Transaction format and limits

`sendTransaction(transaction, options)` accepts an unsigned public WebAPI
transaction template. `__type: 'mmx.Transaction'` is optional at its root.
Outputs, `mmx.operation.Execute`, `mmx.operation.Deposit` and supported native
contract deployments use the same record codec, hashing, completion and
signature authorization as the Rust native wallet. For example, a transfer is:

```js
await wallet.sendTransaction({
  outputs: [{
    __type: 'mmx.txout_t',
    address: RECIPIENT_ADDRESS,
    contract: NATIVE_CURRENCY_ADDRESS,
    amount: '1000000', // one MMX, in atomic units
  }],
}, { gasLimit: 0 });
```

Use canonical lowercase Bech32 addresses, hexadecimal hashes/byte arrays, and
decimal **strings** for atomic amounts and wide integers. Contract arguments
retain their JSON values without guessing types from names such as `address`
or `amount`. VM JSON supports integers, strings, booleans, null, arrays and
objects; fractional numbers are unsupported. `__type` is reserved for VNX typed
records, including inside arguments. Do not use it as an ordinary object key.
Numbers beyond JavaScript's safe integer range are rejected; use strings.

The wallet supplies funding, signatures, the network, version, nonce, expiry
(at most 100 blocks) and maximum fee. Inputs, if supplied, must be ordinary
accounts owned by the selected wallet. A supplied sender must also be owned.
Existing signatures, execution receipts, unknown fields, foreign input owners
and contract/multisig input flags are rejected. This API does not expose partial
signing, arbitrary hash signing, signed transaction bytes or private keys.

`feeLevel` is 1, 2, 3, 5 or 10, defaulting to the wallet's fee setting.
`gasLimit` is an unsigned 32-bit execution-cost budget, default 5,000,000.
Review displays the actual canonical template, transfers, calls/arguments,
deployments, network, expiry and maximum fee (static cost plus execution budget,
scaled by the fee ratio). The fee payer must cover this maximum. The wallet shows formatted amounts, recipients, memos, contract actions and
named argument/value lists from the first review screen. The canonical signed
quote then shows the maximum fee and total; signatures stay private. Structured
additional details replace raw JSON. Contract balance/ownership changes are
not simulated before review. After approval, a node dry run must succeed and stay
within the approved maximum before broadcast. Submission returns `{ txid }`;
confirmation must be followed separately.

Requests are bounded to 64 KiB, depth 32 and 4,096 JSON nodes, with at most
100 supplied inputs, 100 outputs and 32 operations. Only one transaction can be
pending per popup. Signing commands are serialized per signing tab. Before
broadcast, the wallet refreshes balances and subtracts pending reservations so
two reviewed requests using that signer cannot spend the same available funds.
Public reservations are stored per wallet and RPC endpoint, tagged with the
network, so closing a signing popup or reloading cannot release pending funds.
They are removed once included, expired or definitively refused. Storage is
bounded to 256 pending transactions and 2 million characters per wallet; a full
or unwritable store prevents submission rather than evicting pending entries.
This is bookkeeping, not an atomic lock across simultaneously unlocked tabs;
the node still rejects conflicting concurrent submissions. A prepared
transaction is consumed at most once.

Handle `user_rejected`, `website_connections_disabled`, `website_blocked`,
`locked`, `popup_blocked`, `disconnected`, `timeout`, `invalid_request`,
`insufficient_funds`, `insufficient_fee`, `tx_rejected`, `tx_expired` and
`tx_uncertain`. A lost broadcast reply leaves the maximum fee and inputs
reserved. Closing or timing out after submission starts reports `tx_uncertain`. **Do not automatically retry a transaction** after a timeout or
uncertain reply; check wallet Activity / chain state first.

## Browser communication and deployment

The website and popup use `postMessage`, bound to the exact opener window,
origin, protocol version and random channel ID. The fragment contains only
that channel and origin. The popup checks the actual message origin, displays
it for approval, and communicates with wallet tabs over a separate same-origin
`BroadcastChannel`. The website never receives that channel's bridge ID.
There is no wallet-server relay, session cookie or remote signing session.

A successful connection returns a random, origin-bound grant to the SDK. The
wallet stores up to 32 public grants for one hour so a subsequent popup can
identify the approved account. Grants contain only origin, public account,
wallet name and expiry; they cannot unlock or spend. Their IDs travel in the
origin-checked handshake, never in the URL. The SDK keeps its grant in memory
and drops it on disconnect/navigation. Blocking a site or disabling website
connections removes stored grants, so re-enabling requires fresh connection
consent. The grant never substitutes for per-transaction approval.

Connecting uses the cached public address list while the wallet remains locked.
Only a wallet with no complete address list asks for **Unlock and connect** to
derive its missing addresses. Connection consent never extends a signing timeout.

For spending, the popup prefers a main wallet tab. If it is locked, **Unlock to
review** opens a signing session in that tab, so closing the approval popup does
not discard the session. Following transactions reuse it until the configured
idle timeout (five minutes by default), manual lock, reload or tab closure.
Each explicit review/send action refreshes that timeout; incoming website
requests and discovery do not. Cached public addresses remain observable after
locking. Keys, signatures and signed transactions stay in the signing tab.

The popup hands an entered unlock credential to the selected wallet tab using
a one-use P-256 ECDH / AES-GCM envelope through their same-origin channel. The
fresh recipient key and 30-second challenge are tied to the tab, bridge, wallet,
website and lock version. Challenges are consumed on any attempt, and a lock
invalidates outstanding challenges. Plaintext credentials never appear in the
channel or website messages; neither ciphertext nor credentials are persisted.
The handoff is bounded to 32 KiB of encoded unlock data and uses browser
[Web Crypto](https://www.w3.org/TR/webcrypto/). Existing seed-file storage is
unchanged; this envelope only carries a transient unlock between local pages.

Keep a main wallet tab open to retain the timed session. If no wallet tab is
available, a popup can still unlock locally; closing that popup purges its keys
and the next request needs another unlock. If a selected tab stops responding during unlock, the request fails; no second
signing session is opened automatically. Physical mobile suspension is
still browser-dependent. The SDK requests a 480 × 800 content window; simple
transfers (including a short memo) fit with approval controls visible. Longer
contract details and smaller screens can scroll.

The wallet's deployment templates retain COOP `same-origin` on the main page
and use `unsafe-none` only for `/connect.html`. `/wallet-sdk.js` and
`/assets/sdk/*` have `Access-Control-Allow-Origin: *`; wallet pages and RPC do not gain wildcard
CORS from this change. The loader and popup use `Cache-Control: no-cache`;
hashed SDK assets use `public, max-age=31536000, immutable`. CSP,
anti-framing, no-referrer and other wallet security headers remain enabled on
the popup. The Docker image, Caddy, nginx and generated `_headers` files all
include these rules. Any CDN overrides must preserve the path-specific COOP
and SDK CORS headers. See [Docker deployment](web-apps-docker.md).
For standalone nginx, install both `nginx-security-headers.conf` and
`nginx-connect-security-headers.conf` at the snippet paths named in
`apps/wallet/deploy/nginx.conf`.

Chromium tests exercise two actual origins with the production CSP/headers,
real WASM signing, reuse/fallback, rejection, lock, disable and block/unblock.
The mock node independently checks transaction shape, costs and balances and
models deposits; signature verification runs through the real wallet core.
The mock does not execute arbitrary contract VM code. Native tests cover exact Variant
arguments and signature tampering. Physical iOS/Android background suspension
and browser-specific popup UI remain to be checked on devices.


## Sign-in messages

`wallet.signMessage(message)` opens a separate approval popup, displays the exact
message and origin, and signs only after consent (unlocking if necessary). It
returns `{address, origin, network, message, public_key, signature}`. It does not
broadcast a transaction or spend funds. The message must be 1–4096 UTF-8 bytes.
Invoke it directly from a click handler, after obtaining a server challenge.

Servers must issue a random, expiring, one-use challenge and verify all returned
fields against that challenge. The compressed secp256k1 public key is 33 bytes;
the signature is a low-S, 64-byte compact ECDSA signature, both hex encoded.
Verify ECDSA over the SHA-256 digest of this exact UTF-8 payload:

```text
MMX/sign_message/MMX/website_message/v1\n{origin}\n{network}\n{address}\n{message}
```

Each `\n` above is one LF byte. The wallet address must equal the MMX Bech32m
encoding of SHA-256(public_key), using MMX's reversed-byte address encoding.
Neither a public `connect()` reply nor a valid signature for a different origin,
network or nonce is proof for the current session. Consume the challenge once.
The wallet signs the primary address approved by the connection.

Custom transaction funding includes indivisible NFT balances, so a website can
request a canonical NFT transfer or escrow deposit. Ordinary currency balances
and history keep their existing NFT filtering.

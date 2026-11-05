# Website connections to MMX Wallet

The hosted web wallet supports a popup API for public account discovery and
custom transaction submission. Users must first enable **Settings → Website
connections → Allow website connections**. It is off by default, including when
upgrading an existing wallet. Connecting shares the selected wallet's public
addresses only after the user clicks **Connect**. Each transaction requires
**Review transaction**, then **Approve and send** in the wallet window.

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
import { connect } from 'https://wallet.mmx.network/mmx-wallet.js';

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
window or tab according to the browser's UI. Keep it open for the connection.

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
scaled by the fee ratio). The fee payer must cover this maximum. The wallet
shows raw contract actions; it does not simulate their balance/ownership
changes before review. After approval, a node dry run must succeed and stay
within the approved maximum before broadcast. Submission returns `{ txid }`;
confirmation must be followed separately.

Requests are bounded to 64 KiB, depth 32 and 4,096 JSON nodes, with at most
100 supplied inputs, 100 outputs and 32 operations. Only one transaction can be
pending per popup. Signing commands are serialized per signing tab. Before
broadcast, the wallet refreshes balances and subtracts pending reservations so
two reviewed requests using that signer cannot spend the same available funds.
Reservations are local to each signing backend; independently unlocked tabs
still rely on the node to reject conflicting submissions. A prepared
transaction is consumed at most once.

Handle `user_rejected`, `website_connections_disabled`, `website_blocked`,
`locked`, `popup_blocked`, `disconnected`, `timeout`, `invalid_request`,
`insufficient_funds`, `insufficient_fee`, `tx_rejected`, `tx_expired` and
`tx_uncertain`. A lost broadcast reply leaves the maximum fee and inputs
reserved. **Do not automatically retry a transaction** after a timeout or
uncertain reply; check wallet Activity / chain state first.

## Browser communication and deployment

The website and popup use `postMessage`, bound to the exact opener window,
origin, protocol version and random channel ID. The fragment contains only
that channel and origin. The popup checks the actual message origin, displays
it for approval, and communicates with wallet tabs over a separate same-origin
`BroadcastChannel`. The website never receives that channel's bridge ID.
There is no wallet-server relay, session cookie or remote signing session.

An unlocked wallet tab signs locally without sending its seed, passphrase,
keys, signatures or signed transaction to the popup. If the tab is closed,
locked or suspended, **Unlock in this window** lets the user unlock the same
wallet locally in the popup. A fresh popup with no available signer offers
**unlock here**. Manual and idle locks still purge signing material; only the
user's spending actions extend the signing tab's inactivity timer. Discovery
and inbound website requests do not extend it. Closing the popup releases keys
it unlocked without locking the original wallet tab.

The wallet's deployment templates retain COOP `same-origin` on the main page
and use `unsafe-none` only for `/connect.html`. The standalone `/mmx-wallet.js`
has `Access-Control-Allow-Origin: *`; wallet pages and RPC do not gain wildcard
CORS from this change. Both new paths use `Cache-Control: no-cache`. CSP,
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

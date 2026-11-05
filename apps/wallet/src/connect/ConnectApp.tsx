import { useSignalEffect } from '@preact/signals';
import { useEffect, useRef, useState } from 'preact/hooks';
import { isErrorCode, type Backend, type UnlockRequest, type WalletInfo } from '../backend/types';
import { createBackend } from '../backend/web';
import { sanitizeAppState } from '../lib/appstate';
import { formatAmount } from '../lib/amount';
import { errorMessage, messageForCode } from '../lib/errors';
import { websiteAccess } from '../lib/websites';
import { setWebsiteBlocked } from '../state/settings';
import { activeWalletId, appState, lockVersion, platform, setBackend, setWallets, upsertWallet, wallets } from '../state/core';
import { markActivity, startIdleLock } from '../state/idle';
import { applyAppearance } from '../styles';
import { Banner, Button, Card, Field, Heading, Select, Stack, Text } from '../ui/components';
import { FocusLayout } from '../ui/shell/FocusLayout';
import { AuthorizationDialog } from '../ui/screens/unlock/AuthorizationDialog';
import { WalletClient } from './client';
import { startWalletHost } from './host';
import {
  ConnectionError,
  PROTOCOL,
  REQUEST_TIMEOUT,
  VERSION,
  envelope,
  object,
  origin,
  token,
  transactionRequest,
  type Account,
  type Offer,
  type PreparedTransaction,
  type TransactionRequest,
} from './protocol';

type Target = { host: string; walletId: string };
type Request = { id: string; params: TransactionRequest };
function fault(e: unknown): { code: string; message: string } {
  return e instanceof ConnectionError
    ? { code: e.code, message: isErrorCode(e.code) ? messageForCode(e.code) : e.message }
    : { code: object(e) && typeof e.code === 'string' ? e.code : 'internal', message: errorMessage(e) };
}
function amount(value: unknown, decimals: number): string {
  try {
    return formatAmount(BigInt(String(value)), decimals);
  } catch {
    return String(value ?? '');
  }
}
function Review({ prepared }: { prepared: PreparedTransaction }) {
  const tx = prepared.transaction;
  const outputs = Array.isArray(tx.outputs) ? tx.outputs : [];
  const calls = Array.isArray(tx.execute) ? tx.execute : [];
  return (
    <Stack gap={4}>
      <div class="connection__fee">
        <Text tone="muted">Maximum network fee</Text>
        <Heading level={2}>{amount(prepared.maxFee, prepared.decimals)} MMX</Heading>
        <Text size="sm" tone="muted">
          Includes execution costs. Static fee: {amount(prepared.fee, prepared.decimals)} MMX.
        </Text>
      </div>
      <Text size="sm">
        Network: <strong>{prepared.network}</strong> · Valid through block {prepared.expires}
      </Text>
      {outputs.map((v, i) => {
        if (!object(v)) return null;
        const currency = prepared.currencies.find((c) => c.contract === v.contract);
        return (
          <div class="connection__item" key={i}>
            <strong>
              Send {currency ? `${amount(v.amount, currency.decimals)} ${currency.symbol}` : `${String(v.amount)} atomic units`}
            </strong>
            <span class="connection__address">{String(v.address)}</span>
            {!currency ? <span class="connection__address">Currency: {String(v.contract)}</span> : null}
            {v.memo ? <span>Memo: {String(v.memo)}</span> : null}
          </div>
        );
      })}
      {calls.map((v, i) =>
        object(v) ? (
          <div class="connection__item" key={i}>
            <strong>
              {v.__type === 'mmx.operation.Deposit' ? 'Deposit and call' : 'Contract call'}: {String(v.method)}
            </strong>
            <span class="connection__address">{String(v.address)}</span>
            {v.__type === 'mmx.operation.Deposit' ? (
              <span>
                Deposit:{' '}
                {(() => {
                  const currency = prepared.currencies.find((c) => c.contract === v.currency);
                  return currency ? `${amount(v.amount, currency.decimals)} ${currency.symbol}` : `${String(v.amount)} atomic units`;
                })()}
                <span class="connection__address">Currency: {String(v.currency)}</span>
              </span>
            ) : null}
            {v.user ? <span class="connection__address">Caller: {String(v.user)}</span> : <span>Anonymous caller</span>}
            <pre class="connection__json">{JSON.stringify(v.args, null, 2)}</pre>
          </div>
        ) : null,
      )}
      {tx.deploy ? (
        <div class="connection__item">
          <strong>Deploy contract</strong>
          <pre class="connection__json">{JSON.stringify(tx.deploy, null, 2)}</pre>
        </div>
      ) : null}
      <details>
        <summary>Full transaction details</summary>
        <pre class="connection__json">{JSON.stringify(tx, null, 2)}</pre>
      </details>
      <Text size="sm" tone="muted">
        Contract calls may change balances or ownership. Check the method, arguments and fee before approving.
      </Text>
    </Stack>
  );
}

export function ConnectApp() {
  const query = useRef(new URLSearchParams(window.location.hash.slice(1))).current;
  const channel = query.get('channel');
  const expected = origin(query.get('origin'));
  const opener = useRef(window.opener as Window | null).current;
  const valid = token(channel) && expected !== null && opener !== null;
  const [site, setSite] = useState('');
  const [ready, setReady] = useState(false);
  const [offers, setOffers] = useState<Offer[]>([]);
  const [selected, setSelected] = useState('');
  const [connected, setConnected] = useState<Account | null>(null);
  const [request, setRequest] = useState<Request | null>(null);
  const [prepared, setPrepared] = useState<PreparedTransaction | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [authorizing, setAuthorizing] = useState<WalletInfo | null>(null);
  const [sent, setSent] = useState('');
  const [denied, setDenied] = useState('');
  const backendRef = useRef<Backend>();
  const clientRef = useRef<WalletClient>();
  const localHost = useRef<ReturnType<typeof startWalletHost>>(null);
  const target = useRef<Target>();
  const siteRef = useRef('');
  const connectId = useRef('');
  const requestedNetwork = useRef<string>();
  const accountRef = useRef<Account | null>(null);
  const pending = useRef<Request | null>(null);
  const preparedRef = useRef<PreparedTransaction | null>(null);
  const busyRef = useRef(false);
  const alive = useRef(true);
  const generation = useRef(0);
  const expiry = useRef<ReturnType<typeof setTimeout>>();
  const post = (data: Record<string, unknown>) => {
    if (valid && siteRef.current && opener) opener.postMessage({ protocol: PROTOCOL, version: VERSION, channel, ...data }, siteRef.current);
  };
  const finish = (result?: unknown, failure?: { code: string; message: string }) => {
    const current = pending.current;
    if (current) post({ type: 'result', id: current.id, ...(failure ? { error: failure } : { result }) });
    pending.current = null;
    preparedRef.current = null;
    generation.current++;
    clearTimeout(expiry.current);
    setRequest(null);
    setPrepared(null);
    setBusy(false);
    busyRef.current = false;
  };
  const discard = () => {
    const t = target.current;
    if (t) void clientRef.current?.call(t.host, t.walletId, siteRef.current, 'discard').catch(() => undefined);
  };
  const reject = () => {
    discard();
    finish(undefined, { code: 'user_rejected', message: 'Transaction rejected.' });
    setError('');
  };
  const revoke = (code: string) => {
    discard();
    const message = isErrorCode(code) ? messageForCode(code) : 'Wallet disconnected.';
    if (pending.current) finish(undefined, { code, message });
    else generation.current++;
    if (!accountRef.current && connectId.current) post({ type: 'result', id: connectId.current, error: { code, message } });
    if (target.current) clientRef.current?.cancel(target.current.host);
    post({ type: 'closed' });
    target.current = undefined;
    accountRef.current = null;
    setConnected(null);
    setAuthorizing(null);
    setDenied(code);
    setError('');
    setBusy(false);
    busyRef.current = false;
    void backendRef.current?.lockAll();
  };
  const disconnect = () => {
    generation.current++;
    if (pending.current) finish(undefined, { code: 'disconnected', message: 'Wallet disconnected.' });
    const t = target.current;
    if (t) clientRef.current?.cancel(t.host);
    post({ type: 'closed' });
    window.close();
    accountRef.current = null;
    setConnected(null);
    setError('Disconnected. Reconnect from the website.');
  };
  useSignalEffect(() => {
    applyAppearance(appState.value.settings.theme);
  });
  const access = ready && site ? websiteAccess(appState.value.settings, site) : null;
  useEffect(() => {
    if (access) revoke(access);
  }, [access, ready, site]);
  const lastLock = useRef(lockVersion.value);
  useSignalEffect(() => {
    const version = lockVersion.value;
    if (version !== lastLock.current) {
      lastLock.current = version;
      discard();
      if (pending.current) finish(undefined, { code: 'locked', message: 'The wallet was locked.' });
      setError('Wallet locked. Unlock it before reviewing another transaction.');
    }
  });
  useEffect(() => {
    if (!valid) return;
    alive.current = true;
    if (typeof BroadcastChannel === 'undefined') {
      setError('This browser does not support wallet connections. Try a current browser.');
      return;
    }
    const client = new WalletClient();
    clientRef.current = client;
    client.onOffers = setOffers;
    client.onRevoked = revoke;
    let stopIdle: (() => void) | undefined;
    const stops: Array<() => void> = [];
    void (async () => {
      const b = await createBackend({ preload: false });
      if (!alive.current) {
        await b.lockAll();
        return;
      }
      backendRef.current = b;
      setBackend(b);
      platform.value = 'web';
      appState.value = sanitizeAppState(await b.loadAppState(), 'web');
      setWallets(await b.listWallets());
      activeWalletId.value = appState.value.activeWallet;
      if (!alive.current) return;
      stopIdle = startIdleLock({ trackFocus: false });
      localHost.current = startWalletHost();
      if (b.onExternalChange)
        stops.push(
          b.onExternalChange(
            () =>
              void Promise.all([b.loadAppState(), b.listWallets()])
                .then(([state, list]) => {
                  if (alive.current) {
                    appState.value = sanitizeAppState(state, 'web');
                    setWallets(list);
                  }
                })
                .catch(() => undefined),
          ),
        );
      if (b.onExternalLock)
        stops.push(
          b.onExternalLock(() => {
            lockVersion.value++;
            void b
              .listWallets()
              .then(setWallets)
              .catch(() => undefined);
          }),
        );
      setReady(true);
      client.discover();
    })().catch((e) => {
      if (alive.current) setError(errorMessage(e));
    });
    const onMessage = (event: MessageEvent<unknown>) => {
      if (event.source !== opener || event.origin !== expected || !envelope(event.data, channel!)) return;
      const m = event.data;
      if (m.type === 'hello' && token(m.id) && !siteRef.current) {
        if (m.network !== undefined && (typeof m.network !== 'string' || m.network.length > 256)) return;
        siteRef.current = event.origin;
        connectId.current = m.id;
        setSite(event.origin);
        requestedNetwork.current = typeof m.network === 'string' ? m.network : undefined;
      } else if (m.type === 'disconnect' && siteRef.current) disconnect();
      else if (m.type === 'cancel' && pending.current?.id === m.id) {
        discard();
        finish(undefined, { code: 'cancelled', message: 'Request cancelled.' });
      } else if (m.type === 'request' && siteRef.current && token(m.id)) {
        const denied = websiteAccess(appState.peek().settings, siteRef.current);
        if (denied) {
          revoke(denied);
          return;
        }
        if (!accountRef.current) {
          post({ type: 'result', id: m.id, error: { code: 'disconnected', message: 'Connect a wallet first.' } });
          return;
        }
        if (pending.current || busyRef.current) {
          post({ type: 'result', id: m.id, error: { code: 'busy', message: 'Finish the current transaction first.' } });
          return;
        }
        if (m.method !== 'sendTransaction' || !transactionRequest(m.params)) {
          post({ type: 'result', id: m.id, error: { code: 'invalid_request', message: 'Invalid transaction request.' } });
          return;
        }
        if (m.params.network !== accountRef.current.network) {
          post({
            type: 'result',
            id: m.id,
            error: { code: 'invalid_request', message: 'Transaction network does not match the connected wallet.' },
          });
          return;
        }
        const next = { id: m.id, params: structuredClone(m.params) };
        pending.current = next;
        setRequest(next);
        setSent('');
        setError('');
        setPrepared(null);
        expiry.current = setTimeout(() => {
          discard();
          finish(undefined, { code: 'timeout', message: 'Transaction approval expired.' });
          setError('Transaction approval expired. Request it again from the website.');
        }, REQUEST_TIMEOUT);
      }
    };
    window.addEventListener('message', onMessage);
    const announce = () => {
      if (!siteRef.current) opener!.postMessage({ protocol: PROTOCOL, version: VERSION, channel, type: 'ready' }, expected!);
    };
    announce();
    const announceTimer = setInterval(announce, 500);
    const onHide = () => {
      post({ type: 'closed' });
      client.close();
      localHost.current?.close();
      void backendRef.current?.lockAll();
    };
    window.addEventListener('pagehide', onHide);
    return () => {
      alive.current = false;
      generation.current++;
      clearTimeout(expiry.current);
      clearInterval(announceTimer);
      window.removeEventListener('message', onMessage);
      window.removeEventListener('pagehide', onHide);
      stopIdle?.();
      stops.forEach((s) => s());
      onHide();
    };
  }, []);
  const local = wallets.value.filter((w) => !w.error);
  const options = offers.map((o) => ({ value: `${o.host}:${o.wallet.id}`, label: `${o.wallet.name} · already unlocked` }));
  for (const w of local)
    if (!offers.some((o) => o.wallet.id === w.id)) options.push({ value: `local:${w.id}`, label: `${w.name} · unlock here` });
  const selection = options.some((o) => o.value === selected) ? selected : (options[0]?.value ?? '');
  async function connectTarget(t: Target) {
    const epoch = generation.current;
    const access = websiteAccess(appState.peek().settings, siteRef.current);
    if (access) throw new ConnectionError(access);
    const a = (await clientRef.current!.call(t.host, t.walletId, siteRef.current, 'account')) as Account;
    if (!alive.current || generation.current !== epoch) throw new ConnectionError('disconnected');
    const after = websiteAccess(appState.peek().settings, siteRef.current);
    if (after) throw new ConnectionError(after);
    if (requestedNetwork.current !== undefined && requestedNetwork.current !== a.network)
      throw new ConnectionError('invalid_request', 'This website requested a different network.');
    if (accountRef.current && (a.address !== accountRef.current.address || a.network !== accountRef.current.network))
      throw new ConnectionError('invalid_request', 'Wallet account changed. Reconnect from the website.');
    target.current = t;
    accountRef.current = a;
    setConnected(a);
    post({ type: 'connected', id: connectId.current, account: a });
  }
  async function connectSelected() {
    if (busyRef.current || !selection) return;
    const [host, walletId] = selection.split(':') as [string, string];
    if (host === 'local') {
      setAuthorizing(local.find((w) => w.id === walletId) ?? null);
      return;
    }
    busyRef.current = true;
    setBusy(true);
    setError('');
    try {
      await connectTarget({ host, walletId });
    } catch (e) {
      setError(fault(e).message);
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }
  async function unlock(request: UnlockRequest) {
    if (!authorizing || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    try {
      const access = websiteAccess(appState.peek().settings, siteRef.current);
      if (access) throw new ConnectionError(access);
      const info = await backendRef.current!.unlockWallet(authorizing.id, request);
      if (websiteAccess(appState.peek().settings, siteRef.current)) {
        await backendRef.current!.lockAll();
        return;
      }
      upsertWallet(info);
      markActivity();
      await connectTarget({ host: localHost.current!.id, walletId: info.id });
      setAuthorizing(null);
    } catch (e) {
      setError(fault(e).message);
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }
  async function prepare() {
    const r = pending.current,
      t = target.current;
    if (!r || !t || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    const epoch = generation.current;
    try {
      const p = (await clientRef.current!.call(t.host, t.walletId, siteRef.current, 'prepare', {
        params: r.params,
      })) as PreparedTransaction;
      if (!alive.current || generation.current !== epoch || pending.current !== r) {
        discard();
        return;
      }
      preparedRef.current = p;
      setPrepared(p);
    } catch (e) {
      if (generation.current === epoch) setError(fault(e).message);
    } finally {
      if (generation.current === epoch) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  }
  async function send() {
    const p = preparedRef.current,
      r = pending.current,
      t = target.current;
    if (!p || !r || !t || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    const epoch = generation.current;
    try {
      const result = await clientRef.current!.call(t.host, t.walletId, siteRef.current, 'broadcast', { txid: p.txid });
      if (!alive.current || generation.current !== epoch) return;
      finish(result);
      setSent(p.txid);
    } catch (e) {
      if (generation.current !== epoch) return;
      const failure = fault(e);
      finish(undefined, failure);
      setError(failure.message);
    } finally {
      if (generation.current === epoch) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  }
  const fallback = accountRef.current && local.find((w) => w.id === accountRef.current!.walletId);
  async function blockWebsite() {
    try {
      await setWebsiteBlocked(siteRef.current, true);
      revoke('website_blocked');
    } catch (e) {
      setError(errorMessage(e));
    }
  }
  const refusal = access ?? denied;
  return (
    <FocusLayout
      width="wide"
      headerAction={
        <Button variant="ghost" onClick={disconnect}>
          Disconnect
        </Button>
      }
    >
      <Stack gap={6}>
        <Stack gap={2}>
          <Heading level={1}>
            {request ? (prepared ? 'Confirm transaction' : 'Review transaction') : connected ? 'Wallet connected' : 'Connect wallet'}
          </Heading>
          {site ? (
            <Text class="connection__origin">{site}</Text>
          ) : (
            <Text tone="muted">{valid ? 'Waiting for the website…' : 'Open this page using a website’s Connect wallet button.'}</Text>
          )}
        </Stack>
        {valid && site && ready && refusal ? (
          <Card padding="lg">
            <Stack gap={4}>
              <Text>{isErrorCode(refusal) ? messageForCode(refusal) : 'Wallet disconnected.'}</Text>
              <a href="./#/settings" target="_blank" rel="noopener noreferrer">
                Open wallet settings
              </a>
              <Text size="sm" tone="muted">
                After updating Settings, connect again from the website.
              </Text>
            </Stack>
          </Card>
        ) : null}
        {valid && site && ready && !refusal ? (
          <>
            {!connected ? (
              <Card padding="lg">
                <Stack gap={4}>
                  <Text>Choose a wallet to share its public addresses with this website. Every transaction requires your approval.</Text>
                  {options.length ? (
                    <Field label="Wallet">
                      <Select value={selection} onValue={setSelected} options={options} disabled={busy} />
                    </Field>
                  ) : (
                    <Text>No wallets found. Open MMX Wallet to create or import one, then return here.</Text>
                  )}
                  <Button variant="primary" loading={busy} disabled={!selection} onClick={() => void connectSelected()}>
                    Connect
                  </Button>
                </Stack>
              </Card>
            ) : (
              <>
                <Text class="connection__address">Account: {connected.address}</Text>
                {request ? (
                  <Card padding="lg">
                    <Stack gap={5}>
                      {prepared ? (
                        <Review prepared={prepared} />
                      ) : (
                        <>
                          <Text>
                            The website requests a transaction on <strong>{request.params.network}</strong>. Review its details and maximum
                            fee before sending.
                          </Text>
                          <details>
                            <summary>Requested transaction</summary>
                            <pre class="connection__json">{JSON.stringify(request.params.transaction, null, 2)}</pre>
                          </details>
                        </>
                      )}
                      <div class="connection__actions">
                        <Button disabled={busy} onClick={reject}>
                          Reject
                        </Button>
                        <Button variant="primary" loading={busy} onClick={() => void (prepared ? send() : prepare())}>
                          {prepared ? 'Approve and send' : 'Review transaction'}
                        </Button>
                      </div>
                    </Stack>
                  </Card>
                ) : (
                  <Text tone="muted">
                    {sent
                      ? 'Transaction sent. You can return to the website.'
                      : 'Waiting for a transaction request. Keep this window open.'}
                  </Text>
                )}
                {sent ? <Text class="connection__address">Transaction: {sent}</Text> : null}
                {fallback ? (
                  <Button
                    variant="ghost"
                    disabled={busy}
                    onClick={() => {
                      discard();
                      preparedRef.current = null;
                      setPrepared(null);
                      setAuthorizing(fallback);
                    }}
                  >
                    Unlock in this window
                  </Button>
                ) : null}
              </>
            )}
          </>
        ) : null}
        {valid && site && ready && !appState.value.settings.blockedWebsites?.includes(site) ? (
          <Button variant="ghost" disabled={busy} onClick={() => void blockWebsite()}>
            Block this website
          </Button>
        ) : null}
        {error ? <Banner tone="warning">{error}</Banner> : null}
        {authorizing ? (
          <AuthorizationDialog
            open
            wallet={authorizing}
            busy={busy}
            error={error || undefined}
            onClose={() => setAuthorizing(null)}
            onAuthorize={(r) => void unlock(r)}
          />
        ) : null}
      </Stack>
    </FocusLayout>
  );
}

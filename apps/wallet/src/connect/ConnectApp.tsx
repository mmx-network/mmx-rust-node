import { useSignalEffect } from '@preact/signals';
import { useEffect, useRef, useState } from 'preact/hooks';
import { isErrorCode, type Backend, type UnlockRequest, type WalletInfo } from '../backend/types';
import { createBackend } from '../backend/web';
import { sanitizeAppState } from '../lib/appstate';
import { errorMessage, messageForCode } from '../lib/errors';
import { websiteAccess } from '../lib/websites';
import { setWebsiteBlocked } from '../state/settings';
import { activeWalletId, appState, lockVersion, platform, setBackend, setWallets, upsertWallet, wallets } from '../state/core';
import { markActivity, startIdleLock } from '../state/idle';
import { applyAppearance } from '../styles';
import {
  AddressText,
  Amount,
  Banner,
  Button,
  Card,
  Field,
  Heading,
  Icon,
  IconButton,
  Identicon,
  Select,
  Spinner,
  Stack,
  Text,
} from '../ui/components';
import { FocusLayout } from '../ui/shell/FocusLayout';
import { AuthorizationDialog } from '../ui/screens/unlock/AuthorizationDialog';
import { WalletClient } from './client';
import { startWalletHost } from './host';
import { WebsiteSessions, type WebsiteSession } from './sessions';
import { TransactionReview } from './TransactionReview';
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
type Flow =
  | { id: string; site: string; action: 'connect'; network?: string }
  | { id: string; site: string; action: 'sendTransaction'; session: string; params: TransactionRequest };
function fault(e: unknown): { code: string; message: string } {
  return e instanceof ConnectionError
    ? { code: e.code, message: isErrorCode(e.code) ? messageForCode(e.code) : e.message }
    : { code: object(e) && typeof e.code === 'string' ? e.code : 'internal', message: errorMessage(e) };
}
export function ConnectApp() {
  const query = useRef(new URLSearchParams(window.location.hash.slice(1))).current;
  const channel = query.get('channel'),
    expected = origin(query.get('origin'));
  const opener = useRef(window.opener as Window | null).current;
  const valid = token(channel) && expected !== null && opener !== null;
  const [ready, setReady] = useState(false);
  const [flow, setFlow] = useState<Flow | null>(null);
  const flowRef = useRef<Flow | null>(null);
  const [offers, setOffers] = useState<Offer[]>([]);
  const [selected, setSelected] = useState('');
  const [account, setAccount] = useState<Account | null>(null);
  const [prepared, setPrepared] = useState<PreparedTransaction | null>(null);
  const preparedRef = useRef<PreparedTransaction | null>(null);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [error, setError] = useState('');
  const [authorizing, setAuthorizing] = useState<WalletInfo | null>(null);
  const [completed, setCompleted] = useState(false);
  const bRef = useRef<Backend>();
  const clientRef = useRef<WalletClient>();
  const hostRef = useRef<ReturnType<typeof startWalletHost>>(null);
  const target = useRef<Target>();
  const grant = useRef<WebsiteSession | null>(null);
  const alive = useRef(true),
    finished = useRef(false),
    submitting = useRef(false);
  const generation = useRef(0);
  const expiry = useRef<ReturnType<typeof setTimeout>>();
  const post = (data: Record<string, unknown>) => {
    if (valid && flowRef.current) opener!.postMessage({ protocol: PROTOCOL, version: VERSION, channel, ...data }, flowRef.current.site);
  };
  const discard = () => {
    const t = target.current;
    if (t) void clientRef.current?.call(t.host, t.walletId, flowRef.current!.site, 'discard').catch(() => undefined);
    preparedRef.current = null;
  };
  const finish = (result?: Record<string, unknown>, failure?: { code: string; message: string }) => {
    if (finished.current || !flowRef.current) return;
    finished.current = true;
    generation.current++;
    clearTimeout(expiry.current);
    discard();
    if (
      failure &&
      submitting.current &&
      ['timeout', 'disconnected', 'user_rejected', 'website_connections_disabled', 'website_blocked', 'locked'].includes(failure.code)
    )
      failure = { code: 'tx_uncertain', message: 'The request ended while sending. Check wallet Activity before trying again.' };
    post({ type: 'result', id: flowRef.current.id, ...(failure ? { error: failure } : { result }) });
    setCompleted(true);
    // The SDK closes immediately after receiving the result. Also close here
    // if the opener went away or does not implement the SDK cleanup.
    setTimeout(() => window.close(), 250);
  };
  const cancel = () => {
    if (!flowRef.current) {
      window.close();
      return;
    }
    finish(undefined, { code: 'user_rejected', message: 'Request rejected.' });
  };
  useSignalEffect(() => {
    applyAppearance(appState.value.settings.theme);
  });
  const access = ready && flow ? websiteAccess(appState.value.settings, flow.site) : null;
  useEffect(() => {
    if (access) finish(undefined, { code: access, message: messageForCode(access) });
  }, [access, ready, flow]);
  const lastLock = useRef(lockVersion.value);
  useSignalEffect(() => {
    const version = lockVersion.value;
    if (version !== lastLock.current) {
      lastLock.current = version;
      finish(undefined, { code: 'locked', message: 'The wallet was locked. Open a new request to unlock it.' });
    }
  });
  useEffect(() => {
    if (!valid) return;
    if (typeof BroadcastChannel === 'undefined') {
      setError('This browser does not support wallet connections. Try a current browser.');
      return;
    }
    const client = new WalletClient();
    clientRef.current = client;
    client.onOffers = setOffers;
    client.onRevoked = (code) => finish(undefined, { code, message: isErrorCode(code) ? messageForCode(code) : 'Wallet disconnected.' });
    const stops: Array<() => void> = [];
    void (async () => {
      const b = await createBackend({ preload: false });
      if (!alive.current) return;
      bRef.current = b;
      setBackend(b);
      platform.value = 'web';
      appState.value = sanitizeAppState(await b.loadAppState(), 'web');
      setWallets(await b.listWallets());
      activeWalletId.value = appState.value.activeWallet;
      if (!alive.current) return;
      stops.push(startIdleLock({ trackFocus: false }));
      hostRef.current = startWalletHost();
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
      if (m.type === 'hello' && token(m.id) && !flowRef.current) {
        let next: Flow;
        if (
          m.action === 'connect' &&
          (m.network === undefined || (typeof m.network === 'string' && m.network.length > 0 && m.network.length <= 256))
        )
          next = { id: m.id, site: event.origin, action: 'connect', ...(typeof m.network === 'string' ? { network: m.network } : {}) };
        else if (m.action === 'sendTransaction' && token(m.session) && transactionRequest(m.params))
          next = { id: m.id, site: event.origin, action: 'sendTransaction', session: m.session, params: structuredClone(m.params) };
        else {
          flowRef.current = { id: m.id, site: event.origin, action: 'connect' };
          finish(undefined, { code: 'invalid_request', message: 'Invalid wallet request.' });
          return;
        }
        flowRef.current = next;
        setFlow(next);
        expiry.current = setTimeout(
          () => finish(undefined, { code: 'timeout', message: 'Wallet approval expired. Request it again from the website.' }),
          REQUEST_TIMEOUT,
        );
      } else if (m.type === 'cancel' && flowRef.current?.id === m.id) cancel();
    };
    window.addEventListener('message', onMessage);
    const announce = () => {
      if (!flowRef.current) opener!.postMessage({ protocol: PROTOCOL, version: VERSION, channel, type: 'ready' }, expected!);
    };
    announce();
    const timer = setInterval(announce, 500);
    const onHide = () => {
      alive.current = false;
      generation.current++;
      clearTimeout(expiry.current);
      if (!finished.current) post({ type: 'closed' });
      client.close();
      hostRef.current?.close();
      void bRef.current?.lockAll();
    };
    window.addEventListener('pagehide', onHide);
    return () => {
      clearInterval(timer);
      window.removeEventListener('message', onMessage);
      window.removeEventListener('pagehide', onHide);
      stops.forEach((s) => s());
      onHide();
    };
  }, []);
  useEffect(() => {
    if (!ready || !flow || finished.current || access || flow.action !== 'sendTransaction') return;
    let saved: WebsiteSession | null;
    try { saved = new WebsiteSessions(window.localStorage).get(flow.session, flow.site); }
    catch { finish(undefined, {code:'io_error', message:messageForCode('io_error')}); return; }
    if (!saved || saved.account.network !== flow.params.network) {
      finish(undefined, { code: 'disconnected', message: 'Connect your wallet again before requesting a transaction.' });
      return;
    }
    if (!wallets.peek().some((w) => w.id === saved.account.walletId && !w.error)) {
      finish(undefined, { code: 'wallet_not_found', message: 'This wallet is no longer available.' });
      return;
    }
    grant.current = saved;
    setAccount(saved.account);
  }, [ready, flow, access]);
  const local = wallets.value.filter((w) => !w.error);
  const options = local.map((w) => {
    const peer = offers.find((o) => o.wallet.id === w.id);
    return { value: peer ? `${peer.host}:${w.id}` : `local:${w.id}`, label: w.name };
  });
  const choice = options.some((o) => o.value === selected) ? selected : (options[0]?.value ?? '');
  const selectedWallet =
    flow?.action === 'sendTransaction' ? local.find((w) => w.id === account?.walletId) : local.find((w) => w.id === choice.split(':')[1]);
  const isUnlocked = !!selectedWallet && (offers.some((o) => o.wallet.id === selectedWallet.id) || !selectedWallet.locked);
  const name = selectedWallet?.name ?? grant.current?.walletName ?? 'Wallet';
  function check(epoch: number) {
    if (!alive.current || finished.current || epoch !== generation.current) throw new ConnectionError('disconnected');
    const denied = websiteAccess(appState.peek().settings, flowRef.current!.site);
    if (denied) throw new ConnectionError(denied);
  }
  async function perform(t: Target) {
    const current = flowRef.current!;
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    const epoch = generation.current;
    try {
      check(epoch);
      const a = (await clientRef.current!.call(t.host, t.walletId, current.site, 'account')) as Account;
      check(epoch);
      if (current.action === 'connect' && current.network !== undefined && current.network !== a.network)
        throw new ConnectionError('invalid_request', 'The website requested a different network.');
      if (
        current.action === 'sendTransaction' &&
        (a.address !== grant.current?.account.address || a.network !== grant.current.account.network)
      )
        throw new ConnectionError('invalid_request', 'The wallet account changed. Connect again from the website.');
      target.current = t;
      setAccount(a);
      if (current.action === 'connect') {
        const saved = new WebsiteSessions(window.localStorage).create(current.site, a, selectedWallet?.name ?? 'Wallet');
        finish({ account: a, session: saved.id, expires: saved.expires });
      } else {
        const quote = (await clientRef.current!.call(t.host, t.walletId, current.site, 'prepare', {
          params: current.params,
        })) as PreparedTransaction;
        if (!alive.current || finished.current || epoch !== generation.current) {
          void clientRef.current?.call(t.host, t.walletId, current.site, 'discard').catch(() => undefined);
          return;
        }
        check(epoch);
        preparedRef.current = quote;
        setPrepared(quote);
      }
    } catch (e) {
      if (!finished.current) {
        const failure = fault(e);
        if (failure.code === 'locked' || failure.code === 'disconnected') setError('Unlock your wallet to continue.');
        else finish(undefined, failure);
      }
    } finally {
      if (epoch === generation.current) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  }
  async function continueRequest() {
    if (!flow || !selectedWallet || busyRef.current) return;
    const peer = offers.find((o) => o.wallet.id === selectedWallet.id);
    if (peer) {
      await perform({ host: peer.host, walletId: selectedWallet.id });
      return;
    }
    if (!selectedWallet.locked && hostRef.current) {
      await perform({ host: hostRef.current.id, walletId: selectedWallet.id });
      return;
    }
    setAuthorizing(selectedWallet);
  }
  async function unlock(request: UnlockRequest) {
    if (!authorizing || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    try {
      const denied = websiteAccess(appState.peek().settings, flowRef.current!.site);
      if (denied) throw new ConnectionError(denied);
      const info = await bRef.current!.unlockWallet(authorizing.id, request);
      if (finished.current) {
        await bRef.current!.lockAll();
        return;
      }
      upsertWallet(info);
      markActivity();
      setAuthorizing(null);
      busyRef.current = false;
      setBusy(false);
      await perform({ host: hostRef.current!.id, walletId: info.id });
    } catch (e) {
      if (!finished.current) setError(fault(e).message);
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }
  async function send() {
    if (!preparedRef.current || !target.current || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    submitting.current = true;
    post({ type: 'submitting', id: flowRef.current!.id });
    try {
      const t = target.current;
      const result = (await clientRef.current!.call(t.host, t.walletId, flowRef.current!.site, 'broadcast', {
        txid: preparedRef.current.txid,
      })) as { txid: string };
      finish(result);
    } catch (e) {
      finish(undefined, fault(e));
    }
  }
  async function block() {
    if (!flow || busyRef.current) return;
    try {
      await setWebsiteBlocked(flow.site, true);
      finish(undefined, { code: 'website_blocked', message: messageForCode('website_blocked') });
    } catch (e) {
      setError(errorMessage(e));
    }
  }
  const title = flow?.action === 'sendTransaction' ? (prepared ? 'Confirm transaction' : 'Review transaction') : 'Connect wallet';
  return (
    <FocusLayout
      class="connection"
      headerAction={<IconButton icon="close" label="Cancel request" disabled={busy} onClick={cancel} />}
      footer={
        ready && flow && !completed && !access ? (
          <div class="connection__footer">
            {prepared ? (
              <div class="connection__approval-fee">
                <Text size="sm" tone="muted">
                  Max. network fee
                </Text>
                <Amount value={BigInt(prepared.maxFee)} decimals={prepared.decimals} symbol="MMX" />
              </div>
            ) : null}
            <div class="connection__actions">
              <Button disabled={busy} onClick={cancel}>
                {flow.action === 'connect' ? 'Cancel' : 'Reject'}
              </Button>
              <Button
                variant="primary"
                icon={prepared ? 'send' : undefined}
                loading={busy}
                disabled={!selectedWallet || !ready || (flow.action === 'sendTransaction' && !account)}
                onClick={() => void (prepared ? send() : continueRequest())}
              >
                {flow.action === 'connect'
                  ? isUnlocked
                    ? 'Connect'
                    : 'Unlock and connect'
                  : prepared
                    ? 'Send now'
                    : isUnlocked
                      ? 'Continue'
                      : 'Unlock to review'}
              </Button>
            </div>

            <Button variant="ghost" size="sm" disabled={busy} onClick={() => void block()}>
              Block this website
            </Button>
          </div>
        ) : undefined
      }
    >
      <Stack gap={4}>
        {flow ? (
          <div class="connection__site">
            <div class="connection__site-icon">
              <Icon name="globe" size="lg" />
            </div>
            <div>
              <span class="connection__eyebrow">{flow.action === 'connect' ? 'Connection request' : 'Transaction request'}</span>
              <strong>{new URL(flow.site).host}</strong>
              <span class="connection__site-origin">{flow.site}</span>
            </div>
          </div>
        ) : null}
        <div class="connection__heading">
          <Heading level={1}>{completed ? 'Request complete' : title}</Heading>
          {flow?.action === 'connect' && !completed ? <Text tone="muted">Choose the wallet you want to connect.</Text> : null}
        </div>
        {!valid ? <Banner tone="info">Open this page from a website’s Connect wallet button.</Banner> : null}
        {valid && (!ready || !flow) ? (
          <div class="connection__loading">
            <Spinner />
            <Text tone="muted">Opening your wallet…</Text>
          </div>
        ) : null}
        {ready && flow && !completed && !access ? (
          <>
            <Card padding="none" class="connection__card">
              {flow.action === 'connect' ? (
                <div class="connection__connect-content">
                  {options.length > 1 ? (
                    <Field label="Wallet">
                      <Select value={choice} onValue={setSelected} options={options} disabled={busy} />
                    </Field>
                  ) : null}
                  {selectedWallet ? (
                    <div class="connection__wallet">
                      <Identicon value={selectedWallet.id} size={40} />
                      <div>
                        <strong>{selectedWallet.name}</strong>
                        <span>{isUnlocked ? 'Ready to connect' : 'Unlock required'}</span>
                      </div>
                      <Icon name={isUnlocked ? 'check-circle' : 'lock'} size="sm" />
                    </div>
                  ) : (
                    <Text tone="muted">Open MMX Wallet to create or import a wallet first.</Text>
                  )}
                  <div class="connection__permissions">
                    <div>
                      <Icon name="eye" />
                      <span>View your public wallet addresses</span>
                    </div>
                    <div>
                      <Icon name="shield" />
                      <span>Every transaction needs your approval</span>
                    </div>
                  </div>
                </div>
              ) : account ? (
                <>
                  <div class="connection__account">
                    <Identicon value={account.walletId} size={28} />
                    <strong>{name}</strong>
                    <AddressText address={account.address} mode="short" />
                  </div>
                  <TransactionReview transaction={flow.params.transaction} account={account} prepared={prepared} />
                </>
              ) : (
                <div class="connection__loading">
                  <Spinner />
                </div>
              )}
            </Card>
            {error ? <Banner tone="warning">{error}</Banner> : null}
          </>
        ) : null}
        {completed ? <Text tone="muted">Returning to the website…</Text> : null}
        {error && (!flow || !ready) ? <Banner tone="warning">{error}</Banner> : null}
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

/** Same-origin signing service. Signing bytes stay in the selected
 * host. An encrypted, one-use unlock handoff lets an approval popup unlock the
 * persistent wallet tab without sending plaintext credentials on the channel. */
import { effect } from '@preact/signals';
import { publicAccount } from './account';
import { openUnlock, unlockKeyPair, unlockPublicKey } from './unlock';
import { errorMessage } from '../lib/errors';
import { isTransportError } from '../lib/errors';
import { appState, backend, platform, wallets, lockVersion, upsertWallet, activeWalletId, sessionWalletId } from '../state/core';
import { websiteAccess } from '../lib/websites';
import { isErrorCode } from '../backend/types';
import { messageForCode } from '../lib/errors';
import { idleExpired, markActivity } from '../state/idle';
import { lock, activateWallet } from '../state/wallets';
import { notify } from '../state/toasts';
import { refreshNow, trackPending } from '../state/session';
import { customPending } from './pending';
import {
  CHANNEL,
  boundedJson,
  ConnectionError,
  object,
  origin,
  randomToken,
  token,
  transactionRequest,
  messageRequest,
  type PreparedTransaction,
} from './protocol';

export function startWalletHost({kind='wallet'}: {kind?: 'wallet'|'approval'} = {}): { id: string; close: () => void } | null {
  if (platform.peek() !== 'web' || typeof BroadcastChannel === 'undefined') return null;
  const channel = new BroadcastChannel(CHANNEL);
  const id = randomToken();
  let alive = true;
  let queue: Promise<void> = Promise.resolve();
  const clients = new Map<
    string,
    { origin: string; walletId: string; seen: number; addresses?: string[]; prepared?: PreparedTransaction;
      challenge?: {pair:CryptoKeyPair;nonce:string;at:number;lock:number} }
  >();
  const post = (v: Record<string, unknown>) => {
    if (alive) channel.postMessage({ ...v, host: id });
  };
  const offer = (bridge?: string) =>
    post({
      type: 'offer', kind, lock:lockVersion.peek(),
      ...(bridge ? { bridge } : {}),
      wallets:
        appState.peek().settings.allowWebsiteConnections === true
          ? wallets
              .peek()
              .filter((w) => !w.error && (kind==='wallet' || !w.locked))
              .map(({ id, name, addressCount, locked, canView }) => ({ id, name, addressCount, locked, canView }))
          : [],
    });
  const stopEffect = effect(() => {
    wallets.value;
    lockVersion.value;
    const settings = appState.value.settings;
    for (const [bridge, client] of clients) {
      const code = websiteAccess(settings, client.origin);
      if (!code) continue;
      if (client.prepared)
        void backend()
          .discardPrepared(client.prepared.txid)
          .catch(() => undefined);
      clients.delete(bridge);
      post({ type: 'revoked', bridge, code });
    }
    offer();
  });
  async function handle(message: Record<string, unknown>): Promise<void> {
    const { bridge, requestId, method, walletId } = message;
    if (!token(bridge) || !token(requestId) || typeof walletId !== 'string' || !origin(message.origin)) return;
    const stillPresent = () => {
      const error = websiteAccess(appState.peek().settings, message.origin as string);
      if (error) throw new ConnectionError(error, messageForCode(error));
      if (!alive || !clients.has(bridge) || Date.now() - Number(message.sentAt) > 120_000) throw new ConnectionError('disconnected');
    };
    const reply = (result: unknown, error?: { code: string; message: string }) =>
      post({ type: 'reply', bridge, requestId, ...(error ? { error } : { result }) });
    try {
      const access = websiteAccess((await backend().loadAppState()).settings, message.origin as string);
      if (access) throw new ConnectionError(access, messageForCode(access));
      if (method === 'account' || method === 'unlockKey') {
        for (const [key, c] of clients) if (Date.now() - c.seen > 30 * 60_000) clients.delete(key);
        if (clients.size >= 16 && !clients.has(bridge)) throw new ConnectionError('busy');
        clients.set(bridge, { origin: message.origin as string, walletId, seen: Date.now() });
      }
      const client = clients.get(bridge);
      if (!client || client.walletId !== walletId || client.origin !== message.origin) throw new ConnectionError('disconnected');
      client.seen = Date.now();
      if (idleExpired()) await lock({ inactivity: true });
      const info = wallets.peek().find((w) => w.id === walletId);
      if (!info || info.error) throw new ConnectionError('locked');
      const b = backend();
      if (method === 'account') {
        const account=await publicAccount(b,walletId);
        const access = websiteAccess((await b.loadAppState()).settings, client.origin);
        if (access) throw new ConnectionError(access, messageForCode(access));
        stillPresent();
        client.addresses = account.addresses;
        reply(account);
      } else if(method==='unlockKey') {
        if(kind!=='wallet') throw new ConnectionError('invalid_request');
        const version=lockVersion.peek();
        if(message.lock!==version) throw new ConnectionError('locked');
        const pair=await unlockKeyPair();
        stillPresent();
        if(version!==lockVersion.peek()) throw new ConnectionError('locked');
        const challenge={pair,nonce:randomToken(),at:Date.now(),lock:version};
        client.challenge=challenge;
        reply({nonce:challenge.nonce,key:await unlockPublicKey(pair)});
      } else if(method==='unlock') {
        const challenge=client.challenge;
        client.challenge=undefined; // Consumed even on invalid ciphertext or a wrong passphrase.
        if(kind!=='wallet' || !challenge || challenge.nonce!==message.nonce ||
          Date.now()-challenge.at>30_000 || challenge.lock!==lockVersion.peek()) throw new ConnectionError('locked');
        const request=await openUnlock(challenge.pair,message.box,{host:id,bridge,walletId,origin:client.origin,nonce:challenge.nonce});
        stillPresent();
        if(challenge.lock!==lockVersion.peek()) throw new ConnectionError('locked');
        try {
          const unlocked=await b.unlockWallet(walletId,request);
          markActivity();
          upsertWallet(unlocked);
          if(activeWalletId.peek()===walletId && sessionWalletId.peek()!==walletId)
            void activateWallet(walletId,{navigateTo:null}).catch(()=>undefined);
          reply(null);
        } finally { delete request.mnemonic; delete request.passphrase; }
      } else if (method === 'signMessage') {
        if (info.locked) throw new ConnectionError('locked');
        if (!b.signMessage || !messageRequest(message.params)) throw new ConnectionError('invalid_request');
        markActivity();
        const proof = await b.signMessage(walletId, message.params, client.origin);
        stillPresent();
        reply(proof);
      } else if (method === 'prepare') {
        if(info.locked) throw new ConnectionError('locked');
        if (!b.prepareTransaction || !transactionRequest(message.params)) throw new ConnectionError('invalid_request');
        if (client.prepared) await b.discardPrepared(client.prepared.txid);
        // Only the wallet's Review button sends this command. Inbound website
        // traffic and discovery heartbeats never extend the signing session.
        markActivity();
        const prepared = await b.prepareTransaction(walletId, message.params, client.origin);
        try {
          stillPresent();
        } catch (e) {
          await b.discardPrepared(prepared.txid);
          throw e;
        }
        client.prepared = prepared;
        reply(prepared);
      } else if (method === 'broadcast') {
        if(info.locked) throw new ConnectionError('locked');
        if (typeof message.txid !== 'string' || client.prepared?.txid !== message.txid) throw new ConnectionError('invalid_request');
        stillPresent();
        const prepared = client.prepared;
        const txid = prepared.txid;
        client.prepared = undefined;
        markActivity();
        try {
          await b.broadcast(walletId, txid);
          trackPending(customPending(prepared, walletId, client.addresses ?? []));
          reply({ txid });
          notify(`Transaction sent for ${client.origin}.`, 'success');
        } catch (e) {
          if (isTransportError(e)) {
            trackPending(customPending(prepared, walletId, client.addresses ?? [], true));
            throw new ConnectionError(
              'tx_uncertain',
              'The network reply was lost. Check wallet Activity before requesting another transaction.',
            );
          }
          throw e;
        } finally {
          refreshNow();
        }
      } else if (method === 'discard') {
        if (client.prepared) await b.discardPrepared(client.prepared.txid);
        client.prepared = undefined;
        reply(null);
      } else if (method === 'disconnect') {
        if (client.prepared) await b.discardPrepared(client.prepared.txid);
        clients.delete(bridge);
        reply(null);
      } else throw new ConnectionError('invalid_request');
    } catch (e) {
      const code = e instanceof ConnectionError ? e.code : object(e) && typeof e.code === 'string' ? e.code : 'internal';
      reply(null, { code, message: isErrorCode(code) ? messageForCode(code) : e instanceof ConnectionError ? e.message : errorMessage(e) });
    }
  }
  channel.onmessage = (event) => {
    const m: unknown = event.data;
    if (!object(m) || !boundedJson(m, 72_000)) return;
    if (m.type === 'discover' && token(m.bridge)) {
      offer(m.bridge);
      return;
    }
    if (m.type === 'cancel' && token(m.bridge) && m.host === id) {
      const client = clients.get(m.bridge);
      if (client?.prepared)
        void backend()
          .discardPrepared(client.prepared.txid)
          .catch(() => undefined);
      clients.delete(m.bridge);
      return;
    }
    if (m.type !== 'call' || m.host !== id || !Number.isFinite(m.sentAt) || Date.now() - Number(m.sentAt) > 120_000) return;
    // Serialize all websites using this signing tab, including balance snapshots.
    queue = queue.then(() => handle(m)).catch(() => undefined);
  };
  const close = () => {
    if (!alive) return;
    post({ type: 'gone' });
    alive = false;
    stopEffect();
    channel.close();
    window.removeEventListener('pagehide', onHide);
    window.removeEventListener('pageshow', onShow);
  };
  const onHide = (event: PageTransitionEvent) => {
    if (event.persisted) post({ type: 'gone' });
    else close();
  };
  const onShow = () => offer();
  window.addEventListener('pagehide', onHide);
  window.addEventListener('pageshow', onShow);
  return { id, close };
}

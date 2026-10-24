/**
 * Send flow: edit → review → broadcast → result. The prepared transaction
 * shown on review is exactly the one broadcast; a second confirm while one is
 * in flight is ignored; a broadcast is never retried automatically.
 */
import { batch, computed, signal } from '@preact/signals';
import { toBackendError, type Contact, type ErrorCode, type FeeLevel, type PreparedSend, type SendRequest } from '../backend/types';
import type { AssetBalance } from '../chain/balances';
import { NATIVE_CONTRACT, normalizeAddress } from '../lib/address';
import { formatAmount, parseAmount, parseAtomic, toInputString, type AmountError } from '../lib/amount';
import { errorMessage, isTransportError, messageForCode } from '../lib/errors';
import { isFeeLevel } from '../lib/fees';
import { getSeparators } from '../lib/locale';
import { memoState } from '../lib/memo';
import { sanitizeSymbol, shortenHash } from '../lib/text';
import { activeWallet, activeWalletId, backend, contacts, sessionWalletId, settings, upsertWallet } from './core';
import { navigate } from './router';
import { assets, ownAddressSet, params, pendingReservedTotals, refreshNow, trackPending } from './session';
import { notify } from './toasts';

export interface SendDraft {
  /** Recipient address (or text being searched in contacts). */
  to: string;
  contract: string;
  /** Amount as typed (locale format). */
  amount: string;
  memo: string;
}

const EMPTY_DRAFT: SendDraft = { to: '', contract: NATIVE_CONTRACT, amount: '', memo: '' };

export const sendDraft = signal<SendDraft>({ ...EMPTY_DRAFT });

export type SendStep = 'edit' | 'review' | 'done' | 'uncertain';
export type SendField = 'to' | 'amount' | 'memo' | 'passphrase' | 'fee';

export interface SendFlowState {
  step: SendStep;
  /** An operation is running; inputs and buttons should be disabled. */
  busy: false | 'preparing' | 'sending' | 'max' | 'unlocking';
  feeLevel: FeeLevel;
  request?: SendRequest;
  prepared?: PreparedSend;
  /** When the transaction was prepared (ms). */
  preparedAt?: number;
  /** The wallet must be unlocked with its passphrase before continuing. */
  needsPassphrase: boolean;
  error?: { code: ErrorCode; message: string; field?: SendField };
}

function initialFlow(): SendFlowState {
  return { step: 'edit', busy: false, feeLevel: settings.value.feeLevel, needsPassphrase: false };
}

export const sendFlow = signal<SendFlowState>({ step: 'edit', busy: false, feeLevel: 1, needsPassphrase: false });

/** Prepared transactions are kept by the backend for 10 minutes. */
export const PREPARED_TTL_MS = 9 * 60_000;
let expiryTimer: ReturnType<typeof setTimeout> | null = null;
/** Bumped whenever the flow is reset; late results of an older flow are dropped. */
let epoch = 0;

export function updateDraft(patch: Partial<SendDraft>): void {
  sendDraft.value = { ...sendDraft.value, ...patch };
  const flow = sendFlow.value;
  if (flow.error && flow.step === 'edit' && !flow.busy) {
    const { error: _error, ...rest } = flow;
    sendFlow.value = rest;
  }
}

/** Opens Send with a fresh form, optionally prefilled (e.g. from a contact). */
export async function startSend(patch: Partial<SendDraft> = {}): Promise<void> {
  await discardSend();
  batch(() => {
    sendDraft.value = { ...EMPTY_DRAFT, ...patch };
    sendFlow.value = initialFlow();
  });
  navigate('send');
}

export interface DraftCheck {
  /** Normalized recipient when valid. */
  recipient?: string;
  /** Contact matching the recipient. */
  contact?: Contact;
  /** Recipient is one of this wallet's addresses. */
  ownAddress: boolean;
  asset?: AssetBalance;
  amountAtomic?: bigint;
  memo?: string;
  errors: { to?: string; amount?: string; memo?: string };
  /** All fields valid. */
  ready: boolean;
}

export function amountErrorMessage(error: AmountError | 'zero' | 'balance', symbol: string, decimals: number): string {
  switch (error) {
    case 'empty':
      return 'Enter an amount.';
    case 'invalid':
      return 'Use digits only.';
    case 'separator':
      return `Use "${getSeparators().decimal}" for decimals, without spaces or separators.`;
    case 'precision':
      return decimals === 0 ? `${symbol} has no decimal places.` : `${symbol} allows up to ${decimals} decimal places.`;
    case 'too_large':
    case 'balance':
      return 'This is more than your balance.';
    case 'zero':
      return 'Enter an amount greater than zero.';
  }
}

const NO_PENDING: ReadonlyMap<string, bigint> = new Map();

/**
 * What can still be sent of an asset: its balance minus what this wallet's
 * sends that are not included yet hold back (`pendingReservedTotals`; MMX
 * includes their fees).
 */
export function spendableBalance(asset: AssetBalance, pending: ReadonlyMap<string, bigint> = NO_PENDING): bigint {
  const left = asset.total - (pending.get(asset.contract) ?? 0n);
  return left > 0n ? left : 0n;
}

/**
 * Validates a draft against the wallet's balances (minus `pending` outgoing
 * amounts per contract) and contacts. Empty fields report errors too; show
 * them once touched.
 */
export function checkDraft(
  draft: SendDraft,
  assetList: readonly AssetBalance[] | null,
  contactList: readonly Contact[],
  own: ReadonlySet<string>,
  pending: ReadonlyMap<string, bigint> = NO_PENDING,
): DraftCheck {
  const errors: DraftCheck['errors'] = {};
  const result: DraftCheck = { ownAddress: false, errors, ready: false };

  const to = draft.to.trim();
  const recipient = normalizeAddress(to);
  if (!to) errors.to = 'Enter an address or choose a contact.';
  else if (!recipient || recipient === NATIVE_CONTRACT) errors.to = 'Enter a valid MMX address.';
  else {
    result.recipient = recipient;
    result.ownAddress = own.has(recipient);
    const contact = contactList.find((c) => c.address === recipient);
    if (contact) result.contact = contact;
  }

  const asset = assetList?.find((a) => a.contract === draft.contract);
  if (asset) result.asset = asset;
  if (!asset) {
    if (assetList) errors.amount = 'Choose an asset you hold.';
  } else {
    const parsed = parseAmount(draft.amount, asset.decimals);
    if (!parsed.ok) errors.amount = amountErrorMessage(parsed.error, asset.symbol, asset.decimals);
    else if (parsed.value === 0n) errors.amount = amountErrorMessage('zero', asset.symbol, asset.decimals);
    else if (parsed.value > spendableBalance(asset, pending)) errors.amount = amountErrorMessage('balance', asset.symbol, asset.decimals);
    else result.amountAtomic = parsed.value;
  }

  const memo = memoState(draft.memo);
  if (memo.error === 'too_long') errors.memo = 'The memo is too long.';
  else if (memo.error === 'invalid') errors.memo = 'Remove line breaks and special characters from the memo.';
  else if (draft.memo !== '') result.memo = draft.memo;

  result.ready = !errors.to && !errors.amount && !errors.memo && result.amountAtomic !== undefined && !!result.recipient;
  return result;
}

export const draftCheck = computed(() =>
  checkDraft(sendDraft.value, assets.value, contacts.value, ownAddressSet.value, pendingReservedTotals.value),
);

/** Sendable assets: MMX plus tokens with a balance. */
export const sendableAssets = computed(() => (assets.value ?? []).filter((a) => a.native || a.total > 0n));

function fieldFor(code: ErrorCode): SendField | undefined {
  switch (code) {
    case 'invalid_address':
      return 'to';
    case 'invalid_amount':
    case 'insufficient_funds':
    case 'insufficient_fee':
      return 'amount';
    case 'invalid_memo':
      return 'memo';
    case 'invalid_passphrase':
      return 'passphrase';
    default:
      return undefined;
  }
}

function errorState(e: unknown): NonNullable<SendFlowState['error']> {
  const err = toBackendError(e);
  const field = fieldFor(err.code);
  return field ? { code: err.code, message: messageForCode(err.code), field } : { code: err.code, message: errorMessage(err) };
}

function needsUnlock(code: ErrorCode): boolean {
  return code === 'locked' || code === 'passphrase_required';
}

function clearExpiry(): void {
  if (expiryTimer !== null) clearTimeout(expiryTimer);
  expiryTimer = null;
}

/** Leaves an expired review: the backend no longer keeps the transaction. */
function expireReview(txid: string): void {
  const flow = sendFlow.value;
  if (flow.step !== 'review' || flow.prepared?.txid !== txid || flow.busy) return;
  void backend().discardPrepared(txid).catch(() => undefined);
  sendFlow.value = {
    step: 'edit',
    busy: false,
    feeLevel: flow.feeLevel,
    needsPassphrase: false,
    error: { code: 'prepared_not_found', message: messageForCode('prepared_not_found') },
  };
}

function armExpiry(txid: string, delay = PREPARED_TTL_MS): void {
  clearExpiry();
  expiryTimer = setTimeout(() => {
    expiryTimer = null;
    expireReview(txid);
  }, Math.max(0, delay));
}

async function prepare(request: SendRequest): Promise<void> {
  const started = epoch;
  const { error: _error, ...flow } = sendFlow.value;
  sendFlow.value = { ...flow, busy: 'preparing', feeLevel: request.feeLevel, needsPassphrase: false };
  try {
    const prepared = await backend().prepareSend(request);
    if (started !== epoch || activeWalletId.value !== request.walletId) {
      void backend().discardPrepared(prepared.txid).catch(() => undefined);
      return;
    }
    if (!matchesRequest(prepared, request)) {
      void backend().discardPrepared(prepared.txid).catch(() => undefined);
      sendFlow.value = { step: 'edit', busy: false, feeLevel: request.feeLevel, needsPassphrase: false, error: errorState(null) };
      return;
    }
    sendFlow.value = { step: 'review', busy: false, feeLevel: request.feeLevel, request, prepared, preparedAt: Date.now(), needsPassphrase: false };
    armExpiry(prepared.txid);
  } catch (e) {
    if (started !== epoch) return;
    const state = errorState(e);
    sendFlow.value = {
      step: 'edit',
      busy: false,
      feeLevel: request.feeLevel,
      request,
      needsPassphrase: needsUnlock(state.code),
      ...(needsUnlock(state.code) ? {} : { error: state }),
    };
  }
}

/** The backend's prepared transaction must describe exactly what was requested. */
export function matchesRequest(p: PreparedSend, r: SendRequest): boolean {
  return (
    normalizeAddress(p.to) === normalizeAddress(r.to) &&
    p.contract === r.contract &&
    parseAtomic(p.amount) === parseAtomic(r.amount) &&
    (p.memo ?? undefined) === (r.memo ?? undefined) &&
    parseAtomic(p.fee) !== null
  );
}

/** Validates the draft and prepares the transaction for review. Returns false if the draft is invalid. */
export async function reviewSend(): Promise<boolean> {
  const flow = sendFlow.value;
  if (flow.busy) return false;
  const walletId = activeWalletId.value;
  const check = draftCheck.value;
  if (!walletId || !check.ready || !check.recipient || check.amountAtomic === undefined) return false;
  const request: SendRequest = {
    walletId,
    to: check.recipient,
    contract: sendDraft.value.contract,
    amount: check.amountAtomic.toString(),
    feeLevel: isFeeLevel(flow.feeLevel) ? flow.feeLevel : settings.value.feeLevel,
  };
  if (check.memo !== undefined) request.memo = check.memo;
  await prepare(request);
  return sendFlow.value.step === 'review';
}

const PRIORITY_TOO_HIGH = 'Not enough MMX for this priority.';

/**
 * Changes the fee priority on review by preparing a new transaction. The
 * current one stays until the new one is ready; when the balance cannot pay
 * the higher fee, the review keeps the current priority and says so.
 */
export async function setSendFeeLevel(level: FeeLevel): Promise<void> {
  const flow = sendFlow.value;
  if (flow.busy || flow.step !== 'review' || !flow.request || !flow.prepared || flow.feeLevel === level) return;
  const current = flow.prepared;
  const started = epoch;
  const { error: _error, ...review } = flow;
  const request: SendRequest = { ...flow.request, feeLevel: level };
  // "Send now" is disabled while busy, so the current transaction cannot be sent meanwhile.
  sendFlow.value = { ...review, busy: 'preparing' };
  let prepared: PreparedSend;
  try {
    prepared = await backend().prepareSend(request);
  } catch (e) {
    if (started !== epoch) return;
    const err = toBackendError(e);
    if (err.code === 'insufficient_funds' || err.code === 'insufficient_fee') {
      sendFlow.value = { ...review, busy: false, error: { code: err.code, message: PRIORITY_TOO_HIGH, field: 'fee' } };
      // The expiry timer does nothing while busy: catch up if it was due.
      const age = Date.now() - (review.preparedAt ?? 0);
      if (age >= PREPARED_TTL_MS) expireReview(current.txid);
      else if (expiryTimer === null) armExpiry(current.txid, PREPARED_TTL_MS - age);
      return;
    }
    clearExpiry();
    void backend().discardPrepared(current.txid).catch(() => undefined);
    const state = errorState(e);
    sendFlow.value = {
      step: 'edit',
      busy: false,
      feeLevel: flow.feeLevel,
      request: flow.request,
      needsPassphrase: needsUnlock(state.code),
      ...(needsUnlock(state.code) ? {} : { error: state }),
    };
    return;
  }
  if (started !== epoch) {
    void backend().discardPrepared(prepared.txid).catch(() => undefined);
    return;
  }
  clearExpiry();
  void backend().discardPrepared(current.txid).catch(() => undefined);
  if (activeWalletId.value !== request.walletId || !matchesRequest(prepared, request)) {
    void backend().discardPrepared(prepared.txid).catch(() => undefined);
    sendFlow.value = { step: 'edit', busy: false, feeLevel: flow.feeLevel, needsPassphrase: false, error: errorState(null) };
    return;
  }
  sendFlow.value = { step: 'review', busy: false, feeLevel: level, request, prepared, preparedAt: Date.now(), needsPassphrase: false };
  armExpiry(prepared.txid);
}

/** Unlocks the wallet with its passphrase, then continues to review. */
export async function unlockForSend(passphrase: string): Promise<boolean> {
  const flow = sendFlow.value;
  const id = activeWalletId.value;
  if (flow.busy || !id || !passphrase) return false;
  const started = epoch;
  sendFlow.value = { ...flow, busy: 'unlocking' };
  try {
    upsertWallet(await backend().unlockWallet(id, { passphrase }));
  } catch (e) {
    if (started === epoch) sendFlow.value = { ...flow, busy: false, needsPassphrase: true, error: errorState(e) };
    return false;
  }
  if (started !== epoch) return false;
  sendFlow.value = { ...flow, busy: false, needsPassphrase: false };
  if (flow.request) {
    await prepare(flow.request);
    return sendFlow.value.step === 'review';
  }
  return reviewSend();
}

/**
 * Symbol shown for a prepared transfer: MMX, the token's symbol, or (for a
 * token whose symbol is empty) its shortened contract, as in the asset list.
 */
export function preparedSymbol(prepared: Pick<PreparedSend, 'contract' | 'symbol'>): string {
  if (prepared.contract === NATIVE_CONTRACT) return 'MMX';
  return (
    sanitizeSymbol(prepared.symbol) ||
    assets.value?.find((a) => a.contract === prepared.contract)?.symbol ||
    shortenHash(prepared.contract, 8, 4)
  );
}

const UNCERTAIN_MESSAGE = 'We couldn’t confirm the transfer. Check Activity before sending again.';

/**
 * A broadcast finished after the user switched wallets or locked: the result
 * belongs to the earlier wallet, so it is reported in a toast only.
 */
function reportLateResult(message: string, kind: 'success' | 'warning' | 'error'): void {
  if (sessionWalletId.value !== null) notify(message, kind);
}

/** Broadcasts the reviewed transaction once. */
export async function confirmSend(): Promise<void> {
  const flow = sendFlow.value;
  if (flow.busy || flow.step !== 'review' || !flow.prepared || !flow.request) return;
  const { prepared, request } = flow;
  const started = epoch;
  // Set synchronously: a second click sees busy and returns.
  sendFlow.value = { ...flow, busy: 'sending' };
  clearExpiry();
  // Reviewed on this server's network (the backend refuses it on another).
  const network = params.peek()?.network;
  const pending = {
    txid: prepared.txid,
    walletId: request.walletId,
    to: normalizeAddress(prepared.to) ?? prepared.to,
    contract: prepared.contract,
    symbol: preparedSymbol(prepared),
    decimals: prepared.decimals,
    amount: parseAtomic(prepared.amount) ?? 0n,
    fee: parseAtomic(prepared.fee) ?? 0n,
    expires: prepared.expires,
    createdAt: Date.now(),
    status: 'pending' as const,
    ...(network ? { network } : {}),
    ...(prepared.memo ? { memo: prepared.memo } : {}),
  };
  try {
    await backend().broadcast(request.walletId, prepared.txid);
  } catch (e) {
    if (isTransportError(e)) {
      // Tracked for its wallet even when another one is open now.
      trackPending({ ...pending, uncertain: true });
      if (started !== epoch) reportLateResult(UNCERTAIN_MESSAGE, 'warning');
      else sendFlow.value = { ...flow, step: 'uncertain', busy: false };
      return;
    }
    // The backend consumed or rejected the prepared transaction.
    if (started !== epoch) reportLateResult(errorMessage(e), 'error');
    else sendFlow.value = { step: 'edit', busy: false, feeLevel: flow.feeLevel, request, needsPassphrase: false, error: errorState(e) };
    return;
  }
  trackPending(pending);
  refreshNow();
  if (started !== epoch) reportLateResult('Transfer sent', 'success');
  else sendFlow.value = { ...flow, step: 'done', busy: false };
}

/** Leaves review without sending. */
export async function backToEdit(): Promise<void> {
  const flow = sendFlow.value;
  if (flow.busy === 'sending') return;
  epoch++;
  clearExpiry();
  if (flow.prepared && flow.step === 'review') await backend().discardPrepared(flow.prepared.txid).catch(() => undefined);
  sendFlow.value = { step: 'edit', busy: false, feeLevel: flow.feeLevel, needsPassphrase: false };
}

/** Clears the form after a finished send. */
export function finishSend(): void {
  epoch++;
  clearExpiry();
  batch(() => {
    sendDraft.value = { ...EMPTY_DRAFT };
    sendFlow.value = initialFlow();
  });
}

/** Discards any prepared transaction and resets the flow (wallet switch, lock). */
export async function discardSend(): Promise<void> {
  const flow = sendFlow.value;
  epoch++;
  clearExpiry();
  batch(() => {
    sendDraft.value = { ...EMPTY_DRAFT };
    sendFlow.value = initialFlow();
  });
  // A transaction being sent belongs to its broadcast now.
  if (flow.prepared && flow.step === 'review' && flow.busy !== 'sending') {
    await backend().discardPrepared(flow.prepared.txid).catch(() => undefined);
  }
}

/** Fills the amount with the maximum sendable for the selected asset (after fees). */
export async function fillMaxAmount(): Promise<void> {
  const flow = sendFlow.value;
  const id = activeWalletId.value;
  const asset = assets.value?.find((a) => a.contract === sendDraft.value.contract);
  if (flow.busy || !id || !asset) return;
  const started = epoch;
  sendFlow.value = { ...flow, busy: 'max' };
  const draft = sendDraft.value;
  const recipient = normalizeAddress(draft.to);
  try {
    const request: Omit<SendRequest, 'amount'> = {
      walletId: id,
      to: recipient && recipient !== NATIVE_CONTRACT ? recipient : '',
      contract: asset.contract,
      feeLevel: flow.feeLevel,
    };
    if (draft.memo && !memoState(draft.memo).error) request.memo = draft.memo;
    const max = parseAtomic(await backend().maxSendable(request));
    if (started !== epoch || activeWalletId.value !== id) return;
    if (max === null) throw new Error('invalid');
    if (max === 0n) {
      sendFlow.value = { ...sendFlow.value, busy: false, error: { code: 'insufficient_fee', message: messageForCode('insufficient_fee'), field: 'amount' } };
      return;
    }
    sendDraft.value = { ...sendDraft.value, amount: toInputString(max, asset.decimals) };
    sendFlow.value = { ...sendFlow.value, busy: false };
  } catch (e) {
    if (started !== epoch) return;
    const state = errorState(e);
    sendFlow.value = {
      ...sendFlow.value,
      busy: false,
      needsPassphrase: needsUnlock(state.code),
      ...(needsUnlock(state.code) ? {} : { error: state }),
    };
  }
}

/** Total for review: amount + fee when sending MMX, otherwise just the fee in MMX. */
export function reviewTotals(p: PreparedSend): { amount: bigint; fee: bigint; totalNative: bigint | null } {
  const amount = parseAtomic(p.amount) ?? 0n;
  const fee = parseAtomic(p.fee) ?? 0n;
  return { amount, fee, totalNative: p.contract === NATIVE_CONTRACT ? amount + fee : null };
}

/** "12.5 MMX" for a prepared amount. */
export function formatPrepared(atomic: bigint, decimals: number, symbol: string): string {
  return `${formatAmount(atomic, decimals)} ${symbol}`;
}

/** The wallet requires its passphrase for sending (desktop locked wallet). */
export const sendNeedsPassphrase = computed(() => sendFlow.value.needsPassphrase && activeWallet.value?.withPassphrase === true);

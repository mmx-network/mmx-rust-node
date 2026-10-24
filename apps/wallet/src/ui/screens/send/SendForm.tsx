import type { ComponentChildren } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import type { AssetBalance } from '../../../chain/balances';
import { parseAmount } from '../../../lib/amount';
import { messageForCode } from '../../../lib/errors';
import { MEMO_MAX_BYTES, memoState } from '../../../lib/memo';
import {
  activeLabels,
  activeWallet,
  assets,
  backToEdit,
  balancesLoaded,
  chainError,
  draftCheck,
  fillMaxAmount,
  ownAddressIndex,
  pendingReservedTotals,
  reviewSend,
  searchContacts,
  sendableAssets,
  sendDraft,
  sendFlow,
  sendNeedsPassphrase,
  spendableBalance,
  unlockForSend,
  updateDraft,
} from '../../../state';
import { canPasteFromClipboard, readClipboardText, recipientFromText } from '../../../state/actions/send';
import {
  Amount,
  Banner,
  Button,
  Card,
  Combobox,
  Dialog,
  Field,
  Icon,
  IconButton,
  Identicon,
  Page,
  PageHeader,
  PasswordInput,
  Select,
  shortenAddress,
  Skeleton,
  Stack,
  TextInput,
  type ComboboxOption,
  type SelectOption,
} from '../../components';
import { PASSPHRASE_FIELD } from '../welcome/flow';

/** Contacts offered in the To field. */
const SUGGESTIONS = 8;

function assetLabel(a: AssetBalance): string {
  if (a.native) return 'MMX';
  return a.verified ? a.symbol : `${a.symbol} · ${shortenAddress(a.contract)}`;
}

type Touched = Partial<Record<'to' | 'amount', boolean>>;

/** Desktop: asks for the passphrase when the wallet was locked in the background. */
function PassphraseDialog() {
  const open = sendNeedsPassphrase.value;
  const flow = sendFlow.value;
  const wallet = activeWallet.value;
  const [passphrase, setPassphrase] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const busy = flow.busy === 'unlocking' || flow.busy === 'preparing';
  const error = flow.error?.field === 'passphrase' ? flow.error.message : undefined;
  const other = flow.error && flow.error.field !== 'passphrase' ? flow.error.message : undefined;

  const close = (): void => {
    setPassphrase('');
    void backToEdit();
  };
  const submit = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy || !passphrase) return;
    const ok = await unlockForSend(passphrase);
    if (ok) setPassphrase('');
    else inputRef.current?.select();
  };

  if (!open) return null;
  return (
    <Dialog
      open
      onClose={close}
      dismissible={!busy}
      size="sm"
      title="Enter passphrase"
      description={wallet ? `Unlock ${wallet.name} to send.` : undefined}
      footer={
        <>
          <Button variant="secondary" disabled={busy} onClick={close}>
            Cancel
          </Button>
          <Button variant="primary" type="submit" form="send-passphrase" loading={busy} disabled={!passphrase}>
            Unlock
          </Button>
        </>
      }
    >
      <form id="send-passphrase" class="send-passphrase" onSubmit={submit} noValidate>
        {other ? <Banner tone="danger">{other}</Banner> : null}
        <Field label="Passphrase" error={error}>
          <PasswordInput
            name="wallet-passphrase"
            {...PASSPHRASE_FIELD}
            value={passphrase}
            onValue={setPassphrase}
            inputRef={inputRef}
            data-autofocus=""
          />
        </Field>
      </form>
    </Dialog>
  );
}

export function SendForm() {
  const draft = sendDraft.value;
  const flow = sendFlow.value;
  const check = draftCheck.value;
  const loaded = balancesLoaded.value;
  const offline = chainError.value;
  const [touched, setTouched] = useState<Touched>({});
  const [submitted, setSubmitted] = useState(false);
  /** Review was pressed while the balance was still loading: continue once it is in. */
  const [waiting, setWaiting] = useState(false);
  const toRef = useRef<HTMLInputElement>(null);
  const amountRef = useRef<HTMLInputElement>(null);
  const memoRef = useRef<HTMLInputElement>(null);
  const busy = flow.busy !== false;
  const touch = (field: keyof Touched) => () => setTouched((t) => (t[field] ? t : { ...t, [field]: true }));

  // Asset choices: MMX and tokens with a balance (plus the selected one if it ran out).
  const choices = sendableAssets.value.slice();
  const current = assets.value?.find((a) => a.contract === draft.contract);
  if (current && !choices.includes(current)) choices.push(current);
  const asset = check.asset;
  // What is left after this wallet's sends that are not included yet.
  const spendable = asset ? spendableBalance(asset, pendingReservedTotals.value) : 0n;
  const held = choices.some((a) => a.contract === draft.contract);
  const assetOptions: SelectOption[] = choices.map((a) => ({ value: a.contract, label: assetLabel(a) }));
  // An asset prefilled from elsewhere that this wallet does not hold: keep it visible but ask for another.
  if (!held && loaded) assetOptions.unshift({ value: draft.contract, label: 'Choose an asset', disabled: true });

  // To
  const toText = draft.to;
  const recipient = check.recipient;
  const suggestions: ComboboxOption[] = recipient
    ? []
    : searchContacts(toText, SUGGESTIONS).map((c) => ({
        id: c.address,
        label: c.name,
        description: shortenAddress(c.address),
        leading: <Identicon value={c.address} size={28} />,
      }));
  let toHint: ComponentChildren;
  if (check.contact) {
    toHint = (
      <span class="send-form__match">
        <Icon name="contacts" size="sm" />
        {check.contact.name}
        {check.contact.note ? <span class="send-form__note">{check.contact.note}</span> : null}
      </span>
    );
  } else if (recipient && check.ownAddress) {
    const index = ownAddressIndex(recipient);
    const label = activeLabels.value[index];
    toHint = (
      <span class="send-form__match send-form__match--own">
        <Icon name="info" size="sm" />
        {label ? `One of your addresses (${label})` : `One of your addresses (#${index + 1})`}
      </span>
    );
  }
  let toError: string | undefined;
  if (flow.error?.field === 'to') toError = flow.error.message;
  else if (check.errors.to && (submitted || touched.to)) {
    const text = toText.trim();
    if (!text) toError = submitted ? check.errors.to : undefined;
    else toError = /^mmx1/i.test(text) ? check.errors.to : 'Enter an MMX address or choose a contact.';
  }

  // Amount
  let amountError: string | undefined;
  if (flow.error?.field === 'amount') amountError = flow.error.message;
  else if (!asset) amountError = submitted ? (loaded ? check.errors.amount : 'Your balance is still loading.') : undefined;
  else if (check.errors.amount) {
    const text = draft.amount.trim();
    const parsed = text ? parseAmount(text, asset.decimals) : null;
    // Format, precision and balance problems show while typing; an empty or zero amount once left.
    const immediate = parsed !== null && (!parsed.ok || parsed.value > spendable);
    if (submitted || touched.amount || immediate) amountError = check.errors.amount;
  }

  // Memo
  const memo = memoState(draft.memo);
  const memoError = flow.error?.field === 'memo' ? flow.error.message : check.errors.memo;

  const generalError =
    flow.error && !sendNeedsPassphrase.value && (!flow.error.field || flow.error.field === 'passphrase') ? flow.error.message : undefined;

  const paste = async (): Promise<void> => {
    const text = await readClipboardText();
    if (text !== null) {
      updateDraft({ to: recipientFromText(text) });
      setTouched((t) => ({ ...t, to: true }));
    }
    toRef.current?.focus();
  };

  const review = async (): Promise<void> => {
    if (sendFlow.value.busy) return;
    setSubmitted(true);
    if (!balancesLoaded.value && !chainError.value) {
      setWaiting(true);
      return;
    }
    const c = draftCheck.value;
    if (!c.ready) {
      const first = c.errors.to ? toRef : c.errors.amount || c.amountAtomic === undefined ? amountRef : memoRef;
      first.current?.focus();
      return;
    }
    await reviewSend();
  };

  const submit = (event: Event): void => {
    event.preventDefault();
    void review();
  };

  useEffect(() => {
    if (!waiting) return;
    if (loaded) {
      setWaiting(false);
      void review();
    } else if (offline) {
      setWaiting(false);
    }
  }, [waiting, loaded, offline]);

  const available = asset ? (
    <span class="send-form__available">
      Available <Amount value={spendable} decimals={asset.decimals} symbol={asset.symbol} size="sm" />
    </span>
  ) : !loaded ? (
    <Skeleton width="8rem" />
  ) : undefined;

  return (
    <Page width="narrow" class="send">
      <PageHeader title="Send" />
      {offline && !loaded ? <Banner tone="warning">{messageForCode(offline)}</Banner> : null}
      <Card padding="lg" class="send-card">
        <form class="send-form" onSubmit={submit} noValidate>
          <Stack gap={6}>
            <Field label="To" error={toError} hint={toHint}>
              <Combobox
                value={toText}
                onValue={(v) => updateDraft({ to: v })}
                options={suggestions}
                listLabel="Contacts"
                autoHighlight={toText.trim() !== ''}
                onSelect={(o) => {
                  updateDraft({ to: o.id });
                  setTouched((t) => ({ ...t, to: true }));
                  queueMicrotask(() => amountRef.current?.focus());
                }}
                onBlur={touch('to')}
                inputRef={toRef}
                readOnly={busy}
                mono={recipient !== undefined || /^mmx1/i.test(toText.trim())}
                sensitive
                placeholder="Address or contact name"
                enterkeyhint="next"
                leading={recipient ? <Identicon value={recipient} size={22} /> : <Icon name="search" />}
                trailing={
                  toText ? (
                    <IconButton
                      icon="close"
                      label="Clear"
                      size="sm"
                      disabled={busy}
                      onClick={() => {
                        updateDraft({ to: '' });
                        toRef.current?.focus();
                      }}
                    />
                  ) : canPasteFromClipboard() ? (
                    <Button size="sm" variant="ghost" disabled={busy} onClick={() => void paste()}>
                      Paste
                    </Button>
                  ) : undefined
                }
              />
            </Field>

            {assetOptions.length > 1 ? (
              <Field
                label="Asset"
                hint={
                  asset && !asset.verified ? (
                    <span class="send-form__warning">
                      <Icon name="warning" size="sm" />
                      Unverified token. Check the contract address.
                    </span>
                  ) : undefined
                }
              >
                <Select
                  value={draft.contract}
                  onValue={(v) => updateDraft({ contract: v })}
                  disabled={busy}
                  options={assetOptions}
                />
              </Field>
            ) : null}

            <Field label="Amount" error={amountError} labelAction={available}>
              <TextInput
                value={draft.amount}
                onValue={(v) => updateDraft({ amount: v })}
                onBlur={touch('amount')}
                inputRef={amountRef}
                readOnly={busy}
                inputMode="decimal"
                autocomplete="off"
                spellcheck={false}
                placeholder="0"
                size="lg"
                enterkeyhint="next"
                class="send-form__amount"
                trailing={
                  <span class="send-form__amount-end">
                    {asset ? <span class="send-form__unit">{asset.symbol}</span> : null}
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={!asset || (busy && flow.busy !== 'max')}
                      loading={flow.busy === 'max'}
                      onClick={() => void fillMaxAmount()}
                    >
                      Max
                    </Button>
                  </span>
                }
              />
            </Field>

            <Field
              label="Memo"
              optional
              error={memoError}
              hint="Anyone can see the memo."
              counter={memo.showCounter ? `${memo.bytes}/${MEMO_MAX_BYTES}` : undefined}
            >
              <TextInput
                value={draft.memo}
                onValue={(v) => updateDraft({ memo: v })}
                inputRef={memoRef}
                readOnly={busy}
                autocomplete="off"
                enterkeyhint="go"
              />
            </Field>

            {generalError ? <Banner tone="danger">{generalError}</Banner> : null}

            <Button type="submit" variant="primary" size="lg" block iconEnd="arrow-right" loading={flow.busy === 'preparing' || waiting} disabled={busy && flow.busy !== 'preparing'}>
              Review
            </Button>
          </Stack>
        </form>
      </Card>
      <PassphraseDialog />
    </Page>
  );
}

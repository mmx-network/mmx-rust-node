import { useEffect, useRef, useState } from 'preact/hooks';
import { formatAmount } from '../../../lib/amount';
import { LABEL_MAX } from '../../../lib/appstate';
import { errorMessage } from '../../../lib/errors';
import { hasUnsafeCharacters } from '../../../lib/text';
import {
  addressRows,
  balancesLoaded,
  copyAddress,
  nativeAsset,
  navigate,
  notify,
  openAddress,
  params,
  setAddressLabel,
  type AddressRow,
} from '../../../state';
import { receiveIndex, selectReceiveAddress } from '../../../state/actions/receive';
import {
  AddressText,
  Amount,
  Button,
  Card,
  CopyButton,
  Field,
  Identicon,
  List,
  ListItem,
  Page,
  PageHeader,
  QRCode,
  Section,
  Select,
  Skeleton,
  TextInput,
} from '../../components';
import './receive.css';


function addressName(row: AddressRow): string {
  return row.label ?? `Address ${row.index + 1}`;
}

function optionLabel(row: AddressRow, decimals: number, withBalance: boolean): string {
  const name = row.label ? `${row.label} · #${row.index + 1}` : `Address ${row.index + 1}`;
  return withBalance && row.native > 0n ? `${name} · ${formatAmount(row.native, decimals, { sign: 'never' })} MMX` : name;
}

function labelError(text: string): string | undefined {
  const trimmed = text.trim();
  if (Array.from(trimmed).length > LABEL_MAX) return `Use up to ${LABEL_MAX} characters.`;
  if (hasUnsafeCharacters(trimmed)) return 'Remove line breaks and special characters.';
  return undefined;
}

function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Inline label editor for the selected address. */
function LabelForm({ row, onDone }: { row: AddressRow; onDone: () => void }) {
  const [text, setText] = useState(row.label ?? '');
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const error = labelError(text);
  const length = Array.from(text.trim()).length;

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.focus();
    el.select();
  }, []);

  const save = async (event: Event): Promise<void> => {
    event.preventDefault();
    if (busy || error) return;
    setBusy(true);
    try {
      await setAddressLabel(row.index, text);
      onDone();
    } catch (e) {
      notify(errorMessage(e), 'error');
      setBusy(false);
    }
  };

  return (
    <form class="receive-label-form" onSubmit={save} noValidate>
      <Field label={`Label for address ${row.index + 1}`} error={error} counter={length > LABEL_MAX - 8 ? `${length}/${LABEL_MAX}` : undefined}>
        <TextInput
          value={text}
          onValue={setText}
          inputRef={inputRef}
          autocomplete="off"
          placeholder="e.g. Farming"
          enterkeyhint="done"
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault();
              event.stopPropagation();
              onDone();
            }
          }}
        />
      </Field>
      <div class="receive-label-form__actions">
        <Button variant="ghost" disabled={busy} onClick={onDone}>
          Cancel
        </Button>
        <Button variant="primary" type="submit" loading={busy} disabled={!!error}>
          Save label
        </Button>
      </div>
    </form>
  );
}

export default function ReceiveScreen() {
  const rows = addressRows.value;
  const selected = receiveIndex.value;
  const row = rows[selected];
  const loaded = balancesLoaded.value;
  const decimals = nativeAsset.value?.decimals ?? params.value?.decimals ?? 6;
  const [editing, setEditing] = useState(false);
  const cardRef = useRef<HTMLDivElement>(null);
  const editRef = useRef<HTMLButtonElement | null>(null);
  const refocus = useRef(false);

  // Switching to another address (or wallet) closes the label editor.
  useEffect(() => setEditing(false), [row?.address]);

  const choose = (index: number, reveal: boolean): void => {
    selectReceiveAddress(index);
    if (reveal && cardRef.current) {
      const rect = cardRef.current.getBoundingClientRect();
      if (rect.top < 0 || rect.top > window.innerHeight * 0.5) {
        cardRef.current.scrollIntoView({ block: 'start', behavior: prefersReducedMotion() ? 'auto' : 'smooth' });
      }
    }
  };

  // Back from the label editor: focus returns to its button.
  useEffect(() => {
    if (editing || !refocus.current) return;
    refocus.current = false;
    editRef.current?.focus();
  }, [editing]);

  const closeEditor = (): void => {
    refocus.current = true;
    setEditing(false);
  };

  return (
    <Page class="receive">
      <PageHeader title="Receive" />

      {!row ? (
        <div aria-busy="true">
          <Card padding="lg" class="receive-card">
            <div class="receive-card__layout">
              <Skeleton shape="rect" width="200px" height="200px" class="receive-card__qr-skeleton" />
              <div class="receive-card__details">
                <Skeleton width="40%" />
                <Skeleton lines={2} />
                <Skeleton width="10rem" height="2.5rem" shape="rect" />
              </div>
            </div>
          </Card>
        </div>
      ) : (
        <div ref={cardRef} class="receive-card-anchor">
          <Card padding="lg" class="receive-card">
            <div class="receive-card__layout">
              <div class="receive-card__qr">
                <QRCode value={row.address} size={200} label={`QR code of ${addressName(row)}`} />
              </div>
              <div class="receive-card__details">
                {rows.length > 1 ? (
                  <Field label="Address">
                    <Select
                      value={String(row.index)}
                      onValue={(v) => choose(Number(v), false)}
                      options={rows.map((r) => ({ value: String(r.index), label: optionLabel(r, decimals, loaded) }))}
                    />
                  </Field>
                ) : (
                  <p class="receive-card__name">{addressName(row)}</p>
                )}
                <AddressText address={row.address} class="receive-card__address" />
                {editing ? (
                  <LabelForm row={row} onDone={closeEditor} />
                ) : (
                  <div class="receive-card__actions">
                    <CopyButton variant="button" buttonVariant="primary" label="Copy address" onCopy={() => copyAddress(row.address)} />
                    <span
                      class="receive-card__edit"
                      ref={(el) => {
                        editRef.current = (el?.firstElementChild as HTMLButtonElement | null) ?? null;
                      }}
                    >
                      <Button icon="tag" onClick={() => setEditing(true)}>
                        {row.label ? 'Edit label' : 'Add label'}
                      </Button>
                    </span>
                    <Button variant="ghost" icon="external" onClick={() => void openAddress(row.address)}>
                      Explorer
                    </Button>
                  </div>
                )}
              </div>
            </div>
          </Card>
        </div>
      )}

      {rows.length > 1 ? (
        <Section
          title="Your addresses"
          actions={
            <Button size="sm" variant="ghost" icon="settings" onClick={() => navigate('settings')}>
              Manage
            </Button>
          }
        >
          <List inset label="Your addresses" class="receive-list">
            {rows.map((r) => (
              <ListItem
                key={r.address}
                selected={r.index === selected}
                leading={<Identicon value={r.address} size={32} />}
                title={addressName(r)}
                subtitle={
                  <span class="receive-list__sub">
                    {r.label ? <span class="receive-list__index">#{r.index + 1}</span> : null}
                    <AddressText address={r.address} mode="short" />
                  </span>
                }
                trailing={
                  loaded ? (
                    <Amount value={r.native} decimals={decimals} symbol="MMX" tone={r.native > 0n ? 'neutral' : 'muted'} />
                  ) : (
                    <Skeleton width="5rem" />
                  )
                }
                onClick={() => choose(r.index, true)}
              />
            ))}
          </List>
        </Section>
      ) : null}
    </Page>
  );
}

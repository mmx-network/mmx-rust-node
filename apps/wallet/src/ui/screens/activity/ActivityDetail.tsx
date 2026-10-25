import { useEffect, useRef, useState } from 'preact/hooks';
import {
  AddressText,
  Amount,
  Badge,
  Banner,
  Button,
  CopyButton,
  EmptyState,
  Icon,
  IconTile,
  KeyValue,
  KeyValueList,
  Sheet,
  Skeleton,
  Stack,
  Text,
} from '../../components';
import {
  activityItem,
  confirmationsFor,
  counterpartyFor,
  describeAddress,
  historyLoaded,
  loadTx,
  memoFor,
  openTransaction,
  ownAddressIndex,
  params,
  statusFor,
  type ActivityItem,
  type ActivityStatus,
} from '../../../state';
import { copyValue, sendAgain, sendAgainDraft } from '../../../state/actions/activity';
import { getLocale } from '../../../lib/locale';
import { formatDateTime } from '../../../lib/time';
import { isTxid } from '../../../lib/explorer';
import { shortenHash } from '../../../lib/text';
import {
  activityIcon,
  activityTitle,
  counterpartyLabel,
  isUnverified,
  rowAmount,
  STATUS_LABELS,
  statusTone,
} from './present';
import { SaveContactDialog } from './SaveContactDialog';
import './activity.css';

export interface ActivityDetailSheetProps {
  /** Key of the item to show; null = closed. */
  itemKey: string | null;
  onClose: () => void;
}

let integerFormat: Intl.NumberFormat | null = null;

function formatInteger(n: number): string {
  integerFormat ??= new Intl.NumberFormat(getLocale(), { maximumFractionDigits: 0, numberingSystem: 'latn' });
  return integerFormat.format(n);
}

function ownAddressName(address: string): string {
  const d = describeAddress(address);
  if (d.name) return d.name;
  const i = ownAddressIndex(address);
  return i >= 0 ? `Address ${i + 1}` : address;
}

function statusHint(item: ActivityItem, status: ActivityStatus): string | undefined {
  if (status === 'confirmed') {
    const n = confirmationsFor(item);
    return n > 0 ? `${formatInteger(n)} ${n === 1 ? 'confirmation' : 'confirmations'}` : undefined;
  }
  if (status === 'expired') return 'Not sent';
  return undefined;
}

interface PartyProps {
  item: ActivityItem;
  onSave: (address: string) => void;
  /** Address just saved as a contact (shows a confirmation). */
  saved: string | null;
}

function SavedNote() {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => ref.current?.focus({ preventScroll: true }), []);
  return (
    <span class="activity-detail__saved" role="status" tabIndex={-1} ref={ref}>
      <Icon name="check" size="sm" />
      Saved to contacts
    </span>
  );
}

function Counterparty({ item, onSave, saved }: PartyProps) {
  const cp = counterpartyFor(item);
  if (cp === null) return null;
  const label = counterpartyLabel(item);
  if (cp === undefined) {
    return (
      <KeyValue label={label}>
        <Skeleton width="9rem" />
      </KeyValue>
    );
  }
  return (
    <KeyValue label={label}>
      <span class="activity-detail__party">
        {cp.name ? <span class="activity-detail__party-name">{cp.name}</span> : null}
        <AddressText address={cp.address} mode="short" onCopy={() => copyValue(cp.address)} />
        {cp.source === 'address' ? (
          <Button size="sm" variant="ghost" icon="user-plus" class="activity-detail__save" onClick={() => onSave(cp.address)}>
            Save to contacts
          </Button>
        ) : cp.source === 'contact' && saved === cp.address ? (
          <SavedNote />
        ) : null}
      </span>
    </KeyValue>
  );
}

function OwnAddresses({ item }: { item: ActivityItem }) {
  if (item.kind !== 'received' && item.kind !== 'reward' && item.kind !== 'moved') return null;
  const raw = item.ownAddresses.length > 0 ? item.ownAddresses : item.to ? [item.to] : [];
  const list = raw.slice().sort((a, b) => ownAddressIndex(a) - ownAddressIndex(b));
  if (list.length === 0) return null;
  return (
    <KeyValue label={item.kind === 'moved' ? (list.length === 1 ? 'Address' : 'Addresses') : 'To'}>
      <span class="activity-detail__own">
        {list.map((a) => (
          <span key={a} class="activity-detail__party">
            <span class="activity-detail__party-name">{ownAddressName(a)}</span>
            <AddressText address={a} mode="short" onCopy={() => copyValue(a)} />
          </span>
        ))}
      </span>
    </KeyValue>
  );
}

function Details({ item, onSave, saved }: PartyProps) {
  const status = statusFor(item);
  const decimals = params.value?.decimals ?? 6;
  const icon = activityIcon(item, status);
  const amount = rowAmount(item, status, decimals);
  const memo = memoFor(item);
  const reward = item.kind === 'reward';
  const unverified = isUnverified(item) || item.amounts.some((a) => !a.native && !a.verified);
  const tokens = item.amounts.filter((a) => !a.native);
  const hint = statusHint(item, status);
  const unsuccessful = status === 'failed' || status === 'expired';

  return (
    <div class="activity-detail">
      <div class="activity-detail__summary">
        <IconTile icon={icon.icon} tone={icon.tone} size="lg" />
        {amount ? (
          <span class={`activity-detail__amount${unsuccessful ? ' activity-detail__amount--void' : ''}`}>
            <Amount
              value={amount.value}
              decimals={amount.decimals}
              symbol={amount.symbol}
              sign={amount.sign}
              tone={amount.tone === 'muted' && !unsuccessful && !unverified ? 'neutral' : amount.tone}
              size="xl"
            />
          </span>
        ) : null}
        {item.amounts.length > 1 ? (
          <Stack gap={1} align="center" class="activity-detail__more">
            {item.amounts
              .filter((a) => a !== item.primary)
              .map((a) => (
                <Amount
                  key={a.contract}
                  value={a.amount}
                  decimals={a.decimals}
                  symbol={a.symbol}
                  sign={a.amount > 0n && item.kind !== 'moved' ? 'always' : 'auto'}
                  tone={a.verified && a.amount > 0n && item.kind !== 'moved' ? 'positive' : 'neutral'}
                />
              ))}
          </Stack>
        ) : null}
      </div>

      {unverified ? (
        <Banner tone="warning" title="Unverified token">
          Anyone can create a token with this name.
        </Banner>
      ) : null}

      <KeyValueList class="activity-detail__list">
        <KeyValue label="Status" hint={hint}>
          <Badge tone={statusTone(status)}>{STATUS_LABELS[status]}</Badge>
        </KeyValue>
        {item.fee > 0n ? (
          <KeyValue label="Network fee">
            <Amount value={item.fee} decimals={decimals} symbol="MMX" />
          </KeyValue>
        ) : null}
        <Counterparty item={item} onSave={onSave} saved={saved} />
        <OwnAddresses item={item} />
        {tokens.map((t) => (
          <KeyValue key={t.contract} label={tokens.length > 1 ? `Token ${t.symbol}` : 'Token'}>
            <span class="activity-detail__token">
              {t.verified ? null : (
                <Badge tone="warning" icon="warning">
                  Unverified
                </Badge>
              )}
              <AddressText address={t.contract} mode="short" onCopy={() => copyValue(t.contract)} copyLabel="Copy token address" />
            </span>
          </KeyValue>
        ))}
        {memo ? (
          <KeyValue label="Memo">
            <Text as="span" class="activity-detail__memo">
              {memo}
            </Text>
          </KeyValue>
        ) : null}
        {!reward && isTxid(item.txid) ? (
          <KeyValue label="Transaction ID">
            <span class="activity-detail__txid">
              <Text as="span" mono title={item.txid}>
                {shortenHash(item.txid, 8, 8)}
              </Text>
              <CopyButton size="sm" label="Copy transaction ID" onCopy={() => copyValue(item.txid)} />
            </span>
          </KeyValue>
        ) : null}
        {item.height !== undefined ? <KeyValue label="Block">{formatInteger(item.height)}</KeyValue> : null}
        <KeyValue label="Time">
          <time dateTime={new Date(item.timeMs).toISOString()}>{formatDateTime(item.timeMs)}</time>
        </KeyValue>
      </KeyValueList>
    </div>
  );
}

/**
 * Details of one activity item: a side panel on desktop, a bottom sheet on
 * phones. Includes "Save to contacts" for unknown counterparties.
 */
export function ActivityDetailSheet({ itemKey, onClose }: ActivityDetailSheetProps) {
  const [saveAddress, setSaveAddress] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const item = itemKey ? activityItem(itemKey) : undefined;
  const txid = item && item.kind !== 'reward' ? item.txid : null;

  useEffect(() => {
    if (txid) void loadTx(txid).catch(() => undefined);
  }, [txid]);

  useEffect(() => {
    setSaveAddress(null);
    setSaved(null);
  }, [itemKey]);

  let body;
  let footer;
  if (item) {
    const memo = memoFor(item);
    const canRepeat = sendAgainDraft(item, memo) !== null;
    const explorer = item.kind !== 'reward' && isTxid(item.txid);
    body = <Details item={item} onSave={setSaveAddress} saved={saved} />;
    footer =
      explorer || canRepeat ? (
        <>
          {explorer ? (
            <Button icon="external" onClick={() => void openTransaction(item.txid)}>
              View in explorer
            </Button>
          ) : null}
          {canRepeat ? (
            <Button variant="primary" icon="send" onClick={() => void sendAgain(item, memo)}>
              Send again
            </Button>
          ) : null}
        </>
      ) : undefined;
  } else if (!historyLoaded.value) {
    body = (
      <div class="activity-detail" aria-busy="true">
        <div class="activity-detail__summary">
          <Skeleton shape="circle" height="56px" />
          <Skeleton width="10rem" height="2rem" />
        </div>
        <Skeleton lines={5} />
      </div>
    );
  } else {
    const key = itemKey ?? '';
    const lookup = isTxid(key) ? key : null;
    body = (
      <EmptyState
        icon="search"
        tone="neutral"
        title="Transaction not found"
        message="It may be older than the activity shown."
        action={
          lookup ? (
            <Button icon="external" onClick={() => void openTransaction(lookup)}>
              View in explorer
            </Button>
          ) : undefined
        }
      />
    );
  }

  return (
    <>
      <Sheet open={itemKey !== null} onClose={onClose} title={item ? activityTitle(item) : 'Transaction'} footer={footer}>
        {body}
      </Sheet>
      <SaveContactDialog key={saveAddress ?? ''} address={saveAddress} onClose={() => setSaveAddress(null)} onSaved={(c) => setSaved(c.address)} />
    </>
  );
}

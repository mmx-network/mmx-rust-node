import { useEffect, useRef } from 'preact/hooks';
import type { FeeLevel } from '../../../backend/types';
import { NATIVE_CONTRACT, normalizeAddress } from '../../../lib/address';
import { availableFeeLevels, feeOption, FEE_OPTIONS } from '../../../lib/fees';
import {
  assets,
  backToEdit,
  confirmSend,
  describeAddress,
  nativeAsset,
  params,
  preparedSymbol,
  reviewTotals,
  sendFlow,
  setSendFeeLevel,
} from '../../../state';
import {
  AddressText,
  Amount,
  Badge,
  Banner,
  Button,
  Card,
  Field,
  Identicon,
  KeyValue,
  KeyValueList,
  Page,
  PageHeader,
  SegmentedControl,
  Spinner,
} from '../../components';
import { MOBILE_QUERY, useMediaQuery } from './useMediaQuery';

/** "Alice" / "Spending" / "Address 3", and what kind of recipient it is. */
function recipientName(address: string): { name: string; note: string } {
  const who = describeAddress(address);
  if (who.source === 'contact') return { name: who.name ?? 'Contact', note: 'Contact' };
  if (who.source === 'own') return { name: who.name ?? 'Your address', note: 'One of your addresses' };
  // Past transfers are only partly loaded, so the address is never called "new".
  return { name: 'Address', note: 'Not in your contacts' };
}

export function SendReview() {
  const flow = sendFlow.value;
  const prepared = flow.prepared;
  const mobile = useMediaQuery(MOBILE_QUERY);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    window.scrollTo({ top: 0 });
    rootRef.current?.focus({ preventScroll: true });
  }, []);

  if (!prepared) return null;
  const to = normalizeAddress(prepared.to) ?? prepared.to;
  const who = recipientName(to);
  const { amount, fee, totalNative } = reviewTotals(prepared);
  const symbol = preparedSymbol(prepared);
  const native = prepared.contract === NATIVE_CONTRACT;
  const info = assets.value?.find((a) => a.contract === prepared.contract);
  const unverified = !native && info?.verified !== true;
  const nativeDecimals = nativeAsset.value?.decimals ?? params.value?.decimals ?? prepared.decimals;
  const preparing = flow.busy === 'preparing';
  const sending = flow.busy === 'sending';
  const levels = availableFeeLevels(params.value?.minFeeRatio);
  const options = FEE_OPTIONS.filter((o) => levels.includes(o.level)).map((o) => ({
    value: String(o.level),
    label: mobile ? o.multiplier : o.label,
  }));
  const priority = feeOption(flow.feeLevel);
  const updating = <Spinner size="sm" label="Updating" />;
  const feeError = flow.error?.field === 'fee' ? flow.error.message : undefined;
  const otherError = flow.error && flow.error.field !== 'fee' ? flow.error.message : undefined;

  return (
    <Page width="narrow" class="send">
      <PageHeader title="Review" />
      <div ref={rootRef} tabIndex={-1} class="send-review" role="region" aria-label="Transfer details">
        <Card padding="lg" class="send-review__card">
          <div class="send-review__hero">
            <span class="send-review__eyebrow">You send</span>
            <Amount value={amount} decimals={prepared.decimals} symbol={symbol} size="xl" />
            {unverified ? (
              <Badge tone="warning" icon="warning">
                Unverified token
              </Badge>
            ) : null}
          </div>

          <KeyValueList class="send-review__details">
            <KeyValue label="To" stacked>
              <div class="send-review__recipient">
                <Identicon value={to} size={36} />
                <div class="send-review__recipient-text">
                  <span class="send-review__recipient-name">{who.name}</span>
                  <span class="send-review__recipient-note">{who.note}</span>
                </div>
              </div>
              <AddressText address={to} class="send-review__address" />
            </KeyValue>
            {!native ? (
              <KeyValue label="Token">
                <span class="send-review__token">
                  {symbol}
                  <AddressText address={prepared.contract} mode="short" />
                </span>
              </KeyValue>
            ) : null}
            <KeyValue label="Network fee" hint={priority.label}>
              {preparing ? updating : <Amount value={fee} decimals={nativeDecimals} symbol="MMX" />}
            </KeyValue>
            {totalNative !== null ? (
              <KeyValue label="Total" emphasis>
                {preparing ? updating : <Amount value={totalNative} decimals={nativeDecimals} symbol="MMX" size="lg" />}
              </KeyValue>
            ) : null}
            {prepared.memo ? (
              <KeyValue label="Memo" stacked>
                <span class="send-review__memo">{prepared.memo}</span>
              </KeyValue>
            ) : null}
          </KeyValueList>

          {options.length > 1 ? (
            <Field label="Priority" class="send-review__priority" error={feeError}>
              <SegmentedControl
                value={String(flow.feeLevel)}
                onChange={(v) => void setSendFeeLevel(Number(v) as FeeLevel)}
                options={options.map((o) => ({ ...o, disabled: sending || (preparing && o.value !== String(flow.feeLevel)) }))}
                block
              />
            </Field>
          ) : null}
        </Card>

        {otherError ? <Banner tone="danger">{otherError}</Banner> : null}

        <div class="send-review__actions">
          <Button variant="secondary" size="lg" disabled={sending} onClick={() => void backToEdit()}>
            Edit
          </Button>
          <Button variant="primary" size="lg" icon="send" loading={sending} disabled={preparing} onClick={() => void confirmSend()}>
            Send now
          </Button>
        </div>
      </div>
    </Page>
  );
}

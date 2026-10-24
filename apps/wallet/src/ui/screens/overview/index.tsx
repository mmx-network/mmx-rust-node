import { useEffect, useState } from 'preact/hooks';
import {
  Amount,
  AssetIcon,
  Banner,
  Button,
  Card,
  Disclosure,
  EmptyState,
  Icon,
  IconButton,
  List,
  ListItem,
  Page,
  Section,
  SegmentedControl,
  shortenAddress,
  Skeleton,
  Text,
} from '../../components';
import {
  activity,
  balanceSeries,
  balancesLoaded,
  chainError,
  historyLoaded,
  nativeAsset,
  navigate,
  openAddress,
  otherAssets,
  params,
  pendingOutgoingTotals,
  recentActivity,
  refreshNow,
  seriesLoading,
  seriesRange,
  sessionWalletId,
  seriesContract,
  setActivityFilter,
  setSeriesContract,
  setSeriesRange,
  type AssetBalance,
  type SeriesRange,
} from '../../../state';
import { balanceChange, hasTokens, pendingNativeOutgoing, verifiedTokens } from '../../../state/actions/overview';
import { NATIVE_CONTRACT } from '../../../lib/address';
import { messageForCode } from '../../../lib/errors';
import { formatAmount } from '../../../lib/amount';
import { formatDate } from '../../../lib/time';
import { ActivityDetailSheet } from '../activity/ActivityDetail';
import { ActivityRow } from '../activity/ActivityRow';
import { BalanceChart } from './BalanceChart';
import './overview.css';

const RANGE_OPTIONS: Array<{ value: SeriesRange; label: string }> = [
  { value: '1W', label: '1W' },
  { value: '1M', label: '1M' },
  { value: '3M', label: '3M' },
  { value: '1Y', label: '1Y' },
  { value: 'ALL', label: 'All' },
];

const RANGE_PHRASE: Record<SeriesRange, string> = {
  '1W': 'past week',
  '1M': 'past month',
  '3M': 'past 3 months',
  '1Y': 'past year',
  ALL: 'all time',
};

function nativeDecimals(): number {
  return nativeAsset.value?.decimals ?? params.value?.decimals ?? 6;
}

function Hero() {
  const native = nativeAsset.value;
  const decimals = nativeDecimals();
  const pending = pendingNativeOutgoing.value;
  const long = native ? formatAmount(native.total, decimals).length > 12 : false;
  return (
    <Card as="section" variant="hero" padding="lg" class="overview-hero" aria-labelledby="overview-balance">
      <div class="overview-hero__balance">
        <h2 id="overview-balance" class="overview-hero__label">
          Total balance
        </h2>
        {native ? (
          <Amount
            value={native.total}
            decimals={decimals}
            symbol="MMX"
            size="display"
            class={long ? 'overview-hero__amount overview-hero__amount--long' : 'overview-hero__amount'}
          />
        ) : (
          <span class="overview-hero__skeleton" aria-busy="true">
            <span class="visually-hidden">Loading balance</span>
            <Skeleton shape="rect" width="min(17rem, 80%)" height="2.75rem" />
          </span>
        )}
        {pending > 0n ? (
          <p class="overview-hero__pending">
            <Icon name="clock" size="sm" />
            <Amount value={-pending} decimals={decimals} symbol="MMX" size="sm" tone="muted" />
            <span>pending</span>
          </p>
        ) : null}
      </div>
      <div class="overview-hero__actions">
        <Button variant="primary" size="lg" icon="send" onClick={() => navigate('send')}>
          Send
        </Button>
        <Button size="lg" icon="receive" onClick={() => navigate('receive')}>
          Receive
        </Button>
      </div>
    </Card>
  );
}

function BalanceHistory() {
  const series = balanceSeries.value;
  const range = seriesRange.value;
  const change = balanceChange.value;
  const decimals = nativeDecimals();
  const phrase = change && !change.complete ? `since ${formatDate(change.start)}` : RANGE_PHRASE[range];
  return (
    <Section
      title="Balance history"
      class="overview-history"
      actions={
        <SegmentedControl size="sm" label="Chart range" value={range} onChange={setSeriesRange} options={RANGE_OPTIONS} class="overview-history__range" />
      }
    >
      <Card class="overview-history__card">
        <p class="overview-history__change">
          {change ? (
            change.delta === 0n ? (
              <span class="overview-history__flat">No change</span>
            ) : (
              <Amount
                value={change.delta}
                decimals={decimals}
                symbol="MMX"
                sign="always"
                tone={change.delta > 0n ? 'positive' : 'neutral'}
              />
            )
          ) : (
            <Skeleton width="9rem" />
          )}
          {change ? <span class="overview-history__phrase">{phrase}</span> : null}
        </p>
        <BalanceChart
          series={series}
          decimals={decimals}
          symbol="MMX"
          range={range}
          loading={seriesLoading.value}
          label={`Balance history, ${phrase}`}
        />
      </Card>
    </Section>
  );
}

function TokenRow({ asset, pending }: { asset: AssetBalance; pending: bigint }) {
  const unverified = !asset.verified;
  return (
    <ListItem
      class={unverified ? 'overview-token overview-token--unverified' : 'overview-token'}
      onClick={() => {
        setActivityFilter({ kind: 'all', contract: asset.contract });
        navigate('activity');
      }}
      leading={<AssetIcon contract={asset.contract} symbol={asset.symbol} size={36} />}
      title={asset.symbol}
      subtitle={unverified ? <span class="overview-token__contract" title={asset.contract}>
            {shortenAddress(asset.contract, 8, 4)}
          </span> : undefined}
      trailing={<Amount value={asset.total} decimals={asset.decimals} />}
      trailingSub={
        pending > 0n ? (
          <span class="overview-token__pending">
            <Amount value={-pending} decimals={asset.decimals} size="sm" tone="muted" /> pending
          </span>
        ) : undefined
      }
      actions={
        unverified ? (
          <IconButton icon="external" size="sm" label={`View ${asset.symbol} token in explorer`} onClick={() => void openAddress(asset.contract)} />
        ) : undefined
      }
    />
  );
}

function Tokens() {
  const tokens = verifiedTokens.value;
  const others = otherAssets.value;
  const pending = pendingOutgoingTotals.value;
  return (
    <Section title="Tokens" class="overview-tokens">
      <div class="overview-tokens__card">
        {tokens.length > 0 ? (
          <List label="Tokens" class="overview-tokens__list">
            {tokens.map((t) => (
              <TokenRow key={t.contract} asset={t} pending={pending.get(t.contract) ?? 0n} />
            ))}
          </List>
        ) : null}
        {others.length > 0 ? (
          <Disclosure icon="warning" title="Other tokens" meta={String(others.length)} class="overview-tokens__other">
            <Text size="sm" tone="muted" class="overview-tokens__note">
              Not verified. Names can imitate other tokens.
            </Text>
            <List label="Other tokens" class="overview-tokens__list">
              {others.map((t) => (
                <TokenRow key={t.contract} asset={t} pending={pending.get(t.contract) ?? 0n} />
              ))}
            </List>
          </Disclosure>
        ) : null}
      </div>
    </Section>
  );
}

function RecentActivity({ onOpen }: { onOpen: (key: string) => void }) {
  const items = recentActivity.value;
  const loaded = historyLoaded.value;
  return (
    <Section
      title="Recent activity"
      class="overview-recent"
      actions={
        loaded && items.length > 0 ? (
          <Button variant="ghost" size="sm" iconEnd="arrow-right" onClick={() => navigate('activity')}>
            View all
          </Button>
        ) : undefined
      }
    >
      {!loaded ? (
        <div class="overview-recent__loading" aria-busy="true">
          <span class="visually-hidden">Loading activity</span>
          {[0, 1, 2].map((i) => (
            <div key={i} class="overview-recent__skeleton">
              <Skeleton shape="circle" height="40px" />
              <span class="overview-recent__skeleton-lines">
                <Skeleton width="40%" />
                <Skeleton width="65%" />
              </span>
              <Skeleton width="4.5rem" />
            </div>
          ))}
        </div>
      ) : items.length === 0 ? (
        <div class="overview-recent__empty">
          <EmptyState size="sm" icon="activity" title="No activity yet" message="Transfers and rewards will show up here." />
        </div>
      ) : (
        <List inset label="Recent activity">
          {items.map((item) => (
            <ActivityRow key={item.key} item={item} onOpen={onOpen} timeMode="relative" />
          ))}
        </List>
      )}
    </Section>
  );
}

export default function OverviewScreen() {
  const [openKey, setOpenKey] = useState<string | null>(null);
  const wallet = sessionWalletId.value;
  const error = chainError.value;
  const tokens = hasTokens.value;

  // A detail of the previous wallet does not belong to the new one.
  useEffect(() => setOpenKey(null), [wallet]);

  // The chart here always shows MMX.
  useEffect(() => {
    if (seriesContract.value !== NATIVE_CONTRACT) setSeriesContract(NATIVE_CONTRACT);
  }, []);

  return (
    <Page width="wide" class="overview">
      <h1 class="visually-hidden">Overview</h1>
      {error && !balancesLoaded.value ? (
        <Banner
          tone="warning"
          action={
            <Button size="sm" icon="refresh" onClick={() => refreshNow()}>
              Try again
            </Button>
          }
        >
          {messageForCode(error)}
        </Banner>
      ) : null}
      <Hero />
      {historyLoaded.value && activity.value.length === 0 ? null : <BalanceHistory />}
      <div class={tokens ? `overview-grid overview-grid--split${verifiedTokens.value.length === 0 ? ' overview-grid--others-only' : ''}` : 'overview-grid'}>
        {tokens ? <Tokens /> : null}
        <RecentActivity onOpen={setOpenKey} />
      </div>
      <ActivityDetailSheet itemKey={openKey} onClose={() => setOpenKey(null)} />
    </Page>
  );
}

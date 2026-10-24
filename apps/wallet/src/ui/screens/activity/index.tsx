import { useEffect, useMemo, useRef } from 'preact/hooks';
import { Button, EmptyState, ListGroup, Page, PageHeader, SegmentedControl, Select, shortenAddress, Skeleton } from '../../components';
import {
  activity,
  activityFilter,
  activityHasMore,
  activityLoadingMore,
  chainError,
  clock,
  groupByDay,
  historyLoaded,
  loadMoreActivity,
  navigate,
  refreshNow,
  route,
  sessionWalletId,
  setActivityFilter,
  visibleActivity,
  type ActivityFilterKind,
} from '../../../state';
import { activityAssetOptions, type ActivityAssetOption } from '../../../state/actions/activity';
import { messageForCode } from '../../../lib/errors';
import { ActivityDetailSheet } from './ActivityDetail';
import { ActivityRow } from './ActivityRow';
import './activity.css';

export { ActivityRow } from './ActivityRow';
export { ActivityDetailSheet } from './ActivityDetail';

const KIND_OPTIONS: Array<{ value: ActivityFilterKind; label: string }> = [
  { value: 'all', label: 'All' },
  { value: 'received', label: 'Received' },
  { value: 'sent', label: 'Sent' },
  { value: 'rewards', label: 'Rewards' },
];

function assetLabel(a: ActivityAssetOption, all: readonly ActivityAssetOption[]): string {
  if (a.native) return 'MMX';
  if (a.verified) {
    const clash = all.some((o) => o !== a && o.symbol === a.symbol);
    return clash ? `${a.symbol} · ${shortenAddress(a.contract)}` : a.symbol;
  }
  return `${a.symbol} · ${shortenAddress(a.contract)} (unverified)`;
}

function LoadingList() {
  return (
    <div class="activity-loading" aria-busy="true">
      <span class="visually-hidden">Loading activity</span>
      {[0, 1].map((g) => (
        <div key={g} class="activity-loading__group">
          <Skeleton width="5rem" class="activity-loading__heading" />
          <div class="activity-loading__card">
            {[0, 1, 2].map((i) => (
              <div key={i} class="activity-loading__row">
                <Skeleton shape="circle" height="40px" />
                <span class="activity-loading__lines">
                  <Skeleton width="45%" />
                  <Skeleton width="70%" />
                </span>
                <Skeleton width="4.5rem" />
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

/** Loads the next page when the "Load more" button scrolls into view (not when it already is). */
function useAutoLoad(ref: { current: HTMLElement | null }, enabled: boolean): void {
  useEffect(() => {
    const el = ref.current;
    if (!el || !enabled || typeof IntersectionObserver === 'undefined') return;
    let visible: boolean | null = null;
    const io = new IntersectionObserver(
      (entries) => {
        const now = entries[entries.length - 1]?.isIntersecting ?? false;
        if (visible === false && now) void loadMoreActivity();
        visible = now;
      },
      { rootMargin: '0px 0px 240px 0px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [enabled]);
}

export default function ActivityScreen() {
  const r = route.value;
  const selectedKey = r.name === 'activity' ? (r.param ?? null) : null;
  const filter = activityFilter.value;
  const loaded = historyLoaded.value;
  const items = visibleActivity.value;
  const now = clock.value;
  const groups = useMemo(() => groupByDay(items, now), [items, now]);
  const options = activityAssetOptions.value;
  const hasMore = activityHasMore.value;
  const loadingMore = activityLoadingMore.value;
  const error = chainError.value;
  const moreRef = useRef<HTMLDivElement>(null);
  const openedHere = useRef(false);
  const closing = useRef(false);
  const walletRef = useRef(sessionWalletId.value);

  useAutoLoad(moreRef, loaded && hasMore && items.length > 0);

  useEffect(() => {
    closing.current = false;
  }, [selectedKey]);

  // Another wallet's data replaces the list: close a detail that belonged to the previous one.
  useEffect(() => {
    const id = sessionWalletId.value;
    if (walletRef.current !== id) {
      walletRef.current = id;
      openedHere.current = false;
      if (selectedKey) navigate('activity', undefined, { replace: true });
    }
  }, [sessionWalletId.value]);

  const open = (key: string) => {
    if (key === selectedKey) return;
    openedHere.current = selectedKey === null;
    navigate('activity', key, { replace: selectedKey !== null });
  };

  const close = () => {
    if (closing.current) return;
    closing.current = true;
    if (openedHere.current) {
      openedHere.current = false;
      window.history.back();
    } else {
      navigate('activity', undefined, { replace: true });
    }
  };

  const contractValid = filter.contract === null || options.some((o) => o.contract === filter.contract);
  useEffect(() => {
    if (loaded && !contractValid) setActivityFilter({ contract: null });
  }, [loaded, contractValid]);

  const filtered = filter.kind !== 'all' || filter.contract !== null;
  const showAssets = options.length > 1 || filter.contract !== null;

  let content;
  if (!loaded) {
    content = error ? (
      <EmptyState
        icon="warning"
        tone="warning"
        title="Activity could not be loaded"
        message={messageForCode(error)}
        action={
          <Button icon="refresh" onClick={() => refreshNow()}>
            Try again
          </Button>
        }
      />
    ) : (
      <LoadingList />
    );
  } else if (activity.value.length === 0) {
    content = (
      <EmptyState
        icon="activity"
        title="No activity yet"
        message="Transfers and rewards will show up here."
        action={
          <Button variant="primary" icon="receive" onClick={() => navigate('receive')}>
            Receive MMX
          </Button>
        }
      />
    );
  } else if (items.length === 0) {
    content = hasMore ? (
      <EmptyState
        icon="search"
        tone="neutral"
        title="No matches in recent activity"
        action={
          <Button icon="clock" loading={loadingMore} onClick={() => void loadMoreActivity()}>
            Search older activity
          </Button>
        }
      />
    ) : (
      <EmptyState
        icon="filter"
        tone="neutral"
        title="No matching activity"
        action={<Button onClick={() => setActivityFilter({ kind: 'all', contract: null })}>Show all activity</Button>}
      />
    );
  } else {
    content = (
      <div class="activity-list">
        {groups.map((g) => (
          <ListGroup key={g.key} title={g.label} inset class="activity-group">
            {g.items.map((item) => (
              <ActivityRow key={item.key} item={item} onOpen={open} selected={item.key === selectedKey} />
            ))}
          </ListGroup>
        ))}
        {hasMore ? (
          <div class="activity-more" ref={moreRef}>
            <Button loading={loadingMore} onClick={() => void loadMoreActivity()}>
              Load more
            </Button>
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <Page width="default" class="activity">
      <PageHeader title="Activity" />
      {activity.value.length > 0 || filtered ? (
        <div class="activity-filters" role="group" aria-label="Filter activity">
          <SegmentedControl
            label="Type"
            value={filter.kind}
            onChange={(kind) => setActivityFilter({ kind })}
            options={KIND_OPTIONS}
            class="activity-filters__kind"
          />
          {showAssets ? (
            <Select
              aria-label="Asset"
              value={filter.contract ?? ''}
              onValue={(v) => setActivityFilter({ contract: v || null })}
              options={[{ value: '', label: 'All assets' }, ...options.map((o) => ({ value: o.contract, label: assetLabel(o, options) }))]}
              class="activity-filters__asset"
            />
          ) : null}
        </div>
      ) : null}
      {content}
      <ActivityDetailSheet itemKey={selectedKey} onClose={close} />
    </Page>
  );
}

import type { ComponentChildren } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import { applyAppearance, type Appearance } from '@wallet/styles';
import { Badge } from '@wallet/ui/components/Badge';
import { BrandMark } from '@wallet/ui/components/Brand';
import { IconButton } from '@wallet/ui/components/IconButton';
import { Menu } from '@wallet/ui/components/Menu';
import { Tooltip } from '@wallet/ui/components/Tooltip';
import { PageHeader } from '@wallet/ui/components/Layout';
import { api, endpoint } from './lib/api';
import { href, parseRoute, searchTarget, sectionOf, type Route, type Section } from './lib/router';
import { useResource } from './lib/resource';
import type { Block, NodeInfo } from './lib/types';
import { Blocks, RecentList } from './ui/lists';
import { BlockPage } from './ui/block';
import { TransactionPage } from './ui/transaction';
import { AddressPage } from './ui/address';
import { FarmerPage } from './ui/farmer';
import { Button, Card, EmptyState, Icon } from './ui/common';

const NAV = [
  { id: 'blocks', label: 'Blocks', icon: 'overview' },
  { id: 'transactions', label: 'Transactions', icon: 'activity' },
  { id: 'farmers', label: 'Farmers', icon: 'reward' },
] as const;

export function storedAppearance(): Appearance {
  try {
    const value = localStorage.getItem('mmx-explorer.appearance');
    if (value === 'light' || value === 'dark') return value;
  } catch {
    /* Appearance remains usable when storage is unavailable. */
  }
  return 'system';
}

function AppearanceMenu({ appearance, onChange }: { appearance: Appearance; onChange: (value: Appearance) => void }) {
  return (
    <Menu
      label="Appearance"
      placement="bottom-end"
      items={(['system', 'light', 'dark'] as const).map((value) => ({
        id: value,
        label: value === 'system' ? 'Use system setting' : `${value === 'light' ? 'Light' : 'Dark'} mode`,
        icon: value === 'system' ? 'monitor' : value === 'light' ? 'sun' : 'moon',
        checked: value === appearance,
        onSelect: () => onChange(value),
      }))}
      trigger={(props) => (
        <IconButton
          {...props}
          label="Appearance"
          icon={appearance === 'system' ? 'monitor' : appearance === 'light' ? 'sun' : 'moon'}
        />
      )}
    />
  );
}

function Search({ routeKey }: { routeKey: string }) {
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    request.current?.abort();
    setBusy(false);
    setError('');
    return () => request.current?.abort();
  }, [routeKey]);

  const submit = async (): Promise<void> => {
    request.current?.abort();
    setError('');
    const target = searchTarget(input);
    if (!target) {
      setBusy(false);
      setError('Enter a valid block height, hash, transaction ID, MMX address or farmer public key.');
      return;
    }
    if (target.kind !== 'hash') {
      location.hash = href(target);
      return;
    }
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    try {
      const header = await api.get<Block>(endpoint('/header', { hash: target.id }), controller.signal);
      if (!controller.signal.aborted)
        location.hash = href(header ? { kind: 'block', hash: target.id } : { kind: 'transaction', id: target.id });
    } catch (reason) {
      if (!controller.signal.aborted)
        setError(reason instanceof Error ? reason.message : 'Search failed. Please try again.');
    } finally {
      if (request.current === controller) setBusy(false);
    }
  };
  return (
    <div class="search-wrap">
      <form
        class={`chain-search${error ? ' chain-search--error' : ''}`}
        role="search"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <Icon name="search" class="chain-search__icon" />
        <label class="visually-hidden" for="chain-search">
          Search the blockchain
        </label>
        <input
          id="chain-search"
          type="search"
          value={input}
          onInput={(event) => setInput(event.currentTarget.value)}
          placeholder="Block height, hash, transaction, address or farmer key"
          spellcheck={false}
          autoComplete="off"
          maxLength={128}
          aria-invalid={!!error}
          aria-describedby={error ? 'search-error' : undefined}
        />
        <Button type="submit" variant="primary" size="sm" aria-label="Search" loading={busy} iconEnd="arrow-right">
          Search
        </Button>
      </form>
      {error ? (
        <p id="search-error" class="search-error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

function Content({ route, network }: { route: Route; network: NodeInfo | null }): ComponentChildren {
  switch (route.kind) {
    case 'blocks':
      return <Blocks {...route} network={network} />;
    case 'transactions':
    case 'farmers':
      return <RecentList kind={route.kind} page={route.page} />;
    case 'block':
      return <BlockPage route={route} />;
    case 'transaction':
      return <TransactionPage id={route.id} />;
    case 'address':
      return <AddressPage {...route} />;
    case 'farmer':
      return <FarmerPage {...route} />;
    default:
      return (
        <div class="explorer-page">
          <PageHeader title="Page not found" />
          <Card>
            <EmptyState
              icon="search"
              title="This explorer link is not valid"
              message="Use the search above or return to the latest blocks."
              action={
                <a class="btn btn--primary btn--md" href={href({ kind: 'blocks' })}>
                  Explore blocks
                </a>
              }
            />
          </Card>
        </div>
      );
  }
}

export function App() {
  const [hash, setHash] = useState(location.hash);
  const route = parseRoute(hash);
  const [appearance, setAppearance] = useState(storedAppearance);
  const main = useRef<HTMLElement>(null);
  const node = useResource<NodeInfo>(endpoint('/node/info'), 300_000);
  const section = sectionOf(route);
  const identity = `${route.kind}:${'id' in route ? route.id : route.kind === 'block' ? ('hash' in route ? route.hash : route.height) : ''}`;
  useEffect(() => {
    const update = (): void => setHash(location.hash);
    window.addEventListener('hashchange', update);
    return () => window.removeEventListener('hashchange', update);
  }, []);
  useEffect(() => {
    applyAppearance(appearance);
    try {
      localStorage.setItem('mmx-explorer.appearance', appearance);
    } catch {
      /* Optional preference. */
    }
  }, [appearance]);
  useEffect(() => {
    const title = route.kind === 'not-found' ? 'Page not found' : route.kind[0]!.toUpperCase() + route.kind.slice(1);
    document.title = `${title} · MMX Explorer`;
    window.scrollTo({ top: 0 });
    const focused = document.activeElement;
    if (!focused || focused === document.body || !focused.isConnected) {
      const heading = main.current?.querySelector('h1');
      heading?.setAttribute('tabindex', '-1');
      heading?.focus({ preventScroll: true });
    }
  }, [hash]);
  const status = node.error
    ? 'Unavailable'
    : node.data
      ? node.data.is_synced === false
        ? 'Syncing'
        : node.data.name
          ? node.data.name.replace(/^mainnet$/, 'Mainnet').replace(/^testnet/i, 'Testnet ')
          : 'Connected'
      : 'Connecting';
  const tone = node.error ? 'negative' : !node.data || node.data.is_synced === false ? 'warning' : 'positive';
  const navItem = (id: Section, label: string, icon: (typeof NAV)[number]['icon'], mobile = false) => (
    <a
      href={href({ kind: id })}
      class={
        mobile
          ? `tabbar__item${section === id ? ' tabbar__item--active' : ''}`
          : `nav-item${section === id ? ' nav-item--active' : ''}`
      }
      aria-current={section === id ? 'page' : undefined}
    >
      <Icon name={icon} class={mobile ? 'tabbar__icon' : 'nav-item__icon'} />
      <span class={mobile ? 'tabbar__label' : 'nav-item__label'}>{label}</span>
    </a>
  );
  return (
    <div class="shell explorer-shell">
      <button type="button" class="skip-link" onClick={() => main.current?.focus()}>
        Skip to content
      </button>
      <aside class="shell__nav" aria-label="Explorer navigation">
        <a class="shell__brand" href={href({ kind: 'blocks' })} aria-label="MMX Explorer home">
          <BrandMark size={32} />
        </a>
        <nav class="shell__navlist" aria-label="Main">
          <ul class="shell__navitems">
            {NAV.map((item) => (
              <li key={item.id}>
                <Tooltip content={item.label} placement="right" describe={false}>
                  {navItem(item.id, item.label, item.icon)}
                </Tooltip>
              </li>
            ))}
          </ul>
        </nav>
        <div class="shell__status">
          <Tooltip content={`Chain server: ${status}`} placement="right">
            <span class={`network-dot network-dot--${tone}`} role="img" aria-label={status} tabIndex={0} />
          </Tooltip>
        </div>
      </aside>
      <main class="shell__main" ref={main} id="main" tabIndex={-1}>
        <div class="shell__content">
          <header class="explorer-top">
            <a class="explorer-brand" href={href({ kind: 'blocks' })}>
              <BrandMark size={25} class="explorer-mobile-mark" />
              <span>
                MMX <span class="muted">Explorer</span>
              </span>
            </a>
            <div class="explorer-top__actions">
              <Badge tone={tone} dot>
                {status}
              </Badge>
              <IconButton
                label="Refresh data"
                icon="refresh"
                onClick={() => window.dispatchEvent(new Event('explorer:refresh'))}
              />
              <AppearanceMenu appearance={appearance} onChange={setAppearance} />
            </div>
          </header>
          <Search routeKey={hash} />
          <Content key={identity} route={route} network={node.data} />
          <footer class="explorer-footer">
            <span>MMX Explorer</span>
            <span>Public chain data. Open to everyone.</span>
          </footer>
        </div>
      </main>
      <nav class="tabbar" aria-label="Mobile navigation">
        <ul class="tabbar__items">
          {NAV.map((item) => (
            <li key={item.id}>{navItem(item.id, item.label, item.icon, true)}</li>
          ))}
        </ul>
      </nav>
    </div>
  );
}

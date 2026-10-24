import { Tooltip } from '../components/Tooltip';
import { cx } from '../components/internal/cx';

export interface NetworkInfo {
  state: 'online' | 'connecting' | 'offline' | 'syncing';
  height?: number;
  network?: string;
}

/** "mainnet" → "Mainnet", "testnet12" → "Testnet 12". */
export function networkName(network?: string): string {
  if (!network) return 'Online';
  const clean = network.replace(/[^A-Za-z0-9_-]/g, '').slice(0, 24);
  const m = /^([A-Za-z]+)(\d+)$/.exec(clean);
  const text = m ? `${m[1]} ${m[2]}` : clean;
  return text ? text.charAt(0).toUpperCase() + text.slice(1) : 'Online';
}

const heightFormat = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

export function networkLabel(info: NetworkInfo): string {
  switch (info.state) {
    case 'online':
      return networkName(info.network);
    case 'syncing':
      return 'Syncing';
    case 'connecting':
      return 'Connecting…';
    case 'offline':
      return 'Offline';
  }
}

function detail(info: NetworkInfo): string {
  if (info.state === 'offline') return 'Can’t reach the network';
  if (info.state === 'connecting') return 'Connecting to the network';
  if (info.height != null && Number.isFinite(info.height)) return `Block ${heightFormat.format(info.height)}`;
  return info.state === 'syncing' ? 'The network is catching up' : networkName(info.network);
}

export interface NetworkStatusProps extends NetworkInfo {
  /**
   * full: dot and label (default); auto: dot only while online, label otherwise;
   * dot: dot only (the state is in the tooltip and the accessible name).
   */
  display?: 'full' | 'auto' | 'dot';
  class?: string;
}

export function NetworkStatus({ state, height, network, display = 'full', class: className }: NetworkStatusProps) {
  const info = { state, height, network };
  const label = networkLabel(info);
  const hideLabel = display === 'dot' || (display === 'auto' && state === 'online');
  const more = detail(info);
  const tip = !hideLabel ? more : state === 'connecting' ? label : `${label} · ${more}`;
  return (
    <Tooltip content={tip} placement="top">
      <span
        class={cx('net-status', `net-status--${state}`, hideLabel && 'net-status--dot-only', className)}
        tabIndex={0}
        role="status"
        aria-label={hideLabel ? label : undefined}
      >
        <span class="net-status__dot" aria-hidden="true" />
        <span class={cx('net-status__label', hideLabel && 'visually-hidden')}>{label}</span>
      </span>
    </Tooltip>
  );
}

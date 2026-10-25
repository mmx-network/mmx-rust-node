import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';
import { Banner } from '@wallet/ui/components/Banner';
import { Button } from '@wallet/ui/components/Button';
import { Card } from '@wallet/ui/components/Card';
import { CopyButton } from '@wallet/ui/components/CopyButton';
import { EmptyState } from '@wallet/ui/components/EmptyState';
import { Icon, type IconName } from '@wallet/ui/components/Icon';
import { Skeleton } from '@wallet/ui/components/Skeleton';
import { normalizeAddress } from '@wallet/lib/address';
import type { ResourceState } from '../lib/poll';
import { href, type Route } from '../lib/router';
import { number, short } from '../lib/format';
import type { Token } from '../lib/types';

export { Banner, Button, Card, CopyButton, EmptyState, Icon };

export function ChainLink({
  kind,
  id,
  full = false,
}: {
  kind: 'address' | 'farmer' | 'transaction' | 'block';
  id?: string | null;
  full?: boolean;
}) {
  if (!id) return <>—</>;
  const route: Route = kind === 'block' ? { kind, hash: id } : { kind, id };
  return (
    <a class={`chain-link mono${full ? ' chain-link--full' : ''}`} href={href(route)} title={id}>
      {full ? id : short(id)}
    </a>
  );
}

export function BlockLink({ height }: { height?: number | null }) {
  return height == null ? (
    <span class="muted">Pending</span>
  ) : (
    <a class="numeric block-link" href={href({ kind: 'block', height })}>
      #{number(height)}
    </a>
  );
}

export function TokenName({ token }: { token: Token }) {
  const name = token.is_nft ? 'NFT' : (token.symbol ?? 'Unknown token');
  return token.is_native || !token.contract ? (
    <>{name}</>
  ) : (
    <a href={href({ kind: 'address', id: token.contract })}>{name}</a>
  );
}

export function Identifier({ value, label = 'identifier' }: { value: string; label?: string }) {
  return (
    <div class="identifier">
      <code tabIndex={0} aria-label={label}>
        {value}
      </code>
      <CopyButton size="sm" label={`Copy ${label}`} onCopy={() => navigator.clipboard.writeText(value)} />
    </div>
  );
}

export function Feedback<T>({
  resource,
  noun = 'data',
  children,
}: {
  resource: ResourceState<T> & { refresh(): void };
  noun?: string;
  children?: ComponentChildren;
}) {
  if (!resource.loaded)
    return (
      <Card>
        <div class="loading-rows" role="status" aria-label={`Loading ${noun}`} aria-busy="true">
          <Skeleton lines={5} />
        </div>
      </Card>
    );
  return (
    <>
      {resource.error ? (
        <Banner
          tone="danger"
          title={`Could not refresh ${noun}`}
          action={
            <Button size="sm" onClick={resource.refresh} loading={resource.loading}>
              Try again
            </Button>
          }
        >
          {resource.error.message}
          {resource.updated ? ' Showing the last successful response.' : ''}
        </Banner>
      ) : null}
      {resource.data !== null ? (
        children
      ) : !resource.error ? (
        <Card>
          <EmptyState
            icon="search"
            title={`No ${noun} found`}
            message="Check the identifier or try again after the next block."
            action={<Button onClick={resource.refresh}>Refresh</Button>}
          />
        </Card>
      ) : null}
    </>
  );
}

export function Panel({
  title,
  meta,
  actions,
  children,
}: {
  title: string;
  meta?: ComponentChildren;
  actions?: ComponentChildren;
  children: ComponentChildren;
}) {
  return (
    <Card padding="none" as="section" class="data-panel" aria-label={title}>
      <div class="panel-header">
        <div>
          <h2>{title}</h2>
          {meta ? <p>{meta}</p> : null}
        </div>
        {actions}
      </div>
      {children}
    </Card>
  );
}

export function Stat({
  label,
  value,
  icon,
  foot,
}: {
  label: string;
  value: ComponentChildren;
  icon?: IconName;
  foot?: ComponentChildren;
}) {
  return (
    <Card class="stat">
      <div class="stat__label">
        {label}
        {icon ? <Icon name={icon} size="sm" /> : null}
      </div>
      <div class="stat__value numeric">{value}</div>
      {foot ? <div class="stat__foot">{foot}</div> : null}
    </Card>
  );
}

export function Fields({ rows }: { rows: [string, ComponentChildren][] }) {
  return (
    <dl class="fields">
      {rows.map(([label, value]) => (
        <div class="fields__row" key={label}>
          <dt>{label}</dt>
          <dd>{value ?? '—'}</dd>
        </div>
      ))}
    </dl>
  );
}

export function ObjectDetails({ value, label = 'Data' }: { value: Record<string, unknown>; label?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <details class="object-details" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        {label}
        <Icon name="chevron-down" size="sm" />
      </summary>
      {open ? (
        <Fields
          rows={Object.entries(value)
            .filter(([key]) => key !== 'solution')
            .map(([key, item]) => [
              key,
              typeof item === 'string' && normalizeAddress(item) ? (
                <ChainLink kind="address" id={item} full />
              ) : item !== null && typeof item === 'object' ? (
                <pre>{JSON.stringify(item, (name, data: unknown) => (name === 'solution' ? undefined : data), 2)}</pre>
              ) : (
                <span class="mono">{String(item ?? '—')}</span>
              ),
            ])}
        />
      ) : null}
    </details>
  );
}

export { PAGE_SIZE, pageSlice } from '../lib/paging';

export function Pager({
  page,
  pages,
  total,
  previous,
  next,
  busy = false,
  label = 'results',
}: {
  page: number;
  pages?: number;
  total?: number;
  previous?: string;
  next?: string;
  busy?: boolean;
  label?: string;
}) {
  return (
    <nav class="pager" aria-label={`${label} pagination`}>
      <span class="pager__summary">
        {total !== undefined ? `${number(total)} ${label} · ` : ''}Page {number(page)}
        {pages !== undefined ? ` of ${number(pages)}` : ''}
      </span>
      <div class="pager__controls">
        {previous && !busy ? (
          <a class="btn btn--secondary btn--sm" href={previous} rel="prev">
            <Icon name="chevron-left" size="sm" />
            <span>Previous</span>
          </a>
        ) : (
          <Button size="sm" icon="chevron-left" disabled>
            Previous
          </Button>
        )}
        {next && !busy ? (
          <a class="btn btn--secondary btn--sm" href={next} rel="next">
            <span>Next</span>
            <Icon name="chevron-right" size="sm" />
          </a>
        ) : (
          <Button size="sm" iconEnd="chevron-right" disabled>
            Next
          </Button>
        )}
      </div>
    </nav>
  );
}

import { PageHeader } from '@wallet/ui/components/Layout';
import { Badge } from '@wallet/ui/components/Badge';
import { api, endpoint } from '../lib/api';
import { blockPage, transactionsPerSecond } from '../lib/blocks';
import { amount, networkSpace, number } from '../lib/format';
import { href, type Paging, type Section } from '../lib/router';
import { useResource } from '../lib/resource';
import type { Block, Farmer, NodeInfo, Transaction } from '../lib/types';
import { Card, Feedback, Icon, PAGE_SIZE, Pager, Panel, Stat, pageSlice } from './common';
import { BlocksTable, FarmersTable, TransactionsTable } from './tables';

export const rowsOf = <T,>(value: T[] | null): T[] => (Array.isArray(value) ? value.filter((row) => row !== null) : []);
const tpsFormat = new Intl.NumberFormat('en-US', { maximumFractionDigits: 3 });

export function Blocks({ page = 1, at, network }: Paging & { network: NodeInfo | null }) {
  const offset = page > 1 && at !== undefined ? Math.max(0, at - (page - 1) * PAGE_SIZE) : undefined;
  const genesis = offset === 0;
  const path = genesis
    ? endpoint('/header', { height: 0 })
    : endpoint('/headers', { limit: PAGE_SIZE, ...(offset === undefined ? {} : { offset }) });
  const resource = useResource<Block[]>(path, page === 1 ? 10_000 : 0, (signal) => blockPage(api, offset, signal));
  const rows = rowsOf(resource.data);
  const tps = transactionsPerSecond(rows);
  const first = rows[0];
  const last = rows.at(-1);
  const anchor = at ?? first?.height;
  const older =
    last && last.height > 0 && anchor !== undefined ? href({ kind: 'blocks', page: page + 1, at: anchor }) : undefined;
  return (
    <div class="explorer-page">
      <PageHeader
        title="Blocks"
        subtitle="Every block. An open record of the MMX network."
        actions={
          <Badge tone={page === 1 ? 'positive' : 'neutral'} dot>
            {page === 1 ? 'Live updates' : 'Historical blocks'}
          </Badge>
        }
      />
      {page === 1 ? (
        <div class="network-overview">
          <Card variant="hero" class="height-hero">
            <div class="eyebrow">
              <Icon name="globe" size="sm" />
              The MMX blockchain
            </div>
            <div class="height-hero__label">Latest block</div>
            <a
              class="height-hero__height"
              href={first ? href({ kind: 'block', height: first.height }) : '#/explore/blocks'}
            >
              {first ? number(first.height) : '—'}
              <Icon name="arrow-up-right" size="lg" />
            </a>
            <p>Secured by proof of space and time.</p>
          </Card>
          <div class="overview-stats">
            <Stat
              label="Network space"
              icon="server"
              value={networkSpace(network?.total_space)}
              foot="Estimated effective space"
            />
            <Stat
              label="Block reward"
              icon="reward"
              value={
                first?.reward_addr ? (
                  <>
                    {amount(first.reward_amount)} <small>MMX</small>
                  </>
                ) : (
                  '—'
                )
              }
              foot="Latest block"
            />
            <Stat
              label="TPS"
              icon="activity"
              value={tps === null ? '—' : tpsFormat.format(tps)}
              foot={`Average over the last ${number(rows.length)} blocks`}
            />
          </div>
        </div>
      ) : null}
      <Feedback resource={resource} noun="blocks">
        <Panel
          title={page === 1 ? 'Latest blocks' : 'Block history'}
          meta={
            page === 1
              ? 'A new chapter, one block at a time.'
              : first && last
                ? `Blocks ${number(first.height)}–${number(last.height)}`
                : 'Earlier blocks'
          }
        >
          <BlocksTable rows={rows} />
          <Pager
            page={page}
            previous={page > 1 ? href({ kind: 'blocks', page: page - 1, at: anchor }) : undefined}
            next={older}
            label="Blocks"
            busy={resource.loading && !resource.loaded}
          />
        </Panel>
      </Feedback>
    </div>
  );
}

/** Preserve native request bounds; page through the response without refetching. */
export function RecentList({ kind, page = 1 }: { kind: Exclude<Section, 'blocks'>; page?: number }) {
  const farmers = kind === 'farmers';
  const resource = useResource<Transaction[] | Farmer[]>(
    endpoint(farmers ? '/farmers' : '/transactions', { limit: farmers ? 200 : 100 }),
    page === 1 ? (farmers ? 30_000 : 10_000) : 0,
  );
  const all = rowsOf<Transaction | Farmer>(resource.data);
  const slice = pageSlice(all, page);
  return (
    <div class="explorer-page">
      <PageHeader
        title={farmers ? 'Farmers' : 'Transactions'}
        subtitle={
          farmers
            ? 'The farmers securing the network, ranked by blocks farmed.'
            : 'Follow the activity moving through the MMX network.'
        }
        actions={
          <Badge tone={page === 1 ? 'positive' : 'neutral'} dot>
            {page === 1 ? 'Live updates' : 'Updates paused'}
          </Badge>
        }
      />
      <Feedback resource={resource} noun={kind}>
        <Panel
          title={farmers ? 'Farmer ranking' : 'Latest transactions'}
          meta={`${farmers ? 'Top 200 farmers' : 'Latest 100 transactions'} · ${PAGE_SIZE} per page`}
        >
          {farmers ? (
            <FarmersTable rows={slice.rows as Farmer[]} offset={slice.start ? slice.start - 1 : 0} />
          ) : (
            <TransactionsTable rows={slice.rows as Transaction[]} />
          )}
          <Pager
            page={slice.page}
            pages={slice.pages}
            total={all.length}
            previous={slice.page > 1 ? href({ kind, page: slice.page - 1 }) : undefined}
            next={slice.page < slice.pages ? href({ kind, page: slice.page + 1 }) : undefined}
            label={kind}
          />
        </Panel>
      </Feedback>
    </div>
  );
}

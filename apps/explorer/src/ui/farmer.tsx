import { PageHeader } from '@wallet/ui/components/Layout';
import { endpoint } from '../lib/api';
import { atoms, number } from '../lib/format';
import { href, type Paging } from '../lib/router';
import { useResource } from '../lib/resource';
import type { Block, FarmerInfo } from '../lib/types';
import { Card, ChainLink, Feedback, Identifier, PAGE_SIZE, Pager, Panel, Stat, pageSlice } from './common';
import { BlocksTable, Table } from './tables';
import { rowsOf } from './lists';
import { LocalPager } from './block';
import { useState } from 'preact/hooks';

export function FarmerPage({ id, page = 1 }: { id: string } & Paging) {
  const [rewardPage, setRewardPage] = useState(1);
  const farmer = useResource<FarmerInfo>(endpoint('/farmer', { id }), rewardPage === 1 ? 30_000 : 0);
  const blocks = useResource<Block[]>(endpoint('/farmer/blocks', { id, limit: 100 }), page === 1 ? 30_000 : 0);
  const all = rowsOf(blocks.data);
  const slice = pageSlice(all, page);
  const rewards = pageSlice(farmer.data?.rewards ?? [], rewardPage);
  return (
    <div class="explorer-page">
      <PageHeader title="Farmer" subtitle="A closer look at a contributor to the MMX network." />
      <Card>
        <div class="eyebrow">Farmer public key</div>
        <Identifier value={id} label="farmer public key" />
      </Card>
      <Feedback resource={farmer} noun="farmer">
        {farmer.data ? (
          <>
            <div class="stats-grid">
              <Stat label="Blocks farmed" icon="overview" value={number(farmer.data.block_count)} />
              <Stat
                label="Total rewards"
                icon="reward"
                value={
                  <>
                    {atoms(farmer.data.total_reward) ?? number(farmer.data.total_reward_value)} <small>MMX</small>
                  </>
                }
              />
            </div>
            <Panel title="Reward addresses">
              <Table columns={['Address', 'Total rewards']} empty={!rewards.rows.length} emptyLabel="No rewards yet">
                {rewards.rows.map((reward) => (
                  <tr key={reward.address}>
                    <td>
                      <ChainLink kind="address" id={reward.address} />
                    </td>
                    <td class="numeric">
                      {atoms(reward.amount) ?? number(reward.value)} <span class="muted">MMX</span>
                    </td>
                  </tr>
                ))}
              </Table>
              <LocalPager
                {...rewards}
                total={farmer.data.rewards?.length ?? 0}
                onPage={setRewardPage}
                label="addresses"
              />
            </Panel>
          </>
        ) : null}
      </Feedback>
      <Feedback resource={blocks} noun="blocks">
        <Panel title="Farmed blocks" meta={`Latest 100 blocks · ${PAGE_SIZE} per page`}>
          <BlocksTable rows={slice.rows} />
          <Pager
            page={slice.page}
            pages={slice.pages}
            total={all.length}
            previous={slice.page > 1 ? href({ kind: 'farmer', id, page: slice.page - 1 }) : undefined}
            next={slice.page < slice.pages ? href({ kind: 'farmer', id, page: slice.page + 1 }) : undefined}
            label="blocks"
          />
        </Panel>
      </Feedback>
    </div>
  );
}

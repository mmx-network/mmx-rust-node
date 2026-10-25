import { PageHeader } from '@wallet/ui/components/Layout';
import { endpoint } from '../lib/api';
import { number } from '../lib/format';
import { href, type Paging } from '../lib/router';
import { useResource } from '../lib/resource';
import type { BalanceResponse, HistoryEntry } from '../lib/types';
import { Card, ChainLink, Feedback, Identifier, ObjectDetails, PAGE_SIZE, Pager, Panel, pageSlice } from './common';
import { BalancesTable, HistoryTable, Table } from './tables';
import { rowsOf } from './lists';
import { LocalPager } from './block';
import { useState } from 'preact/hooks';

export function AddressPage({ id, page = 1 }: { id: string } & Paging) {
  const [balancePage, setBalancePage] = useState(1);
  const [nftPage, setNftPage] = useState(1);
  const balance = useResource<BalanceResponse & { nfts?: string[] }>(
    endpoint('/balance', { id }),
    balancePage === 1 && nftPage === 1 ? 10_000 : 0,
  );
  const contract = useResource<Record<string, unknown>>(endpoint('/contract', { id }));
  const history = useResource<HistoryEntry[]>(
    endpoint('/address/history', { id, limit: 200 }),
    page === 1 ? 60_000 : 0,
  );
  const rows = rowsOf(history.data);
  const slice = pageSlice(rows, page);
  const balances = pageSlice(balance.data?.balances ?? [], balancePage);
  const nfts = pageSlice(balance.data?.nfts ?? [], nftPage);
  return (
    <div class="explorer-page">
      <PageHeader title="Address" subtitle="Balances, contract details and activity, all in one place." />
      <Card>
        <div class="eyebrow">MMX address</div>
        <Identifier value={id} label="address" />
      </Card>
      <Feedback resource={balance} noun="balances">
        <Panel title="Token balances">
          <BalancesTable rows={balances.rows} />
          <LocalPager
            {...balances}
            total={balance.data?.balances?.length ?? 0}
            onPage={setBalancePage}
            label="tokens"
          />
        </Panel>
        {balance.data?.nfts?.length ? (
          <Panel title="NFTs">
            <Table columns={['Contract']}>
              {nfts.rows.map((nft) => (
                <tr key={nft}>
                  <td>
                    <ChainLink kind="address" id={nft} />
                  </td>
                </tr>
              ))}
            </Table>
            <LocalPager {...nfts} total={balance.data.nfts.length} onPage={setNftPage} label="NFTs" />
          </Panel>
        ) : null}
      </Feedback>
      {contract.data || contract.error ? (
        <Feedback resource={contract} noun="contract">
          <Panel title="Contract">
            <ObjectDetails value={contract.data ?? {}} label={String(contract.data?.__type ?? 'Contract data')} />
          </Panel>
        </Feedback>
      ) : null}
      <Feedback resource={history} noun="activity">
        <Panel
          title="Activity"
          meta={`Latest ${number(rows.length)} entries · ${PAGE_SIZE} per page${page > 1 ? ' · Updates paused' : ''}`}
        >
          <HistoryTable rows={slice.rows} />
          <Pager
            page={slice.page}
            pages={slice.pages}
            total={rows.length}
            previous={slice.page > 1 ? href({ kind: 'address', id, page: slice.page - 1 }) : undefined}
            next={slice.page < slice.pages ? href({ kind: 'address', id, page: slice.page + 1 }) : undefined}
            label="entries"
          />
        </Panel>
      </Feedback>
    </div>
  );
}

import type { ComponentChildren } from 'preact';
import { Badge, type Tone } from '@wallet/ui/components/Badge';
import { amount, date, number, percent, transferAmount } from '../lib/format';
import type { Balance, Block, BlockTransaction, Farmer, HistoryEntry, Transaction, Transfer } from '../lib/types';
import { BlockLink, ChainLink, EmptyState, TokenName } from './common';

export function Table({
  columns,
  children,
  empty = false,
  emptyLabel = 'No results yet',
}: {
  columns: string[];
  children: ComponentChildren;
  empty?: boolean;
  emptyLabel?: string;
}) {
  return empty ? (
    <EmptyState icon="search" size="sm" title={emptyLabel} message="New activity will appear here." />
  ) : (
    <div class="table-scroll" tabIndex={0} role="region" aria-label="Scrollable results">
      <table class="data-table">
        <thead>
          <tr>
            {columns.map((column) => (
              <th scope="col" key={column}>
                {column}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

export function BlocksTable({ rows }: { rows: Block[] }) {
  return (
    <Table
      columns={['Block', 'Hash', 'Transactions', 'Proof', 'Reward', 'TX fees', 'Usage']}
      empty={!rows.length}
      emptyLabel="No blocks found"
    >
      {rows.map((block) => (
        <tr key={block.hash}>
          <td>
            <BlockLink height={block.height} />
            <span class="cell-sub">VDF +{number(block.vdf_count)}</span>
          </td>
          <td>
            <ChainLink kind="block" id={block.hash} />
            <span class="cell-sub">{date(block)}</span>
          </td>
          <td class="numeric">{number(block.tx_count ?? 0)}</td>
          <td>
            <span class="numeric">{block.ksize ? `k${block.ksize}` : '—'}</span>
            <span class="cell-sub numeric">Score {number(block.score)}</span>
          </td>
          <td class="numeric">
            {block.reward_addr ? (
              <>
                {amount(block.reward_amount)} <span class="muted">MMX</span>
              </>
            ) : (
              '—'
            )}
          </td>
          <td class="numeric">
            {amount(block.tx_fees)} <span class="muted">MMX</span>
          </td>
          <td>
            <span class="numeric">{percent(block.static_cost_ratio)} size</span>
            <span class="cell-sub numeric">{percent(block.total_cost_ratio)} cost</span>
          </td>
        </tr>
      ))}
    </Table>
  );
}

export function TransactionStatus({ tx }: { tx: Transaction }) {
  return (
    <Badge tone={tx.did_fail ? 'negative' : tx.height == null ? 'warning' : 'positive'} dot>
      {tx.did_fail ? 'Failed' : tx.height == null ? 'Pending' : 'Confirmed'}
    </Badge>
  );
}

export function TransactionsTable({ rows }: { rows: Transaction[] }) {
  return (
    <Table
      columns={['Transaction', 'Block', 'Status', 'Fee', 'Inputs / Outputs', 'Operations']}
      empty={!rows.length}
      emptyLabel="No transactions found"
    >
      {rows.map((tx) => (
        <tr key={tx.id}>
          <td>
            <ChainLink kind="transaction" id={tx.id} />
            <span class="cell-sub">
              {date(tx)}
              {tx.note ? ` · ${tx.note}` : ''}
            </span>
          </td>
          <td>
            <BlockLink height={tx.height} />
          </td>
          <td>
            <TransactionStatus tx={tx} />
          </td>
          <td class={`numeric${tx.did_fail ? ' negative' : ''}`}>
            {amount(tx.fee)} <span class="muted">MMX</span>
          </td>
          <td class="numeric">
            {number(tx.inputs?.length ?? 0)} / {number(tx.outputs?.length ?? 0)}
          </td>
          <td class="numeric">{number(tx.operations?.length ?? 0)}</td>
        </tr>
      ))}
    </Table>
  );
}

export function BlockTransactionsTable({ rows }: { rows: BlockTransaction[] }) {
  return (
    <Table
      columns={['Transaction', 'Inputs', 'Outputs', 'Operations']}
      empty={!rows.length}
      emptyLabel="No transactions in this block"
    >
      {rows.map((tx) => (
        <tr key={tx.id}>
          <td>
            <ChainLink kind="transaction" id={tx.id} />
          </td>
          <td class="numeric">{number((tx.inputs?.length ?? 0) + (tx.exec_result?.inputs?.length ?? 0))}</td>
          <td class="numeric">{number((tx.outputs?.length ?? 0) + (tx.exec_result?.outputs?.length ?? 0))}</td>
          <td class="numeric">{number(tx.execute?.length ?? 0)}</td>
        </tr>
      ))}
    </Table>
  );
}

export function FarmersTable({ rows, offset }: { rows: Farmer[]; offset: number }) {
  return (
    <Table columns={['Rank', 'Farmer public key', 'Blocks farmed']} empty={!rows.length} emptyLabel="No farmers found">
      {rows.map((farmer, index) => (
        <tr key={farmer.farmer_key}>
          <td class="numeric muted">{number(index + offset + 1)}</td>
          <td>
            <ChainLink kind="farmer" id={farmer.farmer_key} />
          </td>
          <td class="numeric">{number(farmer.block_count)}</td>
        </tr>
      ))}
    </Table>
  );
}

export function BalancesTable({ rows }: { rows: Balance[] }) {
  return (
    <Table
      columns={['Token', 'Balance', 'Locked', 'Spendable', 'Contract']}
      empty={!rows.length}
      emptyLabel="No token balances"
    >
      {rows.map((token) => (
        <tr key={token.contract}>
          <td>
            <TokenName token={token} />
          </td>
          <td class="numeric">{number(token.total)}</td>
          <td class="numeric">{number(token.locked)}</td>
          <td class="numeric">{number(token.spendable)}</td>
          <td>
            {token.is_native ? (
              <span class="muted">Native currency</span>
            ) : (
              <ChainLink kind="address" id={token.contract} />
            )}
          </td>
        </tr>
      ))}
    </Table>
  );
}

export function TransfersTable({ rows }: { rows: Transfer[] }) {
  return (
    <Table columns={['Address', 'Amount', 'Token', 'Memo']} empty={!rows.length} emptyLabel="No transfers">
      {rows.map((entry, index) => (
        <tr key={index}>
          <td>
            <ChainLink kind="address" id={entry.address} />
          </td>
          <td class="numeric">{transferAmount(entry)}</td>
          <td>
            <TokenName token={entry} />
          </td>
          <td class="memo">{entry.memo || '—'}</td>
        </tr>
      ))}
    </Table>
  );
}

const HISTORY_TONE: Record<string, Tone> = {
  REWARD: 'positive',
  RECEIVE: 'positive',
  VDF_REWARD: 'positive',
  SPEND: 'negative',
};
export function HistoryTable({ rows }: { rows: HistoryEntry[] }) {
  return (
    <Table
      columns={['Type', 'Amount', 'Token', 'Block', 'Transaction', 'Memo', 'Time']}
      empty={!rows.length}
      emptyLabel="No address activity"
    >
      {rows.map((entry, index) => (
        <tr key={index}>
          <td>
            <Badge tone={HISTORY_TONE[entry.type] ?? 'neutral'}>{entry.type.replace(/_/g, ' ')}</Badge>
          </td>
          <td class="numeric">{transferAmount(entry)}</td>
          <td>
            <TokenName token={entry} />
          </td>
          <td>
            <BlockLink height={entry.height} />
          </td>
          <td>
            <ChainLink kind="transaction" id={entry.txid} />
          </td>
          <td class="memo">{entry.memo || '—'}</td>
          <td class="nowrap muted">{date(entry)}</td>
        </tr>
      ))}
    </Table>
  );
}

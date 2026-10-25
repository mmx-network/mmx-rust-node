import { PageHeader } from '@wallet/ui/components/Layout';
import { endpoint } from '../lib/api';
import { amount, date, number } from '../lib/format';
import { href } from '../lib/router';
import { useResource } from '../lib/resource';
import type { Transaction } from '../lib/types';
import {
  Banner,
  BlockLink,
  Card,
  ChainLink,
  Feedback,
  Fields,
  Icon,
  Identifier,
  ObjectDetails,
  Panel,
  pageSlice,
} from './common';
import { TransactionStatus, TransfersTable } from './tables';
import { LocalPager } from './block';
import { useState } from 'preact/hooks';

export function TransactionPage({ id }: { id: string }) {
  const resource = useResource<Transaction>(endpoint('/transaction', { id }), 10_000);
  const tx = resource.data;
  const [inputPage, setInputPage] = useState(1);
  const [outputPage, setOutputPage] = useState(1);
  const inputs = pageSlice(tx?.inputs ?? [], inputPage);
  const outputs = pageSlice(tx?.outputs ?? [], outputPage);
  return (
    <div class="explorer-page">
      <a class="back-link" href={href({ kind: 'transactions' })}>
        <Icon name="arrow-left" size="sm" />
        All transactions
      </a>
      <PageHeader
        title="Transaction"
        subtitle={tx ? date(tx) : 'Trace a transaction through the network.'}
        actions={tx ? <TransactionStatus tx={tx} /> : undefined}
      />
      <Card>
        <div class="eyebrow">Transaction ID</div>
        <Identifier value={id} label="transaction ID" />
      </Card>
      <Feedback resource={resource} noun="transaction">
        {tx ? (
          <>
            {tx.did_fail ? (
              <Banner tone="danger" title="Transaction failed">
                {tx.message || 'This transaction did not execute successfully.'}
              </Banner>
            ) : null}
            <Panel title="Transaction details">
              <Fields
                rows={[
                  ['Status', <TransactionStatus tx={tx} />],
                  ['Block', <BlockLink height={tx.height} />],
                  ['Confirmations', tx.height == null ? 'Pending' : number(tx.confirm)],
                  ['Timestamp', date(tx)],
                  ['Sender', <ChainLink kind="address" id={tx.sender} full />],
                  ['Fee', `${amount(tx.fee)} MMX`],
                  ['Cost', `${amount(tx.cost)} MMX`],
                  [
                    'Expires at block',
                    tx.expires === undefined || tx.expires === 0xffffffff ? (
                      'No expiry'
                    ) : (
                      <BlockLink height={tx.expires} />
                    ),
                  ],
                  ['Note', tx.note || '—'],
                  ...(tx.deployed
                    ? [
                        ['Deployed address', <ChainLink kind="address" id={tx.address} full />] as [
                          string,
                          preact.ComponentChildren,
                        ],
                      ]
                    : []),
                ]}
              />
            </Panel>
            <Panel title="Inputs" meta="Tokens spent by this transaction">
              <TransfersTable rows={inputs.rows} />
              <LocalPager {...inputs} total={tx.inputs?.length ?? 0} onPage={setInputPage} label="inputs" />
            </Panel>
            <Panel title="Outputs" meta="Tokens received from this transaction">
              <TransfersTable rows={outputs.rows} />
              <LocalPager {...outputs} total={tx.outputs?.length ?? 0} onPage={setOutputPage} label="outputs" />
            </Panel>
            {tx.operations?.length ? (
              <Panel title="Operations">
                {tx.operations.map((operation, index) => (
                  <ObjectDetails
                    key={index}
                    value={operation}
                    label={`Operation ${index + 1} · ${String(operation.__type ?? 'Contract call')}`}
                  />
                ))}
              </Panel>
            ) : null}
            {tx.deployed ? (
              <Panel title="Deployed contract">
                <ObjectDetails value={tx.deployed} label={String(tx.deployed.__type ?? 'Contract data')} />
              </Panel>
            ) : null}
          </>
        ) : null}
      </Feedback>
    </div>
  );
}

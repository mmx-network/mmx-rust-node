import { PageHeader } from '@wallet/ui/components/Layout';
import { endpoint } from '../lib/api';
import { amount, date, number, percent } from '../lib/format';
import { useResource } from '../lib/resource';
import { href, type Route } from '../lib/router';
import { BlockTransactionsTable } from './tables';
import type { Block } from '../lib/types';
import {
  Button,
  Card,
  ChainLink,
  Feedback,
  Fields,
  Icon,
  Identifier,
  ObjectDetails,
  PAGE_SIZE,
  Panel,
  pageSlice,
} from './common';
import { useState } from 'preact/hooks';

export function BlockPage({ route }: { route: Extract<Route, { kind: 'block' }> }) {
  const resource = useResource<Block>(
    endpoint('/block', 'height' in route ? { height: route.height } : { hash: route.hash }),
  );
  const block = resource.data;
  const [page, setPage] = useState(1);
  const txs = pageSlice(block?.tx_list ?? [], page);
  return (
    <div class="explorer-page">
      <a class="back-link" href={href({ kind: 'blocks' })}>
        <Icon name="arrow-left" size="sm" />
        All blocks
      </a>
      <PageHeader
        title={`Block ${block ? `#${number(block.height)}` : 'height' in route ? `#${number(route.height)}` : 'details'}`}
        subtitle={block ? date(block) : 'An immutable record of network activity.'}
        actions={
          block ? (
            <div class="block-neighbors">
              {block.height > 0 ? (
                <a
                  class="btn btn--secondary btn--sm"
                  href={
                    block.prev
                      ? href({ kind: 'block', hash: block.prev })
                      : href({ kind: 'block', height: block.height - 1 })
                  }
                >
                  <Icon name="chevron-left" size="sm" />
                  Previous block
                </a>
              ) : null}
              {block.height < 0xffffffff ? (
                <a class="btn btn--secondary btn--sm" href={href({ kind: 'block', height: block.height + 1 })}>
                  Next block
                  <Icon name="chevron-right" size="sm" />
                </a>
              ) : null}
            </div>
          ) : undefined
        }
      />
      <Feedback resource={resource} noun="block">
        {block ? (
          <>
            <Card>
              <div class="eyebrow">Block hash</div>
              <Identifier value={block.hash} label="block hash" />
            </Card>
            <Panel title="Block details">
              <Fields
                rows={[
                  ['Height', number(block.height)],
                  ['Previous block', <ChainLink kind="block" id={block.prev} full />],
                  ['Timestamp', date(block)],
                  ['Reward address', <ChainLink kind="address" id={block.reward_addr} full />],
                  ...(block.reward_contract
                    ? [
                        ['Reward contract', <ChainLink kind="address" id={block.reward_contract} full />] as [
                          string,
                          preact.ComponentChildren,
                        ],
                      ]
                    : []),
                  ...(block.reward_account
                    ? [
                        ['Farmer address', <ChainLink kind="address" id={block.reward_account} full />] as [
                          string,
                          preact.ComponentChildren,
                        ],
                      ]
                    : []),
                  ['Block reward', `${amount(block.reward_amount)} MMX`],
                  ['Transaction fees', `${amount(block.tx_fees)} MMX`],
                  ['Transactions', number(block.tx_count ?? block.tx_list?.length ?? 0)],
                  ['Block size', percent(block.static_cost_ratio)],
                  ['Block cost', percent(block.total_cost_ratio)],
                  [
                    'Farmer public key',
                    <ChainLink kind="farmer" id={block.farmer_key ?? block.proof?.[0]?.farmer_key} full />,
                  ],
                ]}
              />
            </Panel>
            <Panel
              title="Transactions"
              meta={`${number(block.tx_list?.length ?? 0)} transactions · ${PAGE_SIZE} per page`}
            >
              <BlockTransactionsTable rows={txs.rows} />
              <LocalPager
                page={txs.page}
                pages={txs.pages}
                total={block.tx_list?.length ?? 0}
                onPage={setPage}
                label="transactions"
              />
            </Panel>
            <Panel title="Consensus details">
              <Fields
                rows={[
                  ['Time difficulty', block.time_diff],
                  ['Space difficulty', block.space_diff],
                  ['Reward vote', number(block.reward_vote)],
                  ['Reward vote sum', `${number(block.reward_vote_sum)} / ${number(block.reward_vote_count)}`],
                  ['Base reward', `${amount(block.base_reward)} MMX`],
                  ['Average transaction fee', `${amount(block.average_txfee)} MMX`],
                  ['VDF height', number(block.vdf_height)],
                  ['VDF count', number(block.vdf_count)],
                  ['VDF iterations', block.vdf_iters],
                  ...(block.vdf_reward_addr ?? []).map(
                    (id, index) =>
                      [`Timelord ${index + 1}`, <ChainLink kind="address" id={id} full />] as [
                        string,
                        preact.ComponentChildren,
                      ],
                  ),
                  ...(block.vdf_reward_payout
                    ? [
                        ['Timelord reward', <ChainLink kind="address" id={block.vdf_reward_payout} full />] as [
                          string,
                          preact.ComponentChildren,
                        ],
                      ]
                    : []),
                  ['Plot size', block.ksize ? `k${block.ksize}` : '—'],
                  ['Proof score', number(block.score)],
                  ['Proofs', number(block.proof?.length ?? 0)],
                  ['Space fork', `${number(block.space_fork_proofs)} / ${number(block.space_fork_len)}`],
                ]}
              />
              {(block.proof ?? []).map((proof, index) => (
                <ObjectDetails key={index} value={proof} label={`Proof ${index + 1}`} />
              ))}
            </Panel>
          </>
        ) : null}
      </Feedback>
    </div>
  );
}

/** Detail payloads already contain their rows; do not request them a second time. */
export function LocalPager({
  page,
  pages,
  total,
  onPage,
  label,
}: {
  page: number;
  pages: number;
  total: number;
  onPage: (page: number) => void;
  label: string;
}) {
  return (
    <nav class="pager" aria-label={`${label} pagination`}>
      <span class="pager__summary">
        {number(total)} {label} · Page {number(page)} of {number(pages)}
      </span>
      <div class="pager__controls">
        <Button size="sm" icon="chevron-left" disabled={page <= 1} onClick={() => onPage(page - 1)}>
          Previous
        </Button>
        <Button size="sm" iconEnd="chevron-right" disabled={page >= pages} onClick={() => onPage(page + 1)}>
          Next
        </Button>
      </div>
    </nav>
  );
}

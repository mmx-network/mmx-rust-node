import { describe, expect, it } from 'vitest';
import {
  compareItems,
  confirmations,
  counterpartyAddress,
  filterItems,
  groupByDay,
  groupKey,
  groupRows,
  isActivityKey,
  memoFromTx,
  mergeActivity,
  pendingItem,
  repeatRecipient,
  type ActivityItem,
} from '../../src/chain/activity';
import type { PendingTx } from '../../src/chain/pending';
import type { HistoryRow, TxInfo, TxType } from '../../src/chain/types';
import { NATIVE_CONTRACT } from '../../src/lib/address';
import { testAddress, testTxid } from './helpers/fake-node';

const OWN1 = testAddress(1);
const OWN2 = testAddress(2);
const FOREIGN = testAddress(50);
const FOREIGN2 = testAddress(51);
const TOKEN = testAddress(90);
const FAKE = testAddress(91);
const OWN = new Set([OWN1, OWN2]);

let seq = 0;
function r(type: TxType, amount: bigint, opts: Partial<HistoryRow> = {}): HistoryRow {
  const contract = opts.contract ?? NATIVE_CONTRACT;
  return {
    txid: testTxid('tx1'),
    height: 100,
    timeMs: 1_000_000 + seq++,
    type,
    address: OWN1,
    contract,
    amount,
    symbol: contract === NATIVE_CONTRACT ? 'MMX' : contract === TOKEN ? 'USDM' : 'MMX',
    decimals: 6,
    native: contract === NATIVE_CONTRACT,
    ...opts,
  };
}

/** TOKEN stands for a token on the verified list; FAKE is not on it. */
const verifiedToken = (c: string) => c === TOKEN;

const one = (rows: HistoryRow[]): ActivityItem => {
  const items = groupRows(rows, verifiedToken);
  expect(items).toHaveLength(1);
  return items[0]!;
};

describe('grouping', () => {
  it('received with memo', () => {
    const item = one([r('RECEIVE', 5_000_000n, { memo: 'Thanks' })]);
    expect(item).toMatchObject({ kind: 'received', fee: 0n, memo: 'Thanks', status: 'confirmed', height: 100, key: testTxid('tx1') });
    expect(item.primary).toMatchObject({ amount: 5_000_000n, native: true, verified: true });
  });

  it('sent: spends from several addresses plus fee', () => {
    const item = one([
      r('SPEND', 3_000_000n),
      r('SPEND', 2_000_000n, { address: OWN2 }),
      r('TXFEE', 60_000n, { address: OWN2 }),
    ]);
    expect(item.kind).toBe('sent');
    expect(item.primary!.amount).toBe(-5_000_000n);
    expect(item.fee).toBe(60_000n);
    expect(item.ownAddresses).toEqual([OWN1, OWN2].sort());
  });

  it('moved between own addresses (zero net, fee shown)', () => {
    const item = one([r('SPEND', 7_000_000n), r('RECEIVE', 7_000_000n, { address: OWN2 }), r('TXFEE', 50_000n)]);
    expect(item.kind).toBe('moved');
    expect(item.primary!.amount).toBe(7_000_000n);
    expect(item.fee).toBe(50_000n);
  });

  it('partial self-transfer is a send of the foreign part', () => {
    const item = one([r('SPEND', 10_000_000n), r('RECEIVE', 4_000_000n, { address: OWN2 }), r('TXFEE', 50_000n)]);
    expect(item.kind).toBe('sent');
    expect(item.primary!.amount).toBe(-6_000_000n);
  });

  it('token send: the token is primary and the fee is in MMX', () => {
    const item = one([r('SPEND', 9n, { contract: TOKEN }), r('TXFEE', 50_000n)]);
    expect(item.kind).toBe('sent');
    expect(item.primary).toMatchObject({ contract: TOKEN, amount: -9n, symbol: 'USDM', verified: true, native: false });
    expect(item.amounts).toHaveLength(1);
  });

  it('a fee-only transaction', () => {
    const item = one([r('TXFEE', 50_000n)]);
    expect(item.kind).toBe('fee');
    expect(item.primary).toBeUndefined();
    expect(item.amounts).toEqual([]);
  });

  it('swap-like: native preferred as primary among negatives', () => {
    const item = one([r('SPEND', 1_000_000n), r('RECEIVE', 5n, { contract: TOKEN }), r('TXFEE', 1n)]);
    expect(item.kind).toBe('sent');
    expect(item.primary!.native).toBe(true);
    expect(item.amounts).toHaveLength(2);
  });

  it('rewards of the same block are separate items by type', () => {
    const block = testTxid('block');
    const items = groupRows([
      r('REWARD', 500_000n, { txid: block }),
      r('VDF_REWARD', 200_000n, { txid: block }),
      r('PROJECT_REWARD', 50_000n, { txid: block, address: OWN2 }),
      r('REWARD', 1n, { txid: block, address: OWN2 }),
    ]);
    expect(items).toHaveLength(3);
    const farm = items.find((i) => i.rewardType === 'farming')!;
    expect(farm.key).toBe(`${block}-farm`);
    expect(farm.primary!.amount).toBe(500_001n);
    expect(items.find((i) => i.rewardType === 'timelord')!.key).toBe(`${block}-vdf`);
    expect(items.find((i) => i.rewardType === 'project')!.key).toBe(`${block}-project`);
    for (const i of items) {
      expect(i.kind).toBe('reward');
      expect(isActivityKey(i.key)).toBe(true);
      expect(i.txid).toBe(block);
    }
  });

  it('an unverified token named MMX is never treated as native', () => {
    const item = one([r('RECEIVE', 1_000_000_000n, { contract: FAKE })]);
    expect(item.primary).toMatchObject({ native: false, verified: false, symbol: 'MMX', contract: FAKE });
    const viaLookup = groupRows([r('RECEIVE', 1n, { contract: FAKE })], (c) => c === FAKE)[0]!;
    expect(viaLookup.primary!.verified).toBe(true);
  });

  it('orders newest first with deterministic ties', () => {
    const items = groupRows([
      r('RECEIVE', 1n, { txid: testTxid('a'), height: 5 }),
      r('RECEIVE', 1n, { txid: testTxid('b'), height: 9 }),
      r('RECEIVE', 1n, { txid: testTxid('c'), height: 7 }),
    ]);
    expect(items.map((i) => i.height)).toEqual([9, 7, 5]);
    expect(groupKey(r('TXFEE', 1n))).toBe(testTxid('tx1'));
  });
});

describe('filters', () => {
  const items = groupRows([
    r('RECEIVE', 1n, { txid: testTxid('recv') }),
    r('SPEND', 1n, { txid: testTxid('sent') }),
    r('TXFEE', 1n, { txid: testTxid('fee') }),
    r('REWARD', 1n, { txid: testTxid('blk') }),
    r('RECEIVE', 5n, { txid: testTxid('tok'), contract: TOKEN }),
    r('SPEND', 2n, { txid: testTxid('mv') }),
    r('RECEIVE', 2n, { txid: testTxid('mv'), address: OWN2 }),
  ]);

  it('filters by kind', () => {
    expect(filterItems(items, { kind: 'all', contract: null })).toHaveLength(6);
    expect(filterItems(items, { kind: 'received', contract: null })).toHaveLength(2);
    expect(filterItems(items, { kind: 'sent', contract: null }).map((i) => i.kind).sort()).toEqual(['fee', 'sent']);
    expect(filterItems(items, { kind: 'rewards', contract: null })).toHaveLength(1);
  });

  it('filters by asset (MMX includes fee-only items)', () => {
    expect(filterItems(items, { kind: 'all', contract: TOKEN })).toHaveLength(1);
    expect(filterItems(items, { kind: 'all', contract: NATIVE_CONTRACT })).toHaveLength(5);
    expect(filterItems(items, { kind: 'received', contract: NATIVE_CONTRACT })).toHaveLength(1);
  });
});

describe('pending sends', () => {
  const p: PendingTx = {
    txid: testTxid('pending'),
    walletId: 'w',
    to: FOREIGN,
    contract: NATIVE_CONTRACT,
    symbol: 'MMX',
    decimals: 6,
    amount: 3_000_000n,
    fee: 50_000n,
    memo: 'Rent',
    expires: 500,
    createdAt: 2_000_000,
    status: 'pending',
  };

  it('become local items on top', () => {
    const history = groupRows([r('RECEIVE', 1n)]);
    const merged = mergeActivity(history, [p], OWN);
    expect(merged[0]).toMatchObject({ key: p.txid, kind: 'sent', status: 'pending', local: true, to: FOREIGN, memo: 'Rent', fee: 50_000n });
    expect(merged[0]!.height).toBeUndefined();
    expect(merged[0]!.primary!.amount).toBe(-3_000_000n);
    expect(pendingItem({ ...p, to: OWN2 }, OWN).kind).toBe('moved');
  });

  it('are replaced by history once included', () => {
    const history = groupRows([r('SPEND', 3_000_000n, { txid: p.txid }), r('TXFEE', 50_000n, { txid: p.txid })]);
    const merged = mergeActivity(history, [{ ...p, status: 'confirmed' }], OWN);
    expect(merged).toHaveLength(1);
    expect(merged[0]!.local).toBeUndefined();
    const failed = mergeActivity(history, [{ ...p, status: 'failed' }], OWN);
    expect(failed[0]!.status).toBe('failed');
  });

  it('pending sort above confirmed items', () => {
    const local = pendingItem(p, OWN);
    const confirmed = groupRows([r('RECEIVE', 1n)])[0]!;
    expect(compareItems(local, confirmed)).toBeLessThan(0);
  });

  it('keep an unverified token unverified, whatever its symbol', () => {
    const fake: PendingTx = { ...p, contract: FAKE, symbol: 'MMX' };
    for (const status of ['pending', 'expired', 'failed'] as const) {
      const item = pendingItem({ ...fake, status }, OWN, verifiedToken);
      expect(item.primary).toMatchObject({ native: false, verified: false, contract: FAKE });
    }
    expect(mergeActivity([], [fake], OWN, verifiedToken)[0]!.primary!.verified).toBe(false);
    expect(mergeActivity([], [fake], OWN)[0]!.primary!.verified).toBe(false);
    expect(pendingItem({ ...p, contract: TOKEN }, OWN, verifiedToken).primary!.verified).toBe(true);
    expect(pendingItem(p, OWN).primary).toMatchObject({ native: true, verified: true });
  });
});

describe('counterparty and memo', () => {
  const tx = (over: Partial<TxInfo>): TxInfo => ({
    id: testTxid('tx1'),
    expires: 1,
    didFail: false,
    fee: 1n,
    inputs: [],
    outputs: [],
    contractCall: false,
    ...over,
  });

  it('received: sender, else first foreign input', () => {
    const item = one([r('RECEIVE', 1n)]);
    expect(counterpartyAddress(item, tx({ sender: FOREIGN }), OWN)).toBe(FOREIGN);
    expect(
      counterpartyAddress(
        item,
        tx({ sender: OWN1, inputs: [{ address: OWN2, contract: NATIVE_CONTRACT, amount: 1n }, { address: FOREIGN2, contract: NATIVE_CONTRACT, amount: 1n }] }),
        OWN,
      ),
    ).toBe(FOREIGN2);
    expect(counterpartyAddress(item, tx({}), OWN)).toBeUndefined();
  });

  it('sent: first foreign output of the primary currency', () => {
    const item = one([r('SPEND', 9n, { contract: TOKEN }), r('TXFEE', 1n)]);
    const info = tx({
      outputs: [
        { address: OWN2, contract: TOKEN, amount: 1n },
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 1n },
        { address: FOREIGN2, contract: TOKEN, amount: 8n, memo: 'Invoice 7' },
      ],
    });
    expect(counterpartyAddress(item, info, OWN)).toBe(FOREIGN2);
    expect(memoFromTx(item, info, OWN)).toBe('Invoice 7');
  });

  it('rewards and moves have no counterparty', () => {
    expect(counterpartyAddress(one([r('REWARD', 1n)]), tx({ sender: FOREIGN }), OWN)).toBeUndefined();
    const moved = one([r('SPEND', 2n), r('RECEIVE', 2n, { address: OWN2 })]);
    expect(counterpartyAddress(moved, tx({ sender: OWN1 }), OWN)).toBeUndefined();
  });

  it('a payment to several recipients has no single counterparty', () => {
    const item = one([r('SPEND', 15n), r('TXFEE', 1n)]);
    const info = tx({
      inputs: [{ address: OWN1, contract: NATIVE_CONTRACT, amount: 15n }],
      outputs: [
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 10n },
        { address: FOREIGN2, contract: NATIVE_CONTRACT, amount: 5n },
      ],
    });
    expect(counterpartyAddress(item, info, OWN)).toBeUndefined();
  });

  it('contract notes are not shown as a memo', () => {
    const item = one([r('SPEND', 500n), r('TXFEE', 1n)]);
    const info = tx({
      contractCall: true,
      outputs: [{ address: FOREIGN, contract: NATIVE_CONTRACT, amount: 500n, memo: 'mmx_offer_proceeds' }],
    });
    expect(memoFromTx(item, info, OWN)).toBeUndefined();
    expect(memoFromTx(item, { ...info, contractCall: false }, OWN)).toBe('mmx_offer_proceeds');
  });
});

describe('repeating a send', () => {
  const tx = (over: Partial<TxInfo>): TxInfo => ({
    id: testTxid('tx1'),
    expires: 1,
    didFail: false,
    fee: 1n,
    inputs: [{ address: OWN1, contract: NATIVE_CONTRACT, amount: 500n }],
    outputs: [{ address: FOREIGN, contract: NATIVE_CONTRACT, amount: 500n }],
    contractCall: false,
    ...over,
  });
  const sent = one([r('SPEND', 500n), r('TXFEE', 1n)]);

  it('repeats a plain transfer to one recipient', () => {
    expect(repeatRecipient(sent, tx({}), OWN)).toBe(FOREIGN);
    const split = tx({
      inputs: [
        { address: OWN1, contract: NATIVE_CONTRACT, amount: 300n },
        { address: OWN2, contract: NATIVE_CONTRACT, amount: 200n },
      ],
      outputs: [
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 450n },
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 50n },
      ],
    });
    const fromTwo = one([r('SPEND', 300n), r('SPEND', 200n, { address: OWN2 }), r('TXFEE', 1n)]);
    expect(repeatRecipient(fromTwo, split, OWN)).toBe(FOREIGN);
    const token = one([r('SPEND', 9n, { contract: TOKEN }), r('TXFEE', 1n)]);
    const tokenTx = tx({ inputs: [{ address: OWN1, contract: TOKEN, amount: 9n }], outputs: [{ address: FOREIGN, contract: TOKEN, amount: 9n }] });
    expect(repeatRecipient(token, tokenTx, OWN)).toBe(FOREIGN);
  });

  it('never turns an NFT purchase into a plain payment to the seller', () => {
    // nft_offer accept: the price is spent, the seller gets it, the NFT comes back.
    const purchase = tx({
      contractCall: true,
      outputs: [
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 500n, memo: 'mmx_offer_proceeds' },
        { address: OWN1, contract: testAddress(77), amount: 1n },
      ],
    });
    expect(repeatRecipient(sent, purchase, OWN)).toBeUndefined();
    // Without the operations the NFT coming back still shows it is no plain transfer.
    expect(repeatRecipient(sent, { ...purchase, contractCall: false }, OWN)).toBeUndefined();
  });

  it('does not repeat a payment to several recipients as one transfer', () => {
    const many = tx({
      outputs: [
        { address: FOREIGN, contract: NATIVE_CONTRACT, amount: 300n },
        { address: FOREIGN2, contract: NATIVE_CONTRACT, amount: 200n },
      ],
    });
    expect(repeatRecipient(sent, many, OWN)).toBeUndefined();
  });

  it('does not repeat swaps, trades or partial amounts', () => {
    expect(repeatRecipient(sent, tx({ contractCall: true }), OWN)).toBeUndefined();
    expect(repeatRecipient(sent, tx({ inputs: [{ address: FOREIGN2, contract: NATIVE_CONTRACT, amount: 500n }] }), OWN)).toBeUndefined();
    expect(repeatRecipient(sent, tx({ outputs: [{ address: FOREIGN, contract: NATIVE_CONTRACT, amount: 499n }] }), OWN)).toBeUndefined();
    expect(repeatRecipient(sent, tx({ outputs: [{ address: FOREIGN, contract: TOKEN, amount: 500n }] }), OWN)).toBeUndefined();
    const swap = one([r('SPEND', 500n), r('RECEIVE', 7n, { contract: TOKEN }), r('TXFEE', 1n)]);
    expect(repeatRecipient(swap, tx({}), OWN)).toBeUndefined();
    const received = one([r('RECEIVE', 500n)]);
    expect(repeatRecipient(received, tx({}), OWN)).toBeUndefined();
  });
});

describe('day groups and confirmations', () => {
  it('groups by local day with pending first', () => {
    const now = new Date(2026, 9, 24, 12).getTime();
    const day = (d: number, h: number) => new Date(2026, 9, d, h).getTime();
    const items = [
      { ...pendingItem({ txid: testTxid('p'), walletId: 'w', to: FOREIGN, contract: NATIVE_CONTRACT, symbol: 'MMX', decimals: 6, amount: 1n, fee: 1n, expires: 1, createdAt: now, status: 'pending' }, OWN) },
      ...groupRows([
        r('RECEIVE', 1n, { txid: testTxid('1'), height: 30, timeMs: day(24, 9) }),
        r('RECEIVE', 1n, { txid: testTxid('2'), height: 20, timeMs: day(24, 1) }),
        r('RECEIVE', 1n, { txid: testTxid('3'), height: 10, timeMs: day(23, 20) }),
      ]),
    ];
    const groups = groupByDay(items, now);
    expect(groups.map((g) => [g.label, g.items.length])).toEqual([
      ['Pending', 1],
      ['Today', 2],
      ['Yesterday', 1],
    ]);
  });

  it('counts confirmations', () => {
    const item = one([r('RECEIVE', 1n, { height: 100 })]);
    expect(confirmations(item, 100)).toBe(1);
    expect(confirmations(item, 105)).toBe(6);
    expect(confirmations(item, 99)).toBe(0);
    expect(confirmations(item, undefined)).toBe(0);
  });
});

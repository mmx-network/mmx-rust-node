/** Synthetic public WebAPI responses, shaped from ../mmx-node/src/WebAPI.cpp.
 * No ledger files or real user balances are used by the regression suite. */
import type { Block, Farmer, HistoryEntry, Transaction } from '../src/lib/types';

export const ADDRESS = 'mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev';
export const hash = (index: number): string => index.toString(16).padStart(64, '0').toUpperCase();
export const FARMER = `02${hash(7)}`;
export const GENESIS = hash(5000);
export const money = (atoms: number) => ({ amount: String(atoms), value: atoms / 1e6 });
export const transactions: Transaction[] = Array.from({ length: 43 }, (_, index) => ({
  id: hash(1000 + index),
  height: 60 - Math.floor(index / 2),
  confirm: index + 1,
  time_stamp: 1790000000000 - index * 10000,
  note: index === 0 ? '<script>untrusted memo</script>' : null,
  fee: money(1234),
  cost: money(1000),
  expires: 0xffffffff,
  sender: ADDRESS,
  did_fail: index === 2,
  message: index === 2 ? 'Insufficient balance' : null,
  inputs: [{ address: ADDRESS, is_native: true, symbol: 'MMX', decimals: 6, amount: '1000001', value: 1.000001 }],
  outputs: Array.from({ length: 23 }, (_, output) => ({
    address: ADDRESS,
    is_native: true,
    symbol: 'MMX',
    decimals: 6,
    amount: String(output + 1),
    value: (output + 1) / 1e6,
    memo: output === 0 ? '<img src=x onerror=alert(1)>' : `Output ${output + 1}`,
  })),
  operations: [{ __type: 'mmx.operation.Execute', address: ADDRESS, method: 'deposit', solution: 'must stay hidden' }],
}));

export const block = (height: number): Block => ({
  height,
  hash: height === 0 ? GENESIS : hash(5000 + height),
  prev: hash(4999 + height),
  tx_count: 23,
  reward_addr: ADDRESS,
  reward_amount: money(500000),
  tx_fees: money(2000),
  static_cost_ratio: 0.04,
  total_cost_ratio: 0.08,
  ksize: 32,
  score: 88,
  farmer_key: FARMER,
  vdf_count: 1,
  vdf_height: 70000 + height,
  vdf_iters: '689290',
  time_stamp: 1790000000000 + height * 10000,
  time_diff: 68929,
  space_diff: 9391,
  reward_vote: 0,
  reward_vote_sum: 0,
  reward_vote_count: height,
  base_reward: money(500000),
  average_txfee: money(100),
  vdf_reward_addr: [ADDRESS],
  proof: [
    { __type: 'mmx.ProofOfSpace', plot_id: hash(33), challenge: hash(44), farmer_key: FARMER, ksize: 32, score: 88 },
  ],
  tx_list: transactions.slice(0, 23).map((tx) => ({ id: tx.id, inputs: tx.inputs, outputs: tx.outputs, execute: [] })),
});
export const farmers: Farmer[] = Array.from({ length: 43 }, (_, index) => ({
  farmer_key: index ? `02${hash(7 + index)}` : FARMER,
  block_count: 1200 - index,
}));
export const history: HistoryEntry[] = Array.from({ length: 43 }, (_, index) => ({
  type: index % 2 ? 'SPEND' : 'RECEIVE',
  amount: '1000001',
  value: 1.000001,
  symbol: 'MMX',
  is_native: true,
  decimals: 6,
  contract: ADDRESS,
  height: 60 - index,
  txid: transactions[index]!.id,
  time_stamp: 1790000000000 - index * 10000,
  memo: `Entry ${index + 1}`,
}));

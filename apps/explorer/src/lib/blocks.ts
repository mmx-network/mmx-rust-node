import { ApiClient, endpoint } from './api';
import { timestamp } from './format';
import type { Block } from './types';

/** Mean transactions per block / observed mean block interval, using only these headers. */
export function transactionsPerSecond(rows: readonly Block[]): number | null {
  if (rows.length < 2) return null;
  let transactions = 0;
  let newest = 0;
  let previous = 0;
  for (const [index, row] of rows.entries()) {
    const time = timestamp(row);
    if (time === null || row.tx_count === undefined || !Number.isSafeInteger(row.tx_count) || row.tx_count < 0)
      return null;
    if (index === 0) newest = time;
    else if (time >= previous || row.height !== rows[index - 1]!.height - 1) return null;
    transactions += row.tx_count;
    previous = time;
  }
  // N timestamps span N-1 intervals: using N transaction counts directly over
  // that span would overstate steady throughput by N/(N-1).
  const meanIntervalSeconds = (newest - previous) / 1000 / (rows.length - 1);
  return transactions / rows.length / meanIntervalSeconds;
}

/** C++ /headers excludes genesis (min(limit, offset)); add it at the boundary. */
export async function blockPage(client: ApiClient, offset: number | undefined, signal: AbortSignal): Promise<Block[]> {
  if (offset === 0) {
    const genesis = await client.get<Block>(endpoint('/header', { height: 0 }), signal);
    return genesis ? [genesis] : [];
  }
  const result = await client.get<Block[]>(
    endpoint('/headers', { limit: 20, ...(offset === undefined ? {} : { offset }) }),
    signal,
  );
  if (!Array.isArray(result)) return [];
  const rows = result.filter((row) => row !== null);
  if (rows.length < 20 && (rows.at(-1)?.height === 1 || (offset === undefined && rows.length === 0))) {
    const genesis = await client.get<Block>(endpoint('/header', { height: 0 }), signal);
    if (genesis) rows.push(genesis);
  }
  return rows;
}

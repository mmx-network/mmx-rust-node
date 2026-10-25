import { ApiClient, endpoint } from './api';
import type { Block } from './types';

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

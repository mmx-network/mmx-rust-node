import { describe, expect, it } from 'vitest';
import { ApiClient } from '../../src/lib/api';
import { blockPage } from '../../src/lib/blocks';
import type { Block } from '../../src/lib/types';

const block = (height: number): Block => ({ height, hash: height.toString(16).padStart(64, '0') });
function chain(tip: number) {
  const requests: string[] = [];
  const client = new ApiClient(undefined, async (input) => {
    const url = new URL(String(input));
    requests.push(url.pathname + url.search);
    if (url.pathname === '/header') return new Response(JSON.stringify(block(0)));
    const offset = Number(url.searchParams.get('offset') ?? tip);
    // Deliberately match WebAPI::render_headers: offset never includes zero.
    const rows = Array.from({ length: Math.min(20, offset) }, (_, index) => block(offset - index));
    return new Response(JSON.stringify(rows));
  });
  return { client, requests };
}

describe('bounded block history with the C++ headers contract', () => {
  it('visits every height through genesis without duplicates or a full-chain scan', async () => {
    const { client, requests } = chain(65);
    const heights: number[] = [];
    for (const offset of [undefined, 45, 25, 5])
      heights.push(...(await blockPage(client, offset, new AbortController().signal)).map((row) => row.height));
    expect(heights).toEqual(Array.from({ length: 66 }, (_, index) => 65 - index));
    expect(requests).toEqual([
      '/headers?limit=20',
      '/headers?limit=20&offset=45',
      '/headers?limit=20&offset=25',
      '/headers?limit=20&offset=5',
      '/header?height=0',
    ]);
  });
  it('fetches genesis alone when exactly one page remains', async () => {
    const { client, requests } = chain(20);
    expect((await blockPage(client, undefined, new AbortController().signal)).map((row) => row.height)).toHaveLength(
      20,
    );
    expect(await blockPage(client, 0, new AbortController().signal)).toEqual([block(0)]);
    expect(requests).toEqual(['/headers?limit=20', '/header?height=0']);
  });
  it('handles a genesis-only chain', async () => {
    const { client } = chain(0);
    expect(await blockPage(client, undefined, new AbortController().signal)).toEqual([block(0)]);
  });
});

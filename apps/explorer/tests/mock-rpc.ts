import { createServer, type Server } from 'node:http';
import { pathToFileURL } from 'node:url';
import { ADDRESS, FARMER, block, farmers, history, money, transactions } from './fixtures.ts';

export function mockRpc(port: number): Server {
  const requests: string[] = [];
  let fail = '';
  let tip = 65;
  const server = createServer((request, response) => {
    const url = new URL(request.url ?? '/', 'http://localhost');
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    const send = (data: unknown, status = 200): void => {
      response.writeHead(status);
      response.end(JSON.stringify(data));
    };
    if (url.pathname === '/__state') {
      if (url.searchParams.has('fail')) fail = url.searchParams.get('fail')!;
      if (url.searchParams.has('tip')) tip = Number(url.searchParams.get('tip'));
      if (url.searchParams.has('reset')) {
        requests.length = 0;
        fail = '';
        tip = 65;
      }
      send({ requests, tip });
      return;
    }
    if (request.method !== 'GET') {
      send(null, 405);
      return;
    }
    requests.push(url.pathname + url.search);
    if (fail && fail === url.pathname) {
      send({ error: 'Synthetic unavailable server' }, 503);
      return;
    }
    const id = url.searchParams.get('id');
    const limit = Number(url.searchParams.get('limit') ?? 20);
    const height = Number(url.searchParams.get('height'));
    switch (url.pathname) {
      case '/node/info':
        send({ name: 'testnet12', height: tip, is_synced: true, total_space: 18511539, block_reward: money(500000) });
        return;
      case '/headers': {
        const offset = Number(url.searchParams.get('offset') ?? tip);
        send(
          Array.from({ length: Math.min(limit, offset) }, (_, index) => {
            const row = block(offset - index);
            delete row.proof;
            delete row.tx_list;
            return row;
          }),
        );
        return;
      }
      case '/header':
      case '/block': {
        const match = url.searchParams.has('hash')
          ? Array.from({ length: tip + 1 }, (_, index) => block(index)).find(
              (row) => row.hash === url.searchParams.get('hash'),
            )
          : height <= tip
            ? block(height)
            : null;
        send(match ?? null, match ? 200 : 204);
        return;
      }
      case '/transactions':
        send(transactions.slice(0, limit));
        return;
      case '/transaction':
        send(transactions.find((row) => row.id === id) ?? null);
        return;
      case '/farmers':
        send(farmers.slice(0, limit));
        return;
      case '/farmer':
        send(
          id === FARMER
            ? {
                farmer_key: FARMER,
                block_count: 1234,
                total_reward: '123456789123456789',
                total_reward_value: 123456789123.45679,
                rewards: [{ address: ADDRESS, amount: '123456789123456789', value: 123456789123.45679 }],
              }
            : null,
        );
        return;
      case '/farmer/blocks':
        send(Array.from({ length: 43 }, (_, index) => block(tip - index)));
        return;
      case '/balance':
        send({
          balances: [
            {
              is_native: true,
              contract: ADDRESS,
              symbol: 'MMX',
              total: 12.345678,
              locked: 1,
              spendable: 11.345678,
              decimals: 6,
            },
          ],
          nfts: [ADDRESS],
        });
        return;
      case '/address/history':
        send(history);
        return;
      case '/contract':
        send({ __type: 'mmx.contract.PubKey', address: ADDRESS, key: FARMER });
        return;
      default:
        send(null, 404);
    }
  });
  return server.listen(port, '127.0.0.1');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const server = mockRpc(Number(process.env.EXPLORER_TEST_RPC_PORT ?? 5186));
  server.on('listening', () =>
    process.stdout.write(`Synthetic chain server listening on ${JSON.stringify(server.address())}\n`),
  );
  for (const signal of ['SIGINT', 'SIGTERM'] as const) process.on(signal, () => server.close(() => process.exit(0)));
}

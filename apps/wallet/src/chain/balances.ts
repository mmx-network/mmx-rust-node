/**
 * Wallet balances: `/address?id=…&limit=1000` per address (NFTs skipped).
 * Totals per contract plus per-address amounts. Tokens are verified by the
 * bundled list (chain/tokens.ts), never by a server flag.
 */
import { NATIVE_CONTRACT } from '../lib/address';
import type { RpcClient } from './rpc';
import { tokenVerifier } from './tokens';
import type { BalanceRow, ChainParams, Currency } from './types';

export interface AddressBalance {
  rows: BalanceRow[];
}

export interface AssetBalance extends Currency {
  /** Native MMX or a token on the bundled list. */
  verified: boolean;
  total: bigint;
  /** address → amount (only addresses holding it). */
  byAddress: Map<string, bigint>;
}

export async function fetchAddressBalance(rpc: RpcClient, address: string, params: ChainParams): Promise<AddressBalance> {
  return { rows: await rpc.addressBalances(address, params) };
}

/**
 * Fetches balances for `addresses` (concurrency is bounded by the client).
 * Rejects if any address fails, so a snapshot is never partially stale.
 */
export async function fetchBalances(
  rpc: RpcClient,
  addresses: readonly string[],
  params: ChainParams,
): Promise<Map<string, AddressBalance>> {
  const results = await Promise.all(addresses.map((a) => fetchAddressBalance(rpc, a, params)));
  const out = new Map<string, AddressBalance>();
  addresses.forEach((a, i) => out.set(a, results[i]!));
  return out;
}

/**
 * Sums per contract over the given addresses. Native MMX is always present
 * (possibly zero). Order: MMX, verified tokens by symbol, other tokens by symbol.
 * Currency metadata comes from the first address reporting it. Tokens are
 * verified by the bundled list of the server's network unless a lookup is given.
 */
export function computeAssets(
  byAddress: ReadonlyMap<string, AddressBalance>,
  addresses: readonly string[],
  params: ChainParams,
  isVerified: (contract: string) => boolean = tokenVerifier(params.network),
): AssetBalance[] {
  const assets = new Map<string, AssetBalance>();
  assets.set(NATIVE_CONTRACT, {
    contract: NATIVE_CONTRACT,
    symbol: 'MMX',
    decimals: params.decimals,
    native: true,
    verified: true,
    total: 0n,
    byAddress: new Map(),
  });
  for (const address of addresses) {
    const entry = byAddress.get(address);
    if (!entry) continue;
    for (const row of entry.rows) {
      let asset = assets.get(row.contract);
      if (!asset) {
        asset = {
          contract: row.contract,
          symbol: row.symbol,
          decimals: row.decimals,
          native: false,
          verified: isVerified(row.contract),
          total: 0n,
          byAddress: new Map(),
        };
        assets.set(row.contract, asset);
      }
      if (row.amount > 0n) {
        asset.total += row.amount;
        asset.byAddress.set(address, (asset.byAddress.get(address) ?? 0n) + row.amount);
      }
    }
  }
  return sortAssets([...assets.values()].filter((a) => a.native || a.total > 0n));
}

export function sortAssets<T extends { native: boolean; verified: boolean; symbol: string; contract: string }>(list: T[]): T[] {
  const rank = (a: T) => (a.native ? 0 : a.verified ? 1 : 2);
  return list.slice().sort((a, b) => rank(a) - rank(b) || a.symbol.localeCompare(b.symbol) || a.contract.localeCompare(b.contract));
}

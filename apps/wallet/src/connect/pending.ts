import type { PendingTx } from '../chain/pending';
import { NATIVE_CONTRACT } from '../lib/address';
import { object, type PreparedTransaction } from './protocol';
/** Gross inputs remain reserved; known change back to this wallet is deducted
 * only from the displayed outgoing estimate, never from the spend reservation. */
export function customPending(p: PreparedTransaction, walletId: string, own: readonly string[], uncertain = false): PendingTx {
  const reserved = new Map<string, bigint>();
  const outgoing = new Map<string, bigint>();
  for (const input of p.transaction.inputs as Array<{ contract: string; amount: string }>) {
    reserved.set(input.contract, (reserved.get(input.contract) ?? 0n) + BigInt(input.amount));
  }
  for (const [key, n] of reserved) outgoing.set(key, n);
  for (const output of p.transaction.outputs as Array<{ address: string; contract: string; amount: string }>) {
    if (own.includes(output.address)) outgoing.set(output.contract, (outgoing.get(output.contract) ?? 0n) - BigInt(output.amount));
  }
  const first = (p.transaction.outputs as unknown[])[0] ?? (p.transaction.execute as unknown[])[0];
  return {
    txid: p.txid,
    walletId,
    to: object(first) && typeof first.address === 'string' ? first.address : own[0]!,
    contract: NATIVE_CONTRACT,
    symbol: 'MMX',
    decimals: p.decimals,
    amount: outgoing.get(NATIVE_CONTRACT) ?? 0n,
    fee: BigInt(p.maxFee),
    expires: p.expires,
    network: p.network,
    createdAt: Date.now(),
    status: 'pending',
    ...(uncertain ? { uncertain: true } : {}),
    custom: {
      reserved: [...reserved].map(([contract, amount]) => ({ contract, amount })),
      outgoing: [...outgoing]
        .filter(([, n]) => n > 0n)
        .map(([contract, amount]) => {
          const currency = p.currencies.find((c) => c.contract === contract);
          return { contract, amount, symbol: currency?.symbol ?? 'Token', decimals: currency?.decimals ?? 0 };
        }),
    },
  };
}

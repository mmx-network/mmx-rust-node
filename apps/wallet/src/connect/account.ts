import type { Backend } from '../backend/types';
import { parseChainParams } from '../chain/rpc';
import type { Account } from './protocol';

/** Cached public addresses can be shared after consent while signing is locked. */
export async function publicAccount(backend: Backend, walletId: string): Promise<Account> {
  const [addresses, raw]=await Promise.all([backend.getAddresses(walletId),backend.rpcGet('/chain/info')]);
  const {network,decimals}=parseChainParams(raw);
  return {walletId,address:addresses[0]!,addresses,network,decimals};
}

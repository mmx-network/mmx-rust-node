/**
 * Token verification. A token counts as verified only when its contract is on
 * the wallet's bundled list for the network, like the C++ wallet's
 * `token_whitelist` (which on mainnet holds native MMX only).
 *
 * The node's `is_validated` flag is never used: `/balance` reports it as false
 * for every token unless a `currency` filter is passed, and with one it is
 * true for any contract. History rows always report false.
 */
import { NATIVE_CONTRACT } from '../lib/address';

/** Verified token contracts per network (lowercase addresses). */
const VERIFIED_TOKENS: Readonly<Record<string, readonly string[]>> = {
  mainnet: [],
};

const sets = new Map<string, ReadonlySet<string>>();

function listFor(network: string): ReadonlySet<string> {
  let set = sets.get(network);
  if (!set) {
    set = new Set(Object.hasOwn(VERIFIED_TOKENS, network) ? VERIFIED_TOKENS[network] : []);
    sets.set(network, set);
  }
  return set;
}

/** True for native MMX and for tokens on the bundled list of `network`. */
export function isVerifiedToken(network: string | undefined, contract: string): boolean {
  if (contract === NATIVE_CONTRACT) return true;
  return network !== undefined && listFor(network).has(contract);
}

/** A lookup bound to one network (unknown network: native MMX only). */
export function tokenVerifier(network: string | undefined): (contract: string) => boolean {
  return (contract) => isVerifiedToken(network, contract);
}

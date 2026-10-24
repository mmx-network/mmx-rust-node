/** Validated chain data as used by the UI. Amounts are bigint atomic units. */

export interface ChainParams {
  network: string;
  /** Native currency decimals. */
  decimals: number;
  blockIntervalMs: number;
  /** Allowed fee multipliers (`min_fee_ratio`). */
  minFeeRatio: number[];
  minTxfee: number;
  minTxfeeIo: number;
  minTxfeeSign: number;
  minTxfeeMemo: number;
}

export interface NodeInfo {
  height: number;
  synced: boolean;
  network: string;
  /** Node clock in ms, when reported. */
  timeMs?: number;
}

export type TxType = 'SPEND' | 'RECEIVE' | 'TXFEE' | 'REWARD' | 'VDF_REWARD' | 'PROJECT_REWARD';

export const TX_TYPES: readonly TxType[] = ['SPEND', 'RECEIVE', 'TXFEE', 'REWARD', 'VDF_REWARD', 'PROJECT_REWARD'];

/** Currency metadata attached to amounts. `native` is decided by the zero contract only. */
export interface Currency {
  contract: string;
  symbol: string;
  decimals: number;
  native: boolean;
}

/** One `/address?id=` balance row. */
export interface BalanceRow extends Currency {
  amount: bigint;
}

/** One `/address/history` entry (confirmed only). */
export interface HistoryRow extends Currency {
  /** Transaction id; for reward types the block hash. Uppercase hex. */
  txid: string;
  height: number;
  timeMs: number;
  type: TxType;
  /** The wallet's own address this entry belongs to. */
  address: string;
  amount: bigint;
  memo?: string;
  /** Token is on the node's whitelist (always false for MMX itself). */
  validated: boolean;
}

export interface TxIo {
  address: string;
  contract: string;
  amount: bigint;
  memo?: string;
  symbol?: string;
  decimals?: number;
}

/** `/transaction?id=` */
export interface TxInfo {
  id: string;
  height?: number;
  timeMs?: number;
  expires: number;
  didFail: boolean;
  message?: string;
  /** Fee paid, native atomic units. */
  fee: bigint;
  sender?: string;
  inputs: TxIo[];
  outputs: TxIo[];
}

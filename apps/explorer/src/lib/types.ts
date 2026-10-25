export interface Amount {
  amount?: string | number;
  value?: string | number;
}
export interface Timed {
  time_stamp?: number | null;
  time?: number | null;
}
export interface NodeInfo {
  height?: number;
  is_synced?: boolean;
  name?: string;
  netspace?: number;
  total_space?: number;
}
export interface Proof {
  farmer_key?: string;
  plot_id?: string;
  challenge?: string;
  ksize?: number;
  score?: number;
  [key: string]: unknown;
}
export interface Block extends Timed {
  height: number;
  hash: string;
  prev?: string;
  tx_count?: number;
  reward_addr?: string | null;
  reward_contract?: string | null;
  reward_account?: string | null;
  reward_amount?: Amount;
  tx_fees?: Amount;
  base_reward?: Amount;
  average_txfee?: Amount;
  total_cost_ratio?: number;
  static_cost_ratio?: number;
  time_diff?: string | number;
  space_diff?: string | number;
  reward_vote?: number;
  reward_vote_sum?: number;
  reward_vote_count?: number;
  vdf_height?: number;
  vdf_count?: number;
  vdf_iters?: string | number;
  vdf_reward_addr?: string[];
  vdf_reward_payout?: string | null;
  ksize?: number;
  score?: number;
  space_fork_proofs?: number;
  space_fork_len?: number;
  farmer_key?: string;
  proof?: Proof[];
  tx_list?: BlockTransaction[];
}
export interface BlockTransaction {
  id: string;
  inputs?: unknown[];
  outputs?: unknown[];
  execute?: unknown[];
  exec_result?: { inputs?: unknown[]; outputs?: unknown[] } | null;
}
export interface Token {
  contract?: string;
  symbol?: string;
  is_native?: boolean;
  is_nft?: boolean;
  decimals?: number;
}
export interface Transfer extends Token {
  address?: string;
  amount?: string | number;
  value?: string | number;
  memo?: string | null;
}
export interface Transaction extends Timed {
  id: string;
  height?: number | null;
  did_fail?: boolean;
  confirm?: number;
  expires?: number;
  note?: string | null;
  message?: string | null;
  sender?: string | null;
  address?: string;
  fee?: Amount;
  cost?: Amount;
  inputs?: Transfer[];
  outputs?: Transfer[];
  operations?: Record<string, unknown>[];
  deployed?: Record<string, unknown> | null;
}
export interface Farmer {
  farmer_key: string;
  block_count: number;
}
export interface FarmerInfo extends Farmer {
  total_reward?: string | number;
  total_reward_value?: number;
  rewards?: { address: string; amount?: string | number; value?: number }[];
}
export interface Balance extends Token {
  total?: string | number;
  locked?: string | number;
  spendable?: string | number;
}
export interface BalanceResponse {
  balances: Balance[];
}
export interface HistoryEntry extends Transfer, Timed {
  type: string;
  txid?: string;
  height?: number | null;
}

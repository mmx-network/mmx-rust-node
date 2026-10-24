use crate::{
    error::{invalid, rpc_invalid, Result},
    rpc::{self, Rpc},
};
use mmx_wallet::{format_amount, Address, ChainParams};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Concurrent requests per wallet (the UI's chain client uses the same).
const FETCH_WORKERS: usize = 4;

/// GETs every path, up to FETCH_WORKERS requests at a time. Results are in
/// path order. With `fail_fast`, no new request starts after one failed and
/// the paths not requested are `None`.
fn get_each(rpc: &Rpc, paths: &[String], fail_fast: bool) -> Vec<Option<Result<Value>>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let work = || {
        let mut done = Vec::new();
        while !failed.load(Ordering::Relaxed) {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(path) = paths.get(i) else {
                break;
            };
            let result = rpc.get(path);
            if fail_fast && result.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            done.push((i, result));
        }
        done
    };
    let mut results: Vec<Option<Result<Value>>> = Vec::new();
    results.resize_with(paths.len(), || None);
    std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..FETCH_WORKERS.min(paths.len()))
            .filter_map(|_| std::thread::Builder::new().spawn_scoped(scope, work).ok())
            .collect();
        // This thread works too, so progress never depends on spawning.
        let mut done = work();
        for helper in helpers {
            if let Ok(more) = helper.join() {
                done.extend(more);
            }
        }
        for (i, result) in done {
            results[i] = Some(result);
        }
    });
    results
}

/// `/address` for every address, in address order; on failure the error of
/// the first failed address is returned and no new requests are started.
fn fetch_balances(rpc: &Rpc, addresses: &[Address]) -> Result<Vec<Value>> {
    let paths: Vec<String> = addresses
        .iter()
        .map(|a| format!("/address?id={a}&limit=1000"))
        .collect();
    let mut values = Vec::with_capacity(addresses.len());
    for result in get_each(rpc, &paths, true) {
        match result {
            Some(Ok(value)) => values.push(value),
            Some(Err(e)) => return Err(e),
            // Skipped after an earlier failure, which is reported below.
            None => (),
        }
    }
    if values.len() != addresses.len() {
        // A helper panicked; never return partial balances.
        return Err(rpc_invalid("incomplete balances response"));
    }
    Ok(values)
}

/// What the chain server knows about a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStatus {
    /// The request failed or the answer was not understood.
    Unavailable,
    /// Not in a block (waiting, or unknown to the server).
    NotIncluded,
    /// In the block at this height (executed or failed).
    Included(u32),
}

/// `/transaction?id=` for every (lowercase hexadecimal) transaction ID, up
/// to FETCH_WORKERS requests at a time, in ID order.
pub fn tx_statuses(rpc: &Rpc, txids: &[String]) -> Vec<TxStatus> {
    let paths: Vec<String> = txids
        .iter()
        .map(|id| format!("/transaction?id={id}"))
        .collect();
    get_each(rpc, &paths, false)
        .into_iter()
        .zip(txids)
        .map(|(result, id)| match result {
            Some(Ok(Value::Null)) => TxStatus::NotIncluded,
            Some(Ok(tx)) if rpc::text(&tx, "id").is_ok_and(|t| t.eq_ignore_ascii_case(id)) => {
                if tx["height"].is_null() {
                    TxStatus::NotIncluded
                } else {
                    rpc::u32_field(&tx, "height").map_or(TxStatus::Unavailable, TxStatus::Included)
                }
            }
            _ => TxStatus::Unavailable,
        })
        .collect()
}

pub struct Balance {
    pub amount: u128,
    pub decimals: u32,
    pub symbol: String,
}
pub struct State {
    pub height: u32,
    pub balances: BTreeMap<(Address, Address), u128>,
    pub totals: BTreeMap<Address, Balance>,
}
impl State {
    pub fn fetch(rpc: &Rpc, addresses: &[Address], params: &ChainParams) -> Result<Self> {
        let height = rpc.height(params)?;
        let mut balances = BTreeMap::new();
        let mut totals: BTreeMap<Address, Balance> = BTreeMap::new();
        for (address, v) in addresses.iter().zip(fetch_balances(rpc, addresses)?) {
            for row in v["balances"]
                .as_array()
                .ok_or_else(|| rpc_invalid("invalid balances response"))?
            {
                let currency = rpc::address(row, "contract")?;
                // NFT holdings have no fungible currency metadata in the node
                // response and are not part of this CLI's currency balances.
                if row["is_nft"].as_bool() == Some(true) {
                    continue;
                }
                let amount = rpc::atomic(&row["amount"])?;
                let decimals = rpc::decimals(row, currency, params)?;
                let symbol = rpc::text(row, "symbol")?.to_owned();
                if balances.insert((*address, currency), amount).is_some() {
                    return Err(rpc_invalid("duplicate balance"));
                }
                if let Some(total) = totals.get_mut(&currency) {
                    if total.decimals != decimals || total.symbol != symbol {
                        return Err(rpc_invalid("inconsistent currency metadata"));
                    }
                    total.amount = total
                        .amount
                        .checked_add(amount)
                        .ok_or_else(|| rpc_invalid("balance total exceeds 128 bits"))?;
                } else {
                    totals.insert(
                        currency,
                        Balance {
                            amount,
                            decimals,
                            symbol,
                        },
                    );
                }
            }
        }
        Ok(Self {
            height,
            balances,
            totals,
        })
    }
    pub fn rows(&self, filter: &Filter, params: &ChainParams) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for (currency, total) in &self.totals {
            if filter.matches(*currency, &total.symbol) {
                rows.push(json!({"currency_address":currency, "symbol":total.symbol, "decimals":total.decimals,
                    "amount_atomic":total.amount.to_string(), "amount":format_amount(total.amount, total.decimals)?}));
            }
        }
        if rows.is_empty() {
            match filter {
                Filter::Symbol(_) => return Err(invalid("no currencies match symbol")),
                Filter::Currency(c) if *c != Address::default() => (),
                _ => rows.push(json!({"currency_address":Address::default(), "symbol":"MMX", "decimals":params.decimals, "amount_atomic":"0", "amount":"0"})),
            }
        }
        Ok(rows)
    }
}
pub enum Filter {
    All,
    Currency(Address),
    Symbol(String),
}
impl Filter {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "all" => Self::All,
            "" | "MMX" => Self::Currency(Address::default()),
            s if s.starts_with("mmx1") => Self::Currency(s.parse()?),
            _ => Self::Symbol(s.into()),
        })
    }
    fn matches(&self, currency: Address, symbol: &str) -> bool {
        match self {
            Self::All => true,
            Self::Currency(c) => *c == currency,
            Self::Symbol(s) => s == symbol,
        }
    }
}
pub fn currency(s: &str) -> Result<Address> {
    match s {
        "" | "MMX" => Ok(Address::default()),
        s if s.starts_with("mmx1") => Ok(s.parse()?),
        _ => Err(invalid("sending a token requires its contract address")),
    }
}
pub fn history(
    rpc: &Rpc,
    addresses: &[Address],
    params: &ChainParams,
    filter: &Filter,
    limit: u32,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for address in addresses {
        let mut until = None;
        let mut matches = 0usize;
        loop {
            let request_limit = if matches!(filter, Filter::Symbol(_)) {
                1000
            } else {
                limit
            };
            let mut path = format!("/address/history?id={address}&limit={request_limit}");
            if let Filter::Currency(c) = filter {
                path.push_str(&format!("&currency={c}"));
            }
            if let Some(h) = until {
                path.push_str(&format!("&until={h}"));
            }
            let v = rpc.get(&path)?;
            let values = v
                .as_array()
                .ok_or_else(|| rpc_invalid("invalid history response"))?;
            if values.is_empty() {
                break;
            }
            let mut min_height = u32::MAX;
            for row in values {
                let c = rpc::address(row, "contract")?;
                let h = if row["height"].is_null() && rpc::boolean(row, "is_pending")? {
                    0
                } else {
                    rpc::u32_field(row, "height")?
                };
                min_height = min_height.min(h);
                if let Filter::Currency(want) = filter {
                    if *want != c {
                        return Err(rpc_invalid("history for wrong currency"));
                    }
                }
                if !filter.matches(c, rpc::text(row, "symbol")?) {
                    continue;
                }
                let decimals = rpc::decimals(row, c, params)?;
                let amount = rpc::atomic(&row["amount"])?;
                let mut row = row.clone();
                row["amount_atomic"] = json!(amount.to_string());
                row["amount"] = json!(format_amount(amount, decimals)?);
                rows.push(row);
                matches += 1;
            }
            if !matches!(filter, Filter::Symbol(_))
                || matches >= limit as usize
                || values.len() < request_limit as usize
                || min_height == 0
            {
                break;
            }
            let next = min_height - 1;
            if until.is_some_and(|h| next >= h) {
                return Err(rpc_invalid("history pagination made no progress"));
            }
            until = Some(next);
        }
    }
    rows.sort_by_key(|row| {
        std::cmp::Reverse((
            row["is_pending"].as_bool().unwrap_or(false),
            row["height"].as_u64().unwrap_or(0),
            row["time_stamp"].as_i64().unwrap_or(0),
        ))
    });
    rows.truncate(limit as usize);
    Ok(rows)
}

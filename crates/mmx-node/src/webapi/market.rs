//! Public offer/swap queries. Contract state is pinned on misses; the frontend
//! caches the final public JSON using the same selected-chain generation.
use super::{
    render::{self, amount},
    storage::View,
    Request, Response,
};
use crate::{error, Result};
use mmx_execution::chain::public as index;
use mmx_vm::{Address, Storage, U256};
use std::collections::BTreeMap;
use vnx::serde_json::{json, Value};

fn integer(value: &Value) -> Result<u128> {
    if value.is_null() {
        Ok(0)
    } else {
        render::uint(value)
    }
}
fn addr(value: &Value) -> Result<Address> {
    if value.is_null() {
        return Ok([0; 32]);
    }
    if let Some(text) = value.as_str() {
        return Ok(mmx_vm::address::decode(text.as_bytes())?);
    }
    if let Some(text) = value["address"].as_str() {
        return Ok(mmx_vm::address::decode(text.as_bytes())?);
    }
    render::address(value)
}
fn pair(value: &Value) -> Result<[u128; 2]> {
    Ok([integer(&value[0])?, integer(&value[1])?])
}
fn sum_pools(data: &Value, field: &str) -> Result<[u128; 2]> {
    let mut sum = [0u128; 2];
    for pool in data.as_array().into_iter().flatten() {
        let values = pair(&pool[field])?;
        for i in 0..2 {
            sum[i] = sum[i]
                .checked_add(values[i])
                .ok_or_else(|| error("pool amount overflow"))?;
        }
    }
    Ok(sum)
}
fn low(value: U256) -> Result<u128> {
    if value >> 128 != U256::zero() {
        return Err(error("amount overflow"));
    }
    Ok(value.low_u128())
}
fn bid_amount(amount: u128, inverse: u128) -> Result<u128> {
    low((U256::from(amount) * U256::from(inverse)) >> 64)
}
fn ask_amount(amount: u128, inverse: u128) -> Result<u128> {
    if inverse == 0 {
        return Err(error("zero offer inverse price"));
    }
    low(((U256::from(amount) << 64) + U256::from(inverse) - U256::one()) / U256::from(inverse))
}
fn parse_amount(text: &str, decimals: i32) -> Result<u128> {
    if text.len() > 128 || !(0..=38).contains(&decimals) {
        return Err(error("invalid decimal amount"));
    }
    let (mantissa, exp) = if let Some(pos) = text.find(['e', 'E']) {
        (&text[..pos], text[pos + 1..].parse::<i32>()?)
    } else {
        (text, 0)
    };
    if !(-128..=128).contains(&exp) {
        return Err(error("invalid amount exponent"));
    }
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|v| v.is_ascii_digit())
    {
        return Err(error("invalid decimal amount"));
    }
    let mut digits = format!("{whole}{fraction}");
    let shift = decimals + exp - fraction.len() as i32;
    if shift < 0 {
        digits.truncate(digits.len().saturating_sub((-shift) as usize));
    } else {
        if digits.len() + shift as usize > 166 {
            return Err(error("amount overflow"));
        }
        digits.extend(std::iter::repeat_n('0', shift as usize));
    }
    if digits.is_empty() {
        Ok(0)
    } else {
        Ok(digits.parse()?)
    }
}
fn decimal(amount: u128, decimals: i32) -> String {
    let mut text = amount.to_string();
    if decimals <= 0 {
        return text;
    }
    let decimals = decimals as usize;
    if text.len() <= decimals {
        text = format!("{}{}", "0".repeat(decimals + 1 - text.len()), text);
    }
    text.insert(text.len() - decimals, '.');
    text.trim_end_matches('0').trim_end_matches('.').into()
}
fn amount_str(value: u128, decimals: i32) -> Value {
    json!({"amount":value.to_string(),"value":decimal(value,decimals)})
}

struct Offer {
    id: Address,
    bid: Address,
    ask: Address,
    balance: u128,
    ask_balance: u128,
    inverse: u128,
    owner: Address,
    partner: Option<Address>,
    height: u32,
    time: Value,
    last: u128,
}
struct Swap {
    id: Address,
    tokens: [Address; 2],
    value: Value,
}
impl Request<'_> {
    fn offer(&self, id: Address, view: &mut View) -> Result<Offer> {
        let data = view.named(
            id,
            &[
                "owner",
                "partner",
                "bid_currency",
                "ask_currency",
                "inv_price",
                "last_update",
            ],
        )?;
        if data.values().all(Value::is_null) {
            return Err(error("no such offer"));
        }
        let get = |key: &str| data.get(key).unwrap_or(&Value::Null);
        let bid = addr(get("bid_currency"))?;
        let ask = addr(get("ask_currency"))?;
        let height = view
            .storage
            .get_transaction_index(&id)?
            .map_or(0, |v| v.height);
        let header = self.call("get_header_at", json!({"height":height}))?;
        Ok(Offer {
            id,
            bid,
            ask,
            balance: view.storage.balance_at(&id, &bid, None)?.unwrap_or(0),
            ask_balance: view.storage.balance_at(&id, &ask, None)?.unwrap_or(0),
            inverse: integer(get("inv_price"))?,
            owner: addr(get("owner"))?,
            partner: if get("partner").is_null() {
                None
            } else {
                Some(addr(get("partner"))?)
            },
            height,
            time: header["time_stamp"].clone(),
            last: integer(get("last_update"))?,
        })
    }
    fn render_offer(&mut self, offer: &Offer) -> Result<Value> {
        self.currency(offer.bid)?;
        self.currency(offer.ask)?;
        let price = 18446744073709551616_f64 / offer.inverse as f64;
        let mut out = json!({"__type":"mmx.offer_data_t","address":mmx_vm::address::encode(&offer.id),"height":offer.height,"time_stamp":offer.time,
            "owner":mmx_vm::address::encode(&offer.owner),"partner":offer.partner.map(|v|mmx_vm::address::encode(&v)),"last_update":offer.last as u32,
            "bid_currency":mmx_vm::address::encode(&offer.bid),"ask_currency":mmx_vm::address::encode(&offer.ask),"bid_balance":offer.balance.to_string(),"ask_balance":offer.ask_balance.to_string(),
            "inv_price":offer.inverse.to_string(),"ask_amount":ask_amount(offer.balance,offer.inverse)?.to_string(),"price":price,"time":offer.time.as_f64().unwrap_or(0.)/1000.});
        if let Some(info) = self.context.currencies.get(&offer.bid) {
            out["bid_balance_value"] = json!(offer.balance as f64 / 10_f64.powi(info.decimals));
            out["bid_symbol"] = json!(info.symbol);
            out["bid_decimals"] = json!(info.decimals);
        }
        if let Some(info) = self.context.currencies.get(&offer.ask) {
            out["ask_balance_value"] = json!(offer.ask_balance as f64 / 10_f64.powi(info.decimals));
            out["ask_symbol"] = json!(info.symbol);
            out["ask_decimals"] = json!(info.decimals);
            out["ask_value"] = json!(
                ask_amount(offer.balance, offer.inverse)? as f64 / 10_f64.powi(info.decimals)
            );
        }
        if let (Some(bid), Some(ask)) = (
            self.context.currencies.get(&offer.bid),
            self.context.currencies.get(&offer.ask),
        ) {
            out["display_price"] = json!(price * 10_f64.powi(bid.decimals - ask.decimals));
        }
        Ok(out)
    }
    fn swap(&mut self, id: Address, view: &mut View) -> Result<Swap> {
        let contract = self.call("get_contract", json!({"address":id}))?;
        if contract["__type"] != "mmx.contract.Executable" {
            return Err(error("no such swap"));
        }
        let data = view.named(id, &["tokens", "volume", "fee_rates", "state"])?;
        let get = |key: &str| data.get(key).cloned().unwrap_or(Value::Null);
        let tokens = [addr(&get("tokens")[0])?, addr(&get("tokens")[1])?];
        for id in tokens {
            self.currency(id)?;
        }
        let state = get("state");
        let volume = pair(&get("volume"))?;
        let fields = ["balance", "fees_paid", "fees_claimed", "user_total"];
        let mut sums = BTreeMap::new();
        for field in fields {
            sums.insert(field, sum_pools(&state, field)?);
        }
        let previous1 =
            view.named_at(id, &["volume", "state"], view.height.saturating_sub(8640))?;
        let previous7 =
            view.named_at(id, &["volume", "state"], view.height.saturating_sub(60480))?;
        let old = |data: &BTreeMap<String, Value>, field: &str| {
            data.get(field).cloned().unwrap_or(Value::Null)
        };
        let mut volumes1 = volume;
        let mut volumes7 = volume;
        let old1 = pair(&old(&previous1, "volume"))?;
        let old7 = pair(&old(&previous7, "volume"))?;
        for i in 0..2 {
            volumes1[i] = volumes1[i].wrapping_sub(old1[i]);
            volumes7[i] = volumes7[i].wrapping_sub(old7[i]);
        }
        let paid1 = sum_pools(&old(&previous1, "state"), "fees_paid")?;
        let paid7 = sum_pools(&old(&previous7, "state"), "fees_paid")?;
        let decimals = tokens.map(|id| self.context.currencies.get(&id).map_or(0, |v| v.decimals));
        let symbols = tokens.map(|id| {
            self.context
                .currencies
                .get(&id)
                .map_or(String::new(), |v| v.symbol.clone())
        });
        let amounts = |numbers: [u128; 2]| -> Value {
            json!([
                amount(numbers[0], decimals[0]),
                amount(numbers[1], decimals[1])
            ])
        };
        let wallet = [
            view.storage.balance_at(&id, &tokens[0], None)?.unwrap_or(0),
            view.storage.balance_at(&id, &tokens[1], None)?.unwrap_or(0),
        ];
        let mut pools = Vec::new();
        for pool in state.as_array().into_iter().flatten() {
            let balance = pair(&pool["balance"])?;
            pools.push(json!({"balance":amounts(balance),"fees_paid":amounts(pair(&pool["fees_paid"])?),
            "fees_claimed":amounts(pair(&pool["fees_claimed"])?),"user_total":amounts(pair(&pool["user_total"])?),"price":balance[1] as f64/10_f64.powi(decimals[1])/(balance[0] as f64/10_f64.powi(decimals[0]))}));
        }
        let apy = |previous: [u128; 2], factor: u128| -> Value {
            json!([
                factor.wrapping_mul(sums["fees_paid"][0].wrapping_sub(previous[0])) as f64
                    / sums["user_total"][0] as f64,
                factor.wrapping_mul(sums["fees_paid"][1].wrapping_sub(previous[1])) as f64
                    / sums["user_total"][1] as f64
            ])
        };
        let fee_rates = get("fee_rates")
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| Ok(integer(v)? as f64 / 18446744073709551616_f64))
            .collect::<Result<Vec<_>>>()?;
        let value = json!({"__type":"mmx.swap_info_t","name":contract["name"],"address":mmx_vm::address::encode(&id),"tokens":tokens.map(|v|mmx_vm::address::encode(&v)),"decimals":decimals,"symbols":symbols,
            "wallet":amounts(wallet),"volume":volume.map(|v|v.to_string()),"balance":amounts(sums["balance"]),"fees_paid":amounts(sums["fees_paid"]),"fees_claimed":amounts(sums["fees_claimed"]),
            "user_total":amounts(sums["user_total"]),"volume_1d":amounts(volumes1),"volume_7d":amounts(volumes7),"avg_apy_1d":apy(paid1,365),"avg_apy_7d":apy(paid7,52),"fee_rates":fee_rates,"pools":pools,
            "price":sums["balance"][1] as f64/10_f64.powi(decimals[1])/(sums["balance"][0] as f64/10_f64.powi(decimals[0]))});
        Ok(Swap { id, tokens, value })
    }
    pub(super) fn market(&mut self, path: &str) -> Result<Option<Response>> {
        if !matches!(
            path,
            "/offers"
                | "/offer"
                | "/offer/trade_estimate"
                | "/trade_history"
                | "/swap/list"
                | "/swap/info"
                | "/swap/user_info"
                | "/swap/history"
                | "/swap/trade_estimate"
        ) {
            return Ok(None);
        }
        if matches!(
            path,
            "/offers" | "/swap/list" | "/swap/history" | "/trade_history"
        ) && self.query.get("limit").is_some_and(|v| v == "0")
        {
            return Ok(Some(Response::json(200, json!([]))?));
        }
        let mut view = View::capture(self)?;
        let value = match path {
            "/offer" => {
                let info = self.offer(self.addr("id")?, &mut view)?;
                self.render_offer(&info)?
            }
            "/offers" => {
                let limit = self.limit(100, 1000)? as usize;
                let bid = self
                    .query
                    .get("bid")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let ask = self
                    .query
                    .get("ask")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let minimum = self
                    .query
                    .get("min_bid")
                    .map(|v| v.parse::<u128>())
                    .transpose()?
                    .unwrap_or(0);
                let state = match self.query.get("state").map(String::as_str) {
                    None | Some("true" | "1") => true,
                    Some("false" | "0") => false,
                    _ => return Err(error("invalid state")),
                };
                let rows = view.rows(
                    index::CONTRACTS,
                    &self.context.params.offer_binary.0,
                    40,
                    10000,
                )?;
                let mut offers = Vec::new();
                for row in &rows {
                    let info = self.offer(row.value.as_slice().try_into()?, &mut view)?;
                    if (state && info.balance == 0)
                        || info.partner.is_some()
                        || (info.bid == info.ask && info.inverse < (1 << 64))
                        || info.balance < minimum
                        || bid.is_some_and(|v| v != info.bid)
                        || ask.is_some_and(|v| v != info.ask)
                    {
                        continue;
                    }
                    offers.push(info);
                    if offers.len() >= limit {
                        break;
                    }
                }
                if offers.len() < limit {
                    view.require_index()?;
                    if rows.len() == 10000 {
                        return Err(error("offer query exceeds row budget"));
                    }
                }
                offers.sort_by(|a, b| b.height.cmp(&a.height).then_with(|| b.id.cmp(&a.id)));
                if bid.is_some() && ask.is_some() {
                    offers.sort_by(|a, b| a.inverse.cmp(&b.inverse).reverse());
                }
                json!(offers
                    .iter()
                    .take(limit)
                    .map(|info| self.render_offer(info))
                    .collect::<Result<Vec<_>>>()?)
            }
            "/offer/trade_estimate" => {
                let info = self.offer(self.addr("id")?, &mut view)?;
                self.currency(info.bid)?;
                self.currency(info.ask)?;
                let mut out = json!({"inv_price":info.inverse.to_string()});
                if let (Some(bid), Some(ask)) = (
                    self.context.currencies.get(&info.bid),
                    self.context.currencies.get(&info.ask),
                ) {
                    let amount = parse_amount(self.required("amount")?, ask.decimals)?;
                    let trade = bid_amount(amount, info.inverse)?;
                    let next = ask_amount(
                        trade
                            .checked_add(1)
                            .ok_or_else(|| error("amount overflow"))?,
                        info.inverse,
                    )?;
                    let next = if amount == 0
                        || (trade + 1) as f64 / next as f64 > trade as f64 / amount as f64
                    {
                        next
                    } else {
                        amount
                            .checked_add(1)
                            .ok_or_else(|| error("amount overflow"))?
                    };
                    out["input"] = amount_str(amount, ask.decimals);
                    out["trade"] = amount_str(trade, bid.decimals);
                    out["next_input"] = amount_str(next, ask.decimals);
                }
                out
            }
            "/swap/info" => self.swap(self.addr("id")?, &mut view)?.value,
            "/swap/list" => {
                let limit = self.limit(50, 100)? as usize;
                let since = self.number("since", 0)?;
                let token = self
                    .query
                    .get("token")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let currency = self
                    .query
                    .get("currency")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let rows = view.rows(
                    index::CONTRACTS,
                    &self.context.params.swap_binary.0,
                    40,
                    10000,
                )?;
                let mut candidates = Vec::new();
                // C++ selects deployments in ascending creation order.
                for row in rows.iter().rev() {
                    let height = !u32::from_be_bytes(row.key[32..36].try_into()?);
                    if height < since {
                        continue;
                    }
                    let swap = self.swap(row.value.as_slice().try_into()?, &mut view)?;
                    if token.is_some_and(|v| v != swap.tokens[0])
                        || currency.is_some_and(|v| v != swap.tokens[1])
                    {
                        continue;
                    }
                    candidates.push(swap);
                    if candidates.len() >= limit {
                        break;
                    }
                }
                if candidates.len() < limit {
                    view.require_index()?;
                    if rows.len() == 10000 {
                        return Err(error("swap query exceeds row budget"));
                    }
                }
                candidates.sort_by(|a, b| {
                    if token.is_some() || currency.is_some() {
                        let i = usize::from(token.is_none());
                        b.value["balance"][i]["value"]
                            .as_f64()
                            .partial_cmp(&a.value["balance"][i]["value"].as_f64())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        let total = |swap: &Swap| {
                            swap.value["avg_apy_7d"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_f64)
                                .sum::<f64>()
                        };
                        total(b)
                            .partial_cmp(&total(a))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }
                });
                json!(candidates
                    .into_iter()
                    .take(limit)
                    .map(|v| v.value)
                    .collect::<Vec<_>>())
            }
            "/swap/user_info" => {
                let swap = self.swap(self.addr("id")?, &mut view)?;
                let user = self.addr("user")?;
                let data =
                    view.map_entry(swap.id, "users", &mmx_vm::Value::Binary(user.to_vec()))?;
                let balance = pair(&data["balance"])?;
                let (fees, liquidity) = if data.is_null() {
                    ([0; 2], [0; 2])
                } else {
                    (
                        pair(&view.call(
                            self,
                            swap.id,
                            "get_earned_fees",
                            &[json!(mmx_vm::address::encode(&user))],
                            None,
                            None,
                        )?)?,
                        pair(&view.call(
                            self,
                            swap.id,
                            "rem_all_liquid",
                            &[json!(true)],
                            Some(user),
                            None,
                        )?)?,
                    )
                };
                let amounts = |values: [u128; 2]| -> Value {
                    json!(values
                        .iter()
                        .enumerate()
                        .map(|(i, n)| amount(
                            *n,
                            self.context
                                .currencies
                                .get(&swap.tokens[i])
                                .map_or(0, |v| v.decimals)
                        ))
                        .collect::<Vec<_>>())
                };
                json!({"pool_idx":data["pool_idx"].as_i64().unwrap_or(-1),"tokens":swap.value["tokens"],"symbols":swap.value["symbols"],"balance":amounts(balance),"fees_earned":amounts(fees),
                    "equivalent_liquidity":amounts(liquidity),"unlock_height":data["unlock_height"].as_u64().unwrap_or(0),"swap":swap.value})
            }
            "/swap/trade_estimate" => {
                let swap = self.swap(self.addr("id")?, &mut view)?;
                let i = self.number("index", u32::MAX)? as usize;
                if i >= 2 {
                    return Err(error("invalid swap index"));
                }
                let iterations = self.number("iters", 20)?;
                if iterations > 1000 {
                    return Err(error("swap iterations exceed budget"));
                }
                let decimals_i = self
                    .context
                    .currencies
                    .get(&swap.tokens[i])
                    .ok_or_else(|| error("unknown token"))?
                    .decimals;
                let decimals_k = self
                    .context
                    .currencies
                    .get(&swap.tokens[1 - i])
                    .ok_or_else(|| error("unknown token"))?
                    .decimals;
                let input = parse_amount(self.required("amount")?, decimals_i)?;
                let result = pair(&view.call(
                    self,
                    swap.id,
                    "trade",
                    &[
                        json!(i),
                        json!(mmx_vm::address::encode(&swap.id)),
                        Value::Null,
                        json!(iterations),
                    ],
                    None,
                    Some((swap.tokens[i], input)),
                )?)?;
                let trade = result[0]
                    .checked_sub(result[1])
                    .ok_or_else(|| error("invalid swap estimate"))?;
                json!({"trade":amount_str(trade,decimals_k),"fee":amount_str(result[1],decimals_k),"fee_percent":100.*result[1] as f64/(trade as f64+result[1] as f64),
                    "avg_price":(trade as f64/10_f64.powi(decimals_k))/(input as f64/10_f64.powi(decimals_i))})
            }
            "/swap/history" => {
                let swap = self.swap(self.addr("id")?, &mut view)?;
                let limit = self.limit(100, 1000)? as usize;
                let rows = view.rows(index::EXECUTIONS, &swap.id, 40, limit)?;
                if rows.len() < limit {
                    view.require_index()?;
                }
                let mut out = Vec::new();
                for row in rows {
                    let entry = crate::rpc::to_json_record(
                        &mmx_consensus::Record::decode_compact(&row.value)?,
                    )?;
                    let method = entry["method"].as_str().unwrap_or("");
                    let args = entry["args"].as_array().cloned().unwrap_or_default();
                    let mut index = 255usize;
                    let mut count = 0;
                    let mut user = self
                        .context
                        .typed(&entry["user"], "optional<mmx.addr_t>", 0)?;
                    let kind = match method {
                        "trade" if !entry["deposit"].is_null() && args.len() >= 2 => {
                            index = args[0].as_u64().unwrap_or(255) as usize;
                            count = integer(&entry["deposit"][1])?;
                            user = json!(mmx_vm::address::encode(&addr(&args[1])?));
                            if index == 0 {
                                "SELL"
                            } else {
                                "BUY"
                            }
                        }
                        "add_liquid" | "rem_liquid" => {
                            if let Some(i) = args.first() {
                                index = i.as_u64().unwrap_or(255) as usize;
                            }
                            if !entry["deposit"].is_null() {
                                count = integer(&entry["deposit"][1])?;
                            } else if args.len() > 1 {
                                count = integer(&args[1])?;
                            }
                            if method == "add_liquid" {
                                "ADD"
                            } else {
                                "REMOVE"
                            }
                        }
                        "rem_all_liquid" => "REMOVE_ALL",
                        "payout" => "PAYOUT",
                        "switch_pool" => "SWITCH",
                        _ => "",
                    };
                    let info = swap
                        .tokens
                        .get(index)
                        .and_then(|id| self.context.currencies.get(id));
                    out.push(json!({"__type":"mmx.swap_entry_t","height":entry["height"],"time_stamp":entry["time_stamp"],"time":entry["time_stamp"].as_f64().unwrap_or(0.)/1000.,
                        "txid":render::hex(&render::bytes(&entry["txid"])?),"type":kind,"index":index as u8,"amount":count.to_string(),"user":user,
                        "value":info.map(|v|count as f64/10_f64.powi(v.decimals)),"symbol":info.map(|v|v.symbol.clone())}));
                }
                json!(out)
            }
            "/trade_history" => {
                let limit = self.limit(100, 1000)? as usize;
                let since = self.number("since", 0)?;
                let bid = self
                    .query
                    .get("bid")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let ask = self
                    .query
                    .get("ask")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let rows = view.rows(index::TRADES, &[], 8, 10000)?;
                let mut out = Vec::new();
                for row in &rows {
                    let height = !u32::from_be_bytes(row.key[..4].try_into()?);
                    if height < since {
                        break;
                    }
                    let (id, entry) = index::decode_trade(&row.value)?;
                    let entry = crate::rpc::to_json_record(&entry)?;
                    let offer = self.offer(id, &mut view)?;
                    if bid.is_some_and(|v| v != offer.bid) || ask.is_some_and(|v| v != offer.ask) {
                        continue;
                    }
                    let inverse = integer(&entry["args"][1])?;
                    let ask_amount = integer(&entry["deposit"][1])?;
                    let bid_amount = bid_amount(ask_amount, inverse)?;
                    self.currency(offer.bid)?;
                    self.currency(offer.ask)?;
                    let mut item = json!({"__type":"mmx.trade_entry_t","height":height,"time_stamp":entry["time_stamp"],"time":entry["time_stamp"].as_f64().unwrap_or(0.)/1000.,"address":mmx_vm::address::encode(&id),
                        "txid":render::hex(&render::bytes(&entry["txid"])?),"bid_currency":mmx_vm::address::encode(&offer.bid),"ask_currency":mmx_vm::address::encode(&offer.ask),
                        "bid_amount":bid_amount.to_string(),"ask_amount":ask_amount.to_string(),"price":18446744073709551616_f64/inverse as f64});
                    if let Some(info) = self.context.currencies.get(&offer.bid) {
                        item["bid_value"] = json!(bid_amount as f64 / 10_f64.powi(info.decimals));
                        item["bid_symbol"] = json!(info.symbol);
                    }
                    if let Some(info) = self.context.currencies.get(&offer.ask) {
                        item["ask_value"] = json!(ask_amount as f64 / 10_f64.powi(info.decimals));
                        item["ask_symbol"] = json!(info.symbol);
                    }
                    if let (Some(bid), Some(ask)) = (
                        self.context.currencies.get(&offer.bid),
                        self.context.currencies.get(&offer.ask),
                    ) {
                        item["display_price"] = json!(
                            18446744073709551616_f64 / inverse as f64
                                * 10_f64.powi(bid.decimals - ask.decimals)
                        );
                    }
                    out.push(item);
                    if out.len() >= limit {
                        break;
                    }
                }
                if out.len() < limit {
                    view.require_index()?;
                    if rows.len() == 10000 {
                        return Err(error("trade history exceeds row budget"));
                    }
                }
                json!(out)
            }
            _ => unreachable!(),
        };
        Ok(Some(Response::json(200, value)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webapi::{rpc, Context};
    use mmx_vm::{
        storage::{AtomicStorage, BalanceWrite, ContractData, StorageWrite, TransactionBatch},
        StorageDb, Value as Vm, Var,
    };
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };
    #[test]
    fn offers_read_real_binary_fields_and_keep_fixed_point_rounding() {
        let fixture: Value =
            vnx::serde_json::from_str(include_str!("../../tests/fixtures/webapi-mainnet.json"))
                .unwrap();
        let binary = &fixture["cases"][3]["native"]["deploy"];
        let id = [1; 32];
        let binary_id = [2; 32];
        let executable =
            rpc::from_json_record(&json!({"__type":"mmx.contract.Executable","binary":binary_id}))
                .unwrap();
        let mut db = StorageDb::memory();
        let mut batch = TransactionBatch::default();
        for (id, record) in [
            (id, executable),
            (binary_id, rpc::from_json_record(binary).unwrap()),
        ] {
            batch.contracts.push((
                id,
                ContractData {
                    bytes: record.encode().unwrap(),
                    read_cost: 0,
                },
            ));
        }
        for field in binary["fields"].as_array().unwrap() {
            let value = match field[0].as_str().unwrap() {
                "owner" => Vm::Binary([3; 32].to_vec()),
                "partner" => Vm::Nil,
                "bid_currency" | "ask_currency" => Vm::Binary([0; 32].to_vec()),
                "inv_price" => Vm::Uint((1u128 << 63).into()),
                "last_update" => Vm::from(42u64),
                _ => unreachable!(),
            };
            batch.writes.push((
                id,
                StorageWrite {
                    address: field[1].as_u64().unwrap(),
                    key: None,
                    var: Var::new(value),
                },
            ));
        }
        batch.balances.push(BalanceWrite {
            contract: id,
            currency: [0; 32],
            amount: 3_000_000,
        });
        db.apply_transaction(&batch).unwrap();
        db.database_mut().commit(1).unwrap();
        let mut view = View::fixture(db, 0);
        let (node, _, _) = crate::rpc::tests::fixture();
        let params = node.lock().unwrap().chain.selected().params().clone();
        let mut request = Request {
            node: &node,
            cache: None,
            end: Instant::now() + Duration::from_secs(2),
            query: BTreeMap::new(),
            context: Context::new(params, 0),
            watch: None,
            bytes: Cell::new(0),
        };
        let offer = request.offer(id, &mut view).unwrap();
        let rendered = request.render_offer(&offer).unwrap();
        assert_eq!(rendered["bid_balance"], "3000000");
        assert_eq!(rendered["ask_amount"], "6000000");
        assert_eq!(rendered["price"], 2.);
        assert_eq!(rendered["owner"], mmx_vm::address::encode(&[3; 32]));
        assert_eq!(ask_amount(1, 3u128 << 63).unwrap(), 1);
        assert_eq!(bid_amount(1, 1u128 << 63).unwrap(), 0);
        assert_eq!(parse_amount("1.2345678", 6).unwrap(), 1_234_567);
        assert_eq!(parse_amount("1e-6", 6).unwrap(), 1);
        assert!(!view.storage.database().has_pending());
    }
}

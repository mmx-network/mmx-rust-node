//! Existing public /wapi REST protocol, served in the node process. Caddy and
//! the existing Node.js load balancer can forward it without translating JSON.
mod advanced;
mod indexed;
mod market;
mod render;
mod storage;
#[cfg(test)]
mod tests;
use crate::{error, rpc, Node, Result};
use mmx_types::VnxType;
use render::{address, amount, uint, Context};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Instant,
};
use vnx::serde_json::{self, json, Value};

pub(crate) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
}
impl Response {
    fn json(status: u16, value: Value) -> Result<Self> {
        let body = serde_json::to_vec(&value)?;
        if body.len() > 32 * 1024 * 1024 {
            return Err(error("WebAPI response exceeds 32 MiB"));
        }
        Ok(Self {
            status,
            body,
            content_type: "application/json",
        })
    }
    fn text(status: u16, value: &str) -> Self {
        Self {
            status,
            body: value.as_bytes().to_vec(),
            content_type: "text/plain; charset=utf-8",
        }
    }
}
fn decode(text: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let a = input
                    .next()
                    .and_then(|v| (v as char).to_digit(16))
                    .ok_or_else(|| error("invalid query escape"))?;
                let b = input
                    .next()
                    .and_then(|v| (v as char).to_digit(16))
                    .ok_or_else(|| error("invalid query escape"))?;
                out.push((a * 16 + b) as u8);
            }
            _ => out.push(byte),
        }
    }
    Ok(String::from_utf8(out)?)
}
fn query(target: &str) -> Result<(&str, BTreeMap<String, String>)> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.strip_prefix("/wapi").unwrap_or(path);
    let mut out = BTreeMap::new();
    if !query.is_empty() {
        for (index, pair) in query.split('&').enumerate() {
            if index >= 128 {
                return Err(error("too many query parameters"));
            }
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            out.insert(decode(key)?, decode(value)?);
        }
    }
    Ok((path, out))
}
struct Request<'a> {
    node: &'a Arc<Mutex<Node>>,
    cache: Option<&'a rpc::cache::Shared>,
    end: Instant,
    query: BTreeMap<String, String>,
    context: Context,
    watch: Option<Watch>,
    bytes: Cell<usize>,
}
impl Request<'_> {
    fn call(&self, method: &str, args: Value) -> Result<Value> {
        if Instant::now() >= self.end {
            return Err(error("WebAPI request deadline"));
        }
        let value = rpc::web_query(self.node, method, args, self.end, self.cache)?;
        let total = self.bytes.get().saturating_add(json_charge(&value));
        if total > 32 * 1024 * 1024 {
            return Err(error("WebAPI aggregate read budget exceeded"));
        }
        self.bytes.set(total);
        Ok(value)
    }
    fn required(&self, key: &str) -> Result<&str> {
        self.query
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| error(format!("missing parameter: {key}")))
    }
    fn number(&self, key: &str, default: u32) -> Result<u32> {
        self.query
            .get(key)
            .map(|v| v.parse().map_err(Into::into))
            .unwrap_or(Ok(default))
    }
    fn limit(&self, default: u32, max: u32) -> Result<u32> {
        let value = self.number("limit", default)?;
        if value > max {
            return Err(error(format!("limit > {max}")));
        }
        Ok(value)
    }
    fn addr(&self, key: &str) -> Result<[u8; 32]> {
        Ok(mmx_vm::address::decode(self.required(key)?.as_bytes())?)
    }
    fn hash(&self, key: &str) -> Result<Vec<u8>> {
        let value = vnx::util::from_hex_string(self.required(key)?)?;
        if value.len() != 32 {
            return Err(error("invalid hash length"));
        }
        Ok(value)
    }
    fn currencies(&mut self, value: &Value) -> Result<()> {
        fn collect(value: &Value, ids: &mut BTreeSet<[u8; 32]>) -> Result<()> {
            match value {
                Value::Object(fields) => {
                    if fields.get("amount").is_some() && fields.get("contract").is_some() {
                        ids.insert(address(&value["contract"])?);
                    }
                    for value in fields.values() {
                        collect(value, ids)?;
                    }
                }
                Value::Array(rows) => {
                    for value in rows {
                        collect(value, ids)?;
                    }
                }
                _ => (),
            }
            Ok(())
        }
        let mut ids = BTreeSet::new();
        collect(value, &mut ids)?;
        if ids.len() > 1000 {
            return Err(error("too many currencies"));
        }
        for id in ids {
            self.currency(id)?;
        }
        Ok(())
    }
    fn currency(&mut self, id: [u8; 32]) -> Result<()> {
        if !self.context.currencies.contains_key(&id) {
            let contract = self.call("get_contract", json!({"address":id}))?;
            self.context.add_currency(id, &contract)?;
        }
        Ok(())
    }
    fn block(&mut self, header: bool) -> Result<Value> {
        let prefix = if header { "get_header" } else { "get_block" };
        let value = if self.query.contains_key("hash") {
            self.call(prefix, json!({"hash":self.hash("hash")?}))?
        } else {
            self.call(
                &format!("{prefix}_at"),
                json!({"height":self.number("height", u32::MAX)?}),
            )?
        };
        if !header {
            self.currencies(&value)?;
        }
        self.context.render(&value)
    }
    fn transaction(&mut self, id: Value) -> Result<Value> {
        let mut tx = self.call("get_transaction", json!({"id":id,"pending":false}))?;
        if tx.is_null() {
            tx = self.call("get_transaction", json!({"id":id,"pending":true}))?;
        }
        if tx.is_null() {
            return Ok(Value::Null);
        }
        let height = self.call("get_tx_height", json!({"id":id}))?;
        let block = if height.is_null() {
            Value::Null
        } else {
            self.call("get_header_at", json!({"height":height}))?
        };
        self.currencies(&tx)?;
        transaction_value(&self.context, &tx, &id, &height, &block)
    }
    fn handle(&mut self, path: &str, body: &[u8], synced_since: u32) -> Result<Response> {
        let value = match path {
            "/chain/info" => self.context.render(&self.call("get_params", json!({}))?)?,
            "/node/info" => {
                let header = self.call("get_header_at", json!({"height":self.context.height}))?;
                let p = &self.context.params;
                let difference = header["space_diff"].as_u64().unwrap_or(0);
                let ideal = u128::from(difference)
                    * u128::from(p.space_diff_constant)
                    * u128::from(p.avg_proof_count)
                    * 2;
                let space = ideal * 2464 / 1000 / 1_000_000_000;
                let speed = (u128::from(header["time_diff"].as_u64().unwrap_or(0))
                    * u128::from(p.time_diff_constant)
                    * 1000
                    / u128::from(p.time_diff_divider)
                    / p.block_interval_ms as u128) as u64;
                let average = mmx_execution::block::economics::min_reward_deduction(
                    header["txfee_buffer"].as_u64().unwrap_or(0),
                );
                let reward = if self.context.height >= p.reward_activation {
                    header["base_reward"].as_u64().unwrap_or(0)
                        + p.min_reward.saturating_sub(average)
                } else {
                    0
                };
                let root = rpc::lock_node_until(self.node, self.end)?.root_height();
                let mut size = 0.;
                let mut count = 0;
                for height in root.saturating_add(1)..=self.context.height {
                    let value = self.call("get_header_at", json!({"height":height}))?;
                    if !value["farmer_sig"].is_null() {
                        size +=
                            value["static_cost"].as_f64().unwrap_or(0.) / p.max_block_size as f64;
                        count += 1;
                    }
                }
                let mut view = storage::View::capture(self)?;
                let supply = view.circulating([0; 32])?;
                let address_count = view.address_count()?;
                json!({"__type":"mmx.NetworkInfo","name":p.network,"height":self.context.height,"vdf_height":header["vdf_height"],
                    "is_synced":true,"synced_since":synced_since,"time_stamp":header["time_stamp"],"time":header["time_stamp"].as_f64().unwrap_or(0.)/1000.,
                    "time_diff":header["time_diff"],"space_diff":header["space_diff"],"block_reward":amount(reward.into(),p.decimals),
                    "total_space":space as u64,"total_supply":if supply<(1<<53){json!(supply as u64)}else{json!(supply.to_string())},"address_count":address_count,"vdf_speed":speed as f64/1_000_000.,
                    "block_size":if count>0 {size / count as f64} else {0.},
                    "average_txfee":amount(average.into(),p.decimals),"genesis_hash":render::hex(&render::bytes(&self.call("get_genesis_hash",json!({}))?)?),
                    "node_version":env!("CARGO_PKG_VERSION"),"node_commit":option_env!("MMX_NODE_COMMIT").unwrap_or("rust")})
            }
            "/header" | "/block" => {
                if !self.query.contains_key("height") && !self.query.contains_key("hash") {
                    return Ok(Response::text(400, "block/header requires hash or height"));
                }
                self.block(path == "/header")?
            }
            "/headers" => {
                let mut limit = self.limit(20, 100)?;
                let offset = self.number("offset", 0)?;
                if limit == 0 {
                    if offset == 0 {
                        limit = 20;
                    } else {
                        return Err(error("headers?limit|offset"));
                    }
                }
                let height = if offset == 0 {
                    self.context.height
                } else {
                    offset
                };
                let mut out = Vec::new();
                self.context.hide_proof = true;
                for i in 0..limit.min(height) {
                    out.push(
                        self.context
                            .render(&self.call("get_header_at", json!({"height":height-i}))?)?,
                    );
                }
                json!(out)
            }
            "/contract" => {
                let value = self.call("get_contract", json!({"address":self.addr("id")?}))?;
                // C++ returns JSON null for a nonexistent contract.
                return Response::json(200, self.context.render(&value)?);
            }
            "/address" | "/balance" => {
                let id = self.addr("id")?;
                let limit = self.limit(100, 1000)?;
                let currency = self
                    .query
                    .get("currency")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let balances=self.call("get_total_balances",json!({"addresses":[id],"whitelist":currency.into_iter().collect::<Vec<_>>(),"limit":limit}))?;
                let pairs = balances
                    .as_array()
                    .ok_or_else(|| error("invalid balance result"))?;
                for pair in pairs {
                    self.currency(address(&pair[0])?)?;
                }
                let locked = path == "/balance" && self.locked(id)?;
                let mut rows = Vec::new();
                let mut nfts = Vec::new();
                for pair in pairs {
                    let token = address(&pair[0])?;
                    let count = uint(&pair[1])?;
                    if path == "/address" {
                        rows.push(self.context.output(token, count));
                    } else if let Some(info) = self.context.currencies.get(&token) {
                        if info.nft {
                            nfts.push(mmx_vm::address::encode(&token));
                        } else {
                            rows.push(json!({"contract":mmx_vm::address::encode(&token),"total":count as f64/10_f64.powi(info.decimals),
                            "spendable":if locked{0.}else{count as f64/10_f64.powi(info.decimals)},"reserved":0,"locked":if locked{count as f64/10_f64.powi(info.decimals)}else{0.},"symbol":info.symbol,"decimals":info.decimals,
                            "is_native":token==[0;32],"is_validated":currency.is_some()}));
                        }
                    }
                }
                if path == "/address" {
                    json!({"balances":rows,"contract":self.context.render(&self.call("get_contract",json!({"address":id}))?)?})
                } else if currency.is_some() {
                    if let Some(row) = rows.into_iter().next() {
                        row
                    } else if !nfts.is_empty() {
                        json!(true)
                    } else {
                        Value::Null
                    }
                } else {
                    json!({"balances":rows,"nfts":nfts})
                }
            }
            "/address/history" => {
                let id = self.addr("id")?;
                let limit = self.limit(100, 1000)?;
                let currency = self
                    .query
                    .get("currency")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let kind = if let Some(kind) = self.query.get("type") {
                    let registry = mmx_types::vnx_registry()?;
                    let schema = registry.by_name("mmx.tx_type_e")?;
                    Some(
                        schema
                            .enum_map
                            .iter()
                            .find(|(_, v)| *v == kind)
                            .map(|(k, _)| *k)
                            .or_else(|| kind.parse().ok())
                            .ok_or_else(|| error("invalid history type"))?,
                    )
                } else {
                    None
                };
                let mut args = json!({"addresses":[id],"filter":{"since":self.number("since",0)?,"until":self.number("until",u32::MAX)?,
                    "limit":limit,"max_search":10000,"currency":currency.into_iter().collect::<Vec<_>>(),"type":kind,"memo":self.query.get("memo"),"white_list":false,"with_pending":false}});
                let rows = self.call("get_history", args.clone())?;
                self.currencies(&rows)?;
                args["__type"] = json!("mmx.Node.get_history");
                let request = rpc::from_json_record(&args)?;
                if rpc::history::can_advance(&request)? && self.context.currencies.len() == 1 {
                    self.watch = Some(Watch {
                        request,
                        context: self.context.clone(),
                    });
                }
                self.context.typed(&rows, "vector<mmx.tx_entry_t>", 0)?
            }
            "/transaction" => self.transaction(json!(self.hash("id")?))?,
            "/transactions" => {
                let limit = self.limit(100, 1000)? as usize;
                let limit = if limit == 0 { 100 } else { limit };
                let offset = self.number("offset", 0)? as usize;
                let ids = if self.query.contains_key("height") {
                    self.call("get_tx_ids_at", json!({"height":self.number("height",0)?}))?
                } else {
                    let mut view = storage::View::capture(self)?;
                    let rows =
                        view.rows(mmx_execution::chain::public::TRANSACTIONS, &[], 8, limit)?;
                    if rows.len() < limit {
                        view.require_index()?;
                    }
                    json!(rows.into_iter().map(|row| row.value).collect::<Vec<_>>())
                };
                let mut out = Vec::new();
                for id in ids
                    .as_array()
                    .ok_or_else(|| error("invalid transaction ids"))?
                    .iter()
                    .skip(offset)
                    .take(limit)
                {
                    let row = self.transaction(id.clone())?;
                    if !row.is_null() {
                        out.push(row);
                    }
                }
                json!(out)
            }
            "/supply" => {
                let id = self.addr("id")?;
                self.currency(id)?;
                let total = storage::View::capture(self)?.circulating(id)?;
                let decimals = self
                    .context
                    .currencies
                    .get(&id)
                    .ok_or_else(|| error("invalid currency"))?
                    .decimals;
                amount(total, decimals)
            }
            "/transaction/validate" | "/transaction/broadcast" => {
                if body.is_empty() {
                    return Ok(Response::text(
                        400,
                        "POST transaction/validate|broadcast {...}",
                    ));
                }
                let mut tx: Value = serde_json::from_slice(body)?;
                tx["__type"] = json!("mmx.Transaction");
                let tx = render::native(&tx, "mmx.Transaction", 0)?;
                let result = self.call("validate", json!({"tx":tx}))?;
                if path.ends_with("broadcast") {
                    self.call("add_transaction", json!({"tx":tx,"pre_validate":false}))?;
                    return Ok(Response::text(200, ""));
                }
                self.context.render(&result)?
            }
            path if path.starts_with("/wallet/")
                || path.starts_with("/farm/")
                || path.starts_with("/config/")
                || path == "/exit"
                || path == "/node/exit"
                || path == "/node/log"
                || path == "/blocks"
                || path == "/passphrase/validate" =>
            {
                return Ok(Response::text(403, "permission denied"))
            }
            _ => {
                if let Some(response) = self.advanced(path, body)? {
                    return Ok(response);
                }
                if let Some(response) = self.indexed(path)? {
                    return Ok(response);
                }
                if let Some(response) = self.market(path)? {
                    return Ok(response);
                }
                return Ok(Response::text(404, "unknown WebAPI route"));
            }
        };
        if value.is_null() {
            Ok(Response::text(204, ""))
        } else {
            Response::json(200, value)
        }
    }
}

fn transaction_value(
    context: &Context,
    tx: &Value,
    id: &Value,
    height: &Value,
    block: &Value,
) -> Result<Value> {
    let result = &tx["exec_result"];
    let failed = result["did_fail"].as_bool().unwrap_or(false);
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for source in [tx, result] {
        inputs.extend(source["inputs"].as_array().into_iter().flatten().cloned());
        outputs.extend(source["outputs"].as_array().into_iter().flatten().cloned());
    }
    let mut in_amounts = BTreeMap::<[u8; 32], u128>::new();
    let mut out_amounts = BTreeMap::<[u8; 32], u128>::new();
    for (rows, totals) in [(&inputs, &mut in_amounts), (&outputs, &mut out_amounts)] {
        for row in rows {
            let entry = totals.entry(address(&row["contract"])?).or_default();
            *entry = entry
                .checked_add(uint(&row["amount"])?)
                .ok_or_else(|| error("amount overflow"))?;
        }
    }
    let operations = tx["execute"].as_array().cloned().unwrap_or_default();
    let mut out = json!({"__type":"mmx.tx_info_t","id":render::hex(&render::bytes(id)?),"expires":tx["expires"],
            "did_fail":failed,"height":height,"time_stamp":block["time_stamp"],"block":context.typed(&block["hash"],"mmx.hash_t",0)?,
            "message":if result.is_null(){Value::Null}else{json!(error_message(result)?)},"fee":amount(result["total_fee"].as_u64().unwrap_or(0).into(),context.params.decimals),
            "cost":amount(result["total_cost"].as_u64().or(tx["static_cost"].as_u64()).unwrap_or(0).into(),context.params.decimals),
            "note":context.typed(&tx["note"],"mmx.tx_note_e",0)?,"sender":context.typed(&tx["sender"],"optional<mmx.addr_t>",0)?,
            "inputs":inputs.iter().map(|v|context.typed(v,"mmx.txin_t",0)).collect::<Result<Vec<_>>>()?,"outputs":outputs.iter().map(|v|context.typed(v,"mmx.txout_t",0)).collect::<Result<Vec<_>>>()?,"operations":context.render(&json!(operations))?,
            "deployed":context.render(&tx["deploy"])?,
            "input_amounts":in_amounts.into_iter().map(|(id,n)| context.output(id,n)).collect::<Vec<_>>(),
            "output_amounts":out_amounts.into_iter().map(|(id,n)| context.output(id,n)).collect::<Vec<_>>()});
    if !height.is_null() {
        out["confirm"] = json!(
            context
                .height
                .saturating_sub(height.as_u64().unwrap_or(0) as u32)
                + 1
        );
    }
    if let Some(time) = block["time_stamp"].as_f64() {
        out["time"] = json!(time / 1000.);
    }
    if !tx["deploy"].is_null() {
        out["address"] = json!(mmx_vm::address::encode(&address(id)?));
    }
    Ok(out)
}

pub(crate) fn respond(
    node: &Arc<Mutex<Node>>,
    cache: Option<&rpc::cache::Shared>,
    method: &str,
    target: &str,
    body: &[u8],
    end: Instant,
) -> Response {
    let result = (|| -> Result<Response> {
        if method == "OPTIONS" {
            return Ok(Response::text(200, ""));
        }
        if !matches!(method, "GET" | "HEAD" | "POST") {
            return Ok(Response::text(405, "invalid method"));
        }
        let (path, query) = query(target)?;
        let cacheable = matches!(method, "GET" | "HEAD")
            && !path.starts_with("/transaction/")
            && path != "/contract/call";
        let key = cache_key(path, &query)?;
        let Some(cache) = cache else {
            return Ok(Response::text(503, "lost sync with network"));
        };
        for _ in 0..3 {
            if Instant::now() >= end {
                return Err(error("WebAPI request deadline"));
            }
            let probe = rpc::cache::web_probe(cache, cacheable.then_some(key.as_slice()));
            if !probe.ready {
                return Ok(Response::text(503, "lost sync with network"));
            }
            if let Some(body) = probe.value {
                return Ok(Response {
                    status: 200,
                    body,
                    content_type: "application/json",
                });
            }
            let response = (|| -> Result<(Response, Option<Watch>)> {
                let params = rpc::web_query(node, "get_params", json!({}), end, Some(cache))?;
                let params = mmx_types::mmx::ChainParams::decode(
                    &rpc::from_json_record(&params)?.encode()?,
                )?;
                let height = rpc::web_query(node, "get_height", json!({}), end, Some(cache))?
                    .as_u64()
                    .ok_or_else(|| error("invalid height"))? as u32;
                let mut request = Request {
                    node,
                    cache: Some(cache),
                    end,
                    query: query.clone(),
                    context: Context::new(params, height),
                    watch: None,
                    bytes: Cell::new(0),
                };
                let response = request.handle(path, body, probe.since)?;
                Ok((response, request.watch))
            })();
            // Never return a composite read assembled across a publication.
            // Submission uses its native admission snapshot and is not replayed.
            if path != "/transaction/broadcast" && !rpc::cache::web_current(cache, probe.epoch) {
                continue;
            }
            let (response, watch) = response?;
            if cacheable && response.status == 200 {
                if let Some(mut fill) = probe.fill {
                    let value: Value = serde_json::from_slice(&response.body)?;
                    let mut keep = true;
                    if path == "/transaction" && value["height"].is_null() {
                        keep = false;
                    }
                    if matches!(path, "/block" | "/header") {
                        if let Some(height) = value["height"].as_u64() {
                            let native = rpc::web_query(
                                node,
                                "get_block_hash",
                                json!({"height":height}),
                                end,
                                Some(cache),
                            )?;
                            let selected = if native.is_null() {
                                String::new()
                            } else {
                                render::hex(&render::bytes(&native)?)
                            };
                            if value["hash"].as_str() != Some(selected.as_str()) {
                                keep = false;
                            } else if path == "/header" || !has_token(&value) {
                                fill = fill.at_height(height.try_into()?);
                            }
                        }
                    }
                    if keep {
                        if let Some(watch) = watch {
                            fill.finish_web_history(&response.body, watch);
                        } else {
                            fill.finish_json(&response.body);
                        }
                    }
                }
            }
            return Ok(response);
        }
        Ok(Response::text(503, "chain changed during WebAPI read"))
    })();
    match result {
        Ok(response) => response,
        Err(error) => Response::text(400, &error.to_string()),
    }
}
fn has_token(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("amount") && fields.get("is_native") == Some(&Value::Bool(false))
            {
                return true;
            }
            fields.values().any(has_token)
        }
        Value::Array(values) => values.iter().any(has_token),
        _ => false,
    }
}

pub(super) fn cache_key(path: &str, query: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!(["WebAPI", path, query]))?)
}

/// Seed the public REST forms from accepted bodies without any ledger reads.
/// Token metadata remains demand-filled; a block with unknown currencies is
/// rendered on its first request using that request's bounded metadata reads.
pub(crate) struct Warm {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub height: Option<u32>,
}
pub(crate) fn pushed(
    block: &mmx_consensus::Record,
    params: &mmx_types::mmx::ChainParams,
    tip: u32,
) -> Result<Vec<Warm>> {
    let native = rpc::to_json_record(block)?;
    let height = crate::node::number(block, "height")?;
    let context = Context::new(params.clone(), tip);
    let mut header = native.clone();
    header["__type"] = json!("mmx.BlockHeader");
    header
        .as_object_mut()
        .ok_or_else(|| error("invalid header"))?
        .remove("tx_list");
    let mut out = Vec::new();
    let hash = render::hex(&render::bytes(&native["hash"])?);
    let put = |out: &mut Vec<Warm>, path: &str, value: &Value| -> Result<()> {
        let body = serde_json::to_vec(value)?;
        if body.len() > 32 * 1024 * 1024 {
            return Ok(());
        }
        for (name, value) in [("height", height.to_string()), ("hash", hash.clone())] {
            out.push(Warm {
                key: cache_key(path, &[(name.into(), value)].into())?,
                value: body.clone(),
                height: Some(height),
            });
        }
        Ok(())
    };
    put(&mut out, "/header", &context.render(&header)?)?;
    let native_only =
        native["tx_list"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|tx| {
                [tx, &tx["exec_result"]].into_iter().all(|source| {
                    ["inputs", "outputs"].into_iter().all(|field| {
                        source[field].as_array().into_iter().flatten().all(|row| {
                            render::address(&row["contract"]).is_ok_and(|v| v == [0; 32])
                        })
                    })
                })
            });
    if native_only {
        put(&mut out, "/block", &context.render(&native)?)?;
        for tx in native["tx_list"].as_array().into_iter().flatten() {
            let value = transaction_value(&context, tx, &tx["id"], &json!(height), &native)?;
            let value = serde_json::to_vec(&value)?;
            if value.len() <= 32 * 1024 * 1024 {
                out.push(Warm {
                    key: cache_key(
                        "/transaction",
                        &[("id".into(), render::hex(&render::bytes(&tx["id"])?))].into(),
                    )?,
                    value,
                    height: None,
                });
            }
        }
    }
    Ok(out)
}

fn error_message(result: &Value) -> Result<String> {
    if result["did_fail"] != true {
        return Ok(String::new());
    }
    let error = &result["error"];
    if error.is_null() {
        return Ok("tx failed".into());
    }
    let mut code = String::new();
    if let Some(number) = error["code"].as_u64().filter(|v| *v != 0) {
        let registry = mmx_types::vnx_registry()?;
        let schema = registry.by_name("mmx.error_code_e")?;
        code = if let Some(name) = schema.enum_map.get(&(number as u32)) {
            format!(" ({name})")
        } else {
            format!(" (code {number})")
        };
    }
    let mut message = error["message"].as_str().unwrap_or("").to_owned();
    message.push_str(&code);
    if let Some(operation) = error["operation"]
        .as_u64()
        .filter(|v| *v != u64::from(u32::MAX))
    {
        if let Some(address) = error["address"]
            .as_u64()
            .filter(|v| *v != u64::from(u32::MAX))
        {
            let mut location = format!("exception at 0x{address:X}");
            if let Some(line) = error["line"].as_u64() {
                location.push_str(&format!(", line {line}"));
            }
            message = format!("{location}: {message}");
        }
        message.push_str(&format!(" (op {operation})"));
    }
    Ok(message)
}

fn json_charge(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_add(match value {
        Value::String(v) => v.len(),
        Value::Array(v) => v.iter().map(json_charge).sum(),
        Value::Object(v) => v.iter().map(|(k, v)| 64 + k.len() + json_charge(v)).sum(),
        _ => 0,
    })
}
#[derive(Clone)]
pub(crate) struct Watch {
    request: mmx_consensus::Record,
    context: Context,
}
impl Watch {
    pub(crate) fn charge(&self) -> usize {
        16 * 1024
            + self.request.memory_size() * 2
            + self.context.params.network.len()
            + self.context.params.min_fee_ratio.len() * 4
            + self
                .context
                .currencies
                .values()
                .map(|v| 512 + v.symbol.len())
                .sum::<usize>()
    }
    pub(crate) fn owners(&self) -> Result<Vec<[u8; 32]>> {
        self.request
            .field("addresses")?
            .seq()?
            .iter()
            .map(|v| Ok(v.bytes()?.try_into()?))
            .collect()
    }
    pub(crate) fn advance(
        &self,
        value: &[u8],
        delta: &[mmx_consensus::Record],
    ) -> Result<Option<Vec<u8>>> {
        if !rpc::history::affected(&self.request, delta)? {
            return Ok(None);
        }
        let owners = self.owners()?;
        for row in delta {
            if owners.contains(&crate::node::hash(row, "address")?)
                && crate::node::hash(row, "contract")? != [0; 32]
            {
                return Err(error("new history currency requires metadata"));
            }
        }
        let public: Value = serde_json::from_slice(value)?;
        let native = render::native(&public, "vector<mmx.tx_entry_t>", 0)?;
        let response = rpc::from_json_record(
            &json!({"__type":"mmx.Node.get_history.return","_ret_0":native}),
        )?;
        let Some(response) = rpc::history::advance(&self.request, response, delta)? else {
            return Ok(None);
        };
        let native = rpc::response_json(&response)?;
        Ok(Some(serde_json::to_vec(&self.context.typed(
            &native,
            "vector<mmx.tx_entry_t>",
            0,
        )?)?))
    }
}

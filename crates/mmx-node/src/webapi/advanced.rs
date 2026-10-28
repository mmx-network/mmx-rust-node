use super::{render, storage::View, Request, Response};
use crate::{error, Result};
use mmx_vm::Value as Vm;
use vnx::serde_json::{self, json, Value};

impl Request<'_> {
    pub(super) fn locked(&self, id: [u8; 32]) -> Result<bool> {
        let mut view = View::capture(self)?;
        let Some((program, _)) = view.optional_program(id)? else {
            return Ok(false);
        };
        if !program.methods.contains_key(b"is_locked".as_slice()) {
            return Ok(false);
        }
        Ok(view
            .call(self, id, "is_locked", &[], None, None)?
            .as_bool()
            .unwrap_or(false))
    }
    pub(super) fn advanced(&mut self, path: &str, body: &[u8]) -> Result<Option<Response>> {
        let value = match path {
            "/contract/storage" => {
                let id = self.addr("id")?;
                json!(View::capture(self)?.data(id)?)
            }
            "/contract/storage/field" => {
                let id = self.addr("id")?;
                let name = self.required("name")?;
                let mut view = View::capture(self)?;
                let (var, offset) = view.field(id, name)?;
                json!({"address":offset,"value":view.resolve(id,var,0)?})
            }
            "/contract/storage/entry" => {
                let id = self.addr("id")?;
                let name = self.required("name")?;
                let mut view = View::capture(self)?;
                let (var, mut offset) = view.field(id, name)?;
                if let Some(Vm::Ref(a)) = var {
                    offset = a;
                }
                let key = if self.query.contains_key("addr") {
                    Vm::Binary(self.addr("addr")?.to_vec())
                } else {
                    Vm::String(self.required("key")?.as_bytes().to_vec())
                };
                use mmx_vm::Storage;
                let found = view.storage.lookup(&id, &key)?;
                let key = found.unwrap_or(0);
                let var = if found.is_some() {
                    view.storage.read(&id, offset, Some(key))?.map(|v| v.value)
                } else {
                    None
                };
                json!({"address":offset,"key":key,"value":view.resolve(id,var,0)?})
            }
            "/contract/call" => {
                let id = self.addr("id")?;
                let method = self.required("method")?;
                let body: Value = if body.is_empty() {
                    json!({})
                } else {
                    serde_json::from_slice(body)?
                };
                let args = body["args"].as_array().cloned().unwrap_or_default();
                let user = self
                    .query
                    .get("user")
                    .map(|v| mmx_vm::address::decode(v.as_bytes()))
                    .transpose()?;
                let deposit = if body["deposit"].is_null() {
                    None
                } else {
                    let pair = body["deposit"]
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or_else(|| error("invalid deposit"))?;
                    Some((
                        mmx_vm::address::decode(
                            pair[0]
                                .as_str()
                                .ok_or_else(|| error("invalid deposit currency"))?
                                .as_bytes(),
                        )?,
                        render::uint(&pair[1])?,
                    ))
                };
                let value = View::capture(self)?.call(self, id, method, &args, user, deposit)?;
                return Ok(Some(Response::json(200, value)?));
            }
            "/plotnft" => {
                let id = self.addr("id")?;
                if id == [0; 32] {
                    return Err(error("plotnft?id"));
                }
                let contract = self.call("get_contract", json!({"address":id}))?;
                if contract["__type"] != "mmx.contract.Executable"
                    || render::address(&contract["binary"])?
                        != self.context.params.plot_nft_binary.0
                {
                    return Ok(Some(Response::text(204, "")));
                }
                let data = View::capture(self)?.data(id)?;
                let field = |key: &str| data.get(key).cloned().unwrap_or(Value::Null);
                let unlock = field("unlock_height");
                let locked = unlock
                    .as_u64()
                    .is_none_or(|height| u64::from(self.context.height) < height);
                json!({"__type":"mmx.plot_nft_info_t","address":mmx_vm::address::encode(&id),"name":contract["name"],
                    "owner":field("owner")["address"],"is_locked":locked,"target":if locked{field("target")["address"].clone()}else{Value::Null},
                    "unlock_height":unlock,"server_url":field("server_url")})
            }
            "/node/graph/blocks" => {
                let limit = self.limit(1000, 1000)?;
                let step = self.number("step", 90)?;
                if step == 0 {
                    return Err(error("step must be positive"));
                }
                let count = limit.min(self.context.height.div_ceil(step));
                let mut out = Vec::new();
                let p = &self.context.params;
                for index in 0..count {
                    let header = self.call(
                        "get_header_at",
                        json!({"height":self.context.height-index*step}),
                    )?;
                    if header.is_null() {
                        out.push(json!({}));
                        continue;
                    }
                    let space = u128::from(header["space_diff"].as_u64().unwrap_or(0))
                        * u128::from(p.space_diff_constant)
                        * u128::from(p.avg_proof_count)
                        * 2
                        * 2464
                        / 1000
                        / 1_000_000_000;
                    let speed = u128::from(header["time_diff"].as_u64().unwrap_or(0))
                        * u128::from(p.time_diff_constant)
                        * 1000
                        / u128::from(p.time_diff_divider)
                        / p.block_interval_ms as u128;
                    out.push(json!({"height":header["height"],"tx_count":header["tx_count"],"netspace":space as f64/1_000_000.,"vdf_speed":speed as f64/1_000_000.,
                        "score":header["proof"].as_array().and_then(|p|p.first()).map(|p|p["score"].clone()),
                        "reward":render::uint(&header["reward_amount"])? as f64/10_f64.powi(p.decimals),
                        "tx_fees":render::uint(&header["tx_fees"])? as f64/10_f64.powi(p.decimals),
                        "base_reward":render::uint(&header["base_reward"])? as f64/10_f64.powi(p.decimals)}));
                }
                json!(out)
            }
            _ => return Ok(None),
        };
        Ok(Some(Response::json(200, value)?))
    }
}

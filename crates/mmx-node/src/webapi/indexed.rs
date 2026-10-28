use super::{render, storage::View, Request, Response};
use crate::{error, Result};
use mmx_consensus::Record;
use mmx_execution::chain::public as index;
use std::collections::BTreeMap;
use vnx::serde_json::{json, Value};

fn integer(value: u128) -> Value {
    if value < (1 << 53) {
        json!(value as u64)
    } else {
        json!(value.to_string())
    }
}
impl Request<'_> {
    pub(super) fn indexed(&mut self, path: &str) -> Result<Option<Response>> {
        let value = match path {
            "/farmers" => {
                let limit = self.limit(100, 1000)? as usize;
                let mut view = View::capture(self)?;
                view.require_index()?;
                let rows = view.rows(index::RANKING, &[], 41, limit)?;
                json!(rows.into_iter().map(|row|json!({"farmer_key":render::hex(&row.key[8..]),"block_count":u64::from_le_bytes(row.value.try_into().unwrap())})).collect::<Vec<_>>())
            }
            "/farmer" | "/farmer/blocks" => {
                let key = vnx::util::from_hex_string(self.required("id")?)?;
                if key.len() != 33 || key.iter().all(|v| *v == 0) {
                    return Err(error("invalid farmer key"));
                }
                let since = self.number("since", 0)?;
                let mut view = View::capture(self)?;
                view.require_index()?;
                if path == "/farmer/blocks" {
                    let limit = self.limit(50, 1000)? as usize;
                    let rows = view.rows(index::FARMER_BLOCKS, &key, 37, limit)?;
                    let mut out = Vec::new();
                    for row in rows {
                        let height = !u32::from_be_bytes(row.key[33..].try_into()?);
                        if height < since {
                            break;
                        }
                        let header = self.call("get_header_at", json!({"height":height}))?;
                        out.push(self.context.render(&header)?);
                    }
                    json!(out)
                } else {
                    let mut rewards = BTreeMap::<[u8; 32], u128>::new();
                    let (count, total) = if since == 0 {
                        let value = view.storage.database().get(index::FARMERS, &key, None)?;
                        let (count, total) = if let Some(value) = value {
                            if value.len() != 24 {
                                return Err(error("invalid farmer totals"));
                            }
                            (
                                u64::from_le_bytes(value[..8].try_into()?),
                                u128::from_le_bytes(value[8..].try_into()?),
                            )
                        } else {
                            (0, 0)
                        };
                        for row in view.rows(index::FARMER_REWARDS, &key, 65, 10000)? {
                            rewards.insert(
                                row.key[33..].try_into()?,
                                u128::from_le_bytes(
                                    row.value
                                        .try_into()
                                        .map_err(|_| error("invalid farmer reward"))?,
                                ),
                            );
                        }
                        (count, total)
                    } else {
                        let rows = view.rows(index::FARMER_BLOCKS, &key, 37, 10000)?;
                        if rows.len() == 10000 {
                            return Err(error("farmer summary exceeds row budget"));
                        }
                        let mut count = 0;
                        let mut total = 0;
                        for row in rows {
                            let height = !u32::from_be_bytes(row.key[33..].try_into()?);
                            if height < since {
                                break;
                            }
                            let header = self.call("get_header_at", json!({"height":height}))?;
                            let reward = render::uint(&header["reward_amount"])?;
                            let address = if header["reward_addr"].is_null() {
                                [0; 32]
                            } else {
                                render::address(&header["reward_addr"])?
                            };
                            *rewards.entry(address).or_default() += reward;
                            count += 1;
                            total += reward;
                        }
                        (count, total)
                    };
                    json!({"farmer_key":render::hex(&key),"block_count":count,"total_reward":integer(total),"total_reward_value":total as f64/10_f64.powi(self.context.params.decimals),
                        "rewards":rewards.into_iter().map(|(id,amount)|json!({"address":mmx_vm::address::encode(&id),"amount":integer(amount),"value":amount as f64/10_f64.powi(self.context.params.decimals)})).collect::<Vec<_>>()})
                }
            }
            "/contract/exec_history" => {
                let id = self.addr("id")?;
                let limit = self.limit(100, 1000)? as usize;
                let mut view = View::capture(self)?;
                let rows = view.rows(index::EXECUTIONS, &id, 40, limit)?;
                if rows.len() < limit {
                    view.require_index()?;
                }
                let mut out = Vec::new();
                for row in rows {
                    let entry = crate::rpc::to_json_record(&Record::decode_compact(&row.value)?)?;
                    if !entry["deposit"].is_null() {
                        self.currency(render::address(&entry["deposit"][0])?)?;
                    }
                    out.push(self.context.render(&entry)?);
                }
                json!(out)
            }
            _ => return Ok(None),
        };
        Ok(Some(Response::json(200, value)?))
    }
}

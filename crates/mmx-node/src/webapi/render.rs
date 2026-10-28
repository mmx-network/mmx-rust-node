//! Public JSON presentation from C++ WebAPI::Render. Native wire aliases are
//! resolved using the declarations, never guessed from a field's spelling.
use crate::{error, Result};
use mmx_types::mmx;
use std::{collections::BTreeMap, sync::LazyLock};
static REGISTRY: LazyLock<vnx::Registry> =
    LazyLock::new(|| mmx_types::vnx_registry().expect("compiled MMX schema"));
use vnx::serde_json::{json, Map, Value};

#[derive(Clone)]
pub(super) struct Currency {
    pub decimals: i32,
    pub symbol: String,
    pub nft: bool,
}
#[derive(Clone)]
pub(super) struct Context {
    pub params: mmx::ChainParams,
    pub height: u32,
    pub hide_proof: bool,
    pub currencies: BTreeMap<[u8; 32], Currency>,
}
pub(super) fn bytes(value: &Value) -> Result<Vec<u8>> {
    value
        .as_array()
        .ok_or_else(|| error("expected native bytes"))?
        .iter()
        .map(|v| {
            Ok(u8::try_from(
                v.as_u64().ok_or_else(|| error("invalid native byte"))?,
            )?)
        })
        .collect()
}
pub(super) fn address(value: &Value) -> Result<[u8; 32]> {
    bytes(value)?
        .try_into()
        .map_err(|_| error("invalid address length"))
}
pub(super) fn uint(value: &Value) -> Result<u128> {
    if let Some(n) = value.as_u64() {
        return Ok(n.into());
    }
    if let Some(n) = value.as_str() {
        return Ok(if let Some(hex) = n.strip_prefix("0x") {
            u128::from_str_radix(hex, 16)?
        } else {
            n.parse()?
        });
    }
    Ok(u128::from_le_bytes(
        bytes(value)?
            .try_into()
            .map_err(|_| error("invalid uint128"))?,
    ))
}
pub(super) fn hex(bytes: &[u8]) -> String {
    vnx::util::to_hex_string(bytes, false, false)
}
pub(super) fn amount(amount: u128, decimals: i32) -> Value {
    json!({"amount":amount.to_string(), "value":amount as f64 / 10_f64.powi(decimals)})
}
fn parameters(ty: &str) -> Option<(&str, Vec<&str>)> {
    let start = ty.find('<')?;
    let inner = ty.strip_suffix('>')?.get(start + 1..)?;
    let mut depth = 0;
    let mut previous = 0;
    let mut args = Vec::new();
    for (pos, byte) in inner.bytes().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                args.push(&inner[previous..pos]);
                previous = pos + 1;
            }
            _ => (),
        }
    }
    args.push(&inner[previous..]);
    Some((&ty[..start], args))
}
impl Context {
    pub fn new(params: mmx::ChainParams, height: u32) -> Self {
        let mut currencies = BTreeMap::new();
        currencies.insert(
            [0; 32],
            Currency {
                decimals: params.decimals,
                symbol: "MMX".into(),
                nft: false,
            },
        );
        Self {
            params,
            height,
            currencies,
            hide_proof: false,
        }
    }
    pub fn add_currency(&mut self, id: [u8; 32], contract: &Value) -> Result<()> {
        let name = contract["__type"].as_str().unwrap_or("");
        let mut parent = Some(name);
        let mut token = false;
        while let Some(name) = parent {
            if name == "mmx.contract.TokenBase" {
                token = true;
                break;
            }
            parent = mmx_types::presentation_parent(name);
        }
        if token {
            self.currencies.insert(
                id,
                Currency {
                    decimals: contract["decimals"].as_i64().unwrap_or(0).try_into()?,
                    symbol: contract["symbol"].as_str().unwrap_or("").into(),
                    nft: false,
                },
            );
        }
        if name == "mmx.contract.Executable"
            && address(&contract["binary"])? == self.params.nft_binary.0
        {
            self.currencies.insert(
                id,
                Currency {
                    decimals: 0,
                    symbol: "NFT".into(),
                    nft: true,
                },
            );
        }
        Ok(())
    }
    pub fn output(&self, id: [u8; 32], amount: u128) -> Value {
        let mut row = json!({"contract":mmx_vm::address::encode(&id),"amount":amount.to_string()});
        self.augment(&mut row, id, amount);
        row
    }
    pub fn augment(&self, row: &mut Value, id: [u8; 32], amount: u128) {
        row["is_native"] = json!(id == [0; 32]);
        if let Some(currency) = self.currencies.get(&id) {
            if currency.nft {
                row["is_nft"] = json!(true);
            } else {
                row["symbol"] = json!(currency.symbol);
                row["decimals"] = json!(currency.decimals);
                row["value"] = json!(amount as f64 / 10_f64.powi(currency.decimals));
            }
        }
    }
    pub fn render(&self, value: &Value) -> Result<Value> {
        self.typed(value, "", 0)
    }
    pub fn typed(&self, value: &Value, ty: &str, depth: usize) -> Result<Value> {
        if depth > 64 {
            return Err(error("public JSON recursion budget"));
        }
        if value.is_null() {
            return Ok(Value::Null);
        }
        let short = ty.rsplit('.').next().unwrap_or(ty);
        match short {
            "addr_t" => return Ok(json!(mmx_vm::address::encode(&address(value)?))),
            "hash_t" | "pubkey_t" | "signature_t" => return Ok(json!(hex(&bytes(value)?))),
            "uint128" | "uint128_t" => return Ok(json!(uint(value)?.to_string())),
            _ => (),
        }
        if let Some((container, args)) = parameters(ty) {
            match container {
                "optional" => return self.typed(value, args[0], depth + 1),
                "vector" | "list" | "set" | "array" => {
                    if matches!(args[0], "uchar" | "uint8") {
                        return Ok(json!(hex(&bytes(value)?)));
                    }
                    return Ok(Value::Array(
                        value
                            .as_array()
                            .ok_or_else(|| error("expected sequence"))?
                            .iter()
                            .map(|v| self.typed(v, args[0], depth + 1))
                            .collect::<Result<_>>()?,
                    ));
                }
                "pair" | "tuple" | "map" => {
                    if container == "map" {
                        return Ok(Value::Array(
                            value
                                .as_array()
                                .ok_or_else(|| error("expected map"))?
                                .iter()
                                .map(|pair| {
                                    Ok(json!([
                                        self.typed(&pair[0], args[0], depth + 1)?,
                                        self.typed(&pair[1], args[1], depth + 1)?
                                    ]))
                                })
                                .collect::<Result<_>>()?,
                        ));
                    }
                    return Ok(Value::Array(
                        value
                            .as_array()
                            .ok_or_else(|| error("expected tuple"))?
                            .iter()
                            .zip(args)
                            .map(|(v, t)| self.typed(v, t, depth + 1))
                            .collect::<Result<_>>()?,
                    ));
                }
                _ => (),
            }
        }
        if !ty.is_empty() {
            let registry = &*REGISTRY;
            if let Ok(schema) = registry.by_name(ty) {
                if schema.is_enum {
                    if let Some(name) = value.as_str() {
                        if schema.enum_map.values().any(|v| v == name) {
                            return Ok(value.clone());
                        }
                    }
                    let number = value
                        .as_u64()
                        .or_else(|| value["value"].as_u64())
                        .ok_or_else(|| error("invalid enum"))?;
                    return Ok(match schema.enum_map.get(&(number as u32)) {
                        Some(name) => json!(name),
                        None => json!(number),
                    });
                }
            }
        }
        match value {
            Value::Number(n) if n.as_u64().is_some_and(|v| v >= (1 << 53)) => {
                Ok(json!(n.to_string()))
            }
            Value::Array(items) => Ok(Value::Array(
                items
                    .iter()
                    .map(|v| self.typed(v, "", depth + 1))
                    .collect::<Result<_>>()?,
            )),
            Value::Object(fields) => {
                let name = fields.get("__type").and_then(Value::as_str).unwrap_or(ty);
                let mut out = Map::new();
                if name.starts_with("mmx.") && REGISTRY.by_name(name).is_ok() {
                    out.insert("__type".into(), json!(name));
                }
                for (field, item) in fields {
                    if self.hide_proof && field == "proof_xs" {
                        continue;
                    }
                    let field_type = mmx_types::presentation_field(name, field).unwrap_or("");
                    out.insert(field.clone(), self.typed(item, field_type, depth + 1)?);
                }
                let mut out = Value::Object(out);
                if matches!(
                    name,
                    "mmx.txio_t"
                        | "mmx.txin_t"
                        | "mmx.txout_t"
                        | "mmx.tx_entry_t"
                        | "mmx.txio_entry_t"
                ) {
                    self.augment(
                        &mut out,
                        address(&value["contract"])?,
                        uint(&value["amount"])?,
                    );
                }
                if matches!(
                    name,
                    "mmx.tx_entry_t" | "mmx.txio_entry_t" | "mmx.exec_entry_t" | "mmx.swap_entry_t"
                ) {
                    out["time"] = json!(value["time_stamp"].as_f64().unwrap_or(0.) / 1000.);
                }
                if name == "mmx.exec_entry_t" && !value["deposit"].is_null() {
                    let id = address(&value["deposit"][0])?;
                    let mut deposit = json!({"currency":mmx_vm::address::encode(&id)});
                    if let Some(currency) = self.currencies.get(&id) {
                        deposit["amount"] = amount(uint(&value["deposit"][1])?, currency.decimals);
                    }
                    out["deposit"] = deposit;
                }
                if name == "mmx.exec_result_t" {
                    out["total_fee_value"] = json!(
                        uint(&value["total_fee"])? as f64 / 10_f64.powi(self.params.decimals)
                    );
                }
                if name == "mmx.Block" || name == "mmx.BlockHeader" {
                    self.header(value, &mut out)?;
                }
                Ok(out)
            }
            _ => Ok(value.clone()),
        }
    }
    fn header(&self, native: &Value, out: &mut Value) -> Result<()> {
        for field in [
            "tx_fees",
            "total_cost",
            "static_cost",
            "reward_amount",
            "base_reward",
            "txfee_buffer",
        ] {
            out[field] = amount(uint(&native[field])?, self.params.decimals);
        }
        let fees = native["tx_fees"]
            .as_u64()
            .ok_or_else(|| error("invalid header fees"))?;
        let buffer = native["txfee_buffer"]
            .as_u64()
            .ok_or_else(|| error("invalid fee buffer"))?;
        out["vdf_reward"] = amount(
            if native["vdf_reward_payout"].is_null() {
                0
            } else {
                self.params.vdf_reward.into()
            },
            self.params.decimals,
        );
        out["project_reward"] = amount(
            mmx_execution::block::economics::project_reward(&self.params, fees)?.into(),
            self.params.decimals,
        );
        out["average_txfee"] = amount(
            mmx_execution::block::economics::min_reward_deduction(buffer).into(),
            self.params.decimals,
        );
        out["static_cost_ratio"] =
            json!(uint(&native["static_cost"])? as f64 / self.params.max_block_size as f64);
        out["total_cost_ratio"] =
            json!(uint(&native["total_cost"])? as f64 / self.params.max_block_cost as f64);
        out["time"] = json!(native["time_stamp"].as_f64().unwrap_or(0.) / 1000.);
        if let Some(proof) = out["proof"].as_array().and_then(|p| p.first()).cloned() {
            out["score"] = proof["score"].clone();
            out["ksize"] = proof["ksize"].clone();
            out["farmer_key"] = proof["farmer_key"].clone();
        }
        Ok(())
    }
}

/// Convert the existing public transaction JSON to the native wire schema.
/// Only declared aliases are converted; strings inside VM arguments stay text.
pub(super) fn native(value: &Value, ty: &str, depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err(error("transaction JSON recursion budget"));
    }
    if value.is_null() {
        return Ok(Value::Null);
    }
    let short = ty.rsplit('.').next().unwrap_or(ty);
    if let Some(text) = value.as_str() {
        match short {
            "addr_t" => return Ok(json!(mmx_vm::address::decode(text.as_bytes())?.to_vec())),
            "hash_t" | "pubkey_t" | "signature_t" => {
                return Ok(json!(vnx::util::from_hex_string(text)?))
            }
            "uint128" | "uint128_t" => {
                return Ok(json!(text.parse::<u128>()?.to_le_bytes().to_vec()))
            }
            "ulong" | "uint64" => return Ok(json!(text.parse::<u64>()?)),
            "long" | "int64" => return Ok(json!(text.parse::<i64>()?)),
            _ => (),
        }
        let registry = &*REGISTRY;
        if let Ok(schema) = registry.by_name(ty) {
            if schema.is_enum {
                return Ok(json!(schema
                    .enum_map
                    .iter()
                    .find(|(_, v)| v.as_str() == text)
                    .map(|(n, _)| *n)
                    .ok_or_else(|| error(format!("invalid {ty}: {text}")))?));
            }
        }
    }
    if let Some((container, args)) = parameters(ty) {
        match container {
            "optional" => return native(value, args[0], depth + 1),
            "vector" | "list" | "set" | "array" => {
                if matches!(args[0], "uchar" | "uint8") {
                    if let Some(text) = value.as_str() {
                        return Ok(json!(vnx::util::from_hex_string(text)?));
                    }
                }
                return Ok(Value::Array(
                    value
                        .as_array()
                        .ok_or_else(|| error("invalid sequence"))?
                        .iter()
                        .map(|v| native(v, args[0], depth + 1))
                        .collect::<Result<_>>()?,
                ));
            }
            "map" => {
                return Ok(Value::Array(
                    value
                        .as_array()
                        .ok_or_else(|| error("invalid map"))?
                        .iter()
                        .map(|v| {
                            Ok(json!([
                                native(&v[0], args[0], depth + 1)?,
                                native(&v[1], args[1], depth + 1)?
                            ]))
                        })
                        .collect::<Result<_>>()?,
                ))
            }
            "pair" | "tuple" => {
                return Ok(Value::Array(
                    value
                        .as_array()
                        .ok_or_else(|| error("invalid tuple"))?
                        .iter()
                        .zip(args)
                        .map(|(v, t)| native(v, t, depth + 1))
                        .collect::<Result<_>>()?,
                ))
            }
            _ => (),
        }
    }
    match value {
        Value::Object(fields) => {
            let name = fields.get("__type").and_then(Value::as_str).unwrap_or(ty);
            let mut out = Map::new();
            for (field, item) in fields {
                out.insert(
                    field.clone(),
                    native(
                        item,
                        mmx_types::presentation_field(name, field).unwrap_or(""),
                        depth + 1,
                    )?,
                );
            }
            Ok(Value::Object(out))
        }
        _ => Ok(value.clone()),
    }
}

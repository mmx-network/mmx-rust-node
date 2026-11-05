//! Schema-directed public transaction JSON. Variant arguments stay JSON: never
//! infer an address or integer from its spelling or a property inside an argument.
use crate::{Address, Error, Result};
use mmx_consensus::Record;
use serde_json::{json, Value};
use std::io::Cursor;

fn bad() -> Error {
    Error::InvalidArgument("invalid public transaction JSON".into())
}
fn bytes(value: &Value) -> Result<Vec<u8>> {
    value
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|v| u8::try_from(v.as_u64().ok_or_else(bad)?).map_err(|_| bad()))
        .collect()
}
fn parameters(ty: &str) -> Option<(&str, Vec<&str>)> {
    let start = ty.find('<')?;
    let inner = ty.strip_suffix('>')?.get(start + 1..)?;
    let (mut depth, mut previous) = (0_i32, 0);
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
fn convert(value: &Value, ty: &str, public: bool, depth: usize) -> Result<Value> {
    if depth > 48 {
        return Err(bad());
    }
    if value.is_null() {
        return Ok(Value::Null);
    }
    if matches!(ty, "vnx.Variant" | "vnx.Object") {
        return Ok(value.clone());
    }
    match ty.rsplit('.').next().unwrap_or(ty) {
        "addr_t" => {
            return if public {
                Ok(json!(
                    Address(bytes(value)?.try_into().map_err(|_| bad())?).to_string()
                ))
            } else if let Some(text) = value.as_str() {
                let a: Address = text.parse()?;
                if a.to_string() != text {
                    return Err(bad());
                }
                Ok(json!(a.0.to_vec()))
            } else {
                Ok(value.clone())
            }
        }
        "hash_t" | "pubkey_t" | "signature_t" => {
            return if public {
                Ok(json!(hex::encode_upper(bytes(value)?)))
            } else if let Some(text) = value.as_str() {
                Ok(json!(hex::decode(text).map_err(|_| bad())?))
            } else {
                Ok(value.clone())
            }
        }
        "uint128" | "uint128_t" => {
            return if public {
                let b = bytes(value)?.try_into().map_err(|_| bad())?;
                Ok(json!(u128::from_le_bytes(b).to_string()))
            } else if let Some(text) = value.as_str() {
                Ok(json!(text
                    .parse::<u128>()
                    .map_err(|_| bad())?
                    .to_le_bytes()
                    .to_vec()))
            } else {
                Ok(value.clone())
            }
        }
        "ulong" | "uint64" => {
            return if public {
                Ok(json!(value.as_u64().ok_or_else(bad)?.to_string()))
            } else if let Some(text) = value.as_str() {
                Ok(json!(text.parse::<u64>().map_err(|_| bad())?))
            } else {
                Ok(value.clone())
            }
        }
        "long" | "int64" => {
            return if public {
                Ok(json!(value.as_i64().ok_or_else(bad)?.to_string()))
            } else if let Some(text) = value.as_str() {
                Ok(json!(text.parse::<i64>().map_err(|_| bad())?))
            } else {
                Ok(value.clone())
            }
        }
        _ => (),
    }
    if let Some((container, args)) = parameters(ty) {
        match container {
            "optional" => return convert(value, args[0], public, depth + 1),
            "vector" | "list" | "set" | "array" => {
                if matches!(args[0], "uchar" | "uint8") {
                    if public {
                        return Ok(json!(hex::encode_upper(bytes(value)?)));
                    }
                    if let Some(text) = value.as_str() {
                        return Ok(json!(hex::decode(text).map_err(|_| bad())?));
                    }
                }
                return Ok(Value::Array(
                    value
                        .as_array()
                        .ok_or_else(bad)?
                        .iter()
                        .map(|v| convert(v, args[0], public, depth + 1))
                        .collect::<Result<_>>()?,
                ));
            }
            "map" => {
                return Ok(Value::Array(
                    value
                        .as_array()
                        .ok_or_else(bad)?
                        .iter()
                        .map(|pair| {
                            if pair.as_array().is_none_or(|p| p.len() != 2) {
                                return Err(bad());
                            }
                            Ok(json!([
                                convert(&pair[0], args[0], public, depth + 1)?,
                                convert(&pair[1], args[1], public, depth + 1)?
                            ]))
                        })
                        .collect::<Result<_>>()?,
                ))
            }
            "pair" | "tuple" => {
                let values = value.as_array().ok_or_else(bad)?;
                if values.len() != args.len() {
                    return Err(bad());
                }
                return Ok(Value::Array(
                    values
                        .iter()
                        .zip(args)
                        .map(|(v, t)| convert(v, t, public, depth + 1))
                        .collect::<Result<_>>()?,
                ));
            }
            _ => (),
        }
    }
    if let Some(fields) = value.as_object() {
        let name = fields.get("__type").and_then(Value::as_str).unwrap_or(ty);
        let mut out = serde_json::Map::new();
        for (field, item) in fields {
            if field == "__type" {
                out.insert(field.clone(), item.clone());
                continue;
            }
            let field_type = mmx_types::presentation_field(name, field).ok_or_else(bad)?;
            out.insert(field.clone(), convert(item, field_type, public, depth + 1)?);
        }
        return Ok(Value::Object(out));
    }
    Ok(value.clone())
}
pub fn decode(value: &Value, ty: &str) -> Result<Record> {
    let native = convert(value, ty, false, 0)?;
    let mut writer = vnx::binary::Writer::new(
        Vec::new(),
        mmx_types::vnx_registry().map_err(mmx_consensus::Error::from)?,
    );
    writer.write(&native).map_err(mmx_consensus::Error::from)?;
    Ok(Record::decode(&writer.into_inner())?)
}
pub fn encode(record: &Record) -> Result<Value> {
    let native = vnx::binary::Reader::new(
        Cursor::new(record.encode()?),
        mmx_types::vnx_registry().map_err(mmx_consensus::Error::from)?,
    )
    .read()
    .map_err(mmx_consensus::Error::from)?;
    convert(&native, &record.name, true, 0)
}

//! Native text conversions needed by ChainParams and the genesis distribution.
//! Keep external MMX types here: VNX byte-array schemas do not distinguish an
//! address from a hash, or a uint128 from ordinary bytes. Unsupported typed
//! payloads fail explicitly instead of passing through a lossy JSON conversion.
use super::invalid;
use mmx_consensus::{Record, Value as V};
use mmx_types::{mmx, VnxType};
use mmx_vm::U256;
use std::{collections::BTreeMap, sync::LazyLock};
use vnx::{code::Code, serde_json::Value as J, Registry, TypeCode};

pub(super) fn params(value: J) -> vnx::Result<mmx::ChainParams> {
    let r = decoder().record("mmx.ChainParams", &value)?;
    mmx::ChainParams::decode(&r.encode().map_err(|e| invalid(e.to_string()))?)
}
pub(super) fn rewards(value: J) -> vnx::Result<Record> {
    if value.get("__type").and_then(J::as_str) != Some("mmx.Transaction") {
        return Err(invalid("expected mmx.Transaction rewards"));
    }
    decoder().record("mmx.Transaction", &value)
}
struct Decoder {
    registry: Registry,
    defaults: BTreeMap<String, Record>,
}
fn decoder() -> &'static Decoder {
    static DECODER: LazyLock<Decoder> = LazyLock::new(|| Decoder {
        registry: mmx_types::vnx_registry().expect("checked-in schema"),
        defaults: [
            Record::from_generated(&mmx::ChainParams::default()),
            Record::from_generated(&mmx::Transaction::default()),
            Record::from_generated(&mmx::txin_t::default()),
            Record::from_generated(&mmx::txout_t::default()),
            Record::from_generated(&mmx::exec_result_t::default()),
            Record::from_generated(&mmx::exec_error_t::default()),
            Record::from_generated(&mmx::uint_fraction_t::default()),
        ]
        .into_iter()
        .map(|r| {
            let r = r.expect("checked-in bootstrap defaults");
            (r.name.clone(), r)
        })
        .collect(),
    });
    &DECODER
}
impl Decoder {
    fn record(&self, name: &str, value: &J) -> vnx::Result<Record> {
        let mut record = self
            .defaults
            .get(name)
            .cloned()
            .ok_or_else(|| invalid(format!("unsupported bootstrap text type {name}")))?;
        let fields = value
            .as_object()
            .ok_or_else(|| invalid(format!("expected {name} object")))?;
        let schema = self.registry.by_name(name)?;
        for field in &schema.fields {
            let Some(value) = fields.get(&field.name) else {
                continue;
            };
            let code = Code::from_words(&field.code)?;
            let converted = match (name, field.name.as_str()) {
                ("mmx.ChainParams", f) if f.ends_with("_binary") => address(value)?,
                ("mmx.Transaction", "sender") => V::Optional(if value.is_null() {
                    None
                } else {
                    Some(Box::new(address(value)?))
                }),
                ("mmx.Transaction", "id" | "content_hash") => V::Bytes(hash(value)?),
                ("mmx.txin_t" | "mmx.txout_t", "address" | "contract") => address(value)?,
                ("mmx.txin_t" | "mmx.txout_t", "amount") => V::Bytes(amount(value)?),
                _ => self.value(schema, &code, value)?,
            };
            record.fields.insert(field.name.clone(), converted);
        }
        Ok(record)
    }
    fn value(&self, owner: &TypeCode, code: &Code, value: &J) -> vnx::Result<V> {
        Ok(match code {
            Code::U8 => V::Uint(number(value)? as u8 as u64),
            Code::U16 => V::Uint(number(value)? as u16 as u64),
            Code::U32 => V::Uint(number(value)? as u32 as u64),
            Code::U64 => V::Uint(number(value)?),
            Code::I32 => V::Int(number(value)? as i32 as i64),
            Code::I64 => V::Int(number(value)? as i64),
            Code::Bool => V::Bool(number(value)? != 0),
            Code::String => V::String(match value {
                J::String(s) => s.as_bytes().to_vec(),
                _ => vnx::serde_json::to_string(value)?.into_bytes(),
            }),
            Code::Optional(c) => V::Optional(if value.is_null() {
                None
            } else {
                Some(Box::new(self.value(owner, c, value)?))
            }),
            Code::List(c) => V::Sequence(match value {
                J::Null => vec![],
                J::Array(values) => values
                    .iter()
                    .map(|v| self.value(owner, c, v))
                    .collect::<vnx::Result<_>>()?,
                v => vec![self.value(owner, c, v)?],
            }),
            Code::Struct(index) => {
                let schema = self.registry.get(owner.depends[*index])?;
                if schema.is_enum {
                    V::Uint(if let Some(text) = value.as_str() {
                        enum_text(schema, text)? as u64
                    } else {
                        number(value)? as u32 as u64
                    })
                } else {
                    V::Record(self.record(&schema.name, value)?)
                }
            }
            Code::Any if value.is_null() => V::Null,
            _ => {
                return Err(invalid(format!(
                    "unsupported bootstrap text payload in {}",
                    owner.name
                )))
            }
        })
    }
}
fn enum_text(schema: &TypeCode, text: &str) -> vnx::Result<u32> {
    if let Some((id, _)) = schema.enum_map.iter().find(|(_, name)| *name == text) {
        return Ok(*id);
    }
    let parsed = vnx::config::parse_json(text)?;
    let decimal = text
        .trim_start_matches(['+', '-'])
        .bytes()
        .all(|b| b.is_ascii_digit())
        && !text.is_empty();
    if parsed.is_string() && !decimal {
        return Ok(vnx::hash64(text) as u32);
    }
    // Native enum text uses std::stoul(base=0), independently of JSON's
    // numeric parser, and ignores trailing characters after the number.
    let text = text.trim_start();
    let negative = text.starts_with('-');
    let digits = text.trim_start_matches(['+', '-']);
    let (digits, base) = if digits.starts_with("0x") || digits.starts_with("0X") {
        (&digits[2..], 16)
    } else if digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    let mut n = 0u64;
    let mut consumed = false;
    for c in digits.chars() {
        let Some(digit) = c.to_digit(base) else { break };
        n = n
            .checked_mul(base as u64)
            .and_then(|n| n.checked_add(digit as u64))
            .ok_or_else(|| invalid("bootstrap enum integer overflow"))?;
        consumed = true;
    }
    if !consumed {
        return Err(invalid("invalid bootstrap enum integer"));
    }
    Ok(if negative { n.wrapping_neg() } else { n } as u32)
}
fn number(value: &J) -> vnx::Result<u64> {
    Ok(match value {
        J::Number(n) => {
            if let Some(v) = n.as_u64() {
                v
            } else if let Some(v) = n.as_i64() {
                v as u64
            } else {
                return Err(invalid("floating-point bootstrap integers are unsupported"));
            }
        }
        J::Bool(v) => u64::from(*v),
        // Native numeric deserialization does not parse text into integers.
        _ => 0,
    })
}
fn array<const N: usize>(value: &J) -> vnx::Result<Vec<u8>> {
    let mut bytes = vec![0; N];
    match value {
        J::Array(values) => {
            for (byte, value) in bytes.iter_mut().zip(values) {
                *byte = number(value)? as u8;
            }
        }
        _ => bytes[0] = number(value)? as u8,
    }
    Ok(bytes)
}
fn address(value: &J) -> vnx::Result<V> {
    Ok(V::Bytes(if let Some(s) = value.as_str() {
        // Native read(addr_t) catches address syntax errors and assigns zero.
        mmx_vm::address::decode(s.as_bytes())
            .unwrap_or_default()
            .to_vec()
    } else {
        array::<32>(value)?
    }))
}
fn hash(value: &J) -> vnx::Result<Vec<u8>> {
    Ok(if let Some(s) = value.as_str() {
        vnx::util::from_hex_string(s)
            .ok()
            .filter(|v| v.len() == 32)
            .unwrap_or_else(|| vec![0; 32])
    } else if let Some(v) = value.as_u64() {
        let mut bytes = vec![0; 32];
        bytes[..8].copy_from_slice(&v.to_le_bytes());
        bytes
    } else {
        array::<32>(value)?
    })
}
fn amount(value: &J) -> vnx::Result<Vec<u8>> {
    let n = if let Some(text) = value.as_str() {
        let (text, base) = text.strip_prefix("0x").map_or((text, 10), |s| (s, 16));
        let mut n = U256::zero();
        // uint256_t::init_from_base accepts all alphanumeric digits (even >=
        // base), counts other bytes as zero, stops at NUL, and wraps at 256 bits.
        for b in text.bytes().take_while(|b| *b != 0) {
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'z' => b - b'a' + 10,
                b'A'..=b'Z' => b - b'A' + 10,
                _ => 0,
            };
            n = n
                .overflowing_mul(U256::from(base))
                .0
                .overflowing_add(U256::from(digit))
                .0;
        }
        if n > U256::from(u128::MAX) {
            0
        } else {
            n.low_u128()
        }
    } else if let Some(n) = value.as_u64() {
        n as u128
    } else {
        return array::<16>(value);
    };
    Ok(n.to_le_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_text_matches_native_conversions() {
        for line in include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/network.jsonl"
        ))
        .lines()
        {
            let row: J = vnx::serde_json::from_str(line).unwrap();
            let kind = row["kind"].as_str().unwrap();
            if kind == "config" {
                continue;
            }
            let value = vnx::config::parse_json(row["input"].as_str().unwrap()).unwrap();
            let actual = if kind == "params" {
                params(value).map(|v| Record::from_generated(&v).unwrap())
            } else {
                rewards(value)
            };
            assert_eq!(
                actual.is_ok(),
                row["result"] == "OK",
                "{}: {actual:?}",
                row["name"]
            );
            if let Ok(actual) = actual {
                let bytes = vnx::util::from_hex_string(row["wire"].as_str().unwrap()).unwrap();
                assert_eq!(actual, Record::decode(&bytes).unwrap(), "{}", row["name"]);
            }
        }
    }
}

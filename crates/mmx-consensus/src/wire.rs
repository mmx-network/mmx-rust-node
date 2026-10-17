use crate::{input, Error, Record, Result, Value as V, Variant};
use std::{collections::BTreeMap, io::Cursor};
use vnx::{binary::Reader, code::Code, Registry, TypeCode};

struct Parser<'a> {
    input: &'a [u8],
    registry: Registry,
    trusted_fallback: bool,
}
impl<'a> Parser<'a> {
    fn schema(&self, hash: u64) -> vnx::Result<&TypeCode> {
        self.registry.get(hash).or_else(|e| {
            if self.trusted_fallback {
                crate::hash::registry().get(hash)
            } else {
                Err(e)
            }
        })
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.input.len() {
            return Err(input("truncated payload"));
        }
        let (head, rest) = self.input.split_at(n);
        self.input = rest;
        Ok(head)
    }
    fn number(&mut self, n: usize, big: bool) -> Result<u64> {
        let raw = self.take(n)?;
        let mut bytes = [0; 8];
        if big {
            bytes[8 - n..].copy_from_slice(raw);
            Ok(u64::from_be_bytes(bytes))
        } else {
            bytes[..n].copy_from_slice(raw);
            Ok(u64::from_le_bytes(bytes))
        }
    }
    fn count(&mut self, big: bool) -> Result<usize> {
        let n = self.number(4, big)? as usize;
        if n > 10_000_000 {
            return Err(input("container length"));
        }
        Ok(n)
    }
    fn any(&mut self, depth: usize) -> Result<V> {
        loop {
            let before = self.input;
            let tag = self.number(2, false)? as u16;
            match tag {
                0 => return Ok(V::Null),
                0xffff | 0x3713 | 0x1337 => (),
                14 | 0x0e00 => {
                    // The runtime owns schema parsing and validation.
                    let mut reader = Reader::new(Cursor::new(before), self.registry.clone());
                    reader.read_type_code()?;
                    self.registry = reader.registry().clone();
                    let n = reader.into_inner().position() as usize;
                    self.take(n - 2)?;
                }
                15 | 0x0f00 => {
                    let big = tag == 0x0f00;
                    let hash = self.number(8, big)?;
                    let schema = self.schema(hash)?.clone();
                    return self.record(&schema, depth + 1);
                }
                _ => return Err(input("expected native record")),
            }
        }
    }
    fn record(&mut self, schema: &TypeCode, depth: usize) -> Result<V> {
        if depth > 512 {
            return Err(input("wire recursion limit"));
        }
        let trusted = crate::hash::registry();
        let expected = trusted.by_name(&schema.name)?;
        if schema.type_hash != expected.type_hash
            || schema.code_hash != expected.code_hash
            || schema.fields.len() != expected.fields.len()
            || !schema.fields.iter().zip(&expected.fields).all(|(a, b)| {
                a.name == b.name
                    && a.code == b.code
                    && a.is_extended == b.is_extended
                    && a.data_size == b.data_size
            })
            || schema.depends != expected.depends
            || schema.is_enum != expected.is_enum
            || schema.is_class != expected.is_class
        {
            return Err(Error::Unsupported(format!(
                "schema version of {}",
                schema.name
            )));
        }
        let mut fields = BTreeMap::new();
        for ext in [false, true] {
            for field in schema.fields.iter().filter(|f| f.is_extended == ext) {
                let before = self.input.len();
                let code = Code::from_words(&field.code)?;
                let value = self.value(&code, Some(schema), depth + 1)?;
                let used = before - self.input.len();
                if !ext && field.data_size > 0 {
                    if used > field.data_size as usize {
                        return Err(input("inline field size"));
                    }
                    self.take(field.data_size as usize - used)?;
                }
                fields.insert(field.name.clone(), value);
            }
        }
        if schema.is_enum {
            return fields.remove("value").ok_or_else(|| input("enum value"));
        }
        // Native pointer deserialization applies dynamic_pointer_cast. A value
        // from another class family becomes nullptr, including deployments.
        match schema.name.as_str() {
            "mmx.Transaction" => {
                for (name, family) in [("execute", "Operation"), ("solutions", "Solution")] {
                    if let Some(V::Sequence(values)) = fields.get_mut(name) {
                        for value in values {
                            cast(value, family);
                        }
                    }
                }
                if let Some(value) = fields.get_mut("deploy") {
                    cast(value, "Contract");
                }
            }
            "mmx.solution.MultiSig" => {
                if let Some(V::Map(values)) = fields.get_mut("solutions") {
                    for value in values.values_mut() {
                        cast(value, "Solution");
                    }
                }
            }
            _ => (),
        }
        Ok(V::Record(Record {
            name: schema.name.clone(),
            fields,
        }))
    }
    fn value(&mut self, code: &Code, owner: Option<&TypeCode>, depth: usize) -> Result<V> {
        if depth > 512 {
            return Err(input("wire recursion limit"));
        }
        // Alternate byte order belongs to each code node, not its children.
        let (code, big) = match code {
            Code::Alt(c) => (c.as_ref(), true),
            _ => (code, false),
        };
        Ok(match code {
            Code::Null => V::Null,
            Code::Bool => V::Bool(self.number(1, big)? != 0),
            Code::U8 => V::Uint(self.number(1, big)?),
            Code::U16 => V::Uint(self.number(2, big)?),
            Code::U32 => V::Uint(self.number(4, big)?),
            Code::U64 => V::Uint(self.number(8, big)?),
            Code::I8 => V::Int(self.number(1, big)? as i8 as i64),
            Code::I16 => V::Int(self.number(2, big)? as i16 as i64),
            Code::I32 => V::Int(self.number(4, big)? as i32 as i64),
            Code::I64 => V::Int(self.number(8, big)? as i64),
            Code::F32 => V::Float32(self.number(4, big)? as u32),
            Code::F64 => V::Float64(self.number(8, big)?),
            Code::String => {
                let n = self.count(big)?;
                V::String(self.take(n)?.to_vec())
            }
            Code::Optional(code) => V::Optional(if self.number(1, big)? == 0 {
                None
            } else {
                Some(Box::new(self.value(code, owner, depth + 1)?))
            }),
            Code::Any => self.any(depth + 1)?,
            Code::Dynamic => {
                let before = self.input;
                // VNX byte-code header lengths self-identify their byte order,
                // independently of the enclosing CODE_DYNAMIC tag.
                let raw_size = self.number(2, false)? as u16;
                let size = if raw_size > 255 {
                    raw_size.swap_bytes()
                } else {
                    raw_size
                } as usize;
                if size == 0 || size > 32 {
                    return Err(input("dynamic code size"));
                }
                let words = (0..size)
                    .map(|_| self.number(2, false).map(|v| v as u16))
                    .collect::<Result<Vec<_>>>()?;
                let code = Code::from_words(&words)?;
                let mut value = self.value(&code, None, depth + 1)?;
                let json_depth = json_depth(&code, &value).map(|n| n + 1);
                let copy_depth = copy_depth(&code, &value) + 1;
                let raw = before[..before.len() - self.input.len()].to_vec();
                // Tuple is not an array in Variant's native classification.
                if matches!(base(&code), Code::Tuple(_)) {
                    value = V::Opaque(raw.clone());
                }
                V::Variant(Variant {
                    value: Box::new(value),
                    raw,
                    json_depth,
                    copy_depth,
                })
            }
            Code::List(code) => {
                let n = self.count(big)?;
                self.sequence(n, code, owner, depth + 1, Some(big))?
            }
            Code::Array(n, code) => self.sequence(*n, code, owner, depth + 1, None)?,
            Code::Matrix(dims, code) => {
                let n = dims
                    .iter()
                    .try_fold(1usize, |n, d| n.checked_mul(*d))
                    .ok_or_else(|| input("matrix size"))?;
                self.sequence(n, code, owner, depth + 1, None)?
            }
            Code::Tuple(codes) => V::Sequence(
                codes
                    .iter()
                    .map(|c| self.value(c, owner, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            Code::Map(key, value) => {
                let mut out = BTreeMap::new();
                for _ in 0..self.count(big)? {
                    let key = self.value(key, owner, depth + 1)?;
                    let value = self.value(value, owner, depth + 1)?;
                    // Native CODE_MAP reads into map[key], replacing an
                    // earlier value for a repeated key.
                    out.insert(key, value);
                }
                V::Map(out)
            }
            Code::Object => {
                let mut out = Vec::new();
                for _ in 0..self.count(big)? {
                    let n = self.count(big)?;
                    let key = self.take(n)?.to_vec();
                    let dynamic = if big {
                        Code::Alt(Box::new(Code::Dynamic))
                    } else {
                        Code::Dynamic
                    };
                    let value = self.value(&dynamic, owner, depth + 1)?;
                    out.push((key, value));
                }
                V::Object(out)
            }
            Code::Struct(index) => {
                let hash = owner
                    .and_then(|o| o.depends.get(*index))
                    .ok_or_else(|| input("struct dependency"))?;
                let schema = self.schema(*hash)?.clone();
                self.record(&schema, depth + 1)?
            }
            _ => return Err(Error::Unsupported(format!("VNX code {code:?}"))),
        })
    }
    fn sequence(
        &mut self,
        n: usize,
        code: &Code,
        owner: Option<&TypeCode>,
        depth: usize,
        list_order: Option<bool>,
    ) -> Result<V> {
        if n > 10_000_000 {
            return Err(input("container length"));
        }
        if matches!(code, Code::U8 | Code::Alt(_)) && matches!(base(code), Code::U8) {
            return Ok(V::Bytes(self.take(n)?.to_vec()));
        }
        if matches!((list_order, code), (Some(false), Code::I8))
            || matches!((list_order,code), (Some(true), Code::Alt(c)) if matches!(c.as_ref(), Code::I8))
        {
            return Ok(V::String(self.take(n)?.to_vec()));
        }
        Ok(V::Sequence(
            (0..n)
                .map(|_| self.value(code, owner, depth + 1))
                .collect::<Result<_>>()?,
        ))
    }
}
fn cast(value: &mut V, family: &str) {
    let valid = match value {
        V::Record(r) => {
            let code = crate::hash::registry()
                .by_name(&r.name)
                .expect("validated schema");
            let base = crate::hash::registry()
                .by_name(&format!("mmx.{family}"))
                .expect("MMX family");
            derives_from(code, base.type_hash)
        }
        V::Null => true,
        _ => false,
    };
    if !valid {
        *value = V::Null;
    }
}
fn derives_from(code: &TypeCode, base: u64) -> bool {
    code.type_hash == base
        || code.parents.iter().any(|hash| {
            crate::hash::registry()
                .get(*hash)
                .is_ok_and(|parent| derives_from(parent, base))
        })
}
fn base(code: &Code) -> &Code {
    if let Code::Alt(c) = code {
        c
    } else {
        code
    }
}
fn copy_depth(code: &Code, value: &V) -> usize {
    match (base(code), value) {
        (Code::Dynamic, V::Variant(v)) => v.copy_depth,
        (Code::List(c) | Code::Array(_, c) | Code::Matrix(_, c), V::Sequence(v)) => {
            1 + v.iter().map(|v| copy_depth(c, v)).max().unwrap_or(0)
        }
        (Code::Object, V::Object(v)) => {
            1 + v
                .iter()
                .map(|(_, v)| copy_depth(&Code::Dynamic, v))
                .max()
                .unwrap_or(1)
        }
        (Code::Optional(c), V::Optional(Some(v))) => 1 + copy_depth(c, v),
        _ => 1,
    }
}
fn json_depth(code: &Code, value: &V) -> Option<usize> {
    Some(match base(code) {
        Code::Null
        | Code::Bool
        | Code::U8
        | Code::U16
        | Code::U32
        | Code::U64
        | Code::I8
        | Code::I16
        | Code::I32
        | Code::I64
        | Code::String => 1,
        Code::Dynamic => {
            if let V::Variant(v) = value {
                v.json_depth?
            } else {
                return None;
            }
        }
        Code::List(c) if matches!(base(c), Code::Dynamic) => {
            let V::Sequence(values) = value else {
                return None;
            };
            1 + values
                .iter()
                .map(|v| json_depth(c, v))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .max()
                .unwrap_or(0)
        }
        Code::Object => {
            let V::Object(values) = value else {
                return None;
            };
            1 + values
                .iter()
                .map(|(_, v)| json_depth(&Code::Dynamic, v))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .max()
                .unwrap_or(0)
        }
        _ => return None,
    })
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Record> {
    decode_with_registry(bytes, &mut Registry::new())
}
pub(crate) fn decode_with_registry(bytes: &[u8], registry: &mut Registry) -> Result<Record> {
    let mut parser = Parser {
        input: bytes,
        registry: registry.clone(),
        trusted_fallback: false,
    };
    let record = parse_record(&mut parser)?;
    *registry = parser.registry;
    Ok(record)
}
pub(crate) fn compact(bytes: &[u8]) -> Result<Record> {
    parse_record(&mut Parser {
        input: bytes,
        registry: Registry::new(),
        trusted_fallback: true,
    })
}
fn parse_record(parser: &mut Parser<'_>) -> Result<Record> {
    let value = parser.any(0)?;
    if !parser.input.is_empty() {
        return Err(input("trailing bytes"));
    }
    match value {
        V::Record(r) => Ok(r),
        _ => Err(input("expected record")),
    }
}
pub(crate) fn variant(bytes: &[u8]) -> Result<Variant> {
    let mut parser = Parser {
        input: bytes,
        registry: Registry::new(),
        trusted_fallback: false,
    };
    let value = parser.value(&Code::Dynamic, None, 0)?;
    if !parser.input.is_empty() {
        return Err(input("trailing bytes"));
    }
    match value {
        V::Variant(v) => Ok(v),
        _ => unreachable!(),
    }
}

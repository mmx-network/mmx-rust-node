//! VM visitor over VNX payloads. VNX handles schemas, codes and scalar decoding;
//! this adapter retains dynamic binary/string/container distinctions for the VM.
use mmx_vm::{Error, InputValue as I, Result, U256};
use std::{collections::BTreeMap, io::Cursor};
use vnx::{
    binary::{ByteOrder, Reader},
    code::Code,
    Registry, TypeCode,
};

pub(crate) fn error(error: impl std::fmt::Display) -> Error {
    Error::Contract(error.to_string())
}
fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    if count > input.len() {
        return Err(Error::Input);
    }
    let (head, rest) = input.split_at(count);
    *input = rest;
    Ok(head)
}
fn number(input: &mut &[u8], count: usize, big: bool) -> Result<u64> {
    let data = take(input, count)?;
    let mut bytes = [0; 8];
    if big {
        bytes[8 - count..].copy_from_slice(data);
        Ok(u64::from_be_bytes(bytes))
    } else {
        bytes[..count].copy_from_slice(data);
        Ok(u64::from_le_bytes(bytes))
    }
}
fn skip(
    input: &mut &[u8],
    code: &Code,
    owner: Option<&TypeCode>,
    registry: &Registry,
    big: bool,
) -> Result<usize> {
    let mut cursor = Cursor::new(*input);
    Reader::new(&mut cursor, registry.clone())
        .with_utf8_lossy(true)
        .with_byte_order(if big {
            ByteOrder::Big
        } else {
            ByteOrder::Little
        })
        .read_payload(code, owner)
        .map_err(error)?;
    let size = cursor.position() as usize;
    take(input, size)?;
    Ok(size)
}
pub(crate) fn header<'a>(
    mut input: &'a [u8],
    registry: &Registry,
) -> Result<(&'a [u8], TypeCode, bool)> {
    let mut registry = registry.clone();
    loop {
        let raw = u16::from_le_bytes(input.get(..2).ok_or(Error::Input)?.try_into().unwrap());
        match raw {
            0xffff | 0x3713 | 0x1337 => {
                take(&mut input, 2)?;
            }
            14 | 0x0e00 => {
                let mut cursor = Cursor::new(input);
                let mut reader = Reader::new(&mut cursor, registry.clone());
                reader.read_type_code().map_err(error)?;
                registry = reader.registry().clone();
                drop(reader);
                take(&mut input, cursor.position() as usize)?;
            }
            15 | 0x0f00 => {
                take(&mut input, 2)?;
                let big = raw == 0x0f00;
                let hash = number(&mut input, 8, big)?;
                return Ok((input, registry.get(hash).map_err(error)?.clone(), big));
            }
            _ => return Err(Error::Input),
        }
    }
}
pub(crate) fn fields<'a>(
    input: &mut &'a [u8],
    owner: &TypeCode,
    registry: &Registry,
    big: bool,
) -> Result<BTreeMap<String, &'a [u8]>> {
    let mut out = BTreeMap::new();
    for extended in [false, true] {
        for field in &owner.fields {
            if field.is_extended != extended {
                continue;
            }
            let before = *input;
            let code = Code::from_words(&field.code).map_err(error)?;
            let size = skip(input, &code, Some(owner), registry, big)?;
            if !extended && field.data_size > size as u64 {
                take(input, field.data_size as usize - size)?;
            }
            out.insert(field.name.clone(), &before[..size]);
        }
    }
    Ok(out)
}
pub(crate) fn input_value(
    input: &mut &[u8],
    code: &Code,
    owner: Option<&TypeCode>,
    registry: &Registry,
    big: bool,
    depth: usize,
) -> Result<I> {
    if depth > 100 {
        return Err(Error::CopyRecursion);
    }
    if let Code::Alt(code) = code {
        return input_value(input, code, owner, registry, true, depth);
    }
    Ok(match code {
        Code::Null => I::Nil,
        Code::Bool => I::Bool(number(input, 1, big)? != 0),
        Code::U8
        | Code::U16
        | Code::U32
        | Code::U64
        | Code::I8
        | Code::I16
        | Code::I32
        | Code::I64 => {
            let (size, signed) = match code {
                Code::U8 => (1, false),
                Code::U16 => (2, false),
                Code::U32 => (4, false),
                Code::U64 => (8, false),
                Code::I8 => (1, true),
                Code::I16 => (2, true),
                Code::I32 => (4, true),
                _ => (8, true),
            };
            let value = number(input, size, big)?;
            if signed && (value >> (size * 8 - 1)) != 0 {
                return Ok(I::Unsupported);
            }
            I::Uint(U256::from(value))
        }
        Code::F32 | Code::F64 => {
            skip(input, code, owner, registry, big)?;
            I::Unsupported
        }
        Code::String => {
            let size = number(input, 4, big)? as usize;
            I::String(take(input, size)?.to_vec())
        }
        Code::Dynamic => {
            let raw = number(input, 2, false)? as u16;
            let size = if raw > 255 { raw.swap_bytes() } else { raw } as usize;
            if size == 0 || size > 32 {
                return Err(Error::Input);
            }
            let words: Vec<_> = (0..size)
                .map(|_| number(input, 2, false).map(|n| n as u16))
                .collect::<Result<_>>()?;
            input_value(
                input,
                &Code::from_words(&words).map_err(error)?,
                None,
                registry,
                false,
                depth + 1,
            )?
        }
        Code::Array(size, code) => {
            sequence(input, *size, code, owner, registry, big, depth, false)?
        }
        Code::List(code) => {
            let size = number(input, 4, big)? as usize;
            sequence(input, size, code, owner, registry, big, depth, true)?
        }
        Code::Map(key, value) => {
            let size = number(input, 4, big)?;
            let mut out = Vec::new();
            for _ in 0..size {
                let key = input_value(input, key, owner, registry, big, depth + 1)?;
                let value = input_value(input, value, owner, registry, big, depth + 1)?;
                out.push((key, value));
            }
            I::Map(out)
        }
        Code::Object => {
            let size = number(input, 4, big)?;
            let mut out = BTreeMap::new();
            for _ in 0..size {
                let len = number(input, 4, big)? as usize;
                let key = take(input, len)?.to_vec();
                out.insert(
                    key,
                    input_value(input, &Code::Dynamic, None, registry, big, depth + 1)?,
                );
            }
            I::Map(out.into_iter().map(|(k, v)| (I::String(k), v)).collect())
        }
        Code::Optional(code) => {
            if number(input, 1, big)? == 0 {
                I::Nil
            } else {
                input_value(input, code, owner, registry, big, depth + 1)?
            }
        }
        Code::Tuple(codes) => I::Array(
            codes
                .iter()
                .map(|c| input_value(input, c, owner, registry, big, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Code::Struct(index) => {
            let owner = owner.ok_or(Error::Input)?;
            let owner = registry
                .get(*owner.depends.get(*index).ok_or(Error::Input)?)
                .map_err(error)?;
            let value = record(input, owner, registry, big, depth + 1)?;
            // Contract::get_field constructs a native Variant. VNX writes
            // embedded structs as sorted Objects with a __type field, whereas
            // an existing CODE_ANY value retains its typed field visitation.
            if let I::Map(mut values) = value {
                values.push((
                    I::String(b"__type".to_vec()),
                    I::String(owner.name.as_bytes().to_vec()),
                ));
                values.sort_by(|(a, _), (b, _)| {
                    let (I::String(a), I::String(b)) = (a, b) else {
                        unreachable!()
                    };
                    a.cmp(b)
                });
                I::Map(values)
            } else {
                value
            }
        }
        Code::Any => {
            if input.starts_with(&[0, 0]) {
                take(input, 2)?;
                I::Nil
            } else {
                let (payload, owner, big) = header(input, registry)?;
                *input = payload;
                record(input, &owner, registry, big, depth + 1)?
            }
        }
        _ => return Err(Error::Input),
    })
}
#[allow(clippy::too_many_arguments)]
fn sequence(
    input: &mut &[u8],
    size: usize,
    code: &Code,
    owner: Option<&TypeCode>,
    registry: &Registry,
    big: bool,
    depth: usize,
    list: bool,
) -> Result<I> {
    let element = if let Code::Alt(code) = code {
        code.as_ref()
    } else {
        code
    };
    if matches!(element, Code::U8) {
        return Ok(I::Binary(take(input, size)?.to_vec()));
    }
    if list && matches!(element, Code::I8) {
        return Ok(I::String(take(input, size)?.to_vec()));
    }
    if size > 1_000_000 {
        return Err(Error::Input);
    }
    let mut out = Vec::new();
    for _ in 0..size {
        out.push(input_value(input, code, owner, registry, big, depth + 1)?);
    }
    Ok(I::Array(out))
}
fn record(
    input: &mut &[u8],
    owner: &TypeCode,
    registry: &Registry,
    big: bool,
    depth: usize,
) -> Result<I> {
    let slices = fields(input, owner, registry, big)?;
    if owner.is_enum {
        let code =
            Code::from_words(&owner.fields.first().ok_or(Error::Input)?.code).map_err(error)?;
        let I::Uint(value) = input_value(
            &mut { slices["value"] },
            &code,
            Some(owner),
            registry,
            big,
            depth + 1,
        )?
        else {
            return Err(Error::Input);
        };
        return Ok(owner
            .enum_map
            .get(&value.low_u32())
            .map_or(I::Uint(value), |s| I::String(s.as_bytes().to_vec())));
    }
    let mut values = Vec::new();
    for field in &owner.fields {
        let code = Code::from_words(&field.code).map_err(error)?;
        values.push((
            I::String(field.name.as_bytes().to_vec()),
            input_value(
                &mut { slices[&field.name] },
                &code,
                Some(owner),
                registry,
                big,
                depth + 1,
            )?,
        ));
    }
    Ok(I::Map(values))
}

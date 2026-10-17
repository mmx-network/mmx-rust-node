use crate::{hash::registry, input, Error, Record, Result, Value};
use vnx::{binary::Writer, code::Code, TypeCode};

struct Encoder(Vec<u8>);
impl Encoder {
    fn record(&mut self, record: &Record, header: bool, depth: usize) -> Result<()> {
        if depth > 512 {
            return Err(input("wire recursion limit"));
        }
        let schema = registry().by_name(&record.name)?;
        if header {
            let mut writer = Writer::new(Vec::new(), registry().clone());
            writer.write_type_code(schema)?;
            self.0.extend(writer.into_inner());
            self.0.extend(15u16.to_le_bytes());
            self.0.extend(schema.code_hash.to_le_bytes());
        }
        for ext in [false, true] {
            for field in schema.fields.iter().filter(|f| f.is_extended == ext) {
                let before = self.0.len();
                self.value(
                    record.field(&field.name)?,
                    &Code::from_words(&field.code)?,
                    schema,
                    depth + 1,
                )?;
                if !ext && field.data_size > 0 {
                    let size = field.data_size as usize;
                    if self.0.len() - before > size {
                        return Err(input("inline field size"));
                    }
                    self.0.resize(before + size, 0);
                }
            }
        }
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<()> {
        self.0.extend(
            u32::try_from(count)
                .map_err(|_| input("wire container length"))?
                .to_le_bytes(),
        );
        Ok(())
    }
    fn value(&mut self, v: &Value, code: &Code, owner: &TypeCode, depth: usize) -> Result<()> {
        if depth > 512 {
            return Err(input("wire recursion limit"));
        }
        match code {
            Code::Null => (),
            Code::Bool => self.0.push(u8::from(v.boolean()?)),
            Code::U8 | Code::U16 | Code::U32 | Code::U64 => {
                let n = match code {
                    Code::U8 => 1,
                    Code::U16 => 2,
                    Code::U32 => 4,
                    _ => 8,
                };
                let value = v.uint()?;
                if n < 8 && value >= 1u64 << (n * 8) {
                    return Err(input("unsigned wire range"));
                }
                self.0.extend(&value.to_le_bytes()[..n]);
            }
            Code::I8 | Code::I16 | Code::I32 | Code::I64 => {
                let n = match code {
                    Code::I8 => 1,
                    Code::I16 => 2,
                    Code::I32 => 4,
                    _ => 8,
                };
                let Value::Int(value) = v else {
                    return Err(input("signed wire integer"));
                };
                if n < 8 && !(-(1i64 << (n * 8 - 1))..(1i64 << (n * 8 - 1))).contains(value) {
                    return Err(input("signed wire range"));
                }
                self.0.extend(&value.to_le_bytes()[..n]);
            }
            Code::String => {
                let s = v.text()?;
                self.count(s.len())?;
                self.0.extend(s);
            }
            Code::F32 => {
                let Value::Float32(bits) = v else {
                    return Err(input("expected float32 bits"));
                };
                self.0.extend(bits.to_le_bytes());
            }
            Code::F64 => {
                let Value::Float64(bits) = v else {
                    return Err(input("expected float64 bits"));
                };
                self.0.extend(bits.to_le_bytes());
            }
            Code::Dynamic => {
                let Value::Variant(variant) = v else {
                    return Err(input("expected Variant"));
                };
                if variant.bytes().is_empty() {
                    self.0.extend([1, 0, 0, 0]);
                } else {
                    self.0.extend(variant.bytes());
                }
            }
            Code::Any => match v.optional()? {
                None => self.0.extend([0, 0]),
                Some(v) => self.record(v.record()?, true, depth + 1)?,
            },
            Code::Optional(c) => {
                let value = v.optional()?;
                self.0.push(u8::from(value.is_some()));
                if let Some(value) = value {
                    self.value(value, c, owner, depth + 1)?;
                }
            }
            Code::Array(n, c) => self.sequence(v, *n, c, owner, depth + 1)?,
            Code::List(c) => {
                let n = match v {
                    Value::Bytes(v) | Value::String(v) => v.len(),
                    _ => v.seq()?.len(),
                };
                self.count(n)?;
                self.sequence(v, n, c, owner, depth + 1)?;
            }
            Code::Map(k, value) => {
                let map = v.map()?;
                self.count(map.len())?;
                for (a, b) in map {
                    self.value(a, k, owner, depth + 1)?;
                    self.value(b, value, owner, depth + 1)?;
                }
            }
            Code::Tuple(codes) => {
                let values = v.seq()?;
                if values.len() != codes.len() {
                    return Err(input("tuple length"));
                }
                for (value, code) in values.iter().zip(codes) {
                    self.value(value, code, owner, depth + 1)?;
                }
            }
            Code::Struct(index) => {
                let schema = registry().get(
                    *owner
                        .depends
                        .get(*index)
                        .ok_or_else(|| input("struct dependency"))?,
                )?;
                if schema.is_enum {
                    self.value(
                        v,
                        &Code::from_words(&schema.fields[0].code)?,
                        schema,
                        depth + 1,
                    )?;
                } else {
                    let record = v.record()?;
                    if record.name != schema.name {
                        return Err(input("struct type"));
                    }
                    self.record(record, false, depth + 1)?;
                }
            }
            _ => return Err(Error::Unsupported(format!("VNX encoding {code:?}"))),
        }
        Ok(())
    }
    fn sequence(
        &mut self,
        v: &Value,
        n: usize,
        code: &Code,
        owner: &TypeCode,
        depth: usize,
    ) -> Result<()> {
        if matches!(code, Code::U8) {
            let bytes = v.bytes()?;
            if bytes.len() != n {
                return Err(input("array length"));
            }
            self.0.extend(bytes);
        } else if matches!(code, Code::I8) && matches!(v, Value::String(_)) {
            let bytes = v.text()?;
            if bytes.len() != n {
                return Err(input("array length"));
            }
            self.0.extend(bytes);
        } else {
            let values = v.seq()?;
            if values.len() != n {
                return Err(input("array length"));
            }
            for value in values {
                self.value(value, code, owner, depth + 1)?;
            }
        }
        Ok(())
    }
}
pub(crate) fn encode(record: &Record) -> Result<Vec<u8>> {
    let mut encoder = Encoder(Vec::new());
    encoder.record(record, true, 0)?;
    Ok(encoder.0)
}

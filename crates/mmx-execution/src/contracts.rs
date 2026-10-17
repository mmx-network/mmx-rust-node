use crate::wire;
use mmx_types::{vnx_registry, VnxType};
use mmx_vm::{storage::ContractData, Address, Error, InputValue as I, Result};
use std::{collections::BTreeMap, io::Cursor};
use vnx::{binary::Reader, code::Code, Registry, TypeCode};

#[derive(Clone, Debug)]
pub struct ExecutableInfo {
    pub binary: Address,
    pub init_method: Vec<u8>,
    pub depends: BTreeMap<Vec<u8>, Address>,
}
#[derive(Clone, Debug, Default)]
pub struct MethodInfo {
    pub is_init: bool,
    pub is_const: bool,
    pub is_public: bool,
    pub entry_point: u32,
}
pub struct BinaryInfo {
    pub constant: Vec<u8>,
    pub binary: Vec<u8>,
    pub methods: BTreeMap<Vec<u8>, MethodInfo>,
    pub line_info: BTreeMap<u32, u32>,
}
/// Original VNX bytes and their schemas. VM-visible strings and method names
/// remain bytes, even when a native contract contains non-UTF-8 text.
pub struct Contract {
    data: ContractData,
    registry: Registry,
    code: TypeCode,
    payload: usize,
    big: bool,
}
impl Contract {
    pub fn decode(data: ContractData) -> Result<Self> {
        // The JSON view is used only to collect schemas and validate the stream.
        // Every execution decision and CREAD value below uses original bytes.
        let mut reader =
            Reader::new(Cursor::new(&data.bytes), Registry::new()).with_utf8_lossy(true);
        reader.read().map_err(wire::error)?;
        let registry = reader.registry().clone();
        if reader.into_inner().position() as usize != data.bytes.len() {
            return Err(Error::Input);
        }
        let (payload, code, big) = wire::header(&data.bytes, &registry)?;
        let offset = data.bytes.len() - payload.len();
        Ok(Self {
            data,
            registry,
            code,
            payload: offset,
            big,
        })
    }
    pub fn encode<T: VnxType>(value: &T, read_cost: u64) -> Result<ContractData> {
        Ok(ContractData {
            bytes: value
                .encode(&vnx_registry().map_err(wire::error)?)
                .map_err(wire::error)?,
            read_cost,
        })
    }
    pub fn type_name(&self) -> &str {
        &self.code.name
    }
    pub fn executable(&self) -> Result<ExecutableInfo> {
        if self.type_name() != "mmx.contract.Executable" {
            return Err(failure("not an executable"));
        }
        let binary = blob(self.read_field(b"binary")?)?
            .try_into()
            .map_err(|_| Error::Input)?;
        let init_method = text(self.read_field(b"init_method")?)?;
        let mut depends = BTreeMap::new();
        for (key, value) in map(self.read_field(b"depends")?)? {
            depends.insert(
                text(key)?,
                blob(value)?.try_into().map_err(|_| Error::Input)?,
            );
        }
        Ok(ExecutableInfo {
            binary,
            init_method,
            depends,
        })
    }
    pub fn binary(&self) -> Result<BinaryInfo> {
        if self.type_name() != "mmx.contract.Binary" {
            return Err(failure("not a binary"));
        }
        let mut methods = BTreeMap::new();
        for (key, value) in map(self.read_field(b"methods")?)? {
            let fields: BTreeMap<_, _> = map(value)?
                .into_iter()
                .map(|(k, v)| Ok((text(k)?, v)))
                .collect::<Result<_>>()?;
            let get = |name: &[u8]| fields.get(name).cloned().unwrap_or(I::Nil);
            let method = MethodInfo {
                is_init: boolean(get(b"is_init"))?,
                is_const: boolean(get(b"is_const"))?,
                is_public: boolean(get(b"is_public"))?,
                entry_point: integer(get(b"entry_point"))?,
            };
            methods.insert(text(key)?, method);
        }
        let mut line_info = BTreeMap::new();
        for (key, value) in map(self.read_field(b"line_info")?)? {
            line_info.insert(integer(key)?, integer(value)?);
        }
        Ok(BinaryInfo {
            constant: blob(self.read_field(b"constant")?)?,
            binary: blob(self.read_field(b"binary")?)?,
            methods,
            line_info,
        })
    }
    pub fn read_field(&self, name: &[u8]) -> Result<I> {
        let value = self.raw_field(name)?;
        match (self.type_name(), name) {
            ("mmx.contract.PubKey", b"address") => {
                let address: Address = blob(value)?.try_into().map_err(|_| Error::Input)?;
                Ok(I::String(mmx_vm::address::encode(&address).into_bytes()))
            }
            ("mmx.contract.MultiSig", b"owners") => {
                let I::Array(owners) = value else {
                    return Err(Error::Input);
                };
                let mut addresses = std::collections::BTreeSet::new();
                for owner in owners {
                    addresses.insert(<Address>::try_from(blob(owner)?).map_err(|_| Error::Input)?);
                }
                Ok(I::Array(
                    addresses
                        .iter()
                        .map(|a| I::String(mmx_vm::address::encode(a).into_bytes()))
                        .collect(),
                ))
            }
            ("mmx.contract.WebData", b"payload") => Ok(I::String(
                blob(value)?
                    .iter()
                    .flat_map(|b| {
                        [
                            b"0123456789ABCDEF"[(b >> 4) as usize],
                            b"0123456789ABCDEF"[(b & 15) as usize],
                        ]
                    })
                    .collect(),
            )),
            _ => Ok(value),
        }
    }
    fn raw_field(&self, name: &[u8]) -> Result<I> {
        if name == b"__type" {
            return Ok(I::String(self.code.name.as_bytes().to_vec()));
        }
        let Some(field) = self.code.fields.iter().find(|f| f.name.as_bytes() == name) else {
            return Ok(I::Nil);
        };
        let fields = wire::fields(
            &mut &self.data.bytes[self.payload..],
            &self.code,
            &self.registry,
            self.big,
        )?;
        wire::input_value(
            &mut { fields[&field.name] },
            &Code::from_words(&field.code).map_err(wire::error)?,
            Some(&self.code),
            &self.registry,
            self.big,
            0,
        )
    }
}
fn map(value: I) -> Result<Vec<(I, I)>> {
    match value {
        I::Map(v) => Ok(v),
        I::Nil => Ok(Vec::new()),
        _ => Err(Error::Input),
    }
}
fn text(value: I) -> Result<Vec<u8>> {
    match value {
        I::String(v) => Ok(v),
        I::Nil => Ok(Vec::new()),
        _ => Err(Error::Input),
    }
}
fn blob(value: I) -> Result<Vec<u8>> {
    match value {
        I::Binary(v) => Ok(v),
        I::Nil => Ok(Vec::new()),
        _ => Err(Error::Input),
    }
}
fn integer(value: I) -> Result<u32> {
    match value {
        I::Uint(v) => Ok(v.low_u32()),
        I::Nil => Ok(0),
        _ => Err(Error::Input),
    }
}
fn boolean(value: I) -> Result<bool> {
    match value {
        I::Bool(v) => Ok(v),
        I::Nil => Ok(false),
        _ => Err(Error::Input),
    }
}
pub(crate) fn failure(message: impl Into<String>) -> Error {
    Error::Contract(message.into())
}

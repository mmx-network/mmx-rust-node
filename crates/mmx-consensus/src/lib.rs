//! Native MMX hash serialization. This is separate from VNX wire serialization.
//!
//! Decode original VNX bytes to preserve integer signs, byte strings, dynamic
//! types and opaque Variant payloads. Generated types are also accepted through
//! `Record::from_generated`, but their JSON-backed Variant fields cannot express
//! every native value. The `authorization` module supplies native signature
//! checks. The `header` and `proof` modules supply native header authentication,
//! proof-record hashes and plot identity checks. Proof work and ledger validation
//! remain separate stages.
pub mod authorization;
mod encode;
mod hash;
pub mod header;
pub mod proof;
mod validation;
mod wire;

use std::collections::BTreeMap;

pub use validation::ValidationParams;
pub type Hash = [u8; 32];
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid consensus input: {0}")]
    Input(String),
    #[error("unsupported consensus type: {0}")]
    Unsupported(String),
    #[error("tx cost amount overflow")]
    CostOverflow,
    #[error(transparent)]
    Vnx(#[from] vnx::Error),
}
fn input(message: impl Into<String>) -> Error {
    Error::Input(message.into())
}

/// Lossless values used by native MMX declarations. Strings contain bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    Null,
    Bool(bool),
    Uint(u64),
    Int(i64),
    Float32(u32),
    Float64(u64),
    String(Vec<u8>),
    Bytes(Vec<u8>),
    Sequence(Vec<Value>),
    Map(BTreeMap<Value, Value>),
    /// Original fields, including repeated names. Native Object conversion
    /// sorts names and keeps the last value; JSON validation checks them all.
    Object(Vec<(Vec<u8>, Value)>),
    Optional(Option<Box<Value>>),
    Record(Record),
    Variant(Variant),
    /// Payload of an unrecognized dynamic scalar (for example IEEE floats).
    Opaque(Vec<u8>),
}

/// Original dynamic bytes and the native value classification. The raw bytes
/// are part of hashing for types outside MMX's JSON value model.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Variant {
    pub(crate) value: Box<Value>,
    pub(crate) raw: Vec<u8>,
    pub(crate) json_depth: Option<usize>,
    pub(crate) copy_depth: usize,
}
impl Variant {
    /// Decode a VNX dynamic payload, including its byte-code header.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        wire::variant(bytes)
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn bytes(&self) -> &[u8] {
        &self.raw
    }
    pub fn is_json(&self) -> bool {
        self.json_depth.is_some_and(|depth| depth <= 100)
    }
    pub(crate) fn check_conversion(&self) -> Result<()> {
        let values: Box<dyn Iterator<Item = &Value> + '_> = match self.value.as_ref() {
            Value::Sequence(values) => Box::new(values.iter()),
            Value::Object(values) => Box::new(values.iter().map(|(_, v)| v)),
            _ => return Ok(()),
        };
        // Native to<vector<Variant>> / to<Object> copies each child using
        // VNX's independent 100-frame copy limit, before MMX traverses it.
        for value in values {
            if matches!(value, Value::Variant(v) if v.copy_depth > 100) {
                return Err(input("native Variant conversion recursion overflow"));
            }
        }
        Ok(())
    }
}

/// A native MMX declaration with lossless field values. Hashing uses the trusted
/// schema's type identity, never a type hash supplied by an untrusted record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Record {
    pub name: String,
    pub fields: BTreeMap<String, Value>,
}
impl Record {
    /// Decode one self-describing VNX record, requiring complete consumption.
    /// Only the checked-in schema versions are supported; other versions fail
    /// explicitly. Schema migration/default insertion is not implemented.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        wire::decode(bytes)
    }
    pub fn from_generated<T: vnx::VnxType>(value: &T) -> Result<Self> {
        Self::decode(&value.encode(&mmx_types::vnx_registry()?)?)
    }
    /// Encode the checked-in VNX schema while retaining original dynamic bytes
    /// and byte strings. This is wire encoding, not hash serialization.
    pub fn encode(&self) -> Result<Vec<u8>> {
        encode::encode(self)
    }
    pub fn field(&self, name: &str) -> Result<&Value> {
        self.fields
            .get(name)
            .ok_or_else(|| input(format!("missing {}.{name}", self.name)))
    }
    pub fn calc_hash(&self, full_hash: bool, hash_version: u32) -> Result<Hash> {
        hash::calc_hash(self, full_hash, hash_version)
    }
    /// Exact bytes hashed for a transaction ID or content hash.
    pub fn hash_serialize(&self, full_hash: bool) -> Result<Vec<u8>> {
        hash::transaction(self, full_hash)
    }
    pub fn is_valid(&self, params: &ValidationParams) -> Result<bool> {
        validation::is_valid(self, params)
    }
    pub fn calc_cost(&self, params: &ValidationParams) -> Result<u64> {
        validation::cost(self, params)
    }
    pub fn num_bytes(&self) -> Result<u64> {
        validation::num_bytes(self)
    }
    pub(crate) fn uint(&self, name: &str) -> Result<u64> {
        self.field(name)?.uint()
    }
    pub(crate) fn text(&self, name: &str) -> Result<&[u8]> {
        self.field(name)?.text()
    }
    pub(crate) fn seq(&self, name: &str) -> Result<&[Value]> {
        self.field(name)?.seq()
    }
    pub(crate) fn map(&self, name: &str) -> Result<&BTreeMap<Value, Value>> {
        self.field(name)?.map()
    }
}
impl Value {
    pub fn record(&self) -> Result<&Record> {
        match self {
            Self::Record(r) => Ok(r),
            _ => Err(input("expected record")),
        }
    }
    pub fn optional(&self) -> Result<Option<&Value>> {
        match self {
            Self::Optional(v) => Ok(v.as_deref()),
            Self::Null => Ok(None),
            Self::Record(_) => Ok(Some(self)),
            _ => Err(input("expected optional")),
        }
    }
    pub fn uint(&self) -> Result<u64> {
        match self {
            Self::Uint(v) => Ok(*v),
            _ => Err(input("expected unsigned integer")),
        }
    }
    pub fn text(&self) -> Result<&[u8]> {
        match self {
            Self::String(v) => Ok(v),
            _ => Err(input("expected string")),
        }
    }
    pub fn bytes(&self) -> Result<&[u8]> {
        match self {
            Self::Bytes(v) => Ok(v),
            _ => Err(input("expected bytes")),
        }
    }
    pub fn seq(&self) -> Result<&[Value]> {
        match self {
            Self::Sequence(v) => Ok(v),
            _ => Err(input("expected sequence")),
        }
    }
    pub fn map(&self) -> Result<&BTreeMap<Value, Value>> {
        match self {
            Self::Map(v) => Ok(v),
            _ => Err(input("expected map")),
        }
    }
    pub fn boolean(&self) -> Result<bool> {
        match self {
            Self::Bool(v) => Ok(*v),
            _ => Err(input("expected bool")),
        }
    }
}

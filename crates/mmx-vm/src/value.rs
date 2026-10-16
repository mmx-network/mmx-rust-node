//! MMX var_t layout on the supported little-endian C++ implementation.

use crate::{CodecError, Input, MAX_VALUE_BYTES, U256};
use std::cmp::Ordering;

pub const FLAG_DIRTY: u8 = 1;
pub const FLAG_CONST: u8 = 1 << 1;
pub const FLAG_STORED: u8 = 1 << 2;
pub const FLAG_DELETED: u8 = 1 << 3;
pub const FLAG_KEY: u8 = 1 << 4;

#[derive(Clone, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Ref(u64),
    Uint(U256),
    /// Strings are byte sequences in the C++ VM, not necessarily UTF-8.
    String(Vec<u8>),
    Binary(Vec<u8>),
    Array {
        address: u64,
        size: u32,
    },
    Map {
        address: u64,
    },
}

impl Value {
    pub fn string(value: impl AsRef<[u8]>) -> Self {
        Self::String(value.as_ref().to_vec())
    }

    pub fn type_tag(&self) -> u8 {
        match self {
            Self::Nil => 0,
            Self::Bool(false) => 1,
            Self::Bool(true) => 2,
            Self::Ref(_) => 3,
            Self::Uint(_) => 4,
            Self::String(_) => 5,
            Self::Binary(_) => 6,
            Self::Array { .. } => 7,
            Self::Map { .. } => 8,
        }
    }

    pub fn num_bytes(&self) -> usize {
        match self {
            Self::Ref(_) | Self::Map { .. } => 8,
            Self::Uint(_) => 32,
            Self::String(v) | Self::Binary(v) => v.len(),
            Self::Array { .. } => 12,
            _ => 0,
        }
    }

    pub fn is_true(&self) -> bool {
        match self {
            Self::Nil | Self::Bool(false) => false,
            Self::Uint(v) => !v.is_zero(),
            Self::String(v) | Self::Binary(v) => !v.is_empty(),
            _ => true,
        }
    }

    pub fn as_uint(&self) -> Option<U256> {
        if let Self::Uint(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    pub fn is_scalar(&self) -> bool {
        !matches!(self, Self::Ref(_) | Self::Array { .. } | Self::Map { .. })
    }
}

impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Self::Uint(v.into())
    }
}
impl From<U256> for Value {
    fn from(v: U256) -> Self {
        Self::Uint(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        let by_type = self.type_tag().cmp(&other.type_tag());
        if by_type != Ordering::Equal {
            return by_type;
        }
        match (self, other) {
            (Self::Ref(l), Self::Ref(r)) => l.cmp(r),
            (Self::Uint(l), Self::Uint(r)) => l.cmp(r),
            (Self::String(l), Self::String(r)) | (Self::Binary(l), Self::Binary(r)) => {
                l.len().cmp(&r.len()).then(l.cmp(r))
            }
            (Self::Array { address: l, .. }, Self::Array { address: r, .. })
            | (Self::Map { address: l }, Self::Map { address: r }) => l.cmp(r),
            _ => Ordering::Equal,
        }
    }
}
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Value {}

#[derive(Clone, Debug)]
pub struct Var {
    pub value: Value,
    pub flags: u8,
    pub ref_count: u32,
}

impl Var {
    pub fn new(value: impl Into<Value>) -> Self {
        Self {
            value: value.into(),
            flags: 0,
            ref_count: 0,
        }
    }

    pub fn encode(&self, with_rc: bool, with_flags: bool) -> Result<Vec<u8>, CodecError> {
        if self.value.num_bytes() > MAX_VALUE_BYTES {
            return Err(CodecError::Limit);
        }
        let mut out = Vec::new();
        if with_rc {
            out.extend(self.ref_count.to_le_bytes());
        }
        if with_flags {
            out.push(self.flags);
        }
        let tag_pos = out.len();
        out.push(self.value.type_tag());
        match &self.value {
            Value::Nil | Value::Bool(_) => (),
            Value::Ref(address) | Value::Map { address } => out.extend(address.to_le_bytes()),
            Value::Uint(v) => {
                let (tag, size) = match v.bits() {
                    0..=16 => (9, 2),
                    17..=32 => (10, 4),
                    33..=64 => (11, 8),
                    65..=128 => (12, 16),
                    _ => (13, 32),
                };
                let mut bytes = [0; 32];
                v.to_little_endian(&mut bytes);
                out[tag_pos] = tag;
                out.extend(&bytes[..size]);
            }
            Value::String(v) | Value::Binary(v) => {
                out.extend((v.len() as u32).to_le_bytes());
                out.extend(v);
            }
            Value::Array { address, size } => {
                out.extend(address.to_le_bytes());
                out.extend(size.to_le_bytes());
            }
        }
        Ok(out)
    }

    /// Decode one value and return bytes consumed (the C++ API permits a tail).
    pub fn decode(
        data: &[u8],
        with_rc: bool,
        with_flags: bool,
    ) -> Result<(Self, usize), CodecError> {
        let mut input = Input::new(data);
        let ref_count = if with_rc { input.u32()? } else { 0 };
        let flags = if with_flags { input.byte()? } else { 0 };
        let value = match input.byte()? {
            0 => Value::Nil,
            1 => Value::Bool(false),
            2 => Value::Bool(true),
            3 => Value::Ref(input.u64()?),
            tag @ (4 | 9..=13) => {
                let size = match tag {
                    9 => 2,
                    10 => 4,
                    11 => 8,
                    12 => 16,
                    _ => 32,
                };
                Value::Uint(U256::from_little_endian(input.take(size)?))
            }
            tag @ (5 | 6) => {
                let size = input.u32()? as usize;
                if size > MAX_VALUE_BYTES {
                    return Err(CodecError::Limit);
                }
                let bytes = input.take(size)?.to_vec();
                if tag == 5 {
                    Value::String(bytes)
                } else {
                    Value::Binary(bytes)
                }
            }
            7 => Value::Array {
                address: input.u64()?,
                size: input.u32()?,
            },
            8 => Value::Map {
                address: input.u64()?,
            },
            _ => return Err(CodecError::Invalid("value tag")),
        };
        Ok((
            Self {
                value,
                flags,
                ref_count,
            },
            input.pos,
        ))
    }
}

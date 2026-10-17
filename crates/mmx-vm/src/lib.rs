//! MMX-specific contract VM. VNX serialization is maintained in `vnx-rust`.

pub mod address;
pub mod engine;
pub mod instruction;
pub mod storage;
pub mod value;

pub use engine::{Engine, Error, Host, NoHost, Output, Result};
pub use instruction::{Instruction, Opcode};
pub use primitive_types::U256;
pub use storage::{Storage, StorageDb};
pub use value::{Value, Var};

/// Raw contract address/hash bytes (little endian for address presentation).
pub type Address = [u8; 32];

pub const SEG_SIZE: u64 = 0x4000000;
pub const STACK_SIZE: u64 = 16 * SEG_SIZE;
pub const MEM_CONST: u64 = 0;
pub const MEM_EXTERN: u64 = SEG_SIZE;
pub const MEM_STACK: u64 = MEM_EXTERN + SEG_SIZE;
pub const MEM_STATIC: u64 = MEM_STACK + STACK_SIZE;
pub const MEM_HEAP: u64 = 1 << 32;
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_KEY_BYTES: usize = 4096;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CodecError {
    #[error("unexpected end of input")]
    Eof,
    #[error("invalid encoding: {0}")]
    Invalid(&'static str),
    #[error("encoding limit exceeded")]
    Limit,
}

pub(crate) struct Input<'a> {
    data: &'a [u8],
    pub pos: usize,
}

impl<'a> Input<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    fn take(&mut self, count: usize) -> std::result::Result<&'a [u8], CodecError> {
        if count > self.remaining() {
            return Err(CodecError::Eof);
        }
        let out = &self.data[self.pos..self.pos + count];
        self.pos += count;
        Ok(out)
    }
    fn byte(&mut self) -> std::result::Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> std::result::Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> std::result::Result<u64, CodecError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

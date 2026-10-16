//! Scalar execution slice of the C++ Engine. Composite values and host opcodes
//! fail explicitly until their ownership and node integration are ported.

use crate::{
    instruction::{BITWISE, CATCH_OVERFLOW, REF_A, REF_B},
    storage::{self, Storage, StorageWrite},
    value::{FLAG_CONST, FLAG_DELETED, FLAG_DIRTY, FLAG_KEY, FLAG_STORED},
    Address, Instruction, Opcode, Value, Var, MAX_VALUE_BYTES, MEM_EXTERN, MEM_STACK, MEM_STATIC,
    STACK_SIZE, U256,
};
use sha2::{Digest, Sha256 as Sha256Hasher};
use std::collections::BTreeMap;

pub const INSTR_COST: u64 = 20;
pub const CALL_COST: u64 = 30;
pub const WRITE_COST: u64 = 50;
pub const WRITE_32_BYTE_COST: u64 = 30;
pub const STORAGE_READ_COST: u64 = 2000;
pub const STORAGE_READ_32_BYTE_COST: u64 = 50;
pub const STORAGE_WRITE_COST: u64 = 2000;
pub const STORAGE_WRITE_BYTE_COST: u64 = 50;
pub const MAX_CALL_RECURSION: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("out of gas: {used} > {limit}")]
    OutOfGas { used: u64, limit: u64 },
    #[error("read-only memory at {0:#x}")]
    ReadOnly(u64),
    #[error("read fail at {0:#x}")]
    Missing(u64),
    #[error("invalid value type (expected {0})")]
    Type(&'static str),
    #[error("integer overflow")]
    Overflow,
    #[error("division by zero")]
    DivisionByZero,
    #[error("opcode not yet ported: {0:?}")]
    UnsupportedOpcode(Opcode),
    #[error("value type not yet executable: {0}")]
    UnsupportedValue(u8),
    #[error("invalid engine state: {0}")]
    State(&'static str),
    #[error("instruction pointer out of bounds: {0}")]
    InstructionPointer(u64),
    #[error("stack overflow")]
    StackOverflow,
    #[error("recursion overflow")]
    RecursionOverflow,
    #[error("value size limit exceeded")]
    ValueSize,
    #[error("out of bounds read")]
    Bounds,
    #[error("unsupported instruction flags")]
    Flags,
    #[error("contract failed ({code}): {message}")]
    Fail { code: u32, message: String },
    #[error(transparent)]
    Storage(#[from] storage::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy)]
struct Frame {
    pc: u64,
    stack: u64,
}

pub struct Engine<'a, S: Storage> {
    pub contract: Address,
    pub protocol_version: u32,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub error_addr: Option<u64>,
    pub error_code: u32,
    backend: &'a mut S,
    read_only: bool,
    memory: BTreeMap<u64, Var>,
    frames: Vec<Frame>,
    initialized: bool,
    failed: bool,
    completed: bool,
}

impl<'a, S: Storage> Engine<'a, S> {
    pub fn new(
        contract: Address,
        backend: &'a mut S,
        read_only: bool,
        protocol_version: u32,
        gas_limit: u64,
    ) -> Self {
        Self {
            contract,
            backend,
            read_only,
            protocol_version,
            gas_limit,
            gas_used: 0,
            error_addr: None,
            error_code: 0,
            memory: BTreeMap::new(),
            frames: Vec::new(),
            initialized: false,
            failed: false,
            completed: false,
        }
    }

    pub fn init(&mut self) -> Result<()> {
        if self.initialized {
            return Err(Error::State("already initialized"));
        }
        self.initialized = true;
        Ok(())
    }

    /// Seed constants / external inputs before init, or assign mutable cells.
    /// Refs, arrays and maps are rejected until reference ownership is ported.
    pub fn write(&mut self, address: u64, value: impl Into<Value>) -> Result<()> {
        if self.failed {
            return Err(Error::State("execution failed"));
        }
        let result = self.write_value(address, value.into(), false);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn write_value(&mut self, address: u64, value: Value, force_assign: bool) -> Result<()> {
        if !value.is_scalar() {
            return Err(Error::UnsupportedValue(value.type_tag()));
        }
        if value.num_bytes() > MAX_VALUE_BYTES {
            return Err(Error::ValueSize);
        }
        if (self.initialized && address < MEM_EXTERN) || (self.read_only && address >= MEM_STATIC) {
            return Err(Error::ReadOnly(address));
        }
        let _ = self.read(address)?;
        let previous = self.memory.get(&address);
        if previous.is_some_and(|var| var.flags & FLAG_CONST != 0) {
            return Err(Error::ReadOnly(address));
        }
        // C++ write() updates existing uint and nil/bool cells without assigning
        // a new allocation; those writes do not incur WRITE_COST.
        let in_place = !force_assign
            && previous.is_some_and(|var| {
                matches!((&var.value, &value), (Value::Uint(_), Value::Uint(_)))
                    || matches!(
                        (&var.value, &value),
                        (Value::Nil | Value::Bool(_), Value::Nil | Value::Bool(_))
                    )
            });
        if !in_place {
            self.charge(WRITE_COST + value.num_bytes() as u64 * WRITE_32_BYTE_COST / 32)?;
        }
        let mut next = self
            .memory
            .get(&address)
            .cloned()
            .unwrap_or_else(|| Var::new(Value::Nil));
        if !in_place || next.value != value || next.flags & FLAG_DELETED != 0 {
            next.flags = (next.flags | FLAG_DIRTY) & !FLAG_DELETED;
        }
        next.value = value;
        self.memory.insert(address, next);
        Ok(())
    }

    pub fn read(&mut self, address: u64) -> Result<Option<Value>> {
        if let Some(var) = self.memory.get(&address) {
            return Ok((var.flags & FLAG_DELETED == 0).then(|| var.value.clone()));
        }
        if address < MEM_STATIC {
            return Ok(None);
        }
        let loaded = self.backend.read(&self.contract, address, None)?;
        let size = loaded
            .as_ref()
            .filter(|v| v.flags & FLAG_DELETED == 0)
            .map_or(0, |v| v.value.num_bytes());
        self.charge(STORAGE_READ_COST + (size / 32) as u64 * STORAGE_READ_32_BYTE_COST)?;
        if let Some(mut var) = loaded {
            if var.flags & FLAG_DELETED != 0 {
                return Ok(None);
            }
            if !var.value.is_scalar() {
                return Err(Error::UnsupportedValue(var.value.type_tag()));
            }
            var.flags = (var.flags & !FLAG_DIRTY) | FLAG_STORED;
            if self.read_only {
                var.flags |= FLAG_CONST;
            }
            let value = var.value.clone();
            self.memory.insert(address, var);
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    /// Execute without mutating the backend. An error permanently prevents
    /// this engine instance from publishing its accumulated writes.
    pub fn run(&mut self, code: &[Instruction], entry: u32) -> Result<()> {
        if !self.initialized || self.failed || !self.frames.is_empty() {
            return Err(Error::State(
                "run requires an initialized, healthy, idle engine",
            ));
        }
        self.completed = false;
        for (_, var) in self.memory.range_mut(..MEM_STACK) {
            var.flags = (var.flags | FLAG_CONST) & !FLAG_DIRTY;
        }
        self.frames.push(Frame {
            pc: entry as u64,
            stack: 0,
        });
        while let Some(frame) = self.frames.last().copied() {
            let result = code
                .get(frame.pc as usize)
                .ok_or(Error::InstructionPointer(frame.pc))
                .and_then(|instr| self.exec(*instr));
            if let Err(error) = result {
                self.failed = true;
                self.error_addr = Some(frame.pc);
                return Err(error);
            }
        }
        self.completed = true;
        Ok(())
    }

    /// Publish one atomic storage batch after execution and all commit gas
    /// charges succeed. The caller advances the database version separately.
    pub fn commit(&mut self) -> Result<()> {
        if self.failed || !self.completed || !self.frames.is_empty() {
            return Err(Error::State("commit requires successful execution"));
        }
        let writes: Vec<_> = self
            .memory
            .range(MEM_STATIC..)
            .filter(|(_, var)| var.flags & FLAG_DIRTY != 0)
            .map(|(address, var)| StorageWrite {
                address: *address,
                key: None,
                var: var.clone(),
            })
            .collect();
        let result = (|| {
            for write in &writes {
                if self.read_only {
                    return Err(Error::ReadOnly(write.address));
                }
                let factor = if write.var.flags & FLAG_KEY != 0 {
                    2
                } else {
                    1
                };
                self.charge(
                    (STORAGE_WRITE_COST
                        + write.var.value.num_bytes() as u64 * STORAGE_WRITE_BYTE_COST)
                        * factor,
                )?;
            }
            self.backend.apply(&self.contract, &writes)?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
            return result;
        }
        self.memory
            .retain(|address, _| !(MEM_STACK..MEM_STATIC).contains(address));
        for write in writes {
            self.memory.get_mut(&write.address).unwrap().flags &= !FLAG_DIRTY;
        }
        self.completed = false;
        Ok(())
    }

    fn charge(&mut self, amount: u64) -> Result<()> {
        self.gas_used = self.gas_used.saturating_add(amount);
        if self.gas_used > self.gas_limit {
            return Err(Error::OutOfGas {
                used: self.gas_used,
                limit: self.gas_limit,
            });
        }
        Ok(())
    }

    fn load(&mut self, address: u64) -> Result<Value> {
        self.read(address)?.ok_or(Error::Missing(address))
    }
    fn uint(&mut self, address: u64) -> Result<U256> {
        self.load(address)?.as_uint().ok_or(Error::Type("uint"))
    }

    fn address(&mut self, instr: Instruction, i: usize) -> Result<u64> {
        let mut address = instr.args[i] as u64;
        if (MEM_STACK..MEM_STATIC).contains(&address) {
            address += self
                .frames
                .last()
                .ok_or(Error::State("empty call stack"))?
                .stack;
            if address >= MEM_STATIC {
                return Err(Error::StackOverflow);
            }
        }
        // A scalar passes through deref() unchanged in C++. Reference values
        // are already rejected by load(), so no ownership behavior is guessed.
        if instr.flags & (1 << i) != 0 {
            self.load(address)?;
        }
        Ok(address)
    }

    fn immediate(&mut self, instr: Instruction, i: usize) -> Result<u64> {
        if instr.flags & (1 << i) == 0 {
            Ok(instr.args[i] as u64)
        } else {
            let mut address = instr;
            address.flags &= !(1 << i);
            let address = self.address(address, i)?;
            Ok(self.uint(address)?.low_u64())
        }
    }

    fn exec(&mut self, instr: Instruction) -> Result<()> {
        use Opcode::*;
        self.charge(INSTR_COST)?;
        if instr.flags & 0x80 != 0 {
            return Err(Error::Flags);
        }
        match instr.code {
            Nop => (),
            Clr => {
                if instr.flags & REF_A != 0 {
                    return Err(Error::Flags);
                }
                let address = instr.args[0] as u64; // C++ CLR uses an absolute address.
                if self.load_optional_for_clear(address)? {
                    let var = self.memory.get_mut(&address).unwrap();
                    var.flags = if var.flags & FLAG_STORED != 0 {
                        var.flags | FLAG_DIRTY | FLAG_DELETED
                    } else {
                        FLAG_DELETED
                    };
                    var.value = Value::Nil;
                }
            }
            Copy => {
                let dst = self.address(instr, 0)?;
                let src = self.address(instr, 1)?;
                if dst != src {
                    let value = self.load(src)?;
                    self.write_value(dst, value, false)?;
                }
            }
            Jump | JumpIf | JumpNot => {
                let take = if instr.code == Jump {
                    true
                } else {
                    let address = self.address(instr, 1)?;
                    self.load(address)?.is_true() == (instr.code == JumpIf)
                };
                if take {
                    let target = self.immediate(instr, 0)?;
                    self.frames.last_mut().unwrap().pc = target;
                    return Ok(());
                }
            }
            Call => {
                if instr.flags & REF_B != 0 {
                    return Err(Error::Flags);
                }
                let pc = self.immediate(instr, 0)?;
                let offset = instr.args[1] as u64;
                let stack = self.frames.last().unwrap().stack + offset;
                if stack >= STACK_SIZE {
                    return Err(Error::StackOverflow);
                }
                if self.frames.len() >= MAX_CALL_RECURSION {
                    return Err(Error::RecursionOverflow);
                }
                self.charge(CALL_COST)?;
                self.frames.push(Frame { pc, stack });
                return Ok(());
            }
            Ret => {
                let frame = self.frames.pop().unwrap();
                let start = MEM_STACK + frame.stack + 1;
                self.memory
                    .retain(|address, _| !(start..MEM_STATIC).contains(address));
                if self.frames.is_empty() {
                    return Ok(());
                }
            }
            Add | Sub | Mul | Div | Mod | Min | Max => {
                let dst = self.address(instr, 0)?;
                let lhs = self.address(instr, 1)?;
                let rhs = self.address(instr, 2)?;
                let l = self.uint(lhs)?;
                let r = self.uint(rhs)?;
                let checked = instr.flags & CATCH_OVERFLOW != 0;
                let (value, overflow) = match instr.code {
                    Add => l.overflowing_add(r),
                    Sub => l.overflowing_sub(r),
                    Mul => {
                        self.charge(if l.bits() > 64 || r.bits() > 64 {
                            200
                        } else {
                            50
                        })?;
                        let (value, overflow) = l.overflowing_mul(r);
                        let overflow = if self.protocol_version >= 1 {
                            overflow
                        } else {
                            value < l && value < r
                        };
                        (value, overflow)
                    }
                    Div | Mod => {
                        if r.is_zero() {
                            return Err(Error::DivisionByZero);
                        }
                        self.charge(if l.bits() > 128 {
                            5000
                        } else if l.bits() > 64 {
                            1000
                        } else {
                            50
                        })?;
                        (if instr.code == Div { l / r } else { l % r }, false)
                    }
                    Min => (l.min(r), false),
                    Max => (l.max(r), false),
                    _ => unreachable!(),
                };
                if checked && overflow {
                    return Err(Error::Overflow);
                }
                self.write_value(dst, Value::Uint(value), false)?;
            }
            Not => {
                let dst = self.address(instr, 0)?;
                let src = self.address(instr, 1)?;
                let value = self.load(src)?;
                let value = if instr.flags & BITWISE != 0 {
                    match value {
                        Value::Uint(v) => Value::Uint(!v),
                        Value::Binary(v) => Value::Binary(v.into_iter().map(|b| !b).collect()),
                        _ => return Err(Error::Type("uint or binary")),
                    }
                } else {
                    Value::Bool(!value.is_true())
                };
                self.write_value(dst, value, false)?;
            }
            Xor | And | Or => {
                let dst = self.address(instr, 0)?;
                let lhs = self.address(instr, 1)?;
                let rhs = self.address(instr, 2)?;
                let l = self.load(lhs)?;
                let r = self.load(rhs)?;
                let value = if instr.flags & BITWISE == 0 {
                    Value::Bool(match instr.code {
                        Xor => l.is_true() ^ r.is_true(),
                        And => l.is_true() & r.is_true(),
                        _ => l.is_true() | r.is_true(),
                    })
                } else {
                    match (l, r) {
                        (Value::Uint(l), Value::Uint(r)) => Value::Uint(match instr.code {
                            Xor => l ^ r,
                            And => l & r,
                            _ => l | r,
                        }),
                        (Value::Binary(l), Value::Binary(r)) if l.len() == r.len() => {
                            Value::Binary(
                                l.into_iter()
                                    .zip(r)
                                    .map(|(l, r)| match instr.code {
                                        Xor => l ^ r,
                                        And => l & r,
                                        _ => l | r,
                                    })
                                    .collect(),
                            )
                        }
                        _ => return Err(Error::Type("matching uints or equal length binaries")),
                    }
                };
                self.write_value(dst, value, false)?;
            }
            Shl | Shr => {
                let dst = self.address(instr, 0)?;
                let src = self.address(instr, 1)?;
                let shift = self.immediate(instr, 2)?;
                let value = self.uint(src)?;
                let value = if shift >= 256 {
                    U256::zero()
                } else if instr.code == Shl {
                    value << shift as usize
                } else {
                    value >> shift as usize
                };
                self.write_value(dst, Value::Uint(value), false)?;
            }
            CmpEq | CmpNe | CmpLt | CmpGt | CmpLe | CmpGe => {
                let dst = self.address(instr, 0)?;
                let lhs = self.address(instr, 1)?;
                let rhs = self.address(instr, 2)?;
                let l = self.load(lhs)?;
                let r = self.load(rhs)?;
                if !matches!(instr.code, CmpEq | CmpNe) && l.type_tag() != r.type_tag() {
                    return Err(Error::Type("same types for ordering"));
                }
                let value = match instr.code {
                    CmpEq => l == r,
                    CmpNe => l != r,
                    CmpLt => l < r,
                    CmpGt => l > r,
                    CmpLe => l <= r,
                    _ => l >= r,
                };
                self.write_value(dst, Value::Bool(value), false)?;
            }
            Type | Size => {
                let dst = self.address(instr, 0)?;
                let src = self.address(instr, 1)?;
                let value = self.load(src)?;
                let value = if instr.code == Type {
                    value.type_tag() as u64
                } else {
                    match value {
                        Value::String(v) | Value::Binary(v) => v.len() as u64,
                        _ => return Err(Error::Type("string or binary")),
                    }
                };
                self.write_value(dst, Value::from(value), false)?;
            }
            Concat | Memcpy | Sha256 => {
                let dst = self.address(instr, 0)?;
                let src = self.address(instr, 1)?;
                let source = self.load(src)?;
                let is_string = matches!(source, Value::String(_));
                let mut bytes = match source {
                    Value::String(v) | Value::Binary(v) => v,
                    _ => return Err(Error::Type("string or binary")),
                };
                if instr.code == Concat {
                    let rhs = self.address(instr, 2)?;
                    let other = self.load(rhs)?;
                    let more = match other {
                        Value::String(v) if is_string => v,
                        Value::Binary(v) if !is_string => v,
                        _ => return Err(Error::Type("same string or binary type")),
                    };
                    if bytes.len() + more.len() > MAX_VALUE_BYTES {
                        return Err(Error::ValueSize);
                    }
                    bytes.extend(more);
                } else if instr.code == Memcpy {
                    let count = self.immediate(instr, 2)?;
                    let offset = self.immediate(instr, 3)?;
                    if offset > bytes.len() as u64 || count > bytes.len() as u64 - offset {
                        return Err(Error::Bounds);
                    }
                    bytes = bytes[offset as usize..(offset + count) as usize].to_vec();
                } else {
                    self.charge((bytes.len() as u64).div_ceil(64) * 2000)?;
                    bytes = Sha256Hasher::digest(bytes).to_vec();
                }
                let value = if is_string && instr.code != Sha256 {
                    Value::String(bytes)
                } else {
                    Value::Binary(bytes)
                };
                self.write_value(dst, value, instr.code != Sha256)?;
            }
            Fail => {
                let code = self.immediate(instr, 1)? as u32;
                self.error_code = code;
                let address = self.address(instr, 0)?;
                let message = match self.read(address)? {
                    Some(Value::String(v)) => String::from_utf8_lossy(&v).into_owned(),
                    Some(Value::Nil) => "null".to_owned(),
                    Some(Value::Bool(v)) => v.to_string(),
                    Some(Value::Uint(v)) => v.to_string(),
                    Some(Value::Binary(v)) => {
                        // Address presentation for 32-byte binaries belongs
                        // to the node layer; retain their raw hex here.
                        let mut message = String::from("0x");
                        for byte in v {
                            use std::fmt::Write;
                            write!(&mut message, "{byte:02x}").unwrap();
                        }
                        message
                    }
                    None => "nullptr".to_owned(),
                    Some(value) => return Err(Error::UnsupportedValue(value.type_tag())),
                };
                return Err(Error::Fail { code, message });
            }
            other => return Err(Error::UnsupportedOpcode(other)),
        }
        if let Some(frame) = self.frames.last_mut() {
            frame.pc += 1;
        }
        Ok(())
    }

    fn load_optional_for_clear(&mut self, address: u64) -> Result<bool> {
        if self.initialized && address < MEM_EXTERN {
            return Err(Error::ReadOnly(address));
        }
        let _ = self.read(address)?;
        if let Some(var) = self.memory.get(&address) {
            if var.flags & FLAG_CONST != 0 || (self.read_only && address >= MEM_STATIC) {
                return Err(Error::ReadOnly(address));
            }
            if var.ref_count != 0 {
                return Err(Error::State("cannot erase a referenced cell"));
            }
            let present = var.flags & FLAG_DELETED == 0;
            if present {
                self.charge(WRITE_COST)?;
            }
            Ok(present)
        } else {
            Ok(false)
        }
    }
}

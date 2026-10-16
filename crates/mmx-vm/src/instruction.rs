//! MMX instruction IDs and its symbol-table compressed bytecode (version 1).
//! This is MMX's own format, independent of the VNX type system.

use crate::{CodecError, Input};
use std::collections::BTreeMap;

pub const REF_A: u8 = 1;
pub const REF_B: u8 = 1 << 1;
pub const REF_C: u8 = 1 << 2;
pub const REF_D: u8 = 1 << 3;
pub const HARD_FAIL: u8 = 1 << 4;
pub const CATCH_OVERFLOW: u8 = 1 << 5;
pub const BITWISE: u8 = 1 << 6;
const MAX_SYMBOLS: usize = 0x7FFF + 0xFF;

macro_rules! opcodes {
    ($($name:ident = $id:literal, $argc:literal;)+) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u8)]
        pub enum Opcode { $($name = $id,)+ }
        impl Opcode {
            pub fn nargs(self) -> usize { match self { $(Self::$name => $argc,)+ } }
        }
        impl TryFrom<u8> for Opcode {
            type Error = CodecError;
            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value { $($id => Ok(Self::$name),)+ _ => Err(CodecError::Invalid("opcode")) }
            }
        }
    }
}

opcodes! {
    Nop = 0x00, 0; Clr = 0x01, 1; Copy = 0x02, 2; Clone = 0x03, 2;
    Jump = 0x04, 1; JumpIf = 0x05, 2; JumpNot = 0x06, 2; Call = 0x07, 2; Ret = 0x08, 0;
    Add = 0x20, 3; Sub = 0x21, 3; Mul = 0x22, 3; Div = 0x23, 3; Mod = 0x24, 3;
    Not = 0x40, 2; Xor = 0x41, 3; And = 0x42, 3; Or = 0x43, 3;
    Min = 0x44, 3; Max = 0x45, 3; Shl = 0x46, 3; Shr = 0x47, 3;
    CmpEq = 0x60, 3; CmpNe = 0x61, 3; CmpLt = 0x62, 3; CmpGt = 0x63, 3;
    CmpLe = 0x64, 3; CmpGe = 0x65, 3;
    Type = 0x80, 2; Size = 0x81, 2; Get = 0x82, 3; Set = 0x83, 3;
    Erase = 0x84, 2; PushBack = 0x85, 2; PopBack = 0x86, 2;
    Conv = 0xA0, 4; Concat = 0xA1, 3; Memcpy = 0xA2, 4; Sha256 = 0xA3, 2; Verify = 0xA4, 4;
    Log = 0xC0, 2; Send = 0xC1, 4; Mint = 0xC2, 3; Event = 0xC3, 2;
    Fail = 0xC4, 2; RemoteCall = 0xC5, 4; ContractRead = 0xC6, 3; Balance = 0xC7, 2;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub code: Opcode,
    pub flags: u8,
    pub args: [u32; 4],
}

impl Instruction {
    pub fn new(code: Opcode, args: [u32; 4]) -> Self {
        Self {
            code,
            flags: 0,
            args,
        }
    }
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }
    fn symbol(self) -> u32 {
        self.code as u32 | ((self.flags as u32) << 8)
    }
}

pub fn encode(code: &[Instruction]) -> Result<Vec<u8>, CodecError> {
    let count = u32::try_from(code.len()).map_err(|_| CodecError::Limit)?;
    let mut counts = BTreeMap::<u32, usize>::new();
    for instr in code {
        *counts.entry(instr.symbol()).or_default() += 1;
        for arg in &instr.args[..instr.code.nargs()] {
            *counts.entry(*arg).or_default() += 1;
        }
    }
    let mut symbols: Vec<_> = counts.into_iter().filter(|(_, count)| *count > 1).collect();
    symbols.sort_by(|(l, lc), (r, rc)| rc.cmp(lc).then(l.cmp(r)));
    symbols.truncate(MAX_SYMBOLS);
    let index: BTreeMap<_, _> = symbols
        .iter()
        .enumerate()
        .map(|(i, (value, _))| (*value, i))
        .collect();
    let mut out = vec![1];
    out.extend((symbols.len() as u32).to_le_bytes());
    for (symbol, _) in &symbols {
        out.extend(symbol.to_le_bytes());
    }
    out.extend(count.to_le_bytes());
    for instr in code {
        encode_symbol(&mut out, instr.symbol(), &index);
        for arg in &instr.args[..instr.code.nargs()] {
            encode_symbol(&mut out, *arg, &index);
        }
    }
    Ok(out)
}

fn encode_symbol(out: &mut Vec<u8>, value: u32, index: &BTreeMap<u32, usize>) {
    if let Some(&i) = index.get(&value) {
        if i < 0xFF {
            out.push(i as u8);
        } else {
            out.push(0xFF);
            out.extend(((i - 0xFF) as u16).to_le_bytes());
        }
    } else if value < 0x7FFF {
        out.push(0xFF);
        out.extend((value as u16 | 0x8000).to_le_bytes());
    } else {
        out.extend([0xFF; 3]);
        out.extend(value.to_le_bytes());
    }
}

pub fn decode(data: &[u8]) -> Result<(Vec<Instruction>, usize), CodecError> {
    let mut input = Input::new(data);
    if input.byte()? != 1 {
        return Err(CodecError::Invalid("bytecode version"));
    }
    let size = input.u32()? as usize;
    if size > MAX_SYMBOLS {
        return Err(CodecError::Limit);
    }
    if size > input.remaining() / 4 {
        return Err(CodecError::Eof);
    }
    let mut symbols = Vec::with_capacity(size);
    for _ in 0..size {
        symbols.push(input.u32()?);
    }
    let count = input.u32()? as usize;
    if count > input.remaining() {
        return Err(CodecError::Eof);
    }
    let mut code = Vec::new();
    for _ in 0..count {
        let symbol = decode_symbol(&mut input, &symbols)?;
        if symbol > u16::MAX as u32 {
            return Err(CodecError::Invalid("instruction flags"));
        }
        let mut instr = Instruction::new(Opcode::try_from(symbol as u8)?, [0; 4]);
        instr.flags = (symbol >> 8) as u8;
        for arg in &mut instr.args[..instr.code.nargs()] {
            *arg = decode_symbol(&mut input, &symbols)?;
        }
        code.push(instr);
    }
    Ok((code, input.pos))
}

fn decode_symbol(input: &mut Input<'_>, symbols: &[u32]) -> Result<u32, CodecError> {
    let first = input.byte()?;
    let index = if first < 0xFF {
        first as usize
    } else {
        let second = u16::from_le_bytes(input.take(2)?.try_into().unwrap());
        if second == 0xFFFF {
            return input.u32();
        }
        if second >= 0x7FFF {
            return Ok((second & 0x7FFF) as u32);
        }
        second as usize + 0xFF
    };
    symbols
        .get(index)
        .copied()
        .ok_or(CodecError::Invalid("symbol index"))
}

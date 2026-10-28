//! Native vm_interface.cpp: binary loading, visitor assignment and engine copies.
use super::*;
use mmx_types::mmx::contract::Binary;

pub const MAX_COPY_RECURSION: usize = 100;

/// Values at a contract boundary. Binary and string values remain distinct;
/// VM strings and memos do not have to be UTF-8.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputValue {
    /// A native visitor error, deferred until this leaf is assigned so earlier
    /// allocations and writes keep their native gas costs.
    Unsupported,
    Nil,
    Bool(bool),
    Uint(U256),
    String(Vec<u8>),
    Binary(Vec<u8>),
    Array(Vec<InputValue>),
    Map(Vec<(InputValue, InputValue)>),
}

impl TryFrom<&vnx::Variant> for InputValue {
    type Error = Error;
    fn try_from(value: &vnx::Variant) -> Result<Self> {
        fn convert(value: &vnx::Variant, depth: usize) -> Result<InputValue> {
            use vnx::Variant as V;
            Ok(match value {
                V::Null => InputValue::Nil,
                V::Bool(v) => InputValue::Bool(*v),
                V::Number(v) => v
                    .as_u64()
                    .map_or(InputValue::Unsupported, |v| InputValue::Uint(v.into())),
                V::String(v) => InputValue::String(v.as_bytes().to_vec()),
                V::Array(values) => {
                    if depth >= MAX_COPY_RECURSION {
                        return Ok(InputValue::Array(Vec::new()));
                    }
                    InputValue::Array(
                        values
                            .iter()
                            .map(|v| convert(v, depth + 1))
                            .collect::<Result<_>>()?,
                    )
                }
                V::Object(values) => {
                    if depth >= MAX_COPY_RECURSION {
                        return Ok(InputValue::Map(Vec::new()));
                    }
                    InputValue::Map(
                        values
                            .iter()
                            .map(|(k, v)| {
                                Ok((
                                    InputValue::String(k.as_bytes().to_vec()),
                                    convert(v, depth + 1)?,
                                ))
                            })
                            .collect::<Result<_>>()?,
                    )
                }
            })
        }
        convert(value, 1)
    }
}

#[derive(Clone, Copy)]
enum Target {
    Address(u64),
    Entry(u64, u64),
    Key,
}

impl<S: Storage> Engine<'_, S> {
    /// Native vm::read(address).to<addr_t>(), used by NFT reward resolution.
    /// This is VNX conversion: invalid strings become zero, binary/list results
    /// truncate or zero-pad, and small integers become the first byte. Traverse
    /// every collection element before conversion, preserving read gas/errors.
    pub fn read_address_result(&mut self, address: u64) -> Result<Address> {
        use crate::value::{FLAG_DELETED, FLAG_STORED};
        #[derive(Clone, Copy)]
        enum Destination {
            Address,
            Byte(usize),
            Discard,
        }
        enum Read {
            Memory(u64),
            Entry(u64, u64),
            Value(Var),
        }
        use Destination::*;
        let mut out = [0; 32];
        let mut pending = vec![(Read::Memory(address), Address)];
        while let Some((read, destination)) = pending.pop() {
            let var = match read {
                Read::Memory(a) => self.read_var(a)?,
                Read::Entry(a, key) => self.read_entry_var(a, key)?,
                Read::Value(v) => Some(v),
            };
            let Some(var) = var else { continue };
            let scalar = matches!(var.value, Value::Bool(_))
                || matches!(var.value, Value::Uint(v) if v >> 64 == U256::zero());
            let byte = match var.value {
                Value::Ref(a) => {
                    pending.push((Read::Memory(a), destination));
                    continue;
                }
                Value::Bool(v) => u8::from(v),
                Value::Uint(v) if v >> 64 == U256::zero() => v.low_u32() as u8,
                Value::Uint(v) => {
                    // Arbitrary HRPs are accepted: even a native hexadecimal
                    // integer string can be a valid Bech32m address.
                    if matches!(destination, Address) {
                        out = crate::address::decode(format!("0x{v:x}").as_bytes())
                            .unwrap_or([0; 32]);
                    }
                    0
                }
                Value::String(v) => {
                    if matches!(destination, Address) {
                        out = crate::address::decode(&v).unwrap_or([0; 32]);
                    }
                    0
                }
                Value::Binary(v) => {
                    if matches!(destination, Address) {
                        let n = v.len().min(out.len());
                        out[..n].copy_from_slice(&v[..n]);
                    }
                    0
                }
                Value::Array { address, size } => {
                    for i in (0..size).rev() {
                        let dst = if matches!(destination, Address) && i < 32 {
                            Byte(i as usize)
                        } else {
                            Discard
                        };
                        pending.push((Read::Entry(address, i as u64), dst));
                    }
                    continue;
                }
                Value::Map { address } => {
                    if var.flags & FLAG_STORED != 0 {
                        return Err(Error::State("cannot read map from storage"));
                    }
                    for ((_, key), value) in
                        self.entries.range((address, 0)..=(address, u64::MAX)).rev()
                    {
                        if value.flags & FLAG_DELETED == 0 {
                            pending.push((Read::Value(value.clone()), Discard));
                            pending.push((Read::Memory(*key), Discard));
                        }
                    }
                    0
                }
                _ => 0,
            };
            match destination {
                Address if scalar => out[0] = byte,
                Byte(i) => out[i] = byte,
                _ => {}
            }
        }
        Ok(out)
    }

    /// Load native constants in order, then decode code, initialize map keys,
    /// and check the gas meter. Bytecode trailing bytes follow native decoding.
    pub fn load_binary(&mut self, binary: &Binary) -> Result<Vec<Instruction>> {
        self.load_binary_parts(&binary.constant, &binary.binary)
    }

    pub fn load_binary_parts(
        &mut self,
        constants: &[u8],
        bytecode: &[u8],
    ) -> Result<Vec<Instruction>> {
        let result = (|| {
            let mut offset = 0;
            let mut dst = 0;
            while offset < constants.len() {
                let (var, size) = Var::decode(&constants[offset..], false, false)?;
                offset += size;
                if dst < MEM_EXTERN {
                    self.assign(dst, var.value)?;
                    dst += 1;
                }
            }
            if dst >= MEM_EXTERN {
                return Err(Error::OutOfMemory);
            }
            let (code, _) = crate::instruction::decode(bytecode)?;
            self.init()?;
            self.check_gas()?;
            Ok(code)
        })();
        self.failed |= result.is_err();
        result
    }

    pub fn assign_input(&mut self, dst: u64, input: &InputValue) -> Result<()> {
        if self.failed {
            return Err(Error::State("execution failed"));
        }
        let result = self
            .assign_input_at(Target::Address(dst), input, 1)
            .map(|_| ());
        self.failed |= result.is_err();
        result
    }

    fn assign_input_at(
        &mut self,
        target: Target,
        input: &InputValue,
        depth: usize,
    ) -> Result<Option<u64>> {
        let value = match input {
            InputValue::Unsupported => return Err(Error::Input),
            InputValue::Nil => Value::Nil,
            InputValue::Bool(v) => Value::Bool(*v),
            InputValue::Uint(v) => Value::Uint(*v),
            InputValue::String(v) => Value::String(v.clone()),
            InputValue::Binary(v) => Value::Binary(v.clone()),
            InputValue::Array(values) => {
                if depth >= MAX_COPY_RECURSION {
                    return Err(Error::CopyRecursion);
                }
                let address = self.alloc()?;
                self.assign(
                    address,
                    Value::Array {
                        address: 0,
                        size: values.len().try_into().map_err(|_| Error::ValueSize)?,
                    },
                )?;
                for (i, value) in values.iter().enumerate() {
                    self.assign_input_at(Target::Entry(address, i as u64), value, depth + 1)?;
                }
                Value::Ref(address)
            }
            InputValue::Map(values) => {
                if depth >= MAX_COPY_RECURSION {
                    return Err(Error::CopyRecursion);
                }
                let address = self.alloc()?;
                self.assign(address, Value::Map { address: 0 })?;
                for (key, value) in values {
                    let key = self.assign_input_at(Target::Key, key, depth + 1)?.unwrap();
                    self.assign_input_at(Target::Entry(address, key), value, depth + 1)?;
                }
                Value::Ref(address)
            }
        };
        match target {
            Target::Address(dst) => self.assign(dst, value)?,
            Target::Entry(dst, key) => self.write_entry(dst, key, value)?,
            Target::Key => {
                if !matches!(value, Value::Uint(_) | Value::String(_) | Value::Binary(_)) {
                    return Err(Error::InvalidKey);
                }
                return Ok(Some(self.lookup(&value, false)?.unwrap_or(0)));
            }
        }
        Ok(None)
    }

    pub fn set_args(&mut self, args: &[InputValue]) -> Result<()> {
        for (i, arg) in args.iter().enumerate() {
            let dst = MEM_STACK + 1 + i as u64;
            if dst >= MEM_STATIC {
                self.failed = true;
                return Err(Error::StackOverflow);
            }
            self.assign_input(dst, arg)?;
        }
        Ok(())
    }

    pub fn set_deposit(&mut self, currency: &Address, amount: u128) -> Result<()> {
        let address = MEM_EXTERN + crate::EXTERN_DEPOSIT;
        self.assign(
            address,
            Value::Array {
                address: 0,
                size: 0,
            },
        )?;
        self.push_back(address, Value::Binary(currency.to_vec()))?;
        self.push_back(address, Value::Uint(amount.into()))
    }
}

/// Deep boundary copy, unlike bytecode COPY's shallow collection references.
/// Native copies preserve traversal/gas order and reject stored maps, inline
/// collection entries, and references deeper than MAX_COPY_RECURSION.
pub fn copy_between<D: Storage, S: Storage>(
    dst: &mut Engine<'_, D>,
    src: &mut Engine<'_, S>,
    dst_addr: u64,
    src_addr: u64,
) -> Result<()> {
    let result = (|| {
        let value = src.read_var(src_addr)?;
        copy_var(dst, src, Target::Address(dst_addr), value, 0)
    })();
    dst.failed |= result.is_err();
    result
}

fn copy_var<D: Storage, S: Storage>(
    dst: &mut Engine<'_, D>,
    src: &mut Engine<'_, S>,
    target: Target,
    var: Option<Var>,
    depth: usize,
) -> Result<()> {
    let depth = depth + 1;
    if depth > MAX_COPY_RECURSION {
        return Err(Error::CopyRecursion);
    }
    if let Some(var) = var {
        let value = match var.value {
            Value::Ref(address) => {
                let heap = dst.alloc()?;
                let value = src.read_var(address)?;
                copy_var(dst, src, Target::Address(heap), value, depth)?;
                Some(Value::Ref(heap))
            }
            Value::Array { address, size } => {
                let Target::Address(to) = target else {
                    return Err(Error::Type("cannot assign array entry"));
                };
                dst.assign(to, Value::Array { address: 0, size })?;
                for key in 0..size as u64 {
                    let value = src.read_entry(address, key)?.map(Var::new);
                    copy_var(dst, src, Target::Entry(to, key), value, depth)?;
                }
                None
            }
            Value::Map { address } => {
                let Target::Address(to) = target else {
                    return Err(Error::Type("cannot assign map entry"));
                };
                if var.flags & FLAG_STORED != 0 {
                    return Err(Error::State("cannot copy map from storage"));
                }
                dst.assign(to, Value::Map { address: 0 })?;
                let entries: Vec<_> = src
                    .entries
                    .range((address, 0)..=(address, u64::MAX))
                    .filter(|(_, v)| v.flags & FLAG_DELETED == 0)
                    .map(|((_, key), v)| (*key, v.clone()))
                    .collect();
                for (key, value) in entries {
                    let key = src.read(key)?.unwrap_or(Value::Nil);
                    let key = dst.lookup(&key, false)?.unwrap_or(0);
                    copy_var(dst, src, Target::Entry(to, key), Some(value), depth)?;
                }
                None
            }
            value => Some(value),
        };
        if let Some(value) = value {
            match target {
                Target::Address(address) => dst.write(address, value)?,
                Target::Entry(address, key) => dst.write_entry(address, key, value)?,
                Target::Key => unreachable!(),
            }
        }
    }
    dst.check_gas()
}

impl<S: Storage> Engine<'_, S> {
    /// Native vm::read() boundary conversion. A returned map must be in memory;
    /// reading a stored map is a separate, explicitly bounded WebAPI operation.
    pub fn read_result(&mut self, address: u64, max_nodes: usize) -> Result<InputValue> {
        fn convert<S: Storage>(
            vm: &mut Engine<'_, S>,
            var: Option<Var>,
            left: &mut usize,
            depth: usize,
        ) -> Result<InputValue> {
            if depth >= MAX_COPY_RECURSION || *left == 0 {
                return Err(Error::State("VM result budget exceeded"));
            }
            *left -= 1;
            let Some(var) = var else {
                return Ok(InputValue::Nil);
            };
            Ok(match var.value {
                Value::Nil => InputValue::Nil,
                Value::Bool(v) => InputValue::Bool(v),
                Value::Uint(v) => InputValue::Uint(v),
                Value::String(v) => InputValue::String(v),
                Value::Binary(v) => InputValue::Binary(v),
                Value::Ref(a) => {
                    let value = vm.read_var(a)?;
                    convert(vm, value, left, depth + 1)?
                }
                Value::Array { address, size } => {
                    if size as usize > *left {
                        return Err(Error::State("VM result budget exceeded"));
                    }
                    let mut rows = Vec::new();
                    for i in 0..size {
                        let value = vm.read_entry_var(address, u64::from(i))?;
                        rows.push(convert(vm, value, left, depth + 1)?);
                    }
                    InputValue::Array(rows)
                }
                Value::Map { address } => {
                    if var.flags & crate::value::FLAG_STORED != 0 {
                        return Err(Error::State("cannot read map from storage"));
                    }
                    let entries: Vec<_> = vm
                        .entries
                        .range((address, 0)..=(address, u64::MAX))
                        .filter(|(_, v)| v.flags & crate::value::FLAG_DELETED == 0)
                        .take(*left + 1)
                        .map(|(k, v)| (k.1, v.clone()))
                        .collect();
                    if entries.len().saturating_mul(2) > *left {
                        return Err(Error::State("VM result budget exceeded"));
                    }
                    let mut rows = Vec::new();
                    for (key, value) in entries {
                        let key = vm.read_var(key)?;
                        rows.push((
                            convert(vm, key, left, depth + 1)?,
                            convert(vm, Some(value), left, depth + 1)?,
                        ));
                    }
                    InputValue::Map(rows)
                }
            })
        }
        let value = self.read_var(address)?;
        let mut remaining = max_nodes;
        convert(self, value, &mut remaining, 0)
    }
}

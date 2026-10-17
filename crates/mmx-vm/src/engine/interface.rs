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

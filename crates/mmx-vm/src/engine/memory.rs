//! VM ownership, entry storage and lazy persistence. These addresses and flags
//! are MMX semantics; Rust ownership alone cannot replace VM reference counts.
use super::*;
use crate::{instruction::HARD_FAIL, MAX_KEY_BYTES, MEM_HEAP};

const NEXT_ALLOC: u64 = MEM_HEAP + 1;
const DYNAMIC_START: u64 = MEM_HEAP + 0x1000;

#[derive(Clone, Copy)]
enum Cell {
    Memory(u64),
    Entry(u64, u64),
}

impl<S: Storage> Engine<'_, S> {
    fn guarded<T>(&mut self, action: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.failed {
            return Err(Error::State("execution failed"));
        }
        let result = action(self);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn cell(&self, cell: Cell) -> Option<&Var> {
        match cell {
            Cell::Memory(a) => self.memory.get(&a),
            Cell::Entry(a, k) => self.entries.get(&(a, k)),
        }
    }

    fn cell_mut(&mut self, cell: Cell) -> Option<&mut Var> {
        match cell {
            Cell::Memory(a) => self.memory.get_mut(&a),
            Cell::Entry(a, k) => self.entries.get_mut(&(a, k)),
        }
    }

    fn put_cell(&mut self, cell: Cell, var: Var) {
        match cell {
            Cell::Memory(a) => {
                self.memory.insert(a, var);
            }
            Cell::Entry(a, k) => {
                self.entries.insert((a, k), var);
            }
        }
    }

    fn read_only_error(cell: Cell) -> Error {
        match cell {
            Cell::Memory(a) | Cell::Entry(a, _) => Error::ReadOnly(a),
        }
    }

    fn stored_read(&mut self, address: u64, key: Option<u64>) -> Result<Option<Var>> {
        let mut var = self
            .backend
            .read(&self.contract, address, key)?
            .filter(|v| v.flags & FLAG_DELETED == 0);
        if let Some(var) = &mut var {
            var.flags = (var.flags & !FLAG_DIRTY) | FLAG_STORED;
            if self.read_only {
                var.flags |= FLAG_CONST;
            }
        }
        let size = var.as_ref().map_or(0, |v| v.value.num_bytes());
        self.charge(STORAGE_READ_COST + (size / 32) as u64 * STORAGE_READ_32_BYTE_COST)?;
        Ok(var)
    }

    pub(super) fn read_var(&mut self, address: u64) -> Result<Option<Var>> {
        if let Some(var) = self.memory.get(&address) {
            return Ok((var.flags & FLAG_DELETED == 0).then(|| var.clone()));
        }
        if address >= MEM_STATIC {
            if let Some(var) = self.stored_read(address, None)? {
                self.memory.insert(address, var.clone());
                return Ok(Some(var));
            }
        }
        Ok(None)
    }

    pub(super) fn load_var(&mut self, address: u64) -> Result<Var> {
        self.read_var(address)?.ok_or(Error::Missing(address))
    }

    pub fn read(&mut self, address: u64) -> Result<Option<Value>> {
        Ok(self.read_var(address)?.map(|var| var.value))
    }

    fn prepare_write(&mut self, address: u64) -> Result<()> {
        if self.initialized && address < MEM_EXTERN {
            return Err(Error::ReadOnly(address));
        }
        if !self.memory.contains_key(&address)
            && address >= MEM_STATIC
            && address < self.new_heap_base
        {
            if let Some(var) = self.stored_read(address, None)? {
                self.memory.insert(address, var);
            }
        }
        Ok(())
    }

    /// Copy a value, materializing collection entries at the destination.
    pub fn write(&mut self, address: u64, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        self.guarded(|engine| engine.write_value(address, value, false))
    }

    /// Create a descriptor directly instead of copying the source's entries.
    pub fn assign(&mut self, address: u64, mut value: Value) -> Result<()> {
        self.guarded(|engine| {
            engine.prepare_write(address)?;
            match &mut value {
                Value::Array { address: a, .. } | Value::Map { address: a } => *a = address,
                _ => (),
            }
            engine.assign_cell(Cell::Memory(address), value)
        })
    }

    pub(super) fn write_value(
        &mut self,
        address: u64,
        value: Value,
        force_assign: bool,
    ) -> Result<()> {
        self.prepare_write(address)?;
        if force_assign {
            self.assign_cell(Cell::Memory(address), value)
        } else {
            self.write_cell(Cell::Memory(address), Var::new(value))
        }
    }

    fn assign_cell(&mut self, cell: Cell, value: Value) -> Result<()> {
        let mut next = Var::new(value);
        if let Some(previous) = self.cell(cell).cloned() {
            if previous.flags & FLAG_CONST != 0 {
                return Err(Self::read_only_error(cell));
            }
            next.flags = previous.flags;
            next.ref_count = previous.ref_count;
            self.clear_value(&previous)?;
        }
        if next.value.num_bytes() > MAX_VALUE_BYTES {
            return Err(Error::ValueSize);
        }
        self.spend(WRITE_COST + next.value.num_bytes() as u64 * WRITE_32_BYTE_COST / 32);
        next.flags = (next.flags | FLAG_DIRTY) & !FLAG_DELETED;
        let reference = if let Value::Ref(a) = next.value {
            Some(a)
        } else {
            None
        };
        self.put_cell(cell, next);
        if let Some(address) = reference {
            self.addref(address);
        }
        Ok(())
    }

    fn write_cell(&mut self, cell: Cell, source: Var) -> Result<()> {
        if source.flags & FLAG_DELETED != 0 {
            return Err(Error::State("source has been deleted"));
        }
        if let Some(previous) = self.cell(cell).cloned() {
            if previous.flags & FLAG_CONST != 0 {
                return Err(Self::read_only_error(cell));
            }
            if previous.flags & FLAG_DELETED != 0 {
                self.cell_mut(cell).unwrap().flags = (previous.flags | FLAG_DIRTY) & !FLAG_DELETED;
            }
            let in_place = matches!(
                (&previous.value, &source.value),
                (Value::Uint(_), Value::Uint(_))
                    | (Value::Ref(_), Value::Ref(_))
                    | (Value::Nil | Value::Bool(_), Value::Nil | Value::Bool(_))
            );
            if in_place {
                if previous.value != source.value {
                    if let Value::Ref(address) = previous.value {
                        self.unref(address)?;
                    }
                    let var = self.cell_mut(cell).unwrap();
                    var.flags |= FLAG_DIRTY;
                    var.value = source.value.clone();
                    if let Value::Ref(address) = source.value {
                        self.addref(address);
                    }
                }
                return Ok(());
            }
        }
        match source.value {
            Value::Array { address, size } => {
                let Cell::Memory(dst) = cell else {
                    return Err(Error::State("cannot assign array to an entry"));
                };
                self.assign_cell(cell, Value::Array { address: dst, size })?;
                for key in 0..size as u64 {
                    let value = self.load_entry_var(address, key)?;
                    self.write_entry_var(dst, key, value)?;
                    self.check_gas()?;
                }
            }
            Value::Map { address } => {
                let Cell::Memory(dst) = cell else {
                    return Err(Error::State("cannot assign map to an entry"));
                };
                if source.flags & FLAG_STORED != 0 {
                    return Err(Error::State("cannot clone map from storage"));
                }
                self.assign_cell(cell, Value::Map { address: dst })?;
                let keys: Vec<_> = self
                    .entries
                    .range((address, 0)..=(address, u64::MAX))
                    .map(|(k, _)| k.1)
                    .collect();
                for key in keys {
                    if let Some(value) = self
                        .entries
                        .get(&(address, key))
                        .filter(|v| v.flags & FLAG_DELETED == 0)
                        .cloned()
                    {
                        self.write_entry_var(dst, key, value)?;
                        self.check_gas()?;
                    }
                }
            }
            value => self.assign_cell(cell, value)?,
        }
        Ok(())
    }

    pub fn copy(&mut self, dst: u64, src: u64) -> Result<()> {
        self.guarded(|engine| {
            if dst != src {
                let value = engine.load_var(src)?;
                engine.prepare_write(dst)?;
                engine.write_cell(Cell::Memory(dst), value)?;
            }
            Ok(())
        })
    }

    /// OP_CLONE allocates one heap cell and returns a reference to its copy.
    /// References inside collections retain their aliasing; this is not a
    /// recursive deep clone.
    pub fn clone_value(&mut self, dst: u64, src: u64) -> Result<()> {
        self.guarded(|engine| {
            let address = engine.alloc()?;
            let value = engine.load_var(src)?;
            engine.prepare_write(address)?;
            engine.write_cell(Cell::Memory(address), value)?;
            engine.write_value(dst, Value::Ref(address), false)
        })
    }

    fn addref(&mut self, address: u64) {
        if let Some(var) = self
            .memory
            .get_mut(&address)
            .filter(|v| v.flags & (FLAG_STORED | FLAG_DELETED) == 0)
        {
            var.flags |= FLAG_DIRTY;
            var.ref_count = var.ref_count.wrapping_add(1);
        }
    }

    fn unref(&mut self, address: u64) -> Result<()> {
        let mut erase = false;
        if let Some(var) = self
            .memory
            .get_mut(&address)
            .filter(|v| v.flags & (FLAG_STORED | FLAG_DELETED) == 0)
        {
            if var.ref_count == 0 {
                return Err(Error::State("reference count underflow"));
            }
            var.flags |= FLAG_DIRTY;
            var.ref_count -= 1;
            erase = var.ref_count == 0;
        }
        if erase {
            self.erase_cell(Cell::Memory(address))?;
        }
        Ok(())
    }

    pub fn alloc(&mut self) -> Result<u64> {
        self.guarded(|engine| {
            if engine.read_var(NEXT_ALLOC)?.is_none() {
                engine.new_heap_base = MEM_HEAP;
                engine.assign(NEXT_ALLOC, Value::from(DYNAMIC_START))?;
                let var = engine.memory.get_mut(&NEXT_ALLOC).unwrap();
                if var.flags & FLAG_STORED == 0 && var.ref_count == 0 {
                    var.ref_count = 1;
                    var.flags |= FLAG_DIRTY;
                }
            }
            let value = engine.memory[&NEXT_ALLOC]
                .value
                .as_uint()
                .ok_or(Error::Type("uint allocation counter"))?;
            if value >= U256::from(u64::MAX) {
                return Err(Error::OutOfMemory);
            }
            let address = value.low_u64();
            if engine.first_alloc {
                engine.first_alloc = false;
                engine.new_heap_base = address;
            }
            let var = engine.memory.get_mut(&NEXT_ALLOC).unwrap();
            var.flags |= FLAG_DIRTY;
            var.value = Value::Uint(value + 1);
            Ok(address)
        })
    }

    pub fn lookup(&mut self, value: &Value, read_only: bool) -> Result<Option<u64>> {
        if matches!(value, Value::Nil) || value.num_bytes() > MAX_KEY_BYTES {
            return Err(Error::InvalidKey);
        }
        if let Some(address) = self.key_map.get(value) {
            return Ok(Some(*address));
        }
        self.charge(
            STORAGE_READ_COST + (value.num_bytes() / 32) as u64 * STORAGE_READ_32_BYTE_COST,
        )?;
        if let Some(address) = self.backend.lookup(&self.contract, value)? {
            let var = self.load_var(address)?;
            if var.flags & (FLAG_CONST | FLAG_KEY) != FLAG_CONST | FLAG_KEY {
                return Err(Error::State("key missing CONST / KEY flags"));
            }
            self.key_map.insert(var.value, address);
            return Ok(Some(address));
        }
        if read_only {
            return Ok(None);
        }
        let address = self.alloc()?;
        self.write_value(address, value.clone(), false)?;
        self.memory.get_mut(&address).unwrap().flags |= FLAG_CONST | FLAG_KEY;
        self.addref(address);
        self.key_map
            .insert(self.memory[&address].value.clone(), address);
        Ok(Some(address))
    }

    fn read_entry_var(&mut self, address: u64, key: u64) -> Result<Option<Var>> {
        if let Some(var) = self.entries.get(&(address, key)) {
            return Ok((var.flags & FLAG_DELETED == 0).then(|| var.clone()));
        }
        if address >= MEM_STATIC {
            if let Some(var) = self.stored_read(address, Some(key))? {
                self.entries.insert((address, key), var.clone());
                return Ok(Some(var));
            }
        }
        Ok(None)
    }

    fn load_entry_var(&mut self, address: u64, key: u64) -> Result<Var> {
        self.read_entry_var(address, key)?
            .ok_or(Error::State("entry read failed"))
    }

    pub fn read_entry(&mut self, address: u64, key: u64) -> Result<Option<Value>> {
        Ok(self.read_entry_var(address, key)?.map(|var| var.value))
    }

    pub fn read_key(&mut self, address: u64, key: &Value) -> Result<Option<Value>> {
        if let Some(key) = self.lookup(key, true)? {
            self.read_entry(address, key)
        } else {
            Ok(None)
        }
    }

    fn write_entry_var(&mut self, address: u64, key: u64, value: Var) -> Result<()> {
        if self.initialized && address < MEM_EXTERN {
            return Err(Error::ReadOnly(address));
        }
        let container = self.load_var(address)?;
        if container.flags & FLAG_CONST != 0 {
            return Err(Error::ReadOnly(address));
        }
        // Unlike read_entry(), writing does not first load a stored entry.
        self.write_cell(Cell::Entry(address, key), value)
    }

    pub fn write_entry(&mut self, address: u64, key: u64, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        self.guarded(|engine| engine.write_entry_var(address, key, Var::new(value)))
    }

    pub fn write_key(&mut self, address: u64, key: &Value, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        self.guarded(|engine| {
            let key = engine.lookup(key, false)?.ok_or(Error::InvalidKey)?;
            engine.write_entry_var(address, key, Var::new(value))
        })
    }

    pub fn push_back(&mut self, address: u64, value: impl Into<Value>) -> Result<()> {
        let value = value.into();
        self.guarded(|engine| engine.push_var(address, Var::new(value)))
    }

    pub(super) fn push_var(&mut self, address: u64, value: Var) -> Result<()> {
        let Value::Array { size, .. } = self.load(address)? else {
            return Err(Error::Type("array"));
        };
        if size == u32::MAX {
            return Err(Error::Bounds);
        }
        if let Value::Array { size, .. } = &mut self.memory.get_mut(&address).unwrap().value {
            *size += 1;
        }
        self.write_entry_var(address, size as u64, value)?;
        self.memory.get_mut(&address).unwrap().flags |= FLAG_DIRTY;
        Ok(())
    }

    pub fn pop_back(&mut self, dst: u64, src: u64) -> Result<()> {
        self.guarded(|engine| {
            if dst == src {
                return Err(Error::State("pop_back destination equals source"));
            }
            let Value::Array { size, .. } = engine.load(src)? else {
                return Err(Error::Type("array"));
            };
            if size == 0 {
                return Err(Error::Bounds);
            }
            let key = size as u64 - 1;
            let value = engine.load_entry_var(src, key)?;
            engine.prepare_write(dst)?;
            engine.write_cell(Cell::Memory(dst), value)?;
            engine.erase_entry(src, key)?;
            let var = engine.memory.get_mut(&src).unwrap();
            if let Value::Array { size, .. } = &mut var.value {
                *size -= 1;
            }
            var.flags |= FLAG_DIRTY;
            Ok(())
        })
    }

    pub fn erase_entry(&mut self, address: u64, key: u64) -> Result<()> {
        self.guarded(|engine| {
            let cell = Cell::Entry(address, key);
            if engine.cell(cell).is_none()
                && address >= MEM_STATIC
                && engine.load_var(address)?.flags & FLAG_STORED != 0
            {
                if let Some(var) = engine.stored_read(address, Some(key))? {
                    engine.put_cell(cell, var);
                }
            }
            engine.erase_cell(cell)
        })
    }

    pub fn erase_key(&mut self, address: u64, key: &Value) -> Result<()> {
        self.guarded(|engine| {
            if let Some(key) = engine.lookup(key, true)? {
                engine.erase_entry(address, key)?;
            }
            Ok(())
        })
    }

    pub fn erase(&mut self, address: u64) -> Result<()> {
        self.guarded(|engine| {
            if !engine.memory.contains_key(&address) && address >= MEM_STATIC {
                if let Some(var) = engine.stored_read(address, None)? {
                    engine.memory.insert(address, var);
                }
            }
            engine.erase_cell(Cell::Memory(address))
        })
    }

    fn erase_cell(&mut self, cell: Cell) -> Result<()> {
        let Some(previous) = self.cell(cell).cloned() else {
            return Ok(());
        };
        if previous.flags & FLAG_CONST != 0 {
            return Err(Self::read_only_error(cell));
        }
        if previous.flags & FLAG_DELETED != 0 {
            return Ok(());
        }
        if previous.ref_count != 0 {
            return Err(Error::State("cannot erase a referenced cell"));
        }
        self.erase_depth += 1;
        if self.erase_depth > MAX_ERASE_RECURSION {
            return Err(Error::EraseRecursion);
        }
        self.charge(WRITE_COST)?;
        self.clear_value(&previous)?;
        let flags = if previous.flags & FLAG_STORED != 0 {
            previous.flags | FLAG_DIRTY | FLAG_DELETED
        } else {
            FLAG_DELETED
        };
        self.put_cell(
            cell,
            Var {
                value: Value::Nil,
                flags,
                ref_count: 0,
            },
        );
        self.erase_depth -= 1;
        Ok(())
    }

    fn clear_value(&mut self, var: &Var) -> Result<()> {
        match var.value {
            Value::Ref(address) => self.unref(address)?,
            Value::Array { address, .. } | Value::Map { address } => {
                if var.flags & FLAG_STORED != 0 {
                    return Err(Error::State("cannot erase collection in storage"));
                }
                let keys: Vec<_> = self
                    .entries
                    .range((address, 0)..=(address, u64::MAX))
                    .map(|(key, _)| key.1)
                    .collect();
                for key in keys {
                    self.erase_cell(Cell::Entry(address, key))?;
                    self.check_gas()?;
                }
            }
            _ => (),
        }
        Ok(())
    }

    pub(super) fn clear_stack(&mut self, offset: u64) -> Result<()> {
        let start = MEM_STACK + offset;
        if start >= MEM_STATIC {
            return Ok(());
        }
        let keys: Vec<_> = self
            .memory
            .range(start..MEM_STATIC)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            let var = self.memory[&key].clone();
            self.clear_value(&var)?;
            self.memory.remove(&key);
            self.check_gas()?;
        }
        Ok(())
    }

    pub(super) fn get(&mut self, dst: u64, address: u64, key: u64, flags: u8) -> Result<()> {
        let value = match self.load(address)? {
            Value::Array { size, .. } => {
                let index = self.uint(key)?;
                if index < U256::from(size) {
                    self.load_entry_var(address, index.low_u64())?
                } else if flags & HARD_FAIL != 0 {
                    return Err(Error::Bounds);
                } else {
                    Var::new(Value::Nil)
                }
            }
            Value::Map { .. } => {
                let key = self.load(key)?;
                if let Some(key) = self.lookup(&key, true)? {
                    self.read_entry_var(address, key)?
                        .unwrap_or_else(|| Var::new(Value::Nil))
                } else {
                    Var::new(Value::Nil)
                }
            }
            _ if flags & HARD_FAIL != 0 => return Err(Error::Type("array or map")),
            _ => Var::new(Value::Nil),
        };
        self.prepare_write(dst)?;
        self.write_cell(Cell::Memory(dst), value)
    }

    pub(super) fn set(&mut self, address: u64, key: u64, src: u64) -> Result<()> {
        let (key, value) = match self.load(address)? {
            Value::Array { size, .. } => {
                let index = self.uint(key)?;
                if index >= U256::from(size) {
                    return Err(Error::Bounds);
                }
                (index.low_u64(), self.load_var(src)?)
            }
            Value::Map { .. } => {
                // C++ evaluates read_fail(src) before entering write_key(),
                // whose body resolves the map key. The read order matters at
                // gas boundaries when either value must be loaded from disk.
                let value = self.load_var(src)?;
                let key = self.load(key)?;
                (self.lookup(&key, false)?.ok_or(Error::InvalidKey)?, value)
            }
            _ => return Err(Error::Type("array or map")),
        };
        self.write_entry_var(address, key, value)
    }

    pub(super) fn concat_arrays(&mut self, dst: u64, lhs: u64, rhs: u64) -> Result<()> {
        let Value::Array {
            address: left,
            size: lsize,
        } = self.load(lhs)?
        else {
            return Err(Error::Type("array"));
        };
        let Value::Array {
            address: right,
            size: rsize,
        } = self.load(rhs)?
        else {
            return Err(Error::Type("array"));
        };
        if dst == lhs || dst == rhs {
            return Err(Error::State("concat destination aliases an input"));
        }
        let address = self.alloc()?;
        self.assign(
            address,
            Value::Array {
                address: 0,
                size: 0,
            },
        )?;
        for (source, size) in [(left, lsize), (right, rsize)] {
            for key in 0..size as u64 {
                let value = self.load_entry_var(source, key)?;
                self.push_var(address, value)?;
                self.check_gas()?;
            }
        }
        self.write_value(dst, Value::Ref(address), false)
    }

    pub(super) fn commit_memory(&mut self) -> Result<()> {
        self.clear_stack(0)?;
        let mut writes: Vec<_> = self
            .memory
            .range(MEM_STATIC..)
            .filter(|(_, v)| v.flags & FLAG_DIRTY != 0)
            .map(|(address, var)| StorageWrite {
                address: *address,
                key: None,
                var: var.clone(),
            })
            .collect();
        writes.extend(
            self.entries
                .range((MEM_STATIC, 0)..)
                .filter(|(_, v)| v.flags & FLAG_DIRTY != 0)
                .map(|((address, key), var)| StorageWrite {
                    address: *address,
                    key: Some(*key),
                    var: var.clone(),
                }),
        );
        for write in &writes {
            if self.read_only {
                return Err(Error::ReadOnly(write.address));
            }
            if let Value::Ref(address) = write.var.value {
                if address < MEM_STATIC {
                    return Err(Error::State("cannot store reference to temporary memory"));
                }
            }
            if write.key.is_some() && write.var.ref_count != 0 {
                return Err(Error::State("entry reference count is nonzero"));
            }
            if write.var.flags & FLAG_KEY != 0 && write.var.flags & FLAG_CONST == 0 {
                return Err(Error::State("key is not const"));
            }
            let factor = if write.key.is_none() && write.var.flags & FLAG_KEY != 0 {
                2
            } else {
                1
            };
            self.charge(
                (STORAGE_WRITE_COST + write.var.value.num_bytes() as u64 * STORAGE_WRITE_BYTE_COST)
                    * factor,
            )?;
        }
        self.check_gas()?;
        self.backend.apply(&self.contract, &writes)?;
        for write in writes {
            let cell = if let Some(key) = write.key {
                Cell::Entry(write.address, key)
            } else {
                Cell::Memory(write.address)
            };
            self.cell_mut(cell).unwrap().flags &= !FLAG_DIRTY;
        }
        Ok(())
    }
}

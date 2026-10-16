//! Contract storage tables and key layouts from StorageDB.cpp.

use crate::{
    value::{FLAG_CONST, FLAG_KEY},
    Address, CodecError, Value, Var, MAX_KEY_BYTES, MEM_STATIC,
};
use mmx_db::{Database, Write};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] mmx_db::Error),
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("invalid storage value: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct StorageWrite {
    pub address: u64,
    pub key: Option<u64>,
    pub var: Var,
}

pub trait Storage {
    fn read(&self, contract: &Address, address: u64, key: Option<u64>) -> Result<Option<Var>>;
    fn lookup(&self, contract: &Address, value: &Value) -> Result<Option<u64>>;
    /// Implementations must apply the entire batch or leave storage unchanged.
    fn apply(&mut self, contract: &Address, writes: &[StorageWrite]) -> Result<()>;
}

/// The RAM and disk backends share table semantics and validation.
pub struct StorageDb {
    database: Database,
}

impl StorageDb {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
    pub fn memory() -> Self {
        Self::new(Database::memory())
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::new(Database::open(path)?))
    }
    pub fn database(&self) -> &Database {
        &self.database
    }
    pub fn database_mut(&mut self) -> &mut Database {
        &mut self.database
    }

    pub fn read_at(
        &self,
        contract: &Address,
        address: u64,
        key: Option<u64>,
        height: Option<u32>,
    ) -> Result<Option<Var>> {
        let (table, key) = data_key(contract, address, key);
        self.database
            .get(table, &key, height)
            .map(decode_var)
            .transpose()
    }

    pub fn find_range(
        &self,
        contract: &Address,
        begin: u64,
        end: u64,
        height: Option<u32>,
    ) -> Result<Vec<(u64, Var)>> {
        self.database
            .scan("storage", &contract[..20], height)
            .into_iter()
            .filter_map(|row| {
                if row.key.len() != 28 {
                    return Some(Err(Error::Invalid("cell key length")));
                }
                let address = u64::from_be_bytes(row.key[20..].try_into().unwrap());
                (address >= begin && address < end).then(|| Ok((address, decode_var(&row.value)?)))
            })
            .collect()
    }

    pub fn entries(
        &self,
        contract: &Address,
        address: u64,
        height: Option<u32>,
    ) -> Result<Vec<(u64, Var)>> {
        let (_, prefix) = data_key(contract, address, None);
        self.database
            .scan("storage_entries", &prefix, height)
            .into_iter()
            .map(|row| {
                if row.key.len() != 36 {
                    return Err(Error::Invalid("entry key length"));
                }
                let key = u64::from_be_bytes(row.key[28..].try_into().unwrap());
                Ok((key, decode_var(&row.value)?))
            })
            .collect()
    }
}

impl Storage for StorageDb {
    fn read(&self, contract: &Address, address: u64, key: Option<u64>) -> Result<Option<Var>> {
        self.read_at(contract, address, key, None)
    }

    fn lookup(&self, contract: &Address, value: &Value) -> Result<Option<u64>> {
        let key = index_key(contract, value)?;
        self.database
            .get("storage_index", &key, None)
            .map(|data| {
                if data.len() != 8 {
                    return Err(Error::Invalid("index address length"));
                }
                Ok(u64::from_le_bytes(data.try_into().unwrap()))
            })
            .transpose()
    }

    fn apply(&mut self, contract: &Address, writes: &[StorageWrite]) -> Result<()> {
        let mut batch = Vec::new();
        for write in writes {
            let var = &write.var;
            if let Value::Ref(address) = &var.value {
                if *address < MEM_STATIC {
                    return Err(Error::Invalid("reference to temporary memory"));
                }
            }
            if write.key.is_some() && var.ref_count != 0 {
                return Err(Error::Invalid("entry reference count"));
            }
            let (table, key) = data_key(contract, write.address, write.key);
            if write.key.is_none() && var.flags & FLAG_KEY != 0 {
                if var.flags & FLAG_CONST == 0
                    || matches!(var.value, Value::Nil)
                    || var.value.num_bytes() > MAX_KEY_BYTES
                {
                    return Err(Error::Invalid(
                        "key must be non-nil, const, and at most 4096 bytes",
                    ));
                }
                batch.push(Write::put(
                    "storage_index",
                    index_key(contract, &var.value)?,
                    write.address.to_le_bytes(),
                ));
            }
            batch.push(Write::put(table, key, var.encode(false, true)?));
        }
        self.database.apply(batch)?;
        Ok(())
    }
}

pub fn data_key(contract: &Address, address: u64, entry: Option<u64>) -> (&'static str, Vec<u8>) {
    // C++ stores the first 20 contract bytes in cell and entry keys; the full
    // 32-byte address is used in the value-to-address index.
    let mut key = contract[..20].to_vec();
    key.extend(address.to_be_bytes());
    if let Some(entry) = entry {
        key.extend(entry.to_be_bytes());
        ("storage_entries", key)
    } else {
        ("storage", key)
    }
}

fn index_key(contract: &Address, value: &Value) -> Result<Vec<u8>> {
    let mut key = contract.to_vec();
    key.extend(Var::new(value.clone()).encode(false, false)?);
    Ok(key)
}

fn decode_var(data: &[u8]) -> Result<Var> {
    let (var, consumed) = Var::decode(data, false, true)?;
    if consumed != data.len() {
        return Err(Error::Invalid("trailing value data"));
    }
    Ok(var)
}

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

/// Self-describing VNX contract bytes and the chain index's variable read cost.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractData {
    pub bytes: Vec<u8>,
    pub read_cost: u64,
}

#[derive(Clone, Debug)]
pub struct BalanceWrite {
    pub contract: Address,
    pub currency: Address,
    pub amount: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionIndex {
    pub height: u32,
    pub contract_read_cost: u64,
}

#[derive(Clone, Debug, Default)]
pub struct TransactionBatch {
    pub writes: Vec<(Address, StorageWrite)>,
    pub balances: Vec<BalanceWrite>,
    pub contracts: Vec<(Address, ContractData)>,
    pub transactions: Vec<(Address, TransactionIndex)>,
    pub supplies: Vec<(Address, u128)>,
}

pub trait AtomicStorage: Storage {
    /// All state, contracts, balances, indexes and supply counters publish
    /// together, or none of them do. Every batch field must be applied.
    fn apply_transaction(&mut self, batch: &TransactionBatch) -> Result<()>;
}

/// Opt-in for full transaction validation. Implementations must supply chain
/// balances/contracts, override transaction-index queries, and persist indexes
/// in the same atomic batch. VM-only backends need not implement this trait.
pub trait LedgerStorage: AtomicStorage {}

/// Block settlement additionally reads and atomically publishes total supply.
pub trait BlockStorage: LedgerStorage {}

pub trait Storage {
    fn get_total_supply(&self, _currency: &Address) -> Result<Option<u128>> {
        Err(Error::Invalid("supply queries unsupported by this backend"))
    }
    fn get_transaction_index(&self, _id: &Address) -> Result<Option<TransactionIndex>> {
        Ok(None)
    }
    fn read(&self, contract: &Address, address: u64, key: Option<u64>) -> Result<Option<Var>>;
    fn lookup(&self, contract: &Address, value: &Value) -> Result<Option<u64>>;
    fn get_contract(&self, _address: &Address) -> Result<Option<ContractData>> {
        Err(Error::Invalid(
            "contract queries unsupported by this backend",
        ))
    }
    /// Ledger snapshot supplied by the transaction executor. VM SEND debits
    /// its private cache; publishing transfers belongs to that executor.
    fn get_balance(&self, _contract: &Address, _currency: &Address) -> Result<Option<u128>> {
        Err(Error::Invalid(
            "balance queries unsupported by this backend",
        ))
    }
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

    pub fn set_balance(
        &mut self,
        contract: &Address,
        currency: &Address,
        amount: u128,
    ) -> Result<()> {
        self.database.insert(
            "balances",
            &balance_key(contract, currency),
            &amount.to_le_bytes(),
        )?;
        Ok(())
    }

    pub fn balance_at(
        &self,
        contract: &Address,
        currency: &Address,
        height: Option<u32>,
    ) -> Result<Option<u128>> {
        self.database
            .get("balances", &balance_key(contract, currency), height)
            .map(|bytes| {
                Ok(u128::from_le_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| Error::Invalid("balance length"))?,
                ))
            })
            .transpose()
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
    fn get_total_supply(&self, currency: &Address) -> Result<Option<u128>> {
        self.database
            .get("total_supply", currency, None)
            .map(|bytes| {
                Ok(u128::from_le_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| Error::Invalid("supply length"))?,
                ))
            })
            .transpose()
    }
    fn get_transaction_index(&self, id: &Address) -> Result<Option<TransactionIndex>> {
        self.database
            .get("transactions", id, None)
            .map(|bytes| {
                if bytes.len() != 12 {
                    return Err(Error::Invalid("transaction index length"));
                }
                Ok(TransactionIndex {
                    height: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    contract_read_cost: u64::from_le_bytes(bytes[4..].try_into().unwrap()),
                })
            })
            .transpose()
    }
    fn get_balance(&self, contract: &Address, currency: &Address) -> Result<Option<u128>> {
        self.balance_at(contract, currency, None)
    }
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
        self.apply_transaction(&TransactionBatch {
            writes: writes
                .iter()
                .cloned()
                .map(|write| (*contract, write))
                .collect(),
            ..Default::default()
        })
    }

    fn get_contract(&self, address: &Address) -> Result<Option<ContractData>> {
        self.database
            .get("contracts", address, None)
            .map(|bytes| {
                if bytes.len() < 8 {
                    return Err(Error::Invalid("contract record length"));
                }
                Ok(ContractData {
                    read_cost: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                    bytes: bytes[8..].to_vec(),
                })
            })
            .transpose()
    }
}

impl AtomicStorage for StorageDb {
    fn apply_transaction(&mut self, transaction: &TransactionBatch) -> Result<()> {
        let batch = self.transaction_writes(transaction)?;
        self.database.apply(batch)?;
        Ok(())
    }
}
impl StorageDb {
    /// Commit ledger effects and chain metadata in a single durable DB frame.
    /// Additional writes use the caller's own tables; this method does not
    /// advance visibility or stage any effects before the journal succeeds.
    pub fn commit_transaction(
        &mut self,
        transaction: &TransactionBatch,
        additional: impl IntoIterator<Item = Write>,
        next_version: u32,
    ) -> Result<()> {
        let mut writes = self.transaction_writes(transaction)?;
        writes.extend(additional);
        self.database.commit_batch(next_version, writes)?;
        Ok(())
    }
    fn transaction_writes(&self, transaction: &TransactionBatch) -> Result<Vec<Write>> {
        let mut batch = Vec::new();
        for (contract, write) in &transaction.writes {
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
        for balance in &transaction.balances {
            batch.push(Write::put(
                "balances",
                balance_key(&balance.contract, &balance.currency),
                balance.amount.to_le_bytes(),
            ));
        }
        for (address, contract) in &transaction.contracts {
            let mut bytes = contract.read_cost.to_le_bytes().to_vec();
            bytes.extend(&contract.bytes);
            batch.push(Write::put("contracts", address, bytes));
        }
        for (id, index) in &transaction.transactions {
            let mut bytes = index.height.to_le_bytes().to_vec();
            bytes.extend(index.contract_read_cost.to_le_bytes());
            batch.push(Write::put("transactions", id, bytes));
        }
        for (currency, amount) in &transaction.supplies {
            batch.push(Write::put("total_supply", currency, amount.to_le_bytes()));
        }
        Ok(batch)
    }
}
impl LedgerStorage for StorageDb {}
impl BlockStorage for StorageDb {}

fn balance_key(contract: &Address, currency: &Address) -> Vec<u8> {
    contract.iter().chain(currency).copied().collect()
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

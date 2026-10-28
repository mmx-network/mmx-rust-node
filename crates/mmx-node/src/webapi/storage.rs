//! Pinned, bounded contract reads for the existing public WebAPI.
use super::{render, Request};
use crate::{error, rpc, Result};
use mmx_execution::{BinaryInfo, Contract};
use mmx_vm::{Address, Engine, InputValue, Storage, StorageDb, Value as Vm, Var};
use std::collections::BTreeMap;
use vnx::serde_json::{json, Value};

pub(super) struct View {
    pub storage: StorageDb,
    pub height: u32,
    pub budget: mmx_db::ReadBudget,
    end: std::time::Instant,
    floor: u32,
    _permit: rpc::ReadPermit,
}
impl View {
    pub fn capture(request: &Request<'_>) -> Result<Self> {
        let node = rpc::lock_node_until(request.node, request.end)?;
        let permit = rpc::ReadPermit::acquire(&node.rpc_workers)?;
        let db = node.chain.selected().storage().database();
        Ok(Self {
            storage: StorageDb::new(db.fork(db.version())?),
            height: db.version().saturating_sub(1),
            end: request.end,
            floor: node.earliest_height(),
            budget: mmx_db::ReadBudget {
                rows: 10000,
                keys: 100000,
                bytes: 32 * 1024 * 1024,
            },
            _permit: permit,
        })
    }
    pub fn program(&self, id: Address) -> Result<(BinaryInfo, BTreeMap<String, u64>)> {
        self.optional_program(id)?
            .ok_or_else(|| error("no such executable or binary"))
    }
    pub fn optional_program(
        &self,
        id: Address,
    ) -> Result<Option<(BinaryInfo, BTreeMap<String, u64>)>> {
        let Some(executable) = self.storage.get_contract(&id)? else {
            return Ok(None);
        };
        let executable = Contract::decode(executable)?;
        if executable.type_name() != "mmx.contract.Executable" {
            return Ok(None);
        }
        let Some(binary) = self
            .storage
            .get_contract(&executable.executable()?.binary)?
        else {
            return Ok(None);
        };
        if Contract::decode(binary.clone())?.type_name() != "mmx.contract.Binary" {
            return Ok(None);
        }
        let native = rpc::to_json_record(&mmx_consensus::Record::decode(&binary.bytes)?)?;
        let mut fields = BTreeMap::new();
        for pair in native["fields"].as_array().into_iter().flatten() {
            fields.insert(
                pair[0]
                    .as_str()
                    .ok_or_else(|| error("invalid field"))?
                    .into(),
                pair[1]
                    .as_u64()
                    .ok_or_else(|| error("invalid field offset"))?,
            );
        }
        Ok(Some((Contract::decode(binary)?.binary()?, fields)))
    }
    pub fn field(&mut self, id: Address, name: &str) -> Result<(Option<Vm>, u64)> {
        let Some((_, fields)) = self.optional_program(id)? else {
            return Ok((None, 0));
        };
        let Some(offset) = fields.get(name) else {
            return Ok((None, 0));
        };
        Ok((
            self.storage.read(&id, *offset, None)?.map(|v| v.value),
            *offset,
        ))
    }
    pub fn data(&mut self, id: Address) -> Result<BTreeMap<String, Value>> {
        let Some((_, fields)) = self.optional_program(id)? else {
            return Ok(BTreeMap::new());
        };
        let mut out = BTreeMap::new();
        for (name, offset) in fields {
            let var = self.storage.read(&id, offset, None)?.map(|v| v.value);
            out.insert(name, self.resolve(id, var, 0)?);
        }
        Ok(out)
    }
    pub fn resolve(&mut self, id: Address, var: Option<Vm>, depth: usize) -> Result<Value> {
        if std::time::Instant::now() >= self.end {
            return Err(error("storage query deadline"));
        }
        if depth >= 100 || self.budget.rows == 0 {
            return Err(error("storage query budget exceeded"));
        }
        self.budget.rows -= 1;
        Ok(match var {
            None | Some(Vm::Nil) => Value::Null,
            Some(Vm::Bool(v)) => json!(v),
            Some(Vm::Uint(v)) => {
                if v >> 64 == mmx_vm::U256::zero() {
                    json!(v.low_u64())
                } else {
                    json!(v.to_string())
                }
            }
            Some(Vm::String(v)) => json!(String::from_utf8(v)?),
            Some(Vm::Binary(v)) => {
                let mut value = json!({"hex":render::hex(&v)});
                if v.len() == 32 {
                    value["hash"] = json!(render::hex(&v));
                    value["address"] = json!(mmx_vm::address::encode(&v.try_into().unwrap()));
                }
                value
            }
            Some(Vm::Ref(a)) => {
                let var = self.storage.read(&id, a, None)?.map(|v| v.value);
                self.resolve(id, var, depth + 1)?
            }
            Some(Vm::Array { address, size }) => {
                if size as usize > self.budget.rows {
                    return Err(error("storage array budget exceeded"));
                }
                let mut rows = Vec::new();
                for i in 0..size {
                    let var = self
                        .storage
                        .read(&id, address, Some(i.into()))?
                        .map(|v| v.value);
                    rows.push(self.resolve(id, var, depth + 1)?);
                }
                json!(rows)
            }
            Some(Vm::Map { address }) => {
                let (binary, _) = self.program(id)?;
                let prefix = mmx_vm::storage::data_key(&id, address, None).1;
                let mut start = prefix.clone();
                start.extend(0u64.to_be_bytes());
                let mut end = prefix.clone();
                end.extend(u64::MAX.to_be_bytes());
                let entries = self.storage.database().scan_range_bounded(
                    "storage_entries",
                    &start,
                    Some(&end),
                    None,
                    &mut self.budget,
                )?;
                let mut out = vnx::serde_json::Map::new();
                for entry in entries {
                    let key = u64::from_be_bytes(entry.key[28..].try_into()?);
                    let key = {
                        let mut vm = Engine::new(id, &mut self.storage, true, 0, 10_000_000);
                        vm.load_binary_parts(&binary.constant, &binary.binary)?;
                        vm.read(key)?
                    };
                    let (name, alias) = match key {
                        Some(Vm::Uint(v)) => (v.to_string(), None),
                        Some(Vm::String(v)) => (String::from_utf8(v)?, None),
                        Some(Vm::Binary(v)) => {
                            let alias = if v.len() == 32 {
                                Some(mmx_vm::address::encode(&v.clone().try_into().unwrap()))
                            } else {
                                None
                            };
                            (render::hex(&v), alias)
                        }
                        _ => ("null".into(), None),
                    };
                    let (var, _) = Var::decode(&entry.value, false, true)?;
                    let value = self.resolve(id, Some(var.value), depth + 1)?;
                    out.insert(name, value.clone());
                    if let Some(alias) = alias {
                        out.insert(alias, value);
                    }
                }
                Value::Object(out)
            }
        })
    }
    pub fn call(
        &mut self,
        request: &Request<'_>,
        id: Address,
        method: &str,
        args: &[Value],
        user: Option<Address>,
        deposit: Option<(Address, u128)>,
    ) -> Result<Value> {
        let (binary, _) = self.program(id)?;
        let entry = binary
            .methods
            .get(method.as_bytes())
            .ok_or_else(|| error(format!("no such method: {method}")))?;
        let params = &request.context.params;
        let mut vm = Engine::new(
            id,
            &mut self.storage,
            entry.is_const,
            u32::from(self.height >= params.hardfork2_height),
            params.max_tx_cost,
        );
        let code = vm.load_binary_parts(&binary.constant, &binary.binary)?;
        for (field, value) in [
            (mmx_vm::EXTERN_TXID, Vm::Nil),
            (mmx_vm::EXTERN_HEIGHT, Vm::from(u64::from(self.height))),
            (mmx_vm::EXTERN_ADDRESS, Vm::Binary(id.to_vec())),
            (
                mmx_vm::EXTERN_NETWORK,
                Vm::String(params.network.as_bytes().to_vec()),
            ),
            (
                mmx_vm::EXTERN_USER,
                user.map(|v| Vm::Binary(v.to_vec())).unwrap_or(Vm::Nil),
            ),
        ] {
            vm.write(mmx_vm::MEM_EXTERN + field, value)?;
        }
        if let Some((currency, amount)) = deposit {
            vm.set_deposit(&currency, amount)?;
        }
        let args = args
            .iter()
            .map(InputValue::try_from)
            .collect::<mmx_vm::Result<Vec<_>>>()?;
        vm.set_args(&args)?;
        vm.run(&code, entry.entry_point)?;
        vm.charge_gas(0)?;
        if std::time::Instant::now() >= self.end {
            return Err(error("contract call deadline"));
        }
        let result = vm.read_result(mmx_vm::MEM_STACK, 10000)?;
        input_json(result)
    }
}
fn input_json(value: InputValue) -> Result<Value> {
    Ok(match value {
        InputValue::Unsupported => return Err(error("unsupported VM result")),
        InputValue::Nil => Value::Null,
        InputValue::Bool(v) => json!(v),
        InputValue::Uint(v) => {
            if v >> 64 == mmx_vm::U256::zero() {
                json!(v.low_u64())
            } else {
                json!(format!("0x{v:X}"))
            }
        }
        InputValue::String(v) => json!(String::from_utf8(v)?),
        InputValue::Binary(v) => json!(v),
        InputValue::Array(v) => json!(v.into_iter().map(input_json).collect::<Result<Vec<_>>>()?),
        InputValue::Map(v) => {
            let pairs = v
                .into_iter()
                .map(|(k, v)| Ok((input_json(k)?, input_json(v)?)))
                .collect::<Result<Vec<_>>>()?;
            if pairs.iter().all(|(k, _)| k.is_string()) {
                Value::Object(
                    pairs
                        .into_iter()
                        .map(|(k, v)| (k.as_str().unwrap().into(), v))
                        .collect(),
                )
            } else {
                json!(pairs)
            }
        }
    })
}

impl View {
    pub fn rows(
        &mut self,
        table: &str,
        prefix: &[u8],
        width: usize,
        limit: usize,
    ) -> Result<Vec<mmx_db::Row>> {
        if std::time::Instant::now() >= self.end {
            return Err(error("storage query deadline"));
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut begin = prefix.to_vec();
        begin.resize(width, 0);
        let mut end = prefix.to_vec();
        let next = (0..end.len()).rev().find(|i| end[*i] != u8::MAX);
        let end = next.map(|i| {
            end[i] += 1;
            for byte in &mut end[i + 1..] {
                *byte = 0;
            }
            end.resize(width, 0);
            end
        });
        let mut budget = self.budget;
        budget.rows = limit.min(budget.rows);
        let result = self.storage.database().scan_range_bounded(
            table,
            &begin,
            end.as_deref(),
            None,
            &mut budget,
        )?;
        self.budget.keys = budget.keys;
        self.budget.bytes = budget.bytes;
        self.budget.rows = self.budget.rows.saturating_sub(result.len());
        use mmx_execution::chain::public as index;
        let offset = match table {
            index::TRANSACTIONS | index::TRADES => Some(0),
            index::CONTRACTS | index::EXECUTIONS => Some(32),
            index::FARMER_BLOCKS => Some(33),
            _ => None,
        };
        let result = if let Some(offset) = offset {
            result
                .into_iter()
                .filter(|row| {
                    row.key
                        .get(offset..offset + 4)
                        .and_then(|v| v.try_into().ok())
                        .map(u32::from_be_bytes)
                        .is_some_and(|v| !v >= self.floor)
                })
                .collect()
        } else {
            result
        };
        Ok(result)
    }
    pub fn index_complete(&self) -> Result<bool> {
        use mmx_execution::chain::public as index;
        let db = self.storage.database();
        let Some(start) = db.get(index::META, index::START, None)? else {
            return Ok(false);
        };
        let Some(cursor) = db.get(index::META, index::CURSOR, None)? else {
            return Ok(false);
        };
        Ok(u32::from_be_bytes(
            cursor
                .try_into()
                .map_err(|_| error("invalid public index cursor"))?,
        ) >= u32::from_be_bytes(
            start
                .try_into()
                .map_err(|_| error("invalid public index start"))?,
        ))
    }
    pub fn require_index(&self) -> Result<()> {
        if !self.index_complete()? {
            return Err(error("public history index is catching up"));
        }
        Ok(())
    }
    pub fn circulating(&self, id: Address) -> Result<u128> {
        Ok(self
            .storage
            .get_total_supply(&id)?
            .unwrap_or(0)
            .saturating_sub(self.storage.balance_at(&[0; 32], &id, None)?.unwrap_or(0)))
    }
    pub fn address_count(&mut self) -> Result<usize> {
        use mmx_execution::chain::public as index;
        if let Some(value) = self
            .storage
            .database()
            .get(index::META, index::ADDRESS_COUNT, None)?
        {
            return Ok(u64::from_le_bytes(
                value
                    .try_into()
                    .map_err(|_| error("invalid address counter"))?,
            ) as usize);
        }
        let rows = self.rows("balances", &[], 64, 10000)?;
        if rows.len() >= 10000 {
            return Err(error("address count exceeds scan budget"));
        }
        Ok(rows
            .iter()
            .filter(|row| {
                row.key.len() == 64
                    && row.key[32..] == [0; 32]
                    && row.value.len() == 16
                    && u128::from_le_bytes(row.value.as_slice().try_into().unwrap()) >= 1_000_000
            })
            .count())
    }
}

impl View {
    pub fn named(&mut self, id: Address, names: &[&str]) -> Result<BTreeMap<String, Value>> {
        let mut out = BTreeMap::new();
        for name in names {
            let (var, _) = self.field(id, name)?;
            out.insert((*name).into(), self.resolve(id, var, 0)?);
        }
        Ok(out)
    }
    pub fn named_at(
        &mut self,
        id: Address,
        names: &[&str],
        height: u32,
    ) -> Result<BTreeMap<String, Value>> {
        let snapshot = StorageDb::new(self.storage.database().fork(height.saturating_add(1))?);
        let live = std::mem::replace(&mut self.storage, snapshot);
        let result = self.named(id, names);
        self.storage = live;
        result
    }
    pub fn map_entry(&mut self, id: Address, name: &str, key: &Vm) -> Result<Value> {
        let (var, offset) = self.field(id, name)?;
        let address = if let Some(Vm::Ref(a)) = var {
            a
        } else {
            offset
        };
        let Some(key) = self.storage.lookup(&id, key)? else {
            return Ok(Value::Null);
        };
        let value = self.storage.read(&id, address, Some(key))?.map(|v| v.value);
        self.resolve(id, value, 0)
    }
}

#[cfg(test)]
impl View {
    pub(super) fn fixture(storage: StorageDb, height: u32) -> Self {
        Self {
            storage,
            height,
            budget: mmx_db::ReadBudget {
                rows: 10000,
                keys: 100000,
                bytes: 32 * 1024 * 1024,
            },
            end: std::time::Instant::now() + std::time::Duration::from_secs(2),
            floor: 0,
            _permit: rpc::ReadPermit::acquire(&std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ))
            .unwrap(),
        }
    }
}

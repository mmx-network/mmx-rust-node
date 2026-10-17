use mmx_vm::{
    storage::{self, BalanceWrite, ContractData, StorageWrite, TransactionBatch},
    value::FLAG_KEY,
    Address, Storage, Value, Var,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

type Cell = (Address, u64, Option<u64>);
pub(crate) struct Overlay<'a, S> {
    backend: &'a S,
    writes: BTreeMap<Cell, Var>,
    keys: BTreeMap<(Address, Value), u64>,
    balances: RefCell<BTreeMap<(Address, Address), u128>>,
    contracts: BTreeMap<Address, ContractData>,
}
impl<'a, S: Storage> Overlay<'a, S> {
    pub fn new(backend: &'a S) -> Self {
        Self {
            backend,
            writes: BTreeMap::new(),
            keys: BTreeMap::new(),
            balances: RefCell::new(BTreeMap::new()),
            contracts: BTreeMap::new(),
        }
    }
    pub fn set_balance(&self, contract: Address, currency: Address, amount: u128) {
        self.balances
            .borrow_mut()
            .insert((contract, currency), amount);
    }
    pub fn deploy(&mut self, address: Address, contract: ContractData) {
        self.contracts.insert(address, contract);
    }
    pub fn batch(&self) -> TransactionBatch {
        TransactionBatch {
            writes: self
                .writes
                .iter()
                .map(|((c, a, k), v)| {
                    (
                        *c,
                        StorageWrite {
                            address: *a,
                            key: *k,
                            var: v.clone(),
                        },
                    )
                })
                .collect(),
            balances: self
                .balances
                .borrow()
                .iter()
                .map(|((c, k), amount)| BalanceWrite {
                    contract: *c,
                    currency: *k,
                    amount: *amount,
                })
                .collect(),
            contracts: self
                .contracts
                .iter()
                .map(|(a, c)| (*a, c.clone()))
                .collect(),
            transactions: Vec::new(),
            supplies: Vec::new(),
        }
    }
}
// Clone the shared handle, not the overlay. Parent and child engines keep their
// own memory caches while every engine commits into this transaction's overlay.
pub(crate) struct Shared<'a, S>(pub Rc<RefCell<Overlay<'a, S>>>);
impl<S> Clone for Shared<'_, S> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<S: Storage> Shared<'_, S> {
    /// Carry VM effects between block transactions. Chain contracts and indexes
    /// remain at the original snapshot until the complete block is published.
    pub(crate) fn absorb_vm(&mut self, batch: &TransactionBatch) -> storage::Result<()> {
        let mut groups = BTreeMap::<Address, Vec<StorageWrite>>::new();
        for (contract, write) in &batch.writes {
            groups.entry(*contract).or_default().push(write.clone());
        }
        for (contract, writes) in groups {
            self.apply(&contract, &writes)?;
        }
        let state = self.0.borrow();
        for balance in &batch.balances {
            state.set_balance(balance.contract, balance.currency, balance.amount);
        }
        Ok(())
    }
}
impl<S: Storage> Storage for Shared<'_, S> {
    fn get_transaction_index(
        &self,
        id: &Address,
    ) -> storage::Result<Option<storage::TransactionIndex>> {
        self.0.borrow().backend.get_transaction_index(id)
    }
    fn read(&self, c: &Address, a: u64, k: Option<u64>) -> storage::Result<Option<Var>> {
        let state = self.0.borrow();
        if let Some(var) = state.writes.get(&(*c, a, k)) {
            return Ok(Some(var.clone()));
        }
        state.backend.read(c, a, k)
    }
    fn lookup(&self, c: &Address, v: &Value) -> storage::Result<Option<u64>> {
        let state = self.0.borrow();
        if let Some(a) = state.keys.get(&(*c, v.clone())) {
            return Ok(Some(*a));
        }
        state.backend.lookup(c, v)
    }
    fn get_balance(&self, c: &Address, k: &Address) -> storage::Result<Option<u128>> {
        let state = self.0.borrow();
        if let Some(amount) = state.balances.borrow().get(&(*c, *k)) {
            return Ok(Some(*amount));
        }
        let amount = state.backend.get_balance(c, k)?;
        if let Some(amount) = amount {
            state.set_balance(*c, *k, amount);
        }
        Ok(amount)
    }
    fn get_contract(&self, a: &Address) -> storage::Result<Option<ContractData>> {
        let state = self.0.borrow();
        if let Some(contract) = state.contracts.get(a) {
            return Ok(Some(contract.clone()));
        }
        state.backend.get_contract(a)
    }
    fn apply(&mut self, c: &Address, writes: &[StorageWrite]) -> storage::Result<()> {
        let mut state = self.0.borrow_mut();
        // StorageRAM disallows overwriting an interned key. Validate before
        // touching the overlay so even a backend batch failure is atomic.
        let mut seen = BTreeMap::new();
        for write in writes {
            let key = (*c, write.address, write.key);
            let previous = seen.get(&key).or_else(|| state.writes.get(&key));
            if write.key.is_none() && previous.is_some_and(|v: &Var| v.flags & FLAG_KEY != 0) {
                return Err(storage::Error::Invalid("cannot overwrite key"));
            }
            seen.insert(key, write.var.clone());
        }
        for (key, var) in seen {
            if key.2.is_none() && var.flags & FLAG_KEY != 0 {
                state.keys.insert((*c, var.value.clone()), key.1);
            }
            state.writes.insert(key, var);
        }
        Ok(())
    }
}

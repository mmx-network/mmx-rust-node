//! Contract execution, transaction and block-body validation with atomic settlement.
//! The invocation API accepts authenticated, funded calls. The `transaction`
//! module combines native hashing, authorization, balances, fees and execution.
//! The `block` module adds header authentication, reservations, shared VM state,
//! ordering and economics. Its proof-aware API also verifies CPU space work and
//! header transitions. VDF work and chain selection remain separate stages.
pub mod block;
mod contracts;
mod overlay;
pub mod transaction;
mod wire;
pub use contracts::{BinaryInfo, Contract, ExecutableInfo, MethodInfo};

use contracts::failure;
use mmx_vm::{
    copy_between,
    storage::{AtomicStorage, ContractData},
    Address, Engine, Error, Host, InputValue, Output, Result, Storage, Value, EXTERN_ADDRESS,
    EXTERN_HEIGHT, EXTERN_NETWORK, EXTERN_TXID, EXTERN_USER, MEM_EXTERN, MEM_STACK,
};
use overlay::{Overlay, Shared};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Context {
    pub txid: Address,
    pub height: u32,
    pub hardfork2_height: u32,
    pub protocol_version: u32,
    pub network: Vec<u8>,
    pub gas_limit: u64,
    /// Already validated static costs; this layer adds contract reads and VM cost.
    pub initial_cost: u64,
    pub min_txfee_read: u64,
    pub min_txfee_exec: u64,
    pub max_rcall_depth: u32,
    pub max_rcall_width: usize,
    /// If absent, prepare the lock set using the reference dependency traversal.
    pub locked_contracts: Option<BTreeSet<Address>>,
    /// An already validated deployment, at txid. Executables run their constructor.
    pub deployment: Option<ContractData>,
}
impl Default for Context {
    fn default() -> Self {
        Self {
            txid: [0; 32],
            height: 0,
            hardfork2_height: 0,
            protocol_version: 1,
            network: b"mainnet".to_vec(),
            gas_limit: 1_000_000,
            initial_cost: 0,
            min_txfee_read: 1000,
            min_txfee_exec: 10_000,
            max_rcall_depth: 3,
            max_rcall_width: 10,
            locked_contracts: None,
            deployment: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Invocation {
    pub address: Address,
    pub method: Vec<u8>,
    pub args: Vec<InputValue>,
    /// Must have been authenticated by transaction validation.
    pub user: Option<Address>,
    /// Must have been reserved from validated transaction inputs.
    pub deposit: Option<(Address, u128)>,
}
impl Invocation {
    pub fn new(address: Address, method: impl AsRef<[u8]>) -> Self {
        Self {
            address,
            method: method.as_ref().to_vec(),
            args: Vec::new(),
            user: None,
            deposit: None,
        }
    }
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ErrorInfo {
    pub code: u32,
    pub address: Option<u32>,
    pub operation: Option<u32>,
    pub line: Option<u32>,
}
#[derive(Debug)]
pub struct Failure {
    pub error: Error,
    pub info: ErrorInfo,
    pub total_cost: u64,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for Failure {}
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub total_cost: u64,
    /// Child outputs precede their caller's outputs. They are published to the
    /// caller only after the entire state/balance batch has succeeded.
    pub outputs: Vec<Output>,
    pub spend: BTreeMap<(Address, Address), u128>,
}

/// Execute all invocations against one disposable overlay, then publish one
/// atomic state/balance/deployment batch. Outputs are returned for the node's
/// input/output settlement; they are not spendable by later invocations here.
/// No database version is advanced. Failed batches leave existing pending state
/// untouched, and report the reference's clamped VM cost and outer error site.
pub fn execute<S: AtomicStorage>(
    storage: &mut S,
    context: &Context,
    operations: &[Invocation],
) -> std::result::Result<Receipt, Failure> {
    prepare(storage, context, operations)?.commit()
}

/// Keeps exclusive access to the backend while the node validates dynamic fees,
/// execution inputs/outputs and other post-execution rules. Dropping it aborts.
/// The caller can add settlement/fee writes to the same atomic batch.
pub struct PreparedExecution<'a, S: AtomicStorage> {
    storage: &'a mut S,
    receipt: Receipt,
    batch: mmx_vm::storage::TransactionBatch,
    info: ErrorInfo,
}
impl<S: AtomicStorage> PreparedExecution<'_, S> {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
    pub fn storage(&self) -> &S {
        self.storage
    }
    pub fn batch_mut(&mut self) -> &mut mmx_vm::storage::TransactionBatch {
        &mut self.batch
    }
    pub fn commit(self) -> std::result::Result<Receipt, Failure> {
        match self.storage.apply_transaction(&self.batch) {
            Ok(()) => Ok(self.receipt),
            Err(error) => Err(Failure {
                error: error.into(),
                info: self.info,
                total_cost: self.receipt.total_cost,
            }),
        }
    }
}

pub fn prepare<'a, S: AtomicStorage>(
    storage: &'a mut S,
    context: &Context,
    operations: &[Invocation],
) -> std::result::Result<PreparedExecution<'a, S>, Failure> {
    let mut receipt = Receipt {
        total_cost: context.initial_cost,
        ..Default::default()
    };
    let mut info = ErrorInfo::default();
    let result = (|| {
        let locks = match &context.locked_contracts {
            Some(locks) => locks.clone(),
            None => prepare_locks(storage, context, operations)?,
        };
        let shared = Shared(Rc::new(RefCell::new(Overlay::new(&*storage))));
        if let Some(contract) = &context.deployment {
            shared.0.borrow_mut().deploy(context.txid, contract.clone());
        }
        let mut session = Session {
            context,
            storage: shared.clone(),
            locks,
            receipt: &mut receipt,
            error: &mut info,
        };
        if let Some(data) = &context.deployment {
            let contract = Arc::new(Contract::decode(data.clone())?);
            if contract.type_name() == "mmx.contract.Executable" {
                let exec = contract.executable()?;
                let mut invocation = Invocation::new(context.txid, exec.init_method);
                let InputValue::Array(args) = contract.read_field(b"init_args")? else {
                    return Err(Error::Input);
                };
                invocation.args = args;
                session.operation(&invocation, contract, true)?;
            }
        }
        for (index, operation) in operations.iter().enumerate() {
            session.error.operation = Some(index as u32);
            let address = if operation.address == [0; 32] {
                context.txid
            } else {
                operation.address
            };
            let contract = if address == context.txid {
                context
                    .deployment
                    .clone()
                    .map(Contract::decode)
                    .transpose()?
                    .map(Arc::new)
            } else {
                session.fetch_total(&address)?
            };
            let contract = contract.ok_or_else(|| failure("no such contract"))?;
            let mut operation = operation.clone();
            operation.address = address;
            session.operation(&operation, contract, false)?;
        }
        let batch = shared.0.borrow().batch();
        Ok(batch)
    })();
    match result {
        Ok(batch) => Ok(PreparedExecution {
            storage,
            receipt,
            batch,
            info,
        }),
        Err(error) => Err(Failure {
            error,
            info,
            total_cost: receipt.total_cost,
        }),
    }
}

pub fn prepare_locks<S: Storage>(
    storage: &S,
    context: &Context,
    operations: &[Invocation],
) -> Result<BTreeSet<Address>> {
    fn depends<S: Storage>(
        storage: &S,
        address: Address,
        depth: u32,
        ctx: &Context,
        path: &mut BTreeSet<Address>,
    ) -> Result<BTreeSet<Address>> {
        if !path.insert(address) {
            return Err(failure("dependency cycle"));
        }
        let mut out = BTreeSet::new();
        if let Some(data) = storage.get_contract(&address)? {
            let contract = Contract::decode(data)?;
            if contract.type_name() == "mmx.contract.Executable" {
                let executable = contract.executable()?;
                if !executable.depends.is_empty() && depth > ctx.max_rcall_depth {
                    return Err(failure("remote call depth overflow"));
                }
                for child in executable.depends.values() {
                    out.extend(depends(storage, *child, depth + 1, ctx, path)?);
                    out.insert(*child);
                }
            }
        }
        path.remove(&address);
        if out.len() > ctx.max_rcall_width {
            return Err(failure("remote call width overflow"));
        }
        Ok(out)
    }
    let mut locks: BTreeSet<_> = operations
        .iter()
        .map(|o| {
            if o.address == [0; 32] {
                context.txid
            } else {
                o.address
            }
        })
        .collect();
    if context.deployment.is_some() {
        locks.insert(context.txid);
    }
    for address in locks.clone() {
        if address == context.txid {
            if let Some(data) = &context.deployment {
                let contract = Contract::decode(data.clone())?;
                if contract.type_name() == "mmx.contract.Executable" {
                    let executable = contract.executable()?;
                    let mut out: BTreeSet<_> = executable.depends.values().copied().collect();
                    if out.len() > context.max_rcall_width {
                        return Err(failure("remote call width overflow"));
                    }
                    for child in out.clone() {
                        out.extend(depends(storage, child, 2, context, &mut BTreeSet::new())?);
                    }
                    if out.len() > context.max_rcall_width {
                        return Err(failure("remote call width overflow"));
                    }
                    locks.extend(out);
                }
            }
        } else {
            locks.extend(depends(storage, address, 1, context, &mut BTreeSet::new())?);
        }
    }
    Ok(locks)
}

struct Session<'a, 's, S> {
    context: &'a Context,
    storage: Shared<'s, S>,
    locks: BTreeSet<Address>,
    receipt: &'a mut Receipt,
    error: &'a mut ErrorInfo,
}
impl<'s, S: Storage> Session<'_, 's, S> {
    fn fetch_total(&mut self, address: &Address) -> Result<Option<Arc<Contract>>> {
        let data = self.storage.get_contract(address)?;
        let index = self.storage.get_transaction_index(address)?;
        let cost = self.context.min_txfee_read.wrapping_add(index.map_or_else(
            || data.as_ref().map_or(0, |d| d.read_cost),
            |i| i.contract_read_cost,
        ));
        let next = self.receipt.total_cost.wrapping_add(cost);
        if next > self.context.gas_limit {
            return Err(failure("not enough gas to read contract"));
        }
        self.receipt.total_cost = next;
        data.map(Contract::decode)
            .transpose()
            .map(|c| c.map(Arc::new))
    }
    fn fetch(
        &self,
        address: &Address,
        vm: &mut Engine<'_, Shared<'s, S>>,
    ) -> Result<Option<Arc<Contract>>> {
        let data = self.storage.get_contract(address)?;
        let index = self.storage.get_transaction_index(address)?;
        let cost = self.context.min_txfee_read.wrapping_add(index.map_or_else(
            || data.as_ref().map_or(0, |d| d.read_cost),
            |i| i.contract_read_cost,
        ));
        let next = vm.gas_used.wrapping_add(cost);
        if next > vm.gas_limit {
            return Err(failure("not enough gas to read contract"));
        }
        vm.gas_used = next;
        data.map(Contract::decode)
            .transpose()
            .map(|c| c.map(Arc::new))
    }
    fn operation(
        &mut self,
        operation: &Invocation,
        contract: Arc<Contract>,
        constructor: bool,
    ) -> Result<()> {
        let storage = self.storage.clone();
        let context = self.context;
        self.operation_authenticated(operation, contract, constructor, None, |vm| {
            if let Some(user) = operation.user {
                let data = storage.get_contract(&user)?;
                let index = storage.get_transaction_index(&user)?;
                let cost = context.min_txfee_read.wrapping_add(index.map_or_else(
                    || data.as_ref().map_or(0, |d| d.read_cost),
                    |i| i.contract_read_cost,
                ));
                if vm.gas_used.wrapping_add(cost) > vm.gas_limit {
                    return Err(failure("not enough gas to read contract"));
                }
                vm.gas_used = vm.gas_used.wrapping_add(cost);
                if let Some(data) = data {
                    Contract::decode(data)?;
                }
            }
            Ok(())
        })
    }
    fn operation_authenticated<F>(
        &mut self,
        operation: &Invocation,
        contract: Arc<Contract>,
        constructor: bool,
        gas_limit: Option<u64>,
        authenticate: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut Engine<'_, Shared<'s, S>>) -> Result<()>,
    {
        let executable = contract.executable()?;
        let mut storage = self.storage.clone();
        let mut vm = Engine::new(
            operation.address,
            &mut storage,
            false,
            self.context.protocol_version,
            gas_limit.unwrap_or_else(|| {
                self.context
                    .gas_limit
                    .saturating_sub(self.receipt.total_cost)
            }),
        );
        authenticate(&mut vm)?;
        if let Some(user) = operation.user {
            vm.write(MEM_EXTERN + EXTERN_USER, Value::Binary(user.to_vec()))?;
        } else {
            vm.write(MEM_EXTERN + EXTERN_USER, Value::Nil)?;
        }
        vm.write(
            MEM_EXTERN + EXTERN_ADDRESS,
            Value::Binary(operation.address.to_vec()),
        )?;
        vm.write(
            MEM_EXTERN + EXTERN_NETWORK,
            Value::String(self.context.network.clone()),
        )?;
        if let Some((currency, amount)) = operation.deposit {
            let amount = amount.wrapping_add(
                self.storage
                    .get_balance(&operation.address, &currency)?
                    .unwrap_or(0),
            );
            self.storage
                .0
                .borrow()
                .set_balance(operation.address, currency, amount);
        }
        let setup = |vm: &mut Engine<'_, Shared<'s, S>>| {
            if let Some((currency, amount)) = operation.deposit {
                vm.set_deposit(&currency, amount)?;
            }
            vm.set_args(&operation.args)
        };
        if self.context.height < self.context.hardfork2_height {
            setup(&mut vm)?;
        }
        let result = self.instance(&mut vm, executable, &operation.method, constructor, setup);
        self.receipt.total_cost = self
            .receipt
            .total_cost
            .wrapping_add(vm.gas_used.min(vm.gas_limit));
        result
    }
    fn instance<F>(
        &mut self,
        vm: &mut Engine<'_, Shared<'s, S>>,
        executable: ExecutableInfo,
        name: &[u8],
        constructor: bool,
        setup: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut Engine<'_, Shared<'s, S>>) -> Result<()>,
    {
        if !self.locks.contains(&vm.contract) {
            return Err(failure("contract not locked"));
        }
        let binary = self
            .fetch(&executable.binary, vm)?
            .ok_or_else(|| failure("no such binary"))?
            .binary()?;
        let method = binary
            .methods
            .iter()
            .find(|(key, _)| key.as_slice() == name)
            .map(|(_, v)| v)
            .ok_or_else(|| failure("no such method"))?;
        if constructor {
            if !method.is_init {
                return Err(failure("not a constructor"));
            }
        } else if !method.is_public {
            return Err(failure("method is not public"));
        }
        let code = vm.load_binary_parts(&binary.constant, &binary.binary)?;
        if self.context.height >= self.context.hardfork2_height {
            setup(vm)?;
        }
        vm.write(
            MEM_EXTERN + EXTERN_HEIGHT,
            Value::from(self.context.height as u64),
        )?;
        vm.write(
            MEM_EXTERN + EXTERN_TXID,
            Value::Binary(self.context.txid.to_vec()),
        )?;
        let mut cache = BTreeMap::new();
        cache.insert(
            self.context.txid,
            self.context
                .deployment
                .clone()
                .map(Contract::decode)
                .transpose()?
                .map(Arc::new),
        );
        let mut host = FrameHost {
            session: self,
            executable,
            cache,
        };
        let result = vm
            .run_with_host(&code, method.entry_point, &mut host)
            .and_then(|_| {
                if method.is_const {
                    vm.charge_gas(0)
                } else {
                    vm.commit().and_then(|_| vm.charge_gas(0))
                }
            });
        if let Err(error) = result {
            self.error.code = vm.error_code;
            self.error.address = vm.error_addr.map(|a| a as u32);
            if let Some((_, line)) = binary
                .line_info
                .range(..=vm.error_addr.unwrap_or(u32::MAX as u64) as u32)
                .next_back()
            {
                self.error.line = Some(*line);
            }
            return Err(error);
        }
        let (outputs, minted) = vm.outputs()?;
        for output in outputs {
            let balance = self
                .storage
                .get_balance(&vm.contract, &output.contract)?
                .ok_or_else(|| failure("contract over-spend"))?;
            if output.amount > balance {
                return Err(failure("contract over-spend"));
            }
            self.storage.0.borrow().set_balance(
                vm.contract,
                output.contract,
                balance - output.amount,
            );
            self.receipt.outputs.push(output.clone());
            let total = self
                .receipt
                .spend
                .entry((vm.contract, output.contract))
                .or_default();
            *total = total.wrapping_add(output.amount);
        }
        self.receipt.outputs.extend_from_slice(minted);
        Ok(())
    }
}
struct FrameHost<'h, 'a, 's, S> {
    session: &'h mut Session<'a, 's, S>,
    executable: ExecutableInfo,
    cache: BTreeMap<Address, Option<Arc<Contract>>>,
}
impl<'s, S: Storage> Host<Shared<'s, S>> for FrameHost<'_, '_, 's, S> {
    fn remote_calls_enabled(&self) -> bool {
        true
    }
    fn contract_reads_enabled(&self) -> bool {
        true
    }
    fn remote_call(
        &mut self,
        vm: &mut Engine<'_, Shared<'s, S>>,
        name: &[u8],
        method: &[u8],
        nargs: u32,
    ) -> Result<()> {
        let address = self
            .executable
            .depends
            .iter()
            .find(|(key, _)| key.as_slice() == name)
            .map(|(_, v)| *v)
            .ok_or_else(|| failure("no such external contract"))?;
        let contract = self.cached(address, vm)?.executable()?;
        vm.charge_gas(self.session.context.min_txfee_exec)?;
        let mut storage = self.session.storage.clone();
        let mut child = Engine::new(
            address,
            &mut storage,
            false,
            vm.protocol_version,
            vm.gas_limit.saturating_sub(vm.gas_used),
        );
        let stack = vm.stack_address()?;
        let setup = |child: &mut Engine<'_, Shared<'s, S>>| {
            for i in 0..nargs {
                copy_between(child, vm, MEM_STACK + 1 + i as u64, stack + 1 + i as u64)?;
            }
            child.write(
                MEM_EXTERN + EXTERN_USER,
                Value::Binary(vm.contract.to_vec()),
            )?;
            child.write(MEM_EXTERN + EXTERN_ADDRESS, Value::Binary(address.to_vec()))?;
            child.write(
                MEM_EXTERN + EXTERN_NETWORK,
                Value::String(self.session.context.network.clone()),
            )
        };
        // Use the same closure in exactly one of the historical setup phases.
        let post = if self.session.context.height < self.session.context.hardfork2_height {
            let mut setup = setup;
            setup(&mut child)?;
            None
        } else {
            Some(setup)
        };
        self.session
            .instance(&mut child, contract, method, false, |child| match post {
                Some(mut setup) => setup(child),
                None => Ok(()),
            })?;
        copy_between(vm, &mut child, stack, MEM_STACK)?;
        vm.charge_gas(child.gas_used)
    }
    fn read_contract(
        &mut self,
        vm: &mut Engine<'_, Shared<'s, S>>,
        address: &Address,
        field: &[u8],
        dst: u64,
    ) -> Result<()> {
        let contract = self.cached(*address, vm)?;
        vm.assign_input(dst, &contract.read_field(field)?)?;
        vm.charge_gas(0)
    }
}
impl<'s, S: Storage> FrameHost<'_, '_, 's, S> {
    fn cached(
        &mut self,
        address: Address,
        vm: &mut Engine<'_, Shared<'s, S>>,
    ) -> Result<Arc<Contract>> {
        if self.cache.get(&address).is_none_or(Option::is_none) {
            self.cache
                .insert(address, self.session.fetch(&address, vm)?);
        }
        self.cache[&address]
            .clone()
            .ok_or_else(|| failure("no such contract"))
    }
}

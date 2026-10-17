//! Single-transaction validation, deferred execution and atomic settlement.
//! Block ordering, block-wide reservations/rewards and chain selection remain
//! outside this API. Ledger reads use the original snapshot, independent of VM
//! balance caches. No database version advances until the caller commits it.
mod amounts;
use super::{prepare_locks, Context, Contract, ErrorInfo, Invocation, Receipt, Session};
use crate::overlay::{Overlay, Shared};
use amounts::Amounts;
use mmx_consensus::{
    authorization::{self, AuthError},
    Record, ValidationParams, Value as N,
};
use mmx_types::mmx::{self, error_code_e as Code};
use mmx_vm::{
    storage::{
        self, BalanceWrite, ContractData, LedgerStorage, TransactionBatch, TransactionIndex,
    },
    Address, InputValue, Output, Storage,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};

type Key = (Address, Address);
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("{0}")]
    Static(String),
    #[error("{0}")]
    Dynamic(String),
    #[error(transparent)]
    Authorization(#[from] AuthError),
    #[error(transparent)]
    Consensus(#[from] mmx_consensus::Error),
    #[error(transparent)]
    Header(#[from] mmx_consensus::header::HeaderError),
    #[error(transparent)]
    Proof(#[from] mmx_consensus::proof::ProofError),
    #[error(transparent)]
    Storage(#[from] storage::Error),
    #[error(transparent)]
    Execution(#[from] mmx_vm::Error),
    #[error("{0}")]
    Mismatch(String),
}
impl ValidationError {
    fn rejects(&self) -> bool {
        matches!(
            self,
            Self::Static(_)
                | Self::Consensus(_)
                | Self::Header(_)
                | Self::Proof(_)
                | Self::Storage(_)
                | Self::Mismatch(_)
        ) || matches!(self,Self::Authorization(e) if e.is_static_failure())
            || matches!(
                self,
                Self::Execution(mmx_vm::Error::Storage(storage::Error::Database(_)))
            )
    }
}
#[derive(Debug)]
pub struct Rejected {
    pub error: ValidationError,
    pub info: ErrorInfo,
    pub total_cost: u64,
}
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for Rejected {}
#[derive(Clone, Debug, Default)]
pub struct TransactionContext {
    pub height: u32,
    pub params: mmx::ChainParams,
    pub locked_contracts: Option<BTreeSet<Address>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionInput {
    pub output: Output,
    pub solution: u16,
    pub flags: u8,
}
#[derive(Clone, Debug)]
pub struct TransactionResult {
    pub did_fail: bool,
    pub total_cost: u32,
    pub total_fee: u32,
    pub inputs: Vec<ExecutionInput>,
    pub outputs: Vec<Output>,
    pub error: Option<ExecutionError>,
}
#[derive(Clone, Debug)]
pub struct ExecutionError {
    pub info: ErrorInfo,
    pub message: Vec<u8>,
}

pub struct PreparedTransaction<'a, S: LedgerStorage> {
    storage: &'a mut S,
    result: TransactionResult,
    batch: TransactionBatch,
}
impl<S: LedgerStorage> PreparedTransaction<'_, S> {
    pub fn storage(&self) -> &S {
        self.storage
    }
    pub fn result(&self) -> &TransactionResult {
        &self.result
    }
    pub fn batch(&self) -> &TransactionBatch {
        &self.batch
    }
    pub fn commit(self) -> std::result::Result<TransactionResult, Rejected> {
        self.storage
            .apply_transaction(&self.batch)
            .map_err(|error| Rejected {
                error: error.into(),
                info: ErrorInfo::default(),
                total_cost: self.result.total_cost.into(),
            })?;
        Ok(self.result)
    }
}
pub fn execute<S: LedgerStorage>(
    storage: &mut S,
    context: &TransactionContext,
    tx: &Record,
) -> std::result::Result<TransactionResult, Rejected> {
    prepare(storage, context, tx)?.commit()
}
pub fn prepare<'a, S: LedgerStorage>(
    storage: &'a mut S,
    context: &TransactionContext,
    tx: &Record,
) -> std::result::Result<PreparedTransaction<'a, S>, Rejected> {
    let mut info = ErrorInfo::default();
    let mut cost = 0;
    let (result, batch) = build(&*storage, &*storage, context, tx, &mut info, &mut cost)
        .and_then(|(result, mut batch)| {
            let sender = bytes(tx.field("sender")?.optional()?.expect("sender validated"))?;
            batch.balances = settle(&*storage, tx, &result, sender)?;
            Ok((result, batch))
        })
        .map_err(|error| Rejected {
            error,
            info,
            total_cost: cost,
        })?;
    Ok(PreparedTransaction {
        storage,
        result,
        batch,
    })
}
fn static_error(message: &str) -> ValidationError {
    ValidationError::Static(message.into())
}
fn dynamic(message: &str) -> ValidationError {
    ValidationError::Dynamic(message.into())
}
fn malformed(message: &str) -> ValidationError {
    mmx_consensus::Error::Input(message.into()).into()
}
pub(crate) fn bytes<const L: usize>(v: &N) -> std::result::Result<[u8; L], ValidationError> {
    v.bytes()?.try_into().map_err(|_| malformed("byte length"))
}
pub(crate) fn uint(r: &Record, name: &str) -> std::result::Result<u64, ValidationError> {
    Ok(r.field(name)?.uint()?)
}
pub(crate) fn optional_record(v: &N) -> std::result::Result<Option<&Record>, ValidationError> {
    Ok(v.optional()?.map(N::record).transpose()?)
}
fn amount(r: &Record) -> std::result::Result<u128, ValidationError> {
    Ok(u128::from_le_bytes(bytes(r.field("amount")?)?))
}
pub(crate) fn io(r: &Record) -> std::result::Result<Output, ValidationError> {
    Ok(Output {
        address: bytes(r.field("address")?)?,
        contract: bytes(r.field("contract")?)?,
        amount: amount(r)?,
        memo: r
            .field("memo")?
            .optional()?
            .map(|m| m.text().map(Vec::from))
            .transpose()?,
    })
}
fn input_io(r: &Record) -> std::result::Result<ExecutionInput, ValidationError> {
    Ok(ExecutionInput {
        output: io(r)?,
        solution: uint(r, "solution")?
            .try_into()
            .map_err(|_| malformed("solution index"))?,
        flags: uint(r, "flags")?
            .try_into()
            .map_err(|_| malformed("input flags"))?,
    })
}
/// Native fees round down, and use 128-bit intermediates.
pub fn cost_to_fee(cost: u64, ratio: u32) -> std::result::Result<u64, ValidationError> {
    ((cost as u128 * ratio as u128) / 1024)
        .try_into()
        .map_err(|_| static_error("fee amount overflow"))
}
pub fn fee_to_cost(fee: u64, ratio: u32) -> std::result::Result<u64, ValidationError> {
    if ratio == 0 {
        return Err(static_error("zero fee ratio"));
    }
    ((fee as u128 * 1024) / ratio as u128)
        .try_into()
        .map_err(|_| static_error("cost value overflow"))
}
fn fetch<S: Storage>(
    storage: &S,
    address: &Address,
    cost: &mut u64,
    limit: u64,
    p: &mmx::ChainParams,
) -> std::result::Result<Option<ContractData>, ValidationError> {
    let data = storage.get_contract(address)?;
    let index = storage.get_transaction_index(address)?;
    let read = index.map_or_else(
        || data.as_ref().map_or(0, |d| d.read_cost),
        |i| i.contract_read_cost,
    );
    let next = cost.wrapping_add(p.min_txfee_read.wrapping_add(read));
    if next > limit {
        return Err(dynamic("not enough gas to read contract"));
    }
    *cost = next;
    Ok(data)
}
fn as_auth(error: ValidationError) -> AuthError {
    match error {
        ValidationError::Authorization(e) => e,
        _ => AuthError::Input(mmx_consensus::Error::Input(error.to_string())),
    }
}
struct Balances<'a, S> {
    storage: &'a S,
    values: BTreeMap<Key, u128>,
}
impl<S: Storage> Balances<'_, S> {
    fn find(&mut self, key: Key) -> std::result::Result<Option<&mut u128>, ValidationError> {
        if !self.values.contains_key(&key) {
            if let Some(v) = self.storage.get_balance(&key.0, &key.1)? {
                self.values.insert(key, v);
            } else {
                return Ok(None);
            }
        }
        Ok(self.values.get_mut(&key))
    }
}
pub(crate) fn locks<S: Storage>(
    storage: &S,
    context: &TransactionContext,
    tx: &Record,
) -> std::result::Result<BTreeSet<Address>, ValidationError> {
    let deployment = optional_record(tx.field("deploy")?)?
        .map(|r| {
            r.encode().map(|bytes| ContractData {
                bytes,
                read_cost: 0,
            })
        })
        .transpose()?;
    let ctx = Context {
        txid: bytes(tx.field("id")?)?,
        max_rcall_depth: context.params.max_rcall_depth,
        max_rcall_width: context.params.max_rcall_width as usize,
        deployment,
        ..Default::default()
    };
    let calls = tx
        .field("execute")?
        .seq()?
        .iter()
        .map(|v| Ok(Invocation::new(bytes(v.record()?.field("address")?)?, b"")))
        .collect::<std::result::Result<Vec<_>, ValidationError>>()?;
    Ok(prepare_locks(storage, &ctx, &calls)?)
}
pub(crate) fn build<S: Storage, V: Storage>(
    storage: &S,
    execution: &V,
    context: &TransactionContext,
    tx: &Record,
    info: &mut ErrorInfo,
    cost: &mut u64,
) -> std::result::Result<(TransactionResult, TransactionBatch), ValidationError> {
    let p = &context.params;
    let validation = ValidationParams::from(p);
    if tx.name != "mmx.Transaction" || !tx.is_valid(&validation)? {
        return Err(static_error("invalid tx"));
    }
    let version = uint(tx, "version")? as u32;
    if version != u32::from(context.height >= p.hardfork2_height) {
        return Err(static_error("invalid tx version"));
    }
    *cost = uint(tx, "static_cost")?;
    if *cost > p.max_tx_cost {
        return Err(static_error("static_cost > max_tx_cost"));
    }
    let max_fee = uint(tx, "max_fee_amount")?;
    if max_fee >> 32 != 0 {
        return Err(static_error("max fee amount >= 2^32"));
    }
    let txid = bytes(tx.field("id")?)?;
    if storage.get_transaction_index(&txid)?.is_some() {
        return Err(static_error("duplicate tx"));
    }
    let ratio = uint(tx, "fee_ratio")? as u32;
    let available = fee_to_cost(max_fee, ratio)?;
    let gas_limit = available.min(p.max_tx_cost);
    let static_fee = cost_to_fee(*cost, ratio)?;
    if static_fee > max_fee {
        return Err(static_error("static transaction fee > max_fee_amount"));
    }
    let sender = authorization::authorize_sender(tx)?;
    let mut balances = Balances {
        storage,
        values: BTreeMap::new(),
    };
    let balance = balances.find((sender, [0; 32]))?;
    if balance.as_ref().is_none_or(|b| **b < static_fee as u128) {
        info.code = Code::INSUFFICIENT_FUNDS_TXFEE.value;
        return Err(static_error("insufficient funds for transaction fee"));
    }
    *balance.unwrap() -= static_fee as u128;
    let deploy = optional_record(tx.field("deploy")?)?;
    let deployment = deploy
        .map(|d| {
            Ok::<_, ValidationError>(ContractData {
                bytes: d.encode()?,
                read_cost: d.num_bytes()?.wrapping_mul(p.min_txfee_read_kbyte) / 1000,
            })
        })
        .transpose()?;
    let mut exec_context = Context {
        txid,
        height: context.height,
        hardfork2_height: p.hardfork2_height,
        protocol_version: version,
        network: p.network.as_bytes().to_vec(),
        gas_limit,
        initial_cost: *cost,
        min_txfee_read: p.min_txfee_read,
        min_txfee_exec: p.min_txfee_exec,
        max_rcall_depth: p.max_rcall_depth,
        max_rcall_width: p.max_rcall_width as usize,
        locked_contracts: context.locked_contracts.clone(),
        deployment: deployment.clone(),
    };
    // prepare_context is outside the native dynamic-failure handler. A bad
    // dependency graph rejects the transaction, rather than charging a failure.
    if exec_context.locked_contracts.is_none() {
        exec_context.locked_contracts = Some(locks(storage, context, tx)?);
    }
    let mut receipt = Receipt {
        total_cost: *cost,
        ..Default::default()
    };
    let shared = Shared(Rc::new(RefCell::new(Overlay::new(execution))));
    let body = run(
        storage,
        context,
        tx,
        &exec_context,
        available,
        &shared,
        &mut balances,
        &mut receipt,
        info,
    );
    *cost = receipt.total_cost;
    let (inputs, outputs, mut failed) = match body {
        Ok((i, o)) => (i, o, None),
        Err(e) if e.rejects() => return Err(e),
        Err(e) => (Vec::new(), Vec::new(), Some(e)),
    };
    let total_fee = cost_to_fee(*cost, ratio)?;
    let fee = total_fee.min(max_fee);
    let dynamic_fee = fee.wrapping_sub(static_fee);
    let balance = balances.find((sender, [0; 32]))?;
    if balance.as_ref().is_none_or(|b| **b < dynamic_fee as u128) {
        info.code = Code::INSUFFICIENT_FUNDS_TXFEE.value;
        return Err(static_error("insufficient funds for transaction fee"));
    }
    *balance.unwrap() -= dynamic_fee as u128;
    if total_fee > max_fee && failed.is_none() {
        info.code = Code::TXFEE_OVERRUN.value;
        failed = Some(dynamic("transaction fee > max_fee_amount"));
    }
    let generated = TransactionResult {
        did_fail: failed.is_some(),
        total_cost: *cost as u32,
        total_fee: fee as u32,
        inputs: if failed.is_some() { Vec::new() } else { inputs },
        outputs: if failed.is_some() {
            Vec::new()
        } else {
            outputs
        },
        error: failed.map(|e| ExecutionError {
            info: info.clone(),
            message: e.to_string().into_bytes().into_iter().take(160).collect(),
        }),
    };
    let result = if let Some(expected) = optional_record(tx.field("exec_result")?)? {
        let result = TransactionResult::from_record(expected)?;
        compare(&generated, &result, *cost, info)?;
        result
    } else {
        generated
    };
    let mut batch = if result.did_fail {
        TransactionBatch::default()
    } else {
        shared.0.borrow().batch()
    };
    batch.transactions.push((
        txid,
        TransactionIndex {
            height: context.height,
            contract_read_cost: deployment.as_ref().map_or(0, |d| d.read_cost),
        },
    ));
    Ok((result, batch))
}
#[allow(clippy::too_many_arguments)]
fn run<S: Storage, V: Storage>(
    storage: &S,
    context: &TransactionContext,
    tx: &Record,
    ctx: &Context,
    available: u64,
    shared: &Shared<'_, V>,
    balances: &mut Balances<'_, S>,
    receipt: &mut Receipt,
    info: &mut ErrorInfo,
) -> std::result::Result<(Vec<ExecutionInput>, Vec<Output>), ValidationError> {
    let p = &context.params;
    let validation = ValidationParams::from(p);
    if uint(tx, "expires")? < context.height as u64 {
        info.code = Code::TX_EXPIRED.value;
        return Err(dynamic("transaction expired"));
    }
    let mut amounts = Amounts::default();
    info.address = Some(0);
    for (index, value) in tx.field("inputs")?.seq()?.iter().enumerate() {
        let r = value.record()?;
        let input = io(r)?;
        let balance = balances.find((input.address, input.contract))?;
        if balance.as_ref().is_none_or(|b| **b < input.amount) {
            info.code = Code::INSUFFICIENT_FUNDS.value;
            return Err(dynamic("insufficient funds"));
        }
        let mut fetch_error = None;
        let authorization = authorization::authorize_input(tx, index, |address| {
            let result = fetch(storage, address, &mut receipt.total_cost, ctx.gas_limit, p)
                .and_then(|d| {
                    d.map(|d| Record::decode(&d.bytes).map_err(Into::into))
                        .transpose()
                });
            result.map_err(|e| {
                let message = e.to_string();
                fetch_error = Some(e);
                AuthError::Input(mmx_consensus::Error::Input(message))
            })
        });
        if let Some(e) = fetch_error {
            return Err(e);
        }
        authorization?;
        *balance.unwrap() -= input.amount;
        let value = amounts.entry(input.contract);
        *value = value.wrapping_add(input.amount);
        info.address = Some(index as u32 + 1);
    }
    info.address = Some(0);
    for (index, value) in tx.field("outputs")?.seq()?.iter().enumerate() {
        let out = io(value.record()?)?;
        if out.amount == 0 {
            return Err(dynamic("zero amount output"));
        }
        let value = amounts.entry(out.contract);
        if out.amount > *value {
            return Err(dynamic("transaction over-spend"));
        }
        *value -= out.amount;
        info.address = Some(index as u32 + 1);
    }
    info.address = None;
    let operations = tx.field("execute")?.seq()?;
    let invocations = operations
        .iter()
        .map(|v| invocation(v.record()?))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let locks = ctx
        .locked_contracts
        .as_ref()
        .expect("locks prepared before dynamic validation");
    if let Some(deploy) = optional_record(tx.field("deploy")?)? {
        if !deploy.is_valid(&validation)? {
            info.code = Code::INVALID_CONTRACT.value;
            return Err(dynamic("invalid contract"));
        }
        shared
            .0
            .borrow_mut()
            .deploy(ctx.txid, ctx.deployment.clone().unwrap());
        if deploy.name == "mmx.contract.Executable" {
            for value in deploy.field("depends")?.map()?.values() {
                let address = bytes(value)?;
                if storage.get_transaction_index(&address)?.is_none() {
                    info.code = Code::INVALID_CONTRACT.value;
                    return Err(dynamic("missing dependency"));
                }
            }
            let contract = Arc::new(Contract::decode(ctx.deployment.clone().unwrap())?);
            let mut call = Invocation::new(ctx.txid, deploy.field("init_method")?.text()?);
            call.args = deploy
                .field("init_args")?
                .seq()?
                .iter()
                .map(vm_input)
                .collect();
            let limit = available
                .saturating_sub(receipt.total_cost)
                .min(p.max_tx_cost);
            Session {
                context: ctx,
                storage: shared.clone(),
                locks: locks.clone(),
                receipt,
                error: info,
            }
            .operation_authenticated(&call, contract, true, Some(limit), |_| Ok(()))?;
        }
    }
    let mut deposits = BTreeMap::<Key, u128>::new();
    for (index, (op, mut call)) in operations.iter().zip(invocations).enumerate() {
        info.operation = Some(index as u32);
        let op = op.record()?;
        if !op.is_valid(&validation)? {
            info.code = Code::INVALID_OPERATION.value;
            return Err(dynamic("invalid operation"));
        }
        if call.address == [0; 32] {
            call.address = ctx.txid;
        }
        let data = if call.address == ctx.txid {
            ctx.deployment.clone()
        } else {
            fetch(
                storage,
                &call.address,
                &mut receipt.total_cost,
                ctx.gas_limit,
                p,
            )?
        };
        let contract = Arc::new(Contract::decode(
            data.ok_or_else(|| dynamic("no such contract"))?,
        )?);
        if let Some((currency, amount)) = call.deposit {
            let value = amounts.entry(currency);
            if amount > *value {
                return Err(dynamic("deposit over-spend"));
            }
            *value -= amount;
            let value = deposits.entry((call.address, currency)).or_default();
            *value = value.wrapping_add(amount);
        }
        let limit = available
            .saturating_sub(receipt.total_cost)
            .min(p.max_tx_cost);
        let mut auth_failure = None;
        let mut fetch_error = None;
        let outcome = Session {
            context: ctx,
            storage: shared.clone(),
            locks: locks.clone(),
            receipt,
            error: info,
        }
        .operation_authenticated(&call, contract, false, Some(limit), |vm| {
            let result = authorization::authorize_operation_user(tx, index, |address| {
                let result =
                    fetch(storage, address, &mut vm.gas_used, vm.gas_limit, p).and_then(|d| {
                        d.map(|d| Record::decode(&d.bytes).map_err(Into::into))
                            .transpose()
                    });
                result.map_err(|e| {
                    let message = e.to_string();
                    fetch_error = Some(e);
                    as_auth(dynamic(&message))
                })
            });
            result.map(|_| ()).map_err(|e| {
                let text = e.to_string();
                auth_failure = Some(e);
                mmx_vm::Error::Contract(text)
            })
        });
        if let Some(e) = fetch_error {
            return Err(e);
        }
        if let Some(e) = auth_failure {
            return Err(e.into());
        }
        outcome?;
    }
    info.operation = None;
    let mut inputs = Vec::new();
    let mut outputs = receipt.outputs.clone();
    for (key, spent) in &receipt.spend {
        let deposit = deposits.entry(*key).or_default();
        let used = (*deposit).min(*spent);
        *deposit -= used;
        let amount = *spent - used;
        if amount != 0 {
            inputs.push(ExecutionInput {
                output: Output {
                    address: key.0,
                    contract: key.1,
                    amount,
                    memo: None,
                },
                solution: u16::MAX,
                flags: 0,
            });
        }
    }
    for ((address, contract), amount) in deposits {
        if amount != 0 {
            outputs.push(Output {
                address,
                contract,
                amount,
                memo: None,
            });
        }
    }
    for currency in amounts.keys(context.height >= p.hardfork2_height) {
        let amount = amounts.values[&currency];
        if amount != 0 {
            if ctx.deployment.is_none() {
                return Err(dynamic("implicit deposit without deploy"));
            }
            outputs.push(Output {
                address: ctx.txid,
                contract: currency,
                amount,
                memo: None,
            });
        }
    }
    if optional_record(tx.field("exec_result")?)?.is_none() {
        for input in &inputs {
            receipt.total_cost = receipt.total_cost.wrapping_add(io_cost(&input.output, p));
        }
        for output in &outputs {
            receipt.total_cost = receipt.total_cost.wrapping_add(io_cost(output, p));
        }
    }
    if receipt.total_cost > p.max_tx_cost {
        return Err(static_error("transaction cost > max_tx_cost"));
    }
    if receipt.total_cost >> 32 != 0 {
        return Err(static_error("transaction cost >= 2^32"));
    }
    Ok((inputs, outputs))
}
fn io_cost(out: &Output, p: &mmx::ChainParams) -> u64 {
    p.min_txfee_io
        .wrapping_add(out.memo.as_ref().map_or(0, |m| {
            (m.len() as u64)
                .div_ceil(32)
                .max(1)
                .wrapping_mul(p.min_txfee_memo)
        }))
}
fn vm_input(value: &N) -> InputValue {
    match value {
        N::Variant(v) => vm_input(v.value()),
        N::Null => InputValue::Nil,
        N::Bool(v) => InputValue::Bool(*v),
        N::Uint(v) => InputValue::Uint((*v).into()),
        N::Int(v) if *v >= 0 => InputValue::Uint((*v as u64).into()),
        N::String(v) => InputValue::String(v.clone()),
        N::Bytes(v) => InputValue::Binary(v.clone()),
        N::Sequence(v) => InputValue::Array(v.iter().map(vm_input).collect()),
        N::Object(v) => InputValue::Map(
            v.iter()
                .map(|(k, v)| (InputValue::String(k.clone()), vm_input(v)))
                .collect(),
        ),
        _ => InputValue::Unsupported,
    }
}
fn invocation(op: &Record) -> std::result::Result<Invocation, ValidationError> {
    Ok(Invocation {
        address: bytes(op.field("address")?)?,
        method: op.field("method")?.text()?.to_vec(),
        args: op.field("args")?.seq()?.iter().map(vm_input).collect(),
        user: op.field("user")?.optional()?.map(bytes).transpose()?,
        deposit: if op.name == "mmx.operation.Deposit" {
            Some((bytes(op.field("currency")?)?, amount(op)?))
        } else {
            None
        },
    })
}
fn settle<S: Storage>(
    storage: &S,
    tx: &Record,
    result: &TransactionResult,
    sender: Address,
) -> std::result::Result<Vec<BalanceWrite>, ValidationError> {
    let mut balances = BTreeMap::<Key, u128>::new();
    if !result.did_fail {
        for out in tx
            .field("outputs")?
            .seq()?
            .iter()
            .map(|v| io(v.record()?))
            .chain(result.outputs.iter().cloned().map(Ok))
        {
            let out = out?;
            let key = (out.address, out.contract);
            if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(key) {
                entry.insert(storage.get_balance(&key.0, &key.1)?.unwrap_or(0));
            }
            let value = balances.get_mut(&key).unwrap();
            *value = value.wrapping_add(out.amount);
        }
    }
    let fee = Output {
        address: sender,
        contract: [0; 32],
        amount: result.total_fee.into(),
        memo: None,
    };
    let mut inputs = vec![fee];
    if !result.did_fail {
        for value in tx.field("inputs")?.seq()? {
            inputs.push(io(value.record()?)?);
        }
        inputs.extend(result.inputs.iter().map(|i| i.output.clone()));
    }
    for input in inputs {
        let key = (input.address, input.contract);
        if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(key) {
            if let Some(v) = storage.get_balance(&key.0, &key.1)? {
                entry.insert(v);
            } else {
                continue;
            }
        }
        let value = balances.get_mut(&key).unwrap();
        *value = value.saturating_sub(input.amount);
    }
    Ok(balances
        .into_iter()
        .map(|((contract, currency), amount)| BalanceWrite {
            contract,
            currency,
            amount,
        })
        .collect())
}
fn compare(
    actual: &TransactionResult,
    expected: &TransactionResult,
    cost: u64,
    info: &ErrorInfo,
) -> std::result::Result<(), ValidationError> {
    let mismatch = |s: &str| ValidationError::Mismatch(s.into());
    if actual.did_fail != expected.did_fail {
        return Err(mismatch("execution success/failure mismatch"));
    }
    if expected.total_cost as u64 != cost {
        return Err(mismatch("transaction cost mismatch"));
    }
    if actual.total_fee != expected.total_fee {
        return Err(mismatch("transaction fee mismatch"));
    }
    if expected.did_fail {
        if !expected.inputs.is_empty() || !expected.outputs.is_empty() {
            return Err(mismatch(
                "failed transaction cannot have execution inputs / outputs",
            ));
        }
    } else {
        if actual.inputs.len() != expected.inputs.len() {
            return Err(mismatch("execution input count mismatch"));
        }
        if actual.outputs != expected.outputs {
            return Err(mismatch("execution output mismatch"));
        }
        for (a, b) in actual.inputs.iter().zip(&expected.inputs) {
            if a.output != b.output || a.flags != b.flags {
                return Err(mismatch("execution input mismatch"));
            }
        }
    }
    if let Some(error) = &expected.error {
        if error.info.code != info.code
            || error.info.address != info.address
            || error.info.operation != info.operation
        {
            return Err(mismatch("execution error information mismatch"));
        }
    } else if expected.did_fail {
        return Err(mismatch("missing error information"));
    }
    Ok(())
}
impl TransactionResult {
    pub fn from_record(r: &Record) -> std::result::Result<Self, ValidationError> {
        let error = optional_record(r.field("error")?)?
            .map(|e| {
                Ok::<_, ValidationError>(ExecutionError {
                    info: ErrorInfo {
                        code: uint(e, "code")? as u32,
                        address: Some(uint(e, "address")? as u32).filter(|v| *v != u32::MAX),
                        operation: Some(uint(e, "operation")? as u32).filter(|v| *v != u32::MAX),
                        line: e
                            .field("line")?
                            .optional()?
                            .map(|v| v.uint().map(|n| n as u32))
                            .transpose()?,
                    },
                    message: e.field("message")?.text()?.to_vec(),
                })
            })
            .transpose()?;
        Ok(Self {
            did_fail: r.field("did_fail")?.boolean()?,
            total_cost: uint(r, "total_cost")? as u32,
            total_fee: uint(r, "total_fee")? as u32,
            inputs: r
                .field("inputs")?
                .seq()?
                .iter()
                .map(|v| input_io(v.record()?))
                .collect::<std::result::Result<_, _>>()?,
            outputs: r
                .field("outputs")?
                .seq()?
                .iter()
                .map(|v| io(v.record()?))
                .collect::<std::result::Result<_, _>>()?,
            error,
        })
    }
    pub fn to_record(&self) -> Record {
        let optional = |v: Option<N>| N::Optional(v.map(Box::new));
        let error = self.error.as_ref().map(|e| {
            N::Record(Record {
                name: "mmx.exec_error_t".into(),
                fields: BTreeMap::from([
                    ("code".into(), N::Uint(e.info.code.into())),
                    (
                        "address".into(),
                        N::Uint(e.info.address.unwrap_or(u32::MAX).into()),
                    ),
                    (
                        "operation".into(),
                        N::Uint(e.info.operation.unwrap_or(u32::MAX).into()),
                    ),
                    (
                        "line".into(),
                        optional(e.info.line.map(|v| N::Uint(v.into()))),
                    ),
                    ("message".into(), N::String(e.message.clone())),
                ]),
            })
        });
        Record {
            name: "mmx.exec_result_t".into(),
            fields: BTreeMap::from([
                ("did_fail".into(), N::Bool(self.did_fail)),
                ("total_cost".into(), N::Uint(self.total_cost.into())),
                ("total_fee".into(), N::Uint(self.total_fee.into())),
                (
                    "inputs".into(),
                    N::Sequence(
                        self.inputs
                            .iter()
                            .map(|i| N::Record(io_record(&i.output, Some((i.solution, i.flags)))))
                            .collect(),
                    ),
                ),
                (
                    "outputs".into(),
                    N::Sequence(
                        self.outputs
                            .iter()
                            .map(|o| N::Record(io_record(o, None)))
                            .collect(),
                    ),
                ),
                ("error".into(), optional(error)),
            ]),
        }
    }
}
fn io_record(out: &Output, input: Option<(u16, u8)>) -> Record {
    let mut fields = BTreeMap::from([
        ("address".into(), N::Bytes(out.address.to_vec())),
        ("contract".into(), N::Bytes(out.contract.to_vec())),
        ("amount".into(), N::Bytes(out.amount.to_le_bytes().to_vec())),
        (
            "memo".into(),
            N::Optional(out.memo.as_ref().map(|m| Box::new(N::String(m.clone())))),
        ),
    ]);
    if let Some((solution, flags)) = input {
        fields.insert("solution".into(), N::Uint(solution.into()));
        fields.insert("flags".into(), N::Uint(flags.into()));
    }
    Record {
        name: if input.is_some() {
            "mmx.txin_t"
        } else {
            "mmx.txout_t"
        }
        .into(),
        fields,
    }
}
/// Attach a computed result as Transaction::update does. The unsigned ID is
/// retained; static cost and content hash are recomputed for block validation.
pub fn with_result(
    tx: &Record,
    result: &TransactionResult,
    params: &mmx::ChainParams,
) -> std::result::Result<Record, ValidationError> {
    let mut tx = tx.clone();
    tx.fields.insert(
        "exec_result".into(),
        N::Optional(Some(Box::new(N::Record(result.to_record())))),
    );
    let cost = tx.calc_cost(&ValidationParams::from(params))?;
    tx.fields
        .insert("static_cost".into(), N::Uint(cost as u32 as u64));
    let hash = tx.calc_hash(true, 0)?;
    tx.fields
        .insert("content_hash".into(), N::Bytes(hash.to_vec()));
    Ok(tx)
}

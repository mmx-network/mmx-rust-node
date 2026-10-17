//! Block-body validation against a fixed chain snapshot, shared VM execution,
//! and atomic settlement. Header hashes, static validity, farmer authentication
//! and contextual scalar checks are enforced here. The caller must also verify
//! space/VDF work, challenge/difficulty transitions, weights and reward targets.
//! This API does not select a chain or advance the database version.
pub mod economics;

use crate::{
    overlay::{Overlay, Shared},
    transaction::{
        self, bytes, io, optional_record, uint, TransactionContext, TransactionResult,
        ValidationError,
    },
    ErrorInfo,
};
use mmx_consensus::Record;
use mmx_types::mmx;
use mmx_vm::{
    storage::{BalanceWrite, BlockStorage, TransactionBatch},
    Address, Output, Storage,
};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

#[derive(Clone, Debug)]
pub struct BlockContext {
    pub params: mmx::ChainParams,
    /// The verified parent corresponding to the backend's current snapshot.
    pub previous: mmx::BlockHeader,
    /// Resolved by the header/chain verifier from preceding VDF rewards.
    pub expected_vdf_payout: Option<Address>,
}

#[derive(Debug)]
pub struct Rejected {
    pub transaction: Option<usize>,
    pub error: Box<ValidationError>,
    pub info: ErrorInfo,
    /// Cost of the rejected transaction; zero for block-wide checks.
    pub total_cost: u64,
}
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(index) = self.transaction {
            write!(f, "transaction {index}: ")?;
        }
        self.error.fmt(f)
    }
}
impl std::error::Error for Rejected {}

/// Serial execution is a valid schedule for the native dependency graph.
/// Predecessors identify the last preceding mutator of each locked contract.
#[derive(Clone, Debug, Default)]
pub struct ExecutionPlan {
    pub locks: Vec<BTreeSet<Address>>,
    pub predecessors: Vec<BTreeSet<usize>>,
}
#[derive(Clone, Debug, Default)]
pub struct Receipt {
    pub results: Vec<TransactionResult>,
    pub inputs: Vec<Output>,
    pub outputs: Vec<Output>,
    /// Native application clamps each currency's net issuance at zero before
    /// adding it to total supply; fees/burns do not reduce that supply counter.
    pub supply_delta: BTreeMap<Address, u128>,
    pub plan: ExecutionPlan,
}
pub struct PreparedBlock<'a, S: BlockStorage> {
    storage: &'a mut S,
    receipt: Receipt,
    batch: TransactionBatch,
}
impl<S: BlockStorage> PreparedBlock<'_, S> {
    pub fn storage(&self) -> &S {
        self.storage
    }
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }
    pub fn batch(&self) -> &TransactionBatch {
        &self.batch
    }
    pub fn commit(self) -> Result<Receipt, Rejected> {
        self.storage
            .apply_transaction(&self.batch)
            .map_err(|e| Rejected {
                transaction: None,
                error: Box::new(e.into()),
                info: ErrorInfo::default(),
                total_cost: 0,
            })?;
        Ok(self.receipt)
    }
}
/// Validate the header envelope and body after the chain layer verifies proof
/// work/transitions and reward targets. Nothing is published until `commit`;
/// dropping the prepared block aborts every transaction.
pub fn prepare<'a, S: BlockStorage>(
    storage: &'a mut S,
    context: &BlockContext,
    block: &Record,
) -> Result<PreparedBlock<'a, S>, Rejected> {
    let mut rejection = Rejected {
        transaction: None,
        error: Box::new(invalid("block validation")),
        info: ErrorInfo::default(),
        total_cost: 0,
    };
    let (receipt, batch) = build(&*storage, context, block, &mut rejection).map_err(|e| {
        *rejection.error = e;
        rejection
    })?;
    Ok(PreparedBlock {
        storage,
        receipt,
        batch,
    })
}
pub fn execute<S: BlockStorage>(
    storage: &mut S,
    context: &BlockContext,
    block: &Record,
) -> Result<Receipt, Rejected> {
    prepare(storage, context, block)?.commit()
}
fn invalid(s: &str) -> ValidationError {
    ValidationError::Dynamic(s.into())
}
fn optional_address(r: &Record, field: &str) -> Result<Option<Address>, ValidationError> {
    r.field(field)?.optional()?.map(bytes).transpose()
}

/// Native binary tree: an unpaired node is carried forward without rehashing.
pub fn transaction_root(hashes: &[Address]) -> Address {
    let mut nodes = hashes.to_vec();
    while nodes.len() > 1 {
        nodes = nodes
            .chunks(2)
            .map(|pair| {
                if pair.len() == 1 {
                    pair[0]
                } else {
                    Sha256::digest([pair[0], pair[1]].concat()).into()
                }
            })
            .collect();
    }
    nodes.first().copied().unwrap_or_default()
}
fn envelope(context: &BlockContext, block: &Record) -> Result<(), ValidationError> {
    if block.name != "mmx.Block" {
        return Err(invalid("expected block"));
    }
    let p = &context.params;
    if !mmx_consensus::header::is_valid(block, p)? {
        return Err(invalid("static header validation failed"));
    }
    mmx_consensus::header::authenticate(block)?;
    mmx_consensus::header::validate_context(block, &context.previous, p)?;
    let height = uint(block, "height")? as u32;
    let txs = block.field("tx_list")?.seq()?;
    if height < p.transaction_activation && !txs.is_empty() {
        return Err(invalid("transactions not activated yet"));
    }
    let mut static_cost = 0u64;
    let mut total_cost = 0u64;
    let mut fees = 0u64;
    let mut hashes = Vec::with_capacity(txs.len());
    for tx in txs {
        let tx = tx.record()?;
        let hash = bytes(tx.field("content_hash")?)?;
        if hash != tx.calc_hash(true, 0)? {
            return Err(invalid("invalid transaction content hash"));
        }
        let result = optional_record(tx.field("exec_result")?)?
            .ok_or_else(|| invalid("transaction missing exec_result"))?;
        static_cost = static_cost.wrapping_add(uint(tx, "static_cost")?);
        total_cost = total_cost.wrapping_add(uint(result, "total_cost")?);
        fees = fees.wrapping_add(uint(result, "total_fee")?);
        hashes.push(hash);
    }
    if uint(block, "static_cost")? != static_cost
        || uint(block, "total_cost")? != total_cost
        || uint(block, "tx_fees")? != fees
        || uint(block, "tx_count")? != txs.len() as u64
        || bytes::<32>(block.field("tx_hash")?)? != transaction_root(&hashes)
    {
        return Err(invalid("block body commitment mismatch"));
    }
    if static_cost > p.max_block_size {
        return Err(invalid("block size too high"));
    }
    if total_cost > p.max_block_cost {
        return Err(invalid("block cost too high"));
    }
    economics::validate_fields(context, block)?;
    Ok(())
}
fn build<S: Storage>(
    storage: &S,
    context: &BlockContext,
    block: &Record,
    rejected: &mut Rejected,
) -> Result<(Receipt, TransactionBatch), ValidationError> {
    envelope(context, block)?;
    let p = &context.params;
    let txs = block
        .field("tx_list")?
        .seq()?
        .iter()
        .map(|v| v.record())
        .collect::<mmx_consensus::Result<Vec<_>>>()?;
    let mut ctx = TransactionContext {
        height: uint(block, "height")? as u32,
        params: p.clone(),
        locked_contracts: None,
    };
    let mut reserved = BTreeMap::<(Address, Address), u128>::new();
    let mut ids = BTreeSet::new();
    let mut previous_ratio = u64::MAX;
    let mut receipt = Receipt::default();
    let mut last_mutator = BTreeMap::new();
    for (index, tx) in txs.iter().enumerate() {
        rejected.transaction = Some(index);
        let sender =
            optional_address(tx, "sender")?.ok_or_else(|| invalid("transaction missing sender"))?;
        let result = TransactionResult::from_record(
            optional_record(tx.field("exec_result")?)?
                .ok_or_else(|| invalid("transaction missing exec_result"))?,
        )?;
        if !ids.insert(bytes::<32>(tx.field("id")?)?) {
            return Err(invalid("duplicate transaction in same block"));
        }
        let ratio = uint(tx, "fee_ratio")?;
        if ratio > previous_ratio {
            return Err(invalid("invalid tx order (fee_ratio)"));
        }
        previous_ratio = ratio;
        reserve(
            storage,
            &mut reserved,
            (sender, [0; 32]),
            result.total_fee.into(),
            "insufficient funds to cover transaction fee",
        )?;
        if !result.did_fail {
            for value in tx.field("inputs")?.seq()? {
                let input = io(value.record()?)?;
                reserve(
                    storage,
                    &mut reserved,
                    (input.address, input.contract),
                    input.amount,
                    "insufficient funds to cover input",
                )?;
            }
        }
        let locks = transaction::locks(storage, &ctx, tx)?;
        let mut predecessors = BTreeSet::new();
        for address in &locks {
            if let Some(previous) = last_mutator.insert(*address, index) {
                predecessors.insert(previous);
            }
        }
        receipt.plan.locks.push(locks);
        receipt.plan.predecessors.push(predecessors);
    }
    rejected.transaction = None;
    minimum_fees(p, &txs)?;
    let mut execution = Shared(Rc::new(RefCell::new(Overlay::new(storage))));
    let mut batch = TransactionBatch::default();
    for (index, tx) in txs.iter().enumerate() {
        rejected.transaction = Some(index);
        ctx.locked_contracts = Some(receipt.plan.locks[index].clone());
        let (result, effects) = transaction::build(
            storage,
            &execution,
            &ctx,
            tx,
            &mut rejected.info,
            &mut rejected.total_cost,
        )?;
        execution.absorb_vm(&effects)?;
        batch.contracts.extend(effects.contracts);
        batch.transactions.extend(effects.transactions);
        receipt.results.push(result);
        rejected.info = ErrorInfo::default();
        rejected.total_cost = 0;
    }
    rejected.transaction = None;
    let reward = if optional_address(block, "reward_addr")?.is_some() {
        economics::block_reward(
            p,
            &context.previous,
            ctx.height,
            !block.field("proof")?.seq()?.is_empty(),
            uint(block, "tx_fees")?,
        )?
    } else {
        0
    };
    if uint(block, "reward_amount")? != reward {
        return Err(invalid("invalid reward_amount"));
    }
    receipt.outputs = reward_outputs(p, block)?;
    for (tx, result) in txs.iter().zip(&receipt.results) {
        let sender = optional_address(tx, "sender")?.unwrap();
        receipt.inputs.push(Output {
            address: sender,
            contract: [0; 32],
            amount: result.total_fee.into(),
            memo: None,
        });
        if !result.did_fail {
            for value in tx.field("inputs")?.seq()? {
                receipt.inputs.push(io(value.record()?)?);
            }
            receipt
                .inputs
                .extend(result.inputs.iter().map(|i| i.output.clone()));
            for value in tx.field("outputs")?.seq()? {
                receipt.outputs.push(io(value.record()?)?);
            }
            receipt.outputs.extend(result.outputs.iter().cloned());
        }
    }
    batch.writes = execution.0.borrow().batch().writes;
    let (balances, supplies) = settle(storage, &mut receipt)?;
    batch.balances = balances;
    batch.supplies = supplies;
    Ok((receipt, batch))
}
fn reserve<S: Storage>(
    storage: &S,
    balances: &mut BTreeMap<(Address, Address), u128>,
    key: (Address, Address),
    amount: u128,
    message: &str,
) -> Result<(), ValidationError> {
    if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(key) {
        entry.insert(
            storage
                .get_balance(&key.0, &key.1)?
                .ok_or_else(|| invalid(message))?,
        );
    }
    let balance = balances.get_mut(&key).unwrap();
    if amount > *balance {
        return Err(invalid(message));
    }
    *balance -= amount;
    Ok(())
}
fn minimum_fees(p: &mmx::ChainParams, txs: &[&Record]) -> Result<(), ValidationError> {
    let n = p.min_fee_ratio.len() as u64;
    if n == 0 {
        return Ok(());
    }
    let mut total = 0u64;
    let mut i = 0u64;
    for tx in txs.iter().rev() {
        total = total.wrapping_add(uint(tx, "static_cost")?);
        // The bounded search has the native thresholds, including rounding,
        // and rejects overflow without looping past the final size tier.
        while i < n && total > (i + 1).wrapping_mul(p.max_block_size) / n {
            i += 1;
        }
        if i >= n {
            return Err(invalid("block size overflow"));
        }
        let minimum = p.min_fee_ratio[i as usize].wrapping_mul(1024);
        if uint(tx, "fee_ratio")? < minimum as u64 {
            return Err(invalid("transaction fee_ratio too small"));
        }
    }
    Ok(())
}
fn reward_outputs(p: &mmx::ChainParams, block: &Record) -> Result<Vec<Output>, ValidationError> {
    let mut outputs = Vec::new();
    let mut push = |address, amount| {
        outputs.push(Output {
            address,
            contract: [0; 32],
            amount,
            memo: None,
        })
    };
    if let Some(address) = optional_address(block, "vdf_reward_payout")? {
        push(address, p.vdf_reward.into());
    }
    if let Some(address) = optional_address(block, "reward_addr")? {
        let amount = uint(block, "reward_amount")?;
        if amount != 0 {
            push(address, amount.into());
        }
    }
    let project = economics::project_reward(p, uint(block, "tx_fees")?)?;
    if project != 0 {
        push(bytes(block.field("project_addr")?)?, project.into());
    }
    Ok(outputs)
}
type Settlement = (Vec<BalanceWrite>, Vec<(Address, u128)>);
fn settle<S: Storage>(storage: &S, receipt: &mut Receipt) -> Result<Settlement, ValidationError> {
    let mut balances = BTreeMap::<(Address, Address), u128>::new();
    for output in &receipt.outputs {
        let key = (output.address, output.contract);
        if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(key) {
            entry.insert(storage.get_balance(&key.0, &key.1)?.unwrap_or(0));
        }
        let balance = balances.get_mut(&key).unwrap();
        *balance = balance.wrapping_add(output.amount);
        let delta = receipt.supply_delta.entry(output.contract).or_default();
        *delta = delta.wrapping_add(output.amount);
    }
    for input in &receipt.inputs {
        let key = (input.address, input.contract);
        if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(key) {
            if let Some(value) = storage.get_balance(&key.0, &key.1)? {
                entry.insert(value);
            }
        }
        if let Some(balance) = balances.get_mut(&key) {
            *balance = balance.saturating_sub(input.amount);
        }
        let delta = receipt.supply_delta.entry(input.contract).or_default();
        *delta = delta.saturating_sub(input.amount);
    }
    let mut supplies = Vec::new();
    for (currency, delta) in &receipt.supply_delta {
        if *delta != 0 {
            supplies.push((
                *currency,
                storage
                    .get_total_supply(currency)?
                    .unwrap_or(0)
                    .wrapping_add(*delta),
            ));
        }
    }
    Ok((
        balances
            .into_iter()
            .map(|((contract, currency), amount)| BalanceWrite {
                contract,
                currency,
                amount,
            })
            .collect(),
        supplies,
    ))
}

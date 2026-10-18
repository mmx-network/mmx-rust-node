//! Select pending transactions against one parent snapshot. Only accepted VM
//! effects flow to successors; input/fee reservations use original balances.
use super::{minimum_fees, reserve};
use crate::{
    overlay::{Overlay, Shared},
    transaction::{self, bytes, io, uint, TransactionContext, ValidationError},
    ErrorInfo,
};
use mmx_consensus::{Record, Value};
use mmx_types::mmx;
use mmx_vm::Storage;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    time::Instant,
};

pub fn transactions<S: Storage, P: std::borrow::Borrow<Record>>(
    storage: &S,
    params: &mmx::ChainParams,
    height: u32,
    pending: &[P],
    deadline: Instant,
) -> Result<Vec<Record>, ValidationError> {
    if height < params.transaction_activation {
        return Ok(vec![]);
    }
    let mut ordered: Vec<_> = pending
        .iter()
        .map(|value| {
            let tx = value.borrow();
            Ok((uint(tx, "fee_ratio")?, bytes::<32>(tx.field("id")?)?, tx))
        })
        .collect::<Result<_, ValidationError>>()?;
    ordered.sort_by_key(|(ratio, id, _)| (std::cmp::Reverse(*ratio), *id));
    let mut execution = Shared(Rc::new(RefCell::new(Overlay::new(storage))));
    let mut context = TransactionContext {
        height,
        params: params.clone(),
        locked_contracts: None,
    };
    let mut reserved = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut out = vec![];
    let mut static_cost = 0u64;
    let mut total_cost = 0u64;
    for (_, id, original) in ordered {
        if Instant::now() >= deadline {
            break;
        }
        if ids.contains(&id) {
            continue;
        }
        let mut tx = original.clone();
        tx.fields
            .insert("exec_result".into(), Value::Optional(None));
        tx.fields.insert(
            "static_cost".into(),
            Value::Uint(tx.calc_cost(&params.into())?),
        );
        tx.fields.insert(
            "content_hash".into(),
            Value::Bytes(tx.calc_hash(true, 0)?.to_vec()),
        );
        let build = (|| {
            context.locked_contracts = Some(transaction::locks(storage, &context, &tx)?);
            transaction::build(
                storage,
                &execution,
                &context,
                &tx,
                &mut ErrorInfo::default(),
                &mut 0,
            )
        })();
        let (result, effects) = match build {
            Ok(v) => v,
            Err(e) if e.is_unsupported() || matches!(e, ValidationError::Storage(_)) => {
                return Err(e)
            }
            Err(_) => continue,
        };
        let tx = transaction::with_result(&tx, &result, params)?;
        let next_static = static_cost
            .checked_add(uint(&tx, "static_cost")?)
            .ok_or_else(|| ValidationError::Static("block static cost overflow".into()))?;
        let next_total = total_cost
            .checked_add(result.total_cost.into())
            .ok_or_else(|| ValidationError::Static("block cost overflow".into()))?;
        if next_static > params.max_block_size || next_total > params.max_block_cost {
            continue;
        }
        let sender = bytes(
            tx.field("sender")?
                .optional()?
                .ok_or_else(|| ValidationError::Static("missing sender".into()))?,
        )?;
        let mut keys = BTreeSet::from([(sender, [0; 32])]);
        if !result.did_fail {
            for input in tx.field("inputs")?.seq()? {
                let input = io(input.record()?)?;
                keys.insert((input.address, input.contract));
            }
        }
        let mut next_reserved = keys
            .into_iter()
            .filter_map(|key| reserved.get(&key).map(|n| (key, *n)))
            .collect();
        let funds = (|| {
            reserve(
                storage,
                &mut next_reserved,
                (sender, [0; 32]),
                result.total_fee.into(),
                "insufficient pending fee funds",
            )?;
            if !result.did_fail {
                for input in tx.field("inputs")?.seq()? {
                    let input = io(input.record()?)?;
                    reserve(
                        storage,
                        &mut next_reserved,
                        (input.address, input.contract),
                        input.amount,
                        "insufficient pending input funds",
                    )?;
                }
            }
            Ok::<_, ValidationError>(())
        })();
        if funds.is_err() {
            continue;
        }
        let mut refs: Vec<_> = out.iter().collect();
        refs.push(&tx);
        if minimum_fees(params, &refs).is_err() {
            continue;
        }
        execution.absorb_vm(&effects)?;
        reserved.extend(next_reserved);
        static_cost = next_static;
        total_cost = next_total;
        ids.insert(id);
        out.push(tx);
    }
    Ok(out)
}

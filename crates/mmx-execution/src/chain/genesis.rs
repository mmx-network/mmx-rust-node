//! Node::init_chain construction from a trusted network bootstrap manifest.
//! Original bootstrap transactions are applied without ordinary validation or
//! VM execution. Mainnet's project relay sets project_addr but is not deployed.
use super::{invalid, ChainResult};
use crate::{
    block,
    transaction::{bytes, io, optional_record, uint, TransactionResult},
};
use mmx_consensus::{header, Record, Value};
use mmx_types::mmx;
use mmx_vm::{
    storage::{ContractData, TransactionBatch, TransactionIndex},
    Address, Output, Storage,
};
use sha2::{Digest, Sha256};

/// Supply the reference network's original, trusted records. Binary order is
/// offer, swap, token, plot NFT, NFT, template, escrow, time lock, relay.
#[derive(Clone, Debug)]
pub struct Assets {
    pub binaries: [Record; 9],
    pub testnet_rewards: Option<Record>,
    pub project_relay: Option<Record>,
}
pub fn build(params: &mmx::ChainParams, assets: &Assets) -> ChainResult<Record> {
    let vdf: Address = Sha256::digest(format!("MMX/{}/vdf/0", params.network)).into();
    let challenge: Address = Sha256::digest(format!("MMX/{}/challenge/0", params.network)).into();
    let mut block = Record::from_generated(&mmx::Block {
        nonce: params.port as u64,
        time_stamp: params.initial_time_stamp as i64,
        time_diff: params
            .initial_time_diff
            .wrapping_mul(params.time_diff_divider),
        space_diff: params.initial_space_diff,
        vdf_output: vdf.into(),
        challenge: challenge.into(),
        ..Default::default()
    })?;
    let mut transactions = Vec::new();
    let project: Address = if params.network == "mainnet" {
        transactions.push(
            assets
                .testnet_rewards
                .as_ref()
                .ok_or_else(|| invalid("failed to read testnet rewards"))?
                .clone(),
        );
        let relay = assets
            .project_relay
            .as_ref()
            .ok_or_else(|| invalid("failed to load tx_project_relay"))?;
        let exec = optional_record(relay.field("deploy")?)?;
        if !relay.is_valid(&params.into())?
            || relay.field("sender")?.optional()?.is_none()
            || uint(relay, "expires")? != u32::MAX as u64
            || exec.is_none_or(|v| v.name != "mmx.contract.Executable")
            || bytes::<32>(exec.unwrap().field("binary")?)? != params.relay_binary.0
        {
            return Err(invalid("invalid tx_project_relay"));
        }
        bytes(relay.field("id")?)?
    } else {
        mmx_vm::address::decode(b"mmx1kx69pm743rshqac5lgcstlr8nq4t93hzm8gumkkxmp5y9fglnkes6ve09z")
            .expect("native testnet project address")
    };
    transactions.extend(assets.binaries.iter().cloned());
    for tx in &transactions {
        if tx.name != "mmx.Transaction" || tx.field("network")?.text()? != params.network.as_bytes()
        {
            return Err(invalid("invalid genesis transaction"));
        }
    }
    let mut static_cost = 0u64;
    let mut total_cost = 0u64;
    let mut fees = 0u64;
    let mut hashes = Vec::new();
    for tx in &transactions {
        static_cost = static_cost.wrapping_add(uint(tx, "static_cost")?);
        if let Some(result) = optional_record(tx.field("exec_result")?)? {
            total_cost = total_cost.wrapping_add(uint(result, "total_cost")?);
            fees = fees.wrapping_add(uint(result, "total_fee")?);
        }
        let hash = bytes::<32>(tx.field("content_hash")?)?;
        if hash != tx.calc_hash(true, 0)? {
            return Err(invalid("invalid genesis block"));
        }
        hashes.push(hash);
    }
    for (name, value) in [
        ("static_cost", static_cost),
        ("total_cost", total_cost),
        ("tx_fees", fees),
        ("tx_count", transactions.len() as u64),
    ] {
        block.fields.insert(name.into(), Value::Uint(value));
    }
    block
        .fields
        .insert("project_addr".into(), Value::Bytes(project.to_vec()));
    block.fields.insert(
        "tx_hash".into(),
        Value::Bytes(block::transaction_root(&hashes).to_vec()),
    );
    block.fields.insert(
        "tx_list".into(),
        Value::Sequence(transactions.into_iter().map(Value::Record).collect()),
    );
    block.fields.insert(
        "hash".into(),
        Value::Bytes(header::calc_hash(&block, params)?.to_vec()),
    );
    block.fields.insert(
        "content_hash".into(),
        Value::Bytes(header::calc_content_hash(&block)?.to_vec()),
    );
    if !header::is_valid(&block, params)? {
        return Err(invalid("invalid genesis block"));
    }
    Ok(block)
}
pub(super) fn effects<S: Storage>(
    storage: &S,
    params: &mmx::ChainParams,
    block: &Record,
) -> ChainResult<(block::Receipt, TransactionBatch)> {
    let mut receipt = block::Receipt {
        outputs: block::reward_outputs(params, block)?,
        ..Default::default()
    };
    let mut batch = TransactionBatch::default();
    for tx in block.field("tx_list")?.seq()? {
        let tx = tx.record()?;
        let id = bytes(tx.field("id")?)?;
        let result = optional_record(tx.field("exec_result")?)?
            .map(TransactionResult::from_record)
            .transpose()?;
        let deploy = optional_record(tx.field("deploy")?)?;
        let read_cost = deploy
            .map(|v| {
                v.num_bytes()
                    .map(|n| n.wrapping_mul(params.min_txfee_read_kbyte) / 1000)
            })
            .transpose()?
            .unwrap_or(0);
        if let Some(result) = &result {
            if let Some(sender) = tx.field("sender")?.optional()? {
                receipt.inputs.push(Output {
                    address: bytes(sender)?,
                    contract: [0; 32],
                    amount: result.total_fee.into(),
                    memo: None,
                });
            }
        }
        if result.as_ref().is_none_or(|r| !r.did_fail) {
            for input in tx.field("inputs")?.seq()? {
                receipt.inputs.push(io(input.record()?)?);
            }
            for output in tx.field("outputs")?.seq()? {
                receipt.outputs.push(io(output.record()?)?);
            }
            if let Some(result) = &result {
                receipt
                    .inputs
                    .extend(result.inputs.iter().map(|v| v.output.clone()));
                receipt.outputs.extend(result.outputs.iter().cloned());
            }
            if let Some(deploy) = deploy {
                batch.contracts.push((
                    id,
                    ContractData {
                        bytes: deploy.encode()?,
                        read_cost,
                    },
                ));
            }
        }
        batch.transactions.push((
            id,
            TransactionIndex {
                height: 0,
                contract_read_cost: read_cost,
            },
        ));
    }
    (batch.balances, batch.supplies) = block::settle(storage, &mut receipt)?;
    Ok((receipt, batch))
}

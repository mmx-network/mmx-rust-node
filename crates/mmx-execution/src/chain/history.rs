//! Native address activity, committed atomically with the selected ledger.
//! Descending-height keys allow recent queries without scanning old activity.
use super::{invalid, ChainResult};
use crate::{
    block,
    transaction::{bytes, io, optional_record, uint},
};
use mmx_consensus::{Hash, Record, Value};
use mmx_db::Write;
use mmx_types::mmx;
use sha2::{Digest, Sha256};

pub const TABLE: &str = "chain_history";
pub const MEMO_TABLE: &str = "chain_history_memo";
pub const START_KEY: &[u8] = b"history_start";

pub fn prefix(address: &Hash, memo: Option<&[u8]>) -> Vec<u8> {
    let mut out = address.to_vec();
    if let Some(memo) = memo {
        out.extend(Sha256::digest(memo));
    }
    out
}
pub fn entry(
    io: &Record,
    txid: Hash,
    height: u32,
    time: Value,
    kind: mmx::tx_type_e,
    pending: bool,
) -> ChainResult<Record> {
    let mut fields = io.fields.clone();
    // Inputs have authorization fields that are not part of a history row.
    fields.retain(|k, _| matches!(k.as_str(), "address" | "contract" | "amount" | "memo"));
    fields.extend([
        ("txid".into(), Value::Bytes(txid.to_vec())),
        ("height".into(), Value::Uint(height.into())),
        ("time_stamp".into(), time),
        ("type".into(), Value::Uint(kind.value.into())),
        ("is_pending".into(), Value::Bool(pending)),
        ("is_validated".into(), Value::Bool(false)),
    ]);
    Ok(Record {
        name: "mmx.tx_entry_t".into(),
        fields,
    })
}
/// Includes fee-only failed transactions, execution transfers and all native
/// reward types. Strings remain lossless bytes throughout the native protocol.
pub fn entries(block: &Record, params: &mmx::ChainParams) -> ChainResult<Vec<Record>> {
    let height = u32::try_from(uint(block, "height")?).map_err(|_| invalid("history height"))?;
    let time = block.field("time_stamp")?.clone();
    let mut out = Vec::new();
    let mut reward_kinds = Vec::new();
    if block.field("vdf_reward_payout")?.optional()?.is_some() {
        reward_kinds.push(mmx::tx_type_e::VDF_REWARD);
    }
    if block.field("reward_addr")?.optional()?.is_some() && uint(block, "reward_amount")? != 0 {
        reward_kinds.push(mmx::tx_type_e::REWARD);
    }
    if block::economics::project_reward(params, uint(block, "tx_fees")?)? != 0 {
        reward_kinds.push(mmx::tx_type_e::PROJECT_REWARD);
    }
    for (reward, kind) in block::reward_outputs(params, block)?
        .iter()
        .zip(reward_kinds)
    {
        let io = Record {
            name: "mmx.txio_t".into(),
            fields: [
                ("address".into(), Value::Bytes(reward.address.to_vec())),
                ("contract".into(), Value::Bytes(reward.contract.to_vec())),
                (
                    "amount".into(),
                    Value::Bytes(reward.amount.to_le_bytes().to_vec()),
                ),
                ("memo".into(), Value::Optional(None)),
            ]
            .into(),
        };
        out.push(entry(
            &io,
            bytes(block.field("hash")?)?,
            height,
            time.clone(),
            kind,
            false,
        )?);
    }
    let mut inputs = Vec::new();
    for tx in block.field("tx_list")?.seq()? {
        for row in transaction_entries(tx.record()?, height, time.clone(), false)? {
            if row.field("type")?.uint()? == u64::from(mmx::tx_type_e::RECEIVE.value) {
                out.push(row);
            } else {
                inputs.push(row);
            }
        }
    }
    // Native Node::apply assigns ordinals to all outputs before all inputs.
    out.extend(inputs);
    Ok(out)
}
pub fn transaction_entries(
    tx: &Record,
    height: u32,
    time: Value,
    pending: bool,
) -> ChainResult<Vec<Record>> {
    let id = bytes(tx.field("id")?)?;
    let result = optional_record(tx.field("exec_result")?)?;
    let mut out = Vec::new();
    if !pending {
        if let (Some(result), Some(sender)) = (result, tx.field("sender")?.optional()?) {
            let fee = Record {
                name: "mmx.txio_t".into(),
                fields: [
                    ("address".into(), sender.clone()),
                    ("contract".into(), Value::Bytes(vec![0; 32])),
                    (
                        "amount".into(),
                        Value::Bytes(
                            u128::from(uint(result, "total_fee")?)
                                .to_le_bytes()
                                .to_vec(),
                        ),
                    ),
                    ("memo".into(), Value::Optional(None)),
                ]
                .into(),
            };
            out.push(entry(
                &fee,
                id,
                height,
                time.clone(),
                mmx::tx_type_e::TXFEE,
                false,
            )?);
        }
    }
    if result
        .map(|r| r.field("did_fail").and_then(Value::boolean))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(out);
    }
    for (field, kind) in [
        ("inputs", mmx::tx_type_e::SPEND),
        ("outputs", mmx::tx_type_e::RECEIVE),
    ] {
        for source in std::iter::once(tx).chain(result) {
            for io in source.field(field)?.seq()? {
                out.push(entry(
                    io.record()?,
                    id,
                    height,
                    time.clone(),
                    kind.clone(),
                    pending,
                )?);
            }
        }
    }
    Ok(out)
}
pub(super) fn writes(block: &Record, params: &mmx::ChainParams) -> ChainResult<Vec<Write>> {
    let height = uint(block, "height")? as u32;
    let mut writes = Vec::new();
    for (index, row) in entries(block, params)?.into_iter().enumerate() {
        // Also validate the native transfer fields before persisting the index.
        let transfer = io(&row)?;
        let ordinal = u32::try_from(index).map_err(|_| invalid("history ordinal overflow"))?;
        let suffix = [(!height).to_be_bytes(), ordinal.to_be_bytes()].concat();
        let key = [transfer.address.as_slice(), &suffix].concat();
        if let Some(memo) = transfer.memo {
            writes.push(Write::put(
                MEMO_TABLE,
                [prefix(&transfer.address, Some(&memo)), suffix].concat(),
                key.clone(),
            ));
        }
        writes.push(Write::put(TABLE, key, row.encode_compact()?));
    }
    Ok(writes)
}

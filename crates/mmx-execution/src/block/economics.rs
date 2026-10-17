//! Integer reward rules from utils.h and Node::calc_block_reward.
use super::{invalid, optional_address, BlockContext};
use crate::transaction::{bytes, uint, ValidationError};
use mmx_consensus::{Record, Value};
use mmx_types::mmx;
use mmx_vm::Address;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Ancestors are supplied newest first. Native tie-breaking uses the proof
/// hash of the oldest visited ancestor, even when history is shorter than the
/// payout interval. The chain layer is responsible for authenticating history.
pub fn vdf_reward_winner(interval: u32, ancestors: &[mmx::BlockHeader]) -> Option<Address> {
    let ancestors = &ancestors[..ancestors.len().min(interval as usize)];
    let salt = ancestors.last()?.proof_hash.0;
    let mut counts = BTreeMap::<Address, u32>::new();
    for header in ancestors {
        for address in &header.vdf_reward_addr {
            let count = counts.entry(address.0).or_default();
            *count = count.wrapping_add(1);
        }
    }
    let mut best = None;
    let mut max_count = 0;
    let mut min_hash = [0; 32];
    for (address, count) in counts {
        let hash: Address = Sha256::digest([address, salt].concat()).into();
        if count > max_count || (count == max_count && hash < min_hash) {
            best = Some(address);
            max_count = count;
            min_hash = hash;
        }
    }
    best
}

pub fn project_reward(p: &mmx::ChainParams, fees: u64) -> Result<u64, ValidationError> {
    if p.project_ratio.inverse == 0 {
        return Err(invalid("zero project ratio denominator"));
    }
    let dynamic =
        (p.project_ratio.value as u64).wrapping_mul(fees) / p.project_ratio.inverse as u64;
    Ok(p.fixed_project_reward.wrapping_add(dynamic).min(fees / 2))
}
pub fn min_reward_deduction(buffer: u64) -> u64 {
    (buffer / 8640).max(1000).min(buffer)
}
pub fn new_txfee_buffer(previous: &mmx::BlockHeader) -> u64 {
    previous
        .txfee_buffer
        .wrapping_sub(min_reward_deduction(previous.txfee_buffer))
        .wrapping_add(previous.tx_fees)
}
pub fn new_base_reward(
    p: &mmx::ChainParams,
    previous: &mmx::BlockHeader,
) -> Result<u64, ValidationError> {
    if previous.reward_vote_count < p.reward_adjust_interval / 2 {
        return Ok(previous.base_reward);
    }
    if p.reward_adjust_div == 0 {
        return Err(invalid("zero reward adjustment divisor"));
    }
    let step = (previous.base_reward / p.reward_adjust_div as u64) as i64;
    let step = step.max(p.reward_adjust_tick as i64);
    let reward = previous.base_reward as i64;
    Ok(match previous.reward_vote_sum.cmp(&0) {
        std::cmp::Ordering::Greater => reward.wrapping_add(step),
        std::cmp::Ordering::Less => reward.wrapping_sub(step),
        std::cmp::Ordering::Equal => reward,
    }
    .clamp(0, 4_200_000_000_000) as u64)
}
pub fn block_reward(
    p: &mmx::ChainParams,
    previous: &mmx::BlockHeader,
    height: u32,
    has_proof: bool,
    fees: u64,
) -> Result<u64, ValidationError> {
    if !has_proof || height < p.reward_activation {
        return Ok(0);
    }
    let reward = previous.base_reward.wrapping_add(
        p.min_reward
            .saturating_sub(min_reward_deduction(previous.txfee_buffer)),
    );
    let deduction = (fees / 2).max(project_reward(p, fees)?);
    Ok(reward.wrapping_add(fees.wrapping_sub(deduction)))
}
fn signed(r: &Record, name: &str) -> Result<i64, ValidationError> {
    match r.field(name)? {
        Value::Int(v) => Ok(*v),
        _ => Err(invalid("signed reward field")),
    }
}
pub(super) fn validate_fields<S: mmx_vm::Storage>(
    storage: &S,
    context: &BlockContext,
    block: &Record,
) -> Result<(), ValidationError> {
    let p = &context.params;
    let prev = &context.previous;
    let height = uint(block, "height")? as u32;
    if p.reward_adjust_interval == 0 || p.vdf_reward_interval == 0 {
        return Err(invalid("zero reward interval"));
    }
    if uint(block, "txfee_buffer")? != new_txfee_buffer(prev) {
        return Err(invalid("invalid txfee_buffer"));
    }
    let vote = signed(block, "reward_vote")?;
    if !(-1..=1).contains(&vote) {
        return Err(invalid("invalid reward_vote"));
    }
    crate::nft::validate(storage, context, block)?;
    let (base, sum, count) = if height.is_multiple_of(p.reward_adjust_interval) {
        (new_base_reward(p, prev)?, vote as i32, u32::from(vote != 0))
    } else {
        (
            prev.base_reward,
            prev.reward_vote_sum.wrapping_add(vote as i32),
            prev.reward_vote_count.wrapping_add(u32::from(vote != 0)),
        )
    };
    if uint(block, "base_reward")? != base
        || signed(block, "reward_vote_sum")? != sum as i64
        || uint(block, "reward_vote_count")? != count as u64
    {
        return Err(invalid("invalid reward votes or base_reward"));
    }
    if bytes::<32>(block.field("project_addr")?)? != prev.project_addr.0 {
        return Err(invalid("invalid project_addr"));
    }
    let payout = optional_address(block, "vdf_reward_payout")?;
    if height.is_multiple_of(p.vdf_reward_interval) {
        if payout != context.expected_vdf_payout {
            return Err(invalid("invalid vdf_reward_payout"));
        }
    } else if payout.is_some() {
        return Err(invalid("invalid vdf_reward_payout (must be null)"));
    }
    Ok(())
}

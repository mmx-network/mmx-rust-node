//! Pure header transition checks from Node::verify_proof. The context contains
//! challenge and VDF iteration expectations resolved from trusted chain history.
//! This verifies space work, but not the VDF output/work or reward targets.
pub mod history;
use crate::{
    proof::{self, bytes, invalid, ProofResult},
    Hash, Record,
};
use mmx_types::mmx;
use primitive_types::U256;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct ProofContext {
    pub parent: mmx::BlockHeader,
    pub iteration_counts: Vec<u64>,
    pub challenge: Hash,
    pub space_diff: u64,
}
pub fn proof_chain(
    params: &mmx::ChainParams,
    parent: &mmx::BlockHeader,
    proof_hash: &Hash,
) -> Hash {
    if parent.height < params.hardfork2_height {
        return *proof_hash;
    }
    let mut h = Sha256::new();
    h.update(b"proof_chain");
    h.update(parent.proof_chain.0);
    h.update(proof_hash);
    h.finalize().into()
}
pub fn next_challenge(
    params: &mmx::ChainParams,
    parent: &mmx::BlockHeader,
    height: u32,
    count: u32,
    proof_hash: &Hash,
    chain: &Hash,
) -> ProofResult<(Hash, bool)> {
    if params.challenge_interval == 0 {
        return Err(invalid("zero challenge interval"));
    }
    let mut out = parent.challenge.0;
    for _ in 0..count {
        out = Sha256::digest([b"next_challenge".as_slice(), &out].concat()).into();
    }
    let proof = if height >= params.hardfork2_height {
        chain
    } else {
        proof_hash
    };
    let hash: Hash =
        Sha256::digest([b"proof_infusion_check".as_slice(), &out, proof].concat()).into();
    let natural =
        U256::from_little_endian(&hash) % U256::from(params.challenge_interval) == U256::zero();
    let forced = parent.space_fork_len.wrapping_add(count)
        > params
            .max_space_fork_len
            .wrapping_mul(params.challenge_interval);
    let fork = natural || forced && parent.height.wrapping_add(1) >= params.hardfork2_height;
    if fork {
        out = Sha256::digest([b"challenge_infusion".as_slice(), &out, proof].concat()).into();
    }
    Ok((out, fork))
}
pub fn new_space_diff(params: &mmx::ChainParams, parent: &mmx::BlockHeader) -> ProofResult<u64> {
    let diff = parent.space_diff;
    if diff >> 48 != 0 {
        return Ok(diff - diff / 1024);
    }
    if parent.space_fork_len == 0 {
        return Ok(diff);
    }
    let expected = parent.space_fork_len.wrapping_mul(params.avg_proof_count);
    if expected == 0 {
        return Err(invalid("zero expected proof count"));
    }
    let next = ((diff as u128 * parent.space_fork_proofs as u128) / expected as u128) as u64;
    let increase = parent.space_fork_proofs > expected;
    let delta = if increase {
        next.wrapping_sub(diff)
    } else {
        diff.wrapping_sub(next)
    };
    let divider = (params.challenge_interval.wrapping_mul(16) / parent.space_fork_len).max(1);
    let adjustment = (delta / divider as u64).max(1);
    Ok(if increase {
        diff.wrapping_add(adjustment)
    } else if diff > adjustment {
        diff - adjustment
    } else {
        1
    })
}
pub fn validate_diff_adjust(current: u64, previous: u64, max_diff_adjust: u32) -> ProofResult<()> {
    if max_diff_adjust >= 64 {
        return Err(invalid("invalid difficulty adjustment shift"));
    }
    let max = (previous >> max_diff_adjust).max(1);
    if current > previous && current - previous > max {
        return Err(invalid("invalid difficulty adjustment upwards"));
    }
    if previous > current && previous - current > max {
        return Err(invalid("invalid difficulty adjustment downwards"));
    }
    Ok(())
}
pub fn block_weight(
    params: &mmx::ChainParams,
    current: &Record,
    parent: &mmx::BlockHeader,
) -> ProofResult<u128> {
    let proofs = current.seq("proof")?;
    if proofs.is_empty() {
        return Ok(0);
    }
    let count = current.uint("vdf_count")?;
    if count == 0 || params.time_diff_constant == 0 {
        return Err(invalid("invalid iteration divisor"));
    }
    let num_iters = current.uint("vdf_iters")?.wrapping_sub(parent.vdf_iters) / count;
    Ok(((num_iters / params.time_diff_constant) as u128)
        .wrapping_mul(proofs[0].record()?.uint("difficulty")? as u128)
        .wrapping_mul(params.avg_proof_count as u128))
}
/// Match Node::verify_proof(BlockHeader) using history-derived inputs, then
/// verify every OG/NFT space proof. Header hashes/signatures are a separate step.
pub fn verify_header(
    current: &Record,
    params: &mmx::ChainParams,
    context: &ProofContext,
) -> ProofResult<()> {
    let parent = &context.parent;
    if bytes::<32>(current, "prev")? != parent.hash.0 {
        return Err(invalid("cannot verify"));
    }
    let count = current.uint("vdf_count")?;
    if count == 0 || count > params.max_vdf_count as u64 {
        return Err(invalid("invalid vdf_count"));
    }
    if current.uint("vdf_height")? != parent.vdf_height.wrapping_add(count as u32) as u64 {
        return Err(invalid("invalid vdf_height"));
    }
    let proofs = current.seq("proof")?;
    if proofs.is_empty() {
        return Err(invalid("missing proof"));
    }
    if proofs.len() > params.max_proof_count as usize {
        return Err(invalid("too many proofs"));
    }
    let mut previous = [0; 32];
    let mut seen = std::collections::BTreeSet::new();
    for p in proofs {
        let hash = proof::calc_proof_hash(p.record()?)?;
        if hash < previous {
            return Err(invalid("invalid proof order"));
        }
        if seen.is_empty() && bytes::<32>(current, "proof_hash")? != hash {
            return Err(invalid("invalid proof_hash"));
        }
        if !seen.insert(hash) {
            return Err(invalid("duplicate proof"));
        }
        previous = hash;
    }
    let hash = bytes(current, "proof_hash")?;
    let chain = bytes(current, "proof_chain")?;
    let height = current.uint("height")? as u32;
    if height >= params.hardfork2_height && chain != proof_chain(params, parent, &hash) {
        return Err(invalid("invalid proof_chain"));
    }
    if context.iteration_counts.len() != count as usize {
        return Err(invalid("missing infusion iterations"));
    }
    let expected = context
        .iteration_counts
        .iter()
        .fold(parent.vdf_iters, |a, b| a.wrapping_add(*b));
    if current.uint("vdf_iters")? != expected {
        return Err(invalid("invalid vdf_iters"));
    }
    let time = current.uint("time_diff")?;
    if time < params.time_diff_divider {
        return Err(invalid("time_diff too low"));
    }
    validate_diff_adjust(time, parent.time_diff, params.max_diff_adjust)?;
    let (challenge, fork) = next_challenge(params, parent, height, count as u32, &hash, &chain)?;
    if bytes::<32>(current, "challenge")? != challenge {
        return Err(invalid("invalid challenge"));
    }
    if current.field("is_space_fork")?.boolean()? != fork {
        return Err(invalid("invalid is_space_fork"));
    }
    if fork {
        if current.uint("space_diff")? != new_space_diff(params, parent)? {
            return Err(invalid("invalid space_diff"));
        }
        if current.uint("space_fork_len")? != count {
            return Err(invalid("invalid space_fork_len at space fork"));
        }
        if current.uint("space_fork_proofs")? != proofs.len() as u64 {
            return Err(invalid("invalid space_fork_proofs at space fork"));
        }
    } else {
        if current.uint("space_diff")? != parent.space_diff {
            return Err(invalid("invalid space_diff change"));
        }
        if current.uint("space_fork_len")?
            != parent.space_fork_len.wrapping_add(count as u32) as u64
        {
            return Err(invalid("invalid space_fork_len"));
        }
        if current.uint("space_fork_proofs")?
            != parent.space_fork_proofs as u64 + proofs.len() as u64
        {
            return Err(invalid("invalid space_fork_proofs"));
        }
    }
    let weight = u128::from_le_bytes(bytes(current, "weight")?);
    if weight != block_weight(params, current, parent)? {
        return Err(invalid("invalid block weight"));
    }
    if u128::from_le_bytes(bytes(current, "total_weight")?)
        != u128::from_le_bytes(parent.total_weight.0).wrapping_add(weight)
    {
        return Err(invalid("invalid total weight"));
    }
    for p in proofs {
        proof::verify(
            p.record()?,
            params,
            &context.challenge,
            context.space_diff,
            current.uint("vdf_height")? as u32,
        )?;
    }
    Ok(())
}

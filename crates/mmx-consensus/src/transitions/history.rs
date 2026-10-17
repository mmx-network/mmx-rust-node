//! Challenge/infusion lookup over authenticated ancestor headers. Storage and
//! chain selection belong to the caller; this module never invents missing
//! non-genesis ancestors.
use super::ProofContext;
use crate::{
    proof::{bytes, invalid, ProofResult},
    Hash, Record,
};
use mmx_types::mmx;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub trait HeaderSource {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>>;
    /// Share immutable ancestry without copying its space-proof payloads.
    fn get_shared_header(&self, hash: &Hash) -> ProofResult<Option<Arc<mmx::BlockHeader>>> {
        Ok(self.get_header(hash)?.map(Arc::new))
    }
}
impl HeaderSource for std::collections::BTreeMap<Hash, mmx::BlockHeader> {
    fn get_header(&self, hash: &Hash) -> ProofResult<Option<mmx::BlockHeader>> {
        Ok(self.get(hash).cloned())
    }
}
fn step(prefix: &[u8], value: &Hash) -> Hash {
    Sha256::digest([prefix, value].concat()).into()
}
pub fn challenge<S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    current: &mmx::BlockHeader,
    offset: u32,
) -> ProofResult<(Hash, u64)> {
    if offset > params.challenge_delay {
        let advance = offset - params.challenge_delay;
        if advance > params.max_vdf_count {
            return Err(invalid("cannot find challenge"));
        }
        let mut value = current.challenge.0;
        for _ in 0..advance {
            value = step(b"next_challenge", &value);
        }
        return Ok((value, current.space_diff));
    }
    let target = params.challenge_delay - offset;
    let mut chain = vec![Arc::new(current.clone())];
    for _ in 0..target {
        let last = chain.last().unwrap();
        if let Some(parent) = source.get_shared_header(&last.prev.0)? {
            chain.push(parent);
        } else if last.height != 0 {
            return Err(invalid("cannot find challenge"));
        } else {
            break;
        }
    }
    chain.reverse();
    let mut list = Vec::new();
    let mut previous: Option<&mmx::BlockHeader> = None;
    for block in &chain {
        if let Some(parent) = previous {
            let mut value = parent.challenge.0;
            for _ in 1..block.vdf_count {
                value = step(b"next_challenge", &value);
                list.push((value, parent.space_diff));
            }
        }
        list.push((block.challenge.0, block.space_diff));
        previous = Some(block);
    }
    list.reverse();
    while target as usize >= list.len() {
        let (value, diff) = *list.last().unwrap();
        list.push((step(b"prev_challenge", &value), diff));
    }
    Ok(list[target as usize])
}
#[derive(Clone, Debug)]
pub struct Infusion {
    pub hash: Hash,
    pub num_iters: u64,
}
pub fn infusion<S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    current: &mmx::BlockHeader,
    offset: u32,
) -> ProofResult<Infusion> {
    let mut block = Arc::new(current.clone());
    if offset < params.infuse_delay {
        let delta = params.infuse_delay - offset;
        let target = block.vdf_height.saturating_sub(delta);
        while block.vdf_height > target {
            block = source
                .get_shared_header(&block.prev.0)?
                .ok_or_else(|| invalid("cannot find infusion"))?;
        }
    }
    let hash = block.hash.0;
    Ok(Infusion {
        hash,
        num_iters: block_iters(source, params, &block)?,
    })
}
/// Native get_time_diff/get_block_iters, including genesis clamping and u64
/// multiplication. `infused` must be an authenticated ancestor.
pub fn block_iters<S: HeaderSource>(
    source: &S,
    params: &mmx::ChainParams,
    infused: &mmx::BlockHeader,
) -> ProofResult<u64> {
    let mut ancestor: Option<Arc<mmx::BlockHeader>> = None;
    for _ in 0..params.commit_delay {
        let block = ancestor.as_deref().unwrap_or(infused);
        if block.height == 0 {
            break;
        }
        ancestor = Some(
            source
                .get_shared_header(&block.prev.0)?
                .ok_or_else(|| invalid("cannot get time difficulty"))?,
        );
    }
    let block = ancestor.as_deref().unwrap_or(infused);
    if params.time_diff_divider == 0 {
        return Err(invalid("zero time difficulty divider"));
    }
    Ok((block.time_diff / params.time_diff_divider).wrapping_mul(params.time_diff_constant))
}
impl ProofContext {
    /// Resolve exactly the Node::get_challenge/get_infusion inputs from a trusted
    /// header source. The candidate's hashes/signature are checked separately.
    pub fn resolve<S: HeaderSource>(
        record: &Record,
        params: &mmx::ChainParams,
        source: &S,
    ) -> ProofResult<Self> {
        let count = record.uint("vdf_count")?;
        if count == 0 || count > params.max_vdf_count as u64 {
            return Err(invalid("invalid vdf_count"));
        }
        let parent = source
            .get_header(&bytes(record, "prev")?)?
            .ok_or_else(|| invalid("cannot verify"))?;
        let current = mmx::BlockHeader {
            hash: bytes::<32>(record, "hash")?.into(),
            prev: bytes::<32>(record, "prev")?.into(),
            height: record.uint("height")? as u32,
            vdf_count: count as u32,
            challenge: bytes::<32>(record, "challenge")?.into(),
            space_diff: record.uint("space_diff")?,
            ..Default::default()
        };
        let (challenge, space_diff) = challenge(source, params, &current, 0)?;
        let iteration_counts = (0..count as u32)
            .map(|i| infusion(source, params, &parent, i).map(|v| v.num_iters))
            .collect::<ProofResult<_>>()?;
        Ok(Self {
            parent,
            iteration_counts,
            challenge,
            space_diff,
        })
    }
}

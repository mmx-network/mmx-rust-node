//! Native soft-root advancement policy. Roots affect fork choice and sync
//! scheduling; they never prune ledger history or prohibit a deeper rollback.
use super::{invalid, ChainResult};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub height: u32,
    pub vdf_height: u32,
    pub proofs: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub commit_threshold: u32,
    pub commit_delay: u32,
    pub max_future_sync: u32,
    pub avg_proof_count: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Advance {
    pub fork_weight: u32,
    pub commit_delay: u32,
    pub heights: Vec<u32>,
}
impl Policy {
    /// `line` is the selected branch after root in chronological order.
    /// `sync_pos` is the next height to fetch, and pending includes backwards jobs.
    pub fn advance(
        &self,
        root: Block,
        line: &[Block],
        synced: bool,
        sync_pos: u32,
        pending: &BTreeSet<u32>,
    ) -> ChainResult<Advance> {
        let Some(peak) = line.last() else {
            return Ok(Advance::default());
        };
        let mut height = root.height;
        let mut proofs = 0u64;
        for block in line {
            height = height
                .checked_add(1)
                .ok_or_else(|| invalid("root height overflow"))?;
            if block.height != height {
                return Err(invalid("noncontiguous root branch"));
            }
            proofs = proofs.wrapping_add(block.proofs);
        }
        // Match C++ operand widths: VDF delta and denominator multiply in u32;
        // the numerator multiplies in u64, then the quotient narrows to u32.
        let denominator = peak
            .vdf_height
            .wrapping_sub(root.vdf_height)
            .wrapping_mul(self.avg_proof_count);
        if denominator == 0 {
            return Err(invalid("zero root proof-density denominator"));
        }
        let fork_weight = (proofs.wrapping_mul(100) / u64::from(denominator)) as u32;
        let commit_delay = if fork_weight >= self.commit_threshold {
            self.commit_delay
        } else {
            self.max_future_sync.wrapping_mul(3)
        };
        let heights = line
            .iter()
            .take_while(|b| {
                b.height.wrapping_add(commit_delay) <= peak.height
                    && !pending.contains(&b.height)
                    && (synced || b.height < sync_pos)
            })
            .map(|b| b.height)
            .collect();
        Ok(Advance {
            fork_weight,
            commit_delay,
            heights,
        })
    }
}

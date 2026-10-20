//! Native verified-input-height / timelord-trust scheduling. Claimed VDF
//! heights never enter priority, and transport receipts cannot earn trust.
use super::*;
use std::{cmp::Reverse, collections::BinaryHeap};
type Key = [u8; 33];
type PointIdentity = (Hash, Hash, u64, u64, Hash, Hash);
fn point_identity(work: &Record) -> Result<PointIdentity> {
    Ok((
        hash(work, "input")?,
        mmx_consensus::vdf::output(work)?,
        work.field("start")?.uint()?,
        work.field("num_iters")?.uint()?,
        hash(work, "prev")?,
        hash(work, "reward_addr")?,
    ))
}
const MAX_TIMELORDS: usize = 100_000;

#[derive(Default)]
pub(super) struct Trust {
    scores: BTreeMap<Key, i64>,
    order: VecDeque<Key>,
}
impl Trust {
    pub(super) fn score(&self, key: &Key) -> i64 {
        self.scores.get(key).copied().unwrap_or(0)
    }
    fn update(&mut self, key: Key, apply: impl FnOnce(i64) -> i64) {
        if !self.scores.contains_key(&key) {
            if self.scores.len() == MAX_TIMELORDS {
                self.scores.remove(&self.order.pop_front().unwrap());
            }
            self.order.push_back(key);
        }
        self.scores.insert(key, apply(self.score(&key)));
    }
    fn submitted(&mut self, key: Key) {
        self.update(key, |n| n.saturating_sub(1));
    }
    fn advancing(&mut self, key: Key) {
        self.update(key, |n| n.saturating_add(2));
    }
    fn failed(&mut self, key: Key) {
        self.update(key, |n| n.min(0).saturating_sub(100));
    }
}
#[derive(Default)]
struct Ready {
    jobs: BinaryHeap<(u32, i64, Reverse<usize>)>,
    waiting: BTreeMap<Hash, Vec<(i64, usize)>>,
}
impl Ready {
    fn insert(&mut self, index: usize, input: Hash, trust: i64, heights: &BTreeMap<Hash, u32>) {
        if let Some(height) = heights.get(&input) {
            self.jobs.push((*height, trust, Reverse(index)));
        } else {
            self.waiting.entry(input).or_default().push((trust, index));
        }
    }
    fn advance(&mut self, output: Hash, height: u32) {
        if let Some(jobs) = self.waiting.remove(&output) {
            self.jobs.extend(
                jobs.into_iter()
                    .map(|(trust, index)| (height, trust, Reverse(index))),
            );
        }
    }
}

impl Node {
    pub(super) fn verify_vdfs(
        &mut self,
        tip: &mmx::BlockHeader,
        root: &mmx::BlockHeader,
    ) -> Result<()> {
        let now = vnx::util::get_wall_time_millis();
        let ttl = self
            .chain
            .selected()
            .params()
            .block_interval_ms
            .saturating_mul(10);
        self.work.retain(|(work, received, size)| {
            let keep = work
                .field("start")
                .and_then(Value::uint)
                .is_ok_and(|start| start >= root.vdf_iters)
                && (hash(work, "content_hash")
                    .is_ok_and(|id| self.verified_work.contains_key(&id))
                    || now.saturating_sub(*received) <= ttl);
            if !keep {
                self.work_bytes -= *size;
                if let Ok(id) = hash(work, "content_hash") {
                    self.verified_work.remove(&id);
                }
            }
            keep
        });
        let retained: BTreeSet<_> = self
            .work
            .iter()
            .map(|(w, _, _)| hash(w, "content_hash"))
            .collect::<Result<_>>()?;
        self.rejected_work.retain(|id| retained.contains(id));
        if let Some(worker) = &mut self.vdf_worker {
            for (id, key) in worker.poll(&retained) {
                // Eviction cannot erase an outstanding CPU failure penalty.
                self.vdf_trust.failed(key);
                if retained.contains(&id) {
                    self.rejected_work.insert(id);
                }
            }
        }
        if self.work.iter().all(|(work, _, _)| {
            hash(work, "content_hash").is_ok_and(|id| {
                self.verified_work.contains_key(&id) || self.rejected_work.contains(&id)
            })
        }) {
            return Ok(());
        }
        let selected = self.chain.selected();
        let mut heights = self.chain.verified_vdf_heights(self.root)?;
        for height in self.root..=tip.height {
            let header = selected
                .header(
                    &selected
                        .hash_at(height)?
                        .ok_or_else(|| error("missing selected height"))?,
                )?
                .ok_or_else(|| error("missing selected header"))?;
            heights.insert(header.vdf_output.0, header.vdf_height);
        }
        let points: Vec<_> = self.verified_work.values().cloned().collect();
        // Native find_vdf_point avoids repeating work for an already verified
        // input/output pair. Also require compatible linkage metadata so a
        // signed wrong-start point cannot suppress a correct replacement.
        let mut seen_points: BTreeSet<PointIdentity> = points
            .iter()
            .map(|p| {
                let p = p.point();
                (
                    p.input.0,
                    p.output.0,
                    p.start,
                    p.num_iters,
                    p.prev.0,
                    p.reward_addr.0,
                )
            })
            .collect();
        let next = mmx_consensus::vdf::next_points(&self.chain, selected.params(), tip, &points)?;
        let mut advance_output = next.last().map_or(tip.vdf_output.0, |p| p.point().output.0);
        let mut advance_count = next.len();
        let mut chronological: Vec<_> = points.iter().collect();
        chronological
            .sort_by_key(|point| point.point().start.wrapping_add(point.point().num_iters));
        for point in chronological {
            if let Some(height) = heights.get(&point.point().input.0).copied() {
                heights.insert(point.point().output.0, height.wrapping_add(1));
            }
        }
        let mut ready = Ready::default();
        for (index, (work, _, _)) in self.work.iter().enumerate() {
            let id = hash(work, "content_hash")?;
            if self.verified_work.contains_key(&id) || self.rejected_work.contains(&id) {
                continue;
            }
            if seen_points.contains(&point_identity(work)?) {
                self.rejected_work.insert(id);
                continue;
            }
            let key: Key = work.field("timelord_key")?.bytes()?.try_into()?;
            ready.insert(
                index,
                hash(work, "input")?,
                self.vdf_trust.score(&key),
                &heights,
            );
        }
        while let Some((height, _, Reverse(index))) = ready.jobs.pop() {
            let work = &self.work[index].0;
            let id = hash(work, "content_hash")?;
            let key: Key = work.field("timelord_key")?.bytes()?.try_into()?;
            if seen_points.contains(&point_identity(work)?) {
                self.rejected_work.insert(id);
                continue;
            }
            // Native's pending-job guard avoids repeat history/static reads
            // while both CPU/GPU slots are busy. Completed evidence still gets
            // admitted even when another pair of jobs has already been queued.
            if self
                .vdf_worker
                .as_ref()
                .is_some_and(|worker| !worker.can_process(&id))
            {
                continue;
            }
            // Missing infusion history waits; native does not charge trust for
            // a static rejection or put it ahead of authenticated chain work.
            if mmx_consensus::transitions::history::HeaderSource::get_header(
                &self.chain,
                &hash(work, "prev")?,
            )?
            .is_none()
            {
                continue;
            }
            match mmx_consensus::vdf::validate(work, self.chain.selected().params(), &self.chain) {
                Ok(()) => (),
                Err(mmx_consensus::vdf::VdfError::History(_)) => continue,
                Err(mmx_consensus::vdf::VdfError::Input(mmx_consensus::Error::Unsupported(_))) => {
                    continue
                }
                Err(_) => {
                    self.rejected_work.insert(id);
                    continue;
                }
            }
            let point = if let Some(worker) = &mut self.vdf_worker {
                let (proof, submitted) = worker.proof(id, hash(work, "hash")?, work)?;
                if submitted {
                    self.vdf_trust.submitted(key);
                }
                proof.and_then(|proof| {
                    proof
                        .admit(self.chain.selected().params(), &self.chain)
                        .ok()
                })
            } else {
                self.vdf_trust.submitted(key);
                match mmx_consensus::vdf::WorkVerifiedProof::verify(work.clone()) {
                    Ok(proof) => proof
                        .admit(self.chain.selected().params(), &self.chain)
                        .ok(),
                    Err(_) => {
                        self.vdf_trust.failed(key);
                        self.rejected_work.insert(id);
                        None
                    }
                }
            };
            if let Some(point) = point {
                let output = point.point().output.0;
                if point.point().input.0 == advance_output
                    && advance_count < (self.chain.selected().params().max_vdf_count as usize)
                {
                    self.vdf_trust.advancing(key);
                    advance_output = output;
                    advance_count += 1;
                }
                heights.insert(output, height.wrapping_add(1));
                ready.advance(output, height.wrapping_add(1));
                seen_points.insert(point_identity(work)?);
                self.verified_work.insert(id, point);
            }
        }
        self.verified_work
            .retain(|_, point| point.point().start >= root.vdf_iters);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_input_height_precedes_trust_and_completion_unblocks_only_its_output() {
        let heights = BTreeMap::from([([1; 32], 4), ([2; 32], 8)]);
        let mut ready = Ready::default();
        ready.insert(0, [1; 32], 1000, &heights);
        ready.insert(1, [2; 32], -100, &heights);
        ready.insert(2, [2; 32], 3, &heights);
        ready.insert(3, [9; 32], i64::MAX, &heights);
        assert_eq!(ready.jobs.pop(), Some((8, 3, Reverse(2))));
        assert_eq!(ready.jobs.pop(), Some((8, -100, Reverse(1))));
        ready.advance([8; 32], u32::MAX);
        assert_eq!(ready.jobs.pop(), Some((4, 1000, Reverse(0))));
        assert!(
            ready.jobs.is_empty(),
            "unverified claimed heights have no scheduling input"
        );
        ready.advance([9; 32], 9);
        assert_eq!(ready.jobs.pop(), Some((9, i64::MAX, Reverse(3))));
    }
    #[test]
    fn native_trust_charges_submission_rewards_advancement_and_penalizes_failure() {
        let mut trust = Trust::default();
        let key = [1; 33];
        trust.submitted(key);
        assert_eq!(trust.score(&key), -1);
        trust.advancing(key);
        assert_eq!(trust.score(&key), 1);
        trust.submitted(key);
        assert_eq!(trust.score(&key), 0);
        trust.failed(key);
        assert_eq!(trust.score(&key), -100);
        trust.failed(key);
        assert_eq!(trust.score(&key), -200);
        trust.update(key, |_| 100);
        trust.failed(key);
        assert_eq!(trust.score(&key), -100);
        trust.update(key, |_| i64::MIN);
        trust.failed(key);
        assert_eq!(trust.score(&key), i64::MIN);
    }
}

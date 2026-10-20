use crate::{error, Result};
use mmx_types::mmx;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
type Hash = [u8; 32];

#[derive(Clone)]
pub struct Config {
    pub segment_iters: u32,
    pub max_history: usize,
    pub reward_addr: Hash,
}
impl Config {
    pub fn new(segment_iters: u32) -> Self {
        Self {
            segment_iters,
            max_history: 1_000_000,
            reward_addr: [0; 32],
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub running: bool,
    pub waiting: bool,
    pub iterations: u64,
    pub generation: u64,
    pub resets: u64,
    pub stale_jobs: u64,
    pub history: usize,
    pub pending: usize,
    pub infusions: usize,
    pub hashes_per_sec: u64,
}
#[derive(Clone, Debug)]
struct Point {
    iterations: u64,
    output: Hash,
}
/// Opaque plan; completion only applies if both its generation and parent match.
pub struct Job {
    pub(crate) generation: u64,
    pub(crate) start: u64,
    pub(crate) input: Hash,
    pub(crate) end: u64,
}
impl Job {
    /// Deterministic synchronous stepping for embedded callers and witnesses.
    pub fn compute(&self) -> Hash {
        let mut output = self.input;
        mmx_crypto::recursive::hash_one(&mut output, self.end - self.start);
        output
    }
}
pub struct Completed {
    pub generation: u64,
    pub proof: mmx::ProofOfTime,
}
/// Contiguous segment history: O(1) lookup/append/eviction, 32 bytes per point
/// rather than native's tree allocation and absolute counter per segment.
#[derive(Default)]
struct History {
    begin: u64,
    points: VecDeque<Hash>,
}
impl History {
    fn reset(&mut self, point: &Point) {
        self.begin = point.iterations;
        self.points.clear();
        self.points.push_back(point.output);
    }
    fn index(&self, iterations: u64, step: u64) -> Option<usize> {
        let delta = iterations.checked_sub(self.begin)?;
        if delta % step != 0 {
            return None;
        }
        let index = usize::try_from(delta / step).ok()?;
        (index < self.points.len()).then_some(index)
    }
    fn get(&self, iterations: u64, step: u64) -> Option<Hash> {
        self.index(iterations, step).map(|i| self.points[i])
    }
}
pub struct Engine {
    config: Config,
    history: History,
    infuse: BTreeMap<u64, Hash>,
    pending: BTreeMap<u64, mmx::IntervalRequest>,
    peak: Option<Point>,
    published_end: u64,
    forecast_end: u64,
    generation: u64,
    resets: u64,
    stale_jobs: u64,
    speed: u64,
}
fn remove_between<T>(map: &mut BTreeMap<u64, T>, start: u64, end: u64) {
    use std::ops::Bound::Excluded;
    let keys: Vec<_> = map
        .range((Excluded(start), Excluded(end)))
        .map(|(k, _)| *k)
        .collect();
    for key in keys {
        map.remove(&key);
    }
}
impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        if config.segment_iters == 0 || !(2..=16_000_000).contains(&config.max_history) {
            return Err(error("invalid timelord segment size or history limit"));
        }
        Ok(Self {
            config,
            history: History::default(),
            infuse: BTreeMap::new(),
            pending: BTreeMap::new(),
            peak: None,
            published_end: 0,
            forecast_end: 0,
            generation: 0,
            resets: 0,
            stale_jobs: 0,
            speed: 0,
        })
    }
    pub fn segment_iters(&self) -> u32 {
        self.config.segment_iters
    }
    fn reset(&mut self, point: Point) {
        self.generation = self.generation.wrapping_add(1);
        self.resets = self.resets.saturating_add(1);
        self.history.reset(&point);
        let step = u64::from(self.config.segment_iters);
        self.pending
            .retain(|_, request| request.start.abs_diff(point.iterations) % step == 0);
        self.infuse
            .retain(|iterations, _| iterations.abs_diff(point.iterations) % step == 0);
        while self
            .pending
            .first_key_value()
            .is_some_and(|(end, _)| *end < point.iterations)
        {
            self.pending.pop_first();
        }
        self.forecast_end = point.iterations;
        self.peak = Some(point);
        self.published_end = 0;
    }
    pub fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.peak = None;
        self.history.points.clear();
        self.infuse.clear();
        self.pending.clear();
        self.published_end = 0;
        self.forecast_end = 0;
    }
    /// Native two-second publication reset permits a requested proof to be sent
    /// again after a missed connection; normal duplicate requests cost no hashing.
    pub fn refresh_publication(&mut self) {
        self.published_end = 0;
    }
    pub fn submit(&mut self, req: mmx::IntervalRequest) -> Result<()> {
        let step = u64::from(self.config.segment_iters);
        let span = req
            .end
            .checked_sub(req.start)
            .ok_or_else(|| error("reversed VDF interval"))?;
        if req.vdf_height == 0
            || span == 0
            || span % step != 0
            || span / step >= self.config.max_history as u64
        {
            return Err(error("invalid VDF interval shape or retained-history span"));
        }
        if self.pending.len() >= 1000 && !self.pending.contains_key(&req.end) && req.input.is_none()
        {
            return Err(error("too many pending VDF intervals"));
        }
        let known = self.history.get(req.start, step);
        let passed = self
            .peak
            .as_ref()
            .is_some_and(|p| req.start <= p.iterations);
        let changed = passed
            && (self.infuse.get(&req.start) != Some(&req.infuse.0)
                || self
                    .infuse
                    .range((
                        std::ops::Bound::Excluded(req.start),
                        std::ops::Bound::Excluded(req.end),
                    ))
                    .any(|(iterations, _)| {
                        self.peak
                            .as_ref()
                            .is_some_and(|p| *iterations <= p.iterations)
                    }));
        // Never reproduce native history[start]'s zero insertion when history
        // has expired: restart only from an actual retained or supplied input.
        let anchor = if let Some(input) = &req.input {
            let ahead = self.peak.as_ref().is_none_or(|p| req.start > p.iterations);
            (changed || ahead || known != Some(input.0)).then_some(input.0)
        } else if changed {
            Some(known.ok_or_else(|| error("VDF restart input is no longer retained"))?)
        } else {
            None
        };
        if anchor.is_none()
            && self
                .peak
                .as_ref()
                .is_some_and(|p| req.start.abs_diff(p.iterations) % step != 0)
        {
            return Err(error("VDF request is not on the active segment lattice"));
        }
        remove_between(&mut self.infuse, req.start, req.end);
        remove_between(&mut self.pending, req.start, req.end);
        self.infuse.insert(req.start, req.infuse.0);
        if let Some(output) = anchor {
            self.reset(Point {
                iterations: req.start,
                output,
            });
        }
        self.forecast_end = self.forecast_end.max(req.end);
        if req.end > self.published_end {
            self.pending.insert(req.end, req);
        }
        while self.pending.len() > 1000 {
            self.pending.pop_last();
        }
        if let Some((end, _)) = self.pending.last_key_value() {
            self.forecast_end = self.forecast_end.max(*end);
        }
        while self.infuse.len() > 1000 {
            if self
                .infuse
                .first_key_value()
                .is_some_and(|(start, _)| *start < self.history.begin)
            {
                self.infuse.pop_first();
            } else {
                self.infuse.pop_last();
            }
        }
        Ok(())
    }
    pub fn job(&self) -> Option<Job> {
        let point = self.peak.as_ref()?;
        if point.iterations >= self.forecast_end {
            return None;
        }
        let end = point
            .iterations
            .checked_add(u64::from(self.config.segment_iters))?;
        let mut input = point.output;
        if let Some(infusion) = self.infuse.get(&point.iterations) {
            let mut hash = Sha256::new();
            hash.update(input);
            hash.update(infusion);
            let mut reward = Sha256::new();
            reward.update(hash.finalize());
            reward.update(self.config.reward_addr);
            input = reward.finalize().into();
        }
        Some(Job {
            generation: self.generation,
            start: point.iterations,
            input,
            end,
        })
    }
    pub fn complete(&mut self, job: &Job, output: Hash, elapsed: std::time::Duration) -> bool {
        if job.generation != self.generation
            || self.peak.as_ref().is_none_or(|p| p.iterations != job.start)
        {
            self.stale_jobs = self.stale_jobs.saturating_add(1);
            return false;
        }
        self.peak = Some(Point {
            iterations: job.end,
            output,
        });
        if self.history.points.len() == self.config.max_history {
            self.history.points.pop_front();
            self.history.begin += u64::from(self.config.segment_iters);
            while self
                .pending
                .first_key_value()
                .is_some_and(|(end, _)| *end < self.history.begin)
            {
                self.pending.pop_first();
            }
        }
        self.history.points.push_back(output);
        if !elapsed.is_zero() {
            let speed = (u128::from(self.config.segment_iters) * 1_000_000_000 / elapsed.as_nanos())
                .min(u128::from(u64::MAX)) as u64;
            self.speed = if self.speed == 0 {
                speed
            } else {
                ((u128::from(self.speed) * 1023 + u128::from(speed)) / 1024) as u64
            };
        }
        self.ready()
    }
    pub(crate) fn ready(&self) -> bool {
        self.pending.first_key_value().is_some_and(|(end, _)| {
            self.history
                .get(*end, u64::from(self.config.segment_iters))
                .is_some()
        })
    }
    pub fn take_completed(&mut self, max: usize) -> Vec<Completed> {
        let mut out = Vec::new();
        let step = u64::from(self.config.segment_iters);
        while out.len() < max && self.ready() {
            let (_, req) = self.pending.pop_first().unwrap();
            // A newer forecast can split or replace this older interval. Never
            // sign a segment list containing an infusion inside its CPU chain.
            if self.infuse.get(&req.start) != Some(&req.infuse.0)
                || self
                    .infuse
                    .range((
                        std::ops::Bound::Excluded(req.start),
                        std::ops::Bound::Excluded(req.end),
                    ))
                    .next()
                    .is_some()
            {
                continue;
            }
            let Some(begin) = self.history.index(req.start, step) else {
                continue;
            };
            let end = self.history.index(req.end, step).unwrap();
            let segments = self
                .history
                .points
                .range(begin + 1..=end)
                .map(|h| (*h).into())
                .collect();
            let proof = mmx::ProofOfTime {
                vdf_height: req.vdf_height,
                start: req.start,
                num_iters: req.end - req.start,
                segment_size: self.config.segment_iters,
                input: self.history.points[begin].into(),
                prev: req.infuse,
                reward_addr: self.config.reward_addr.into(),
                segments,
                ..Default::default()
            };
            self.published_end = req.end;
            out.push(Completed {
                generation: self.generation,
                proof,
            });
        }
        out
    }
    pub fn status(&self) -> Status {
        Status {
            running: self.peak.is_some(),
            waiting: self
                .peak
                .as_ref()
                .is_some_and(|p| p.iterations >= self.forecast_end),
            iterations: self.peak.as_ref().map_or(0, |p| p.iterations),
            generation: self.generation,
            resets: self.resets,
            stale_jobs: self.stale_jobs,
            history: self.history.points.len(),
            pending: self.pending.len(),
            infusions: self.infuse.len(),
            hashes_per_sec: self.speed,
        }
    }
}

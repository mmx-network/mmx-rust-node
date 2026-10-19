//! Native per-plot dispatch, without a batch join in the service loop. Queued
//! contexts and in-flight plots are bounded separately; expired pending work
//! retires even when one old OS read never returns.
use super::{LookupContext, LookupJob};
use crate::{error, Result};
use mmx_consensus::Record;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
const MAX_QUEUED: usize = 8;
#[derive(Debug)]
pub struct LookupReport {
    pub vdf_height: u32,
    pub elapsed: Duration,
    pub found: usize,
    pub skipped: usize,
}
type Emit = Arc<dyn Fn(Record) + Send + Sync>;
type Done = Arc<dyn Fn(LookupReport) + Send + Sync>;
struct Batch {
    lookup: Arc<LookupContext>,
    remaining: AtomicUsize,
    found: AtomicUsize,
    skipped: AtomicUsize,
    emit: Emit,
    done: Done,
}
impl Batch {
    fn finish(&self, count: usize, skipped: bool) {
        if skipped {
            self.skipped.fetch_add(count, Ordering::Relaxed);
        }
        if self.remaining.fetch_sub(count, Ordering::AcqRel) == count {
            (self.done)(LookupReport {
                vdf_height: self.lookup.request.vdf_height,
                elapsed: self.lookup.started.elapsed(),
                found: self.found.load(Ordering::Acquire),
                skipped: self.skipped.load(Ordering::Acquire),
            });
        }
    }
}
struct Pending {
    batch: Arc<Batch>,
    plots: Vec<crate::plot::Prover>,
}
impl Pending {
    fn cancel(self) {
        self.batch.finish(self.plots.len(), true);
    }
}
// Only this lease owns the in-flight entry; completion never needs Harvester.
struct Flight {
    state: Arc<Mutex<State>>,
    plot: [u8; 32],
    batch: Arc<Batch>,
}
impl Drop for Flight {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.plot);
        // Refill on the completing worker. Waiting for the service tick leaves
        // idle workers between waves and loses Rayon's worker-local scheduling.
        drive(&self.state);
        self.batch.finish(1, false);
    }
}
struct State {
    jobs: BTreeMap<(u32, u64), Pending>,
    next: u64,
    active: BTreeSet<[u8; 32]>,
    workers: usize,
    closed: bool,
}
impl State {
    fn claim_next(&mut self) -> Option<(crate::plot::Prover, Arc<Batch>)> {
        if self.closed || self.active.len() >= self.workers {
            return None;
        }
        let (id, index) = self.jobs.iter().rev().find_map(|(id, job)| {
            if job.batch.lookup.expired() {
                return None;
            }
            job.plots
                .iter()
                .position(|p| !self.active.contains(&p.header.plot_id.0))
                .map(|index| (*id, index))
        })?;
        let job = self.jobs.get_mut(&id).unwrap();
        let plot = job.plots.swap_remove(index);
        let batch = job.batch.clone();
        self.active.insert(plot.header.plot_id.0);
        if job.plots.is_empty() {
            self.jobs.remove(&id);
        }
        Some((plot, batch))
    }
}
fn drive(shared: &Arc<Mutex<State>>) {
    let expired = {
        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<_> = state
            .jobs
            .iter()
            .filter(|(_, job)| job.batch.lookup.expired())
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .map(|id| state.jobs.remove(&id).unwrap())
            .collect::<Vec<_>>()
    };
    for job in expired {
        job.cancel();
    }
    loop {
        let next = shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .claim_next();
        let Some((plot, batch)) = next else {
            break;
        };
        let flight = Flight {
            state: shared.clone(),
            plot: plot.header.plot_id.0,
            batch,
        };
        rayon::spawn(move || {
            flight.batch.lookup.lookup_plot(&plot, &|proof| {
                flight.batch.found.fetch_add(1, Ordering::Relaxed);
                (flight.batch.emit)(proof);
            });
            drop(flight);
        });
    }
}
pub struct LookupScheduler {
    state: Arc<Mutex<State>>,
}
impl LookupScheduler {
    pub fn new(workers: usize) -> Result<Self> {
        if !(1..=256).contains(&workers) {
            return Err(error("lookup workers must be 1..256"));
        }
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                jobs: BTreeMap::new(),
                next: 0,
                active: BTreeSet::new(),
                workers,
                closed: false,
            })),
        })
    }
    pub fn active(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .len()
    }
    pub fn queued(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .jobs
            .len()
    }
    pub fn submit(&mut self, job: LookupJob, emit: Emit, done: Done) {
        let count = job.plots.len();
        let batch = Arc::new(Batch {
            lookup: job.context,
            remaining: AtomicUsize::new(count),
            found: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            emit,
            done,
        });
        if count == 0 {
            batch.finish(0, false);
            return;
        }
        let evicted = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.next = state.next.wrapping_add(1);
            let next = state.next;
            state.jobs.insert(
                (batch.lookup.request.vdf_height, next),
                Pending {
                    batch,
                    plots: job.plots,
                },
            );
            let mut evicted = Vec::new();
            while state.jobs.len() > MAX_QUEUED {
                evicted.push(state.jobs.pop_first().unwrap().1);
            }
            evicted
        };
        for job in evicted {
            job.cancel();
        }
    }
    pub fn clear_pending(&mut self) {
        let jobs = std::mem::take(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()).jobs);
        for job in jobs.into_values() {
            job.cancel();
        }
    }
    /// The service tick handles newly submitted/expired work. Completing workers
    /// independently refill capacity, so plot throughput never waits for a tick.
    pub fn poll(&mut self) -> Result<()> {
        drive(&self.state);
        Ok(())
    }
}
impl Drop for LookupScheduler {
    fn drop(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.clear_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::Prover;
    use std::sync::mpsc;
    fn prover() -> (tempfile::TempDir, Prover) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.plot");
        std::fs::write(&path, include_bytes!("../../tests/native/native-0.plot")).unwrap();
        let plot = Prover::open(path).unwrap();
        (dir, plot)
    }
    fn release(scheduler: &LookupScheduler, plot: Prover, batch: Arc<Batch>) {
        scheduler
            .state
            .lock()
            .unwrap()
            .active
            .remove(&plot.header.plot_id.0);
        batch.finish(1, false);
    }
    fn job(plot: &Prover, height: u32, window: Duration) -> LookupJob {
        LookupJob {
            context: Arc::new(LookupContext {
                request: mmx_types::mmx::Challenge {
                    vdf_height: height,
                    ..Default::default()
                },
                params: Default::default(),
                name: "scheduler test".into(),
                max_f1: 65536,
                started: std::time::Instant::now(),
                window,
            }),
            plots: vec![plot.clone()],
        }
    }
    #[test]
    fn newest_work_waits_for_its_plot_without_duplicate_io_or_unbounded_workers() {
        let (_dir, plot) = prover();
        let (sent, reports) = mpsc::channel();
        let done: Done = Arc::new(move |r| {
            let _ = sent.send(r);
        });
        let emit: Emit = Arc::new(|_| {});
        let mut scheduler = LookupScheduler::new(2).unwrap();
        for height in [1, 3, 2] {
            scheduler.submit(
                job(&plot, height, Duration::from_secs(30)),
                emit.clone(),
                done.clone(),
            );
        }
        // Taking the lease simulates a blocked OS read, without touching plots.
        let (first_plot, first) = scheduler.state.lock().unwrap().claim_next().unwrap();
        assert_eq!(first.lookup.request.vdf_height, 3);
        assert!(scheduler.state.lock().unwrap().claim_next().is_none());
        let mut other = plot.clone();
        other.header.plot_id.0[0] ^= 1;
        scheduler.submit(
            job(&other, 4, Duration::from_secs(30)),
            emit.clone(),
            done.clone(),
        );
        let (second_plot, second) = scheduler.state.lock().unwrap().claim_next().unwrap();
        assert_eq!(scheduler.active(), 2);
        assert_eq!(second.lookup.request.vdf_height, 4);
        assert!(scheduler.state.lock().unwrap().claim_next().is_none());
        // Queued expiry can retire independently of either old in-flight read.
        scheduler.submit(job(&plot, 9, Duration::ZERO), emit, done);
        scheduler.poll().unwrap();
        let expired = reports.try_recv().unwrap();
        assert_eq!((expired.vdf_height, expired.skipped), (9, 1));
        assert_eq!(scheduler.active(), 2);
        release(&scheduler, first_plot, first);
        let (third_plot, third) = scheduler.state.lock().unwrap().claim_next().unwrap();
        assert_eq!(third.lookup.request.vdf_height, 2);
        release(&scheduler, second_plot, second);
        release(&scheduler, third_plot, third);
        scheduler.clear_pending();
        assert_eq!(scheduler.active(), 0);
        assert_eq!(reports.try_iter().count(), 4);
    }
    #[test]
    fn pending_contexts_are_bounded_and_evictions_and_expiry_complete_once() {
        let (_dir, plot) = prover();
        let (sent, reports) = mpsc::channel();
        let done: Done = Arc::new(move |r| {
            let _ = sent.send(r);
        });
        let emit: Emit = Arc::new(|_| panic!("expired work must never read or emit"));
        let mut scheduler = LookupScheduler::new(2).unwrap();
        for height in 0..32 {
            scheduler.submit(
                job(&plot, height, Duration::ZERO),
                emit.clone(),
                done.clone(),
            );
            assert!(scheduler.queued() <= MAX_QUEUED);
        }
        assert_eq!(
            scheduler
                .state
                .lock()
                .unwrap()
                .jobs
                .first_key_value()
                .unwrap()
                .0
                 .0,
            24
        );
        scheduler.poll().unwrap();
        let all: Vec<_> = reports.try_iter().collect();
        assert_eq!(all.len(), 32);
        assert!(all.iter().all(|r| r.skipped == 1 && r.found == 0));
        assert_eq!(scheduler.queued(), 0);
        assert_eq!(scheduler.active(), 0);
        scheduler.clear_pending();
        assert!(reports.try_recv().is_err());
        assert!(LookupScheduler::new(0).is_err());
        assert!(LookupScheduler::new(257).is_err());
    }
}

//! Bounded CPU-only VDF work. The ledger admits returned evidence against its
//! current authenticated history; a worker never owns a database snapshot.
use crate::{error, Result};
use mmx_consensus::{vdf::CpuVerifiedProof, Hash, Record};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
};

pub(crate) struct Worker {
    jobs: SyncSender<(Hash, Record)>,
    results: Receiver<(Hash, mmx_consensus::vdf::VdfResult<CpuVerifiedProof>)>,
    pending: BTreeSet<Hash>,
    complete: BTreeMap<Hash, Option<CpuVerifiedProof>>,
}
impl Worker {
    pub fn new() -> Result<Self> {
        Self::spawn(CpuVerifiedProof::verify)
    }
    fn spawn(
        verify: impl Fn(Record) -> mmx_consensus::vdf::VdfResult<CpuVerifiedProof> + Send + 'static,
    ) -> Result<Self> {
        let (jobs, receive) = mpsc::sync_channel::<(Hash, Record)>(1);
        let (send, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("mmx-vdf".into())
            .spawn(move || {
                while let Ok((id, proof)) = receive.recv() {
                    if send.send((id, verify(proof))).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            results,
            pending: BTreeSet::new(),
            complete: BTreeMap::new(),
        })
    }
    pub fn poll(&mut self, retained: &BTreeSet<Hash>) {
        while let Ok((id, result)) = self.results.try_recv() {
            self.pending.remove(&id);
            if retained.contains(&id) {
                if let Err(error) = &result {
                    eprintln!("Rejected VDF CPU proof: {error}");
                }
                self.complete.insert(id, result.ok());
            }
        }
        self.complete.retain(|id, _| retained.contains(id));
    }
    pub fn proof(&mut self, id: Hash, record: &Record) -> Result<Option<&CpuVerifiedProof>> {
        if !self.complete.contains_key(&id) && !self.pending.contains(&id) && self.pending.len() < 2
        {
            match self.jobs.try_send((id, record.clone())) {
                Ok(()) => {
                    self.pending.insert(id);
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return Err(error("VDF worker stopped")),
            }
        }
        Ok(self.complete.get(&id).and_then(Option::as_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn blocked_cpu_does_not_block_admission_and_duplicate_and_queued_jobs_are_bounded() {
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let mut worker = Worker::spawn(move |record| {
            entered.send(()).unwrap();
            released.recv().unwrap();
            CpuVerifiedProof::verify(record)
        })
        .unwrap();
        let record = Record {
            name: "invalid".into(),
            fields: BTreeMap::new(),
        };
        assert!(worker.proof([0; 32], &record).unwrap().is_none());
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        // The first CPU task is deliberately blocked, independent of host speed.
        worker.proof([0; 32], &record).unwrap();
        worker.proof([1; 32], &record).unwrap();
        worker.proof([2; 32], &record).unwrap();
        assert_eq!(worker.pending.len(), 2);
        assert!(!worker.pending.contains(&[2; 32]));
        release.send(()).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        release.send(()).unwrap();
        let retained = [[0; 32], [1; 32]].into_iter().collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.pending.is_empty() {
            assert!(Instant::now() < deadline);
            worker.poll(&retained);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(worker.complete.len(), 2);
        worker.proof([0; 32], &record).unwrap();
        assert!(
            worker.pending.is_empty(),
            "failed CPU work must not repeat each tick"
        );
        worker.poll(&BTreeSet::new());
        assert!(worker.complete.is_empty());
    }
}

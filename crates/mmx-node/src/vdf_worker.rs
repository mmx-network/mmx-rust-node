//! Bounded CPU/OpenCL VDF work. The ledger admits returned evidence against its
//! current authenticated history; a worker never owns a database snapshot.
use crate::{error, Result};
use mmx_consensus::{accelerator::Accelerator, vdf::WorkVerifiedProof, Hash, Record};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
};

pub(crate) struct Worker {
    jobs: SyncSender<(Hash, Hash, Record)>,
    results: Receiver<(Hash, Hash, mmx_consensus::vdf::VdfResult<WorkVerifiedProof>)>,
    pending: BTreeSet<Hash>,
    pending_work: BTreeSet<Hash>,
    complete: BTreeMap<Hash, Option<WorkVerifiedProof>>,
}
impl Worker {
    pub fn new(accelerator: Option<Arc<Accelerator>>) -> Result<Self> {
        Self::spawn(move |record| match &accelerator {
            Some(a) => WorkVerifiedProof::verify_with(record, a),
            None => WorkVerifiedProof::verify(record),
        })
    }
    fn spawn(
        verify: impl Fn(Record) -> mmx_consensus::vdf::VdfResult<WorkVerifiedProof> + Send + 'static,
    ) -> Result<Self> {
        let (jobs, receive) = mpsc::sync_channel::<(Hash, Hash, Record)>(1);
        let (send, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("mmx-vdf".into())
            .spawn(move || {
                while let Ok((id, work, proof)) = receive.recv() {
                    if send.send((id, work, verify(proof))).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            results,
            pending: BTreeSet::new(),
            pending_work: BTreeSet::new(),
            complete: BTreeMap::new(),
        })
    }
    pub fn poll(&mut self, retained: &BTreeSet<Hash>) {
        while let Ok((id, work, result)) = self.results.try_recv() {
            self.pending.remove(&id);
            self.pending_work.remove(&work);
            if retained.contains(&id) {
                if let Err(error) = &result {
                    eprintln!("Rejected VDF work proof: {error}");
                }
                self.complete.insert(id, result.ok());
            }
        }
        self.complete.retain(|id, _| retained.contains(id));
    }
    pub fn proof(
        &mut self,
        id: Hash,
        work: Hash,
        record: &Record,
    ) -> Result<Option<&WorkVerifiedProof>> {
        if !self.complete.contains_key(&id)
            && !self.pending.contains(&id)
            && !self.pending_work.contains(&work)
            && self.pending.len() < 2
        {
            match self.jobs.try_send((id, work, record.clone())) {
                Ok(()) => {
                    self.pending.insert(id);
                    self.pending_work.insert(work);
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
            WorkVerifiedProof::verify(record)
        })
        .unwrap();
        let record = Record {
            name: "invalid".into(),
            fields: BTreeMap::new(),
        };
        assert!(worker.proof([0; 32], [0; 32], &record).unwrap().is_none());
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        // The first CPU task is deliberately blocked, independent of host speed.
        worker.proof([0; 32], [0; 32], &record).unwrap();
        // A different signed envelope of the same work must not consume the
        // remaining slot, including while the original queue entry expires.
        worker.proof([7; 32], [0; 32], &record).unwrap();
        assert_eq!(worker.pending.len(), 1);
        worker.poll(&BTreeSet::new());
        worker.proof([8; 32], [0; 32], &record).unwrap();
        assert_eq!(worker.pending.len(), 1);
        worker.proof([1; 32], [1; 32], &record).unwrap();
        worker.proof([2; 32], [2; 32], &record).unwrap();
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
        worker.proof([0; 32], [0; 32], &record).unwrap();
        assert!(
            worker.pending.is_empty(),
            "failed CPU work must not repeat each tick"
        );
        worker.poll(&BTreeSet::new());
        assert!(worker.complete.is_empty());
        assert!(worker.pending_work.is_empty());
    }
}

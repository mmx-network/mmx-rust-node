//! Bounded background connection attempts. A slow seed must not hold up the
//! existing sync pool, and three healthy peers must keep replenishing to eight.
use super::*;
use std::thread::JoinHandle;

pub(super) struct Connections<S> {
    jobs: BTreeMap<String, JoinHandle<Result<S>>>,
}
impl<S: Send + 'static> Connections<S> {
    pub fn new() -> Self {
        Self {
            jobs: BTreeMap::new(),
        }
    }
    pub fn refill(
        &mut self,
        active: usize,
        addresses: &[String],
        connected: &BTreeMap<Hash, String>,
        retry_at: &mut BTreeMap<String, Instant>,
        connect: impl Fn(&str) -> Result<S> + Send + Sync + 'static,
    ) -> Result<()> {
        let slots = MAX_PEERS.saturating_sub(active + self.jobs.len());
        if slots == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let mut candidates: Vec<_> = addresses
            .iter()
            .filter(|address| {
                !self.jobs.contains_key(*address)
                    && !connected.values().any(|a| a == *address)
                    && retry_at
                        .get(*address)
                        .is_none_or(|deadline| now >= *deadline)
            })
            .cloned()
            .collect();
        candidates.sort();
        candidates.dedup();
        candidates.shuffle(&mut rand::thread_rng());
        candidates.truncate(slots);
        let connect = Arc::new(connect);
        for address in candidates {
            let endpoint = address.clone();
            let connect = connect.clone();
            let job = thread::Builder::new()
                .name("mmx-peer-connect".into())
                .spawn(move || connect(&endpoint))?;
            retry_at.insert(address.clone(), now + Duration::from_secs(30));
            self.jobs.insert(address, job);
        }
        Ok(())
    }
    pub fn finished(&mut self) -> Vec<(String, Result<S>)> {
        let ready: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.is_finished())
            .map(|(address, _)| address.clone())
            .collect();
        ready
            .into_iter()
            .map(|address| {
                let result = self
                    .jobs
                    .remove(&address)
                    .unwrap()
                    .join()
                    .unwrap_or_else(|_| Err(error("peer connection worker panicked")));
                (address, result)
            })
            .collect()
    }
}
impl<S> Drop for Connections<S> {
    fn drop(&mut self) {
        for (_, job) in std::mem::take(&mut self.jobs) {
            let _ = job.join();
        }
    }
}

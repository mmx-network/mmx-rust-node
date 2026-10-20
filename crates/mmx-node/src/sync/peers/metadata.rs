//! At most one outstanding metadata operation per authenticated connection.
//! Late returns restore their owner and update status, but never count toward
//! a different height window's three independent observations.
use super::*;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread::JoinHandle,
};

#[derive(Clone, Copy)]
enum Kind {
    Status,
    Hashes(u32, u32),
}
enum Value {
    Status(Option<u32>),
    Hashes(Vec<Option<BlockHash>>),
}
struct Reply {
    id: Hash,
    query: u64,
    kind: Kind,
    elapsed: Duration,
    busy: u64,
    result: Result<Value>,
}
pub(super) struct Job<S> {
    query: u64,
    worker: JoinHandle<S>,
}
pub(super) struct Queue {
    sender: SyncSender<Reply>,
    receiver: Option<Receiver<Reply>>,
}
impl Queue {
    pub fn new() -> Self {
        let (sender, receiver) = sync_channel(MAX_PEERS);
        Self {
            sender,
            receiver: Some(receiver),
        }
    }
}
struct Observation {
    query: u64,
    peer: usize,
    hashes: Vec<Option<BlockHash>>,
}
impl<S: PeerSource> PeerPool<S> {
    fn next_query(&mut self) -> u64 {
        self.query = self.query.wrapping_add(1);
        self.query
    }
    fn start_metadata(&mut self, index: usize, query: u64, kind: Kind) -> Result<()> {
        let peer = &mut self.peers[index];
        if peer.inflight.is_some() {
            return Err(error("peer already owns a metadata operation"));
        }
        let mut source = peer
            .source
            .take()
            .ok_or_else(|| error("missing metadata connection owner"))?;
        let id = peer.id;
        let sender = self.metadata.sender.clone();
        let worker = thread::spawn(move || {
            let began = Instant::now();
            let (result, busy) = catch_unwind(AssertUnwindSafe(|| {
                let result = match kind {
                    Kind::Status => source.synced_height().map(Value::Status),
                    Kind::Hashes(start, count) => source.hashes(start, count).map(Value::Hashes),
                };
                (result, source.backpressure_retries())
            }))
            .unwrap_or_else(|_| (Err(error("peer metadata worker panicked")), 0));
            let _ = sender.send(Reply {
                id,
                query,
                kind,
                elapsed: began.elapsed(),
                busy,
                result,
            });
            source
        });
        peer.inflight = Some(Job { query, worker });
        Ok(())
    }
    fn accept_metadata(&mut self, reply: Reply) -> Result<Option<Observation>> {
        let Some(index) = self.peers.iter().position(|peer| peer.id == reply.id) else {
            return Ok(None);
        };
        let peer = &mut self.peers[index];
        // A removed connection can finish after another connection with the
        // same authenticated ID has been installed. Never take/join the newer
        // operation or restore an older owner from that stale completion.
        if peer
            .inflight
            .as_ref()
            .is_none_or(|job| job.query != reply.query)
        {
            return Ok(None);
        }
        let job = peer
            .inflight
            .take()
            .ok_or_else(|| error("unexpected peer metadata return"))?;
        peer.source = Some(
            job.worker
                .join()
                .map_err(|_| error("peer metadata owner panicked"))?,
        );
        peer.backpressure = peer.backpressure.max(reply.busy);
        if let Some(replacement) = peer.replacement.take() {
            self.retired_backpressure += peer.backpressure;
            peer.source = Some(replacement);
            peer.active = true;
            peer.qualified = false;
            peer.needs_status = true;
            peer.height = 0;
            peer.latency = Duration::MAX;
            peer.backpressure = 0;
            return Ok(None);
        }
        match reply.result {
            Ok(Value::Status(Some(height))) => {
                peer.height = height;
                peer.qualified = true;
                peer.needs_status = false;
            }
            Ok(Value::Status(None)) => {
                eprintln!("Sync peer {}: no longer synced", short_id(&peer.id));
                peer.active = false;
                peer.qualified = false;
            }
            Ok(Value::Hashes(hashes)) => {
                let Kind::Hashes(_, count) = reply.kind else {
                    return Err(error("invalid metadata return type"));
                };
                if hashes.len() != count as usize {
                    eprintln!("Sync peer {}: incomplete hash window", short_id(&peer.id));
                    peer.active = false;
                } else if peer.active && peer.qualified {
                    peer.latency = reply.elapsed;
                    return Ok(Some(Observation {
                        query: reply.query,
                        peer: index,
                        hashes,
                    }));
                }
            }
            Err(e) => {
                eprintln!(
                    "Sync peer {}: metadata request failed: {e}",
                    short_id(&peer.id)
                );
                peer.active = false;
                peer.qualified = false;
            }
        }
        Ok(None)
    }
    fn receive_metadata(&mut self, wait: Duration) -> Result<Option<Observation>> {
        match self.metadata.receiver.as_ref().unwrap().recv_timeout(wait) {
            Ok(reply) => self.accept_metadata(reply),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(error("peer metadata queue disconnected")),
        }
    }
    fn drain_metadata(&mut self) -> Result<Vec<Observation>> {
        let mut observations = Vec::new();
        loop {
            match self.metadata.receiver.as_ref().unwrap().try_recv() {
                Ok(reply) => {
                    if let Some(observation) = self.accept_metadata(reply)? {
                        observations.push(observation);
                    }
                }
                Err(TryRecvError::Empty) => return Ok(observations),
                Err(TryRecvError::Disconnected) => {
                    return Err(error("peer metadata queue disconnected"));
                }
            }
        }
    }
    pub(super) fn poll_metadata(&mut self) -> Result<()> {
        if self.peers.iter().any(|p| p.inflight.is_some()) {
            self.receive_metadata(Duration::from_millis(50))?;
        }
        self.drain_metadata()?;
        Ok(())
    }
    pub(super) fn drain_control(&mut self) -> Result<()> {
        self.drain_metadata()?;
        Ok(())
    }
    fn start_statuses(&mut self) -> Result<()> {
        for index in 0..self.peers.len() {
            let peer = &self.peers[index];
            if peer.active && peer.needs_status && peer.source.is_some() {
                let query = self.next_query();
                self.start_metadata(index, query, Kind::Status)?;
            }
        }
        Ok(())
    }
    pub fn refresh(&mut self) -> Result<u32> {
        self.drain_metadata()?;
        if self.refreshed.elapsed() >= Duration::from_secs(10) {
            for peer in &mut self.peers {
                if peer.active {
                    peer.needs_status = true;
                }
            }
            self.refreshed = Instant::now();
        }
        self.start_statuses()?;
        while self
            .peers
            .iter()
            .filter(|p| p.active && p.qualified)
            .count()
            < MIN_PEERS
        {
            if self.len() < MIN_PEERS || !self.peers.iter().any(|p| p.inflight.is_some()) {
                return Err(error("sync requires at least three distinct synced peers"));
            }
            self.receive_metadata(Duration::from_millis(50))?;
            self.start_statuses()?;
        }
        Ok(self
            .peers
            .iter()
            .filter(|p| p.active && p.qualified)
            .map(|p| p.height)
            .max()
            .unwrap_or(0))
    }
    pub(super) fn observations(
        &mut self,
        start: u32,
        count: u32,
        stop: &AtomicBool,
    ) -> Result<Vec<(usize, Vec<Option<BlockHash>>)>> {
        if count == 0 || count > BATCH_BLOCKS || start.checked_add(count - 1).is_none() {
            return Err(error("invalid coordinated hash window"));
        }
        self.drain_metadata()?;
        let query = self.next_query();
        let mut asked = BTreeSet::new();
        let mut replies = Vec::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(error("sync cancelled"));
            }
            self.start_statuses()?;
            let mut ready: Vec<_> = self
                .peers
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.active && p.qualified && p.source.is_some() && !asked.contains(&p.id)
                })
                .map(|(index, _)| index)
                .collect();
            ready.shuffle(&mut rand::thread_rng());
            for index in ready {
                asked.insert(self.peers[index].id);
                self.start_metadata(index, query, Kind::Hashes(start, count))?;
            }
            for observation in self.drain_metadata()? {
                if observation.query == query {
                    replies.push((observation.peer, observation.hashes));
                }
            }
            if replies.len() >= MIN_PEERS {
                return Ok(replies);
            }
            // Draining can return a stale connection owner after every current
            // job has finished. It still needs a fresh query for this window.
            let available = self
                .peers
                .iter()
                .any(|p| p.active && p.qualified && p.source.is_some() && !asked.contains(&p.id));
            if self.len() < MIN_PEERS
                || (!available && !self.peers.iter().any(|p| p.inflight.is_some()))
            {
                return Err(error("fewer than three peers answered the hash window"));
            }
            if available {
                continue;
            }
            if let Some(observation) = self.receive_metadata(Duration::from_millis(50))? {
                if observation.query == query {
                    replies.push((observation.peer, observation.hashes));
                }
            }
        }
    }
}
fn short_id(id: &Hash) -> String {
    vnx::util::to_hex_string(&id[..8], true, false)
}
impl<S> Drop for PeerPool<S> {
    fn drop(&mut self) {
        // Closing the receive end also releases a worker blocked on delivery.
        self.metadata.receiver.take();
        for peer in &mut self.peers {
            if let Some(job) = peer.inflight.take() {
                if let Ok(source) = job.worker.join() {
                    peer.source = Some(source);
                }
            }
        }
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};
    struct Source {
        height: u32,
        release: Option<Arc<(Mutex<bool>, Condvar)>>,
        started: Option<SyncSender<()>>,
    }
    impl PeerSource for Source {
        fn hashes(&mut self, _: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
            Ok(vec![None; count as usize])
        }
        fn fetch(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>> {
            Ok(vec![None; hashes.len()])
        }
        fn synced_height(&mut self) -> Result<Option<u32>> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
            }
            if let Some(release) = &self.release {
                let mut ready = release.0.lock().unwrap();
                while !*ready {
                    ready = release.1.wait(ready).unwrap();
                }
            }
            Ok(Some(self.height))
        }
    }
    #[test]
    fn a_reconnect_waits_for_its_old_owner_and_late_returns_cannot_replace_the_new_owner() {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, ready) = sync_channel(1);
        let mut pool = PeerPool::new(vec![(
            [1; 32],
            Source {
                height: 999,
                release: Some(release.clone()),
                started: Some(started),
            },
        )]);
        let old = pool.next_query();
        pool.start_metadata(0, old, Kind::Status).unwrap();
        ready.recv_timeout(Duration::from_secs(3)).unwrap();
        for id in [2u8, 3] {
            assert!(pool.add(
                [id; 32],
                Source {
                    height: 41,
                    release: None,
                    started: None
                }
            ));
        }
        assert!(pool.install_authenticated(
            [1; 32],
            Source {
                height: 42,
                release: None,
                started: None
            }
        ));
        assert_eq!(
            pool.len(),
            2,
            "a pending replacement is not an extra qualified observer"
        );
        pool.remove_failed();
        assert_eq!(
            pool.peers.len(),
            3,
            "keep the pending replacement and old owner bounded in one slot"
        );
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let deadline = Instant::now() + Duration::from_secs(3);
        while pool.peers[0].replacement.is_some() {
            assert!(Instant::now() < deadline);
            pool.poll().unwrap();
            std::thread::yield_now();
        }
        assert_eq!(
            pool.len(),
            3,
            "polling below the sync minimum must restore the replacement"
        );
        assert_eq!(pool.peers[0].source.as_ref().unwrap().height, 42);
        assert!(
            !pool.peers[0].qualified,
            "old advertised height must not qualify the replacement"
        );
        let fresh = pool.next_query();
        pool.start_metadata(0, fresh, Kind::Status).unwrap();
        assert!(pool
            .accept_metadata(Reply {
                id: [1; 32],
                query: old,
                kind: Kind::Status,
                elapsed: Duration::ZERO,
                busy: 0,
                result: Ok(Value::Status(Some(999)))
            })
            .unwrap()
            .is_none());
        assert_eq!(pool.peers[0].inflight.as_ref().unwrap().query, fresh);
        pool.receive_metadata(Duration::from_secs(3)).unwrap();
        assert_eq!(pool.peers[0].height, 42);
        assert!(pool.peers[0].qualified);
        assert_eq!(pool.peers.len(), 3);
    }
}

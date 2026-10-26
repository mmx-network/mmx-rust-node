//! Bounded body completion delivery independent of slow connection owners.
use super::*;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    thread::JoinHandle,
};

pub(super) type Key = (u32, Hash);
pub(super) type Failure = (Key, Box<dyn std::error::Error + Send + Sync>);
pub(super) struct Request {
    pub key: Key,
    pub block: Hash,
    pub heights: BTreeSet<u32>,
}
pub(super) struct Job<S> {
    query: u64,
    requested: Instant,
    requests: Vec<Request>,
    pending: BTreeSet<usize>,
    returned: usize,
    params: Arc<mmx_types::mmx::ChainParams>,
    busy_before: u64,
    metadata_latency: Duration,
    pub worker: JoinHandle<S>,
}
enum Reply {
    Body {
        id: Hash,
        query: u64,
        index: usize,
        value: Option<Record>,
    },
    Done {
        id: Hash,
        query: u64,
        result: Result<()>,
        busy: u64,
    },
}
pub(super) struct Queue {
    sender: SyncSender<Reply>,
    receiver: Option<Receiver<Reply>>,
}
impl Queue {
    pub fn new() -> Self {
        let (sender, receiver) = sync_channel(MAX_BODY_INFLIGHT);
        Self {
            sender,
            receiver: Some(receiver),
        }
    }
    pub fn close(&mut self) {
        self.receiver.take();
    }
}
impl<S: PeerSource> PeerPool<S> {
    pub(super) fn pending_bodies(&self) -> usize {
        self.peers
            .iter()
            .filter_map(|peer| peer.body.as_ref())
            .map(|job| job.pending.len())
            .sum()
    }
    pub(super) fn start_bodies(
        &mut self,
        index: usize,
        requests: Vec<Request>,
        params: Arc<mmx_types::mmx::ChainParams>,
    ) -> Result<Instant> {
        if requests.is_empty()
            || requests.len() > MAX_BODY_REQUESTS
            || self.pending_bodies() + requests.len() > MAX_BODY_INFLIGHT
        {
            return Err(error("body request admission limit"));
        }
        let query = self.next_query();
        let peer = &mut self.peers[index];
        if peer.inflight.is_some() || peer.body.is_some() {
            return Err(error("peer already owns an operation"));
        }
        let mut source = peer
            .source
            .take()
            .ok_or_else(|| error("missing body connection owner"))?;
        let ids: Vec<_> = requests.iter().map(|request| request.block).collect();
        let sender = self.bodies.sender.clone();
        let id = peer.id;
        let requested = Instant::now();
        let busy_before = source.backpressure_retries();
        let worker = match thread::Builder::new()
            .name("mmx-peer-bodies".into())
            .spawn(move || {
                let (result, busy) = catch_unwind(AssertUnwindSafe(|| {
                    let result = source.fetch_each(&ids, &mut |index, value| {
                        sender
                            .send(Reply::Body {
                                id,
                                query,
                                index,
                                value,
                            })
                            .map_err(|_| error("body coordinator stopped"))
                    });
                    (result, source.backpressure_retries())
                }))
                .unwrap_or_else(|_| (Err(error("peer body worker panicked")), 0));
                let _ = sender.send(Reply::Done {
                    id,
                    query,
                    result,
                    busy,
                });
                source
            }) {
            Ok(worker) => worker,
            Err(failure) => {
                peer.active = false;
                peer.qualified = false;
                return Err(failure.into());
            }
        };
        peer.body = Some(Job {
            query,
            requested,
            pending: (0..requests.len()).collect(),
            returned: 0,
            requests,
            params,
            busy_before,
            metadata_latency: peer.latency,
            worker,
        });
        Ok(requested)
    }
    pub(super) fn inherit_body_leases(&self, jobs: &mut BTreeMap<Key, Fetch>) {
        for (peer_index, peer) in self.peers.iter().enumerate() {
            if let Some(owner) = &peer.body {
                for &index in &owner.pending {
                    let request = &owner.requests[index];
                    for job in jobs
                        .values_mut()
                        .filter(|job| job.hash.content == request.key.1)
                    {
                        job.tried.insert(peer_index);
                        let lease = owner.requested + self.timeout;
                        job.retry_at = Some(job.retry_at.map_or(lease, |old| old.max(lease)));
                    }
                }
            }
        }
    }
    pub(super) fn expire_bodies(&mut self) {
        for peer in &mut self.peers {
            if peer.body.as_ref().is_some_and(|job| {
                !job.pending.is_empty() && job.requested.elapsed() >= self.timeout
            }) {
                peer.active = false;
                peer.qualified = false;
            }
        }
    }
    fn accept_body(&mut self, reply: Reply) -> Result<Option<Failure>> {
        let (id, query) = match &reply {
            Reply::Body { id, query, .. } | Reply::Done { id, query, .. } => (*id, *query),
        };
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == id) else {
            return Ok(None);
        };
        let Some(job) = peer.body.as_mut().filter(|job| job.query == query) else {
            return Ok(None);
        };
        match reply {
            Reply::Body { index, value, .. } => {
                if !job.pending.remove(&index) {
                    peer.active = false;
                    peer.qualified = false;
                    return Ok(None);
                }
                job.returned += usize::from(value.is_some());
                if peer.replacement.is_some() {
                    return Ok(None);
                }
                let request = &job.requests[index];
                if let Some(block) = value {
                    let matches = block.name == "mmx.Block"
                        && number(&block, "height")
                            .is_ok_and(|height| request.heights.contains(&height))
                        && hash(&block, "hash").ok() == Some(request.block)
                        && hash(&block, "content_hash").ok() == Some(request.key.1);
                    if matches {
                        match mmx_execution::block::prevalidate(&block, &job.params) {
                            Ok(()) => {
                                let height = number(&block, "height")?;
                                for &claimed in &request.heights {
                                    if self.retained_heights.contains(&claimed) {
                                        let value = (claimed == height).then(|| block.clone());
                                        // A late/height-alias reply cannot erase a valid body.
                                        self.completed
                                            .entry((claimed, request.key.1))
                                            .and_modify(|old| {
                                                if value.is_some() {
                                                    *old = value.clone()
                                                }
                                            })
                                            .or_insert(value);
                                    }
                                }
                            }
                            Err(failure) if failure.is_unsupported() => {
                                return Ok(Some((request.key, Box::new(failure))))
                            }
                            Err(_) => (),
                        }
                    }
                }
            }
            Reply::Done { result, busy, .. } => {
                let job = peer.body.take().expect("matched body owner");
                let source = match job.worker.join() {
                    Ok(source) => source,
                    Err(_) => {
                        peer.active = false;
                        peer.qualified = false;
                        return Err(error("peer body owner panicked"));
                    }
                };
                peer.backpressure = peer.backpressure.max(busy);
                if std::env::var_os("MMX_PROFILE_REPLAY").is_some() {
                    eprintln!(
                        "Sync body: peer={} count={} returned={} metadata_ms={:.3} elapsed_ms={:.3} busy_retries={}",
                        vnx::util::to_hex_string(&id[..8], true, false),
                        job.requests.len(),
                        job.returned,
                        job.metadata_latency.as_secs_f64() * 1e3,
                        job.requested.elapsed().as_secs_f64() * 1e3,
                        busy.saturating_sub(job.busy_before)
                    );
                }
                peer.source = Some(source);
                if let Some(replacement) = peer.replacement.take() {
                    self.retired_backpressure += peer.backpressure;
                    peer.source = Some(replacement);
                    peer.active = true;
                    peer.qualified = false;
                    peer.needs_status = true;
                    peer.height = 0;
                    peer.latency = Duration::MAX;
                    peer.backpressure = 0;
                } else if result.is_err() || !job.pending.is_empty() || !peer.active {
                    peer.active = false;
                    peer.qualified = false;
                    // Release failed sockets/admission leases promptly rather
                    // than retaining their sender until the next maintenance pass.
                    peer.source = None;
                    if let Err(failure) = result {
                        eprintln!(
                            "Sync peer {}: block request failed: {failure}",
                            vnx::util::to_hex_string(&id[..8], true, false)
                        );
                    }
                }
            }
        }
        Ok(None)
    }
    pub(super) fn drain_bodies(&mut self) -> Result<Vec<Failure>> {
        let mut failures = Vec::new();
        loop {
            match self.bodies.receiver.as_ref().unwrap().try_recv() {
                Ok(reply) => {
                    if let Some(failure) = self.accept_body(reply)? {
                        failures.push(failure);
                    }
                }
                Err(TryRecvError::Empty) => return Ok(failures),
                Err(TryRecvError::Disconnected) => {
                    return Err(error("body coordinator disconnected"))
                }
            }
        }
    }
    pub(super) fn wait_body(&mut self, wait: Duration) -> Result<Option<Failure>> {
        match self.bodies.receiver.as_ref().unwrap().recv_timeout(wait) {
            Ok(reply) => self.accept_body(reply),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(error("body coordinator disconnected")),
        }
    }
}

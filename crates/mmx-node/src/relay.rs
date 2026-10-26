//! Native peer gossip scheduling, receipts and bounded verified replay IDs.
mod credits;
mod history;
mod upload;
use crate::{
    error,
    node::{bytes, hash, record},
    Result,
};
use mmx_consensus::{Record, Value};
use rand::{seq::SliceRandom, Rng};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
type RelayEntries = BTreeMap<u64, Item>;
struct Item {
    record: Arc<Record>,
    content: [u8; 32],
    size: usize,
    published: Instant,
    delays: BTreeMap<u64, Duration>,
    reliable: bool,
}
const MAX_VERIFIED_IDS: usize = 100_000;
const MAX_OUT_OF_ORDER: usize = 4096;
fn allows(profile: u64, record: &Record, reliable: bool) -> bool {
    let role = profile as u32;
    if matches!(
        record.name.as_str(),
        "mmx.Transaction" | "mmx.ProofResponse"
    ) {
        role == mmx_types::mmx::node_type_e::FULL_NODE.value
            && (record.name != "mmx.Transaction" || !reliable || profile & (1 << 32) != 0)
    } else {
        role == mmx_types::mmx::node_type_e::FULL_NODE.value
            || role == mmx_types::mmx::node_type_e::LIGHT_NODE.value
    }
}
struct Delivery {
    seq: u64,
    record: Arc<Record>,
    reliable: bool,
}
struct Ready {
    cursor: u64,
    records: Vec<Delivery>,
    processed: Vec<u64>,
}
#[derive(Clone, Default)]
pub(crate) struct Relay {
    state: Arc<Mutex<RelayState>>,
    latest: Arc<AtomicU64>,
    wake: Arc<(Mutex<()>, Condvar)>,
}
struct RelayState {
    max_hash_cache: usize,
    max_sent_cache: Arc<AtomicUsize>,
    do_relay: bool,
    send_interval: Duration,
    entries: [RelayEntries; 2],
    notices: [BTreeMap<u64, [u8; 32]>; 2],
    ids: BTreeMap<[u8; 32], (usize, u64)>,
    verified: Seen,
    envelopes: Seen,
    params: Option<Arc<mmx_types::mmx::ChainParams>>,
    members: BTreeMap<u64, u64>,
    next_member: u64,
    target: Duration,
    upload: Option<upload::Budget>,
    compressed_limit: usize,
    credits: credits::Credits,
    early: BTreeMap<u64, ([u8; 32], usize)>,
    early_bytes: usize,
    max_segments: usize,
    history: history::History,
}
impl Default for RelayState {
    fn default() -> Self {
        Self {
            max_hash_cache: MAX_VERIFIED_IDS,
            max_sent_cache: Arc::new(AtomicUsize::new(20_000)),
            do_relay: true,
            send_interval: Duration::from_millis(20),
            entries: Default::default(),
            notices: Default::default(),
            ids: Default::default(),
            verified: Default::default(),
            envelopes: Default::default(),
            params: None,
            members: Default::default(),
            next_member: 0,
            target: Duration::from_secs(2),
            upload: None,
            compressed_limit: crate::peer::DEFAULT_COMPRESSED_LIMIT,
            credits: Default::default(),
            early: Default::default(),
            early_bytes: 0,
            max_segments: 65536,
            history: Default::default(),
        }
    }
}
impl RelayState {
    fn notice(&mut self, class: usize, seq: u64, content: [u8; 32]) {
        self.notices[class].insert(seq, content);
        if self.notices[0].len() + self.notices[1].len() > MAX_VERIFIED_IDS {
            let oldest = if self.notices[0]
                .first_key_value()
                .map(|(s, _)| *s)
                .unwrap_or(u64::MAX)
                < self.notices[1]
                    .first_key_value()
                    .map(|(s, _)| *s)
                    .unwrap_or(u64::MAX)
            {
                0
            } else {
                1
            };
            self.notices[oldest].pop_first();
        }
    }
}
struct Member {
    id: u64,
    state: std::sync::Weak<Mutex<RelayState>>,
}
impl Drop for Member {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .members
                .remove(&self.id);
        }
    }
}
impl Relay {
    pub(crate) fn publish(&self, id: [u8; 32], record: Arc<Record>, size: usize) -> Result<()> {
        self.publish_with_origin(id, record, size, false)
    }
    pub(crate) fn publish_with_origin(
        &self,
        id: [u8; 32],
        record: Arc<Record>,
        size: usize,
        reliable: bool,
    ) -> Result<()> {
        self.publish_kind(id, record, size, reliable, true)
    }
    fn publish_kind(
        &self,
        id: [u8; 32],
        record: Arc<Record>,
        size: usize,
        reliable: bool,
        verified: bool,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        let content = hash(&record, "content_hash")?;
        let notify =
            !state.verified.ids.contains(&content) && !state.envelopes.ids.contains(&content);
        if verified {
            if state.params.is_some() && !state.verified.ids.contains(&content) {
                if record.name == "mmx.Block" {
                    let through = state.credits.vdf_height;
                    state.history.prune(through);
                }
                state.credits.verified(&record)?;
            }
            if record.name == "mmx.ProofOfTime" {
                state.history.insert(record.clone(), size)?;
            }
            let limit = state.max_hash_cache;
            state.verified.remember(content, limit);
            // Promote an existing early publication without resetting its
            // native fanout timing or retransmitting its payload.
            if let Some((class, seq)) = state.ids.get(&id).copied() {
                if let Some((_, early_size)) = state.early.remove(&seq) {
                    state.early_bytes -= early_size;
                    if let Some(item) = state.entries[class].get_mut(&seq) {
                        // Keep the verified owner's normalized public record,
                        // dropping any unhashed padding from the early copy.
                        item.size = size;
                        item.record = record;
                    }
                    return Ok(());
                }
            }
        } else {
            if !state.do_relay && !reliable {
                return Ok(());
            }
            let max_segments = state.max_segments;
            if size > 32 * 1024 * 1024
                || state.verified.ids.contains(&content)
                || !state.credits.consume(&record, max_segments)?
            {
                return Ok(());
            }
            // Early publications can fail final verification. Bound them
            // independently of the owner's verified relay retention window.
            while state.early.len() >= 128 || state.early_bytes + size > 32 * 1024 * 1024 {
                let (seq, (id, size)) = state.early.pop_first().unwrap();
                state.entries[0].remove(&seq);
                state.ids.remove(&id);
                state.early_bytes -= size;
            }
        }
        if !state.do_relay && !reliable {
            return Ok(());
        }
        if let Some((class, seq)) = state.ids.remove(&id) {
            state.entries[class].remove(&seq);
        }
        let seq = self
            .latest
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| error("transaction relay sequence overflow"))?;
        let class = usize::from(record.name == "mmx.Transaction");
        if notify {
            state.notice(class, seq, content);
        }
        state.ids.insert(id, (class, seq));
        if !verified {
            state.early.insert(seq, (id, size));
            state.early_bytes += size;
        }
        let mut members: Vec<_> = state
            .members
            .iter()
            .filter(|(_, profile)| allows(**profile, &record, reliable))
            .map(|(id, _)| *id)
            .collect();
        members.shuffle(&mut rand::thread_rng());
        // Native Router::send_to shuffles once, then spaces all ranks within
        // target * N/(N+1). Store timing metadata, never peer payload copies.
        let interval = state.target.as_micros() / (members.len() as u128 + 1);
        let delays = members
            .into_iter()
            .enumerate()
            .map(|(rank, id)| (id, Duration::from_micros((interval * rank as u128) as u64)))
            .collect();
        state.entries[class].insert(
            seq,
            Item {
                record,
                content,
                size,
                published: Instant::now(),
                delays,
                reliable,
            },
        );
        self.latest.store(seq, Ordering::Release);
        drop(state);
        self.notify()
    }
    fn notify(&self) -> Result<()> {
        let _guard = self
            .wake
            .0
            .lock()
            .map_err(|_| error("relay notification poisoned"))?;
        self.wake.1.notify_all();
        Ok(())
    }
    /// Only ledger/proof-verified publications seed this cache. A claimed hash
    /// or ReceiveNote cannot poison another peer's validation/admission path.
    pub(crate) fn knows(&self, record: &Record) -> Result<bool> {
        let Ok(id) = hash(record, "content_hash") else {
            return Ok(false);
        };
        self.knows_hash(&id)
    }
    pub(crate) fn knows_hash(&self, id: &[u8; 32]) -> Result<bool> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?
            .verified
            .ids
            .contains(id))
    }
    pub(crate) fn reset_verified(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        state.verified = Seen::default();
        state.envelopes = Seen::default();
        state.credits = credits::Credits::default();
        let early = std::mem::take(&mut state.early);
        for (seq, (id, _)) in early {
            state.entries[0].remove(&seq);
            state.ids.remove(&id);
        }
        state.early_bytes = 0;
        state.notices = Default::default();
        state.history = Default::default();
        Ok(())
    }
    /// Native receive_msg_hash deduplication precedes expensive ledger/space
    /// work. Authenticate commitments before remembering a new ID, so a fake
    /// body or unsigned claim cannot suppress the real envelope from a peer.
    pub(crate) fn admit_gossip(
        &self,
        record: Record,
        size: usize,
        inbox: &crate::inbox::Inbox,
    ) -> Result<()> {
        if !matches!(
            record.name.as_str(),
            "mmx.Block"
                | "mmx.ProofOfTime"
                | "mmx.ProofResponse"
                | "mmx.ValidatorVote"
                | "mmx.Transaction"
        ) {
            return Ok(());
        }
        let id = hash(&record, "content_hash")?;
        let params = {
            let state = self.state.lock().map_err(|_| error("relay poisoned"))?;
            if state.verified.ids.contains(&id) || state.envelopes.ids.contains(&id) {
                return Ok(());
            }
            let params = state
                .params
                .clone()
                .ok_or_else(|| error("missing peer chain parameters"))?;
            if state.credits.too_old(&record, params.commit_delay)? {
                return Ok(());
            }
            params
        };
        let size = size.max(record.memory_size());
        let limit = match record.name.as_str() {
            "mmx.ProofResponse" => 64 * 1024,
            "mmx.ValidatorVote" => 4096,
            "mmx.Transaction" => 256 * 1024,
            _ => 32 * 1024 * 1024,
        };
        // Unhashed padding/schema overhead can preserve a real content ID.
        // Never remember a record the bounded owner/inbox cannot retain.
        if size > limit {
            return Err(error("peer envelope exceeds retention limit"));
        }
        match record.name.as_str() {
            "mmx.Block" => mmx_execution::block::prevalidate(&record, &params)?,
            "mmx.ProofOfTime" => {
                if !mmx_consensus::vdf::is_valid(&record)? {
                    return Err(error("invalid VDF envelope"));
                }
                mmx_consensus::vdf::authenticate(&record)?;
            }
            "mmx.ProofResponse" => {
                if !mmx_consensus::response::is_valid(&record)? {
                    return Err(error("invalid proof envelope"));
                }
                mmx_consensus::response::authenticate(&record)?;
            }
            "mmx.ValidatorVote" => {
                if !mmx_consensus::vote::is_valid(&record)? {
                    return Err(error("invalid vote envelope"));
                }
            }
            "mmx.Transaction" => {
                if record.field("exec_result")?.optional()?.is_some()
                    || !record.is_valid(&params.as_ref().into())?
                {
                    return Err(error("invalid transaction envelope"));
                }
            }
            _ => unreachable!(),
        }
        {
            let mut state = self.state.lock().map_err(|_| error("relay poisoned"))?;
            // Two decoder workers may authenticate the same first publication
            // concurrently. Only one can insert and hand it to the owner.
            if state.verified.ids.contains(&id) || state.envelopes.ids.contains(&id) {
                return Ok(());
            }
            let limit = state.max_hash_cache;
            state.envelopes.remember(id, limit);
            let seq = self
                .latest
                .load(Ordering::Relaxed)
                .checked_add(1)
                .ok_or_else(|| error("relay sequence overflow"))?;
            state.notice(usize::from(record.name == "mmx.Transaction"), seq, id);
            self.latest.store(seq, Ordering::Release);
        }
        // Native recv_notify acknowledges authenticated receipt immediately,
        // independently of expensive final validation or permission to relay.
        self.notify()?;
        // Queue admission never upgrades an envelope to verified work, nor
        // earns any relay credit. Native first-seen age is not refreshed.
        let record = Arc::new(record);
        self.publish_kind(id, record.clone(), size, false, false)?;
        inbox.submit_shared(record, size)
    }
    pub(crate) fn remove(&self, id: &[u8; 32]) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        if let Some((class, seq)) = state.ids.remove(id) {
            state.entries[class].remove(&seq);
            if let Some((_, size)) = state.early.remove(&seq) {
                state.early_bytes -= size;
            }
        }
        Ok(())
    }
    pub(crate) fn set_target(&self, target: Duration) -> Result<()> {
        if target.as_micros() > u128::from(u64::MAX) {
            return Err(error("relay target too large"));
        }
        self.state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .target = target;
        Ok(())
    }
    pub(crate) fn configure_policy(&self, config: &crate::router::RouterConfig) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        state.max_hash_cache = config.max_hash_cache;
        state
            .max_sent_cache
            .store(config.max_sent_cache, Ordering::Release);
        state.do_relay = config.do_relay;
        state.send_interval = config.send_interval;
        state.verified.prune(config.max_hash_cache);
        state.envelopes.prune(config.max_hash_cache);
        Ok(())
    }
    pub(crate) fn configure_upload(
        &self,
        params: &mmx_types::mmx::ChainParams,
        max_upload: f64,
        max_pending: f64,
    ) -> Result<()> {
        let budget = upload::Budget::new(params, max_upload, max_pending, Instant::now())?;
        let mut state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        state.upload = Some(budget);
        state.params = Some(Arc::new(params.clone()));
        Ok(())
    }
    pub(crate) fn set_compressed_limit(&self, limit: usize) -> Result<()> {
        if limit == 0 || limit > crate::peer::MAX_FRAME {
            return Err(error("Router.max_msg_size must be between 1 and 67108864"));
        }
        self.state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .compressed_limit = limit;
        Ok(())
    }
    pub(crate) fn set_max_vdf_segments(&self, limit: usize) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .max_segments = limit;
        Ok(())
    }
    pub(crate) fn compressed_limit(&self) -> Result<usize> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .compressed_limit)
    }
    fn admit_upload(
        &self,
        peer: &mut upload::Pending,
        record: &Record,
        now: Instant,
        sample: f64,
    ) -> Result<bool> {
        let mut state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        let Some(budget) = &mut state.upload else {
            return Ok(true);
        };
        Ok(budget.admit(
            peer,
            hash(record, "content_hash")?,
            record.field("static_cost")?.uint()?,
            now,
            sample,
        ))
    }
    pub(crate) fn attach(&self) -> Result<RelayPeer> {
        let mut state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        state.next_member = state
            .next_member
            .checked_add(1)
            .ok_or_else(|| error("relay member sequence overflow"))?;
        let id = state.next_member;
        state.members.insert(
            id,
            (1 << 32) | u64::from(mmx_types::mmx::node_type_e::FULL_NODE.value),
        );
        Ok(RelayPeer {
            received: Arc::new(Mutex::new(PeerCache {
                limit: state.max_sent_cache.clone(),
                ..Default::default()
            })),
            cursors: [self.latest.load(Ordering::Acquire); 2],
            acknowledgements: [self.latest.load(Ordering::Acquire); 2],
            replay: state.history.snapshot(),
            member: Some(Arc::new(Member {
                id,
                state: Arc::downgrade(&self.state),
            })),
            ..Default::default()
        })
    }
    fn receipts_since(&self, cursor: u64, class: usize) -> Result<(u64, Vec<[u8; 32]>)> {
        let state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        let mut next = self.latest.load(Ordering::Acquire);
        let mut out = Vec::new();
        for (seq, content) in state.notices[class].range((
            std::ops::Bound::Excluded(cursor),
            std::ops::Bound::Unbounded,
        )) {
            if out.len() == 128 {
                next = seq - 1;
                break;
            }
            out.push(*content);
        }
        Ok((next, out))
    }
    fn for_peer(
        &self,
        cursor: u64,
        class: usize,
        member: Option<u64>,
        known: &Seen,
        completed: &BTreeSet<u64>,
    ) -> Result<Ready> {
        let state = self.state.lock().map_err(|_| error("relay poisoned"))?;
        let mut next = self.latest.load(Ordering::Acquire);
        let mut out = Vec::new();
        let mut bytes = 0;
        let mut processed = Vec::new();
        let now = Instant::now();
        // Scan past a delayed item so a ready proof/VDF behind it can be sent.
        // Already-sent out-of-order IDs use the existing bounded peer cache.
        for (scanned, (seq, item)) in state.entries[class]
            .range((
                std::ops::Bound::Excluded(cursor),
                std::ops::Bound::Unbounded,
            ))
            .enumerate()
        {
            if scanned == 256 {
                next = next.min(seq - 1);
                break;
            }
            if !state.do_relay && !item.reliable {
                processed.push(*seq);
                continue;
            }
            if completed.contains(seq) {
                continue;
            }
            if member
                .and_then(|id| state.members.get(&id))
                .is_some_and(|profile| !allows(*profile, &item.record, item.reliable))
            {
                processed.push(*seq);
                continue;
            }
            if next < *seq && completed.len() + processed.len() + out.len() >= MAX_OUT_OF_ORDER {
                break;
            }
            if known.ids.contains(&item.content) {
                processed.push(*seq);
                continue;
            }
            if member
                .and_then(|id| item.delays.get(&id))
                .is_some_and(|delay| now.duration_since(item.published) < *delay)
            {
                next = next.min(seq - 1);
                continue;
            }
            if !out.is_empty() && (out.len() >= 16 || bytes + item.size > 256 * 1024) {
                next = next.min(seq - 1);
                break;
            }
            bytes += item.size;
            out.push(Delivery {
                seq: *seq,
                record: item.record.clone(),
                reliable: item.reliable,
            });
        }
        Ok(Ready {
            cursor: next,
            records: out,
            processed,
        })
    }
    #[cfg(test)]
    fn since(&self, cursor: u64, class: usize) -> Result<(u64, Vec<Arc<Record>>)> {
        let ready = self.for_peer(cursor, class, None, &Seen::default(), &BTreeSet::new())?;
        Ok((
            ready.cursor,
            ready.records.into_iter().map(|d| d.record).collect(),
        ))
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.latest.load(Ordering::Acquire)
    }
    pub(crate) fn wait(&self, observed: u64) -> Result<()> {
        let interval = self
            .state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .send_interval;
        let guard = self
            .wake
            .0
            .lock()
            .map_err(|_| error("relay notification poisoned"))?;
        if self.sequence() == observed {
            drop(
                self.wake
                    .1
                    .wait_timeout(guard, interval)
                    .map_err(|_| error("relay notification poisoned"))?,
            );
        }
        Ok(())
    }
}
#[derive(Clone, Default)]
pub(crate) struct RelayPeer {
    cursors: [u64; 2],
    acknowledgements: [u64; 2],
    completed: [BTreeSet<u64>; 2],
    member: Option<Arc<Member>>,
    received: Arc<Mutex<PeerCache>>,
    replay: history::Replay,
}
struct PeerCache {
    seen: Seen,
    uploads: upload::Pending,
    limit: Arc<AtomicUsize>,
}
impl Default for PeerCache {
    fn default() -> Self {
        Self {
            seen: Seen::default(),
            uploads: upload::Pending::default(),
            limit: Arc::new(AtomicUsize::new(20_000)),
        }
    }
}
#[derive(Default)]
struct Seen {
    ids: BTreeSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
}
impl Seen {
    fn prune(&mut self, limit: usize) {
        while self.order.len() > limit {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
    }
    fn remember(&mut self, id: [u8; 32], limit: usize) {
        if self.ids.insert(id) {
            self.order.push_back(id);
            self.prune(limit);
        }
    }
}
impl RelayPeer {
    pub(crate) fn set_profile(&self, role: u32, synced: bool) -> Result<()> {
        if let Some(member) = &self.member {
            if let Some(state) = member.state.upgrade() {
                if let Some(profile) = state
                    .lock()
                    .map_err(|_| error("relay poisoned"))?
                    .members
                    .get_mut(&member.id)
                {
                    *profile = u64::from(role) | if synced { 1 << 32 } else { 0 };
                }
            }
        }
        Ok(())
    }
    /// The decoder needs only shared receipt/cache state. Do not pin the
    /// sender's initial VDF payload snapshot for the connection's lifetime.
    pub(crate) fn receipt_view(&self) -> Self {
        Self {
            member: self.member.clone(),
            received: self.received.clone(),
            ..Default::default()
        }
    }
    pub fn note(&self, record: &Record) -> Result<()> {
        let id = if record.name == "mmx.ReceiveNote" {
            Some(hash(record, "hash")?)
        } else if matches!(
            record.name.as_str(),
            "mmx.Transaction"
                | "mmx.Block"
                | "mmx.ValidatorVote"
                | "mmx.ProofResponse"
                | "mmx.ProofOfTime"
        ) {
            hash(record, "content_hash").ok()
        } else {
            None
        };
        if let Some(id) = id {
            let mut seen = self
                .received
                .lock()
                .map_err(|_| error("peer relay cache poisoned"))?;
            let limit = seen.limit.load(Ordering::Acquire);
            seen.seen.remember(id, limit);
            if record.name == "mmx.ReceiveNote" {
                seen.uploads.acknowledge(&id, Instant::now());
            }
        }
        Ok(())
    }
    pub fn flush(
        &mut self,
        relay: &Relay,
        mut send: impl FnMut(&Record) -> Result<()>,
    ) -> Result<()> {
        // Consensus first; receipts are immediate and independent of the
        // randomized body schedule. No state/cache lock spans socket I/O.
        for class in 0..2 {
            let (next, ids) = relay.receipts_since(self.acknowledgements[class], class)?;
            for id in ids {
                send(&record(
                    "mmx.ReceiveNote",
                    [
                        ("hash".into(), bytes(id)),
                        ("time".into(), Value::Int(vnx::time::now_micros())),
                    ],
                ))?;
            }
            self.acknowledgements[class] = next;
            let ready = {
                let known = self
                    .received
                    .lock()
                    .map_err(|_| error("peer relay cache poisoned"))?;
                relay.for_peer(
                    self.cursors[class],
                    class,
                    self.member.as_ref().map(|m| m.id),
                    &known.seen,
                    &self.completed[class],
                )?
            };
            self.completed[class].extend(
                ready
                    .processed
                    .into_iter()
                    .filter(|seq| *seq > ready.cursor),
            );
            for delivery in ready.records {
                let seq = delivery.seq;
                let r = delivery.record;
                let should_send = {
                    let mut peer = self
                        .received
                        .lock()
                        .map_err(|_| error("peer relay cache poisoned"))?;
                    if peer.seen.ids.contains(&hash(&r, "content_hash")?) {
                        false
                    } else if r.name == "mmx.Transaction" && !delivery.reliable {
                        relay.admit_upload(
                            &mut peer.uploads,
                            &r,
                            Instant::now(),
                            rand::thread_rng().gen(),
                        )?
                    } else {
                        true
                    }
                };
                if should_send {
                    send(&r)?;
                    self.note(&r)?;
                }
                // Native drops unreliable messages refused by upload credits.
                // A new publication or local RPC submission is a separate event.
                if seq > ready.cursor {
                    self.completed[class].insert(seq);
                }
            }
            self.cursors[class] = ready.cursor;
            self.completed[class].retain(|seq| *seq > ready.cursor);
            if class == 0 {
                // Live consensus first, then bounded bootstrap VDFs, then TXs.
                // Failed writes leave the front available until disconnect or
                // an explicit caller retry; no payload/cache lock spans I/O.
                let mut count = 0;
                let mut bytes = 0;
                while let Some((record, size)) = self.replay.front().cloned() {
                    let known = self
                        .received
                        .lock()
                        .map_err(|_| error("peer relay cache poisoned"))?
                        .seen
                        .ids
                        .contains(&hash(&record, "content_hash")?);
                    if !known {
                        if count == 16 || (count > 0 && bytes + size > 256 * 1024) {
                            break;
                        }
                        send(&record)?;
                        self.note(&record)?;
                        count += 1;
                        bytes += size;
                    }
                    self.replay.pop_front();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn router_cache_limits_and_relay_switch_keep_receipts_and_local_publications() {
        let relay = Relay::default();
        relay.set_target(Duration::ZERO).unwrap();
        let policy = crate::router::RouterConfig {
            max_hash_cache: 2,
            max_sent_cache: 1,
            do_relay: false,
            ..Default::default()
        };
        relay.configure_policy(&policy).unwrap();
        let mut peer = relay.attach().unwrap();
        for id in 1..=4 {
            relay.publish([id; 32], message(id), 100).unwrap();
        }
        assert!(!relay.knows(&message(1)).unwrap());
        assert!(relay.knows(&message(4)).unwrap());
        assert_eq!(relay.state.lock().unwrap().verified.ids.len(), 2);
        assert_eq!(
            relay.sequence(),
            0,
            "relay=false does not forward received publications"
        );
        relay
            .publish_with_origin([5; 32], transaction(5), 100, true)
            .unwrap();
        let mut sent = vec![];
        peer.flush(&relay, |record| {
            sent.push(record.name.clone());
            Ok(())
        })
        .unwrap();
        assert!(
            sent.iter().any(|name| name == "mmx.Transaction"),
            "local publications remain reliable"
        );
        peer.note(&record(
            "mmx.ReceiveNote",
            [("hash".into(), bytes([6; 32]))],
        ))
        .unwrap();
        peer.note(&record(
            "mmx.ReceiveNote",
            [("hash".into(), bytes([7; 32]))],
        ))
        .unwrap();
        assert_eq!(peer.received.lock().unwrap().seen.ids.len(), 1);
        let disabled = crate::router::RouterConfig {
            max_hash_cache: 0,
            max_sent_cache: 0,
            ..Default::default()
        };
        relay.configure_policy(&disabled).unwrap();
        sent.clear();
        relay
            .publish_with_origin([8; 32], transaction(8), 100, true)
            .unwrap();
        peer.flush(&relay, |record| {
            sent.push(record.name.clone());
            Ok(())
        })
        .unwrap();
        assert!(relay.state.lock().unwrap().verified.ids.is_empty());
        assert!(peer.received.lock().unwrap().seen.ids.is_empty());
        assert!(
            sent.iter().any(|name| name == "mmx.ReceiveNote"),
            "cache=0 must not disable receipt delivery"
        );
    }
    #[test]
    fn native_role_filters_rank_only_recipients_and_local_transactions_require_sync() {
        let relay = Relay::default();
        relay.set_target(Duration::ZERO).unwrap();
        let profiles = [
            (mmx_types::mmx::node_type_e::FULL_NODE.value, true),
            (mmx_types::mmx::node_type_e::FULL_NODE.value, false),
            (mmx_types::mmx::node_type_e::LIGHT_NODE.value, true),
            (mmx_types::mmx::node_type_e::API_CLIENT.value, true),
        ];
        let mut peers: Vec<_> = profiles
            .iter()
            .map(|(role, synced)| {
                let peer = relay.attach().unwrap();
                peer.set_profile(*role, *synced).unwrap();
                peer
            })
            .collect();
        for (index, (name, reliable, recipients)) in [
            ("mmx.Block", true, 3),
            ("mmx.ProofResponse", false, 2),
            ("mmx.Transaction", false, 2),
            ("mmx.Transaction", true, 1),
            ("mmx.ValidatorVote", false, 3),
        ]
        .into_iter()
        .enumerate()
        {
            let id = [index as u8; 32];
            relay
                .publish_with_origin(
                    id,
                    Arc::new(record(name, [("content_hash".into(), bytes(id))])),
                    100,
                    reliable,
                )
                .unwrap();
            let state = relay.state.lock().unwrap();
            let (_, seq) = state.ids.get(&id).unwrap();
            assert_eq!(
                state.entries[usize::from(name == "mmx.Transaction")][seq]
                    .delays
                    .len(),
                recipients
            );
        }
        for (index, peer) in peers.iter_mut().enumerate() {
            let mut bodies = Vec::new();
            let mut notices = 0;
            peer.flush(&relay, |r| {
                if r.name == "mmx.ReceiveNote" {
                    notices += 1;
                } else {
                    bodies.push(hash(r, "content_hash")?[0]);
                }
                Ok(())
            })
            .unwrap();
            bodies.sort();
            assert_eq!(
                bodies,
                [vec![0, 1, 2, 3, 4], vec![0, 1, 2, 4], vec![0, 4], vec![]][index]
            );
            assert_eq!(notices, 5, "native receipt notifications reach every role");
            peer.flush(&relay, |_| {
                panic!("filtered or sent bodies must not repeat")
            })
            .unwrap();
        }
        peers[1]
            .set_profile(mmx_types::mmx::node_type_e::FULL_NODE.value, true)
            .unwrap();
        relay
            .publish_with_origin(
                [5; 32],
                Arc::new(record(
                    "mmx.Transaction",
                    [("content_hash".into(), bytes([5; 32]))],
                )),
                100,
                true,
            )
            .unwrap();
        let mut count = 0;
        peers[1]
            .flush(&relay, |r| {
                count += usize::from(r.name == "mmx.Transaction");
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    fn native_envelopes() -> (mmx_types::mmx::ChainParams, Vec<Record>) {
        use mmx_types::VnxType;
        use vnx::serde_json::{self, Value as Json};
        let rows: Vec<Json> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let raw = |v: &Json| vnx::util::from_hex_string(v.as_str().unwrap()).unwrap();
        let genesis = rows.iter().find(|r| r["name"] == "synthetic").unwrap();
        let params = mmx_types::mmx::ChainParams::decode(&raw(&genesis["params"])).unwrap();
        let steps = &rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"];
        let block = Record::decode(&raw(&steps[1]["block"])).unwrap();
        let vdf = Record::decode(&raw(&steps[0]["vdf"])).unwrap();
        let next_vdf = Record::decode(&raw(&steps[1]["vdf"])).unwrap();
        let row: Json = include_str!("../../mmx-consensus/tests/fixtures/responses.jsonl")
            .lines()
            .map(|s| serde_json::from_str::<Json>(s).unwrap())
            .find(|r| r["name"] == "extensions1_0")
            .unwrap();
        let proof = Record::decode(&raw(&row["wire"])).unwrap();
        let mut tx = block.field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap()
            .clone();
        tx.fields
            .insert("exec_result".into(), Value::Optional(None));
        tx.fields.insert(
            "static_cost".into(),
            Value::Uint(tx.calc_cost(&(&params).into()).unwrap()),
        );
        tx.fields
            .insert("content_hash".into(), bytes(tx.calc_hash(true, 0).unwrap()));
        (params, vec![block, vdf, proof, tx, next_vdf])
    }
    #[test]
    fn early_vdf_uses_earned_credit_shares_the_inbox_body_and_promotes_without_resending() {
        let (params, records) = native_envelopes();
        let (previous, next) = (&records[1], &records[4]);
        let relay = Relay::default();
        relay.configure_upload(&params, 0., 0.1).unwrap();
        let previous_id = hash(previous, "content_hash").unwrap();
        relay
            .publish(previous_id, Arc::new(previous.clone()), 100)
            .unwrap();
        relay.remove(&previous_id).unwrap();
        let id = hash(next, "content_hash").unwrap();
        let inbox = crate::inbox::Inbox::default();
        let mut padded = next.clone();
        padded
            .fields
            .insert("padding".into(), Value::Bytes(vec![0; 4096]));
        relay.admit_gossip(padded.clone(), 100, &inbox).unwrap();
        let sequence = relay.sequence();
        assert!(!relay.knows(next).unwrap());
        let queued = inbox.take().unwrap().pop_front().unwrap().0;
        assert!(Arc::ptr_eq(
            &queued,
            &relay.state.lock().unwrap().entries[0]
                .last_key_value()
                .unwrap()
                .1
                .record
        ));
        let mut peer = RelayPeer::default();
        let mut payloads = vec![];
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                payloads.push(r.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(payloads.as_slice(), std::slice::from_ref(&padded));
        relay
            .publish(id, Arc::new(next.clone()), next.memory_size())
            .unwrap();
        assert!(relay.knows(next).unwrap());
        assert_eq!(relay.sequence(), sequence);
        assert!(relay.state.lock().unwrap().early.is_empty());
        assert!(!relay.state.lock().unwrap().entries[0]
            .last_key_value()
            .unwrap()
            .1
            .record
            .fields
            .contains_key("padding"));
        peer.flush(&relay, |_| {
            panic!("verified promotion retransmitted an early publication")
        })
        .unwrap();
    }
    #[test]
    fn failed_early_work_has_separate_count_byte_and_rewind_bounds() {
        let (params, records) = native_envelopes();
        let relay = Relay::default();
        relay.configure_upload(&params, 0., 0.1).unwrap();
        for n in 0..200u64 {
            let mut early = records[1].clone();
            let mut id = [0; 32];
            id[..8].copy_from_slice(&n.to_le_bytes());
            early.fields.insert("content_hash".into(), bytes(id));
            relay
                .state
                .lock()
                .unwrap()
                .credits
                .verified(&records[1])
                .unwrap();
            relay
                .publish_kind(id, Arc::new(early), 100, false, false)
                .unwrap();
        }
        assert_eq!(relay.state.lock().unwrap().early.len(), 128);
        for n in 200..250u64 {
            let mut early = records[1].clone();
            let mut id = [0; 32];
            id[..8].copy_from_slice(&n.to_le_bytes());
            early.fields.insert("content_hash".into(), bytes(id));
            relay
                .state
                .lock()
                .unwrap()
                .credits
                .verified(&records[1])
                .unwrap();
            relay
                .publish_kind(id, Arc::new(early), 1024 * 1024, false, false)
                .unwrap();
        }
        let state = relay.state.lock().unwrap();
        assert_eq!(state.early.len(), 32);
        assert_eq!(state.early_bytes, 32 * 1024 * 1024);
        drop(state);
        relay.reset_verified().unwrap();
        let state = relay.state.lock().unwrap();
        assert_eq!(state.early_bytes, 0);
        assert!(state.entries[0].is_empty() && state.ids.is_empty());
    }
    #[test]
    fn first_seen_cache_authenticates_commitments_without_promoting_or_poisoning_work() {
        let (params, records) = native_envelopes();
        for native in records {
            let relay = Relay::default();
            relay.configure_upload(&params, 0., 0.1).unwrap();
            let mut peer = relay.attach().unwrap();
            let inbox = crate::inbox::Inbox::default();
            let mut forged = native.clone();
            match native.name.as_str() {
                "mmx.Block" => {
                    let mut txs = native.field("tx_list").unwrap().seq().unwrap().to_vec();
                    let mut tx = txs[0].record().unwrap().clone();
                    tx.fields.insert("nonce".into(), Value::Uint(0));
                    txs[0] = Value::Record(tx);
                    forged.fields.insert("tx_list".into(), Value::Sequence(txs));
                }
                "mmx.ProofOfTime" => {
                    forged.fields.insert("timelord_sig".into(), bytes([0; 64]));
                }
                "mmx.ProofResponse" => {
                    forged.fields.insert("farmer_sig".into(), bytes([0; 64]));
                }
                "mmx.Transaction" => {
                    forged.fields.insert("nonce".into(), Value::Uint(0));
                }
                _ => unreachable!(),
            }
            assert!(
                relay.admit_gossip(forged, 100, &inbox).is_err(),
                "{}",
                native.name
            );
            relay.admit_gossip(native.clone(), 100, &inbox).unwrap();
            relay.admit_gossip(native.clone(), 100, &inbox).unwrap();
            assert_eq!(inbox.stats().unwrap().messages, 1);
            assert!(!relay.knows(&native).unwrap());
            assert!(relay.since(0, 0).unwrap().1.is_empty());
            let mut receipts = vec![];
            peer.flush(&relay, |record| {
                receipts.push(record.clone());
                Ok(())
            })
            .unwrap();
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].name, "mmx.ReceiveNote");
            assert_eq!(
                hash(&receipts[0], "hash").unwrap(),
                hash(&native, "content_hash").unwrap()
            );
            peer.flush(&relay, |_| {
                panic!("a repeated envelope refreshed its receipt")
            })
            .unwrap();
            relay.reset_verified().unwrap();
            relay.admit_gossip(native, 100, &inbox).unwrap();
            assert_eq!(inbox.stats().unwrap().messages, 2);
        }
    }
    #[test]
    fn simultaneous_connections_enqueue_only_one_authenticated_first_envelope() {
        let (params, records) = native_envelopes();
        let native = &records[1];
        let relay = Relay::default();
        relay.configure_upload(&params, 0., 0.1).unwrap();
        let inbox = crate::inbox::Inbox::default();
        let gate = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    gate.wait();
                    relay.admit_gossip(native.clone(), 100, &inbox).unwrap();
                });
            }
        });
        assert_eq!(inbox.stats().unwrap().messages, 1);
        assert!(!relay.knows(native).unwrap());
    }
    #[test]
    fn oversized_unhashed_padding_cannot_poison_a_real_response_or_spend_credits() {
        let (params, records) = native_envelopes();
        let response = &records[2];
        let relay = Relay::default();
        relay.configure_upload(&params, 0., 0.1).unwrap();
        let inbox = crate::inbox::Inbox::default();
        let mut padded = response.clone();
        padded
            .fields
            .insert("padding".into(), Value::Bytes(vec![0; 64 * 1024]));
        assert!(mmx_consensus::response::is_valid(&padded).unwrap());
        mmx_consensus::response::authenticate(&padded).unwrap();
        assert!(relay.admit_gossip(padded, 100, &inbox).is_err());
        assert!(relay.state.lock().unwrap().envelopes.ids.is_empty());
        relay.admit_gossip(response.clone(), 100, &inbox).unwrap();
        assert_eq!(inbox.stats().unwrap().messages, 1);
        assert!(!relay.knows(response).unwrap());
    }
    fn message(id: u8) -> Arc<Record> {
        Arc::new(record(
            "mmx.ProofResponse",
            [("content_hash".into(), bytes([id; 32]))],
        ))
    }
    fn transaction(id: u8) -> Arc<Record> {
        Arc::new(record(
            "mmx.Transaction",
            [
                ("content_hash".into(), bytes([id; 32])),
                ("static_cost".into(), Value::Uint(20_000)),
            ],
        ))
    }
    #[test]
    fn local_submissions_and_consensus_bypass_forwarded_transaction_upload_limits() {
        let relay = Relay::default();
        relay
            .configure_upload(&mmx_types::mmx::ChainParams::default(), 0., 0.)
            .unwrap();
        relay
            .publish_with_origin([1; 32], transaction(1), 100, false)
            .unwrap();
        relay
            .publish_with_origin([2; 32], transaction(2), 100, true)
            .unwrap();
        let mut consensus = message(3).as_ref().clone();
        consensus.name = "mmx.ValidatorVote".into();
        relay.publish([3; 32], Arc::new(consensus), 100).unwrap();
        let mut peer = RelayPeer::default();
        let mut payloads = Vec::new();
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                payloads.push(hash(r, "content_hash")?);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(payloads, vec![[3; 32], [2; 32]]);
        peer.flush(&relay, |_| panic!("credit-refused messages must not retry"))
            .unwrap();
    }
    #[test]
    fn receipt_releases_only_matching_pending_upload_cost_for_future_messages() {
        let relay = Relay::default();
        relay
            .configure_upload(&mmx_types::mmx::ChainParams::default(), 0., 0.00004)
            .unwrap();
        for id in [1, 2] {
            relay.publish([id; 32], transaction(id), 100).unwrap();
        }
        let mut peer = RelayPeer::default();
        let mut payloads = Vec::new();
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                payloads.push(hash(r, "content_hash")?);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(payloads, vec![[1; 32]]);
        peer.note(&record(
            "mmx.ReceiveNote",
            [("hash".into(), bytes([1; 32]))],
        ))
        .unwrap();
        relay.publish([3; 32], transaction(3), 100).unwrap();
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                payloads.push(hash(r, "content_hash")?);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(payloads, vec![[1; 32], [3; 32]]);
    }
    #[test]
    fn fanout_has_one_unique_native_rank_per_live_connection_and_retires_members() {
        let relay = Relay::default();
        let peers: Vec<_> = (0..8).map(|_| relay.attach().unwrap()).collect();
        relay.publish([1; 32], message(1), 100).unwrap();
        {
            let state = relay.state.lock().unwrap();
            let item = state.entries[0].first_key_value().unwrap().1;
            assert_eq!(
                item.delays.keys().copied().collect::<BTreeSet<_>>(),
                state.members.keys().copied().collect()
            );
            let mut delays: Vec<_> = item.delays.values().copied().collect();
            delays.sort();
            assert_eq!(
                delays,
                (0..8)
                    .map(|n| Duration::from_micros(2_000_000 / 9 * n))
                    .collect::<Vec<_>>()
            );
        }
        let clone = peers[0].clone();
        drop(peers);
        assert_eq!(relay.state.lock().unwrap().members.len(), 1);
        drop(clone);
        assert!(relay.state.lock().unwrap().members.is_empty());
    }
    #[test]
    fn a_delayed_body_never_hides_ready_consensus_and_out_of_order_sends_do_not_repeat() {
        let relay = Relay::default();
        let mut peer = relay.attach().unwrap();
        let id = peer.member.as_ref().unwrap().id;
        relay.publish([1; 32], message(1), 100).unwrap();
        relay.publish([2; 32], message(2), 100).unwrap();
        relay.state.lock().unwrap().entries[0]
            .get_mut(&1)
            .unwrap()
            .delays
            .insert(id, Duration::from_secs(100));
        let mut sent = Vec::new();
        peer.flush(&relay, |r| {
            sent.push(r.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            sent.iter().filter(|r| r.name == "mmx.ReceiveNote").count(),
            2
        );
        assert_eq!(
            sent.iter()
                .filter(|r| r.name != "mmx.ReceiveNote")
                .map(|r| hash(r, "content_hash").unwrap())
                .collect::<Vec<_>>(),
            vec![[2; 32]]
        );
        peer.received.lock().unwrap().seen = Seen::default();
        peer.flush(&relay, |_| {
            panic!("not due and already sent work must not repeat")
        })
        .unwrap();
        relay.state.lock().unwrap().entries[0]
            .get_mut(&1)
            .unwrap()
            .delays
            .insert(id, Duration::ZERO);
        let mut sent = Vec::new();
        peer.flush(&relay, |r| {
            sent.push(r.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(hash(&sent[0], "content_hash").unwrap(), [1; 32]);
    }
    #[test]
    fn receipts_are_immediate_and_can_cancel_payloads_waiting_for_their_stage() {
        let relay = Relay::default();
        let mut peer = relay.attach().unwrap();
        relay.publish([1; 32], message(1), 100).unwrap();
        let id = peer.member.as_ref().unwrap().id;
        relay.state.lock().unwrap().entries[0]
            .get_mut(&1)
            .unwrap()
            .delays
            .insert(id, Duration::from_secs(100));
        let mut sent = Vec::new();
        peer.flush(&relay, |r| {
            sent.push(r.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].name, "mmx.ReceiveNote");
        peer.note(&sent[0]).unwrap();
        relay.state.lock().unwrap().entries[0]
            .get_mut(&1)
            .unwrap()
            .delays
            .insert(id, Duration::ZERO);
        peer.flush(&relay, |_| panic!("peer already acknowledged this payload"))
            .unwrap();
    }
    #[test]
    fn zero_target_sends_new_work_immediately_without_replaying_old_proofs_to_new_connections() {
        let relay = Relay::default();
        relay.set_target(Duration::ZERO).unwrap();
        let peers: Vec<_> = (0..8).map(|_| relay.attach().unwrap()).collect();
        relay.publish([1; 32], message(1), 100).unwrap();
        assert!(relay.state.lock().unwrap().entries[0]
            .first_key_value()
            .unwrap()
            .1
            .delays
            .values()
            .all(|d| d.is_zero()));
        drop(peers);
        let mut joined = relay.attach().unwrap();
        let mut sent = Vec::new();
        joined
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert!(sent.is_empty());
        relay.publish([2; 32], message(2), 100).unwrap();
        joined
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].name, "mmx.ProofResponse");
    }
    #[test]
    fn bootstrap_replays_only_shared_verified_vdfs_and_live_consensus_takes_priority() {
        let (_, native) = native_envelopes();
        let relay = Relay::default();
        relay.set_target(Duration::ZERO).unwrap();
        for record in [&native[0], &native[1], &native[2], &native[3]] {
            relay
                .publish(
                    hash(record, "content_hash").unwrap(),
                    Arc::new(record.clone()),
                    record.memory_size(),
                )
                .unwrap();
        }
        let mut peer = relay.attach().unwrap();
        let receiver = peer.receipt_view();
        assert!(receiver.replay.is_empty());
        assert_eq!(peer.replay.len(), 1);
        let id = hash(&native[1], "content_hash").unwrap();
        let state = relay.state.lock().unwrap();
        let (class, seq) = state.ids[&id];
        assert!(Arc::ptr_eq(
            &peer.replay[0].0,
            &state.entries[class][&seq].record
        ));
        drop(state);
        relay.publish([200; 32], message(200), 100).unwrap();
        let mut sent = vec![];
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                sent.push(r.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(sent, [message(200).as_ref().clone(), native[1].clone()]);
        peer.flush(&relay, |_| panic!("initial work was replayed twice"))
            .unwrap();
        assert!(peer.replay.is_empty() && receiver.replay.is_empty());
    }
    #[test]
    fn vdf_bootstrap_is_batched_and_stalled_sends_retain_only_their_unsent_shared_front() {
        let relay = Relay::default();
        for height in 1..=17 {
            let r = Arc::new(record(
                "mmx.ProofOfTime",
                [
                    ("vdf_height".into(), Value::Uint(height)),
                    ("content_hash".into(), bytes([height as u8; 32])),
                ],
            ));
            relay.publish([height as u8; 32], r, 100).unwrap();
        }
        let mut peer = relay.attach().unwrap();
        assert!(peer.flush(&relay, |_| Err(error("stalled"))).is_err());
        assert_eq!(peer.replay.len(), 17);
        let mut sent = 0;
        peer.flush(&relay, |_| {
            sent += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(sent, 16);
        assert_eq!(peer.replay.len(), 1);
        peer.flush(&relay, |_| {
            sent += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(sent, 17);
        assert!(peer.replay.is_empty());
        relay.reset_verified().unwrap();
        assert!(relay.attach().unwrap().replay.is_empty());
    }
    #[test]
    fn verified_replay_ids_survive_payload_eviction_and_ignore_receipt_claims() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), bytes([7; 32]))],
        ));
        let peer = RelayPeer::default();
        peer.note(&record(
            "mmx.ReceiveNote",
            [("hash".into(), bytes([7; 32]))],
        ))
        .unwrap();
        assert!(!relay.knows(&tx).unwrap());
        relay.publish([8; 32], tx.clone(), 100).unwrap();
        relay.remove(&[8; 32]).unwrap();
        assert!(relay.knows(&tx).unwrap());
        relay.reset_verified().unwrap();
        assert!(!relay.knows(&tx).unwrap());
        let mut seen = Seen::default();
        for n in 0..=MAX_VERIFIED_IDS {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            seen.remember(id, MAX_VERIFIED_IDS);
            if n < MAX_VERIFIED_IDS {
                seen.remember([0; 32], MAX_VERIFIED_IDS);
            }
        }
        assert_eq!(seen.ids.len(), MAX_VERIFIED_IDS);
        assert_eq!(seen.order.len(), MAX_VERIFIED_IDS);
        assert!(!seen.ids.contains(&[0; 32]));
    }

    #[test]
    fn native_receipts_suppress_only_the_acknowledging_peers_payload() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), bytes([7; 32]))],
        ));
        relay.publish([8; 32], tx.clone(), 100).unwrap();
        let mut receiver = RelayPeer::default();
        let note = Record::from_generated(&mmx_types::mmx::ReceiveNote {
            hash: [7; 32].into(),
            time: 42,
        })
        .unwrap();
        receiver.note(&note).unwrap();
        let mut sent = Vec::new();
        receiver
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].name, "mmx.ReceiveNote");
        assert_eq!(hash(&sent[0], "hash").unwrap(), [7; 32]);
        // Receipt claims are per peer and never grant global/consensus credit.
        let mut other = RelayPeer::default();
        let mut sent = Vec::new();
        other
            .flush(&relay, |r| {
                sent.push(r.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(&sent[1], tx.as_ref());
        assert!(receiver
            .note(&record(
                "mmx.ReceiveNote",
                [("hash".into(), bytes([0; 31]))]
            ))
            .is_err());
    }

    #[test]
    fn consensus_relay_precedes_a_transaction_backlog_without_dropping_transactions() {
        let relay = Relay::default();
        for i in 1..=64u8 {
            let tx = Arc::new(record(
                "mmx.Transaction",
                [("content_hash".into(), Value::Bytes(vec![i; 32]))],
            ));
            relay.publish([i; 32], tx, 65536).unwrap();
        }
        let proof = Arc::new(record(
            "mmx.ProofResponse",
            [("content_hash".into(), Value::Bytes(vec![200; 32]))],
        ));
        relay.publish([200; 32], proof, 100).unwrap();
        let mut peer = RelayPeer::default();
        let mut sent = Vec::new();
        peer.flush(&relay, |r| {
            if r.name != "mmx.ReceiveNote" {
                sent.push(r.clone());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(sent[0].name, "mmx.ProofResponse");
        assert_eq!(sent.len(), 5);
        for _ in 0..20 {
            peer.flush(&relay, |r| {
                if r.name != "mmx.ReceiveNote" {
                    sent.push(r.clone());
                }
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(sent.len(), 65);
        assert_eq!(
            sent.iter().filter(|r| r.name == "mmx.Transaction").count(),
            64
        );
    }

    #[test]
    fn relay_deduplicates_echoes_and_retries_failed_sends_without_copying_bodies() {
        let relay = Relay::default();
        let tx = Arc::new(record(
            "mmx.Transaction",
            [("content_hash".into(), Value::Bytes(vec![1; 32]))],
        ));
        relay.publish([2; 32], tx.clone(), 100).unwrap();
        assert!(Arc::ptr_eq(&relay.since(0, 1).unwrap().1[0], &tx));
        let mut peer = RelayPeer::default();
        assert!(peer.flush(&relay, |_| Err(error("disconnected"))).is_err());
        let mut count = 0;
        peer.flush(&relay, |r| {
            count += usize::from(r.name != "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        peer.flush(&relay, |r| {
            count += usize::from(r.name != "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        let mut echo = RelayPeer::default();
        echo.note(&tx).unwrap();
        echo.flush(&relay, |r| {
            assert_eq!(r.name, "mmx.ReceiveNote");
            Ok(())
        })
        .unwrap();
        relay.remove(&[2; 32]).unwrap();
        assert!(relay.since(0, 1).unwrap().1.is_empty());
    }
}

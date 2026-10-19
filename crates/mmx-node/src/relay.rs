//! Native peer gossip scheduling, receipts and bounded verified replay IDs.
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
        atomic::{AtomicU64, Ordering},
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
    entries: [RelayEntries; 2],
    ids: BTreeMap<[u8; 32], (usize, u64)>,
    verified: Seen,
    members: BTreeSet<u64>,
    next_member: u64,
    target: Duration,
    upload: Option<upload::Budget>,
}
impl Default for RelayState {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            ids: Default::default(),
            verified: Default::default(),
            members: Default::default(),
            next_member: 0,
            target: Duration::from_secs(2),
            upload: None,
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
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        let content = hash(&record, "content_hash")?;
        state.verified.remember(content, MAX_VERIFIED_IDS);
        if let Some((class, seq)) = state.ids.remove(&id) {
            state.entries[class].remove(&seq);
        }
        let seq = self
            .latest
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| error("transaction relay sequence overflow"))?;
        let class = usize::from(record.name == "mmx.Transaction");
        state.ids.insert(id, (class, seq));
        let mut members: Vec<_> = state.members.iter().copied().collect();
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
        Ok(self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?
            .verified
            .ids
            .contains(&id))
    }
    pub(crate) fn reset_verified(&self) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?
            .verified = Seen::default();
        Ok(())
    }
    pub(crate) fn remove(&self, id: &[u8; 32]) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("transaction relay poisoned"))?;
        if let Some((class, seq)) = state.ids.remove(id) {
            state.entries[class].remove(&seq);
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
    pub(crate) fn configure_upload(
        &self,
        params: &mmx_types::mmx::ChainParams,
        max_upload: f64,
        max_pending: f64,
    ) -> Result<()> {
        let budget = upload::Budget::new(params, max_upload, max_pending, Instant::now())?;
        self.state
            .lock()
            .map_err(|_| error("relay poisoned"))?
            .upload = Some(budget);
        Ok(())
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
        state.members.insert(id);
        Ok(RelayPeer {
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
        for (seq, item) in state.entries[class].range((
            std::ops::Bound::Excluded(cursor),
            std::ops::Bound::Unbounded,
        )) {
            if out.len() == 128 {
                next = seq - 1;
                break;
            }
            out.push(item.content);
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
            if completed.contains(seq) {
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
        let guard = self
            .wake
            .0
            .lock()
            .map_err(|_| error("relay notification poisoned"))?;
        if self.sequence() == observed {
            drop(
                self.wake
                    .1
                    .wait_timeout(guard, std::time::Duration::from_millis(20))
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
}
#[derive(Default)]
struct PeerCache {
    seen: Seen,
    uploads: upload::Pending,
}
#[derive(Default)]
struct Seen {
    ids: BTreeSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
}
impl Seen {
    fn remember(&mut self, id: [u8; 32], limit: usize) {
        if self.ids.insert(id) {
            self.order.push_back(id);
            if self.order.len() > limit {
                let old = self.order.pop_front().unwrap();
                self.ids.remove(&old);
            }
        }
    }
}
impl RelayPeer {
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
            seen.seen.remember(id, 20_000);
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
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        relay.publish([3; 32], message(3), 100).unwrap();
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
                state.members
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
    fn zero_target_and_connections_joined_after_publication_send_without_staging_delay() {
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
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].name, "mmx.ProofResponse");
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

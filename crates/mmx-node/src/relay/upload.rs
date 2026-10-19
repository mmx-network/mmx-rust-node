//! Router transaction upload credits. Values use the same MMX cost conversion
//! as C++; these are transport limits, never block/transaction consensus rules.
use crate::{error, Result};
use mmx_consensus::Hash;
use mmx_types::mmx;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
const MAX_UNACKNOWLEDGED: usize = 20_000;
pub(super) struct Budget {
    capacity: f64,
    credits: f64,
    updated: Instant,
    pending_limit: f64,
    scale: f64,
}
pub(super) struct Pending {
    entries: BTreeMap<Hash, f64>,
    cost: f64,
    decayed: Instant,
}
impl Default for Pending {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            cost: 0.,
            decayed: Instant::now(),
        }
    }
}
impl Pending {
    fn decay(&mut self, now: Instant) {
        let seconds = now.saturating_duration_since(self.decayed).as_secs();
        if seconds > 0 {
            self.cost = self.cost.max(0.) * 0.999f64.powf(seconds as f64);
            self.decayed += Duration::from_secs(seconds);
        }
    }
    pub fn acknowledge(&mut self, id: &Hash, now: Instant) {
        self.decay(now);
        if let Some(cost) = self.entries.remove(id) {
            self.cost = (self.cost - cost).max(0.);
        }
    }
}
impl Budget {
    pub fn new(
        params: &mmx::ChainParams,
        max_upload: f64,
        max_pending: f64,
        now: Instant,
    ) -> Result<Self> {
        let scale = 10f64.powf(-f64::from(params.decimals));
        let block_size = params.max_block_size as f64 * scale;
        let block_time = params.block_interval_ms as f64 * 1e-3;
        let capacity = max_upload * block_size / block_time;
        let pending_limit = max_pending * block_size;
        if !max_upload.is_finite()
            || max_upload < 0.
            || !max_pending.is_finite()
            || max_pending < 0.
            || !scale.is_finite()
            || scale <= 0.
            || block_time <= 0.
            || !capacity.is_finite()
            || !pending_limit.is_finite()
        {
            return Err(error(
                "invalid Router transaction upload limits or chain cost scale",
            ));
        }
        Ok(Self {
            capacity,
            credits: 0.,
            updated: now,
            pending_limit,
            scale,
        })
    }
    pub fn admit(
        &mut self,
        peer: &mut Pending,
        id: Hash,
        static_cost: u64,
        now: Instant,
        sample: f64,
    ) -> bool {
        let seconds = now.saturating_duration_since(self.updated).as_secs_f64();
        self.credits = (self.credits + self.capacity * seconds).min(self.capacity);
        self.updated = now;
        peer.decay(now);
        // Native checks existing debt before adding this transaction, allowing
        // one large transaction to cross the threshold, then waiting for notes.
        if peer.cost >= self.pending_limit
            || (peer.entries.len() >= MAX_UNACKNOWLEDGED && !peer.entries.contains_key(&id))
        {
            return false;
        }
        // C++'s zero-bandwidth NaN comparison bypasses the probability limit.
        // Preserve that config behavior; the peer's pending limit still applies.
        if self.capacity > 0. && sample > self.credits / self.capacity {
            return false;
        }
        let cost = static_cost as f64 * self.scale;
        if !cost.is_finite() {
            return false;
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = peer.entries.entry(id) {
            entry.insert(cost);
            peer.cost += cost;
        }
        self.credits = (self.credits - cost).max(0.);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn params() -> mmx::ChainParams {
        mmx::ChainParams {
            max_block_size: 250_000_000,
            block_interval_ms: 10_000,
            decimals: 6,
            ..Default::default()
        }
    }
    fn peer(at: Instant) -> Pending {
        Pending {
            decayed: at,
            ..Default::default()
        }
    }
    #[test]
    fn native_cost_units_global_probability_and_peer_debt_match_boundary_order() {
        let at = Instant::now();
        let mut budget = Budget::new(&params(), 2., 0.1, at).unwrap();
        assert_eq!(
            (budget.capacity, budget.pending_limit, budget.scale),
            (50., 25., 1e-6)
        );
        let mut a = peer(at);
        assert!(!budget.admit(&mut a, [1; 32], 20_000_000, at, 0.5));
        let now = at + Duration::from_secs(1);
        assert!(budget.admit(&mut a, [1; 32], 20_000_000, now, 0.5));
        assert!(budget.admit(&mut a, [2; 32], 20_000_000, now, 0.1));
        assert_eq!(a.cost, 40.);
        assert!(!budget.admit(&mut a, [3; 32], 1, now, 0.));
        a.acknowledge(&[255; 32], now);
        assert_eq!(a.cost, 40.);
        a.acknowledge(&[1; 32], now);
        assert_eq!(a.cost, 20.);
        assert!(!budget.admit(&mut a, [3; 32], 5_000_000, now, 0.8));
        let mut b = peer(now);
        assert!(!budget.admit(&mut b, [4; 32], 5_000_000, now, 0.8));
        assert!(budget.admit(&mut b, [4; 32], 5_000_000, now, 0.1));
        assert_eq!(budget.credits, 5.);
        assert_eq!(b.cost, 5.);
    }
    #[test]
    fn idle_refill_caps_at_one_second_and_pending_decay_and_late_receipts_are_native() {
        let at = Instant::now();
        let mut budget = Budget::new(&params(), 2., 0.1, at).unwrap();
        let mut peer = peer(at);
        assert!(budget.admit(
            &mut peer,
            [1; 32],
            20_000_000,
            at + Duration::from_secs(1),
            0.5
        ));
        let now = at + Duration::from_secs(3);
        peer.decay(now);
        assert!((peer.cost - 20. * 0.999f64.powi(2)).abs() < 1e-12);
        peer.acknowledge(&[1; 32], now);
        assert_eq!(peer.cost, 0.);
        assert!(peer.entries.is_empty());
        assert!(budget.admit(
            &mut peer,
            [2; 32],
            1_000_000,
            at + Duration::from_secs(1000),
            0.9
        ));
        assert_eq!(budget.credits, 49.);
    }
    #[test]
    fn zero_upload_preserves_native_unlimited_behavior_and_zero_pending_stops_forwarding() {
        let at = Instant::now();
        let mut budget = Budget::new(&params(), 0., 0.1, at).unwrap();
        let mut peer = peer(at);
        assert!(budget.admit(&mut peer, [1; 32], 20_000_000, at, 0.999));
        assert!(budget.admit(&mut peer, [2; 32], 20_000_000, at, 0.999));
        assert!(!budget.admit(&mut peer, [3; 32], 1, at, 0.));
        peer.acknowledge(&[1; 32], at);
        assert!(budget.admit(&mut peer, [3; 32], 1, at, 0.999));
        let mut stopped = Budget::new(&params(), 0., 0., at).unwrap();
        assert!(!stopped.admit(&mut peer, [4; 32], 0, at, 0.));
        assert!(Budget::new(&params(), -1., 0.1, at).is_err());
        assert!(Budget::new(&params(), f64::NAN, 0.1, at).is_err());
    }
    #[test]
    fn unacknowledged_id_bound_also_covers_zero_cost_transactions() {
        let at = Instant::now();
        let mut budget = Budget::new(&params(), 0., 0.1, at).unwrap();
        let mut peer = peer(at);
        for n in 0..MAX_UNACKNOWLEDGED {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            assert!(budget.admit(&mut peer, id, 0, at, 0.5));
        }
        assert!(!budget.admit(&mut peer, [255; 32], 0, at, 0.5));
        peer.acknowledge(&[0; 32], at);
        assert!(budget.admit(&mut peer, [255; 32], 0, at, 0.5));
        assert_eq!(peer.entries.len(), MAX_UNACKNOWLEDGED);
    }
}

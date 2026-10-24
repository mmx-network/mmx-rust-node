//! Bounded immutable-header read cache, separate from validation capabilities.
use mmx_consensus::Hash;
use mmx_types::mmx;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
#[derive(Default)]
pub(super) struct Headers(RefCell<Entries>);
#[derive(Default)]
struct Entries {
    values: BTreeMap<Hash, Arc<mmx::BlockHeader>>,
    order: VecDeque<Hash>,
    // Temporary header-only overlay for detached execution of quarantined
    // bodies. It is bounded by quarantine admission and released at publication.
    pinned: BTreeMap<Hash, Arc<mmx::BlockHeader>>,
}
impl Headers {
    /// Only the retained prefix remains authoritative in a speculative branch.
    pub fn before(&self, height: u32) -> Self {
        let entries = self.0.borrow();
        let mut copy = Entries::default();
        for hash in &entries.order {
            let header = &entries.values[hash];
            if header.height < height {
                copy.order.push_back(*hash);
                copy.values.insert(*hash, header.clone());
            }
        }
        Self(RefCell::new(copy))
    }
    pub fn get_shared(&self, hash: &Hash) -> Option<Arc<mmx::BlockHeader>> {
        let entries = self.0.borrow();
        entries
            .values
            .get(hash)
            .or_else(|| entries.pinned.get(hash))
            .cloned()
    }
    pub fn pin(&self, headers: impl IntoIterator<Item = Arc<mmx::BlockHeader>>) {
        self.0
            .borrow_mut()
            .pinned
            .extend(headers.into_iter().map(|h| (h.hash.0, h)));
    }
    pub fn unpin(&self) {
        self.0.borrow_mut().pinned.clear();
    }
    pub fn insert(&self, header: mmx::BlockHeader) -> Arc<mmx::BlockHeader> {
        let mut entries = self.0.borrow_mut();
        let hash = header.hash.0;
        if let Some(old) = entries.values.get(&hash) {
            return old.clone();
        }
        let header = Arc::new(header);
        entries.values.insert(hash, header.clone());
        entries.order.push_back(hash);
        if entries.order.len() > 2048 {
            if let Some(old) = entries.order.pop_front() {
                entries.values.remove(&old);
            }
        }
        header
    }
    pub fn clear(&self) {
        *self.0.borrow_mut() = Entries::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_pending_ancestry_survives_cache_eviction_and_is_released_at_publication() {
        let cache = Headers::default();
        let hash = [255; 32];
        cache.pin([Arc::new(mmx::BlockHeader {
            hash: hash.into(),
            height: 1,
            ..Default::default()
        })]);
        for n in 0..2050u32 {
            let mut hash = [0; 32];
            hash[..4].copy_from_slice(&n.to_be_bytes());
            cache.insert(mmx::BlockHeader {
                hash: hash.into(),
                height: n + 2,
                ..Default::default()
            });
        }
        assert_eq!(cache.get_shared(&hash).unwrap().height, 1);
        assert_eq!(cache.0.borrow().values.len(), 2048);
        // A new independent snapshot cannot inherit another speculative overlay.
        assert!(cache.before(u32::MAX).get_shared(&hash).is_none());
        cache.unpin();
        assert!(cache.get_shared(&hash).is_none());
    }
}

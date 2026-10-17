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
        self.0.borrow().values.get(hash).cloned()
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

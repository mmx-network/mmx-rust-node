//! Bounded immutable-header read cache, separate from validation capabilities.
use mmx_consensus::Hash;
use mmx_types::mmx;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};
#[derive(Default)]
pub(super) struct Headers(RefCell<Entries>);
#[derive(Default)]
struct Entries {
    values: BTreeMap<Hash, mmx::BlockHeader>,
    order: VecDeque<Hash>,
}
impl Headers {
    pub fn get(&self, hash: &Hash) -> Option<mmx::BlockHeader> {
        self.0.borrow().values.get(hash).cloned()
    }
    pub fn insert(&self, header: &mmx::BlockHeader) {
        let mut entries = self.0.borrow_mut();
        let hash = header.hash.0;
        if entries.values.contains_key(&hash) {
            return;
        }
        entries.values.insert(hash, header.clone());
        entries.order.push_back(hash);
        if entries.order.len() > 2048 {
            if let Some(old) = entries.order.pop_front() {
                entries.values.remove(&old);
            }
        }
    }
    pub fn clear(&self) {
        *self.0.borrow_mut() = Entries::default();
    }
}

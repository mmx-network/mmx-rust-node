//! Router::vdf_history: only verified VDFs bootstrap a new connection.
use crate::{node::number, Result};
use mmx_consensus::Record;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
const MAX_ITEMS: usize = 4096;
const MAX_BYTES: usize = 32 * 1024 * 1024;
pub(super) type Replay = VecDeque<(Arc<Record>, usize)>;
#[derive(Default)]
pub(super) struct History {
    records: BTreeMap<u32, (Arc<Record>, usize)>,
    bytes: usize,
}
impl History {
    pub fn insert(&mut self, record: Arc<Record>, size: usize) -> Result<()> {
        let height = number(&record, "vdf_height")?;
        if size > MAX_BYTES || self.records.contains_key(&height) {
            return Ok(());
        }
        self.records.insert(height, (record, size));
        self.bytes += size;
        while self.records.len() > MAX_ITEMS || self.bytes > MAX_BYTES {
            self.bytes -= self.records.pop_first().unwrap().1 .1;
        }
        Ok(())
    }
    pub fn prune(&mut self, through: u32) {
        while self
            .records
            .first_key_value()
            .is_some_and(|(h, _)| *h <= through)
        {
            self.bytes -= self.records.pop_first().unwrap().1 .1;
        }
    }
    pub fn snapshot(&self) -> Replay {
        self.records.values().cloned().collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;
    use mmx_consensus::Value;
    fn vdf(height: u32) -> Arc<Record> {
        Arc::new(record(
            "mmx.ProofOfTime",
            [("vdf_height".into(), Value::Uint(height.into()))],
        ))
    }
    #[test]
    fn history_keeps_first_verified_work_at_each_height_and_pins_only_bounded_shared_records() {
        let mut history = History::default();
        let first = vdf(1);
        history.insert(first.clone(), 100).unwrap();
        history.insert(vdf(1), 200).unwrap();
        history.insert(vdf(2), 100).unwrap();
        let pinned = history.snapshot();
        assert!(Arc::ptr_eq(&pinned[0].0, &first));
        history.prune(1);
        assert_eq!(history.records.keys().copied().collect::<Vec<_>>(), [2]);
        assert_eq!(history.bytes, 100);
        assert_eq!(pinned.len(), 2);
        for h in 3..43 {
            history.insert(vdf(h), 1024 * 1024).unwrap();
        }
        assert_eq!(history.bytes, MAX_BYTES);
        assert_eq!(history.records.len(), 32);
        history.prune(u32::MAX);
        for h in 0..MAX_ITEMS as u32 + 10 {
            history.insert(vdf(h), 1).unwrap();
        }
        assert_eq!(history.records.len(), MAX_ITEMS);
        assert_eq!(history.records.first_key_value().unwrap().0, &10);
    }
}

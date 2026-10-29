//! Transient VDF work, like C++ Node's vdf_tree/vdf_index. Proof payloads are
//! shared across candidate blocks and never become historical ledger records.
use super::{invalid, ChainResult};
use crate::transaction::bytes;
use mmx_consensus::{Hash, Record};
use std::{collections::BTreeMap, sync::Arc};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 1024;

#[derive(Default)]
pub(super) struct TimeWork {
    records: BTreeMap<Hash, (Arc<Record>, usize, u64)>,
    bytes: usize,
}
impl TimeWork {
    pub fn remember(&mut self, proofs: &[Record]) -> ChainResult<()> {
        let mut additions = BTreeMap::new();
        let mut charged = self.bytes;
        for proof in proofs {
            let id = bytes::<32>(proof.field("content_hash")?)?;
            if self.records.contains_key(&id) || additions.contains_key(&id) {
                continue;
            }
            let end = proof
                .field("start")?
                .uint()?
                .checked_add(proof.field("num_iters")?.uint()?)
                .ok_or_else(|| invalid("VDF work iteration overflow"))?;
            let size = proof.memory_size().saturating_add(128);
            charged = charged.saturating_add(size);
            if charged > MAX_BYTES || self.records.len() + additions.len() >= MAX_RECORDS {
                return Err(invalid("transient VDF work capacity"));
            }
            additions.insert(id, (Arc::new(proof.clone()), size, end));
        }
        self.records.extend(additions);
        self.bytes = charged;
        Ok(())
    }
    pub fn prune(&mut self, root_iters: u64) {
        self.records.retain(|_, (_, size, end)| {
            if *end <= root_iters {
                self.bytes -= *size;
                false
            } else {
                true
            }
        });
    }
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.values().map(|(proof, _, _)| proof.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mmx_consensus::Value;

    fn proof(id: u8, end: u64, size: usize) -> Record {
        Record {
            name: "mmx.ProofOfTime".into(),
            fields: [
                ("content_hash".into(), Value::Bytes(vec![id; 32])),
                ("start".into(), Value::Uint(0)),
                ("num_iters".into(), Value::Uint(end)),
                ("padding".into(), Value::Bytes(vec![0; size])),
            ]
            .into(),
        }
    }
    #[test]
    fn shared_work_is_deduplicated_bounded_atomically_and_released_at_root() {
        let mut work = TimeWork::default();
        let first = proof(1, 10, 64);
        work.remember(&[first.clone(), first.clone()]).unwrap();
        let bytes = work.bytes;
        work.remember(&[first]).unwrap();
        assert_eq!(work.bytes, bytes);
        assert_eq!(work.records.len(), 1);
        assert!(work
            .remember(&[proof(2, 20, 64), proof(3, 30, MAX_BYTES)])
            .is_err());
        assert_eq!(work.bytes, bytes);
        assert_eq!(work.records.len(), 1);
        work.remember(&[proof(2, 20, 64)]).unwrap();
        work.prune(10);
        assert_eq!(work.records.len(), 1);
        work.prune(20);
        assert!(work.records.is_empty());
        assert_eq!(work.bytes, 0);
    }
}

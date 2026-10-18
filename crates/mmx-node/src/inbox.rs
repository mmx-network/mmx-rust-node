//! Bounded transport handoff. Peer I/O never needs the ledger mutex to enqueue.
use crate::{error, Result};
use mmx_consensus::Record;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_MESSAGES: usize = 16_384;
#[derive(Clone, Default)]
pub(crate) struct Inbox(Arc<Mutex<Queue>>);
#[derive(Default)]
struct Queue {
    messages: VecDeque<(Record, usize)>,
    bytes: usize,
    transactions: VecDeque<(Record, usize)>,
    transaction_bytes: usize,
    dropped: u64,
}
#[derive(Clone, Copy)]
pub(crate) struct Stats {
    pub messages: usize,
    pub bytes: usize,
    pub dropped: u64,
}
impl Inbox {
    pub fn submit(&self, record: Record, wire_bytes: usize) -> Result<()> {
        if !matches!(
            record.name.as_str(),
            "mmx.Block"
                | "mmx.ProofOfTime"
                | "mmx.ValidatorVote"
                | "mmx.ProofResponse"
                | "mmx.Transaction"
        ) {
            return Ok(());
        }
        let wire_bytes = wire_bytes.max(record.memory_size());
        let mut queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
        if record.name == "mmx.Transaction" {
            if wire_bytes > 256 * 1024 {
                queue.dropped = queue.dropped.wrapping_add(1);
                return Ok(());
            }
            while queue.transaction_bytes + wire_bytes > 8 * 1024 * 1024
                || queue.transactions.len() >= 256
            {
                let (_, bytes) = queue
                    .transactions
                    .pop_front()
                    .expect("nonempty transaction inbox");
                queue.transaction_bytes -= bytes;
                queue.dropped = queue.dropped.wrapping_add(1);
            }
            queue.transaction_bytes += wire_bytes;
            queue.transactions.push_back((record, wire_bytes));
            return Ok(());
        }
        if wire_bytes > MAX_BYTES {
            queue.dropped = queue.dropped.wrapping_add(1);
            return Ok(());
        }
        while queue.bytes + wire_bytes > MAX_BYTES || queue.messages.len() >= MAX_MESSAGES {
            let (_, bytes) = queue.messages.pop_front().expect("nonempty bounded inbox");
            queue.bytes -= bytes;
            queue.dropped = queue.dropped.wrapping_add(1);
        }
        queue.messages.push_back((record, wire_bytes));
        queue.bytes += wire_bytes;
        Ok(())
    }
    pub fn take(&self) -> Result<VecDeque<(Record, usize)>> {
        let mut queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
        queue.bytes = 0;
        let mut messages = std::mem::take(&mut queue.messages);
        for _ in 0..4 {
            if let Some((r, size)) = queue.transactions.pop_front() {
                queue.transaction_bytes -= size;
                messages.push_back((r, size));
            }
        }
        Ok(messages)
    }
    pub fn clear(&self) -> Result<()> {
        let mut queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
        queue.messages.clear();
        queue.transactions.clear();
        queue.bytes = 0;
        queue.transaction_bytes = 0;
        Ok(())
    }
    pub fn stats(&self) -> Result<Stats> {
        let queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
        Ok(Stats {
            messages: queue.messages.len() + queue.transactions.len(),
            bytes: queue.bytes + queue.transaction_bytes,
            dropped: queue.dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;

    #[test]
    fn transaction_flood_cannot_evict_consensus_messages() {
        let inbox = Inbox::default();
        inbox.submit(record("mmx.Block", []), 100).unwrap();
        for _ in 0..300 {
            inbox.submit(record("mmx.Transaction", []), 100).unwrap();
        }
        let batch = inbox.take().unwrap();
        assert_eq!(batch.len(), 5);
        assert_eq!(batch[0].0.name, "mmx.Block");
        assert_eq!(inbox.stats().unwrap().messages, 252);
        assert_eq!(inbox.stats().unwrap().dropped, 44);
        inbox.clear().unwrap();
        assert_eq!(inbox.stats().unwrap().bytes, 0);
    }

    #[test]
    fn handoff_preserves_fifo_and_bounds_bytes_and_count() {
        let inbox = Inbox::default();
        inbox
            .submit(record("mmx.ValidatorVote", []), MAX_BYTES)
            .unwrap();
        inbox.submit(record("mmx.ProofOfTime", []), 1).unwrap();
        assert_eq!(inbox.stats().unwrap().dropped, 1);
        assert_eq!(
            inbox.take().unwrap().pop_front().unwrap().0.name,
            "mmx.ProofOfTime"
        );
        assert_eq!(inbox.stats().unwrap().bytes, 0);
        inbox
            .submit(record("mmx.Block", []), MAX_BYTES + 1)
            .unwrap();
        assert_eq!(inbox.stats().unwrap().dropped, 2);
        assert!(inbox.take().unwrap().is_empty());
        for _ in 0..MAX_MESSAGES + 1 {
            inbox.submit(record("mmx.ProofResponse", []), 0).unwrap();
        }
        assert_eq!(inbox.stats().unwrap().messages, MAX_MESSAGES);
        assert_eq!(inbox.stats().unwrap().dropped, 3);
        inbox.clear().unwrap();
        assert_eq!(inbox.stats().unwrap().messages, 0);
        let vote = record("mmx.ValidatorVote", []);
        let block = record("mmx.Block", []);
        let charged = vote.memory_size().max(10) + block.memory_size().max(20);
        inbox.submit(vote, 10).unwrap();
        inbox.submit(block, 20).unwrap();
        assert_eq!(inbox.stats().unwrap().bytes, charged);
        assert_eq!(
            inbox
                .take()
                .unwrap()
                .into_iter()
                .map(|(r, _)| r.name)
                .collect::<Vec<_>>(),
            ["mmx.ValidatorVote", "mmx.Block"]
        );
    }
    #[test]
    fn decoded_heap_is_charged_even_when_the_wire_size_is_tiny() {
        let inbox = Inbox::default();
        let mut block = record("mmx.Block", []);
        block.fields.insert(
            "test".into(),
            mmx_consensus::Value::Sequence(vec![mmx_consensus::Value::Null; 1024]),
        );
        let size = block.memory_size();
        inbox.submit(block, 10).unwrap();
        assert_eq!(inbox.stats().unwrap().bytes, size);
        assert!(size > 50_000);
    }
}

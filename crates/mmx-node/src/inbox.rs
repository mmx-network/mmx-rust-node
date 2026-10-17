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
            "mmx.Block" | "mmx.ProofOfTime" | "mmx.ValidatorVote" | "mmx.ProofResponse"
        ) {
            return Ok(());
        }
        let mut queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
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
        Ok(std::mem::take(&mut queue.messages))
    }
    pub fn clear(&self) -> Result<()> {
        drop(self.take()?);
        Ok(())
    }
    pub fn stats(&self) -> Result<Stats> {
        let queue = self.0.lock().map_err(|_| error("peer inbox poisoned"))?;
        Ok(Stats {
            messages: queue.messages.len(),
            bytes: queue.bytes,
            dropped: queue.dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;

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
        inbox.submit(record("mmx.ValidatorVote", []), 10).unwrap();
        inbox.submit(record("mmx.Block", []), 20).unwrap();
        assert_eq!(inbox.stats().unwrap().bytes, 30);
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
}

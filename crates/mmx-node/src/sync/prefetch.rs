//! One bounded network window ahead of ledger validation, with backpressure.
use super::{Progress, Source, BATCH_BYTES};
use crate::{error, Node, Result};
use mmx_consensus::{Hash, Record};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
};

enum Command {
    Height,
    Hash(u32),
    Blocks(u32, u32),
}
enum Reply {
    Height(Result<u32>),
    Hash(Result<Option<Hash>>),
    Blocks(Result<Vec<Option<Record>>>),
}
struct Producer {
    commands: SyncSender<Command>,
    replies: Receiver<Reply>,
    pending: bool,
    cached: VecDeque<Option<Record>>,
    next: u32,
}
impl Producer {
    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| error("sync producer stopped"))
    }
    fn receive(&self) -> Result<Reply> {
        self.replies
            .recv()
            .map_err(|_| error("sync producer disconnected"))
    }
    fn collect(&mut self) -> Result<()> {
        if self.pending {
            let Reply::Blocks(result) = self.receive()? else {
                return Err(error("unexpected sync producer reply"));
            };
            self.pending = false;
            self.cached.extend(result?);
            if self.cached.is_empty() {
                return Err(error("peer omitted prefetched window"));
            }
        }
        Ok(())
    }
}
impl Source for Producer {
    fn height(&mut self) -> Result<u32> {
        self.collect()?;
        self.send(Command::Height)?;
        match self.receive()? {
            Reply::Height(result) => result,
            _ => Err(error("unexpected sync height reply")),
        }
    }
    fn hash_at(&mut self, height: u32) -> Result<Option<Hash>> {
        self.collect()?;
        self.send(Command::Hash(height))?;
        match self.receive()? {
            Reply::Hash(result) => result,
            _ => Err(error("unexpected sync hash reply")),
        }
    }
    fn block_at(&mut self, height: u32) -> Result<Option<Record>> {
        self.blocks_at(height, 1)?
            .pop()
            .ok_or_else(|| error("peer omitted block"))
    }
    fn prefetch(&mut self, start: u32, count: u32) -> Result<()> {
        if count != 0 && !self.pending && self.cached.is_empty() {
            self.send(Command::Blocks(start, count))?;
            self.next = start;
            self.pending = true;
        }
        Ok(())
    }
    fn blocks_at(&mut self, start: u32, count: u32) -> Result<Vec<Option<Record>>> {
        let mut blocks = Vec::with_capacity(count as usize);
        while blocks.len() < count as usize {
            let current = start
                .checked_add(blocks.len() as u32)
                .ok_or_else(|| error("sync height overflow"))?;
            if !self.pending && self.cached.is_empty() {
                self.prefetch(current, count - blocks.len() as u32)?;
            }
            self.collect()?;
            if self.next != current {
                return Err(error("prefetched height discontinuity"));
            }
            while blocks.len() < count as usize {
                let Some(block) = self.cached.pop_front() else {
                    break;
                };
                blocks.push(block);
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| error("sync height overflow"))?;
            }
        }
        Ok(blocks)
    }
}
fn window(
    source: &mut impl Source,
    start: u32,
    count: u32,
    byte_limit: usize,
    stop: &AtomicBool,
) -> Result<Vec<Option<Record>>> {
    let mut blocks = Vec::new();
    let mut bytes = 0usize;
    while blocks.len() < count as usize && bytes < byte_limit {
        if stop.load(Ordering::Relaxed) {
            return Err(error("sync cancelled"));
        }
        let next = start
            .checked_add(blocks.len() as u32)
            .ok_or_else(|| error("sync height overflow"))?;
        let n = (count - blocks.len() as u32).min(4);
        let values = source.blocks_at(next, n)?;
        if values.len() != n as usize {
            return Err(error("peer omitted advertised block window"));
        }
        for value in values {
            if let Some(record) = &value {
                bytes = bytes
                    .checked_add(record.encode()?.len())
                    .ok_or_else(|| error("sync byte count overflow"))?;
            }
            blocks.push(value);
        }
    }
    Ok(blocks)
}

/// Fetch at most one window ahead on a scoped worker. The producer owns the
/// native connection throughout; final tip checks still run after publication.
/// Dropping either channel cancels queued work and joins the producer on errors.
pub fn synchronize_pipelined<S: Source + Send>(
    source: &mut S,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    report: impl FnMut(&Progress),
) -> Result<Progress> {
    with_producer(source, stop, |producer| {
        super::synchronize(producer, node, stop, report)
    })
}
fn with_producer<S: Source + Send, R>(
    source: &mut S,
    stop: &AtomicBool,
    operation: impl FnOnce(&mut Producer) -> R,
) -> R {
    thread::scope(|scope| {
        let (commands, incoming) = sync_channel(1);
        let (outgoing, replies) = sync_channel(1);
        scope.spawn(move || {
            while let Ok(command) = incoming.recv() {
                let reply = match command {
                    Command::Height => Reply::Height(source.height()),
                    Command::Hash(h) => Reply::Hash(source.hash_at(h)),
                    Command::Blocks(start, count) => {
                        Reply::Blocks(window(source, start, count, BATCH_BYTES, stop))
                    }
                };
                if outgoing.send(reply).is_err() {
                    break;
                }
            }
        });
        let mut producer = Producer {
            commands,
            replies,
            pending: false,
            cached: VecDeque::new(),
            next: 0,
        };
        let result = operation(&mut producer);
        drop(producer);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::record;
    use mmx_consensus::Value;
    use std::{sync::mpsc, time::Duration};

    #[derive(Default)]
    struct Numbers {
        calls: Vec<(u32, u32)>,
        fail: bool,
        omit: bool,
        gate: Option<(mpsc::Sender<u32>, mpsc::Receiver<()>)>,
    }
    impl Source for Numbers {
        fn height(&mut self) -> Result<u32> {
            Ok(100)
        }
        fn hash_at(&mut self, height: u32) -> Result<Option<Hash>> {
            Ok(Some([height as u8; 32]))
        }
        fn block_at(&mut self, height: u32) -> Result<Option<Record>> {
            Ok(self.blocks_at(height, 1)?.pop().unwrap())
        }
        fn blocks_at(&mut self, start: u32, count: u32) -> Result<Vec<Option<Record>>> {
            self.calls.push((start, count));
            if let Some((started, release)) = &self.gate {
                started.send(start)?;
                release.recv_timeout(Duration::from_secs(5))?;
            }
            if self.fail {
                return Err(error("injected source failure"));
            }
            Ok((0..count - u32::from(self.omit))
                .map(|i| {
                    Some(record(
                        "mmx.Node.get_height.return",
                        [("_ret_0".into(), Value::Uint(u64::from(start + i)))],
                    ))
                })
                .collect())
        }
    }
    fn heights(records: Vec<Option<Record>>) -> Vec<u64> {
        records
            .into_iter()
            .map(|r| r.unwrap().field("_ret_0").unwrap().uint().unwrap())
            .collect()
    }
    #[test]
    fn prefetched_windows_keep_partial_groups_and_metadata_order() {
        let mut source = Numbers::default();
        with_producer(&mut source, &AtomicBool::new(false), |p| {
            p.prefetch(10, 2).unwrap();
            assert_eq!(p.height().unwrap(), 100);
            assert_eq!(heights(p.blocks_at(10, 4).unwrap()), [10, 11, 12, 13]);
            p.prefetch(14, 8).unwrap();
            assert_eq!(heights(p.blocks_at(14, 3).unwrap()), [14, 15, 16]);
            assert_eq!(p.hash_at(99).unwrap(), Some([99; 32]));
            assert_eq!(heights(p.blocks_at(17, 5).unwrap()), [17, 18, 19, 20, 21]);
        });
        assert_eq!(source.calls, [(10, 2), (12, 2), (14, 4), (18, 4)]);
    }
    #[test]
    fn windows_bound_bytes_and_reject_missing_results_and_cancelled_work() {
        let stop = AtomicBool::new(false);
        let mut source = Numbers::default();
        assert_eq!(window(&mut source, 0, 64, 1, &stop).unwrap().len(), 4);
        assert_eq!(source.calls, [(0, 4)]);
        source.omit = true;
        assert!(window(&mut source, 4, 4, BATCH_BYTES, &stop).is_err());
        source.calls.clear();
        stop.store(true, Ordering::Relaxed);
        assert!(window(&mut source, 4, 4, BATCH_BYTES, &stop).is_err());
        assert!(source.calls.is_empty());
    }
    #[test]
    fn prefetch_runs_before_consumption_and_joins_after_early_return() {
        let (started, notify) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let mut source = Numbers {
            gate: Some((started, wait)),
            ..Default::default()
        };
        with_producer(&mut source, &AtomicBool::new(false), |p| {
            p.prefetch(20, 4).unwrap();
            assert_eq!(notify.recv_timeout(Duration::from_secs(5)).unwrap(), 20);
            release.send(()).unwrap();
            // Return without consuming an outstanding window, as after a
            // consensus rejection or operator cancellation in the consumer.
        });
        assert_eq!(source.calls, [(20, 4)]);
    }
    #[test]
    fn prefetched_source_errors_propagate_and_terminate_cleanly() {
        let mut source = Numbers {
            fail: true,
            ..Default::default()
        };
        with_producer(&mut source, &AtomicBool::new(false), |p| {
            p.prefetch(20, 4).unwrap();
            let error = p.blocks_at(20, 4).unwrap_err();
            assert_eq!(error.to_string(), "injected source failure");
        });
        assert_eq!(source.calls, [(20, 4)]);
    }
}

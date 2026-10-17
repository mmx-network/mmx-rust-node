//! One multi-peer window ahead, with a single owner for all hash/body jobs.
use super::{PeerPool, PeerSource, ReadContext, Window, WindowSource, BATCH_BLOCKS};
use crate::{error, Node, Result};
use std::{
    sync::{
        atomic::AtomicBool,
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
};

enum Command {
    Refresh,
    Window(u32, u32, Box<ReadContext>),
}
enum Reply {
    Refresh(Result<u32>),
    Window(Result<Window>),
}
pub(super) struct Producer<'a> {
    commands: SyncSender<Command>,
    replies: Receiver<Reply>,
    pending: bool,
    cached: Option<Window>,
    node: &'a Arc<Mutex<Node>>,
}
impl Producer<'_> {
    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| error("peer coordinator producer stopped"))
    }
    fn receive(&self) -> Result<Reply> {
        self.replies
            .recv()
            .map_err(|_| error("peer coordinator producer disconnected"))
    }
    fn collect(&mut self) -> Result<()> {
        if self.pending {
            let Reply::Window(window) = self.receive()? else {
                return Err(error("unexpected peer coordinator window reply"));
            };
            self.pending = false;
            self.cached = Some(window?);
        }
        Ok(())
    }
}
impl WindowSource for Producer<'_> {
    fn refresh(&mut self) -> Result<u32> {
        self.collect()?;
        self.send(Command::Refresh)?;
        match self.receive()? {
            Reply::Refresh(result) => result,
            _ => Err(error("unexpected peer coordinator status reply")),
        }
    }
    fn prefetch(&mut self, start: u32, count: u32, local: &mut Node) -> Result<()> {
        if count == 0 || count > BATCH_BLOCKS || start.checked_add(count).is_none() {
            return Err(error("invalid peer coordinator prefetch window"));
        }
        if self.pending || self.cached.is_some() {
            return Err(error("peer coordinator already has a queued window"));
        }
        // The caller already holds the ledger mutex. Register here so the
        // producer can begin peer I/O while that caller validates the ledger.
        let context = ReadContext::new(local)?;
        local.sync_pending.extend(start..start + count);
        self.send(Command::Window(start, count, Box::new(context)))?;
        self.pending = true;
        Ok(())
    }
    fn window(&mut self, start: u32, count: u32) -> Result<Window> {
        if !self.pending && self.cached.is_none() {
            let node = self.node.clone();
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            self.prefetch(start, count, &mut local)?;
        }
        self.collect()?;
        let window = self
            .cached
            .take()
            .ok_or_else(|| error("peer coordinator omitted window"))?;
        if window.start != start
            || window.heights.is_empty()
            || window.heights.len() > count as usize
        {
            return Err(error("peer coordinator window discontinuity"));
        }
        Ok(window)
    }
}
pub(super) fn with_pool<S: PeerSource, T>(
    pool: &mut PeerPool<S>,
    node: &Arc<Mutex<Node>>,
    stop: &AtomicBool,
    work: impl FnOnce(&mut Producer<'_>) -> Result<T>,
) -> Result<T> {
    thread::scope(|scope| {
        let (commands, incoming) = sync_channel(1);
        let (outgoing, replies) = sync_channel(1);
        let worker = scope.spawn(move || {
            while let Ok(command) = incoming.recv() {
                let reply = match command {
                    Command::Refresh => Reply::Refresh(pool.refresh()),
                    Command::Window(start, count, context) => {
                        Reply::Window(pool.prepared_window(start, count, *context, node, stop))
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
            cached: None,
            node,
        };
        let result = work(&mut producer);
        // Release both channel endpoints before joining, including when a
        // consumer exits before receiving its outstanding network window.
        drop(producer);
        worker
            .join()
            .map_err(|_| error("peer coordinator producer panicked"))?;
        result
    })
}

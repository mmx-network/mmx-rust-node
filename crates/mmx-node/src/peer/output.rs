//! Bounded, ordered socket output. Encoding is transactional; socket writes
//! never hold the encoder/queue mutex needed by the receive owner.
use super::{frame_bytes_with_options, Relay, RelayPeer, MAX_FRAME};
use crate::{error, Result};
use mmx_consensus::Record;
use std::{
    collections::VecDeque,
    io::{self, Write},
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};
#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;

const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_POLL: Duration = Duration::from_millis(50);
const MAX_MESSAGES: usize = 256;
// Rejections must remain cheap and bounded even when normal replies fill the
// queue. A peer that fills this reserve is disconnected rather than losing RPCs.
const REJECTION_MESSAGES: usize = 32;
const REJECTION_BYTES: usize = 16 * 1024;

pub(super) struct Output {
    pub stream: TcpStream,
    pub outbound: mmx_consensus::StreamEncoder,
    pub compressed_limit: usize,
    pub compress_level: i32,
    queue: Option<Queue>,
}
struct Batch {
    bytes: Box<[u8]>,
    messages: usize,
    rejection: bool,
    end: Instant,
}
struct Queue {
    pending: VecDeque<Batch>,
    // Charges include the batch currently owned by the writer, even after pop.
    bytes: usize,
    messages: usize,
    rejection_bytes: usize,
    rejections: usize,
    limit: usize,
    blocked: bool,
    closed: bool,
    wake: Option<Thread>,
    relay: Option<(Relay, RelayPeer)>,
}
impl Queue {
    fn new(limit: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            bytes: 0,
            messages: 0,
            rejection_bytes: 0,
            rejections: 0,
            limit,
            blocked: false,
            closed: false,
            wake: None,
            relay: None,
        }
    }
}
struct Socket {
    stream: TcpStream,
    timeout: Duration,
}
impl Drop for Socket {
    fn drop(&mut self) {
        // Also interrupt the receive owner if the writer unwinds unexpectedly.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
pub(super) struct Worker {
    pub stop: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}
fn full() -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::new(io::ErrorKind::WouldBlock, "peer write queue is full").into()
}
impl Output {
    pub fn new(stream: TcpStream, compressed_limit: usize) -> Self {
        Self {
            stream,
            outbound: Default::default(),
            compressed_limit,
            compress_level: 3,
            queue: None,
        }
    }
    pub fn send_batch(&mut self, values: &[Record]) -> Result<()> {
        self.send(values, false)
    }
    pub fn pressured(&self) -> bool {
        self.queue
            .as_ref()
            .is_some_and(|q| q.blocked || q.bytes >= q.limit || q.messages >= MAX_MESSAGES)
    }
    pub fn reject(&mut self, value: &Record) -> Result<()> {
        self.send(std::slice::from_ref(value), true)
    }
    fn send(&mut self, values: &[Record], rejection: bool) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        if let Some(q) = &self.queue {
            if q.closed {
                return Err(error("peer output closed"));
            }
            if rejection {
                if q.rejections.saturating_add(values.len()) > REJECTION_MESSAGES
                    || q.rejection_bytes >= REJECTION_BYTES
                {
                    return Err(full());
                }
            } else if q.bytes >= q.limit || q.messages.saturating_add(values.len()) > MAX_MESSAGES {
                return Err(full());
            }
        }
        let mut outbound = self.outbound.clone();
        let mut bytes = Vec::new();
        for value in values {
            let frame = frame_bytes_with_options(
                &outbound.encode(value)?,
                self.compressed_limit,
                self.compress_level,
            )?;
            if bytes.len().saturating_add(frame.len()) > MAX_FRAME + 8 {
                return Err(error("peer write batch exceeds limit"));
            }
            bytes.extend_from_slice(&frame);
        }
        if let Some(q) = &mut self.queue {
            if rejection {
                if q.rejection_bytes.saturating_add(bytes.len()) > REJECTION_BYTES {
                    return Err(full());
                }
                q.rejection_bytes += bytes.len();
                q.rejections += values.len();
            } else {
                // Like MsgServer, admit below the byte threshold before
                // encoding, permitting one bounded batch to cross it. Large
                // legitimate records need not fit a second, smaller frame cap.
                q.bytes += bytes.len();
                q.messages += values.len();
            }
            q.pending.push_back(Batch {
                bytes: bytes.into_boxed_slice(),
                messages: values.len(),
                rejection,
                end: Instant::now() + WRITE_TIMEOUT,
            });
            if let Some(wake) = &q.wake {
                wake.unpark();
            }
        } else {
            // Low-level Connection helpers remain synchronous. Production
            // clients enable the owned writer before authentication begins.
            if let Err(e) = self.stream.write_all(&bytes) {
                let _ = self.stream.shutdown(Shutdown::Both);
                return Err(e.into());
            }
        }
        // Queued batches retain ownership and FIFO order. Any write failure
        // closes the connection, so advanced schemas cannot escape unwritten.
        self.outbound = outbound;
        Ok(())
    }
    pub fn attach(&mut self, relay: Relay, sender: RelayPeer) -> Result<()> {
        let queue = self.queue.as_mut().ok_or_else(|| error("no peer writer"))?;
        if queue.closed {
            return Err(error("peer output closed"));
        }
        queue.relay = Some((relay, sender));
        if let Some(wake) = &queue.wake {
            wake.unpark();
        }
        Ok(())
    }
}
impl Worker {
    pub fn start(
        output: Arc<Mutex<Output>>,
        stream: TcpStream,
        limit: usize,
        interval: Duration,
        expires: Option<Instant>,
    ) -> Result<Self> {
        if limit == 0 {
            return Err(error("Router.max_write_queue must be positive"));
        }
        stream.set_write_timeout(Some(WRITE_POLL))?;
        let mut socket = Socket {
            stream,
            timeout: WRITE_POLL,
        };
        output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .queue = Some(Queue::new(limit));
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let shared = output.clone();
        let thread = thread::Builder::new()
            .name("mmx-peer-send".into())
            .spawn(move || {
                let mut relay = None;
                let mut run = || -> Result<()> {
                    loop {
                        check_end(&stopping, expires)?;
                        if let Some(attached) = shared
                            .lock()
                            .map_err(|_| error("peer output poisoned"))?
                            .queue
                            .as_mut()
                            .and_then(|q| q.relay.take())
                        {
                            relay = Some(attached);
                        }
                        let mut sent = drain(&shared, &mut socket, &stopping, expires)? > 0;
                        if let Some((relay, sender)) = &mut relay {
                            sender.flush_one(relay, |record| {
                                // Control work gets the next frame boundary.
                                // Never hold the encoder mutex across writes.
                                drain(&shared, &mut socket, &stopping, expires)?;
                                shared
                                    .lock()
                                    .map_err(|_| error("peer output poisoned"))?
                                    .send_batch(std::slice::from_ref(record))?;
                                drain(&shared, &mut socket, &stopping, expires)?;
                                sent = true;
                                Ok(())
                            })?;
                        }
                        if !sent {
                            let idle = expires.map_or(interval, |end| {
                                interval.min(end.saturating_duration_since(Instant::now()))
                            });
                            // unpark retains a token even if enqueue, publish,
                            // attach or stop happens just before this park.
                            thread::park_timeout(idle);
                        }
                    }
                };
                let result = run();
                let mut output = shared.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(q) = &mut output.queue {
                    q.closed = true;
                    q.pending.clear();
                    q.relay = None;
                }
                drop(output);
                let _ = socket.stream.shutdown(Shutdown::Both);
                if let Err(error) = result {
                    if !stopping.load(Ordering::Acquire) {
                        eprintln!("Peer output closed: {error}");
                    }
                }
            })?;
        output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .queue
            .as_mut()
            .ok_or_else(|| error("no peer write queue"))?
            .wake = Some(thread.thread().clone());
        thread.thread().unpark();
        Ok(Self { stop, thread })
    }
}
fn check_end(stop: &AtomicBool, end: Option<Instant>) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err(error("peer writer stopped"));
    }
    if end.is_some_and(|end| Instant::now() >= end) {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "peer write timed out").into());
    }
    Ok(())
}
fn drain(
    output: &Mutex<Output>,
    socket: &mut Socket,
    stop: &AtomicBool,
    expires: Option<Instant>,
) -> Result<usize> {
    let mut sent = 0;
    loop {
        check_end(stop, expires)?;
        let batch = {
            let mut output = output.lock().map_err(|_| error("peer output poisoned"))?;
            let q = output
                .queue
                .as_mut()
                .ok_or_else(|| error("no peer writer"))?;
            let Some(batch) = q.pending.pop_front() else {
                q.blocked = false;
                return Ok(sent);
            };
            batch
        };
        let end = expires.map_or(batch.end, |end| end.min(batch.end));
        let mut offset = 0;
        while offset < batch.bytes.len() {
            check_end(stop, Some(end))?;
            let timeout = WRITE_POLL
                .min(end.saturating_duration_since(Instant::now()))
                .max(Duration::from_millis(1));
            if timeout != socket.timeout {
                socket.stream.set_write_timeout(Some(timeout))?;
                socket.timeout = timeout;
            }
            let blocked = match socket.stream.write(&batch.bytes[offset..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) => {
                    offset += count;
                    offset < batch.bytes.len()
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    true
                }
                Err(e) => return Err(e.into()),
            };
            if blocked {
                if let Some(q) = &mut output
                    .lock()
                    .map_err(|_| error("peer output poisoned"))?
                    .queue
                {
                    q.blocked = true;
                }
            }
        }
        let mut output = output.lock().map_err(|_| error("peer output poisoned"))?;
        let q = output
            .queue
            .as_mut()
            .ok_or_else(|| error("no peer writer"))?;
        if batch.rejection {
            q.rejections -= batch.messages;
            q.rejection_bytes -= batch.bytes.len();
        } else {
            q.messages -= batch.messages;
            q.bytes -= batch.bytes.len();
        }
        sent += batch.messages;
    }
}

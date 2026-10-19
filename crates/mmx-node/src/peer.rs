//! Native vnx.addons.MsgServer framing: u32 compressed size, u32 magic, Zstd
//! VNX record. Type declarations persist across frames on each connection. Consensus
//! records are decoded losslessly, including byte strings and Variant payloads.
use crate::{
    error,
    inbox::Inbox,
    node::{number, record},
    peer_state::PeerState,
    transactions::{Relay, RelayPeer},
    Node, Result,
};
use mmx_consensus::{Hash, Record, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAGIC: u32 = 0x4B277B06;
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Bound native block lookups pipelined on a single peer connection.
pub const MAX_BODY_REQUESTS: usize = 32;
const MAX_RPC_REQUESTS: usize = 128;

/// Native get_block_hash_ex: the chain ID and the complete content commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHash {
    pub block: Hash,
    pub content: Hash,
}

fn optional_block_hash(value: Value) -> Result<Option<BlockHash>> {
    value
        .optional()?
        .map(|v| {
            let pair = v.seq()?;
            if pair.len() != 2 {
                return Err(error("invalid peer hash pair"));
            }
            Ok(BlockHash {
                block: pair[0].bytes()?.try_into()?,
                content: pair[1].bytes()?.try_into()?,
            })
        })
        .transpose()
}

pub fn encode_frame(value: &Record) -> Result<Vec<u8>> {
    let raw = value.encode()?;
    frame_bytes(&raw)
}
fn frame_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() > MAX_FRAME {
        return Err(error("peer record exceeds frame limit"));
    }
    let data = zstd::stream::encode_all(raw, 3)?;
    if data.len() > MAX_FRAME {
        return Err(error("compressed peer frame exceeds limit"));
    }
    Ok([
        (data.len() as u32).to_le_bytes().as_slice(),
        &MAGIC.to_le_bytes(),
        &data,
    ]
    .concat())
}
pub fn decode_frame(frame: &[u8]) -> Result<Record> {
    Ok(decode_frame_with_registry(frame, &mut vnx::Registry::new())?.0)
}
fn decode_frame_with_registry(
    frame: &[u8],
    registry: &mut vnx::Registry,
) -> Result<(Record, usize)> {
    if frame.len() < 8 {
        return Err(error("truncated peer frame"));
    }
    let mut size = u32::from_le_bytes(frame[..4].try_into()?);
    let magic = u32::from_le_bytes(frame[4..8].try_into()?);
    if magic == MAGIC.swap_bytes() {
        size = size.swap_bytes();
    } else if magic != MAGIC {
        return Err(error("invalid peer frame magic"));
    }
    if size == 0 || size as usize > MAX_FRAME || frame.len() != size as usize + 8 {
        return Err(error("invalid peer frame size"));
    }
    let decoder = zstd::stream::read::Decoder::new(&frame[8..])?;
    let mut raw = Vec::new();
    decoder.take(MAX_FRAME as u64 + 1).read_to_end(&mut raw)?;
    if raw.len() > MAX_FRAME {
        return Err(error("decompressed peer frame exceeds limit"));
    }
    Ok((Record::decode_with_registry(&raw, registry)?, raw.len()))
}
pub struct Connection {
    stream: TcpStream,
    registry: vnx::Registry,
    outbound: mmx_consensus::StreamEncoder,
    last_size: usize,
}
impl Connection {
    pub fn connect(address: &str) -> Result<Self> {
        let mut last = None;
        for addr in address.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, Duration::from_secs(3)) {
                Ok(stream) => return Self::new(stream),
                Err(e) => last = Some(e),
            }
        }
        Err(last
            .map(Into::into)
            .unwrap_or_else(|| error("peer has no addresses")))
    }
    pub fn new(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            stream,
            registry: vnx::Registry::new(),
            outbound: Default::default(),
            last_size: 0,
        })
    }
    pub fn send(&mut self, value: &Record) -> Result<()> {
        self.send_batch(std::slice::from_ref(value))
    }
    fn send_batch(&mut self, values: &[Record]) -> Result<()> {
        // Encoding, frame limits and I/O can all fail. Only successful writes
        // may announce schemas to this connection's subsequent frames.
        let mut outbound = self.outbound.clone();
        let mut batch = Vec::new();
        for value in values {
            let frame = frame_bytes(&outbound.encode(value)?)?;
            if batch.len().saturating_add(frame.len()) > MAX_FRAME + 8 {
                return Err(error("peer write batch exceeds limit"));
            }
            batch.extend_from_slice(&frame);
        }
        if let Err(e) = self.stream.write_all(&batch) {
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            return Err(e.into());
        }
        self.outbound = outbound;
        Ok(())
    }
    /// None means idle before the first frame byte. Partial-frame timeouts close
    /// the connection, so a later read can never mistake payload for a header.
    pub fn receive(&mut self, idle: Duration) -> Result<Option<Record>> {
        self.receive_until(idle, Instant::now() + Duration::from_secs(10))
    }
    fn quick_ack(&self) {
        // Native peers can leave small response tails behind Nagle's gate.
        // QUICKACK is transient, so refresh the receiver hint after reads.
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::TcpStreamExt;
            let _ = self.stream.set_quickack(true);
        }
    }
    fn receive_until(&mut self, idle: Duration, end: Instant) -> Result<Option<Record>> {
        let result = self.receive_frame_until(idle, end);
        if result.is_err() {
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }
    fn receive_frame_until(&mut self, idle: Duration, end: Instant) -> Result<Option<Record>> {
        self.stream.set_read_timeout(Some(
            idle.min(end.saturating_duration_since(Instant::now()))
                .max(Duration::from_millis(1)),
        ))?;
        let mut header = [0; 8];
        match self.stream.read(&mut header[..1]) {
            Ok(0) => return Err(error("peer disconnected")),
            Ok(_) => self.quick_ack(),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        }
        self.read_frame_bytes(&mut header[1..], end)?;
        let mut size = u32::from_le_bytes(header[..4].try_into()?);
        let magic = u32::from_le_bytes(header[4..].try_into()?);
        if magic == MAGIC.swap_bytes() {
            size = size.swap_bytes();
        } else if magic != MAGIC {
            return Err(error("invalid peer frame magic"));
        }
        if size == 0 || size as usize > MAX_FRAME {
            return Err(error("invalid peer frame size"));
        }
        let mut frame = Vec::from(header);
        crate::io::safe_read(&mut frame, size as usize, |part| {
            self.read_frame_bytes(part, end)
        })?;
        let (record, size) = decode_frame_with_registry(&frame, &mut self.registry)?;
        self.last_size = size;
        Ok(Some(record))
    }
    fn read_frame_bytes(&mut self, mut bytes: &mut [u8], end: Instant) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "peer frame timed out",
                ));
            }
            self.stream.set_read_timeout(Some(remaining))?;
            match self.stream.read(bytes) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "peer disconnected during frame",
                    ))
                }
                Ok(count) => {
                    self.quick_ack();
                    bytes = &mut bytes[count..];
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "peer frame timed out",
                    ));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
pub fn respond(
    node: &Arc<Mutex<Node>>,
    connection: &mut Connection,
    request: &Record,
) -> Result<()> {
    let state = node
        .lock()
        .map_err(|_| error("node lock poisoned"))?
        .peer_state();
    respond_with(node, &state, connection, request)
}
fn respond_with(
    node: &Arc<Mutex<Node>>,
    state: &PeerState,
    connection: &mut Connection,
    request: &Record,
) -> Result<()> {
    let id = number(request, "id")?;
    let method = request.field("method")?.record()?;
    let call = || -> Result<Record> {
        // Router::on_request is deliberately narrower than the public RPC API.
        // Never expose VM execution or arbitrary ledger scans over peer sockets.
        if !matches!(
            method.name.as_str(),
            "mmx.Router.get_id"
                | "mmx.Router.get_info"
                | "mmx.Router.sign_msg"
                | "mmx.Router.get_peers"
                | "mmx.Node.get_height"
                | "mmx.Node.get_synced_height"
                | "mmx.Node.get_block"
                | "mmx.Node.get_block_at"
                | "mmx.Node.get_block_hash"
                | "mmx.Node.get_block_hash_ex"
                | "mmx.Node.get_header"
                | "mmx.Node.get_header_at"
                | "mmx.Node.get_tx_ids_at"
        ) {
            return Ok(Record::from_generated(&mmx_types::vnx::NoSuchMethod {
                what: "method is not available to peers".into(),
                method: method.name.clone(),
                ..Default::default()
            })?);
        }
        if let Some(reply) = state.call(method)? {
            return Ok(reply);
        }
        node.lock()
            .map_err(|_| error("node lock poisoned"))?
            .call(method)
    };
    let result = match call() {
        Ok(value) => value,
        Err(e) => record(
            "vnx.Exception",
            [("what".into(), Value::String(e.to_string().into_bytes()))],
        ),
    };
    connection.send(&record(
        "mmx.Return",
        [
            ("id".into(), Value::Uint(id.into())),
            ("result".into(), Value::Record(result)),
        ],
    ))
}
pub struct Client {
    connection: Connection,
    node: Arc<Mutex<Node>>,
    next_id: u32,
    inbox: Inbox,
    backpressure_retries: u64,
    peer_state: PeerState,
    relay: Relay,
    relay_peer: RelayPeer,
}
impl Client {
    pub fn connect(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        let (inbox, peer_state, relay) = {
            let local = node.lock().map_err(|_| error("node lock poisoned"))?;
            (local.inbox(), local.peer_state(), local.transaction_relay())
        };
        Ok(Self {
            connection: Connection::connect(address)?,
            node,
            next_id: 1,
            inbox,
            backpressure_retries: 0,
            peer_state,
            relay,
            relay_peer: Default::default(),
        })
    }
    pub fn call(&mut self, method: Record) -> Result<Value> {
        self.call_many(vec![method])?
            .pop()
            .ok_or_else(|| error("missing peer result"))
    }
    fn call_many(&mut self, methods: Vec<Record>) -> Result<Vec<Value>> {
        self.relay_peer
            .flush(&self.relay, |r| self.connection.send(r))?;
        let mut pending = std::collections::BTreeMap::new();
        let mut results = vec![None; methods.len()];
        let mut queued: std::collections::VecDeque<_> = (0..methods.len()).collect();
        let end = Instant::now() + Duration::from_secs(15);
        let mut send_after = Instant::now();
        let mut request_window = methods.len().clamp(1, MAX_RPC_REQUESTS);
        let mut draining = false;
        while (!pending.is_empty() || !queued.is_empty()) && Instant::now() < end {
            if Instant::now() >= send_after && (!draining || pending.is_empty()) {
                draining = false;
                let mut batch = Vec::new();
                while pending.len() < request_window {
                    let Some(index) = queued.pop_front() else {
                        break;
                    };
                    let method = &methods[index];
                    let id = self.next_id;
                    self.next_id = self.next_id.wrapping_add(1);
                    pending.insert(id, (index, format!("{}.return", method.name)));
                    batch.push(record(
                        "mmx.Request",
                        [
                            ("id".into(), Value::Uint(id.into())),
                            ("method".into(), Value::Record(method.clone())),
                        ],
                    ));
                }
                if !batch.is_empty() {
                    // Keep independent native frames, but avoid one tiny TCP
                    // write per RPC exhausting a WAN congestion window.
                    self.connection.send_batch(&batch)?;
                }
            }
            let mut idle = end
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(500));
            if !queued.is_empty() && Instant::now() < send_after {
                idle = idle.min(send_after.saturating_duration_since(Instant::now()));
            }
            let Some(message) = self.connection.receive_until(
                idle.max(Duration::from_millis(1)),
                end.min(Instant::now() + Duration::from_secs(10)),
            )?
            else {
                continue;
            };
            if message.name == "mmx.Return" {
                let Some((index, expected)) = pending.remove(&number(&message, "id")?) else {
                    continue;
                };
                let result = message.field("result")?.record()?;
                if result.name == "vnx.OverflowException" {
                    // Native Router rejects this request before dispatch when
                    // its socket send queue is blocked. Keep other replies and
                    // retry only this explicitly unexecuted request.
                    queued.push_back(index);
                    self.backpressure_retries += 1;
                    if !draining {
                        request_window = (request_window / 2).clamp(1, 16);
                    }
                    draining = true;
                    send_after = Instant::now() + Duration::from_millis(50);
                    continue;
                }
                if result.name != expected {
                    return Err(error(format!("unexpected peer result: {}", result.name)));
                }
                results[index] = Some(result.field("_ret_0")?.clone());
            } else {
                self.handle(message)?;
            }
        }
        results
            .into_iter()
            .map(|v| {
                v.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "peer request timed out")
                        .into()
                })
            })
            .collect()
    }
    pub fn backpressure_retries(&self) -> u64 {
        self.backpressure_retries
    }
    /// Authenticate the native node ID, reject aliases/self-connections, and
    /// qualify only synced full nodes on our genesis before using sync data.
    pub fn qualify(&mut self, genesis: Hash, own_id: Hash) -> Result<(Hash, u32)> {
        use rand::RngCore;
        let id: Hash = self
            .call(record("mmx.Router.get_id", []))?
            .bytes()?
            .try_into()?;
        let info = self.call(record("mmx.Router.get_info", []))?;
        let info = info.record()?;
        if id == own_id
            || crate::node::hash(info, "id")? != id
            || info.field("type")?.uint()?
                != u64::from(mmx_types::mmx::node_type_e::FULL_NODE.value)
        {
            return Err(error("peer is not a distinct full node"));
        }
        let mut challenge = [0; 32];
        rand::thread_rng().fill_bytes(&mut challenge);
        let signed = self.call(record(
            "mmx.Router.sign_msg",
            [("msg".into(), Value::Bytes(challenge.to_vec()))],
        ))?;
        let pair = signed.seq()?;
        if pair.len() != 2 {
            return Err(error("invalid peer identity proof"));
        }
        let key = pair[0].bytes()?.try_into()?;
        if mmx_crypto::public_key_address(key) != id
            || !mmx_crypto::verify(&challenge, key, pair[1].bytes()?.try_into()?)?
        {
            return Err(error("peer identity verification failed"));
        }
        if self.hash_at(0)? != Some(genesis) {
            return Err(error("peer genesis mismatch"));
        }
        let height = self
            .synced_height()?
            .ok_or_else(|| error("peer is not synced"))?;
        Ok((id, height))
    }
    pub fn synced_height(&mut self) -> Result<Option<u32>> {
        self.call(record("mmx.Node.get_synced_height", []))?
            .optional()?
            .map(|v| Ok(u32::try_from(v.uint()?)?))
            .transpose()
    }
    pub fn hash_pairs(&mut self, start: u32, count: u32) -> Result<Vec<Option<BlockHash>>> {
        if count > crate::sync::BATCH_BLOCKS {
            return Err(error("peer hash window exceeds sync batch limit"));
        }
        let methods = (0..count)
            .map(|i| {
                Ok(record(
                    "mmx.Node.get_block_hash_ex",
                    [(
                        "height".into(),
                        Value::Uint(
                            start
                                .checked_add(i)
                                .ok_or_else(|| error("peer height overflow"))?
                                .into(),
                        ),
                    )],
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.call_many(methods)?
            .into_iter()
            .map(optional_block_hash)
            .collect()
    }
    pub fn blocks_by_hash(&mut self, hashes: &[Hash]) -> Result<Vec<Option<Record>>> {
        if hashes.len() > MAX_BODY_REQUESTS {
            return Err(error("peer body request window exceeds limit"));
        }
        self.call_many(
            hashes
                .iter()
                .map(|hash| {
                    record(
                        "mmx.Node.get_block",
                        [("hash".into(), Value::Bytes(hash.to_vec()))],
                    )
                })
                .collect(),
        )?
        .into_iter()
        .map(|value| match value {
            Value::Null => Ok(None),
            Value::Record(b) => Ok(Some(b)),
            _ => Err(error("invalid peer block result")),
        })
        .collect()
    }
    pub fn known_peers(&mut self) -> Result<Vec<String>> {
        let value = self.call(record(
            "mmx.Router.get_peers",
            [("max_count".into(), Value::Uint(32))],
        ))?;
        value
            .seq()?
            .iter()
            .take(32)
            .map(|v| Ok(std::str::from_utf8(v.text()?)?.to_owned()))
            .collect()
    }
    /// Four outstanding native requests avoid a full round trip per block
    /// without filling a native peer's ten-MiB outbound queue.
    pub fn blocks_at(&mut self, start: u32, count: u32) -> Result<Vec<Option<Record>>> {
        if count > 4 {
            return Err(error("peer request window exceeds four blocks"));
        }
        let methods = (0..count)
            .map(|i| {
                let height = start
                    .checked_add(i)
                    .ok_or_else(|| error("peer height overflow"))?;
                Ok(record(
                    "mmx.Node.get_block_at",
                    [("height".into(), Value::Uint(height.into()))],
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.call_many(methods)?
            .into_iter()
            .map(|v| match v {
                Value::Null => Ok(None),
                Value::Record(b) => Ok(Some(b)),
                _ => Err(error("invalid peer block result")),
            })
            .collect()
    }
    fn handle(&mut self, message: Record) -> Result<()> {
        self.relay_peer.note(&message);
        if message.name == "mmx.Request" {
            respond_with(&self.node, &self.peer_state, &mut self.connection, &message)
        } else {
            // Invalid unsolicited broadcasts do not abort an outstanding sync
            // request; the same validation gates apply as to inbound peers.
            self.inbox.submit(message, self.connection.last_size)
        }
    }
    pub fn poll(&mut self) -> Result<()> {
        self.relay_peer
            .flush(&self.relay, |r| self.connection.send(r))?;
        if let Some(message) = self.connection.receive(Duration::from_millis(500))? {
            self.handle(message)?;
        }
        Ok(())
    }
    pub fn height(&mut self) -> Result<u32> {
        Ok(u32::try_from(
            self.call(record("mmx.Node.get_height", []))?.uint()?,
        )?)
    }
    pub fn hash_at(&mut self, height: u32) -> Result<Option<Hash>> {
        self.call(record(
            "mmx.Node.get_block_hash",
            [("height".into(), Value::Uint(height.into()))],
        ))?
        .optional()?
        .map(|v| {
            v.bytes()?
                .try_into()
                .map_err(|_| mmx_consensus::Error::Input("peer hash length".into()))
        })
        .transpose()
        .map_err(Into::into)
    }
    pub fn block_at(&mut self, height: u32) -> Result<Option<Record>> {
        let value = self.call(record(
            "mmx.Node.get_block_at",
            [("height".into(), Value::Uint(height.into()))],
        ))?;
        match value {
            Value::Null => Ok(None),
            Value::Record(b) => Ok(Some(b)),
            _ => Err(error("invalid peer block result")),
        }
    }
}
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    pub fn bind(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        let (inbox, peer_state, relay) = {
            let local = node.lock().map_err(|_| error("node lock poisoned"))?;
            (local.inbox(), local.peer_state(), local.transaction_relay())
        };
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let thread = thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            while !running.load(Ordering::Relaxed) {
                let mut i = 0;
                while i < workers.len() {
                    if workers[i].is_finished() {
                        let _ = workers.swap_remove(i).join();
                    } else {
                        i += 1;
                    }
                }
                match listener.accept() {
                    Ok((stream, _)) if workers.len() < 32 => {
                        let node = node.clone();
                        let inbox = inbox.clone();
                        let peer_state = peer_state.clone();
                        let relay = relay.clone();
                        let stop = running.clone();
                        workers.push(thread::spawn(move || {
                            let run = || -> Result<()> {
                                let mut connection = Connection::new(stream)?;
                                let mut relay_peer = RelayPeer::default();
                                while !stop.load(Ordering::Relaxed) {
                                    relay_peer.flush(&relay, |r| connection.send(r))?;
                                    if let Some(msg) =
                                        connection.receive(Duration::from_millis(200))?
                                    {
                                        relay_peer.note(&msg);
                                        if msg.name == "mmx.Request" {
                                            respond_with(
                                                &node,
                                                &peer_state,
                                                &mut connection,
                                                &msg,
                                            )?;
                                        } else {
                                            inbox.submit(msg, connection.last_size)?;
                                        }
                                    }
                                }
                                Ok(())
                            };
                            if let Err(e) = run() {
                                eprintln!("Peer connection closed: {e}");
                            }
                        }));
                    }
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            address,
            stop,
            thread: Some(thread),
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_zero_width_values_cannot_bypass_decoded_allocation_limits() {
        let mut raw = Record::from_generated(&mmx_types::mmx::contract::Data::default())
            .unwrap()
            .encode()
            .unwrap();
        assert!(raw.ends_with(&[1, 0, 0, 0]));
        raw.truncate(raw.len() - 4);
        // Dynamic List<Null>: ten million decoded values consume no element
        // bytes. Reject the count before allocating or iterating the elements.
        for word in [2u16, 12, 0] {
            raw.extend(word.to_le_bytes());
        }
        raw.extend(10_000_000u32.to_le_bytes());
        let frame = frame_bytes(&raw).unwrap();
        assert!(frame.len() < 1024);
        assert!(decode_frame(&frame)
            .unwrap_err()
            .to_string()
            .contains("allocation limit"));
    }

    #[test]
    fn schema_announcements_are_connection_local_and_failed_frames_do_not_advance_them() {
        fn read_frame(stream: &mut TcpStream) -> Vec<u8> {
            let mut header = [0; 8];
            stream.read_exact(&mut header).unwrap();
            let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
            assert!(size <= MAX_FRAME);
            let mut frame = vec![0; 8 + size];
            frame[..8].copy_from_slice(&header);
            stream.read_exact(&mut frame[8..]).unwrap();
            frame
        }
        fn reply(bytes: Vec<u8>) -> Record {
            record(
                "mmx.Return",
                [
                    ("id".into(), Value::Uint(7)),
                    (
                        "result".into(),
                        Value::Record(record(
                            "mmx.Router.get_peers.return",
                            [("_ret_0".into(), Value::Sequence(vec![Value::String(bytes)]))],
                        )),
                    ),
                ],
            )
        }
        let query = record(
            "mmx.Request",
            [
                ("id".into(), Value::Uint(7)),
                (
                    "method".into(),
                    Value::Record(record(
                        "mmx.Node.get_block_hash_ex",
                        [("height".into(), Value::Uint(1200))],
                    )),
                ),
            ],
        );
        let expected_query = query.clone();
        let expected_reply = reply(b"127.0.0.1:11337".to_vec());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let first = read_frame(&mut stream);
            let repeated = read_frame(&mut stream);
            let next_class = read_frame(&mut stream);
            assert_eq!(first, encode_frame(&expected_query).unwrap());
            assert!(repeated.len() < first.len());
            eprintln!(
                "hash request frame bytes: first={}, repeated={}",
                first.len(),
                repeated.len()
            );
            assert!(
                decode_frame(&repeated).is_err(),
                "a continuing frame needs its stream registry"
            );
            let mut registry = vnx::Registry::new();
            for frame in [&first, &repeated] {
                assert_eq!(
                    decode_frame_with_registry(frame, &mut registry).unwrap().0,
                    expected_query
                );
            }
            assert_eq!(
                decode_frame_with_registry(&next_class, &mut registry)
                    .unwrap()
                    .0,
                expected_reply
            );
            assert_eq!(
                stream.read(&mut [0]).unwrap(),
                0,
                "failed batches must not send a prefix"
            );
            drop(stream);
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            assert_eq!(
                read_frame(&mut stream),
                first,
                "a new connection must announce schemas again"
            );
        });
        let mut connection = Connection::connect(&address.to_string()).unwrap();
        connection.send(&query).unwrap();
        connection.send(&query).unwrap();
        let oversized = reply(vec![b'x'; MAX_FRAME]);
        let next = reply(b"127.0.0.1:11337".to_vec());
        assert!(connection.send_batch(&[next.clone(), oversized]).is_err());
        connection.send(&next).unwrap();
        drop(connection);
        Connection::connect(&address.to_string())
            .unwrap()
            .send(&query)
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn trickled_headers_and_payloads_share_a_deadline_and_cannot_be_reused_after_timeout() {
        for prefix in [1, 8] {
            let frame = encode_frame(&record("mmx.Node.get_height", [])).unwrap();
            assert!(frame.len() > 16);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (started, ready) = std::sync::mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(&frame[..prefix]).unwrap();
                started.send(()).unwrap();
                for byte in &frame[prefix..] {
                    thread::sleep(Duration::from_millis(20));
                    if stream.write_all(&[*byte]).is_err() {
                        break;
                    }
                }
            });
            let mut connection = Connection::connect(&address.to_string()).unwrap();
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
            let before = Instant::now();
            let failure = connection
                .receive_until(Duration::from_secs(1), before + Duration::from_millis(150))
                .unwrap_err();
            assert_eq!(
                failure.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::TimedOut
            );
            assert!(before.elapsed() < Duration::from_secs(2));
            assert!(connection.receive(Duration::from_millis(1)).is_err());
            drop(connection);
            server.join().unwrap();
        }
    }
}

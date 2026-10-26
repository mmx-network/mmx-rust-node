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
#[cfg(test)]
mod discovery_tests;
mod identity;
mod output;
use output::{Output, Worker};
use std::{
    io::Read,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAGIC: u32 = 0x4B277B06;
pub const MAX_FRAME: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_COMPRESSED_LIMIT: usize = 8 * 1024 * 1024;
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
    frame_bytes_with_limit(raw, MAX_FRAME)
}
fn frame_bytes_with_limit(raw: &[u8], compressed_limit: usize) -> Result<Vec<u8>> {
    frame_bytes_with_options(raw, compressed_limit, 3)
}
fn frame_bytes_with_options(raw: &[u8], compressed_limit: usize, level: i32) -> Result<Vec<u8>> {
    if raw.len() > MAX_FRAME {
        return Err(error("peer record exceeds frame limit"));
    }
    let data = zstd::stream::encode_all(raw, level)?;
    if data.len() > compressed_limit {
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
    output: Arc<Mutex<Output>>,
    writer: Option<Worker>,
    relay_receipts: Option<RelayPeer>,
    last_size: usize,
    received_at: Instant,
    expires_at: Option<Instant>,
    incoming: bool,
    compressed_limit: usize,
}
impl Connection {
    pub fn connect(address: &str) -> Result<Self> {
        Self::connect_with_policy(address, None)
    }
    fn connect_with_policy(
        address: &str,
        policy: Option<&crate::router::RouterConfig>,
    ) -> Result<Self> {
        if policy.is_some_and(|p| p.blocks(address)) {
            return Err(error("peer is blocked by Router.block_peers"));
        }
        let mut last = None;
        for addr in address.to_socket_addrs()? {
            if policy.is_some_and(|p| p.blocks_ip(addr.ip())) {
                continue;
            }
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
        socket2::SockRef::from(&stream).set_keepalive(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let output = Output::new(stream.try_clone()?, DEFAULT_COMPRESSED_LIMIT);
        Ok(Self {
            stream,
            registry: vnx::Registry::new(),
            output: Arc::new(Mutex::new(output)),
            writer: None,
            relay_receipts: None,
            last_size: 0,
            received_at: Instant::now(),
            expires_at: None,
            incoming: false,
            compressed_limit: DEFAULT_COMPRESSED_LIMIT,
        })
    }
    fn incoming(stream: TcpStream, policy: &crate::router::RouterConfig) -> Result<Self> {
        let mut connection = Self::new(stream)?;
        connection.incoming = true;
        policy.configure_inbound_socket(&socket2::SockRef::from(&connection.stream))?;
        connection.expires_at = policy
            .disconnect_interval
            .map(|period| Instant::now() + period);
        Ok(connection)
    }
    pub fn send(&mut self, value: &Record) -> Result<()> {
        self.send_batch(std::slice::from_ref(value))
    }
    fn send_batch(&mut self, values: &[Record]) -> Result<()> {
        // Gossip and replies share ordered schemas and bounded queued frames.
        // Production socket writes run without this encoder/queue mutex.
        self.output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .send_batch(values)
    }
    fn start_writer(&mut self, limit: usize, interval: Duration) -> Result<()> {
        if self.writer.is_some() {
            return Err(error("peer writer already started"));
        }
        self.writer = Some(Worker::start(
            self.output.clone(),
            self.stream.try_clone()?,
            limit,
            interval,
            self.expires_at,
        )?);
        Ok(())
    }
    fn start_relay(&mut self, relay: Relay, role: u32, synced: bool) -> Result<RelayPeer> {
        if self.relay_receipts.is_some() {
            return Err(error("peer relay already started"));
        }
        self.set_compressed_limit(relay.compressed_limit()?)?;
        if self.writer.is_none() {
            self.start_writer(
                crate::router::RouterConfig::default().max_write_queue,
                relay.send_interval()?,
            )?;
        }
        let sender = relay.attach()?;
        sender.set_profile(role, synced)?;
        let receiver = sender.receipt_view();
        sender.set_waker(
            self.writer
                .as_ref()
                .ok_or_else(|| error("no peer writer"))?
                .thread
                .thread()
                .clone(),
        )?;
        self.output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .attach(relay, sender)?;
        self.relay_receipts = Some(receiver.clone());
        Ok(receiver)
    }
    fn set_compressed_limit(&mut self, limit: usize) -> Result<()> {
        if limit == 0 || limit > MAX_FRAME {
            return Err(error("Router.max_msg_size must be between 1 and 67108864"));
        }
        self.output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .compressed_limit = limit;
        self.compressed_limit = limit;
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
        let result = (|| {
            loop {
                let Some(record) = self.receive_frame_until(idle, end)? else {
                    return Ok(None);
                };
                if record.name != "mmx.ReceiveNote" {
                    return Ok(Some(record));
                }
                // Native receipt control frames can precede an RPC response or
                // gossip. Share their suppression state with the independent
                // sender; neither the ledger nor socket output lock is needed.
                crate::node::hash(&record, "hash")?;
                if let Some(receipts) = &self.relay_receipts {
                    receipts.note(&record)?;
                }
                if Instant::now() >= end {
                    return Err(error("peer receipt stream timed out"));
                }
            }
        })();
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
        // Native MsgServer bounds compressed bytes from the header, before
        // receiving/allocating the body. Decoded bytes have a separate bound.
        if size == 0 || size as usize > self.compressed_limit {
            return Err(error("invalid peer frame size"));
        }
        let mut frame = Vec::from(header);
        crate::io::safe_read(&mut frame, size as usize, |part| {
            self.read_frame_bytes(part, end)
        })?;
        let (record, size) = decode_frame_with_registry(&frame, &mut self.registry)?;
        self.last_size = size;
        self.received_at = Instant::now();
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
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(worker) = self.writer.take() {
            worker.stop.store(true, Ordering::Release);
            worker.thread.thread().unpark();
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            let _ = worker.thread.join();
        }
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
    {
        let mut output = connection
            .output
            .lock()
            .map_err(|_| error("peer output poisoned"))?;
        if output.pressured() {
            // Native Router::on_request rejects before dispatch. The caller
            // can safely retry this ID's operation without losing successes.
            return output.reject(&record(
                "mmx.Return",
                [
                    ("id".into(), Value::Uint(id.into())),
                    (
                        "result".into(),
                        Value::Record(Record::from_generated(
                            &mmx_types::vnx::OverflowException::default(),
                        )?),
                    ),
                ],
            ));
        }
    }
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
                | "mmx.Node.get_genesis_hash"
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
    relay_peer: RelayPeer,
    relay: Relay,
    identity: Option<identity::Identity>,
    auth_gossip: Vec<(Record, usize)>,
    auth_bytes: usize,
    stopping: Option<Arc<AtomicBool>>,
    discovery_enabled: bool,
    discovery_next: Instant,
    discovery_pending: Option<(u32, Instant)>,
    _permit: crate::router::Permit,
}
impl Client {
    pub fn connect(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        Self::connect_stoppable(address, node, None)
    }
    pub(crate) fn connect_stoppable(
        address: &str,
        node: Arc<Mutex<Node>>,
        stopping: Option<Arc<AtomicBool>>,
    ) -> Result<Self> {
        let (inbox, peer_state, relay) = {
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            local.enable_background_transactions()?;
            (local.inbox(), local.peer_state(), local.transaction_relay())
        };
        let permit = peer_state.connections.outbound()?;
        let connection = Connection::connect_with_policy(address, Some(&peer_state.router))?;
        Self::from_connection(connection, node, inbox, peer_state, relay, stopping, permit)
    }
    fn from_connection(
        mut connection: Connection,
        node: Arc<Mutex<Node>>,
        inbox: Inbox,
        peer_state: PeerState,
        relay: Relay,
        stopping: Option<Arc<AtomicBool>>,
        permit: crate::router::Permit,
    ) -> Result<Self> {
        connection
            .output
            .lock()
            .map_err(|_| error("peer output poisoned"))?
            .compress_level = peer_state.router.compress_level;
        connection.set_compressed_limit(relay.compressed_limit()?)?;
        connection.start_writer(
            peer_state.router.max_write_queue,
            peer_state.router.send_interval,
        )?;
        let mut client = Self {
            connection,
            node,
            next_id: 1,
            inbox,
            backpressure_retries: 0,
            peer_state,
            relay_peer: Default::default(),
            relay,
            identity: None,
            auth_gossip: Vec::new(),
            auth_bytes: 0,
            stopping,
            discovery_enabled: false,
            discovery_next: Instant::now(),
            discovery_pending: None,
            _permit: permit,
        };
        client.authenticate()?;
        Ok(client)
    }
    pub fn call(&mut self, method: Record) -> Result<Value> {
        self.call_many(vec![method])?
            .pop()
            .ok_or_else(|| error("missing peer result"))
    }
    fn call_many(&mut self, methods: Vec<Record>) -> Result<Vec<Value>> {
        self.call_many_with_timeout(methods, self.peer_state.router.fetch_timeout)
    }
    fn receive_deadline(&self, mut end: Instant) -> Result<Instant> {
        let now = Instant::now();
        if let Some(lifetime) = self.connection.expires_at {
            if now >= lifetime {
                return Err(error("peer inbound connection lifetime exceeded"));
            }
            end = end.min(lifetime);
        }
        if let Some(timeout) = self.peer_state.router.connection_timeout {
            let idle = self.connection.received_at + timeout;
            if now >= idle {
                return Err(error("peer receive idle timeout"));
            }
            end = end.min(idle);
        }
        Ok(end)
    }
    fn call_many_with_timeout(
        &mut self,
        methods: Vec<Record>,
        timeout: Duration,
    ) -> Result<Vec<Value>> {
        let mut results = vec![None; methods.len()];
        self.call_many_stream(methods, timeout, &mut |index, value| {
            results[index] = Some(value);
            Ok(())
        })?;
        results
            .into_iter()
            .map(|value| value.ok_or_else(|| error("missing peer result")))
            .collect()
    }
    fn call_many_stream(
        &mut self,
        methods: Vec<Record>,
        timeout: Duration,
        emit: &mut dyn FnMut(usize, Value) -> Result<()>,
    ) -> Result<()> {
        let mut pending = std::collections::BTreeMap::new();
        let mut queued: std::collections::VecDeque<_> = (0..methods.len()).collect();
        let end = Instant::now() + timeout;
        let mut send_after = Instant::now();
        let mut request_window = methods.len().clamp(1, MAX_RPC_REQUESTS);
        let mut draining = false;
        while (!pending.is_empty() || !queued.is_empty()) && Instant::now() < end {
            let connection_end = self.receive_deadline(end)?;
            if self
                .stopping
                .as_ref()
                .is_some_and(|stop| stop.load(Ordering::Relaxed))
            {
                return Err(error("peer shutdown"));
            }
            self.discover_if_due()?;
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
            idle = idle.min(connection_end.saturating_duration_since(Instant::now()));
            if !queued.is_empty() && Instant::now() < send_after {
                idle = idle.min(send_after.saturating_duration_since(Instant::now()));
            }
            let Some(message) = self.connection.receive_until(
                idle.max(Duration::from_millis(1)),
                connection_end.min(Instant::now() + Duration::from_secs(10)),
            )?
            else {
                continue;
            };
            if message.name == "mmx.Return" {
                if self.discovery_return(&message)? {
                    continue;
                }
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
                emit(index, result.field("_ret_0")?.clone())?;
            } else {
                self.handle(message)?;
            }
        }
        if !pending.is_empty() || !queued.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "peer request timed out",
            )
            .into());
        }
        Ok(())
    }
    pub fn backpressure_retries(&self) -> u64 {
        self.backpressure_retries
    }
    /// Authenticate the native node ID, reject aliases/self-connections, and
    /// qualify only synced full nodes on our genesis before using sync data.
    pub fn qualify(&mut self, genesis: Hash, own_id: Hash) -> Result<(Hash, u32)> {
        if self.connection.incoming {
            return Err(error("historical sync requires an outbound connection"));
        }
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| error("peer is not authenticated"))?;
        let id = identity.id;
        if id == own_id
            || genesis != self.peer_state.genesis()
            || identity.role != mmx_types::mmx::node_type_e::FULL_NODE.value
        {
            return Err(error("peer is not a distinct full node on our genesis"));
        }
        let height = self
            .synced_height()?
            .ok_or_else(|| error("peer is not synced"))?;
        self.peer_state
            .book
            .confirm(self.connection.stream.peer_addr()?)?;
        Ok((id, height))
    }
    pub(crate) fn enable_discovery(&mut self, immediate: bool) {
        if let Some(interval) = self.peer_state.router.discover_interval {
            self.discovery_enabled = self.identity.as_ref().is_some_and(|identity| {
                identity.role == mmx_types::mmx::node_type_e::FULL_NODE.value
            });
            self.discovery_next =
                Instant::now() + if immediate { Duration::ZERO } else { interval };
        }
    }
    fn discover_if_due(&mut self) -> Result<()> {
        let Some(interval) = self.peer_state.router.discover_interval else {
            return Ok(());
        };
        if !self.discovery_enabled || self.peer_state.router.max_peer_set == 0 {
            return Ok(());
        }
        let now = Instant::now();
        if self
            .discovery_pending
            .is_some_and(|(_, deadline)| now >= deadline)
        {
            self.discovery_pending = None;
        }
        if now < self.discovery_next || self.discovery_pending.is_some() {
            return Ok(());
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.connection.send(&record(
            "mmx.Request",
            [
                ("id".into(), Value::Uint(id.into())),
                (
                    "method".into(),
                    Value::Record(record(
                        "mmx.Router.get_peers",
                        [(
                            "max_count".into(),
                            Value::Uint(
                                self.peer_state
                                    .router
                                    .max_peer_set
                                    .min(crate::peer_book::DISCOVERY_COUNT)
                                    as u64,
                            ),
                        )],
                    )),
                ),
            ],
        ))?;
        self.discovery_next = now + interval;
        self.discovery_pending =
            Some((id, now + interval.min(self.peer_state.router.fetch_timeout)));
        Ok(())
    }
    fn discovery_return(&mut self, message: &Record) -> Result<bool> {
        let id = number(message, "id")?;
        if !self
            .discovery_pending
            .is_some_and(|(pending, _)| id == pending)
        {
            return Ok(false);
        }
        let (_, deadline) = self.discovery_pending.take().unwrap();
        if Instant::now() >= deadline {
            return Ok(true);
        }
        // Discovery is optional: malformed/negative replies cannot poison
        // addresses or fail an unrelated hash/body operation.
        if let Ok(result) = message.field("result").and_then(Value::record) {
            if result.name == "mmx.Router.get_peers.return" {
                if let Ok(values) = result.field("_ret_0").and_then(Value::seq) {
                    for value in values.iter().take(
                        self.peer_state
                            .router
                            .max_peer_set
                            .min(crate::peer_book::DISCOVERY_COUNT),
                    ) {
                        if let Ok(address) = value.text().and_then(|bytes| {
                            std::str::from_utf8(bytes).map_err(|_| {
                                mmx_consensus::Error::Input("invalid peer address".into())
                            })
                        }) {
                            self.peer_state.book.learn(address)?;
                        }
                    }
                }
            }
        }
        Ok(true)
    }
    pub fn synced_height(&mut self) -> Result<Option<u32>> {
        let height = self
            .call(record("mmx.Node.get_synced_height", []))?
            .optional()?
            .map(|v| -> Result<u32> { Ok(u32::try_from(v.uint()?)?) })
            .transpose()?;
        if let Some(identity) = &self.identity {
            self.relay_peer
                .set_profile(identity.role, height.is_some())?;
        }
        Ok(height)
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
        let mut values = vec![None; hashes.len()];
        self.blocks_by_hash_each(hashes, &mut |index, value| {
            values[index] = value;
            Ok(())
        })?;
        Ok(values)
    }
    pub(crate) fn blocks_by_hash_each(
        &mut self,
        hashes: &[Hash],
        emit: &mut dyn FnMut(usize, Option<Record>) -> Result<()>,
    ) -> Result<()> {
        if hashes.len() > MAX_BODY_REQUESTS {
            return Err(error("peer body request window exceeds limit"));
        }
        // The coordinator borrows this connection for a bounded body group.
        // Return ownership at the native body lease, rather than holding up
        // all providers for the full metadata/fetch inactivity timeout.
        self.call_many_stream(
            hashes
                .iter()
                .map(|hash| {
                    record(
                        "mmx.Node.get_block",
                        [("hash".into(), Value::Bytes(hash.to_vec()))],
                    )
                })
                .collect(),
            self.peer_state.router.body_timeout,
            &mut |index, value| {
                let value = match value {
                    Value::Null => None,
                    Value::Record(block) => Some(block),
                    _ => return Err(error("invalid peer block result")),
                };
                emit(index, value)
            },
        )
    }
    pub fn known_peers(&mut self) -> Result<Vec<String>> {
        let maximum = self.peer_state.router.max_peer_set.min(32);
        if maximum == 0 {
            return Ok(vec![]);
        }
        let value = self.call(record(
            "mmx.Router.get_peers",
            [("max_count".into(), Value::Uint(maximum as u64))],
        ))?;
        value
            .seq()?
            .iter()
            .take(maximum)
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
        self.relay_peer.note(&message)?;
        if message.name == "mmx.Request" {
            respond_with(&self.node, &self.peer_state, &mut self.connection, &message)
        } else if message.name == "mmx.Return" {
            self.discovery_return(&message)?;
            Ok(())
        } else if self.identity.is_none() {
            // Native sends its VDF bootstrap before answering authentication.
            // Retain a bounded window without admitting or forwarding it yet.
            let size = self.connection.last_size.max(message.memory_size());
            if self.auth_gossip.len() >= 128 || size > 32 * 1024 * 1024 - self.auth_bytes {
                return Err(error("peer pre-authentication gossip exceeds limit"));
            }
            self.auth_bytes += size;
            self.auth_gossip.push((message, size));
            Ok(())
        } else {
            self.relay
                .admit_gossip(message, self.connection.last_size, &self.inbox)
        }
    }
    pub fn poll(&mut self) -> Result<()> {
        self.discover_if_due()?;
        let now = Instant::now();
        let end = self.receive_deadline(now + Duration::from_secs(10))?;
        if let Some(message) = self.connection.receive_until(
            Duration::from_millis(500).min(end.saturating_duration_since(now)),
            end,
        )? {
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
            let mut local = node.lock().map_err(|_| error("node lock poisoned"))?;
            local.enable_background_transactions()?;
            (local.inbox(), local.peer_state(), local.transaction_relay())
        };
        let listener = peer_state.router.bind_listener(address)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let policy = peer_state.router.clone();
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
                    Ok((stream, address)) => {
                        if policy.blocks_ip(address.ip()) {
                            continue;
                        }
                        let Some(permit) = peer_state.connections.inbound(policy.max_connections)
                        else {
                            continue;
                        };
                        let node = node.clone();
                        let inbox = inbox.clone();
                        let peer_state = peer_state.clone();
                        let relay = relay.clone();
                        let stop = running.clone();
                        workers.push(thread::spawn(move || {
                            let run = || -> Result<()> {
                                let mut client = Client::from_connection(
                                    Connection::incoming(stream, &peer_state.router)?,
                                    node,
                                    inbox,
                                    peer_state,
                                    relay,
                                    Some(stop.clone()),
                                    permit,
                                )?;
                                client.enable_discovery(false);
                                let mut query = Instant::now();
                                while !stop.load(Ordering::Relaxed) {
                                    client.poll()?;
                                    if query.elapsed() >= client.peer_state.router.query_interval
                                        && client.identity.as_ref().is_some_and(|identity| {
                                            identity.role
                                                == mmx_types::mmx::node_type_e::FULL_NODE.value
                                        })
                                    {
                                        client.synced_height()?;
                                        query = Instant::now();
                                    }
                                }
                                Ok(())
                            };
                            if let Err(e) = run() {
                                eprintln!("Peer connection closed: {e}");
                            }
                        }));
                    }
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
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn dropping_a_peer_wakes_its_sender_even_with_a_one_minute_idle_interval() {
        for idle_first in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut connection =
                Connection::connect(&listener.local_addr().unwrap().to_string()).unwrap();
            let _remote = listener.accept().unwrap().0;
            let relay = Relay::default();
            relay
                .configure_policy(&crate::router::RouterConfig {
                    send_interval: Duration::from_secs(60),
                    ..Default::default()
                })
                .unwrap();
            connection
                .start_relay(
                    relay.clone(),
                    mmx_types::mmx::node_type_e::FULL_NODE.value,
                    true,
                )
                .unwrap();
            let stop = connection.writer.as_ref().unwrap().stop.clone();
            let wake = connection.writer.as_ref().unwrap().thread.thread().clone();
            if idle_first {
                thread::sleep(Duration::from_millis(20));
            }
            let (finished, ready) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                drop(connection);
                let _ = finished.send(());
            });
            let result = ready.recv_timeout(Duration::from_secs(2));
            // Always release the test worker, including on regression.
            if result.is_err() {
                stop.store(true, Ordering::Release);
                wake.unpark();
            }
            worker.join().unwrap();
            assert!(
                result.is_ok(),
                "peer shutdown waited for the sender's idle timer"
            );
        }
    }

    #[test]
    fn native_listener_socket_settings_do_not_override_outgoing_endpoint_defaults() {
        let policy = crate::router::RouterConfig {
            tcp_no_delay: false,
            tcp_keepalive: false,
            send_buffer_size: Some(65_536),
            receive_buffer_size: Some(32_768),
            listen_queue_size: 7,
            disconnect_interval: Some(Duration::from_secs(1)),
            ..Default::default()
        };
        let listener = policy.bind_listener("127.0.0.1:0").unwrap();
        let socket = socket2::SockRef::from(&listener);
        assert!(!socket.keepalive().unwrap());
        assert!(!socket.tcp_nodelay().unwrap());
        assert!(socket.send_buffer_size().unwrap() >= 65_536);
        assert!(socket.recv_buffer_size().unwrap() >= 32_768);
        let address = listener.local_addr().unwrap();
        let outgoing =
            Connection::connect_with_policy(&address.to_string(), Some(&policy)).unwrap();
        let incoming = Connection::incoming(listener.accept().unwrap().0, &policy).unwrap();
        let socket = socket2::SockRef::from(&incoming.stream);
        assert!(!socket.keepalive().unwrap());
        assert!(!socket.tcp_nodelay().unwrap());
        assert!(socket.send_buffer_size().unwrap() >= 65_536);
        assert!(socket.recv_buffer_size().unwrap() >= 32_768);
        assert!(incoming.expires_at.is_some());
        let socket = socket2::SockRef::from(&outgoing.stream);
        assert!(socket.keepalive().unwrap());
        assert!(socket.tcp_nodelay().unwrap());
        assert!(outgoing.expires_at.is_none());
    }

    #[test]
    fn compressed_header_limit_rejects_before_waiting_for_or_allocating_a_body() {
        for (limit, swapped) in [
            (DEFAULT_COMPRESSED_LIMIT, false),
            (DEFAULT_COMPRESSED_LIMIT, true),
            (1024, false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (release, held) = std::sync::mpsc::channel();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let size = (limit + 1) as u32;
                let (size, magic) = if swapped {
                    (size.swap_bytes(), MAGIC.swap_bytes())
                } else {
                    (size, MAGIC)
                };
                stream.write_all(&size.to_le_bytes()).unwrap();
                stream.write_all(&magic.to_le_bytes()).unwrap();
                // Keep the socket open without transmitting a body. Rejection
                // must come from the header rather than EOF or a read timeout.
                held.recv_timeout(Duration::from_secs(3)).unwrap();
            });
            let mut connection = Connection::connect(&address.to_string()).unwrap();
            connection.set_compressed_limit(limit).unwrap();
            let err = connection.receive(Duration::from_secs(1)).unwrap_err();
            release.send(()).unwrap();
            server.join().unwrap();
            assert_eq!(err.to_string(), "invalid peer frame size");
        }
    }

    #[test]
    fn native_compressed_limit_does_not_restrict_the_raw_record_to_eight_megabytes() {
        let expected = record(
            "mmx.Router.get_peers.return",
            [(
                "_ret_0".into(),
                Value::Sequence(vec![Value::String(vec![
                    b'x';
                    DEFAULT_COMPRESSED_LIMIT + 1
                ])]),
            )],
        );
        let raw = expected.encode().unwrap();
        assert!(raw.len() > DEFAULT_COMPRESSED_LIMIT && raw.len() < MAX_FRAME);
        let frame = frame_bytes_with_limit(&raw, DEFAULT_COMPRESSED_LIMIT).unwrap();
        assert!(frame.len() < 4096);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            listener.accept().unwrap().0.write_all(&frame).unwrap();
        });
        let mut connection = Connection::connect(&address.to_string()).unwrap();
        assert_eq!(
            connection.receive(Duration::from_secs(3)).unwrap().unwrap(),
            expected
        );
        server.join().unwrap();
    }

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
        connection.set_compressed_limit(1).unwrap();
        assert!(connection.send(&query).is_err());
        connection
            .set_compressed_limit(DEFAULT_COMPRESSED_LIMIT)
            .unwrap();
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
    #[test]
    fn native_receipts_are_consumed_during_reads_and_update_the_shared_sender() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut connection = Connection::new(listener.accept().unwrap().0).unwrap();
            let mut sender = RelayPeer::default();
            connection.relay_receipts = Some(sender.clone());
            let query = connection.receive(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!(query.name, "mmx.Node.get_height");
            let relay = Relay::default();
            let mut tx = Record::from_generated(&mmx_types::mmx::Transaction::default()).unwrap();
            tx.fields
                .insert("content_hash".into(), Value::Bytes(vec![42; 32]));
            relay.publish([1; 32], Arc::new(tx), 100).unwrap();
            sender.flush(&relay, |r| connection.send(r)).unwrap();
            connection
                .send(&record(
                    "mmx.Node.get_height.return",
                    [("_ret_0".into(), Value::Uint(17))],
                ))
                .unwrap();
        });
        let mut client = Connection::connect(&address.to_string()).unwrap();
        client
            .send_batch(&[
                Record::from_generated(&mmx_types::mmx::ReceiveNote {
                    hash: [42; 32].into(),
                    time: 1,
                })
                .unwrap(),
                record("mmx.Node.get_height", []),
            ])
            .unwrap();
        let reply = client.receive(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(reply.name, "mmx.Node.get_height.return");
        assert_eq!(reply.field("_ret_0").unwrap().uint().unwrap(), 17);
        // The acknowledged transaction payload must never appear on this wire.
        assert!(client.receive(Duration::from_secs(1)).is_err());
        server.join().unwrap();
    }
    #[test]
    fn receipt_only_activity_refreshes_receive_idle_time_without_resetting_frame_deadlines() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, finished) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let mut connection = Connection::new(listener.accept().unwrap().0).unwrap();
            connection
                .send(
                    &Record::from_generated(&mmx_types::mmx::ReceiveNote {
                        hash: [5; 32].into(),
                        time: 0,
                    })
                    .unwrap(),
                )
                .unwrap();
            let _ = finished.recv();
        });
        let mut connection = Connection::connect(&address.to_string()).unwrap();
        let before = connection.received_at;
        assert!(connection
            .receive(Duration::from_millis(100))
            .unwrap()
            .is_none());
        assert!(connection.received_at > before);
        stop.send(()).unwrap();
        server.join().unwrap();
    }
}

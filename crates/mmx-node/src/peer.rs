//! Native vnx.addons.MsgServer framing: u32 compressed size, u32 magic, Zstd
//! VNX record. Each decompressed frame has its own type-code stream. Consensus
//! records are decoded losslessly, including byte strings and Variant payloads.
use crate::{
    error,
    node::{number, record},
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

pub fn encode_frame(value: &Record) -> Result<Vec<u8>> {
    let raw = value.encode()?;
    if raw.len() > MAX_FRAME {
        return Err(error("peer record exceeds frame limit"));
    }
    let data = zstd::stream::encode_all(raw.as_slice(), 3)?;
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
    decode_frame_with_registry(frame, &mut vnx::Registry::new())
}
fn decode_frame_with_registry(frame: &[u8], registry: &mut vnx::Registry) -> Result<Record> {
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
    Ok(Record::decode_with_registry(&raw, registry)?)
}
pub struct Connection {
    stream: TcpStream,
    registry: vnx::Registry,
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
        })
    }
    pub fn send(&mut self, value: &Record) -> Result<()> {
        self.stream.write_all(&encode_frame(value)?)?;
        Ok(())
    }
    /// None means idle before the first frame byte. Partial-frame timeouts close
    /// the connection, so a later read can never mistake payload for a header.
    pub fn receive(&mut self, idle: Duration) -> Result<Option<Record>> {
        self.stream.set_read_timeout(Some(idle))?;
        let mut header = [0; 8];
        match self.stream.read(&mut header[..1]) {
            Ok(0) => return Err(error("peer disconnected")),
            Ok(_) => (),
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
        self.stream
            .set_read_timeout(Some(Duration::from_secs(10)))?;
        self.stream.read_exact(&mut header[1..])?;
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
        frame.resize(size as usize + 8, 0);
        self.stream.read_exact(&mut frame[8..])?;
        decode_frame_with_registry(&frame, &mut self.registry).map(Some)
    }
}
pub fn respond(
    node: &Arc<Mutex<Node>>,
    connection: &mut Connection,
    request: &Record,
) -> Result<()> {
    let id = number(request, "id")?;
    let method = request.field("method")?.record()?;
    let result = match node
        .lock()
        .map_err(|_| error("node lock poisoned"))?
        .call(method)
    {
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
}
impl Client {
    pub fn connect(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        Ok(Self {
            connection: Connection::connect(address)?,
            node,
            next_id: 1,
        })
    }
    pub fn call(&mut self, method: Record) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let expected = format!("{}.return", method.name);
        self.connection.send(&record(
            "mmx.Request",
            [
                ("id".into(), Value::Uint(id.into())),
                ("method".into(), Value::Record(method)),
            ],
        ))?;
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            let Some(message) = self.connection.receive(Duration::from_millis(500))? else {
                continue;
            };
            if message.name == "mmx.Return" {
                if number(&message, "id")? != id {
                    continue;
                }
                let result = message.field("result")?.record()?;
                if result.name != expected {
                    return Err(error(format!("unexpected peer result: {}", result.name)));
                }
                return Ok(result.field("_ret_0")?.clone());
            }
            self.handle(&message)?;
        }
        Err(error("peer request timed out"))
    }
    fn handle(&mut self, message: &Record) -> Result<()> {
        if message.name == "mmx.Request" {
            respond(&self.node, &mut self.connection, message)
        } else {
            // Invalid unsolicited broadcasts do not abort an outstanding sync
            // request; the same validation gates apply as to inbound peers.
            if let Err(e) = self
                .node
                .lock()
                .map_err(|_| error("node lock poisoned"))?
                .receive(message)
            {
                eprintln!("Rejected {}: {e}", message.name);
            }
            Ok(())
        }
    }
    pub fn poll(&mut self) -> Result<()> {
        if let Some(message) = self.connection.receive(Duration::from_millis(500))? {
            self.handle(&message)?;
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
                        let stop = running.clone();
                        workers.push(thread::spawn(move || {
                            let run = || -> Result<()> {
                                let mut connection = Connection::new(stream)?;
                                while !stop.load(Ordering::Relaxed) {
                                    if let Some(msg) =
                                        connection.receive(Duration::from_millis(200))?
                                    {
                                        if msg.name == "mmx.Request" {
                                            respond(&node, &mut connection, &msg)?;
                                        } else {
                                            let _ = node
                                                .lock()
                                                .map_err(|_| error("node lock poisoned"))?
                                                .receive(&msg);
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

//! Bounded HTTP/1 JSON-RPC transport. Each connection handles one request and
//! closes; absolute socket deadlines cover headers, body, chunks and responses.
use crate::{error, rpc, Node, Result};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vnx::serde_json::json;
const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 8;
struct Deadline {
    stream: TcpStream,
    end: Instant,
}
impl Deadline {
    fn remaining(&self) -> io::Result<Duration> {
        let time = self.end.saturating_duration_since(Instant::now());
        if time.is_zero() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "HTTP deadline"))
        } else {
            Ok(time)
        }
    }
}
impl Read for Deadline {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}
impl Write for Deadline {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
type HttpResult<T> = std::result::Result<T, (u16, &'static str)>;
fn line(input: &mut impl BufRead, max: usize) -> HttpResult<Vec<u8>> {
    let mut line = Vec::new();
    (&mut *input)
        .take((max + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|_| (408, "HTTP read deadline or disconnect"))?;
    if line.len() > max || !line.ends_with(b"\r\n") {
        return Err((400, "invalid or oversized HTTP line"));
    }
    Ok(line)
}
fn headers(input: &mut impl BufRead) -> HttpResult<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = line(input, MAX_HEADERS - out.len())?;
        let done = line == b"\r\n";
        out.extend(line);
        if done {
            return Ok(out);
        }
    }
}
fn request(input: &mut BufReader<Deadline>) -> HttpResult<Vec<u8>> {
    let raw = headers(input)?;
    let mut fields = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut fields);
    if !request
        .parse(&raw)
        .map_err(|_| (400, "invalid HTTP headers"))?
        .is_complete()
    {
        return Err((400, "incomplete headers"));
    }
    if request.method != Some("POST") {
        return Err((405, "POST JSON-RPC requests to /rpc"));
    }
    if !matches!(request.path, Some("/rpc" | "/")) {
        return Err((404, "not found"));
    }
    let mut length = None;
    let mut chunked = false;
    let mut expect = false;
    for field in request.headers.iter() {
        let value = std::str::from_utf8(field.value)
            .map_err(|_| (400, "invalid header value"))?
            .trim();
        if field.name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err((400, "invalid content length"));
            }
            let size = value
                .parse::<usize>()
                .map_err(|_| (413, "body too large"))?;
            if size > MAX_BODY {
                return Err((413, "body exceeds 1 MiB"));
            }
            length = Some(size);
        } else if field.name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err((400, "unsupported transfer encoding"));
            }
            chunked = true;
        } else if field.name.eq_ignore_ascii_case("expect") {
            if !value.eq_ignore_ascii_case("100-continue") {
                return Err((417, "unsupported expectation"));
            }
            expect = true;
        }
    }
    if chunked && length.is_some() {
        return Err((400, "ambiguous HTTP framing"));
    }
    if expect {
        input
            .get_mut()
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(|_| (408, "HTTP write deadline"))?;
    }
    let mut body = Vec::new();
    if chunked {
        for _ in 0..4096 {
            let raw = line(input, 1024)?;
            let size = raw[..raw.len() - 2]
                .split(|b| *b == b';')
                .next()
                .unwrap_or_default();
            if size.is_empty() || !size.iter().all(|b| b.is_ascii_hexdigit()) {
                return Err((400, "invalid chunk size"));
            }
            let size = usize::from_str_radix(
                std::str::from_utf8(size).map_err(|_| (400, "invalid chunk"))?,
                16,
            )
            .map_err(|_| (413, "chunk too large"))?;
            if size == 0 {
                let raw = headers(input)?;
                let mut fields = [httparse::EMPTY_HEADER; 64];
                if !httparse::parse_headers(&raw, &mut fields)
                    .map_err(|_| (400, "invalid trailers"))?
                    .is_complete()
                {
                    return Err((400, "incomplete trailers"));
                }
                return Ok(body);
            }
            if size > MAX_BODY - body.len() {
                return Err((413, "body exceeds 1 MiB"));
            }
            crate::io::safe_read(&mut body, size, |part| input.read_exact(part))
                .map_err(|_| (408, "HTTP body deadline or disconnect"))?;
            let mut end = [0; 2];
            input
                .read_exact(&mut end)
                .map_err(|_| (408, "HTTP body deadline or disconnect"))?;
            if end != *b"\r\n" {
                return Err((400, "invalid chunk delimiter"));
            }
        }
        return Err((400, "too many HTTP chunks"));
    }
    crate::io::safe_read(&mut body, length.unwrap_or(0), |part| {
        input.read_exact(part)
    })
    .map_err(|_| (408, "HTTP body deadline or disconnect"))?;
    Ok(body)
}
fn serve(stream: TcpStream, node: &Arc<Mutex<Node>>, timeout: Duration) -> Result<()> {
    let end = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| error("HTTP timeout overflow"))?;
    let mut input = BufReader::new(Deadline { stream, end });
    let (status, body) = match request(&mut input) {
        Ok(bytes) => match rpc::json_request(node, &bytes) {
            Some(v) => (200, v.to_string()),
            None => (204, String::new()),
        },
        Err((status, message)) => (
            status,
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":message}})
                .to_string(),
        ),
    };
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        417 => "Expectation Failed",
        _ => "Error",
    };
    let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
    input.get_mut().write_all(response.as_bytes())?;
    Ok(())
}
type Streams = Arc<Mutex<BTreeMap<u64, TcpStream>>>;
struct Lease {
    id: u64,
    streams: Streams,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut streams) = self.streams.lock() {
            streams.remove(&self.id);
        }
    }
}
pub struct HttpServer {
    address: String,
    stop: Arc<AtomicBool>,
    streams: Streams,
    thread: Option<JoinHandle<()>>,
}
impl HttpServer {
    pub fn bind(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        Self::bind_with_timeout(address, node, Duration::from_secs(5))
    }
    /// Absolute connection lifetime for socket I/O, including slow chunked bodies.
    pub fn bind_with_timeout(
        address: &str,
        node: Arc<Mutex<Node>>,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(error("HTTP timeout must be positive"));
        }
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?.to_string();
        listener.set_nonblocking(true)?;
        let streams: Streams = Arc::new(Mutex::new(BTreeMap::new()));
        let active = streams.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let thread = thread::Builder::new()
            .name("mmx-http".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                let mut id = 0_u64;
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
                        Ok((stream, _)) if workers.len() < MAX_CONNECTIONS => {
                            let Ok(copy) = stream.try_clone() else {
                                continue;
                            };
                            id = id.wrapping_add(1);
                            let lease = Lease {
                                id,
                                streams: active.clone(),
                            };
                            if let Ok(mut streams) = active.lock() {
                                streams.insert(id, copy);
                            } else {
                                break;
                            }
                            let node = node.clone();
                            workers.push(thread::spawn(move || {
                                let _lease = lease;
                                let _ = serve(stream, &node, timeout);
                            }));
                        }
                        Ok(_) => (),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
                if let Ok(streams) = active.lock() {
                    for stream in streams.values() {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })?;
        Ok(Self {
            address,
            stop,
            streams,
            thread: Some(thread),
        })
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(streams) = self.streams.lock() {
            for stream in streams.values() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

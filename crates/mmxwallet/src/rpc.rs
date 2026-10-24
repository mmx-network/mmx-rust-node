use crate::error::{invalid, rpc_invalid, Error, Result};
use mmx_wallet::{Address, ChainParams};
use serde_json::Value;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
const MAX_RESPONSE: u64 = 16 * 1024 * 1024;
/// Public chain server used when none is configured.
pub const DEFAULT_URL: &str = "rpc.mmx.network";
pub struct Rpc {
    /// Keeps connections alive between requests (GET only).
    agent: ureq::Agent,
    /// A new connection for every POST, so a transfer is never written to a
    /// kept-alive connection the server has already closed.
    post_agent: ureq::Agent,
    /// The server answered with HTTP/1.0, which closes every connection:
    /// reads use new connections too.
    no_keep_alive: AtomicBool,
    base: String,
}
/// A kept-alive connection the server closed before answering. Safe to
/// retry for reads on another connection.
fn stale_connection(e: &ureq::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(e, ureq::Error::Io(io) if matches!(io.kind(), UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe))
}
fn agent(pooled: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false);
    let config = if pooled {
        config
    } else {
        config
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
    };
    config.build().new_agent()
}
fn transport(e: ureq::Error) -> Error {
    let code = match e {
        ureq::Error::Timeout(_) => "rpc_timeout",
        ureq::Error::BodyExceedsLimit(_) => "rpc_response_invalid",
        _ => "rpc_transport_error",
    };
    Error::new(code, "RPC request failed; a submitted transaction may still have been accepted; check its ID before retrying")
}
impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            agent: agent(true),
            post_agent: agent(false),
            no_keep_alive: AtomicBool::new(false),
            base: Self::normalize(url)?,
        })
    }
    /// The server URL as used: HTTP(S) with a host, `https://` added when no
    /// scheme is given, trailing slashes removed.
    pub fn normalize(url: &str) -> Result<String> {
        let mut base = url.trim().trim_end_matches('/').to_owned();
        if !base.contains("://") {
            base = format!("https://{base}");
        }
        let uri: ureq::http::Uri = base.parse().map_err(|_| invalid("invalid RPC URL"))?;
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.host().is_none()
            || base.chars().any(char::is_control)
        {
            return Err(invalid("RPC URL must use HTTP or HTTPS"));
        }
        Ok(base)
    }
    /// Normalized server URL (scheme added, trailing slashes removed).
    pub fn base(&self) -> &str {
        &self.base
    }
    fn request(&self, path: &str, body: Option<&[u8]>) -> Result<Vec<u8>> {
        self.request_limited(path, body, MAX_RESPONSE)
    }
    fn request_limited(&self, path: &str, body: Option<&[u8]>, limit: u64) -> Result<Vec<u8>> {
        let url = format!("{}{path}", self.base);
        let mut response = match body {
            Some(b) => self
                .post_agent
                .post(&url)
                .header("Content-Type", "application/json")
                .send(b),
            None => {
                let agent = if self.no_keep_alive.load(Ordering::Relaxed) {
                    &self.post_agent
                } else {
                    &self.agent
                };
                let mut attempt = 0;
                loop {
                    match agent.get(&url).call() {
                        Err(e) if stale_connection(&e) && attempt < 3 => attempt += 1,
                        result => break result,
                    }
                }
            }
        }
        .map_err(transport)?;
        if response.version() == ureq::http::Version::HTTP_10 {
            self.no_keep_alive.store(true, Ordering::Relaxed);
        }
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(transport)?;
        if !(200..300).contains(&status) {
            // Server-controlled text: printable characters only.
            let detail: String = String::from_utf8_lossy(&bytes)
                .chars()
                .take(500)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            return Err(Error::new(
                "rpc_http_error",
                format!("RPC returned HTTP {status}: {detail}"),
            ));
        }
        Ok(bytes)
    }
    pub fn get(&self, path: &str) -> Result<Value> {
        self.get_limited(path, MAX_RESPONSE)
    }
    /// GET with a response body limit in bytes; an empty body is `null`.
    pub fn get_limited(&self, path: &str, limit: u64) -> Result<Value> {
        let bytes = self.request_limited(path, None, limit)?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| rpc_invalid("invalid JSON response from RPC"))
    }
    pub fn post(&self, path: &str, bytes: &[u8]) -> Result<()> {
        self.request(path, Some(bytes))?;
        Ok(())
    }
    pub fn validate(&self, bytes: &[u8], max_fee: u32) -> Result<u128> {
        let v: Value = serde_json::from_slice(&self.request("/transaction/validate", Some(bytes))?)
            .map_err(|_| rpc_invalid("invalid JSON response from RPC"))?;
        if boolean(&v, "did_fail")? {
            return Err(Error::new(
                "wallet_error",
                format!("transaction execution would fail: {}", v["error"]),
            ));
        }
        let fee = atomic(&v["total_fee"])?;
        if fee > max_fee as u128 {
            return Err(rpc_invalid("RPC returned a fee above the signed maximum"));
        }
        Ok(fee)
    }
    pub fn params(&self) -> Result<ChainParams> {
        let v = self.get("/chain/info")?;
        if v["network"].as_str().is_none_or(str::is_empty) {
            return Err(rpc_invalid("chain parameters missing network"));
        }
        let params: ChainParams =
            serde_json::from_value(v).map_err(|_| rpc_invalid("invalid chain parameters"))?;
        if params.decimals > 18 {
            return Err(rpc_invalid("invalid native currency decimals"));
        }
        Ok(params)
    }
    pub fn height(&self, params: &ChainParams) -> Result<u32> {
        let node = self.get("/node/info")?;
        if !boolean(&node, "is_synced")? {
            return Err(Error::new("rpc_not_synced", "RPC node is not synced"));
        }
        if text(&node, "name")? != params.network {
            return Err(Error::new(
                "rpc_network_mismatch",
                "RPC network does not match chain parameters",
            ));
        }
        u32_field(&node, "height")
    }
}
pub fn text<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v[field]
        .as_str()
        .ok_or_else(|| rpc_invalid(format!("invalid RPC field: {field}")))
}
pub fn u32_field(v: &Value, field: &str) -> Result<u32> {
    v[field]
        .as_u64()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| rpc_invalid(format!("invalid RPC field: {field}")))
}
pub fn boolean(v: &Value, field: &str) -> Result<bool> {
    v[field]
        .as_bool()
        .ok_or_else(|| rpc_invalid(format!("invalid RPC field: {field}")))
}
pub fn atomic(v: &Value) -> Result<u128> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| rpc_invalid("invalid 128-bit atomic amount"))
}
pub fn address(v: &Value, field: &str) -> Result<Address> {
    text(v, field)?
        .parse()
        .map_err(|_| rpc_invalid("invalid address from RPC"))
}
pub fn decimals(v: &Value, currency: Address, params: &ChainParams) -> Result<u32> {
    let decimals = u32_field(v, "decimals")?;
    if decimals > 18 || (currency == Address::default() && decimals != params.decimals) {
        return Err(rpc_invalid("inconsistent currency decimals"));
    }
    Ok(decimals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, thread};

    #[test]
    fn stalled_http_request_returns_structured_timeout() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            // Keep the connection open without responding, exercising the
            // actual client's deadline rather than simulating an error enum.
            thread::sleep(Duration::from_millis(200));
        });
        let short = || {
            ureq::Agent::config_builder()
                .proxy(None)
                .timeout_global(Some(Duration::from_millis(50)))
                .build()
                .new_agent()
        };
        let rpc = Rpc {
            base: format!("http://{address}"),
            agent: short(),
            post_agent: short(),
            no_keep_alive: AtomicBool::new(false),
        };
        let error = rpc.get("/node/info").unwrap_err();
        handle.join().unwrap();
        assert_eq!(error.code, "rpc_timeout");
    }

    #[test]
    fn concurrent_reads_from_http_1_0_servers_succeed() {
        use std::io::Write;
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in server.incoming().flatten() {
                thread::spawn(move || {
                    let mut request = [0; 1024];
                    if stream.read(&mut request).unwrap_or(0) > 0 {
                        let _ = stream.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\n{}");
                        // Close a moment later, as a busy server would, so
                        // the connection still looks open when it is pooled.
                        thread::sleep(Duration::from_millis(20));
                    }
                });
            }
        });
        let rpc = Rpc::new(&format!("http://{address}")).unwrap();
        thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        assert_eq!(rpc.get("/node/info").unwrap(), serde_json::json!({}));
                    }
                });
            }
        });
    }
}

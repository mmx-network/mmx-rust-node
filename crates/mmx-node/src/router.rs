//! Native Router configuration shared by admission, peer transport and sync.
use crate::{error, Result};
use std::{
    collections::BTreeSet,
    net::{IpAddr, SocketAddr, TcpListener, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub host: String,
    pub port: Option<i32>,
    pub max_connections: Option<usize>,
    pub num_peers_out: usize,
    pub min_sync_peers: usize,
    pub max_peer_set: usize,
    pub connection_timeout: Option<Duration>,
    pub disconnect_interval: Option<Duration>,
    pub query_interval: Duration,
    pub send_interval: Duration,
    pub connect_interval: Duration,
    pub fetch_timeout: Duration,
    pub body_timeout: Duration,
    pub sync_loss_delay: Duration,
    pub peer_retry_interval: Duration,
    pub fixed_peers: BTreeSet<String>,
    pub block_peers: BTreeSet<String>,
    pub compress_level: i32,
    pub tcp_no_delay: bool,
    pub tcp_keepalive: bool,
    pub send_buffer_size: Option<usize>,
    pub receive_buffer_size: Option<usize>,
    pub listen_queue_size: i32,
    pub max_hash_cache: usize,
    pub max_sent_cache: usize,
    pub do_relay: bool,
}
impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: None,
            max_connections: Some(100),
            num_peers_out: 8,
            min_sync_peers: 3,
            max_peer_set: 100,
            connection_timeout: Some(Duration::from_secs(180)),
            disconnect_interval: None,
            query_interval: Duration::from_secs(10),
            send_interval: Duration::from_millis(20),
            connect_interval: Duration::from_secs(10),
            fetch_timeout: Duration::from_secs(15),
            body_timeout: Duration::from_secs(15),
            sync_loss_delay: Duration::from_secs(60),
            peer_retry_interval: Duration::from_secs(360 * 60),
            fixed_peers: BTreeSet::new(),
            block_peers: BTreeSet::new(),
            compress_level: 0,
            tcp_no_delay: true,
            tcp_keepalive: true,
            send_buffer_size: None,
            receive_buffer_size: None,
            listen_queue_size: 100,
            max_hash_cache: 200_000,
            max_sent_cache: 20_000,
            do_relay: true,
        }
    }
}
impl RouterConfig {
    pub fn from_config(config: &vnx::config::Config) -> Result<Self> {
        let mut value = Self::default();
        macro_rules! read {
            ($field:ident, $name:literal, $ty:ty) => {
                if let Some(setting) = config.read::<$ty>(concat!("Router.", $name))? {
                    value.$field = setting;
                }
            };
        }
        read!(host, "host", String);
        value.port = config.read("Router.port")?;
        if value.host.is_empty() || value.port.is_some_and(|p| p > u16::MAX as i32) {
            return Err(error("invalid Router.host or Router.port"));
        }
        if let Some(limit) = config.read::<i32>("Router.max_connections")? {
            value.max_connections = (limit >= 0).then_some(limit as usize);
        }
        if let Some(n) = config.read::<u32>("Router.num_peers_out")? {
            value.num_peers_out = n as usize;
        }
        if let Some(n) = config.read::<u32>("Router.min_sync_peers")? {
            value.min_sync_peers = n as usize;
        }
        if value.num_peers_out == 0 || value.min_sync_peers == 0 {
            return Err(error(
                "Router.num_peers_out and Router.min_sync_peers must be at least one",
            ));
        }
        value.min_sync_peers = value.min_sync_peers.min(value.num_peers_out);
        if value
            .max_connections
            .is_some_and(|n| value.num_peers_out > n)
        {
            return Err(error("Router.num_peers_out > Router.max_connections"));
        }
        if let Some(n) = config.read::<u32>("Router.max_peer_set")? {
            value.max_peer_set = n as usize;
        }
        if let Some(ms) = config.read::<i32>("Router.connection_timeout_ms")? {
            value.connection_timeout = (ms > 0).then(|| Duration::from_millis(ms as u64));
        }
        if let Some(seconds) = config.read::<u32>("Router.disconnect_interval")? {
            value.disconnect_interval = (seconds > 0).then(|| Duration::from_secs(seconds.into()));
        }
        for (name, target) in [
            ("query_interval_ms", &mut value.query_interval),
            ("send_interval_ms", &mut value.send_interval),
            ("connect_interval_ms", &mut value.connect_interval),
            ("fetch_timeout_ms", &mut value.fetch_timeout),
        ] {
            if let Some(ms) = config.read::<i32>(&format!("Router.{name}"))? {
                if ms <= 0 {
                    return Err(error(format!("Router.{name} must be positive")));
                }
                *target = Duration::from_millis(ms as u64);
                if name == "fetch_timeout_ms" {
                    value.body_timeout = Duration::from_millis((ms as u64 / 8).max(1));
                }
            }
        }
        if let Some(seconds) = config.read::<u32>("Router.sync_loss_delay")? {
            value.sync_loss_delay = Duration::from_secs(seconds.into());
        }
        if let Some(minutes) = config.read::<u32>("Router.peer_retry_interval")? {
            value.peer_retry_interval = Duration::from_secs(u64::from(minutes) * 60);
        }
        read!(fixed_peers, "fixed_peers", BTreeSet<String>);
        if let Some(masters) = config.read::<BTreeSet<String>>("Router.master_nodes")? {
            value.fixed_peers.extend(masters);
        }
        for peer in &value.fixed_peers {
            endpoint(peer, 1)?;
        }
        read!(block_peers, "block_peers", BTreeSet<String>);
        value.block_peers = value
            .block_peers
            .iter()
            .map(|address| canonical_host(address))
            .collect();
        read!(compress_level, "compress_level", i32);
        if !(0..=20).contains(&value.compress_level) {
            return Err(error("Router.compress_level must be between 0 and 20"));
        }
        read!(tcp_no_delay, "tcp_no_delay", bool);
        read!(tcp_keepalive, "tcp_keepalive", bool);
        read!(listen_queue_size, "listen_queue_size", i32);
        for (key, target) in [
            ("send_buffer_size", &mut value.send_buffer_size),
            ("receive_buffer_size", &mut value.receive_buffer_size),
        ] {
            if let Some(size) = config.read::<i32>(&format!("Router.{key}"))? {
                // TcpEndpoint only sets positive sizes; zero/negative retain
                // the operating system's defaults.
                *target = (size > 0).then_some(size as usize);
            }
        }
        if let Some(n) = config.read::<u32>("Router.max_hash_cache")? {
            value.max_hash_cache = n as usize;
        }
        if let Some(n) = config.read::<u32>("Router.max_sent_cache")? {
            value.max_sent_cache = n as usize;
        }
        read!(do_relay, "do_relay", bool);
        for period in [
            value.query_interval,
            value.connect_interval,
            value.fetch_timeout,
            value.peer_retry_interval,
        ] {
            if Instant::now().checked_add(period).is_none() {
                return Err(error("Router interval is too large"));
            }
        }
        Ok(value)
    }
    pub fn listen_address(&self, network_port: u16) -> Option<String> {
        let port = self.port.unwrap_or(i32::from(network_port));
        (port >= 0).then(|| format_host(&self.host, port as u16))
    }
    pub fn pool_capacity(&self) -> usize {
        self.num_peers_out.saturating_add(self.fixed_peers.len())
    }
    pub fn blocks(&self, address: &str) -> bool {
        self.block_peers.contains(&canonical_host(address))
    }
    pub fn blocks_ip(&self, ip: IpAddr) -> bool {
        self.block_peers.contains(&ip.to_string())
    }
    pub(crate) fn configure_inbound_socket(&self, socket: &socket2::Socket) -> Result<()> {
        socket.set_tcp_nodelay(self.tcp_no_delay)?;
        socket.set_keepalive(self.tcp_keepalive)?;
        if let Some(size) = self.send_buffer_size {
            socket.set_send_buffer_size(size)?;
        }
        if let Some(size) = self.receive_buffer_size {
            socket.set_recv_buffer_size(size)?;
        }
        Ok(())
    }
    pub(crate) fn bind_listener(&self, address: &str) -> Result<TcpListener> {
        let mut last = None;
        for address in address.to_socket_addrs()? {
            let bind = || -> Result<TcpListener> {
                let socket = socket2::Socket::new(
                    socket2::Domain::for_address(address),
                    socket2::Type::STREAM,
                    Some(socket2::Protocol::TCP),
                )?;
                socket.set_reuse_address(true)?;
                self.configure_inbound_socket(&socket)?;
                socket.bind(&address.into())?;
                socket.listen(self.listen_queue_size)?;
                socket.set_nonblocking(true)?;
                Ok(socket.into())
            };
            match bind() {
                Ok(listener) => return Ok(listener),
                Err(failure) => last = Some(failure),
            }
        }
        Err(last.unwrap_or_else(|| error("peer listener has no addresses")))
    }
}
fn canonical_host(address: &str) -> String {
    let host = host_part(address);
    host.parse::<IpAddr>()
        .map_or_else(|_| host.to_ascii_lowercase(), |ip| ip.to_string())
}
fn format_host(host: &str, port: u16) -> String {
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) => SocketAddr::new(ip, port).to_string(),
        Err(_) => format!("{host}:{port}"),
    }
}
fn host_part(address: &str) -> &str {
    if let Some(rest) = address.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(host, _)| host);
    }
    if address.parse::<IpAddr>().is_ok() {
        return address;
    }
    address.rsplit_once(':').map_or(address, |(host, _)| host)
}
/// Native discoveries have no port; explicit Rust HOST:PORT peers retain theirs.
pub fn endpoint(address: &str, port: u16) -> Result<String> {
    let address = address.trim();
    if address.is_empty() || address.chars().any(char::is_whitespace) {
        return Err(error("empty or invalid peer address"));
    }
    if let Ok(ip) = address.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port).to_string());
    }
    if let Ok(socket) = address.parse::<SocketAddr>() {
        return Ok(socket.to_string());
    }
    if let Some((host, explicit)) = address.rsplit_once(':') {
        if host.is_empty() || host.contains(':') {
            return Err(error("invalid peer endpoint"));
        }
        return Ok(format_host(host, explicit.parse::<u16>()?));
    }
    Ok(format_host(address, port))
}

/// Counts open sockets and outbound connection attempts without a ledger lock.
#[derive(Clone, Default)]
pub(crate) struct Connections(Arc<AtomicUsize>);
pub(crate) struct Permit(Arc<AtomicUsize>);
impl Connections {
    pub fn inbound(&self, limit: Option<usize>) -> Option<Permit> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if limit.is_some_and(|limit| count >= limit) {
                    None
                } else {
                    count.checked_add(1)
                }
            })
            .ok()
            .map(|_| Permit(self.0.clone()))
    }
    // C++ connect_client is not rejected by max_connections: fixed/outbound
    // peers can still connect when inbound clients have filled the listener.
    pub fn outbound(&self) -> Result<Permit> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map(|_| Permit(self.0.clone()))
            .map_err(|_| error("peer connection count overflow"))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vnx::serde_json::json;

    #[test]
    fn native_router_file_and_local_overrides_keep_cpp_units_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Router.json"),
            include_str!("../tests/fixtures/router-default.json"),
        )
        .unwrap();
        let config = vnx::config::Config::new();
        config.load_tree(dir.path()).unwrap();
        let policy = RouterConfig::from_config(&config).unwrap();
        assert_eq!(policy.host, "0.0.0.0");
        assert_eq!(policy.max_connections, Some(100));
        assert_eq!(policy.num_peers_out, 8);
        assert_eq!(policy.min_sync_peers, 5);
        assert_eq!(policy.max_peer_set, 100);
        assert_eq!(policy.fetch_timeout, Duration::from_secs(180));
        assert_eq!(policy.body_timeout, Duration::from_millis(22_500));
        assert_eq!(policy.connection_timeout, Some(Duration::from_secs(180)));
        assert_eq!(policy.connect_interval, Duration::from_secs(10));
        assert_eq!(policy.max_hash_cache, 200_000);
        assert_eq!(policy.max_sent_cache, 20_000);
        assert_eq!(policy.compress_level, 0);

        config.set("Router.max_connections", json!(-1)).unwrap();
        config.set("Router.num_peers_out", json!(2)).unwrap();
        config.set("Router.min_sync_peers", json!(5)).unwrap();
        config.set("Router.peer_retry_interval", json!(7)).unwrap();
        config.set("Router.query_interval_ms", json!(1234)).unwrap();
        config
            .set("Router.connection_timeout_ms", json!(0))
            .unwrap();
        config.set("Router.disconnect_interval", json!(7)).unwrap();
        config.set("Router.tcp_keepalive", json!(false)).unwrap();
        config.set("Router.listen_queue_size", json!(17)).unwrap();
        config.set("Router.send_buffer_size", json!(65536)).unwrap();
        config
            .set("Router.receive_buffer_size", json!(32768))
            .unwrap();
        config
            .set("Router.fixed_peers", json!(["one.example"]))
            .unwrap();
        config
            .set("Router.master_nodes", json!(["two.example", "one.example"]))
            .unwrap();
        let policy = RouterConfig::from_config(&config).unwrap();
        assert_eq!(policy.max_connections, None);
        assert_eq!(
            policy.min_sync_peers, 2,
            "native clamps the minimum to outbound target"
        );
        assert_eq!(
            policy.pool_capacity(),
            4,
            "fixed peers do not consume ordinary outbound slots"
        );
        assert_eq!(policy.peer_retry_interval, Duration::from_secs(420));
        assert_eq!(policy.query_interval, Duration::from_millis(1234));
        assert_eq!(policy.connection_timeout, None);
        assert_eq!(policy.disconnect_interval, Some(Duration::from_secs(7)));
        assert!(!policy.tcp_keepalive);
        assert_eq!(policy.listen_queue_size, 17);
        assert_eq!(policy.send_buffer_size, Some(65_536));
        assert_eq!(policy.receive_buffer_size, Some(32_768));
        config.set("Router.disconnect_interval", json!(0)).unwrap();
        config.set("Router.send_buffer_size", json!(0)).unwrap();
        config.set("Router.receive_buffer_size", json!(-1)).unwrap();
        let policy = RouterConfig::from_config(&config).unwrap();
        assert_eq!(policy.disconnect_interval, None);
        assert_eq!(policy.send_buffer_size, None);
        assert_eq!(policy.receive_buffer_size, None);
    }
    #[test]
    fn bad_config_is_rejected_before_connections_or_ledger_start() {
        for (key, value) in [
            ("num_peers_out", json!(0)),
            ("min_sync_peers", json!(0)),
            ("max_connections", json!(7)),
            ("fetch_timeout_ms", json!(-1)),
            ("query_interval_ms", json!(0)),
            ("connect_interval_ms", json!(0)),
            ("compress_level", json!(21)),
            ("port", json!(65536)),
            ("fixed_peers", json!(["bad.example:invalid"])),
        ] {
            let config = vnx::config::Config::new();
            config.set(&format!("Router.{key}"), value).unwrap();
            assert!(
                RouterConfig::from_config(&config).is_err(),
                "accepted {key}"
            );
        }
    }
    #[test]
    fn endpoint_and_listener_overrides_preserve_ports_and_ipv6() {
        assert_eq!(
            endpoint("peer.example", 11337).unwrap(),
            "peer.example:11337"
        );
        assert_eq!(
            endpoint("peer.example:12345", 11337).unwrap(),
            "peer.example:12345"
        );
        assert_eq!(endpoint("::1", 11337).unwrap(), "[::1]:11337");
        assert_eq!(endpoint("[::1]", 11337).unwrap(), "[::1]:11337");
        assert_eq!(endpoint("[::1]:12345", 11337).unwrap(), "[::1]:12345");
        let mut config = RouterConfig {
            host: "::1".into(),
            ..Default::default()
        };
        assert_eq!(config.listen_address(11337).as_deref(), Some("[::1]:11337"));
        config.port = Some(0);
        assert_eq!(config.listen_address(11337).as_deref(), Some("[::1]:0"));
        config.port = Some(-1);
        assert!(config.listen_address(11337).is_none());
        config
            .block_peers
            .extend(["bad.example".into(), "::1".into(), "192.0.2.1".into()]);
        assert!(config.blocks("BAD.EXAMPLE:11337"));
        assert!(config.blocks_ip("0:0:0:0:0:0:0:1".parse().unwrap()));
        assert!(config.blocks_ip("192.0.2.1".parse().unwrap()));
        assert!(!config.blocks("good.example:11337"));
    }
    #[test]
    fn native_admission_counts_outbound_and_releases_failed_or_closed_sockets() {
        let counts = Connections::default();
        let outgoing = counts.outbound().unwrap();
        let incoming = counts.inbound(Some(2)).unwrap();
        assert!(counts.inbound(Some(2)).is_none());
        drop(outgoing);
        let replacement = counts.inbound(Some(2)).unwrap();
        let fixed = counts.outbound().unwrap(); // native fixed/outbound bypasses inbound admission
        assert!(counts.inbound(Some(2)).is_none());
        drop((incoming, replacement, fixed));
        assert_eq!(counts.0.load(Ordering::Acquire), 0);
        let unlimited: Vec<_> = (0..100).map(|_| counts.inbound(None).unwrap()).collect();
        assert_eq!(counts.0.load(Ordering::Acquire), 100);
        drop(unlimited);
        assert_eq!(counts.0.load(Ordering::Acquire), 0);
    }
    #[test]
    fn simultaneous_inbound_admission_cannot_exceed_its_budget() {
        let counts = Connections::default();
        let barrier = std::sync::Barrier::new(33);
        let observed = std::thread::scope(|scope| {
            for _ in 0..32 {
                let counts = &counts;
                let barrier = &barrier;
                scope.spawn(move || {
                    let permit = counts.inbound(Some(3));
                    barrier.wait();
                    barrier.wait();
                    drop(permit);
                });
            }
            barrier.wait();
            let observed = counts.0.load(Ordering::Acquire);
            barrier.wait();
            observed
        });
        assert_eq!(observed, 3);
        assert_eq!(counts.0.load(Ordering::Acquire), 0);
    }
}

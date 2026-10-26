//! Bounded untrusted hints and authenticated outbound peers. Cache I/O never
//! holds the directory lock or runs on a transport/coordinator thread.
use crate::{
    error,
    router::{canonical_ip, RouterConfig},
    Result,
};
use mmx_consensus::Hash;
use rand::seq::IteratorRandom;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use vnx::serde_json::{self, json, Value};

pub(crate) const DISCOVERY_COUNT: usize = 32;
const FILE_BYTES: u64 = 65_536;
const FILE_PEERS: usize = 1024;
const SAVE_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub(crate) struct Book(Arc<Shared>);
struct Shared {
    port: u16,
    genesis: Hash,
    state: Mutex<State>,
    wake: Condvar,
}
struct State {
    policy: Arc<RouterConfig>,
    verified: BTreeMap<SocketAddr, u64>,
    hints: BTreeMap<SocketAddr, u64>,
    serial: u64,
    revision: u64,
    save_revision: u64,
    dirty: bool,
    stopping: bool,
}
impl State {
    fn changed(&mut self, persist: bool) {
        self.revision = self.revision.wrapping_add(1);
        if persist {
            self.save_revision = self.save_revision.wrapping_add(1);
            self.dirty = true;
        }
    }
}
impl Book {
    pub fn new(port: u16, genesis: Hash) -> Self {
        Self(Arc::new(Shared {
            port,
            genesis,
            wake: Condvar::new(),
            state: Mutex::new(State {
                policy: Arc::new(RouterConfig::default()),
                verified: BTreeMap::new(),
                hints: BTreeMap::new(),
                serial: 0,
                revision: 1,
                save_revision: 0,
                dirty: false,
                stopping: false,
            }),
        }))
    }
    pub fn configure(&self, policy: Arc<RouterConfig>) -> Result<()> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        let before = state.verified.len();
        state
            .verified
            .retain(|address, _| !policy.blocks_ip(address.ip()));
        state
            .hints
            .retain(|address, _| !policy.blocks_ip(address.ip()));
        trim(&mut state.verified, policy.max_peer_set);
        trim(&mut state.hints, policy.max_peer_set);
        state.policy = policy;
        let persist = before != state.verified.len();
        state.changed(persist);
        self.0.wake.notify_one();
        Ok(())
    }
    pub fn learn(&self, address: &str) -> Result<()> {
        let Ok(ip) = address.parse::<IpAddr>() else {
            return Ok(());
        };
        let ip = canonical_ip(ip);
        if !public_ip(ip) || self.0.port == 0 {
            return Ok(());
        }
        let endpoint = SocketAddr::new(ip, self.0.port);
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        if state.policy.max_peer_set == 0
            || state.policy.blocks_ip(ip)
            || state.verified.contains_key(&endpoint)
            || state.hints.contains_key(&endpoint)
        {
            return Ok(());
        }
        state.serial = state.serial.saturating_add(1);
        let serial = state.serial;
        state.hints.insert(endpoint, serial);
        let limit = state.policy.max_peer_set;
        trim(&mut state.hints, limit);
        state.changed(false);
        Ok(())
    }
    pub fn confirm(&self, endpoint: SocketAddr) -> Result<()> {
        let endpoint = canonical_endpoint(endpoint);
        if endpoint.port() == 0 {
            return Ok(());
        }
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        if state.policy.max_peer_set == 0 || state.policy.blocks_ip(endpoint.ip()) {
            return Ok(());
        }
        state.serial = state.serial.saturating_add(1);
        let serial = state.serial;
        state.hints.remove(&endpoint);
        state.verified.insert(endpoint, serial);
        let limit = state.policy.max_peer_set;
        trim(&mut state.verified, limit);
        state.changed(true);
        self.0.wake.notify_one();
        Ok(())
    }
    pub fn candidates(&self, since: u64) -> Result<Option<(u64, Vec<String>)>> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        if state.revision == since {
            return Ok(None);
        }
        let peers = state
            .verified
            .keys()
            .chain(state.hints.keys())
            .map(ToString::to_string)
            .collect();
        Ok(Some((state.revision, peers)))
    }
    pub fn verified(&self) -> Result<Vec<String>> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?
            .verified
            .keys()
            .map(ToString::to_string)
            .collect())
    }
    pub fn public(&self, count: usize) -> Result<Vec<String>> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        Ok(state
            .verified
            .keys()
            .filter(|address| address.port() == self.0.port && public_ip(address.ip()))
            .choose_multiple(&mut rand::thread_rng(), count.min(DISCOVERY_COUNT))
            .into_iter()
            .map(|address| address.ip().to_string())
            .collect())
    }
    pub fn file_name(&self) -> String {
        format!(
            "rust-peers-{}-{}.json",
            self.0.port,
            vnx::util::to_hex_string(&self.0.genesis, true, false)
        )
    }
    fn load(&self, path: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.len() > FILE_BYTES {
            return Err(error("peer cache is not a bounded regular file"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > FILE_BYTES {
            return Err(error("peer cache exceeds byte limit"));
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if value["version"] != json!(1)
            || value["port"] != json!(self.0.port)
            || value["genesis"] != json!(vnx::util::to_hex_string(&self.0.genesis, true, false))
        {
            return Err(error("peer cache network/version mismatch"));
        }
        let peers = value["peers"]
            .as_array()
            .filter(|peers| peers.len() <= FILE_PEERS)
            .ok_or_else(|| error("invalid peer cache entries"))?;
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?;
        if !state.verified.is_empty() {
            return Ok(());
        }
        for value in peers.iter().rev() {
            if let Some(endpoint) = value
                .as_str()
                .and_then(|value| value.parse::<SocketAddr>().ok())
            {
                let endpoint = canonical_endpoint(endpoint);
                if endpoint.port() > 0
                    && !state.policy.blocks_ip(endpoint.ip())
                    && state.policy.max_peer_set > 0
                {
                    state.serial = state.serial.saturating_add(1);
                    let serial = state.serial;
                    state.verified.insert(endpoint, serial);
                }
            }
        }
        let limit = state.policy.max_peer_set;
        trim(&mut state.verified, limit);
        state.changed(false);
        Ok(())
    }
}
fn trim(entries: &mut BTreeMap<SocketAddr, u64>, limit: usize) {
    while entries.len() > limit {
        let oldest = *entries.iter().min_by_key(|(_, serial)| *serial).unwrap().0;
        entries.remove(&oldest);
    }
}
fn public_ip(ip: IpAddr) -> bool {
    match canonical_ip(ip) {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (18..=19).contains(&b))
                && !(a == 192 && b == 0)
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000 && !(segments[0] == 0x2001 && segments[1] == 0xdb8)
        }
    }
}
fn canonical_endpoint(endpoint: SocketAddr) -> SocketAddr {
    match endpoint {
        SocketAddr::V6(address) => address.ip().to_ipv4_mapped().map_or(endpoint, |ip| {
            SocketAddr::new(IpAddr::V4(ip), address.port())
        }),
        endpoint => endpoint,
    }
}
pub(crate) struct Cache {
    book: Book,
    pub path: PathBuf,
    worker: Option<JoinHandle<()>>,
}
impl Cache {
    pub fn start(book: Book, directory: &Path) -> Result<Self> {
        let path = directory.join(book.file_name());
        if let Err(failure) = book.load(&path) {
            if !failure
                .downcast_ref::<std::io::Error>()
                .is_some_and(|failure| failure.kind() == std::io::ErrorKind::NotFound)
            {
                eprintln!("Peer cache {}: {failure}", path.display());
            }
        }
        Self::spawn(book, path, write_cache)
    }
    fn spawn(
        book: Book,
        path: PathBuf,
        mut save: impl FnMut(&Path, &Value) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        book.0
            .state
            .lock()
            .map_err(|_| error("peer directory poisoned"))?
            .stopping = false;
        let owner = book.clone();
        let target = path.clone();
        let worker = std::thread::Builder::new()
            .name("mmx-peer-cache".into())
            .spawn(move || {
                let mut next_save = Instant::now();
                let mut state = owner
                    .0
                    .state
                    .lock()
                    .unwrap_or_else(|failure| failure.into_inner());
                loop {
                    if state.stopping && !state.dirty {
                        break;
                    }
                    if !state.dirty || (!state.stopping && Instant::now() < next_save) {
                        let wait = if state.dirty {
                            next_save.saturating_duration_since(Instant::now())
                        } else {
                            SAVE_INTERVAL
                        };
                        state = owner
                            .0
                            .wake
                            .wait_timeout(state, wait)
                            .unwrap_or_else(|failure| failure.into_inner())
                            .0;
                        continue;
                    }
                    let generation = state.save_revision;
                    let mut peers: Vec<_> = state.verified.iter().collect();
                    peers.sort_by_key(|(_, serial)| std::cmp::Reverse(**serial));
                    let peers: Vec<_> = peers
                        .into_iter()
                        .take(FILE_PEERS)
                        .map(|(address, _)| address.to_string())
                        .collect();
                    drop(state);
                    let value = json!({"version":1,"port":owner.0.port,
                    "genesis":vnx::util::to_hex_string(&owner.0.genesis,true,false),"peers":peers});
                    let result = save(&target, &value);
                    if let Err(failure) = &result {
                        eprintln!("Peer cache {}: {failure}", target.display());
                    }
                    state = owner
                        .0
                        .state
                        .lock()
                        .unwrap_or_else(|failure| failure.into_inner());
                    if result.is_ok() && state.save_revision == generation {
                        state.dirty = false;
                    }
                    if state.stopping && (result.is_err() || !state.dirty) {
                        break;
                    }
                    next_save = Instant::now() + SAVE_INTERVAL;
                }
            })?;
        Ok(Self {
            book,
            path,
            worker: Some(worker),
        })
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            {
                let mut state = self
                    .book
                    .0
                    .state
                    .lock()
                    .unwrap_or_else(|failure| failure.into_inner());
                state.stopping = true;
                self.book.0.wake.notify_all();
            }
            if worker.join().is_err() {
                eprintln!("Peer cache writer panicked");
            }
        }
    }
}
fn write_cache(path: &Path, value: &Value) -> Result<()> {
    write_cache_with(path, value, |temporary, path| {
        temporary.persist(path).map_err(|failure| failure.error)?;
        Ok(())
    })
}
fn write_cache_with(
    path: &Path,
    value: &Value,
    replace: impl FnOnce(tempfile::NamedTempFile, &Path) -> Result<()>,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > FILE_BYTES {
        return Err(error("peer cache exceeds byte limit"));
    }
    let directory = path
        .parent()
        .ok_or_else(|| error("peer cache has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    replace(temporary, path)?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_book(limit: usize) -> Book {
        let book = Book::new(11337, [7; 32]);
        book.configure(Arc::new(RouterConfig {
            max_peer_set: limit,
            ..Default::default()
        }))
        .unwrap();
        book
    }
    #[test]
    fn hints_are_bounded_filtered_and_cannot_evict_or_advertise_as_confirmed_peers() {
        let book = make_book(2);
        for address in ["1.1.1.1:11337", "8.8.8.8:11337", "9.9.9.9:11337"] {
            book.confirm(address.parse().unwrap()).unwrap();
        }
        for address in [
            "127.0.0.1",
            "::ffff:127.0.0.1",
            "10.0.0.1",
            "172.16.1.2",
            "192.168.1.2",
            "169.254.1.1",
            "100.64.0.1",
            "0.1.2.3",
            "224.0.0.1",
            "198.18.0.1",
            "203.0.113.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "peer.example",
            "1.2.3.4:11337",
        ] {
            book.learn(address).unwrap();
        }
        assert_eq!(book.candidates(0).unwrap().unwrap().1.len(), 2);
        for address in ["4.2.2.1", "4.2.2.2", "4.2.2.3"] {
            book.learn(address).unwrap();
        }
        assert_eq!(book.verified().unwrap(), ["8.8.8.8:11337", "9.9.9.9:11337"]);
        assert_eq!(book.candidates(0).unwrap().unwrap().1.len(), 4);
        let public: std::collections::BTreeSet<_> =
            book.public(usize::MAX).unwrap().into_iter().collect();
        assert_eq!(public, ["8.8.8.8".to_owned(), "9.9.9.9".to_owned()].into());
        let revision = book.candidates(0).unwrap().unwrap().0;
        book.learn("4.2.2.3").unwrap();
        assert!(book.candidates(revision).unwrap().is_none());
    }
    #[test]
    fn cache_roundtrip_preserves_verified_ports_and_network_namespace_but_never_hints() {
        let dir = tempfile::tempdir().unwrap();
        let original = make_book(4);
        original
            .confirm("127.0.0.1:12345".parse().unwrap())
            .unwrap();
        original
            .confirm("[2606:4700::1111]:11337".parse().unwrap())
            .unwrap();
        original
            .confirm("[fe80::1%7]:11337".parse().unwrap())
            .unwrap();
        original.learn("8.8.8.8").unwrap();
        let cache = Cache::start(original.clone(), dir.path()).unwrap();
        let path = cache.path.clone();
        drop(cache);
        assert!(std::fs::metadata(&path).unwrap().len() <= FILE_BYTES);
        let restored = make_book(4);
        let cache = Cache::start(restored.clone(), dir.path()).unwrap();
        assert_eq!(restored.verified().unwrap(), original.verified().unwrap());
        assert_eq!(restored.candidates(0).unwrap().unwrap().1.len(), 3);
        assert_eq!(restored.public(32).unwrap(), ["2606:4700::1111"]);
        drop(cache);
        let blocked = make_book(4);
        blocked
            .configure(Arc::new(RouterConfig {
                max_peer_set: 4,
                block_peers: ["127.0.0.1".into()].into(),
                ..Default::default()
            }))
            .unwrap();
        let cache = Cache::start(blocked.clone(), dir.path()).unwrap();
        assert_eq!(
            blocked.verified().unwrap(),
            ["[2606:4700::1111]:11337", "[fe80::1%7]:11337"]
        );
        drop(cache);
        for different in [Book::new(11338, [7; 32]), Book::new(11337, [8; 32])] {
            assert_ne!(different.file_name(), original.file_name());
            assert!(different.load(&path).is_err());
            assert!(different.verified().unwrap().is_empty());
        }
    }
    #[test]
    fn corrupt_oversized_and_nonregular_caches_do_not_poison_or_abort_startup() {
        let dir = tempfile::tempdir().unwrap();
        let book = make_book(2);
        let path = dir.path().join(book.file_name());
        std::fs::write(&path, b"{truncated").unwrap();
        let cache = Cache::start(book.clone(), dir.path()).unwrap();
        assert!(book.verified().unwrap().is_empty());
        drop(cache);
        assert_eq!(std::fs::read(&path).unwrap(), b"{truncated");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(FILE_BYTES + 1)
            .unwrap();
        assert!(book.load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(book.load(&path).is_err());
        std::fs::remove_dir(&path).unwrap();
        book.confirm("1.1.1.1:11337".parse().unwrap()).unwrap();
        let cache = Cache::start(book.clone(), dir.path()).unwrap();
        drop(cache);
        let before = std::fs::read(&path).unwrap();
        assert!(write_cache(&path, &json!({"padding":"x".repeat(FILE_BYTES as usize)})).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "failed replacement preserves the last complete cache"
        );
        let result = write_cache_with(
            &path,
            &json!({"replacement":"complete new snapshot"}),
            |temporary, _| {
                assert!(temporary.as_file().metadata()?.len() > 0);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected atomic replacement failure",
                )
                .into())
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn a_blocked_writer_does_not_hold_up_directory_updates_and_shutdown_flushes_the_latest_snapshot(
    ) {
        use std::sync::mpsc::channel;
        let dir = tempfile::tempdir().unwrap();
        let book = make_book(4);
        book.confirm("1.1.1.1:11337".parse().unwrap()).unwrap();
        let path = dir.path().join(book.file_name());
        let (snapshots, received) = channel();
        let (release, resume) = channel();
        let cache = Cache::spawn(book.clone(), path.clone(), move |path, value| {
            let _ = snapshots.send(value.clone());
            resume.recv()?;
            write_cache(path, value)
        })
        .unwrap();
        let release = release; // close before joining the writer on assertion failure
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::scope(|scope| {
            let (finished, done) = channel();
            let book = &book;
            let worker = scope.spawn(move || {
                book.confirm("9.9.9.9:11337".parse().unwrap()).unwrap();
                finished.send(book.candidates(0).unwrap()).unwrap();
            });
            let result = done.recv_timeout(Duration::from_secs(2));
            if result.is_err() {
                let _ = release.send(());
            }
            worker.join().unwrap();
            assert_eq!(result.unwrap().unwrap().1.len(), 2);
        });
        assert!(
            received.try_recv().is_err(),
            "changes coalesce while one write is blocked"
        );
        release.send(()).unwrap();
        release.send(()).unwrap();
        drop(cache);
        let restored = make_book(4);
        restored.load(&path).unwrap();
        assert_eq!(restored.verified().unwrap(), book.verified().unwrap());
    }
}

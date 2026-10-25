use super::*;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex,
    thread,
};

const NATIVE: &str = "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev";
/// crates/mmx-wallet/tests/fixtures/reference.json (C++ reference values).
const PHRASE: &str = "cactus wedding mansion deal random arrive gate tissue bench party setup \
                      captain abstract mandate crouch lift anxiety embark achieve army expect \
                      account amount claim";
const FP_NONE: &str = "3291348052";
const FP_EMPTY: &str = "2555221629";
const FP_PASS: &str = "2138401816";
const PASS: &str = "test 世界 🔑";

/// Minimal chain server: one request per connection, answers from `MockState`.
struct Mock {
    url: String,
    state: Arc<Mutex<MockState>>,
}
struct MockState {
    network: String,
    height: u32,
    synced: bool,
    /// address → `/address` balance rows
    balances: HashMap<String, Vec<Value>>,
    /// path (without query) → (status, body); overrides the defaults
    responses: HashMap<String, (u16, Vec<u8>)>,
    /// lowercase transaction ID → `/transaction` answer (default: unknown)
    transactions: HashMap<String, Value>,
    gets: Vec<String>,
    posts: Vec<(String, Vec<u8>)>,
    /// path (without query) → how long its answers are held back
    delays: HashMap<String, Duration>,
}
impl Mock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState {
            network: "mainnet".into(),
            height: 1000,
            synced: true,
            balances: HashMap::new(),
            responses: HashMap::new(),
            transactions: HashMap::new(),
            gets: Vec::new(),
            posts: Vec::new(),
            delays: HashMap::new(),
        }));
        let shared = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                thread::spawn(move || serve(stream, &state));
            }
        });
        Self { url, state }
    }
    fn with<T>(&self, f: impl FnOnce(&mut MockState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }
    fn set_mmx(&self, address: &str, amount: u128) {
        self.with(|s| {
            s.balances.insert(
                address.into(),
                vec![json!({"contract": NATIVE, "amount": amount.to_string(), "decimals": 6, "symbol": "MMX", "is_native": true})],
            )
        });
    }
    fn gets(&self) -> usize {
        self.with(|s| s.gets.len())
    }
    fn gets_of(&self, path: &str) -> usize {
        self.with(|s| {
            s.gets
                .iter()
                .filter(|t| t.split('?').next() == Some(path))
                .count()
        })
    }
    /// All POST paths, in order.
    fn post_paths(&self) -> Vec<String> {
        self.with(|s| s.posts.iter().map(|(p, _)| p.clone()).collect())
    }
    fn posts(&self, path: &str) -> Vec<Vec<u8>> {
        self.with(|s| {
            s.posts
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, b)| b.clone())
                .collect()
        })
    }
    fn respond(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
        self.with(|s| s.responses.insert(path.into(), (status, body.into())));
    }
}

fn serve(mut stream: TcpStream, state: &Mutex<MockState>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let Ok(n) = stream.read(&mut buf) else { return };
        if n == 0 {
            return;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or("").split(' ');
    let method = request.next().unwrap_or("").to_owned();
    let target = request.next().unwrap_or("").to_owned();
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = data[header_end..].to_vec();
    while body.len() < length {
        let Ok(n) = stream.read(&mut buf) else { return };
        if n == 0 {
            return;
        }
        body.extend_from_slice(&buf[..n]);
    }
    let (status, response) = answer(state, &method, &target, body);
    // Copied out first: sleeping with the state locked would stall the test.
    let path = target.split('?').next().unwrap_or("");
    let delay = state.lock().unwrap().delays.get(path).copied();
    if let Some(delay) = delay {
        thread::sleep(delay);
    }
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    let _ = stream.write_all(&response);
}

fn answer(state: &Mutex<MockState>, method: &str, target: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
    let mut s = state.lock().unwrap();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .unwrap_or("")
            .to_owned()
    };
    if method == "POST" {
        s.posts.push((path.to_owned(), body.clone()));
    } else {
        s.gets.push(target.to_owned());
    }
    if let Some((status, body)) = s.responses.get(path) {
        return (*status, body.clone());
    }
    let reply = |v: Value| (200, serde_json::to_vec(&v).unwrap());
    match (method, path) {
        ("GET", "/node/info") => {
            reply(json!({"name": s.network, "height": s.height, "is_synced": s.synced}))
        }
        ("GET", "/chain/info") => reply(json!({
            "network": "mainnet", "decimals": 6, "min_txfee": 20000, "min_txfee_io": 10000,
            "min_txfee_sign": 10000, "min_txfee_memo": 5000, "min_fee_ratio": [1, 2, 3, 5, 10]
        })),
        ("GET", "/address") => {
            let rows = s.balances.get(&param("id")).cloned().unwrap_or_default();
            reply(json!({"balances": rows}))
        }
        ("GET", "/address/history") => reply(json!([])),
        ("GET", "/transaction") => match s.transactions.get(&param("id")) {
            Some(tx) => reply(tx.clone()),
            None => (204, Vec::new()),
        },
        ("POST", "/transaction/validate") => {
            let tx: Value = serde_json::from_slice(&body).unwrap();
            let fee =
                tx["static_cost"].as_u64().unwrap() * tx["fee_ratio"].as_u64().unwrap() / 1024;
            reply(json!({"did_fail": false, "total_fee": fee.to_string()}))
        }
        ("POST", "/transaction/broadcast") => reply(json!({})),
        _ => (404, b"{\"error\":\"unknown endpoint\"}".to_vec()),
    }
}

fn service(mock: Option<&Mock>) -> (tempfile::TempDir, Service) {
    let dir = tempfile::tempdir().unwrap();
    let wallets = dir.path().join("wallets");
    if let Some(mock) = mock {
        storage::create_private_directory(&wallets).unwrap();
        fs::write(
            wallets.join(config::FILE),
            serde_json::to_vec(&json!({ "rpc": mock.url })).unwrap(),
        )
        .unwrap();
    }
    let service = Service::open(Some(wallets)).unwrap();
    (dir, service)
}

fn secret(s: &str) -> Zeroizing<String> {
    Zeroizing::new(s.to_owned())
}

fn read_config(service: &Service) -> Value {
    serde_json::from_slice(&fs::read(service.directory().join(config::FILE)).unwrap()).unwrap()
}

/// Writes a key file directly, as the node or another tool would.
fn write_key(service: &Service, name: &str, key: &KeyFile) {
    storage::save_key(&service.directory().join(name), key).unwrap();
}

fn derive(seed: &[u8; 32], passphrase: &str, count: u32) -> Vec<String> {
    Wallet::derive(seed, passphrase, 0, count)
        .unwrap()
        .addresses
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn seed() -> [u8; 32] {
    words_to_seed(PHRASE).unwrap()
}

/// A new wallet saved from fresh words, as the UI does after the backup check.
fn create(
    svc: &Service,
    name: &str,
    passphrase: Option<Zeroizing<String>>,
    count: u32,
) -> (WalletInfo, Zeroizing<String>) {
    let words = Service::new_mnemonic();
    let info = svc.create(name, words.clone(), passphrase, count).unwrap();
    (info, words)
}

fn send_request(wallet: &str, to: &str, amount: &str) -> SendRequest {
    SendRequest {
        wallet_id: wallet.into(),
        to: to.into(),
        contract: NATIVE.into(),
        amount: amount.into(),
        memo: None,
        fee_level: 1,
    }
}

/// Mainnet fee of a transfer (the mock's chain parameters).
fn fee(inputs: usize, owners: usize, memo: Option<&str>, level: u128) -> u128 {
    let params: mmx_wallet::ChainParams = serde_json::from_value(json!({
        "network": "mainnet", "decimals": 6, "min_txfee": 20000, "min_txfee_io": 10000,
        "min_txfee_sign": 10000, "min_txfee_memo": 5000
    }))
    .unwrap();
    mmx_wallet::static_fee(&params, inputs, owners, memo) * level
}
fn fee_one_input(memo: Option<&str>, level: u128) -> u128 {
    fee(1, 1, memo, level)
}

#[test]
fn service_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Service>();
}

#[test]
fn error_codes_are_translated() {
    for (from, to) in [
        ("rpc_transport_error", "rpc_unreachable"),
        ("transaction_expired", "tx_expired"),
        ("wallet_directory_unavailable", "io_error"),
        ("wallet_error", "internal"),
        ("transaction_exists", "internal"),
        ("mnemonic_required", "internal"),
        ("rpc_timeout", "rpc_timeout"),
        ("locked", "locked"),
    ] {
        assert_eq!(service_error(Error::new(from, "m")).code, to, "{from}");
    }
}

#[test]
fn create_list_rename_resize_and_remove() {
    let (_dir, svc) = service(None);
    assert!(svc.wallets().unwrap().is_empty());
    let (info, words) = create(&svc, "  Savings ", None, 10);
    assert_eq!(words.split(' ').count(), 24);
    assert_eq!(info.id, format!("wallet_{}.dat", info.fingerprint));
    assert_eq!(info.name, "Savings");
    assert!(!info.with_passphrase && !info.locked);
    assert_eq!(info.address_count, 10);
    assert_eq!(svc.wallets().unwrap(), vec![info.clone()]);
    assert_eq!(
        svc.app_state().unwrap().active_wallet.as_deref(),
        Some(info.id.as_str())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&svc.directory().join(&info.id)), 0o600);
        assert_eq!(mode(svc.directory()), 0o700);
    }
    // The key file is the CLI's format and opens to the same addresses.
    let key = storage::read_key(&svc.directory().join(&info.id)).unwrap();
    assert_eq!(seed_to_words(&key.seed_value), *words);
    assert!(key.finger_print.is_none());
    let addresses = svc.addresses(&info.id).unwrap();
    assert_eq!(addresses, derive(&key.seed_value, "", 10));
    assert_eq!(*svc.mnemonic(&info.id).unwrap(), *words);

    svc.rename(&info.id, " Daily ").unwrap();
    assert_eq!(svc.wallets().unwrap()[0].name, "Daily");
    for bad in ["", "   ", "tab\there", &"x".repeat(65)] {
        assert_eq!(
            svc.rename(&info.id, bad).unwrap_err().code,
            "invalid_argument"
        );
    }
    assert_eq!(
        svc.rename("wallet_1.dat", "X").unwrap_err().code,
        "wallet_not_found"
    );

    assert_eq!(svc.set_address_count(&info.id, 3).unwrap(), addresses[..3]);
    assert_eq!(svc.wallets().unwrap()[0].address_count, 3);
    assert_eq!(svc.addresses(&info.id).unwrap().len(), 3);
    assert_eq!(svc.set_address_count(&info.id, 100).unwrap().len(), 100);
    for bad in [0, 101] {
        assert_eq!(
            svc.set_address_count(&info.id, bad).unwrap_err().code,
            "invalid_argument"
        );
    }
    assert_eq!(svc.wallets().unwrap()[0].address_count, 100);

    assert_eq!(
        svc.import("Again", words.clone(), None, 10)
            .unwrap_err()
            .code,
        "wallet_exists"
    );

    svc.remove(&info.id).unwrap();
    assert!(svc.wallets().unwrap().is_empty());
    let removed: Vec<_> = fs::read_dir(svc.directory().join("removed"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(removed.len(), 1);
    assert!(removed[0].starts_with(&format!("{}.", info.id)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let removed_dir = fs::metadata(svc.directory().join("removed")).unwrap();
        assert_eq!(removed_dir.permissions().mode() & 0o777, 0o700);
    }
    let config = read_config(&svc);
    assert!(config["wallets"].get(&info.id).is_none());
    assert!(config.get("active_wallet").is_none());
    assert_eq!(svc.remove(&info.id).unwrap_err().code, "wallet_not_found");
    assert_eq!(
        svc.addresses(&info.id).unwrap_err().code,
        "wallet_not_found"
    );

    // Removing again after re-importing keeps both backups.
    let again = svc.import("Again", words.clone(), None, 5).unwrap();
    assert_eq!(again.id, info.id);
    assert_eq!(again.address_count, 5);
    svc.remove(&again.id).unwrap();
    assert_eq!(
        fs::read_dir(svc.directory().join("removed"))
            .unwrap()
            .count(),
        2
    );
}

#[test]
fn import_normalizes_words_and_redacts_errors() {
    let (_dir, svc) = service(None);
    let messy = format!("  {}\n", PHRASE.to_uppercase().replace(' ', " \t "));
    let info = svc.import("Main", secret(&messy), None, 10).unwrap();
    assert_eq!(info.fingerprint, FP_NONE);
    assert_eq!(info.id, format!("wallet_{FP_NONE}.dat"));
    assert_eq!(svc.addresses(&info.id).unwrap(), derive(&seed(), "", 10));
    let wrong_checksum = PHRASE.replace("claim", "zoo");
    for bad in [
        "",
        "abandon",
        wrong_checksum.as_str(),
        "never-echo-this-secret",
        "abandón abandon",
    ] {
        let e = svc.import("Main", secret(bad), None, 10).unwrap_err();
        assert_eq!(e.code, "invalid_mnemonic", "{bad}");
        assert!(!e.message.contains("never-echo"));
    }
    assert_eq!(
        svc.import("Main", secret(&"abandon ".repeat(600)), None, 10)
            .unwrap_err()
            .code,
        "invalid_mnemonic"
    );
    assert_eq!(
        svc.import("", secret(PHRASE), None, 10).unwrap_err().code,
        "invalid_argument"
    );
    let with_pass = svc
        .import("Pass", secret(PHRASE), Some(secret(PASS)), 3)
        .unwrap();
    assert_eq!(with_pass.fingerprint, FP_PASS);
    assert!(with_pass.with_passphrase && !with_pass.locked);
    assert_eq!(
        svc.addresses(&with_pass.id).unwrap(),
        derive(&seed(), PASS, 3)
    );
    assert_eq!(
        svc.import("Main", secret(PHRASE), None, 0)
            .unwrap_err()
            .code,
        "invalid_argument"
    );
}

#[test]
fn passphrase_detection_and_locking() {
    let (_dir, svc) = service(None);
    let (info, words) = create(&svc, "Vault", Some(secret("correct horse")), 10);
    assert!(info.with_passphrase && !info.locked);
    let key = storage::read_key(&svc.directory().join(&info.id)).unwrap();
    assert_eq!(
        info.fingerprint,
        fingerprint(&key.seed_value, Some("correct horse"))
    );
    let expected = derive(&key.seed_value, "correct horse", 10);
    assert_eq!(svc.addresses(&info.id).unwrap(), expected);

    svc.lock(&info.id).unwrap();
    assert!(svc.wallets().unwrap()[0].locked);
    for code in [
        svc.addresses(&info.id).unwrap_err().code,
        svc.mnemonic(&info.id).unwrap_err().code,
        svc.set_address_count(&info.id, 5).unwrap_err().code,
        svc.max_send(&info.id, NATIVE, None, 1).unwrap_err().code,
        svc.prepare_send(&send_request(&info.id, &expected[1], "1"))
            .unwrap_err()
            .code,
    ] {
        assert_eq!(code, "locked");
    }
    // Locked wallets can still be renamed; nothing else changed.
    svc.rename(&info.id, "Cold").unwrap();
    assert_eq!(svc.wallets().unwrap()[0].address_count, 10);

    assert_eq!(
        svc.unlock(&info.id, secret("")).unwrap_err().code,
        "passphrase_required"
    );
    assert_eq!(
        svc.unlock(&info.id, secret("correct horse "))
            .unwrap_err()
            .code,
        "invalid_passphrase"
    );
    let unlocked = svc.unlock(&info.id, secret("correct horse")).unwrap();
    assert!(!unlocked.locked && unlocked.with_passphrase);
    assert_eq!(unlocked.name, "Cold");
    assert_eq!(svc.addresses(&info.id).unwrap(), expected);
    assert_eq!(*svc.mnemonic(&info.id).unwrap(), *words);
    svc.lock_all();
    assert_eq!(svc.addresses(&info.id).unwrap_err().code, "locked");

    // Importing the same words with the passphrase finds the same wallet;
    // without it, they are a different wallet.
    assert_eq!(
        svc.import("X", words.clone(), Some(secret("correct horse")), 10)
            .unwrap_err()
            .code,
        "wallet_exists"
    );
    let plain = svc.import("Plain", words.clone(), None, 10).unwrap();
    assert!(!plain.with_passphrase);
    assert_ne!(svc.addresses(&plain.id).unwrap(), expected);
}

#[test]
fn empty_and_absent_passphrases_never_prompt() {
    let (_dir, svc) = service(None);
    // C++ wallets created with an empty passphrase store its fingerprint.
    let empty = KeyFile::new(seed(), Some(""));
    write_key(&svc, "wallet.dat", &empty);
    // Others store the no-passphrase fingerprint explicitly.
    let other = words_to_seed(&seed_to_words(&[7; 32])).unwrap();
    let explicit: KeyFile = serde_json::from_value(json!({
        "__type": "mmx.KeyFile",
        "seed_value": hex(&other),
        "finger_print": fingerprint(&other, None),
    }))
    .unwrap();
    write_key(&svc, "wallet_explicit.dat", &explicit);

    let list = svc.wallets().unwrap();
    assert_eq!(list.len(), 2);
    for w in &list {
        assert!(
            !w.with_passphrase && !w.locked && w.error.is_none(),
            "{w:?}"
        );
    }
    assert_eq!(list[0].id, "wallet.dat");
    assert_eq!(list[0].name, "Wallet");
    assert_eq!(list[0].fingerprint, FP_EMPTY);
    assert_eq!(list[1].name, "Wallet explicit");
    assert_eq!(
        svc.addresses("wallet.dat").unwrap(),
        derive(&seed(), "", 10)
    );
    assert_eq!(
        svc.unlock("wallet.dat", secret("ignored")).unwrap(),
        list[0]
    );
    svc.lock("wallet.dat").unwrap();
    assert!(!svc.wallets().unwrap()[0].locked);
    assert_eq!(
        svc.addresses("wallet_explicit.dat").unwrap(),
        derive(&other, "", 10)
    );

    // The same seed without a passphrase is the same wallet as the
    // empty-passphrase one.
    assert_eq!(
        svc.import("Dup", secret(PHRASE), None, 10)
            .unwrap_err()
            .code,
        "wallet_exists"
    );
    // New wallets never store an empty passphrase.
    let (created, _) = create(&svc, "New", Some(secret("")), 10);
    assert!(!created.with_passphrase);
    let key = storage::read_key(&svc.directory().join(&created.id)).unwrap();
    assert!(key.finger_print.is_none());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn unreadable_key_files_are_listed_with_an_error() {
    let (_dir, svc) = service(None);
    let (good, _) = create(&svc, "Good", None, 10);
    fs::write(svc.directory().join("wallet_bad.dat"), b"\xffgarbage").unwrap();
    fs::write(svc.directory().join("mmxwallet_big.dat"), vec![b' '; 70000]).unwrap();
    let odd: KeyFile = serde_json::from_value(json!({
        "__type": "mmx.KeyFile", "seed_value": hex(&[9; 32]), "finger_print": "12ab\u{202e}"
    }))
    .unwrap();
    write_key(&svc, "wallet_odd.dat", &odd);
    // Not wallets: other names, directories and links.
    fs::write(svc.directory().join("notes.dat"), b"x").unwrap();
    fs::create_dir(svc.directory().join("wallet_dir.dat")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        svc.directory().join(&good.id),
        svc.directory().join("wallet_link.dat"),
    )
    .unwrap();

    let list = svc.wallets().unwrap();
    let ids: Vec<&str> = list.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "mmxwallet_big.dat",
            &good.id,
            "wallet_bad.dat",
            "wallet_odd.dat"
        ]
    );
    for w in list.iter().filter(|w| w.id != good.id) {
        assert_eq!(w.error.as_deref(), Some("invalid_key_file"), "{}", w.id);
        assert!(w.fingerprint.is_empty() && w.locked);
    }
    assert!(list
        .iter()
        .find(|w| w.id == good.id)
        .unwrap()
        .error
        .is_none());
    assert_eq!(
        svc.unlock("wallet_bad.dat", secret("")).unwrap_err().code,
        "internal"
    );
    assert_eq!(
        svc.addresses("wallet_odd.dat").unwrap_err().code,
        "internal"
    );
    assert_eq!(
        svc.addresses("wallet_link.dat").unwrap_err().code,
        "wallet_not_found"
    );
    assert_eq!(
        svc.addresses("notes.dat").unwrap_err().code,
        "invalid_argument"
    );
    // Unusable files can still be removed (moved aside).
    svc.remove("wallet_bad.dat").unwrap();
    assert_eq!(svc.wallets().unwrap().len(), 3);
}

#[test]
fn wallet_ids_are_validated() {
    let (dir, svc) = service(None);
    let (info, _) = create(&svc, "A", None, 10);
    // A key file outside the wallet directory must not be reachable.
    fs::copy(
        svc.directory().join(&info.id),
        dir.path().join("wallet_outside.dat"),
    )
    .unwrap();
    for (id, code) in [
        ("../wallet_outside.dat", "invalid_argument"),
        ("wallet_/../../wallet_outside.dat", "invalid_argument"),
        ("/etc/passwd", "invalid_argument"),
        ("mmxwallet.json", "invalid_argument"),
        ("", "invalid_argument"),
        ("wallet_outside.dat", "wallet_not_found"),
    ] {
        assert_eq!(svc.addresses(id).unwrap_err().code, code, "{id}");
        assert_eq!(svc.set_active(id).unwrap_err().code, code, "{id}");
        assert_eq!(svc.remove(id).unwrap_err().code, code, "{id}");
    }
    assert!(dir.path().join("wallet_outside.dat").exists());
}

fn sample_state(id: &str) -> AppState {
    AppState {
        active_wallet: Some(id.into()),
        labels: BTreeMap::from([(
            id.to_owned(),
            BTreeMap::from([("0".into(), " Main ".into()), ("99".into(), "Cold".into())]),
        )]),
        contacts: vec![
            Contact {
                id: "c-1".into(),
                name: " Alice ".into(),
                address: "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw".into(),
                note: Some("  rent ".into()),
            },
            Contact {
                id: "c-2".into(),
                name: "Bob".into(),
                address: "mmx17pzl9cgmesyjur7rvvev70fx7f55rvyv7549cvrtf7chjml4qh4sryu9yl".into(),
                note: Some("".into()),
            },
        ],
        settings: Settings {
            theme: "dark".into(),
            auto_lock_minutes: 0,
            fee_level: 5,
            rpc_url: None,
        },
    }
}

#[test]
fn app_state_round_trip_preserves_unknown_keys() {
    let (_dir, svc) = service(None);
    // Defaults with no configuration at all.
    let defaults = svc.app_state().unwrap();
    assert_eq!(defaults, AppState::default());
    assert_eq!(defaults.settings.auto_lock_minutes, 24 * 60);
    assert_eq!(defaults.settings.fee_level, 1);

    let (info, _) = create(&svc, "A", None, 10);
    let path = svc.directory().join(config::FILE);
    let mut config = read_config(&svc);
    config["custom"] = json!({"keep": [1, 2]});
    config["wallets"][&info.id]["color"] = json!("red");
    config["app"] = json!({"window": {"w": 900}});
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();

    svc.set_app_state(sample_state(&info.id)).unwrap();
    let state = svc.app_state().unwrap();
    assert_eq!(state.active_wallet.as_deref(), Some(info.id.as_str()));
    assert_eq!(
        state.labels[&info.id],
        BTreeMap::from([("0".into(), "Main".into()), ("99".into(), "Cold".into())])
    );
    assert_eq!(state.contacts[0].name, "Alice");
    assert_eq!(state.contacts[0].note.as_deref(), Some("rent"));
    assert_eq!(state.contacts[1].note, None);
    assert_eq!(
        state.settings,
        Settings {
            theme: "dark".into(),
            auto_lock_minutes: 0,
            fee_level: 5,
            rpc_url: None
        }
    );
    let stored = read_config(&svc);
    assert_eq!(stored["custom"], json!({"keep": [1, 2]}));
    assert_eq!(stored["wallets"][&info.id]["color"], json!("red"));
    assert_eq!(stored["wallets"][&info.id]["name"], json!("A"));
    assert_eq!(stored["wallets"][&info.id]["labels"]["0"], json!("Main"));
    assert_eq!(stored["app"]["window"], json!({"w": 900}));
    assert_eq!(stored["app"]["theme"], json!("dark"));
    assert_eq!(stored["contacts"][1]["note"], json!(""));
    // The camelCase JSON shape the UI uses.
    assert_eq!(
        serde_json::to_value(&state).unwrap()["settings"],
        json!({"theme": "dark", "autoLockMinutes": 0, "feeLevel": 5})
    );

    // Clearing labels removes them; labels of unknown wallets are ignored.
    let mut next = state.clone();
    next.labels = BTreeMap::from([(
        "wallet_gone.dat".to_owned(),
        BTreeMap::from([("1".into(), "x".into())]),
    )]);
    next.active_wallet = Some("wallet_gone.dat".into());
    svc.set_app_state(next).unwrap();
    let stored = read_config(&svc);
    assert!(stored["wallets"][&info.id].get("labels").is_none());
    assert!(stored["wallets"].get("wallet_gone.dat").is_none());
    assert_eq!(stored["active_wallet"], json!(info.id));
    assert!(svc.app_state().unwrap().labels.is_empty());
}

#[test]
fn app_state_is_validated_before_saving() {
    let (_dir, svc) = service(None);
    let (info, _) = create(&svc, "A", None, 10);
    svc.set_app_state(sample_state(&info.id)).unwrap();
    let before = fs::read(svc.directory().join(config::FILE)).unwrap();
    let zero = NATIVE.to_owned();
    type Change = Box<dyn Fn(&mut AppState)>;
    fn case(f: impl Fn(&mut AppState) + 'static) -> Change {
        Box::new(f)
    }
    let cases: Vec<(&str, Change)> = vec![
        ("theme", case(|s| s.settings.theme = "blue".into())),
        ("auto lock", case(|s| s.settings.auto_lock_minutes = 1441)),
        ("fee level", case(|s| s.settings.fee_level = 4)),
        (
            "label index",
            case(|s| {
                s.labels
                    .values_mut()
                    .next()
                    .unwrap()
                    .insert("100".into(), "x".into());
            }),
        ),
        (
            "label index form",
            case(|s| {
                s.labels
                    .values_mut()
                    .next()
                    .unwrap()
                    .insert("01".into(), "x".into());
            }),
        ),
        (
            "empty label",
            case(|s| {
                s.labels
                    .values_mut()
                    .next()
                    .unwrap()
                    .insert("1".into(), " ".into());
            }),
        ),
        (
            "label wallet id",
            case(|s| {
                s.labels.insert("../x.dat".into(), BTreeMap::new());
            }),
        ),
        (
            "active id",
            case(|s| s.active_wallet = Some("../wallet.dat".into())),
        ),
        (
            "contact address",
            case(|s| s.contacts[0].address = "mmx1nope".into()),
        ),
        (
            "contact uppercase",
            case(|s| s.contacts[0].address = s.contacts[0].address.to_uppercase()),
        ),
        (
            "contact zero",
            case(move |s| s.contacts[0].address = zero.clone()),
        ),
        (
            "duplicate address",
            case(|s| s.contacts[1].address = s.contacts[0].address.clone()),
        ),
        ("duplicate id", case(|s| s.contacts[1].id = "c-1".into())),
        ("contact id", case(|s| s.contacts[1].id = "c 2".into())),
        (
            "contact name",
            case(|s| s.contacts[0].name = "x".repeat(65)),
        ),
        (
            "contact name text",
            case(|s| s.contacts[0].name = "A\u{202e}B".into()),
        ),
        (
            "contact note",
            case(|s| s.contacts[0].note = Some("x".repeat(257))),
        ),
        (
            "contact count",
            case(|s| {
                let c = s.contacts[0].clone();
                s.contacts = (0..1001)
                    .map(|i| Contact {
                        id: format!("c-{i}"),
                        ..c.clone()
                    })
                    .collect();
            }),
        ),
        (
            "server URL",
            case(|s| s.settings.rpc_url = Some("ftp://example.com".into())),
        ),
        (
            "server URL query",
            case(|s| s.settings.rpc_url = Some("https://example.com/?x=1".into())),
        ),
        (
            "server URL user",
            case(|s| s.settings.rpc_url = Some("https://user:pw@example.com".into())),
        ),
    ];
    for (what, change) in cases {
        let mut state = sample_state(&info.id);
        change(&mut state);
        assert_eq!(
            svc.set_app_state(state).unwrap_err().code,
            "invalid_argument",
            "{what}"
        );
    }
    assert_eq!(
        fs::read(svc.directory().join(config::FILE)).unwrap(),
        before
    );
}

#[test]
fn existing_and_damaged_configurations() {
    let (_dir, svc) = service(None);
    let (info, _) = create(&svc, "A", None, 10);
    let path = svc.directory().join(config::FILE);
    // Files written by older versions only name the active wallet.
    fs::write(&path, format!("{{\"active_wallet\":\"{}\"}}", info.id)).unwrap();
    let state = svc.app_state().unwrap();
    assert_eq!(state.active_wallet.as_deref(), Some(info.id.as_str()));
    assert_eq!(state.settings, Settings::default());
    let listed = &svc.wallets().unwrap()[0];
    assert_eq!(listed.name, format!("Wallet {}", info.fingerprint));
    assert_eq!(listed.address_count, 10);
    svc.rename(&info.id, "Main").unwrap();
    assert_eq!(read_config(&svc)["active_wallet"], json!(info.id));

    // A damaged file reads as defaults and is set aside on the next change.
    fs::write(&path, b"{ not json").unwrap();
    assert_eq!(svc.app_state().unwrap().settings, Settings::default());
    assert_eq!(svc.wallets().unwrap().len(), 1);
    svc.rename(&info.id, "Main").unwrap();
    let aside: Vec<_> = fs::read_dir(svc.directory())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("mmxwallet.json.invalid."))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(
        fs::read(svc.directory().join(&aside[0])).unwrap(),
        b"{ not json"
    );
    assert_eq!(
        read_config(&svc)["wallets"][&info.id]["name"],
        json!("Main")
    );
}

#[test]
fn rpc_get_allows_only_chain_reads() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    assert_eq!(svc.rpc(), mock.url);
    let info = svc.rpc_get("/node/info").unwrap();
    assert_eq!(info["height"], json!(1000));
    assert_eq!(
        svc.rpc_get("/address?id=mmx1abc&limit=1000").unwrap(),
        json!({"balances": []})
    );
    assert_eq!(
        svc.rpc_get(&format!("/transaction?id={}", "A".repeat(64)))
            .unwrap(),
        Value::Null
    );
    let before = mock.gets();
    for bad in [
        "/transaction/broadcast",
        "/wallet/keys",
        "/node/info/../../wallet",
        "/address?id=a%2Fb",
        "/address?id=x&url=http://evil",
        "node/info",
        "",
    ] {
        assert_eq!(
            svc.rpc_get(bad).unwrap_err().code,
            "invalid_argument",
            "{bad}"
        );
    }
    assert_eq!(mock.gets(), before);
    assert!(mock.posts("/transaction/broadcast").is_empty());

    mock.respond("/chain/info", 503, "busy");
    assert_eq!(
        svc.rpc_get("/chain/info").unwrap_err().code,
        "rpc_http_error"
    );
    mock.respond("/chain/info", 200, "{\"network\":");
    assert_eq!(
        svc.rpc_get("/chain/info").unwrap_err().code,
        "rpc_response_invalid"
    );
    mock.respond("/chain/info", 302, "");
    assert_eq!(
        svc.rpc_get("/chain/info").unwrap_err().code,
        "rpc_http_error"
    );
    let mut big = b"\"".to_vec();
    big.resize(RPC_GET_LIMIT as usize + 16, b'a');
    big.push(b'"');
    mock.respond("/chain/info", 200, big);
    assert_eq!(
        svc.rpc_get("/chain/info").unwrap_err().code,
        "rpc_response_invalid"
    );
}

#[test]
fn chain_server_is_checked_before_it_is_saved() {
    let mock = Mock::start();
    let (_dir, svc) = service(None);
    assert_eq!(svc.rpc(), "https://rpc.mmx.network");
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    assert_eq!(
        svc.set_rpc(Some(&closed)).unwrap_err().code,
        "rpc_unreachable"
    );
    for bad in [
        "ftp://x",
        "https://",
        "http://a b",
        "https://x/?q",
        "https://u@x",
    ] {
        assert_eq!(
            svc.set_rpc(Some(bad)).unwrap_err().code,
            "invalid_argument",
            "{bad}"
        );
    }
    mock.respond("/node/info", 200, "{\"hello\":1}");
    assert_eq!(
        svc.set_rpc(Some(&mock.url)).unwrap_err().code,
        "rpc_response_invalid"
    );
    assert_eq!(svc.rpc(), "https://rpc.mmx.network");
    assert!(!svc.directory().join(config::FILE).exists());
    mock.with(|s| s.responses.clear());

    assert_eq!(
        svc.set_rpc(Some(&format!("{}/", mock.url))).unwrap(),
        mock.url
    );
    assert_eq!(svc.rpc(), mock.url);
    assert_eq!(read_config(&svc)["rpc"], json!(mock.url));
    assert_eq!(
        svc.app_state().unwrap().settings.rpc_url.as_deref(),
        Some(mock.url.as_str())
    );
    // Reopening keeps the server.
    let reopened = Service::open(Some(svc.directory().to_owned())).unwrap();
    assert_eq!(reopened.rpc(), mock.url);

    // Saving the app state probes only a changed server.
    let probes = mock.gets();
    let mut state = svc.app_state().unwrap();
    svc.set_app_state(state.clone()).unwrap();
    assert_eq!(mock.gets(), probes);
    state.settings.rpc_url = Some(closed.clone());
    assert_eq!(
        svc.set_app_state(state.clone()).unwrap_err().code,
        "rpc_unreachable"
    );
    assert_eq!(svc.rpc(), mock.url);
    state.settings.rpc_url = None;
    svc.set_app_state(state).unwrap();
    assert_eq!(svc.rpc(), "https://rpc.mmx.network");
    assert!(read_config(&svc).get("rpc").is_none());

    svc.set_rpc(Some(&mock.url)).unwrap();
    assert_eq!(svc.set_rpc(None).unwrap(), "https://rpc.mmx.network");
    assert!(read_config(&svc).get("rpc").is_none());
}

#[test]
fn prepare_then_broadcast_exactly_once() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 10);
    let addresses = svc.addresses(&info.id).unwrap();
    mock.set_mmx(&addresses[0], 5_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";

    let fee = fee_one_input(Some("rent"), 1);
    let max = svc.max_send(&info.id, NATIVE, Some("rent"), 1).unwrap();
    assert_eq!(max, (5_000_000 - fee).to_string());
    assert_eq!(
        svc.max_send(&info.id, NATIVE, None, 10).unwrap(),
        (5_000_000 - fee_one_input(None, 10)).to_string()
    );

    let mut request = send_request(&info.id, to, &max);
    request.memo = Some("rent".into());
    let prepared = svc.prepare_send(&request).unwrap();
    assert_eq!(prepared.txid.len(), 64);
    assert_eq!(prepared.txid, prepared.txid.to_ascii_uppercase());
    assert_eq!(prepared.to, to);
    assert_eq!(prepared.contract, NATIVE);
    assert_eq!((prepared.symbol.as_str(), prepared.decimals), ("MMX", 6));
    assert_eq!(prepared.amount, max);
    assert_eq!(prepared.fee, fee.to_string());
    assert_eq!((prepared.height, prepared.expires), (1000, 1100));
    assert_eq!(prepared.memo.as_deref(), Some("rent"));
    // Reviewing sends nothing: the signed transfer stays on this device.
    assert!(mock.post_paths().is_empty());
    assert_eq!(
        serde_json::to_value(&prepared)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [
            "amount", "contract", "decimals", "expires", "fee", "height", "memo", "symbol", "to",
            "txid"
        ]
    );

    svc.broadcast(&info.id, &prepared.txid.to_ascii_lowercase())
        .unwrap();
    let sent = mock.posts("/transaction/broadcast");
    assert_eq!(sent.len(), 1);
    let tx: Value = serde_json::from_slice(&sent[0]).unwrap();
    assert_eq!(tx["outputs"][0]["address"], json!(to));
    assert_eq!(tx["outputs"][0]["amount"], json!(max));
    assert_eq!(tx["outputs"][0]["memo"], json!("rent"));
    assert_eq!(
        tx["id"].as_str().unwrap().to_ascii_uppercase(),
        prepared.txid
    );
    // The fee shown is the one the node charges for a simple transfer.
    assert_eq!(
        u128::from(tx["static_cost"].as_u64().unwrap()) * 1024 / 1024,
        fee
    );
    assert_eq!(
        svc.broadcast(&info.id, &prepared.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert_eq!(
        mock.post_paths(),
        ["/transaction/validate", "/transaction/broadcast"]
    );

    // The broadcast transfer's funds are reserved: everything is spent.
    mock.set_mmx(&addresses[0], 5_000_000);
    assert_eq!(svc.max_send(&info.id, NATIVE, None, 1).unwrap(), "0");
    // Expired below the height: released.
    mock.with(|s| s.height = 1101);
    let max = 5_000_000 - fee_one_input(None, 1);
    assert_eq!(
        svc.max_send(&info.id, NATIVE, None, 1).unwrap(),
        max.to_string()
    );
    // One more than the maximum fails, before anything is sent.
    let over = (max + 1).to_string();
    assert_eq!(
        svc.prepare_send(&send_request(&info.id, to, &over))
            .unwrap_err()
            .code,
        "insufficient_funds"
    );
    assert_eq!(
        svc.prepare_send(&send_request(&info.id, to, "5000001"))
            .unwrap_err()
            .code,
        "insufficient_funds"
    );

    // Discarded and foreign-wallet transfers are gone.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.discard(&p.txid).unwrap();
    svc.discard(&p.txid).unwrap();
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    let (other, _) = create(&svc, "Other", None, 10);
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    assert_eq!(
        svc.broadcast(&other.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert_eq!(svc.discard("xyz").unwrap_err().code, "invalid_argument");
    assert_eq!(
        mock.post_paths(),
        ["/transaction/validate", "/transaction/broadcast"]
    );
}

#[test]
fn only_the_confirmed_transfer_reaches_the_server() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 10);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 5_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    // Review, change the priority, edit and review again, then send.
    let first = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.discard(&first.txid).unwrap();
    let mut faster = send_request(&info.id, to, "1000");
    faster.fee_level = 2;
    let second = svc.prepare_send(&faster).unwrap();
    assert_eq!(second.fee, fee_one_input(None, 2).to_string());
    svc.discard(&second.txid).unwrap();
    let last = svc
        .prepare_send(&send_request(&info.id, to, "2000"))
        .unwrap();
    assert!(mock.post_paths().is_empty());
    svc.broadcast(&info.id, &last.txid).unwrap();
    // Exactly one signed transfer left the device: the confirmed one,
    // checked by the node's dry run first.
    assert_eq!(
        mock.post_paths(),
        ["/transaction/validate", "/transaction/broadcast"]
    );
    assert_eq!(
        mock.posts("/transaction/validate"),
        mock.posts("/transaction/broadcast")
    );
    let sent: Value = serde_json::from_slice(&mock.posts("/transaction/broadcast")[0]).unwrap();
    assert_eq!(sent["id"].as_str().unwrap().to_ascii_uppercase(), last.txid);
}

#[test]
fn refused_and_failed_broadcasts() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let fee = fee_one_input(None, 1);
    let max = || -> u128 {
        svc.max_send(&info.id, NATIVE, None, 1)
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_eq!(max(), 1_000_000 - fee);
    // The node's refusals are definite: nothing is pending, nothing reserved.
    for (body, code) in [
        (
            "insufficient funds for transaction fee: 1300 > 0",
            "insufficient_fee",
        ),
        ("insufficient funds for mmx1abc", "insufficient_funds"),
        ("transaction expired at height 1000", "tx_expired"),
        ("duplicate tx", "tx_rejected"),
    ] {
        let p = svc
            .prepare_send(&send_request(&info.id, to, "1000"))
            .unwrap();
        mock.respond("/transaction/broadcast", 400, body);
        let e = svc.broadcast(&info.id, &p.txid).unwrap_err();
        assert_eq!(e.code, code, "{body}");
        assert_eq!(max(), 1_000_000 - fee, "{body}");
        // Consumed: never sent twice.
        mock.with(|s| s.responses.clear());
        assert_eq!(
            svc.broadcast(&info.id, &p.txid).unwrap_err().code,
            "prepared_not_found"
        );
    }
    assert_eq!(mock.posts("/transaction/broadcast").len(), 4);
    // Transport failures leave the outcome open: the funds stay reserved.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    mock.respond("/transaction/broadcast", 503, "lost sync");
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "rpc_http_error"
    );
    mock.with(|s| s.responses.clear());
    assert_eq!(max(), 1_000_000 - 1000 - fee - fee);
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert_eq!(mock.posts("/transaction/broadcast").len(), 5);
}

#[test]
fn transfers_that_would_fail_are_not_sent() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let fee = fee_one_input(None, 1);
    let max = || -> u128 {
        svc.max_send(&info.id, NATIVE, None, 1)
            .unwrap()
            .parse()
            .unwrap()
    };
    // The node's broadcast would accept these and charge the fee: the dry
    // run stops them, nothing is posted and nothing stays reserved.
    for (body, code) in [
        (
            json!({"did_fail": true, "total_fee": fee.to_string(),
                   "error": {"code": 1_975_784_893u32, "message": "insufficient funds for mmx1"}}),
            "insufficient_funds",
        ),
        (
            json!({"did_fail": true, "total_fee": fee.to_string(),
                   "error": {"code": 5, "message": "failed"}}),
            "tx_rejected",
        ),
        // A fee above the one shown on review.
        (
            json!({"did_fail": false, "total_fee": (fee + 1).to_string()}),
            "tx_not_sent",
        ),
    ] {
        let p = svc
            .prepare_send(&send_request(&info.id, to, "1000"))
            .unwrap();
        mock.respond("/transaction/validate", 200, body.to_string());
        assert_eq!(svc.broadcast(&info.id, &p.txid).unwrap_err().code, code);
        mock.with(|s| s.responses.clear());
        assert_eq!(max(), 1_000_000 - fee, "{code}");
    }
    // An unanswered dry run sends nothing either, and says so.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    mock.respond("/transaction/validate", 503, "busy");
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "tx_not_sent"
    );
    mock.with(|s| s.responses.clear());
    assert_eq!(max(), 1_000_000 - fee);
    assert!(mock.posts("/transaction/broadcast").is_empty());
}

#[test]
fn a_server_on_another_network_keeps_its_own_reservations() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let fee = fee_one_input(None, 1);
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.broadcast(&info.id, &p.txid).unwrap();
    let reserved = |svc: &Service| guard(&svc.reserved).get(&info.id).map_or(0, Vec::len);
    assert_eq!(reserved(&svc), 1);
    // Reservations are tagged with their network and only reduce balances of
    // that network.
    let list = guard(&svc.reserved).get(&info.id).unwrap().clone();
    assert_eq!(list[0].network, "mainnet");
    let mut balances = mmx_wallet::Balances::new();
    let key = (
        svc.addresses(&info.id).unwrap()[0].parse().unwrap(),
        mmx_wallet::Address::default(),
    );
    balances.insert(key, 1_000_000);
    reserve::apply(&mut balances, &list, "testnet10");
    assert_eq!(balances[&key], 1_000_000);
    reserve::apply(&mut balances, &list, "mainnet");
    assert_eq!(balances[&key], 1_000_000 - 1000 - fee);
    // A transfer reviewed for one network is not sent to a server on another.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    mock.with(|s| s.network = "testnet10".into());
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "tx_not_sent"
    );
    assert_eq!(mock.posts("/transaction/broadcast").len(), 1);
    assert_eq!(reserved(&svc), 1);
}

#[test]
fn expired_transfers_are_not_sent() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    // The node runs a transfer at the next height: expires 1100 is too old
    // at height 1100.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    assert_eq!(p.expires, 1100);
    mock.with(|s| s.height = 1100);
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "tx_expired"
    );
    assert!(mock.post_paths().is_empty());
    // A failed height read sends nothing, and says so.
    mock.with(|s| s.height = 1000);
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    mock.respond("/node/info", 500, "busy");
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "tx_not_sent"
    );
    assert!(mock.post_paths().is_empty());
    mock.with(|s| s.responses.clear());
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.broadcast(&info.id, &p.txid).unwrap();
    assert_eq!(
        mock.post_paths(),
        ["/transaction/validate", "/transaction/broadcast"]
    );
    // Time asleep counts: the wall clock expires a review too.
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    guard(&svc.prepared).get_mut(&p.txid).unwrap().created_at -= PREPARED_TTL;
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert_eq!(mock.post_paths().len(), 2);
}

#[test]
fn pending_transfers_reserve_their_funds() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 2);
    let a = svc.addresses(&info.id).unwrap();
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let max = || -> u128 {
        svc.max_send(&info.id, NATIVE, None, 1)
            .unwrap()
            .parse()
            .unwrap()
    };
    let inputs = |tx: &Value| -> Vec<(String, String)> {
        tx["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["address"].as_str().unwrap().to_owned(),
                    i["amount"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let last_sent = || -> Value {
        serde_json::from_slice(mock.posts("/transaction/broadcast").last().unwrap()).unwrap()
    };
    // One address with 10 MMX: send 6, then the rest is what is left.
    mock.set_mmx(&a[0], 10_000_000);
    let fee1 = fee_one_input(None, 1);
    let p1 = svc
        .prepare_send(&send_request(&info.id, to, "6000000"))
        .unwrap();
    svc.broadcast(&info.id, &p1.txid).unwrap();
    assert_eq!(max(), 10_000_000 - 6_000_000 - fee1 - fee1);
    assert_eq!(
        svc.prepare_send(&send_request(&info.id, to, "6000000"))
            .unwrap_err()
            .code,
        "insufficient_funds"
    );
    // Nothing is asked about the transfer before the chain grows.
    assert_eq!(mock.gets_of("/transaction"), 0);

    // In a block: the balances show it, the reservation is released.
    mock.with(|s| {
        s.height = 1001;
        s.transactions.insert(
            p1.txid.to_ascii_lowercase(),
            json!({"id": p1.txid, "height": 1001, "did_fail": false}),
        );
    });
    mock.set_mmx(&a[0], 10_000_000 - 6_000_000 - fee1);
    assert_eq!(max(), 4_000_000 - fee1 - fee1);
    assert_eq!(mock.gets_of("/transaction"), 1);
    assert_eq!(max(), 4_000_000 - fee1 - fee1);
    assert_eq!(mock.gets_of("/transaction"), 1);

    // Two addresses (A = 10, B = 8): the second transfer of 6 is funded by
    // B, since A's remaining funds are reserved.
    mock.set_mmx(&a[0], 10_000_000);
    mock.set_mmx(&a[1], 8_000_000);
    let p2 = svc
        .prepare_send(&send_request(&info.id, to, "6000000"))
        .unwrap();
    svc.broadcast(&info.id, &p2.txid).unwrap();
    let tx2 = last_sent();
    assert_eq!(inputs(&tx2), [(a[0].clone(), "6000000".to_owned())]);
    // A keeps 4, so B (8) pays the fee (two signatures).
    assert_eq!(tx2["sender"], json!(a[1]));
    let fee2 = fee(1, 2, None, 1);
    let p3 = svc
        .prepare_send(&send_request(&info.id, to, "6000000"))
        .unwrap();
    svc.broadcast(&info.id, &p3.txid).unwrap();
    let tx3 = last_sent();
    assert_eq!(inputs(&tx3), [(a[1].clone(), "6000000".to_owned())]);
    // A has 4 left, B 8 - fee - 6: A pays this fee.
    assert_eq!(tx3["sender"], json!(a[0]));
    let left = (4_000_000 - fee2) + (2_000_000 - fee2) - fee(2, 2, None, 1);
    assert_eq!(max(), left);

    // A server that cannot answer keeps the funds reserved; unknown
    // transfers stay reserved until they expire.
    mock.with(|s| s.height = 1002);
    mock.respond("/transaction", 500, "down");
    assert_eq!(max(), left);
    mock.with(|s| {
        s.responses.clear();
        s.height = 1003;
    });
    assert_eq!(max(), left);
    mock.with(|s| s.height = 1102);
    assert_eq!(max(), 18_000_000 - fee(2, 2, None, 1));

    // Removing the wallet forgets its transfers.
    mock.with(|s| s.height = 1000);
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.broadcast(&info.id, &p.txid).unwrap();
    assert!(guard(&svc.reserved).contains_key(&info.id));
    svc.remove(&info.id).unwrap();
    assert!(guard(&svc.reserved).is_empty());
}

#[test]
fn send_requests_are_validated_before_network_use() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 10);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let before = mock.gets();
    let cases: Vec<(SendRequest, &str)> = vec![
        (send_request(&info.id, NATIVE, "1"), "invalid_address"),
        (
            send_request(&info.id, &to.to_uppercase(), "1"),
            "invalid_address",
        ),
        (send_request(&info.id, "MMX", "1"), "invalid_address"),
        (send_request(&info.id, &to[..61], "1"), "invalid_address"),
        (send_request(&info.id, to, "0"), "invalid_amount"),
        (send_request(&info.id, to, "1.5"), "invalid_amount"),
        (send_request(&info.id, to, "-1"), "invalid_amount"),
        (send_request(&info.id, to, "1e6"), "invalid_amount"),
        (
            SendRequest {
                memo: Some("é".repeat(33)),
                ..send_request(&info.id, to, "1")
            },
            "invalid_memo",
        ),
        (
            SendRequest {
                memo: Some("line\nbreak".into()),
                ..send_request(&info.id, to, "1")
            },
            "invalid_memo",
        ),
        (
            SendRequest {
                fee_level: 4,
                ..send_request(&info.id, to, "1")
            },
            "invalid_argument",
        ),
        (
            SendRequest {
                fee_level: 0,
                ..send_request(&info.id, to, "1")
            },
            "invalid_argument",
        ),
        (
            SendRequest {
                contract: "MMX".into(),
                ..send_request(&info.id, to, "1")
            },
            "invalid_argument",
        ),
        (send_request("../wallet.dat", to, "1"), "invalid_argument"),
        (send_request("wallet_none.dat", to, "1"), "wallet_not_found"),
    ];
    for (request, code) in cases {
        assert_eq!(
            svc.prepare_send(&request).unwrap_err().code,
            code,
            "{request:?}"
        );
    }
    assert_eq!(
        svc.max_send(&info.id, "MMX", None, 1).unwrap_err().code,
        "invalid_argument"
    );
    assert_eq!(
        svc.max_send(&info.id, NATIVE, Some(&"x".repeat(65)), 1)
            .unwrap_err()
            .code,
        "invalid_memo"
    );
    assert_eq!(svc.max_send(&info.id, NATIVE, None, 3).unwrap(), "0");
    assert_eq!(mock.gets(), before + 1 + 1 + 10);
    // An empty memo is no memo.
    let request = SendRequest {
        memo: Some(String::new()),
        ..send_request(&info.id, to, "1")
    };
    assert_eq!(
        svc.prepare_send(&request).unwrap_err().code,
        "insufficient_funds"
    );
}

#[test]
fn prepared_transfers_expire() {
    let mock = Mock::start();
    let (_dir, mut svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    svc.prepared_ttl = Duration::ZERO;
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert!(mock.posts("/transaction/broadcast").is_empty());
    svc.prepared_ttl = PREPARED_TTL;
    // At most MAX_PREPARED are kept; the oldest go first.
    let first = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    for _ in 0..MAX_PREPARED {
        svc.prepare_send(&send_request(&info.id, to, "1000"))
            .unwrap();
    }
    assert_eq!(guard(&svc.prepared).len(), MAX_PREPARED);
    assert_eq!(
        svc.broadcast(&info.id, &first.txid).unwrap_err().code,
        "prepared_not_found"
    );
}

#[test]
fn locking_drops_prepared_transfers() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", Some(secret("pw")), 10);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[3], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.lock(&info.id).unwrap();
    svc.unlock(&info.id, secret("pw")).unwrap();
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.lock_all();
    svc.unlock(&info.id, secret("pw")).unwrap();
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    let p = svc
        .prepare_send(&send_request(&info.id, to, "1000"))
        .unwrap();
    svc.remove(&info.id).unwrap();
    assert_eq!(
        svc.broadcast(&info.id, &p.txid).unwrap_err().code,
        "prepared_not_found"
    );
    assert!(mock.posts("/transaction/broadcast").is_empty());
}

#[test]
fn locking_during_review_preparation_keeps_nothing() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", Some(secret("pw")), 1);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    mock.with(|s| {
        s.delays
            .insert("/address".into(), Duration::from_millis(300))
    });
    let result = thread::scope(|scope| {
        let pending = scope.spawn(|| svc.prepare_send(&send_request(&info.id, to, "1000")));
        while mock.gets_of("/address") == 0 {
            thread::sleep(Duration::from_millis(5));
        }
        svc.lock(&info.id).unwrap();
        pending.join().unwrap()
    });
    assert_eq!(result.unwrap_err().code, "locked");
    assert!(guard(&svc.prepared).is_empty());
    assert!(mock.post_paths().is_empty());
}

#[test]
fn max_send_and_fees_with_tokens_and_several_addresses() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 10);
    let a = svc.addresses(&info.id).unwrap();
    let token = "mmx17pzl9cgmesyjur7rvvev70fx7f55rvyv7549cvrtf7chjml4qh4sryu9yl";
    let token_row = |amount: &str| json!({"contract": token, "amount": amount, "decimals": 2, "symbol": "TOK\u{202e}EN"});
    // Tokens only: the fee cannot be paid.
    mock.with(|s| s.balances.insert(a[1].clone(), vec![token_row("500")]));
    assert_eq!(
        svc.max_send(&info.id, token, None, 1).unwrap_err().code,
        "insufficient_fee"
    );
    let mut request = send_request(&info.id, &a[2], "500");
    request.contract = token.into();
    assert_eq!(
        svc.prepare_send(&request).unwrap_err().code,
        "insufficient_fee"
    );
    // With MMX on another address the whole token balance can be sent.
    mock.set_mmx(&a[4], 3_000_000);
    assert_eq!(svc.max_send(&info.id, token, None, 1).unwrap(), "500");
    let prepared = svc.prepare_send(&request).unwrap();
    assert_eq!((prepared.symbol.as_str(), prepared.decimals), ("TOKEN", 2));
    assert_eq!(prepared.contract, token);
    request.amount = "501".into();
    assert_eq!(
        svc.prepare_send(&request).unwrap_err().code,
        "insufficient_funds"
    );
    // A token the wallet does not hold.
    let other = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    assert_eq!(svc.max_send(&info.id, other, None, 1).unwrap(), "0");
    request.contract = other.into();
    request.amount = "1".into();
    assert_eq!(
        svc.prepare_send(&request).unwrap_err().code,
        "insufficient_funds"
    );

    // MMX on several addresses: the maximum is accepted, one more is not.
    mock.set_mmx(&a[7], 1_000_000);
    mock.set_mmx(&a[9], 2_000);
    // a[4] is spent first; a[7] pays the fee (a[9] holds less than the fee).
    let max: u128 = svc
        .max_send(&info.id, NATIVE, None, 2)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(max, 4_000_000 - fee(2, 2, None, 2));
    let mut request = send_request(&info.id, other, &max.to_string());
    request.fee_level = 2;
    svc.prepare_send(&request).unwrap();
    request.amount = (max + 1).to_string();
    assert_eq!(
        svc.prepare_send(&request).unwrap_err().code,
        "insufficient_funds"
    );

    // Any failed balance request fails the whole operation.
    mock.respond("/address", 500, "down");
    assert_eq!(
        svc.max_send(&info.id, NATIVE, None, 1).unwrap_err().code,
        "rpc_http_error"
    );
}

#[test]
fn unsynced_or_foreign_servers_are_refused() {
    let mock = Mock::start();
    let (_dir, svc) = service(Some(&mock));
    let (info, _) = create(&svc, "Main", None, 10);
    mock.set_mmx(&svc.addresses(&info.id).unwrap()[0], 1_000_000);
    let to = "mmx1hzz9tgs2dz9366t3p4ep8trmaejx7tk9al9ah3md2u37pkesa3qqfyepyw";
    mock.with(|s| s.synced = false);
    assert_eq!(
        svc.max_send(&info.id, NATIVE, None, 1).unwrap_err().code,
        "rpc_not_synced"
    );
    assert_eq!(
        svc.prepare_send(&send_request(&info.id, to, "1"))
            .unwrap_err()
            .code,
        "rpc_not_synced"
    );
    mock.with(|s| {
        s.synced = true;
        s.network = "testnet10".into();
    });
    assert_eq!(
        svc.max_send(&info.id, NATIVE, None, 1).unwrap_err().code,
        "rpc_network_mismatch"
    );
    assert_eq!(
        svc.prepare_send(&send_request(&info.id, to, "1"))
            .unwrap_err()
            .code,
        "rpc_network_mismatch"
    );
    assert!(mock.post_paths().is_empty());
    // Not synced is not a reason to refuse a server in settings.
    mock.with(|s| s.synced = false);
    svc.set_rpc(Some(&mock.url)).unwrap();
}

#[test]
fn serialized_shapes_match_the_ui_contract() {
    let info = WalletInfo {
        id: "wallet.dat".into(),
        name: "W".into(),
        fingerprint: "1".into(),
        with_passphrase: true,
        locked: false,
        address_count: 10,
        error: None,
    };
    assert_eq!(
        serde_json::to_value(&info).unwrap(),
        json!({"id": "wallet.dat", "name": "W", "fingerprint": "1", "withPassphrase": true,
               "locked": false, "addressCount": 10})
    );
    let request: SendRequest = serde_json::from_value(json!({
        "walletId": "wallet.dat", "to": "mmx1", "contract": NATIVE, "amount": "5", "feeLevel": 2
    }))
    .unwrap();
    assert_eq!(request.memo, None);
    let state: AppState = serde_json::from_value(json!({
        "labels": {"wallet.dat": {"3": "x"}},
        "contacts": [{"id": "c-1", "name": "A", "address": NATIVE}],
        "settings": {"theme": "light", "autoLockMinutes": 5, "feeLevel": 1, "rpcUrl": "https://x"}
    }))
    .unwrap();
    assert_eq!(state.settings.rpc_url.as_deref(), Some("https://x"));
    assert_eq!(
        serde_json::to_value(Error::new("locked", "wallet is locked")).unwrap(),
        json!({"code": "locked", "message": "wallet is locked"})
    );
}

#[test]
fn the_all_zero_phrase_is_not_imported() {
    // Valid words with an all-zero seed: such a key file is rejected when
    // read, so the wallet would be unusable.
    let (_dir, svc) = service(None);
    let zero = format!("{}art", "abandon ".repeat(23));
    for passphrase in [None, Some(secret(PASS))] {
        let e = svc
            .import("Zero", secret(&zero), passphrase, 10)
            .unwrap_err();
        assert_eq!(e.code, "invalid_mnemonic");
    }
    assert!(svc.wallets().unwrap().is_empty());
}

#[test]
fn new_wallets_are_saved_only_from_confirmed_words() {
    let (_dir, svc) = service(None);
    let words = Service::new_mnemonic();
    assert_eq!(words.split(' ').count(), 24);
    assert!(words_to_seed(&words).is_ok());
    assert_ne!(*words, *Service::new_mnemonic());
    // New words store nothing.
    assert!(svc.wallets().unwrap().is_empty());
    assert!(!svc.directory().exists());

    let info = svc.create(" Main ", words.clone(), None, 10).unwrap();
    assert_eq!(info.name, "Main");
    assert_eq!(info.id, format!("wallet_{}.dat", info.fingerprint));
    assert!(!info.with_passphrase && !info.locked);
    assert_eq!(
        svc.app_state().unwrap().active_wallet.as_deref(),
        Some(info.id.as_str())
    );
    let key = storage::read_key(&svc.directory().join(&info.id)).unwrap();
    assert_eq!(seed_to_words(&key.seed_value), *words);
    assert_eq!(*svc.mnemonic(&info.id).unwrap(), *words);
    // The same words again are the same wallet.
    assert_eq!(
        svc.create("Again", words.clone(), None, 10)
            .unwrap_err()
            .code,
        "wallet_exists"
    );
    // With a passphrase they are another, protected wallet.
    let vault = svc
        .create("Vault", words.clone(), Some(secret("pw")), 5)
        .unwrap();
    assert!(vault.with_passphrase && !vault.locked);
    assert_eq!(vault.address_count, 5);
    assert_eq!(
        svc.addresses(&vault.id).unwrap(),
        derive(&key.seed_value, "pw", 5)
    );
    assert_eq!(
        svc.create("Vault", words.clone(), Some(secret("pw")), 5)
            .unwrap_err()
            .code,
        "wallet_exists"
    );
    for bad in ["", "abandon", "never-echo-this-secret"] {
        let e = svc.create("X", secret(bad), None, 10).unwrap_err();
        assert_eq!(e.code, "invalid_mnemonic", "{bad}");
        assert!(!e.message.contains("never-echo"));
    }
    for (name, count) in [("", 10), ("X", 0), ("X", 101)] {
        assert_eq!(
            svc.create(name, Service::new_mnemonic(), None, count)
                .unwrap_err()
                .code,
            "invalid_argument"
        );
    }
    assert_eq!(svc.wallets().unwrap().len(), 2);
}

#[test]
fn saving_app_state_keeps_the_active_wallet_chosen_elsewhere() {
    let (_dir, svc) = service(None);
    let (a, _) = create(&svc, "A", None, 10);
    let (b, _) = create(&svc, "B", None, 10);
    svc.set_active(&a.id).unwrap();
    let mut stale = svc.app_state().unwrap();
    assert_eq!(stale.active_wallet.as_deref(), Some(a.id.as_str()));
    // `mmxwallet use` switches wallets while the app is open.
    storage::set_active(svc.directory(), &svc.directory().join(&b.id)).unwrap();
    stale.settings.theme = "dark".into();
    svc.set_app_state(stale).unwrap();
    assert_eq!(read_config(&svc)["active_wallet"], json!(b.id));
    let state = svc.app_state().unwrap();
    assert_eq!(state.active_wallet.as_deref(), Some(b.id.as_str()));
    assert_eq!(state.settings.theme, "dark");
}

#[test]
fn token_symbols_fall_back_to_the_contract() {
    let contract: Address = NATIVE.parse().unwrap();
    assert_eq!(display_symbol(" USDM ", &contract), "USDM");
    assert_eq!(display_symbol("\u{202e}X\u{0007}", &contract), "X");
    assert_eq!(
        display_symbol(" \u{202e} ", &contract),
        "mmx1qqqq\u{2026}ytev"
    );
    assert_eq!(display_symbol("", &contract), "mmx1qqqq\u{2026}ytev");
}

//! Desktop wallet backend.
//!
//! Wallets are the key files in the wallet directory (the same files, format
//! and `mmxwallet.json` as the CLI and the node GUI); chain data comes from
//! the configured chain server. Every value received from the UI is
//! validated here. The service is blocking, `Send + Sync`, safe to call
//! concurrently, and never holds a lock during a network request.
//!
//! Errors carry the stable codes of the wallet UI (`invalid_passphrase`,
//! `locked`, `rpc_unreachable`, …); codes the CLI reports differently are
//! translated at this boundary (see [`service_error`]).
use crate::{
    config::{self, Config, Loaded},
    remote,
    rpc::{self, Rpc},
    storage, validate,
};
use mmx_wallet::{
    fingerprint, seed_to_words, words_to_seed, Address, ChainParams, KeyFile, Output, Transaction,
    TransferOptions, Wallet,
};
use reserve::Reservation;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};
use zeroize::Zeroizing;

pub use crate::error::{Error, Result};

mod reserve;

/// Prepared transfers can be broadcast for this long after review.
const PREPARED_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PREPARED: usize = 32;
/// Blocks until a prepared transfer can no longer be included.
const EXPIRE_DELTA: u32 = 100;
const RPC_GET_LIMIT: u64 = 8 * 1024 * 1024;
const RPC_URL_MAX: usize = 512;
const MAX_CONTACTS: usize = 1000;
/// Wallets with labels in one app state (labels of unlisted wallets are dropped).
const MAX_LABELLED_WALLETS: usize = 1000;
const MAX_AUTO_LOCK_MINUTES: u32 = 24 * 60;
const DEFAULT_AUTO_LOCK_MINUTES: u32 = 5;
const DEFAULT_FEE_LEVEL: u32 = 1;
const MAX_MNEMONIC_INPUT: usize = 4096;
const SYMBOL_MAX_CHARS: usize = 24;
const THEMES: [&str; 3] = ["system", "light", "dark"];

/// The error codes of the wallet UI.
pub const ERROR_CODES: [&str; 25] = [
    "invalid_passphrase",
    "passphrase_required",
    "invalid_mnemonic",
    "wallet_mismatch",
    "wallet_exists",
    "wallet_not_found",
    "locked",
    "invalid_address",
    "invalid_amount",
    "invalid_memo",
    "insufficient_funds",
    "insufficient_fee",
    "invalid_argument",
    "rpc_unreachable",
    "rpc_timeout",
    "rpc_not_synced",
    "rpc_network_mismatch",
    "rpc_http_error",
    "rpc_response_invalid",
    "tx_rejected",
    "tx_expired",
    "tx_not_sent",
    "prepared_not_found",
    "io_error",
    "internal",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletInfo {
    /// Key file name in the wallet directory.
    pub id: String,
    pub name: String,
    /// Decimal wallet fingerprint ("Wallet ID"); empty when `error` is set.
    pub fingerprint: String,
    /// A (non-empty) passphrase is needed to open the wallet.
    pub with_passphrase: bool,
    pub locked: bool,
    pub can_view: bool,
    pub address_count: u32,
    /// `invalid_key_file` or `io_error` when the key file cannot be used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// "system", "light" or "dark".
    pub theme: String,
    /// 0 = never.
    pub auto_lock_minutes: u32,
    /// 1, 2, 3, 5 or 10.
    pub fee_level: u32,
    /// Chain server; none = the public default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_url: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            auto_lock_minutes: DEFAULT_AUTO_LOCK_MINUTES,
            fee_level: DEFAULT_FEE_LEVEL,
            rpc_url: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_wallet: Option<String>,
    /// Address labels: wallet id → address index ("0".."99") → label.
    #[serde(default)]
    pub labels: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub contacts: Vec<Contact>,
    #[serde(default)]
    pub settings: Settings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub wallet_id: String,
    pub to: String,
    /// Currency contract; the zero address is native MMX.
    pub contract: String,
    /// Atomic units, decimal.
    pub amount: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    pub fee_level: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedSend {
    /// Transaction ID, uppercase hexadecimal.
    pub txid: String,
    pub to: String,
    pub contract: String,
    pub symbol: String,
    pub decimals: u32,
    /// Atomic units, decimal.
    pub amount: String,
    /// Network fee in atomic MMX, decimal.
    pub fee: String,
    /// Last block height that can include the transfer.
    pub expires: u32,
    /// Chain height the transfer was prepared at.
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protection {
    None,
    /// Created with an empty passphrase (C++ wallets): opens without asking.
    Empty,
    Passphrase,
}

struct Prepared {
    wallet: String,
    tx: Transaction,
    /// The signed transfer as broadcast.
    bytes: Vec<u8>,
    params: ChainParams,
    created: Instant,
    /// Wall-clock time, which (unlike `created`) also counts time asleep.
    created_at: SystemTime,
}

pub struct Service {
    dir: PathBuf,
    /// Serializes changes to key files and `mmxwallet.json` (local disk only).
    files: Mutex<()>,
    /// Derived signing keys for unlocked passphrase wallets. No passphrase is retained.
    unlocked: Mutex<HashMap<String, std::sync::Arc<Wallet>>>,
    session_epoch: std::sync::atomic::AtomicU64,
    /// Reviewed, signed transfers by uppercase transaction ID.
    prepared: Mutex<HashMap<String, Prepared>>,
    /// Funds of broadcast transfers not in a block yet, by wallet id.
    reserved: Mutex<HashMap<String, Vec<Reservation>>>,
    rpc: Mutex<Arc<Rpc>>,
    prepared_ttl: Duration,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn run<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    f().map_err(service_error)
}

/// Maps CLI error codes to the wallet UI's codes.
pub fn service_error(e: Error) -> Error {
    let code = match e.code {
        "rpc_transport_error" => "rpc_unreachable",
        "transaction_expired" => "tx_expired",
        "wallet_directory_unavailable" => "io_error",
        code if ERROR_CODES.contains(&code) => code,
        _ => "internal",
    };
    Error {
        code,
        message: e.message,
    }
}

fn not_found() -> Error {
    Error::new("wallet_not_found", "wallet not found")
}
fn locked() -> Error {
    Error::new("locked", "wallet is locked")
}
fn insufficient_funds() -> Error {
    Error::new(
        "insufficient_funds",
        "not enough funds for the amount and fee",
    )
}
fn insufficient_fee() -> Error {
    Error::new("insufficient_fee", "not enough MMX to pay the network fee")
}

/// The stored fingerprint and how the wallet is opened (§6.3): no
/// passphrase, an empty passphrase (opens silently) or a real passphrase.
fn detect(key: &KeyFile) -> Result<(String, Protection)> {
    let seed = &key.seed_value;
    let Some(stored) = key.finger_print.as_deref() else {
        return Ok((fingerprint(seed, None), Protection::None));
    };
    // Fingerprints are decimal u32 values; anything else can never match.
    if stored.is_empty()
        || stored.len() > 10
        || !stored.bytes().all(|b| b.is_ascii_digit())
        || stored.parse::<u32>().map(|n| n.to_string()).as_deref() != Ok(stored)
    {
        return Err(Error::new("invalid_key_file", "invalid wallet fingerprint"));
    }
    let protection = if stored == fingerprint(seed, None) {
        Protection::None
    } else if stored == fingerprint(seed, Some("")) {
        Protection::Empty
    } else {
        Protection::Passphrase
    };
    Ok((stored.to_owned(), protection))
}

/// "Wallet" for `wallet.dat`, "Wallet <x>" for `wallet_<x>.dat`.
fn default_name(id: &str) -> String {
    let stem = id.strip_suffix(".dat").unwrap_or(id);
    stem.strip_prefix("mmxwallet_")
        .or_else(|| stem.strip_prefix("wallet_"))
        .filter(|rest| !rest.is_empty())
        .and_then(|rest| validate::name(&format!("Wallet {rest}")).ok())
        .unwrap_or_else(|| "Wallet".into())
}

/// Lowercase words separated by single spaces. BIP39 English words are
/// ASCII; the buffer never grows, so no copy of the words is left behind.
fn normalize_mnemonic(input: &str) -> Result<Zeroizing<String>> {
    let bad = || Error::new("invalid_mnemonic", "invalid recovery words");
    if input.len() > MAX_MNEMONIC_INPUT {
        return Err(bad());
    }
    let mut words = Zeroizing::new(String::with_capacity(input.len()));
    for word in input.split_whitespace() {
        if !word.is_ascii() {
            return Err(bad());
        }
        if !words.is_empty() {
            words.push(' ');
        }
        for c in word.chars() {
            words.push(c.to_ascii_lowercase());
        }
    }
    Ok(words)
}

/// RPC text shown to the user: printable, trimmed and short.
fn display_symbol(symbol: &str, contract: &Address) -> String {
    let clean = symbol
        .chars()
        .filter(|c| !validate::is_unsafe_char(*c))
        .take(SYMBOL_MAX_CHARS)
        .collect::<String>()
        .trim()
        .to_owned();
    if !clean.is_empty() {
        return clean;
    }
    // A symbol that is empty after cleaning is valid on chain; show the
    // shortened contract instead, as the UI's asset list does.
    let contract = contract.to_string();
    match (
        contract.get(..8),
        contract.get(contract.len().saturating_sub(4)..),
    ) {
        (Some(head), Some(tail)) if contract.len() > 13 => format!("{head}\u{2026}{tail}"),
        _ => contract,
    }
}

/// A chain server URL accepted in settings (HTTP(S), host, optional port
/// and path; no credentials, query or fragment), normalized.
fn rpc_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty()
        || url.len() > RPC_URL_MAX
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || url.contains(['?', '#', '@'])
        || url.trim_end_matches('/').ends_with(':')
    {
        return Err(Error::new("invalid_argument", "invalid server URL"));
    }
    Rpc::normalize(url)
}

fn rpc_client(url: &str) -> Result<Rpc> {
    Rpc::new(&rpc_url(url)?)
}

fn default_rpc() -> Result<Rpc> {
    Rpc::new(rpc::DEFAULT_URL)
}

/// Checks that the server answers like an MMX node.
fn probe(rpc: &Rpc) -> Result<()> {
    let info = rpc.get("/node/info")?;
    rpc::text(&info, "name")?;
    rpc::u32_field(&info, "height")?;
    rpc::boolean(&info, "is_synced")?;
    Ok(())
}

fn settings(config: &Config) -> Settings {
    let mut settings = Settings::default();
    let app = config.get("app").and_then(Value::as_object);
    let get = |key: &str| app.and_then(|a| a.get(key));
    if let Some(theme) = get("theme")
        .and_then(Value::as_str)
        .filter(|t| THEMES.contains(t))
    {
        settings.theme = theme.into();
    }
    if let Some(minutes) = get("auto_lock_minutes")
        .and_then(Value::as_u64)
        .filter(|m| *m <= u64::from(MAX_AUTO_LOCK_MINUTES))
    {
        settings.auto_lock_minutes = minutes as u32;
    }
    if let Some(level) = get("fee_level")
        .and_then(Value::as_u64)
        .and_then(|l| u32::try_from(l).ok())
        .filter(|l| validate::FEE_LEVELS.contains(l))
    {
        settings.fee_level = level;
    }
    settings.rpc_url = config::rpc(config).and_then(|url| rpc_url(url).ok());
    settings
}

fn contact(id: &str, name: &str, address: &str, note: Option<&str>) -> Result<Contact> {
    let id = validate::contact_id(id)?.to_owned();
    let name = validate::name(name)?;
    let address = validate::canonical_address(address)
        .filter(|a| *a != Address::default())
        .ok_or_else(|| Error::new("invalid_argument", "invalid contact address"))?
        .to_string();
    let note = validate::note(note.unwrap_or(""))?;
    Ok(Contact {
        id,
        name,
        address,
        note,
    })
}

/// Stored contacts; invalid and duplicate entries are skipped.
fn contacts(config: &Config) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    let mut addresses = HashSet::new();
    for value in config
        .get("contacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if out.len() >= MAX_CONTACTS {
            break;
        }
        let text = |key: &str| value.get(key).and_then(Value::as_str);
        let (Some(id), Some(name), Some(address)) = (text("id"), text("name"), text("address"))
        else {
            continue;
        };
        if let Ok(c) = contact(id, name, address, text("note")) {
            if ids.insert(c.id.clone()) && addresses.insert(c.address.clone()) {
                out.push(c);
            }
        }
    }
    out
}

fn validate_contacts(input: &[Contact]) -> Result<Vec<Contact>> {
    if input.len() > MAX_CONTACTS {
        return Err(Error::new("invalid_argument", "too many contacts"));
    }
    let mut ids = HashSet::new();
    let mut addresses = HashSet::new();
    let mut out = Vec::with_capacity(input.len());
    for c in input {
        let c = contact(&c.id, &c.name, &c.address, c.note.as_deref())?;
        if !ids.insert(c.id.clone()) || !addresses.insert(c.address.clone()) {
            return Err(Error::new("invalid_argument", "duplicate contact"));
        }
        out.push(c);
    }
    Ok(out)
}

impl Service {
    /// Opens the wallet directory (default: `$MMX_HOME` or `~/.mmx`). Nothing
    /// is created until the first change.
    pub fn open(dir: Option<PathBuf>) -> Result<Self> {
        run(|| {
            let dir = match dir {
                Some(dir) => dir,
                None => storage::directory()?,
            };
            let rpc = match config::rpc(&config::read_lenient(&dir)).map(rpc_client) {
                Some(Ok(rpc)) => rpc,
                _ => default_rpc()?,
            };
            Ok(Self {
                dir,
                files: Mutex::new(()),
                unlocked: Mutex::new(HashMap::new()),
                session_epoch: std::sync::atomic::AtomicU64::new(0),
                prepared: Mutex::new(HashMap::new()),
                reserved: Mutex::new(HashMap::new()),
                rpc: Mutex::new(Arc::new(rpc)),
                prepared_ttl: PREPARED_TTL,
            })
        })
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }

    /// All key files in the wallet directory, sorted by id. A file that
    /// cannot be used is listed with `error` set.
    pub fn wallets(&self) -> Result<Vec<WalletInfo>> {
        run(|| {
            let config = config::read_lenient(&self.dir);
            let unlocked: HashSet<String> = guard(&self.unlocked).keys().cloned().collect();
            Ok(storage::wallet_files(&self.dir)?
                .into_iter()
                .map(|(id, path)| {
                    let key = storage::read_key(&path);
                    let unlocked = unlocked.contains(&id);
                    describe(&config, id, key.and_then(|k| detect(&k)), unlocked)
                })
                .collect())
        })
    }

    /// 24 recovery words of a new random seed (OS RNG). Nothing is stored:
    /// the wallet is saved by [`Service::create`] once its backup is confirmed.
    pub fn new_mnemonic() -> Zeroizing<String> {
        Zeroizing::new(seed_to_words(&KeyFile::generate(None).seed_value))
    }

    /// Saves a new wallet from the words of [`Service::new_mnemonic`] after
    /// the user confirmed their backup, and makes it active. An empty
    /// passphrase means none.
    pub fn create(
        &self,
        name: &str,
        mnemonic: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
        count: u32,
    ) -> Result<WalletInfo> {
        run(|| self.add_words(name, mnemonic, passphrase, count, true))
    }

    /// Adds a wallet from its 24 recovery words and makes it active.
    pub fn import(
        &self,
        name: &str,
        mnemonic: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
        count: u32,
    ) -> Result<WalletInfo> {
        run(|| self.add_words(name, mnemonic, passphrase, count, false))
    }

    /// Saves a wallet from recovery words, unless the same wallet exists.
    fn add_words(
        &self,
        name: &str,
        mnemonic: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
        count: u32,
        new: bool,
    ) -> Result<WalletInfo> {
        let name = validate::name(name)?;
        let count = validate::num_addresses(count)?;
        let words = normalize_mnemonic(&mnemonic)?;
        drop(mnemonic);
        let seed = Zeroizing::new(words_to_seed(&words)?);
        drop(words);
        let passphrase = passphrase.filter(|p| !p.is_empty());
        let key = KeyFile::new(*seed, passphrase.as_deref().map(String::as_str));
        let (fingerprint, protection) = detect(&key)?;
        // A passphrase whose fingerprint equals the no-passphrase (or
        // empty-passphrase) one would be taken for none when the wallet is
        // opened (about 1 in 2^31). Existing wallets are kept as they are;
        // a new one gets another passphrase.
        if new && passphrase.is_some() && protection != Protection::Passphrase {
            return Err(Error::new(
                "invalid_argument",
                "this passphrase cannot be used; choose another one",
            ));
        }
        let _files = guard(&self.files);
        // The same seed opened the same way is the same wallet, even
        // under another file name.
        for (_, path) in storage::wallet_files(&self.dir)? {
            let Ok(existing) = storage::read_key(&path) else {
                continue;
            };
            if existing.seed_value != key.seed_value {
                continue;
            }
            let same = match (protection, existing.finger_print.as_deref()) {
                (Protection::Passphrase, stored) => stored == Some(fingerprint.as_str()),
                _ => matches!(
                    detect(&existing),
                    Ok((_, Protection::None | Protection::Empty))
                ),
            };
            if same {
                return Err(Error::new("wallet_exists", "wallet already exists"));
            }
        }
        self.add(name, count, &key, fingerprint, protection, passphrase)
    }

    /// Saves a new key file and its settings. The caller holds `files`.
    fn add(
        &self,
        name: String,
        count: u32,
        key: &KeyFile,
        fingerprint: String,
        protection: Protection,
        passphrase: Option<Zeroizing<String>>,
    ) -> Result<WalletInfo> {
        let epoch = self
            .session_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        let id = format!("wallet_{fingerprint}.dat");
        let derived = key.unlock(
            passphrase.as_deref().map(String::as_str).unwrap_or(""),
            0,
            count,
        )?;
        let public = derived.addresses.clone();
        let signing = std::sync::Arc::new(derived);
        drop(passphrase);
        storage::save_key(&self.dir.join(&id), key)?;
        // The key file is the wallet; its settings fall back to defaults if
        // they cannot be read. Cache writes must succeed before starting a session.
        if let Ok(mut config) = self.config_for_write() {
            config::set_wallet_name(&mut config, &id, &name);
            config::set_num_addresses(&mut config, &id, count);
            config::set_cached_addresses(&mut config, &id, &fingerprint, &public);
            config::set_active(&mut config, Some(&id));
            config::save(&self.dir, config)?;
        }
        if protection == Protection::Passphrase {
            let mut unlocked = guard(&self.unlocked);
            if epoch
                != self
                    .session_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(locked());
            }
            unlocked.insert(id.clone(), signing);
        }
        Ok(WalletInfo {
            id,
            name,
            fingerprint,
            with_passphrase: protection == Protection::Passphrase,
            locked: false,
            can_view: true,
            address_count: count,
            error: None,
        })
    }

    /// Opens a wallet that needs a passphrase. Wallets without one (or with
    /// an empty one) are always open and ignore `passphrase`.
    pub fn unlock(&self, id: &str, passphrase: Zeroizing<String>) -> Result<WalletInfo> {
        let epoch = self
            .session_epoch
            .load(std::sync::atomic::Ordering::Acquire);
        run(|| {
            let (id, path) = self.wallet_file(id)?;
            let key = storage::read_key(&path)?;
            let (fingerprint, protection) = detect(&key)?;
            if protection == Protection::Passphrase {
                if passphrase.is_empty() {
                    return Err(Error::new("passphrase_required", "passphrase required"));
                }
                if mmx_wallet::fingerprint(&key.seed_value, Some(passphrase.as_str()))
                    != fingerprint
                {
                    return Err(Error::new("invalid_passphrase", "wrong passphrase"));
                }
                let count = self.address_count(&id)?;
                let wallet = key.unlock(&passphrase, 0, count)?;
                let addresses = wallet.addresses.clone();
                let signing = std::sync::Arc::new(wallet);
                let _files = guard(&self.files);
                let mut config = self.config_for_write()?;
                config::set_cached_addresses(&mut config, &id, &fingerprint, &addresses);
                config::save(&self.dir, config)?;
                let mut unlocked = guard(&self.unlocked);
                if epoch
                    != self
                        .session_epoch
                        .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(locked());
                }
                unlocked.insert(id.clone(), signing);
            }
            let config = config::read_lenient(&self.dir);
            Ok(describe(&config, id, Ok((fingerprint, protection)), true))
        })
    }

    /// Drops the wallet's signing keys and its prepared transfers. Wallets
    /// that open without a passphrase stay open.
    pub fn lock(&self, id: &str) -> Result<()> {
        run(|| {
            let id = validate::wallet_id(id)?;
            self.session_epoch
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if guard(&self.unlocked).remove(id).is_some() {
                guard(&self.prepared).retain(|_, p| p.wallet != id);
            }
            Ok(())
        })
    }

    pub fn lock_all(&self) {
        self.session_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let ids: HashSet<String> = guard(&self.unlocked).drain().map(|(id, _)| id).collect();
        guard(&self.prepared).retain(|_, p| !ids.contains(&p.wallet));
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        run(|| {
            let name = validate::name(name)?;
            let _files = guard(&self.files);
            let (id, _) = self.wallet_file(id)?;
            let mut config = self.config_for_write()?;
            config::set_wallet_name(&mut config, &id, &name);
            config::save(&self.dir, config)
        })
    }

    /// Sets how many addresses the wallet uses (1–100) and returns them.
    pub fn set_address_count(&self, id: &str, count: u32) -> Result<Vec<String>> {
        self.set_address_count_authorized(id, count, None)
    }

    pub fn set_address_count_authorized(
        &self,
        id: &str,
        count: u32,
        passphrase: Option<Zeroizing<String>>,
    ) -> Result<Vec<String>> {
        run(|| {
            let count = validate::num_addresses(count)?;
            let epoch = self
                .session_epoch
                .load(std::sync::atomic::Ordering::Acquire);
            let (id, path) = self.wallet_file(id)?;
            let key = storage::read_key(&path)?;
            let (fp, _) = detect(&key)?;
            let mut derived = None;
            let cached = config::cached_addresses(&config::read_lenient(&self.dir), &id, &fp);
            let addresses = if cached.as_ref().is_some_and(|a| a.len() >= count as usize) {
                cached.unwrap()
            } else {
                let wallet = self
                    .derive_wallet(&id, count, passphrase.as_deref().map(String::as_str))?
                    .0;
                if cached
                    .as_ref()
                    .is_some_and(|old| old.iter().zip(&wallet.addresses).any(|(a, b)| a != b))
                {
                    return Err(Error::new(
                        "wallet_mismatch",
                        "cached addresses do not match the wallet",
                    ));
                }
                let addresses = wallet.addresses.clone();
                derived = Some(std::sync::Arc::new(wallet));
                addresses
            };
            drop(passphrase);
            let _files = guard(&self.files);
            if epoch
                != self
                    .session_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(locked());
            }
            let mut config = self.config_for_write()?;
            config::set_num_addresses(&mut config, &id, count);
            config::set_cached_addresses(&mut config, &id, &fp, &addresses);
            config::save(&self.dir, config)?;
            if let Some(wallet) = derived {
                let mut sessions = guard(&self.unlocked);
                if sessions.contains_key(&id) {
                    sessions.insert(id.clone(), wallet);
                }
            }
            Ok(addresses
                .iter()
                .take(count as usize)
                .map(ToString::to_string)
                .collect())
        })
    }

    /// Moves the key file to `<dir>/removed/<id>.<unix time>` (never deletes
    /// it) and forgets the wallet's settings.
    pub fn remove(&self, id: &str) -> Result<()> {
        run(|| {
            let files = guard(&self.files);
            let (id, _) = self.wallet_file(id)?;
            storage::move_to_removed(&self.dir, &id)?;
            // The wallet is gone; stale settings would be harmless.
            if let Ok(mut config) = self.config_for_write() {
                config::remove_wallet(&mut config, &id);
                if config::active(&config) == Some(id.as_str()) {
                    config::set_active(&mut config, None);
                }
                let _ = config::save(&self.dir, config);
            }
            drop(files);
            guard(&self.unlocked).remove(&id);
            guard(&self.prepared).retain(|_, p| p.wallet != id);
            guard(&self.reserved).remove(&id);
            Ok(())
        })
    }

    /// The wallet's 24 recovery words (the wallet must be open).
    pub fn mnemonic(&self, id: &str) -> Result<Zeroizing<String>> {
        self.mnemonic_authorized(id, None)
    }

    pub fn mnemonic_authorized(
        &self,
        id: &str,
        passphrase: Option<Zeroizing<String>>,
    ) -> Result<Zeroizing<String>> {
        run(|| {
            let (id, path) = self.wallet_file(id)?;
            let key = storage::read_key(&path)?;
            self.passphrase(&id, &key, passphrase.as_deref().map(String::as_str))?;
            Ok(Zeroizing::new(seed_to_words(&key.seed_value)))
        })
    }

    pub fn addresses(&self, id: &str) -> Result<Vec<String>> {
        run(|| {
            let (id, path) = self.wallet_file(id)?;
            let key = storage::read_key(&path)?;
            let (fp, protection) = detect(&key)?;
            let config = config::read_lenient(&self.dir);
            let count = config::num_addresses(&config, &id);
            if let Some(addresses) =
                config::cached_addresses(&config, &id, &fp).filter(|a| a.len() >= count as usize)
            {
                return Ok(addresses
                    .iter()
                    .take(count as usize)
                    .map(ToString::to_string)
                    .collect());
            }
            if protection == Protection::Passphrase {
                return Err(Error::new(
                    "passphrase_required",
                    "derive addresses with the passphrase",
                ));
            }
            let wallet = self.open_wallet(&id, count)?.0;
            let public = wallet.addresses.clone();
            drop(wallet);
            let _files = guard(&self.files);
            let mut config = self.config_for_write()?;
            config::set_cached_addresses(&mut config, &id, &fp, &public);
            config::save(&self.dir, config)?;
            Ok(public.iter().map(ToString::to_string).collect())
        })
    }

    pub fn set_active(&self, id: &str) -> Result<()> {
        run(|| {
            let _files = guard(&self.files);
            let (id, _) = self.wallet_file(id)?;
            let mut config = self.config_for_write()?;
            config::set_active(&mut config, Some(&id));
            config::save(&self.dir, config)
        })
    }

    /// Address labels, contacts and settings. Labels are returned for the
    /// listed wallets only; invalid stored entries are skipped.
    pub fn app_state(&self) -> Result<AppState> {
        run(|| {
            let config = config::read_lenient(&self.dir);
            let ids: Vec<String> = storage::wallet_files(&self.dir)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let mut labels = BTreeMap::new();
            for id in &ids {
                let wallet_labels = config::labels(&config, id);
                if !wallet_labels.is_empty() {
                    labels.insert(
                        id.clone(),
                        wallet_labels
                            .into_iter()
                            .map(|(i, l)| (i.to_string(), l))
                            .collect(),
                    );
                }
            }
            Ok(AppState {
                active_wallet: config::active(&config)
                    .filter(|a| ids.iter().any(|id| id == a))
                    .map(str::to_owned),
                labels,
                contacts: contacts(&config),
                settings: settings(&config),
            })
        })
    }

    /// Validates and saves the app state. Labels of wallets that are no
    /// longer listed are ignored; a changed server URL is checked first.
    /// `active_wallet` is not written: [`Service::set_active`] saves every
    /// switch, and a value loaded earlier would undo a change made since
    /// (for example by `mmxwallet use`).
    pub fn set_app_state(&self, state: AppState) -> Result<()> {
        run(|| {
            let invalid = |message: &str| Error::new("invalid_argument", message);
            let s = &state.settings;
            if !THEMES.contains(&s.theme.as_str()) {
                return Err(invalid("invalid theme"));
            }
            if s.auto_lock_minutes > MAX_AUTO_LOCK_MINUTES {
                return Err(invalid("invalid auto-lock time"));
            }
            validate::fee_ratio(s.fee_level)?;
            let contacts = validate_contacts(&state.contacts)?;
            if state.labels.len() > MAX_LABELLED_WALLETS {
                return Err(invalid("too many labelled wallets"));
            }
            let mut labels: BTreeMap<&str, BTreeMap<u32, String>> = BTreeMap::new();
            for (id, wallet_labels) in &state.labels {
                validate::wallet_id(id)?;
                let mut clean = BTreeMap::new();
                for (index, label) in wallet_labels {
                    let index =
                        config::label_index(index).ok_or_else(|| invalid("invalid label index"))?;
                    clean.insert(index, validate::name(label)?);
                }
                labels.insert(id, clean);
            }
            if let Some(id) = &state.active_wallet {
                validate::wallet_id(id)?;
            }
            // Check a new server before taking any lock.
            let current =
                config::rpc(&config::read_lenient(&self.dir)).and_then(|url| rpc_url(url).ok());
            let rpc_change = match s
                .rpc_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
            {
                None => current.is_some().then_some(None),
                Some(url) => {
                    let url = rpc_url(url)?;
                    if current.as_deref() == Some(url.as_str()) {
                        None
                    } else {
                        let client = Rpc::new(&url)?;
                        probe(&client)?;
                        Some(Some(client))
                    }
                }
            };

            let _files = guard(&self.files);
            let listed: Vec<String> = storage::wallet_files(&self.dir)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let mut config = self.config_for_write()?;
            let none = BTreeMap::new();
            for id in &listed {
                config::set_labels(&mut config, id, labels.get(id.as_str()).unwrap_or(&none));
            }
            let contacts: Vec<Value> = contacts
                .iter()
                .map(|c| {
                    json!({"id": c.id, "name": c.name, "address": c.address,
                           "note": c.note.as_deref().unwrap_or("")})
                })
                .collect();
            config.insert("contacts".into(), Value::Array(contacts));
            let app = config
                .entry("app")
                .and_modify(|v| {
                    if !v.is_object() {
                        *v = json!({});
                    }
                })
                .or_insert_with(|| json!({}));
            app["theme"] = json!(s.theme);
            app["auto_lock_minutes"] = json!(s.auto_lock_minutes);
            app["fee_level"] = json!(s.fee_level);
            if let Some(client) = &rpc_change {
                config::set_rpc(&mut config, client.as_ref().map(Rpc::base));
            }
            config::save(&self.dir, config)?;
            if let Some(client) = rpc_change {
                *guard(&self.rpc) = Arc::new(match client {
                    Some(client) => client,
                    None => default_rpc()?,
                });
            }
            Ok(())
        })
    }

    /// GET on the chain server for an allowlisted endpoint. Returns the
    /// parsed JSON body (`null` for an empty body).
    pub fn rpc_get(&self, path: &str) -> Result<Value> {
        run(|| {
            validate::rpc_path(path)?;
            self.client().get_limited(path, RPC_GET_LIMIT)
        })
    }

    /// Sets the chain server after checking that it answers; `None` (or an
    /// empty URL) restores the public default. Returns the URL in use.
    pub fn set_rpc(&self, url: Option<&str>) -> Result<String> {
        run(|| {
            let client = match url.map(str::trim).filter(|u| !u.is_empty()) {
                Some(url) => {
                    let client = rpc_client(url)?;
                    probe(&client)?;
                    Some(client)
                }
                None => None,
            };
            let _files = guard(&self.files);
            let mut config = self.config_for_write()?;
            config::set_rpc(&mut config, client.as_ref().map(Rpc::base));
            config::save(&self.dir, config)?;
            let client = Arc::new(match client {
                Some(client) => client,
                None => default_rpc()?,
            });
            let base = client.base().to_owned();
            *guard(&self.rpc) = client;
            Ok(base)
        })
    }

    /// The chain server URL in use.
    pub fn rpc(&self) -> String {
        self.client().base().to_owned()
    }

    /// Largest amount of `currency` the wallet can send with this memo and
    /// fee level, in atomic units (fresh balances).
    pub fn max_send(
        &self,
        id: &str,
        currency: &str,
        memo: Option<&str>,
        fee_level: u32,
    ) -> Result<String> {
        run(|| {
            let currency = validate::currency(currency)?;
            let memo = validate::memo(memo)?;
            let fee_ratio = validate::fee_ratio(fee_level)?;
            let wallet = Wallet::watch_only(
                self.addresses(id)?
                    .iter()
                    .map(|a| a.parse())
                    .collect::<mmx_wallet::Result<Vec<_>>>()?,
            )?;
            let rpc = self.client();
            let params = rpc.params()?;
            let mut state = remote::State::fetch(&rpc, &wallet.addresses, &params)?;
            self.reserve(&rpc, id, &params.network, &mut state);
            match wallet.max_transfer_amount(
                &state.balances,
                &params,
                currency,
                memo.as_deref(),
                fee_ratio,
            ) {
                Ok(amount) => Ok(amount.to_string()),
                Err(mmx_wallet::Error::InsufficientFunds(_)) => Err(insufficient_fee()),
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Builds, signs and checks a transfer with fresh balances, less the
    /// funds of the wallet's broadcast transfers that are not in a block yet.
    /// Nothing is sent: the signed transfer stays on this device for
    /// [`Service::broadcast`] for 10 minutes, bound to the wallet.
    pub fn prepare_send(&self, request: &SendRequest) -> Result<PreparedSend> {
        run(|| {
            let epoch = self
                .session_epoch
                .load(std::sync::atomic::Ordering::Acquire);
            let to = validate::recipient(&request.to)?;
            let currency = validate::currency(&request.contract)?;
            let amount = validate::atomic_amount(&request.amount)?;
            let memo = validate::memo(request.memo.as_deref())?;
            let fee_ratio = validate::fee_ratio(request.fee_level)?;
            let count = self.address_count(&request.wallet_id)?;
            let addresses = self
                .addresses(&request.wallet_id)?
                .iter()
                .map(|a| a.parse())
                .collect::<mmx_wallet::Result<Vec<_>>>()?;
            let (_, path) = self.wallet_file(&request.wallet_id)?;
            let with_passphrase = detect(&storage::read_key(&path)?)?.1 == Protection::Passphrase;
            if with_passphrase && !guard(&self.unlocked).contains_key(&request.wallet_id) {
                return Err(locked());
            }
            let rpc = self.client();
            let params = rpc.params()?;
            let mut state = remote::State::fetch(&rpc, &addresses, &params)?;
            self.reserve(&rpc, &request.wallet_id, &params.network, &mut state);
            let native = currency == Address::default();
            let (decimals, symbol) = if native {
                (params.decimals, "MMX".to_owned())
            } else {
                let total = state.totals.get(&currency).ok_or_else(insufficient_funds)?;
                (total.decimals, display_symbol(&total.symbol, &currency))
            };
            // Acquire signing keys only after network reads. A protected session
            // retains its keys until manual or inactivity lock.
            let (wallet, _) = self.open_wallet(&request.wallet_id, count)?;
            if wallet.addresses[..count as usize] != addresses {
                return Err(Error::new(
                    "wallet_mismatch",
                    "cached addresses do not match the wallet",
                ));
            }
            let tx = wallet.transfer(
                &state.balances,
                &params,
                state.height,
                Output {
                    address: to,
                    contract: currency,
                    amount,
                    memo: memo.clone(),
                },
                &TransferOptions {
                    fee_ratio,
                    expire_delta: EXPIRE_DELTA,
                    ..Default::default()
                },
            )?;
            drop(wallet);
            if !tx.fee_payer_covered(&state.balances) {
                return Err(if native {
                    insufficient_funds()
                } else {
                    insufficient_fee()
                });
            }
            // A simple transfer is charged exactly its static fee.
            let fee = tx.fee_amount();
            let bytes = serde_json::to_vec(&tx)
                .map_err(|_| Error::new("internal", "transaction serialization failed"))?;
            let txid = tx.id_hex().to_ascii_uppercase();
            let expires = tx.expires;
            // Kept only if the wallet was not locked meanwhile; `lock` removes
            // the signing keys before the prepared transfers, so holding
            // `unlocked` here closes the gap.
            let unlocked = guard(&self.unlocked);
            if epoch
                != self
                    .session_epoch
                    .load(std::sync::atomic::Ordering::Acquire)
                || (with_passphrase && !unlocked.contains_key(&request.wallet_id))
            {
                return Err(locked());
            }
            self.keep_prepared(
                txid.clone(),
                Prepared {
                    wallet: request.wallet_id.clone(),
                    tx,
                    bytes,
                    params,
                    created: Instant::now(),
                    created_at: SystemTime::now(),
                },
            );
            drop(unlocked);
            Ok(PreparedSend {
                txid,
                to: to.to_string(),
                contract: currency.to_string(),
                symbol,
                decimals,
                amount: amount.to_string(),
                fee: fee.to_string(),
                expires,
                height: state.height,
                memo,
            })
        })
    }

    /// Sends a prepared transfer of this wallet, once: it is removed whatever
    /// the outcome. It is checked first (height, network, the node's dry run)
    /// and not sent when a check fails (`tx_not_sent` when one cannot be
    /// made). Its funds stay reserved unless the node refused it.
    pub fn broadcast(&self, id: &str, txid: &str) -> Result<()> {
        run(|| {
            let id = validate::wallet_id(id)?;
            let txid = validate::txid(txid)?;
            let prepared = guard(&self.prepared)
                .remove(&txid)
                .filter(|p| p.wallet == id && self.fresh(p))
                .ok_or_else(|| {
                    Error::new("prepared_not_found", "the transfer is no longer available")
                })?;
            let rpc = self.client();
            // Nothing is sent unless these checks pass, so a failure here is
            // never reported as a transfer that may have been accepted.
            let not_sent = |e: Error| match e.code {
                _ if rpc::is_refusal(&e) => e,
                "wallet_error" => Error::new("tx_rejected", "the transfer can no longer be sent"),
                _ => Error::new(
                    "tx_not_sent",
                    format!(
                        "the transfer could not be checked, nothing was sent: {}",
                        e.message
                    ),
                ),
            };
            // A cheap read first, so an expired transfer is never sent (the
            // node runs it at the next height), nor one for another network.
            let height = rpc.height(&prepared.params).map_err(not_sent)?;
            if prepared.tx.expires <= height {
                return Err(Error::new(
                    "tx_expired",
                    "the transfer expired before it was sent",
                ));
            }
            // The node's broadcast accepts a transfer whose execution fails
            // (it is included and charged its fee), so the dry run must
            // pass first, at no more than the fee shown on review.
            rpc.validate(&prepared.bytes, prepared.tx.fee_amount())
                .map_err(not_sent)?;
            let reservation = Reservation::new(&prepared.tx, &prepared.params.network, height);
            let reserved_txid = reservation.txid.clone();
            // Reserved before sending, so a transfer prepared meanwhile
            // already leaves these funds alone.
            reserve::add(
                guard(&self.reserved).entry(id.to_owned()).or_default(),
                reservation,
            );
            let result = rpc.post("/transaction/broadcast", &prepared.bytes);
            if result.as_ref().is_err_and(rpc::is_refusal) {
                self.release(id, &reserved_txid);
            }
            result
        })
    }

    /// Drops a prepared transfer (no error if it is already gone).
    pub fn discard(&self, txid: &str) -> Result<()> {
        run(|| {
            let txid = validate::txid(txid)?;
            guard(&self.prepared).remove(&txid);
            Ok(())
        })
    }

    /// Whether a prepared transfer is within its time to live, by the
    /// monotonic clock and by the wall clock (which counts time asleep).
    fn fresh(&self, p: &Prepared) -> bool {
        p.created.elapsed() < self.prepared_ttl
            && SystemTime::now()
                .duration_since(p.created_at)
                .ok()
                .is_none_or(|age| age < self.prepared_ttl)
    }

    fn keep_prepared(&self, txid: String, prepared: Prepared) {
        let mut all = guard(&self.prepared);
        all.retain(|_, p| self.fresh(p));
        while all.len() >= MAX_PREPARED {
            let Some(oldest) = all
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            all.remove(&oldest);
        }
        all.insert(txid, prepared);
    }

    fn client(&self) -> Arc<Rpc> {
        guard(&self.rpc).clone()
    }

    /// Subtracts the funds of the wallet's broadcast transfers that are not
    /// in a block yet from `state` (balances read at `state.height`). Those
    /// checked at an earlier height are looked up first; no lock is held
    /// meanwhile. A failed lookup keeps the funds reserved.
    fn reserve(&self, rpc: &Rpc, id: &str, network: &str, state: &mut remote::State) {
        let height = state.height;
        let due = match guard(&self.reserved).get_mut(id) {
            Some(list) => reserve::due(list, network, height),
            None => return,
        };
        let statuses = if due.is_empty() {
            Vec::new()
        } else {
            remote::tx_statuses(rpc, &due)
        };
        let mut all = guard(&self.reserved);
        let Some(list) = all.get_mut(id) else {
            return;
        };
        reserve::update(list, &due, &statuses, height);
        reserve::apply(&mut state.balances, list, network);
        if list.is_empty() {
            all.remove(id);
        }
    }

    fn release(&self, id: &str, txid: &str) {
        let mut all = guard(&self.reserved);
        if let Some(list) = all.get_mut(id) {
            list.retain(|r| r.txid != txid);
            if list.is_empty() {
                all.remove(id);
            }
        }
    }

    /// The id's key file; the id must name a listed wallet.
    fn wallet_file(&self, id: &str) -> Result<(String, PathBuf)> {
        validate::wallet_id(id)?;
        storage::wallet_files(&self.dir)?
            .into_iter()
            .find(|(name, _)| name == id)
            .ok_or_else(not_found)
    }

    fn address_count(&self, id: &str) -> Result<u32> {
        validate::wallet_id(id)?;
        Ok(config::num_addresses(&config::read_lenient(&self.dir), id))
    }

    /// Validates an operation's secret. No passphrase is ever remembered.
    fn passphrase(
        &self,
        _id: &str,
        key: &KeyFile,
        supplied: Option<&str>,
    ) -> Result<(Zeroizing<String>, bool)> {
        match detect(key)?.1 {
            Protection::None | Protection::Empty => Ok((Zeroizing::new(String::new()), false)),
            Protection::Passphrase => {
                let pass = supplied
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| Error::new("passphrase_required", "passphrase required"))?;
                if key.finger_print.as_deref()
                    != Some(fingerprint(&key.seed_value, Some(pass)).as_str())
                {
                    return Err(Error::new("invalid_passphrase", "wrong passphrase"));
                }
                Ok((Zeroizing::new(pass.to_owned()), true))
            }
        }
    }

    fn derive_wallet(
        &self,
        id: &str,
        count: u32,
        supplied: Option<&str>,
    ) -> Result<(Wallet, bool)> {
        let (id, path) = self.wallet_file(id)?;
        let key = storage::read_key(&path)?;
        let (passphrase, with_passphrase) = self.passphrase(&id, &key, supplied)?;
        let wallet = Wallet::derive(&key.seed_value, &passphrase, 0, count)?;
        Ok((wallet, with_passphrase))
    }

    /// Shares the current signing session; regular wallets derive from disk.
    fn open_wallet(&self, id: &str, count: u32) -> Result<(std::sync::Arc<Wallet>, bool)> {
        let (id, path) = self.wallet_file(id)?;
        let key = storage::read_key(&path)?;
        if detect(&key)?.1 == Protection::Passphrase {
            let wallet = guard(&self.unlocked).get(&id).cloned().ok_or_else(locked)?;
            if wallet.addresses.len() < count as usize {
                return Err(Error::new(
                    "passphrase_required",
                    "derive missing addresses with the passphrase",
                ));
            }
            return Ok((wallet, true));
        }
        Ok((std::sync::Arc::new(key.unlock("", 0, count)?), false))
    }

    /// The configuration to modify. An unreadable file is set aside first so
    /// its content is not lost.
    fn config_for_write(&self) -> Result<Config> {
        match config::load(&self.dir)? {
            Loaded::Missing => Ok(Config::new()),
            Loaded::Valid(config) => Ok(config),
            Loaded::Invalid => {
                config::set_aside(&self.dir)?;
                Ok(Config::new())
            }
        }
    }
}

fn describe(
    config: &Config,
    id: String,
    key: Result<(String, Protection)>,
    unlocked: bool,
) -> WalletInfo {
    let name = config::wallet_name(config, &id).unwrap_or_else(|| default_name(&id));
    let address_count = config::num_addresses(config, &id);
    match key {
        Ok((fingerprint, protection)) => {
            let with_passphrase = protection == Protection::Passphrase;
            let can_view = config::cached_addresses(config, &id, &fingerprint)
                .is_some_and(|a| a.len() >= address_count as usize);
            WalletInfo {
                id,
                name,
                fingerprint,
                with_passphrase,
                locked: with_passphrase && !unlocked,
                can_view,
                address_count,
                error: None,
            }
        }
        Err(e) => WalletInfo {
            id,
            name,
            fingerprint: String::new(),
            with_passphrase: false,
            locked: true,
            can_view: false,
            address_count,
            error: Some(
                if e.code == "io_error" {
                    "io_error"
                } else {
                    "invalid_key_file"
                }
                .into(),
            ),
        },
    }
}

#[cfg(test)]
mod tests;

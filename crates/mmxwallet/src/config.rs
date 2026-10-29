//! `mmxwallet.json` in the wallet directory: the active wallet, the chain
//! server and per-wallet settings. Keys this crate does not know are kept on
//! every write; writes are atomic.
//!
//! ```json
//! { "active_wallet": "wallet_123.dat", "rpc": "https://rpc.mmx.network",
//!   "wallets": { "wallet_123.dat": { "name": "Savings", "num_addresses": 10,
//!                                    "labels": { "0": "Main" } } },
//!   "contacts": [ { "id": "c-1", "name": "Alice", "address": "mmx1…", "note": "" } ],
//!   "app": { "theme": "system", "auto_lock_minutes": 1440, "fee_level": 1 } }
//! ```
use crate::{
    error::{invalid, Result},
    storage, validate,
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Read},
    path::Path,
};
use zeroize::Zeroizing;

pub const FILE: &str = "mmxwallet.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub type Config = Map<String, Value>;

pub enum Loaded {
    Missing,
    Valid(Config),
    /// Not a JSON object (or too large to be one of ours).
    Invalid,
}

pub fn load(dir: &Path) -> Result<Loaded> {
    let file = match fs::File::open(dir.join(FILE)) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(e) => return Err(e.into()),
    };
    // This path can contain a key, so do not leave its raw bytes in memory.
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BYTES as usize + 1));
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Ok(Loaded::Invalid);
    }
    if mmx_wallet::KeyFile::decode(&bytes).is_ok() {
        return Err(invalid("wallet key cannot be used as configuration"));
    }
    Ok(match serde_json::from_slice(&bytes) {
        Ok(Value::Object(map)) => {
            if map.get("__type").and_then(Value::as_str) == Some("mmx.KeyFile") {
                return Err(invalid("wallet key cannot be used as configuration"));
            }
            Loaded::Valid(map)
        }
        _ => Loaded::Invalid,
    })
}

/// The configuration; an unreadable file is an error.
pub fn read(dir: &Path) -> Result<Config> {
    match load(dir)? {
        Loaded::Missing => Ok(Config::new()),
        Loaded::Valid(map) => Ok(map),
        Loaded::Invalid => Err(invalid(format!(
            "invalid wallet configuration: {}",
            dir.join(FILE).display()
        ))),
    }
}

/// The configuration for optional defaults: a missing directory or an
/// unreadable file reads as empty.
pub fn read_lenient(dir: &Path) -> Config {
    match load(dir) {
        Ok(Loaded::Valid(map)) => map,
        _ => Config::new(),
    }
}

pub fn save(dir: &Path, config: Config) -> Result<()> {
    storage::create_private_directory(dir)?;
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(config))
        .map_err(|_| invalid("configuration serialization failed"))?;
    bytes.push(b'\n');
    // Recheck the current path even when the caller loaded settings earlier.
    load(dir)?;
    storage::save(&dir.join(FILE), &bytes, None)
}

/// Moves an unreadable configuration aside so a new one can be written
/// without losing the old content.
pub fn set_aside(dir: &Path) -> Result<()> {
    let from = dir.join(FILE);
    let to = storage::unique_path(dir, &format!("{FILE}.invalid"))?;
    load(dir)?;
    fs::rename(from, to)?;
    Ok(())
}

pub fn active(config: &Config) -> Option<&str> {
    config
        .get("active_wallet")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub fn set_active(config: &mut Config, id: Option<&str>) {
    match id {
        Some(id) => {
            config.insert("active_wallet".into(), Value::from(id));
        }
        None => {
            config.remove("active_wallet");
        }
    }
}

pub fn rpc(config: &Config) -> Option<&str> {
    config
        .get("rpc")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

pub fn set_rpc(config: &mut Config, url: Option<&str>) {
    match url {
        Some(url) => {
            config.insert("rpc".into(), Value::from(url));
        }
        None => {
            config.remove("rpc");
        }
    }
}

/// Public address cache, bound to the key file's fingerprint. Never contains keys.
pub fn cached_addresses(
    config: &Config,
    id: &str,
    fingerprint: &str,
) -> Option<Vec<mmx_wallet::Address>> {
    cached_addresses_for(config, id, fingerprint, 0)
}

pub fn cached_addresses_for(
    config: &Config,
    id: &str,
    fingerprint: &str,
    account: u32,
) -> Option<Vec<mmx_wallet::Address>> {
    let field = if account == 0 {
        "public_addresses".to_owned()
    } else {
        format!("public_addresses_{account}")
    };
    let cache = wallet(config, id)?.get(&field)?.as_object()?;
    if cache.get("fingerprint")?.as_str()? != fingerprint {
        return None;
    }
    let values = cache.get("addresses")?.as_array()?;
    if values.is_empty() || values.len() > validate::MAX_NUM_ADDRESSES as usize {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .map(|v| {
            let address = validate::canonical_address(v.as_str()?)?;
            seen.insert(address).then_some(address)
        })
        .collect()
}

pub fn set_cached_addresses(
    config: &mut Config,
    id: &str,
    fingerprint: &str,
    addresses: &[mmx_wallet::Address],
) {
    set_cached_addresses_for(config, id, fingerprint, 0, addresses);
}

pub fn set_cached_addresses_for(
    config: &mut Config,
    id: &str,
    fingerprint: &str,
    account: u32,
    addresses: &[mmx_wallet::Address],
) {
    // Retain already derived addresses when an unlock uses a smaller visible count.
    if cached_addresses_for(config, id, fingerprint, account).is_some_and(|old| {
        old.len() > addresses.len() && old.iter().zip(addresses).all(|(a, b)| a == b)
    }) {
        return;
    }
    let field = if account == 0 {
        "public_addresses".to_owned()
    } else {
        format!("public_addresses_{account}")
    };
    wallet_mut(config, id).insert(
        field,
        serde_json::json!({
            "fingerprint": fingerprint,
            "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>()
        }),
    );
}

fn wallet<'a>(config: &'a Config, id: &str) -> Option<&'a Config> {
    config.get("wallets")?.as_object()?.get(id)?.as_object()
}

/// The wallet's settings object, created (or replacing a malformed value) as needed.
fn wallet_mut<'a>(config: &'a mut Config, id: &str) -> &'a mut Config {
    let wallets = object_mut(config, "wallets");
    object_mut(wallets, id)
}

fn object_mut<'a>(map: &'a mut Config, key: &str) -> &'a mut Config {
    let value = map
        .entry(key)
        .and_modify(|v| {
            if !v.is_object() {
                *v = Value::Object(Config::new());
            }
        })
        .or_insert_with(|| Value::Object(Config::new()));
    match value {
        Value::Object(map) => map,
        _ => unreachable!("value was just made an object"),
    }
}

pub fn wallet_name(config: &Config, id: &str) -> Option<String> {
    let name = wallet(config, id)?.get("name")?.as_str()?;
    validate::name(name).ok()
}

pub fn set_wallet_name(config: &mut Config, id: &str, name: &str) {
    wallet_mut(config, id).insert("name".into(), Value::from(name));
}

pub fn num_addresses(config: &Config, id: &str) -> u32 {
    wallet(config, id)
        .and_then(|w| w.get("num_addresses"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .and_then(|n| validate::num_addresses(n).ok())
        .unwrap_or(validate::DEFAULT_NUM_ADDRESSES)
}

pub fn set_num_addresses(config: &mut Config, id: &str, count: u32) {
    wallet_mut(config, id).insert("num_addresses".into(), Value::from(count));
}

/// Address labels by index; malformed entries are skipped.
pub fn labels(config: &Config, id: &str) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    let Some(labels) = wallet(config, id)
        .and_then(|w| w.get("labels"))
        .and_then(Value::as_object)
    else {
        return out;
    };
    for (key, value) in labels {
        if let (Some(index), Some(label)) = (
            label_index(key),
            value.as_str().and_then(|s| validate::name(s).ok()),
        ) {
            out.insert(index, label);
        }
    }
    out
}

pub fn set_labels(config: &mut Config, id: &str, labels: &BTreeMap<u32, String>) {
    if labels.is_empty() {
        if let Some(wallet) = config
            .get_mut("wallets")
            .and_then(Value::as_object_mut)
            .and_then(|w| w.get_mut(id))
            .and_then(Value::as_object_mut)
        {
            wallet.remove("labels");
        }
    } else {
        let wallet = wallet_mut(config, id);
        let map = labels
            .iter()
            .map(|(i, l)| (i.to_string(), Value::from(l.as_str())))
            .collect();
        wallet.insert("labels".into(), Value::Object(map));
    }
}

pub fn remove_wallet(config: &mut Config, id: &str) {
    if let Some(wallets) = config.get_mut("wallets").and_then(Value::as_object_mut) {
        wallets.remove(id);
    }
}

/// Canonical decimal address index below the maximum address count.
pub fn label_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || key.len() > 3
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    key.parse::<u32>()
        .ok()
        .filter(|i| *i < validate::MAX_NUM_ADDRESSES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wallet_settings_preserve_unknown_keys() {
        let mut config: Config = serde_json::from_value(json!({
            "active_wallet": "wallet.dat",
            "extra": [1, 2],
            "wallets": {"wallet.dat": {"name": "Main", "color": "red"}, "other.dat": 7}
        }))
        .unwrap();
        set_wallet_name(&mut config, "wallet.dat", "Savings");
        set_num_addresses(&mut config, "wallet.dat", 12);
        set_num_addresses(&mut config, "other.dat", 3);
        assert_eq!(
            Value::Object(config.clone()),
            json!({
                "active_wallet": "wallet.dat",
                "extra": [1, 2],
                "wallets": {
                    "wallet.dat": {"name": "Savings", "color": "red", "num_addresses": 12},
                    "other.dat": {"num_addresses": 3}
                }
            })
        );
        assert_eq!(num_addresses(&config, "wallet.dat"), 12);
        assert_eq!(num_addresses(&config, "missing.dat"), 10);
        remove_wallet(&mut config, "wallet.dat");
        assert_eq!(wallet_name(&config, "wallet.dat"), None);
    }

    #[test]
    fn malformed_values_fall_back_to_defaults() {
        let config: Config = serde_json::from_value(json!({
            "wallets": {
                "a.dat": {"name": "bad\nname", "num_addresses": 500, "labels": {"0": "ok", "01": "x", "100": "y", "2": 5, "3": " "}},
                "b.dat": {"num_addresses": 0},
                "c.dat": {"num_addresses": "7"}
            }
        }))
        .unwrap();
        assert_eq!(wallet_name(&config, "a.dat"), None);
        assert_eq!(num_addresses(&config, "a.dat"), 10);
        assert_eq!(num_addresses(&config, "b.dat"), 10);
        assert_eq!(num_addresses(&config, "c.dat"), 10);
        assert_eq!(
            labels(&config, "a.dat"),
            BTreeMap::from([(0, "ok".to_owned())])
        );
    }

    #[test]
    fn label_indices_are_canonical() {
        assert_eq!(label_index("0"), Some(0));
        assert_eq!(label_index("99"), Some(99));
        for bad in ["", "00", "07", "100", "-1", "+1", "1.0", "abc"] {
            assert_eq!(label_index(bad), None, "{bad}");
        }
    }

    fn assert_key_rejected(dir: &Path, bytes: &[u8]) {
        let path = dir.join(FILE);
        fs::write(&path, bytes).unwrap();
        for result in [
            load(dir).map(|_| ()),
            read(dir).map(|_| ()),
            save(dir, Config::new()),
            set_aside(dir),
        ] {
            assert_eq!(result.unwrap_err().code, "invalid_argument");
            assert!(fs::read(&path).unwrap() == bytes, "key changed");
            assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
        }
    }

    #[test]
    fn configuration_rejects_valid_json_and_binary_keys() {
        let dir = tempfile::tempdir().unwrap();
        for passphrase in [None, Some("config-key-test")] {
            let key = mmx_wallet::KeyFile::generate(passphrase);
            assert_key_rejected(dir.path(), &key.encode());
            assert_key_rejected(dir.path(), &serde_json::to_vec(&key).unwrap());
            // The decoder also accepts legacy JSON without an explicit type.
            let mut legacy = serde_json::to_value(&key).unwrap();
            legacy.as_object_mut().unwrap().remove("__type");
            let bytes = serde_json::to_vec(&legacy).unwrap();
            assert!(mmx_wallet::KeyFile::decode(&bytes).is_ok());
            assert_key_rejected(dir.path(), &bytes);
        }
    }

    #[test]
    fn configuration_rejects_damaged_or_extended_tagged_keys() {
        let dir = tempfile::tempdir().unwrap();
        for value in [
            json!({"__type": "mmx.KeyFile", "seed_value": "damaged"}),
            json!({"__type": "mmx.KeyFile", "wallets": {}, "extra": true}),
        ] {
            assert_key_rejected(dir.path(), &serde_json::to_vec(&value).unwrap());
        }
    }

    #[test]
    fn writes_recheck_keys_replacing_previously_read_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let key = mmx_wallet::KeyFile::generate(None);
        for bytes in [key.encode(), serde_json::to_vec(&key).unwrap()] {
            for rename in [false, true] {
                fs::write(&path, b"{\"extra\":true}").unwrap();
                let config = read(dir.path()).unwrap();
                fs::write(&path, &bytes).unwrap();
                let result = if rename {
                    set_aside(dir.path())
                } else {
                    save(dir.path(), config)
                };
                assert_eq!(result.unwrap_err().code, "invalid_argument");
                assert!(fs::read(&path).unwrap() == bytes, "key changed");
                assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
            }
        }
    }

    #[test]
    fn ordinary_settings_keep_load_save_and_set_aside_behavior() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(load(dir.path()).unwrap(), Loaded::Missing));
        let path = dir.path().join(FILE);
        fs::write(&path, b"{\"extra\":[1,2],\"__type\":\"other.Settings\"}").unwrap();
        let mut config = read(dir.path()).unwrap();
        set_rpc(&mut config, Some("https://example.com"));
        save(dir.path(), config).unwrap();
        let config = read(dir.path()).unwrap();
        assert_eq!(config["extra"], json!([1, 2]));
        assert_eq!(config["__type"], "other.Settings");
        for bytes in [b"{ damaged".to_vec(), vec![b' '; MAX_BYTES as usize + 1]] {
            fs::write(&path, &bytes).unwrap();
            assert!(matches!(load(dir.path()).unwrap(), Loaded::Invalid));
            set_aside(dir.path()).unwrap();
            assert!(!path.exists());
            let backup = fs::read_dir(dir.path()).unwrap().last().unwrap().unwrap();
            assert!(fs::read(backup.path()).unwrap() == bytes, "backup changed");
            fs::remove_file(backup.path()).unwrap();
            save(dir.path(), Config::new()).unwrap();
        }
        fs::write(&path, b"malformed").unwrap();
        save(dir.path(), Config::new()).unwrap();
        assert!(matches!(load(dir.path()).unwrap(), Loaded::Valid(_)));
    }

    #[test]
    fn writes_propagate_configuration_read_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(FILE)).unwrap();
        assert!(load(dir.path()).is_err());
        assert!(save(dir.path(), Config::new()).is_err());
        assert!(set_aside(dir.path()).is_err());
        assert!(dir.path().join(FILE).is_dir());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn configuration_links_to_keys_are_never_replaced_or_moved() {
        use std::os::unix::fs::{symlink, MetadataExt};
        for symbolic in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let key_path = dir.path().join("key.json");
            let path = dir.path().join(FILE);
            let bytes = serde_json::to_vec(&mmx_wallet::KeyFile::generate(None)).unwrap();
            fs::write(&key_path, &bytes).unwrap();
            if symbolic {
                symlink(&key_path, &path).unwrap();
            } else {
                fs::hard_link(&key_path, &path).unwrap();
            }
            let before = fs::symlink_metadata(&path).unwrap();
            for result in [
                load(dir.path()).map(|_| ()),
                save(dir.path(), Config::new()),
                set_aside(dir.path()),
            ] {
                assert_eq!(result.unwrap_err().code, "invalid_argument");
                let after = fs::symlink_metadata(&path).unwrap();
                assert_eq!(
                    (after.dev(), after.ino(), after.mode()),
                    (before.dev(), before.ino(), before.mode())
                );
                assert!(fs::read(&path).unwrap() == bytes, "linked key changed");
                assert!(fs::read(&key_path).unwrap() == bytes, "key changed");
                assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
            }
        }
    }
}

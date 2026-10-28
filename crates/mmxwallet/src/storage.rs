use crate::{
    config,
    error::{invalid, Error, Result},
    validate,
};
use mmx_wallet::KeyFile;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub fn directory() -> Result<PathBuf> {
    match std::env::var_os("MMX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        Some(dir) => Ok(dir),
        None => user_directory(),
    }
}

/// The standalone desktop wallet never inherits the node's MMX_HOME.
pub fn user_directory() -> Result<PathBuf> {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(home_var)
        .filter(|s| !s.is_empty())
        .map(|p| PathBuf::from(p).join(".mmx"))
        .ok_or_else(|| {
            Error::new(
                "wallet_directory_unavailable",
                format!("set {home_var}, or select a wallet with --file"),
            )
        })
}
pub fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}
const MAX_KEY_FILE: usize = 65536;
pub fn read_key(path: &Path) -> Result<KeyFile> {
    // Reserved up front so the buffer holding the seed is never reallocated
    // (which would leave a copy behind in freed memory).
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_KEY_FILE + 1));
    fs::File::open(path)?
        .take(MAX_KEY_FILE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_KEY_FILE {
        return Err(Error::new("wallet_error", "wallet file exceeds size limit"));
    }
    Ok(KeyFile::decode(&bytes)?)
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
pub fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
pub fn save(path: &Path, bytes: &[u8], exists_code: Option<&'static str>) -> Result<()> {
    let parent = parent(path);
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Some(code) = exists_code {
        tmp.persist_noclobber(path).map_err(|e| {
            Error::new(
                if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                    code
                } else {
                    "io_error"
                },
                "failed to save without overwriting; choose a new path",
            )
        })?;
    } else {
        tmp.persist(path).map_err(|e| Error::from(e.error))?;
    }
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn save_key(path: &Path, key: &KeyFile) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(Error::new(
            "wallet_exists",
            "wallet already exists; choose a new path",
        ));
    }
    create_private_directory(parent(path))?;
    let bytes = Zeroizing::new(if path.extension().is_some_and(|e| e == "json") {
        serde_json::to_vec(key).expect("key serialization")
    } else {
        key.encode()
    });
    save(path, &bytes, Some("wallet_exists"))
}
#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub fingerprint: String,
    pub with_passphrase: bool,
}
/// A key file in the wallet directory that cannot be read or decoded.
pub struct Unreadable {
    pub path: PathBuf,
    pub error: Error,
}
/// Regular files in `dir` named like key files (`wallet.dat`, `wallet_*.dat`,
/// `mmxwallet_*.dat`), sorted by name. Symbolic links are not followed.
pub fn wallet_files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if validate::is_wallet_file_name(&name) {
            files.push((name, entry.path()));
        }
    }
    files.sort();
    Ok(files)
}
/// Readable wallets plus the key files that could not be read.
pub fn scan(dir: &Path) -> Result<(Vec<Entry>, Vec<Unreadable>)> {
    let mut entries = Vec::new();
    let mut unreadable = Vec::new();
    for (_, path) in wallet_files(dir)? {
        match read_key(&path) {
            Ok(key) => entries.push(Entry {
                fingerprint: key.fingerprint(),
                with_passphrase: key.requires_passphrase(),
                path,
            }),
            Err(error) => unreadable.push(Unreadable { path, error }),
        }
    }
    Ok((entries, unreadable))
}
pub fn wallets(dir: &Path) -> Result<Vec<Entry>> {
    Ok(scan(dir)?.0)
}
/// The name of the wallet directory's key file that `path` refers to, so a
/// wallet opened with `--file` keeps its settings.
pub fn wallet_file_name(dir: &Path, path: &Path) -> Option<String> {
    let target = fs::canonicalize(path).ok()?;
    let name = target.file_name()?.to_str()?;
    (target.parent()? == fs::canonicalize(dir).ok()?
        && validate::is_wallet_file_name(name)
        && fs::metadata(&target).ok()?.is_file())
    .then(|| name.to_owned())
}
pub fn active(dir: &Path) -> Result<Option<String>> {
    Ok(config::active(&config::read(dir)?).map(str::to_owned))
}
pub fn set_active(dir: &Path, path: &Path) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid("invalid wallet filename"))?
        .to_string_lossy()
        .into_owned();
    let mut config = config::read(dir)?;
    config::set_active(&mut config, Some(&name));
    config::save(dir, config)
}
/// `dir/<base>.<unix time>`, with a counter appended if that name is taken.
pub fn unique_path(dir: &Path, base: &str) -> Result<PathBuf> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("{base}.{time}")
        } else {
            format!("{base}.{time}-{n}")
        };
        let path = dir.join(name);
        if fs::symlink_metadata(&path).is_err() {
            return Ok(path);
        }
    }
    Err(Error::new("io_error", "no free file name"))
}
/// Moves a key file into `<dir>/removed/` (created private) under a
/// timestamped name. Key files are never deleted.
pub fn move_to_removed(dir: &Path, name: &str) -> Result<PathBuf> {
    let removed = dir.join("removed");
    create_private_directory(&removed)?;
    let target = unique_path(&removed, name)?;
    fs::rename(dir.join(name), &target)?;
    #[cfg(unix)]
    {
        fs::File::open(&removed)?.sync_all()?;
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(target)
}
fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}
/// The error of an unreadable key file, naming the file.
fn unreadable_error(u: &Unreadable) -> Error {
    Error::new(
        u.error.code,
        format!(
            "{}: {}",
            file_name(&u.path).unwrap_or("wallet file"),
            u.error.message
        ),
    )
}
/// The wallet named by `selector` (a fingerprint, or with `allow_index` a
/// list index), else the `active` one, else the only one. An unreadable key
/// file never makes another wallet the selected one: when it is the one
/// named, its error is returned, and it counts as a wallet when there is
/// nothing to choose by.
pub fn select(
    entries: &[Entry],
    unreadable: &[Unreadable],
    selector: Option<&str>,
    active: Option<&str>,
    allow_index: bool,
) -> Result<Entry> {
    if entries.is_empty() && unreadable.is_empty() {
        return Err(Error::new(
            "wallet_error",
            "no wallets found; run 'mmxwallet create' or 'mmxwallet import'",
        ));
    }
    if let Some(s) = selector {
        if !s.starts_with('#') {
            let matches: Vec<_> = entries.iter().filter(|e| e.fingerprint == s).collect();
            if matches.len() > 1 {
                return Err(invalid("wallet fingerprint is ambiguous; select by index"));
            }
            if let Some(e) = matches.first() {
                return Ok((*e).clone());
            }
        }
        if allow_index {
            if let Ok(i) = s.trim_start_matches('#').parse::<usize>() {
                if let Some(e) = entries.get(i) {
                    return Ok(e.clone());
                }
            }
        }
        let key_file = format!("wallet_{s}.dat");
        if let Some(u) = unreadable
            .iter()
            .find(|u| file_name(&u.path).is_some_and(|n| n == s || n == key_file))
        {
            return Err(unreadable_error(u));
        }
        return Err(invalid("no wallet matches selector"));
    }
    if let Some(a) = active {
        if let Some(e) = entries.iter().find(|e| file_name(&e.path) == Some(a)) {
            return Ok(e.clone());
        }
        if let Some(u) = unreadable.iter().find(|u| file_name(&u.path) == Some(a)) {
            return Err(unreadable_error(u));
        }
        return select(entries, unreadable, Some(a), None, false);
    }
    match (entries, unreadable) {
        ([only], []) => Ok(only.clone()),
        ([], [only]) => Err(unreadable_error(only)),
        _ => Err(Error::new(
            "wallet_error",
            "multiple wallets found; select with 'mmxwallet use' or --wallet",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, fingerprint: &str) -> Entry {
        Entry {
            path: PathBuf::from("/w").join(name),
            fingerprint: fingerprint.into(),
            with_passphrase: false,
        }
    }
    fn broken(name: &str) -> Unreadable {
        Unreadable {
            path: PathBuf::from("/w").join(name),
            error: Error::new("wallet_error", "invalid or unsupported wallet key file"),
        }
    }
    fn picked(r: Result<Entry>) -> String {
        file_name(&r.unwrap().path).unwrap().to_owned()
    }
    fn failed(r: Result<Entry>) -> (&'static str, String) {
        let e = r.err().unwrap();
        (e.code, e.message)
    }

    #[test]
    fn unreadable_key_files_are_never_skipped_over() {
        let good = [entry("wallet_1.dat", "1")];
        let bad = [broken("wallet_2.dat")];
        // Nothing to choose by: the broken file counts as a wallet.
        assert_eq!(
            failed(select(&good, &bad, None, None, false)).1,
            "multiple wallets found; select with 'mmxwallet use' or --wallet"
        );
        assert_eq!(
            failed(select(&[], &bad, None, None, false)),
            (
                "wallet_error",
                "wallet_2.dat: invalid or unsupported wallet key file".into()
            )
        );
        // The active or selected wallet is the broken one: its own error.
        for (selector, active) in [
            (None, Some("wallet_2.dat")),
            (Some("2"), None),
            (Some("wallet_2.dat"), Some("wallet_1.dat")),
        ] {
            assert_eq!(
                failed(select(&good, &bad, selector, active, false)).1,
                "wallet_2.dat: invalid or unsupported wallet key file",
                "{selector:?} {active:?}"
            );
        }
        // A readable active or selected wallet is still used.
        assert_eq!(
            picked(select(&good, &bad, None, Some("wallet_1.dat"), false)),
            "wallet_1.dat"
        );
        assert_eq!(
            picked(select(&good, &bad, Some("1"), None, false)),
            "wallet_1.dat"
        );
        assert_eq!(
            picked(select(&good, &[], None, None, false)),
            "wallet_1.dat"
        );
        assert_eq!(
            failed(select(&good, &bad, Some("3"), None, false)).0,
            "invalid_argument"
        );
        assert_eq!(
            failed(select(&[], &[], None, None, false)).0,
            "wallet_error"
        );
    }

    #[test]
    fn key_files_are_found_by_any_path() {
        let dir = tempfile::tempdir().unwrap();
        let wallets = dir.path().join("wallets");
        create_private_directory(&wallets).unwrap();
        fs::write(wallets.join("wallet_7.dat"), b"x").unwrap();
        fs::write(wallets.join("notes.dat"), b"x").unwrap();
        fs::write(dir.path().join("wallet_8.dat"), b"x").unwrap();
        let name = |p: PathBuf| wallet_file_name(&wallets, &p);
        assert_eq!(
            name(wallets.join("wallet_7.dat")).as_deref(),
            Some("wallet_7.dat")
        );
        assert_eq!(
            name(wallets.join("..").join("wallets").join("wallet_7.dat")).as_deref(),
            Some("wallet_7.dat")
        );
        assert_eq!(name(wallets.join("notes.dat")), None);
        assert_eq!(name(dir.path().join("wallet_8.dat")), None);
        assert_eq!(name(wallets.join("wallet_9.dat")), None);
        #[cfg(unix)]
        {
            let link = dir.path().join("link.dat");
            std::os::unix::fs::symlink(wallets.join("wallet_7.dat"), &link).unwrap();
            assert_eq!(name(link).as_deref(), Some("wallet_7.dat"));
        }
    }
}

//! Versioned tables with atomic journal commits and inclusive historical reads.
//!
//! Writes belong to `version()`. `commit(next)` makes them durable, then moves
//! to `next`. `revert(v)` discards writes at versions >= v, including pending
//! writes. This follows the C++ Table convention, with a new journal format.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::path::Path;

use thiserror::Error;

const MAGIC: &[u8; 8] = b"MMXRDB\x01\n";
const MAX_FRAME: usize = 64 * 1024 * 1024;
const FRAME_HEADER: u64 = 12;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("corrupt database: {0}")]
    Corrupt(&'static str),
    #[error("invalid version transition from {current} to {requested}")]
    Version { current: u32, requested: u32 },
    #[error("invalid table name or entry size")]
    InvalidEntry,
    #[error("journal write failed; close and reopen the database")]
    Poisoned,
}

pub type Result<T> = std::result::Result<T, Error>;

/// C++ Table::default_comparator orders by length first, then by bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Key(Vec<u8>);

impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.len().cmp(&other.0.len()).then(self.0.cmp(&other.0))
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A put or tombstone, staged atomically by `Database::apply`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Write {
    pub table: String,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

impl Write {
    pub fn put(
        table: impl Into<String>,
        key: impl Into<Vec<u8>>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            table: table.into(),
            key: key.into(),
            value: Some(value.into()),
        }
    }

    pub fn delete(table: impl Into<String>, key: impl Into<Vec<u8>>) -> Self {
        Self {
            table: table.into(),
            key: key.into(),
            value: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub version: u32,
}

type Versions = BTreeMap<u32, Option<Vec<u8>>>;
type Tables = BTreeMap<String, BTreeMap<Key, Versions>>;
type Pending = BTreeMap<(String, Key), Option<Vec<u8>>>;

/// Single writer; opening a journal takes an exclusive OS file lock.
/// All versions are currently indexed in RAM. Compaction is a later port step.
pub struct Database {
    tables: Tables,
    pending: Pending,
    version: u32,
    journal: Option<File>,
    poisoned: bool,
}

impl Database {
    pub fn memory() -> Self {
        Self {
            tables: BTreeMap::new(),
            pending: BTreeMap::new(),
            version: 0,
            journal: None,
            poisoned: false,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let mut parents_to_sync = Vec::new();
        for directory in path.ancestors().take_while(|directory| !directory.exists()) {
            if let Some(parent) = directory.parent() {
                parents_to_sync.push(parent.to_owned());
            }
        }
        fs::create_dir_all(&path)?;
        // Persist each newly created directory entry, starting at the existing
        // ancestor, so the journal cannot outlive its path after a power loss.
        for parent in parents_to_sync.iter().rev() {
            File::open(parent)?.sync_all()?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("journal.mmx"))?;
        file.try_lock().map_err(std::io::Error::from)?;
        if file.metadata()?.len() == 0 {
            file.write_all(MAGIC)?;
            file.sync_all()?;
            // Persist creation of the journal as well as its contents.
            File::open(&path)?.sync_all()?;
        }
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Corrupt("unsupported journal header"));
        }
        let mut db = Self::memory();
        let mut valid_end = 8;
        loop {
            let remaining = file.metadata()?.len() - valid_end;
            if remaining < FRAME_HEADER {
                break;
            }
            let mut header = [0; 12];
            file.read_exact(&mut header)?;
            let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
            let guard = u32::from_le_bytes(header[4..8].try_into().unwrap());
            if guard != !(len as u32) {
                return Err(Error::Corrupt("journal frame length guard"));
            }
            let checksum = u32::from_le_bytes(header[8..].try_into().unwrap());
            if len > MAX_FRAME {
                return Err(Error::Corrupt("journal frame too large"));
            }
            if remaining - FRAME_HEADER < len as u64 {
                break; // Only a physically incomplete final frame is recoverable.
            }
            let mut payload = vec![0; len];
            file.read_exact(&mut payload)?;
            if crc32fast::hash(&payload) != checksum {
                return Err(Error::Corrupt("journal checksum mismatch"));
            }
            db.replay(&payload)?;
            valid_end += FRAME_HEADER + len as u64;
        }
        // A crashed append cannot hide a subsequent commit behind its torn tail.
        file.set_len(valid_end)?;
        file.seek(SeekFrom::Start(valid_end))?;
        file.sync_all()?;
        db.journal = Some(file);
        Ok(db)
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        self.check_writable()?;
        let writes: Vec<_> = writes.into_iter().collect();
        for write in &writes {
            validate(write)?;
        }
        for write in writes {
            self.pending
                .insert((write.table, Key(write.key)), write.value);
        }
        Ok(())
    }

    pub fn insert(&mut self, table: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.apply([Write::put(table, key, value)])
    }

    pub fn remove(&mut self, table: &str, key: &[u8]) -> Result<()> {
        self.apply([Write::delete(table, key)])
    }

    /// `max_version` is inclusive. `None` includes staged writes.
    pub fn get(&self, table: &str, key: &[u8], max_version: Option<u32>) -> Option<&[u8]> {
        self.row(table, &Key(key.to_vec()), max_version)
            .and_then(|(_, value)| value.as_deref())
    }

    fn row(&self, table: &str, key: &Key, height: Option<u32>) -> Option<(u32, &Option<Vec<u8>>)> {
        let height = height.unwrap_or(u32::MAX);
        if height >= self.version {
            if let Some(value) = self.pending.get(&(table.to_owned(), key.clone())) {
                return Some((self.version, value));
            }
        }
        self.tables
            .get(table)?
            .get(key)?
            .range(..=height)
            .next_back()
            .map(|(v, value)| (*v, value))
    }

    /// An owned, stable snapshot, ordered by the original size-first comparator.
    pub fn scan(&self, table: &str, prefix: &[u8], max_version: Option<u32>) -> Vec<Row> {
        let mut keys = BTreeSet::new();
        if let Some(rows) = self.tables.get(table) {
            keys.extend(rows.keys().filter(|key| key.0.starts_with(prefix)).cloned());
        }
        keys.extend(
            self.pending
                .keys()
                .filter(|(name, key)| name == table && key.0.starts_with(prefix))
                .map(|(_, key)| key.clone()),
        );
        keys.into_iter()
            .filter_map(|key| {
                let (version, value) = self.row(table, &key, max_version)?;
                Some(Row {
                    key: key.0,
                    value: value.clone()?,
                    version,
                })
            })
            .collect()
    }

    pub fn commit(&mut self, next_version: u32) -> Result<()> {
        self.check_writable()?;
        self.check_commit(next_version)?;
        let mut payload = vec![1];
        payload.extend(self.version.to_le_bytes());
        payload.extend(next_version.to_le_bytes());
        let count = u32::try_from(self.pending.len()).map_err(|_| Error::InvalidEntry)?;
        payload.extend(count.to_le_bytes());
        for ((table, key), value) in &self.pending {
            payload.extend((table.len() as u16).to_le_bytes());
            payload.extend(table.as_bytes());
            payload.extend((key.0.len() as u32).to_le_bytes());
            payload.extend(&key.0);
            payload.extend(
                value
                    .as_ref()
                    .map_or(u32::MAX, |v| v.len() as u32)
                    .to_le_bytes(),
            );
            if let Some(value) = value {
                payload.extend(value);
            }
            if payload.len() > MAX_FRAME {
                return Err(Error::InvalidEntry);
            }
        }
        self.append(&payload)?;
        self.finish_commit(next_version);
        Ok(())
    }

    /// Discard versions >= `version`. Reverting the current version clears
    /// pending writes; reverting to zero removes all state.
    pub fn revert(&mut self, version: u32) -> Result<()> {
        self.check_writable()?;
        if version > self.version {
            return Err(Error::Version {
                current: self.version,
                requested: version,
            });
        }
        let mut payload = vec![2];
        payload.extend(version.to_le_bytes());
        self.append(&payload)?;
        self.finish_revert(version);
        Ok(())
    }

    fn check_writable(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    fn check_commit(&self, next: u32) -> Result<()> {
        if next <= self.version || next == u32::MAX {
            return Err(Error::Version {
                current: self.version,
                requested: next,
            });
        }
        Ok(())
    }

    fn append(&mut self, payload: &[u8]) -> Result<()> {
        if let Some(file) = &mut self.journal {
            let result = (|| -> std::io::Result<()> {
                file.write_all(&(payload.len() as u32).to_le_bytes())?;
                file.write_all(&(!(payload.len() as u32)).to_le_bytes())?;
                file.write_all(&crc32fast::hash(payload).to_le_bytes())?;
                file.write_all(payload)?;
                file.sync_all()
            })();
            if let Err(error) = result {
                self.poisoned = true;
                return Err(error.into());
            }
        }
        Ok(())
    }

    fn finish_commit(&mut self, next: u32) {
        for ((table, key), value) in std::mem::take(&mut self.pending) {
            self.tables
                .entry(table)
                .or_default()
                .entry(key)
                .or_default()
                .insert(self.version, value);
        }
        self.version = next;
    }

    fn finish_revert(&mut self, version: u32) {
        self.pending.clear();
        self.tables.retain(|_, rows| {
            rows.retain(|_, history| {
                history.split_off(&version);
                !history.is_empty()
            });
            !rows.is_empty()
        });
        self.version = version;
    }

    fn replay(&mut self, payload: &[u8]) -> Result<()> {
        let mut input = Input(payload);
        match input.byte()? {
            1 => {
                let version = input.u32()?;
                let next = input.u32()?;
                if version != self.version || self.check_commit(next).is_err() {
                    return Err(Error::Corrupt("commit version mismatch"));
                }
                let count = input.u32()?;
                let mut pending = BTreeMap::new();
                for _ in 0..count {
                    let size = u16::from_le_bytes(input.take(2)?.try_into().unwrap()) as usize;
                    let table = std::str::from_utf8(input.take(size)?)
                        .map_err(|_| Error::Corrupt("invalid table name"))?
                        .to_owned();
                    let size = input.u32()? as usize;
                    let key = input.take(size)?.to_vec();
                    let size = input.u32()?;
                    let value = if size == u32::MAX {
                        None
                    } else {
                        Some(input.take(size as usize)?.to_vec())
                    };
                    let write = Write { table, key, value };
                    validate(&write).map_err(|_| Error::Corrupt("invalid entry"))?;
                    pending.insert((write.table, Key(write.key)), write.value);
                }
                input.finish()?;
                self.pending = pending;
                self.finish_commit(next);
            }
            2 => {
                let version = input.u32()?;
                input.finish()?;
                if version > self.version {
                    return Err(Error::Corrupt("revert version mismatch"));
                }
                self.finish_revert(version);
            }
            _ => return Err(Error::Corrupt("unknown journal operation")),
        }
        Ok(())
    }
}

fn validate(write: &Write) -> Result<()> {
    if write.table.is_empty()
        || write.table.len() > u16::MAX as usize
        || !write
            .table
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        || write.key.len() > MAX_FRAME
        || write.value.as_ref().is_some_and(|v| v.len() > MAX_FRAME)
    {
        return Err(Error::InvalidEntry);
    }
    Ok(())
}

struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8]> {
        if size > self.0.len() {
            return Err(Error::Corrupt("truncated record"));
        }
        let (head, tail) = self.0.split_at(size);
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn finish(&self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Corrupt("trailing record data"))
        }
    }
}

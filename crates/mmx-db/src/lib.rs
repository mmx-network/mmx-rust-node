//! Versioned native storage with atomic commits and inclusive historical reads.
//!
//! Writes belong to `version()`. `commit(next)` makes them durable, then moves
//! to `next`. `revert(v)` discards writes at versions >= v, including pending
//! writes. Native persistence and the in-memory reference share the C++ Table convention.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use thiserror::Error;
pub mod journal;
pub mod native;
pub mod sorted;
pub mod store;
pub use store::Store;

const MAX_FRAME: usize = 64 * 1024 * 1024;

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
    #[error("database write failed; close and reopen the database")]
    Poisoned,
    #[error("replacement snapshot does not share the retained history")]
    SnapshotMismatch,
    #[error("history before database version {floor} was purged (requested {requested})")]
    Pruned { floor: u32, requested: u32 },
}

pub type Result<T> = std::result::Result<T, Error>;

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

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

/// In-memory versioned tables, used for speculative branches and reference tests.
/// Persistent storage is provided by `Store` / `native::Native`.
pub struct Database {
    tables: Tables,
    pending: Pending,
    version: u32,
}

impl Database {
    pub fn memory() -> Self {
        Self {
            tables: BTreeMap::new(),
            pending: BTreeMap::new(),
            version: 0,
        }
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
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
    /// Stable range snapshot in the native size-first order, [start, end).
    pub fn scan_range(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max_version: Option<u32>,
    ) -> Vec<Row> {
        use std::ops::Bound;
        let start = Key(start.to_vec());
        let end = end.map(|v| Key(v.to_vec()));
        if end.as_ref().is_some_and(|end| *end <= start) {
            return vec![];
        }
        let mut keys = BTreeSet::new();
        if let Some(rows) = self.tables.get(table) {
            keys.extend(
                rows.range((
                    Bound::Included(&start),
                    end.as_ref().map_or(Bound::Unbounded, Bound::Excluded),
                ))
                .map(|(k, _)| k.clone()),
            );
        }
        keys.extend(
            self.pending
                .keys()
                .filter(|(name, key)| {
                    name == table && key >= &start && end.as_ref().is_none_or(|end| key < end)
                })
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
        self.commit_batch(next_version, [])
    }

    /// Validate and publish additional writes with the pending batch atomically.
    /// Validation failure leaves the original pending and committed state intact.
    pub fn commit_batch(
        &mut self,
        next_version: u32,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<()> {
        self.check_commit(next_version)?;
        let mut pending = self.pending.clone();
        for write in writes {
            validate(&write)?;
            pending.insert((write.table, Key(write.key)), write.value);
        }
        self.pending = pending;
        self.finish_commit(next_version);
        Ok(())
    }

    /// Discard versions >= `version`. Reverting the current version clears
    /// pending writes; reverting to zero removes all state.
    pub fn revert(&mut self, version: u32) -> Result<()> {
        if version > self.version {
            return Err(Error::Version {
                current: self.version,
                requested: version,
            });
        }
        self.finish_revert(version);
        Ok(())
    }

    /// Detached, committed history before `version`, ready for speculative
    /// writes at that version. No file handles or pending writes are shared.
    pub fn fork(&self, version: u32) -> Result<Self> {
        if version > self.version || self.has_pending() {
            return Err(Error::SnapshotMismatch);
        }
        let mut fork = Self::memory();
        fork.tables = self.tables.clone();
        fork.finish_revert(version);
        Ok(fork)
    }

    /// Replace history at versions >= `version` with a completely validated
    /// detached branch. Validation failure leaves the old state unchanged. Retained
    /// history must match exactly, preventing adoption of a stale/foreign base.
    pub fn replace_suffix(&mut self, version: u32, branch: &Self) -> Result<()> {
        if self.has_pending()
            || branch.has_pending()
            || branch.version < version
            || branch.version == u32::MAX
            || self.fork(version)?.tables != branch.fork(version)?.tables
        {
            return Err(Error::SnapshotMismatch);
        }
        self.tables = branch.tables.clone();
        self.version = branch.version;
        Ok(())
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

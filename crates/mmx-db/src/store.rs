//! Common fallible owned reads for the journal and indexed MVCC backends.
use crate::{
    indexed::{Disk, Snapshot},
    Database, Error, Result, Row, Write,
};
use std::path::Path;
pub struct Store(Backend);
enum Backend {
    Journal(Database),
    Disk(Disk),
    Snapshot(Snapshot),
}
impl From<Database> for Store {
    fn from(db: Database) -> Self {
        Self(Backend::Journal(db))
    }
}
impl Store {
    pub fn memory() -> Self {
        Database::memory().into()
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Database::open(path)?.into())
    }
    pub fn open_indexed(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Backend::Disk(Disk::open(path.as_ref())?)))
    }
    pub fn version(&self) -> u32 {
        match &self.0 {
            Backend::Journal(d) => d.version(),
            Backend::Disk(d) => d.version(),
            Backend::Snapshot(s) => s.local.version(),
        }
    }
    pub fn has_pending(&self) -> bool {
        match &self.0 {
            Backend::Journal(d) => d.has_pending(),
            Backend::Disk(d) => d.has_pending(),
            Backend::Snapshot(s) => s.local.has_pending(),
        }
    }
    /// Rewrite a legacy indexed table into the large-row layout, retaining all
    /// versions and tombstones atomically. The logical format stays compatible.
    /// Requires a primary indexed writer with no pending writes or snapshots.
    pub fn optimize_layout(&mut self) -> Result<bool> {
        match &mut self.0 {
            Backend::Disk(d) => d.optimize_layout(),
            Backend::Journal(_) | Backend::Snapshot(_) => Err(Error::SnapshotMismatch),
        }
    }
    pub fn get(&self, t: &str, k: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        match &self.0 {
            Backend::Journal(d) => Ok(d.get(t, k, max).map(ToOwned::to_owned)),
            Backend::Disk(d) => d.get(t, k, max),
            Backend::Snapshot(s) => s.get(t, k, max),
        }
    }
    pub fn scan(&self, t: &str, prefix: &[u8], max: Option<u32>) -> Result<Vec<Row>> {
        match &self.0 {
            Backend::Journal(d) => Ok(d.scan(t, prefix, max)),
            Backend::Disk(d) => d.scan(t, Some(prefix), &[], None, max),
            Backend::Snapshot(s) => s.scan(t, Some(prefix), &[], None, max),
        }
    }
    pub fn scan_range(
        &self,
        t: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        match &self.0 {
            Backend::Journal(d) => Ok(d.scan_range(t, start, end, max)),
            Backend::Disk(d) => d.scan(t, None, start, end, max),
            Backend::Snapshot(s) => s.scan(t, None, start, end, max),
        }
    }
    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        match &mut self.0 {
            Backend::Journal(d) => d.apply(writes),
            Backend::Disk(d) => d.apply(writes),
            Backend::Snapshot(s) => s.local.apply(writes),
        }
    }
    pub fn insert(&mut self, t: &str, k: &[u8], v: &[u8]) -> Result<()> {
        self.apply([Write::put(t, k, v)])
    }
    pub fn remove(&mut self, t: &str, k: &[u8]) -> Result<()> {
        self.apply([Write::delete(t, k)])
    }
    pub fn commit(&mut self, next: u32) -> Result<()> {
        self.commit_batch(next, [])
    }
    pub fn commit_batch(
        &mut self,
        next: u32,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<()> {
        match &mut self.0 {
            Backend::Journal(d) => d.commit_batch(next, writes),
            Backend::Disk(d) => d.commit_batch(next, writes),
            Backend::Snapshot(s) => s.local.commit_batch(next, writes),
        }
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        match &mut self.0 {
            Backend::Journal(d) => d.revert(version),
            Backend::Disk(d) => d.revert(version),
            Backend::Snapshot(s) => s.revert(version),
        }
    }
    pub fn fork(&self, version: u32) -> Result<Self> {
        Ok(Self(match &self.0 {
            Backend::Journal(d) => Backend::Journal(d.fork(version)?),
            Backend::Disk(d) => Backend::Snapshot(d.fork(version)?),
            Backend::Snapshot(s) => Backend::Snapshot(s.fork(version)?),
        }))
    }
    pub fn replace_suffix(&mut self, version: u32, branch: &Self) -> Result<()> {
        match (&mut self.0, &branch.0) {
            (Backend::Journal(d), Backend::Journal(b)) => d.replace_suffix(version, b),
            (Backend::Disk(d), Backend::Snapshot(b)) => d.replace(version, b),
            _ => Err(Error::SnapshotMismatch),
        }
    }
}

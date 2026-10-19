//! Common fallible reads for native persistent storage and in-memory branches.
use crate::{Database, Error, Result, Row, Write};
use std::path::Path;
pub struct Store(Backend);
enum Backend {
    Memory(Database),
    Native(crate::native::Native),
    NativeSnapshot(crate::native::Snapshot),
}
impl From<Database> for Store {
    fn from(db: Database) -> Self {
        Self(Backend::Memory(db))
    }
}
impl Store {
    pub fn memory() -> Self {
        Database::memory().into()
    }
    /// Open the native append-only database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_native(path)
    }
    pub fn open_native(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Backend::Native(crate::native::Native::open(path)?)))
    }
    pub fn open_native_with_options(
        path: impl AsRef<Path>,
        options: crate::native::Options,
    ) -> Result<Self> {
        Ok(Self(Backend::Native(
            crate::native::Native::open_with_options(path, options)?,
        )))
    }
    /// Earliest allowed database version for rollback (block height + 1).
    pub fn retention_floor(&self) -> u32 {
        match &self.0 {
            Backend::Native(d) => d.retention_floor(),
            Backend::NativeSnapshot(s) => s.retention_floor(),
            _ => 0,
        }
    }
    pub fn purge_before(&mut self, version: u32) -> Result<crate::native::PurgeReport> {
        match &mut self.0 {
            Backend::Native(d) => d.purge_before(version),
            _ => Err(Error::SnapshotMismatch),
        }
    }
    pub fn collection_tag(&self) -> Option<u64> {
        match &self.0 {
            Backend::Native(d) => d.collection_tag(),
            _ => None,
        }
    }
    pub fn purge_before_with_tag(
        &mut self,
        version: u32,
        tag: u64,
        keep: impl FnMut(&str, &[u8]) -> Result<bool>,
    ) -> Result<crate::native::PurgeReport> {
        match &mut self.0 {
            Backend::Native(d) => d.purge_before_with_tag(version, tag, keep),
            _ => Err(Error::SnapshotMismatch),
        }
    }
    pub fn purge_before_with(
        &mut self,
        version: u32,
        keep: impl FnMut(&str, &[u8]) -> Result<bool>,
    ) -> Result<crate::native::PurgeReport> {
        match &mut self.0 {
            Backend::Native(d) => d.purge_before_with(version, keep),
            _ => Err(Error::SnapshotMismatch),
        }
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        match &mut self.0 {
            Backend::Native(d) => d.checkpoint(),
            _ => Err(Error::SnapshotMismatch),
        }
    }
    pub fn version(&self) -> u32 {
        match &self.0 {
            Backend::Memory(d) => d.version(),
            Backend::Native(d) => d.version(),
            Backend::NativeSnapshot(s) => s.version(),
        }
    }
    pub fn has_pending(&self) -> bool {
        match &self.0 {
            Backend::Memory(d) => d.has_pending(),
            Backend::Native(d) => d.has_pending(),
            Backend::NativeSnapshot(s) => s.has_pending(),
        }
    }
    pub fn get(&self, t: &str, k: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        match &self.0 {
            Backend::Memory(d) => Ok(d.get(t, k, max).map(ToOwned::to_owned)),
            Backend::Native(d) => d.get(t, k, max),
            Backend::NativeSnapshot(s) => s.get(t, k, max),
        }
    }
    pub fn get_bounded(
        &self,
        t: &str,
        k: &[u8],
        max: Option<u32>,
        budget: &mut crate::ReadBudget,
    ) -> Result<Option<Vec<u8>>> {
        match &self.0 {
            Backend::Memory(d) => d.get_bounded(t, k, max, budget),
            Backend::Native(d) => d.get_bounded(t, k, max, budget),
            Backend::NativeSnapshot(s) => s.get_bounded(t, k, max, budget),
        }
    }
    pub fn scan(&self, t: &str, prefix: &[u8], max: Option<u32>) -> Result<Vec<Row>> {
        match &self.0 {
            Backend::Memory(d) => Ok(d.scan(t, prefix, max)),
            Backend::Native(d) => d.scan(t, prefix, max),
            Backend::NativeSnapshot(s) => s.scan(t, prefix, max),
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
            Backend::Memory(d) => Ok(d.scan_range(t, start, end, max)),
            Backend::Native(d) => d.scan_range(t, start, end, max),
            Backend::NativeSnapshot(s) => s.scan_range(t, start, end, max),
        }
    }
    pub fn scan_range_bounded(
        &self,
        t: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
        budget: &mut crate::ReadBudget,
    ) -> Result<Vec<Row>> {
        match &self.0 {
            Backend::Memory(d) => d.scan_range_bounded(t, start, end, max, budget),
            Backend::Native(d) => d.scan_range_bounded(t, start, end, max, budget),
            Backend::NativeSnapshot(s) => s.scan_range_bounded(t, start, end, max, budget),
        }
    }
    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        match &mut self.0 {
            Backend::Memory(d) => d.apply(writes),
            Backend::Native(d) => d.apply(writes),
            Backend::NativeSnapshot(s) => s.apply(writes),
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
            Backend::Memory(d) => d.commit_batch(next, writes),
            Backend::Native(d) => d.commit_batch(next, writes),
            Backend::NativeSnapshot(s) => s.commit_batch(next, writes),
        }
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        match &mut self.0 {
            Backend::Memory(d) => d.revert(version),
            Backend::Native(d) => d.revert(version),
            Backend::NativeSnapshot(s) => s.revert(version),
        }
    }
    pub fn fork(&self, version: u32) -> Result<Self> {
        Ok(Self(match &self.0 {
            Backend::Memory(d) => Backend::Memory(d.fork(version)?),
            Backend::Native(d) => Backend::NativeSnapshot(d.fork(version)?),
            Backend::NativeSnapshot(s) => Backend::NativeSnapshot(s.fork(version)?),
        }))
    }
    pub fn replace_suffix(&mut self, version: u32, branch: &Self) -> Result<()> {
        match (&mut self.0, &branch.0) {
            (Backend::Memory(d), Backend::Memory(b)) => d.replace_suffix(version, b),
            (Backend::Native(d), Backend::NativeSnapshot(b)) => d.replace_suffix(version, b),
            _ => Err(Error::SnapshotMismatch),
        }
    }
    /// Whether a detached execution result still belongs to this exact base.
    pub fn can_replace_suffix(&self, version: u32, branch: &Self) -> Result<bool> {
        match (&self.0, &branch.0) {
            (Backend::Native(d), Backend::NativeSnapshot(b)) => d.can_replace_suffix(version, b),
            (Backend::Memory(d), Backend::Memory(b)) => {
                if d.has_pending()
                    || b.has_pending()
                    || version > d.version()
                    || version > b.version()
                {
                    return Ok(false);
                }
                Ok(d.fork(version)?.tables == b.fork(version)?.tables)
            }
            _ => Ok(false),
        }
    }
}

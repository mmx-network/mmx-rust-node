//! Versioned append-only storage. Values stay in commit log segments; immutable
//! sorted files contain only references. Forks share those files and keep their
//! speculative suffix in RAM. A manifest checkpoints a bounded journal tail.
pub mod import;
mod manifest;
use crate::{
    journal::{Action, Change, Indexed, Journal, Reader, ValueRef},
    sorted::{self, Builder, Entry, Run, View},
    validate, Database, Error, Key, Pending, Result, Row, Write,
};
use manifest::Manifest;
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};
type OrderKey = (String, Key, Reverse<u32>);
type Memtable = BTreeMap<OrderKey, Option<ValueRef>>;
type Rows = BTreeMap<Key, (u32, Option<ValueRef>)>;

#[derive(Clone, Copy)]
pub struct Options {
    pub memtable_bytes: usize,
    pub segment_bytes: u64,
    pub level_factor: usize,
    pub reader_cache: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            memtable_bytes: 4 * 1024 * 1024,
            segment_bytes: 64 * 1024 * 1024,
            level_factor: 4,
            reader_cache: 64,
        }
    }
}
struct Cache {
    clock: u64,
    readers: BTreeMap<u64, (u64, Reader)>,
}
struct Context {
    path: PathBuf,
    _lock: File,
    cache: Arc<Mutex<Cache>>,
    active: Mutex<(u64, Reader)>,
    cache_limit: usize,
}
struct DataFile {
    path: PathBuf,
    id: u64,
    retired: AtomicBool,
    cache: Weak<Mutex<Cache>>,
}
impl Drop for DataFile {
    fn drop(&mut self) {
        if self.retired.load(Ordering::Relaxed) {
            if let Some(cache) = self.cache.upgrade() {
                if let Ok(mut cache) = cache.lock() {
                    cache.readers.remove(&self.id);
                }
            }
            let _ = fs::remove_file(&self.path);
        }
    }
}
struct RunFile {
    id: u64,
    path: PathBuf,
    run: Run,
    retired: AtomicBool,
}
impl Drop for RunFile {
    fn drop(&mut self) {
        if self.retired.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
#[derive(Clone)]
struct RunView {
    file: Arc<RunFile>,
    cutoff: u32,
}
#[derive(Clone)]
struct State {
    version: u32,
    generation: u64,
    floor: u32,
    mem: Arc<Memtable>,
    mem_bytes: usize,
    runs: Vec<RunView>,
    files: Arc<BTreeMap<u64, Arc<DataFile>>>,
}
pub struct Native {
    context: Arc<Context>,
    state: State,
    journal: Journal,
    next_id: u64,
    active: u64,
    checkpoint_generation: u64,
    collected_floor: u32,
    collection_tag: u64,
    pending: Pending,
    poisoned: bool,
    options: Options,
}
pub struct Snapshot {
    context: Arc<Context>,
    base: State,
    cutoff: u32,
    local: Database,
}
fn data_path(path: &Path, id: u64) -> PathBuf {
    path.join(format!("native-data-{id:016X}.log"))
}
fn run_path(path: &Path, id: u64) -> PathBuf {
    path.join(format!("native-index-{id:016X}.sst"))
}
impl Context {
    fn value(&self, files: &BTreeMap<u64, Arc<DataFile>>, reference: ValueRef) -> Result<Vec<u8>> {
        let file = files
            .get(&reference.file_id)
            .ok_or(Error::Corrupt("missing native value file"))?;
        let active = self
            .active
            .lock()
            .map_err(|_| Error::Corrupt("native reader lock"))?;
        if reference.file_id == active.0 {
            let reader = active.1.clone();
            drop(active);
            return reader.value(reference);
        }
        drop(active);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| Error::Corrupt("native reader cache"))?;
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        if !cache.readers.contains_key(&file.id) {
            if cache.readers.len() >= self.cache_limit {
                if let Some(id) = cache
                    .readers
                    .iter()
                    .min_by_key(|(_, v)| v.0)
                    .map(|(id, _)| *id)
                {
                    cache.readers.remove(&id);
                }
            }
            cache
                .readers
                .insert(file.id, (clock, Reader::open(&file.path, file.id)?));
        }
        let entry = cache.readers.get_mut(&file.id).unwrap();
        entry.0 = clock;
        let reader = entry.1.clone();
        drop(cache);
        reader.value(reference)
    }
}
impl State {
    fn check_read(&self, max: Option<u32>) -> Result<()> {
        if max.is_some_and(|v| v < self.floor.saturating_sub(1)) {
            return Err(Error::Pruned {
                floor: self.floor,
                requested: max.unwrap(),
            });
        }
        Ok(())
    }
    fn check_revert(&self, version: u32) -> Result<()> {
        if version < self.floor {
            return Err(Error::Pruned {
                floor: self.floor,
                requested: version,
            });
        }
        if version > self.version {
            return Err(Error::Version {
                current: self.version,
                requested: version,
            });
        }
        Ok(())
    }
    fn get(&self, context: &Context, table: &str, key: &[u8], max: u32) -> Result<Option<Vec<u8>>> {
        let mut latest = None;
        if let Some(((t, k, Reverse(version)), value)) = self
            .mem
            .range((table.into(), Key(key.to_vec()), Reverse(max))..)
            .next()
        {
            if t == table && k.0 == key {
                latest = Some((*version, *value));
            }
        }
        for view in self.runs.iter().rev() {
            if view.cutoff == 0 {
                continue;
            }
            let limit = max
                .min(view.cutoff - 1)
                .min(view.file.run.metadata().max_version);
            if latest
                .as_ref()
                .is_some_and(|(version, _)| *version >= limit)
            {
                continue;
            }
            if let Some(entry) = view.file.run.get(table, key, limit)? {
                if latest
                    .as_ref()
                    .is_some_and(|(version, _)| *version >= entry.version)
                {
                    continue;
                }
                let value = entry
                    .write
                    .value
                    .map(|v| ValueRef::decode(&v))
                    .transpose()?;
                latest = Some((entry.version, value));
            }
        }
        latest
            .and_then(|(_, value)| value)
            .map(|value| context.value(&self.files, value))
            .transpose()
    }
    fn rows(
        &self,
        table: &str,
        prefix: Option<&[u8]>,
        start: &[u8],
        end: Option<&[u8]>,
        max: u32,
    ) -> Result<Rows> {
        let start_key = Key(start.to_vec());
        let end_key = end.map(|v| Key(v.to_vec()));
        if end_key.as_ref().is_some_and(|v| *v <= start_key) {
            return Ok(Rows::new());
        }
        let mut rows = Rows::new();
        let mut insert = |key: Vec<u8>, version: u32, value: Option<ValueRef>| {
            let key = Key(key);
            if prefix.is_none_or(|p| key.0.starts_with(p))
                && rows.get(&key).is_none_or(|(old, _)| *old <= version)
            {
                rows.insert(key, (version, value));
            }
        };
        for view in &self.runs {
            for entry in view.file.run.entries_from(table, start)? {
                let entry = entry?;
                if entry.write.table != table
                    || end_key
                        .as_ref()
                        .is_some_and(|end| Key(entry.write.key.clone()) >= *end)
                {
                    break;
                }
                if entry.version <= max && entry.version < view.cutoff {
                    insert(
                        entry.write.key,
                        entry.version,
                        entry
                            .write
                            .value
                            .map(|v| ValueRef::decode(&v))
                            .transpose()?,
                    );
                }
            }
        }
        for ((t, key, Reverse(version)), value) in self
            .mem
            .range((table.into(), start_key, Reverse(u32::MAX))..)
        {
            if t != table || end_key.as_ref().is_some_and(|end| key >= end) {
                break;
            }
            if *version <= max {
                insert(key.0.clone(), *version, *value);
            }
        }
        Ok(rows)
    }
    fn apply(&mut self, action: Action, entries: Vec<Indexed>) -> Result<()> {
        if action.expected != self.version
            || self.generation.checked_add(1) != Some(action.generation)
            || action.floor < self.floor
            || action.cut < self.floor
        {
            return Err(Error::Corrupt("native recovered transition"));
        }
        let mem = Arc::make_mut(&mut self.mem);
        if action.cut < self.version {
            mem.retain(|(_, _, Reverse(v)), _| *v < action.cut);
            for view in &mut self.runs {
                view.cutoff = view.cutoff.min(action.cut);
            }
        }
        for entry in entries {
            if entry
                .value
                .is_some_and(|v| !self.files.contains_key(&v.file_id))
            {
                return Err(Error::Corrupt("native recovered reference"));
            }
            mem.insert(
                (entry.table, Key(entry.key), Reverse(entry.version)),
                entry.value,
            );
        }
        self.mem_bytes = mem
            .iter()
            .map(|((t, k, _), value)| t.len() + k.0.len() + 20 + value.map_or(0, |_| 24))
            .sum();
        self.version = action.next;
        self.generation = action.generation;
        self.floor = action.floor;
        Ok(())
    }
}
impl Native {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, Options::default())
    }
    pub fn open_with_options(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        if options.memtable_bytes == 0
            || options.segment_bytes < 8
            || options.level_factor < 2
            || options.reader_cache == 0
        {
            return Err(Error::InvalidEntry);
        }
        let path = std::path::absolute(path)?;
        if path.join("native.import").exists() {
            return Err(Error::Corrupt(
                "native import must be completed before opening",
            ));
        }
        if path.join("state.sqlite").exists() || path.join("journal.mmx").exists() {
            return Err(Error::Corrupt(
                "native database requires explicit legacy import",
            ));
        }
        let mut parents = Vec::new();
        for dir in path.ancestors().take_while(|p| !p.exists()) {
            if let Some(parent) = dir.parent() {
                parents.push(parent.to_owned());
            }
        }
        fs::create_dir_all(&path)?;
        for parent in parents.iter().rev() {
            File::open(parent)?.sync_all()?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("writer.lock"))?;
        lock.try_lock().map_err(std::io::Error::from)?;
        let meta_path = path.join("native.manifest");
        let manifest = if meta_path.exists() {
            Manifest::read(&meta_path)?
        } else {
            // A new native directory cannot silently adopt foreign data files.
            if fs::read_dir(&path)?.any(|entry| {
                entry.is_ok_and(|e| {
                    e.file_name() != "writer.lock"
                        && !manifest_temporary(&e.file_name().to_string_lossy())
                })
            }) {
                return Err(Error::Corrupt("nonempty native database directory"));
            }
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                if manifest_temporary(&entry.file_name().to_string_lossy())
                    && entry.file_type()?.is_file()
                {
                    fs::remove_file(entry.path())?;
                }
            }
            let manifest = Manifest {
                version: 0,
                generation: 0,
                floor: 0,
                collected_floor: 0,
                collection_tag: 0,
                next_id: 1,
                active: 0,
                cursor: 8,
                files: vec![0],
                runs: vec![],
            };
            manifest.save(&meta_path, true)?;
            manifest
        };
        // Initial metadata becomes durable before the empty value file. A
        // crash in that gap can only recreate this specific empty database.
        if !data_path(&path, 0).exists()
            && manifest.version == 0
            && manifest.generation == 0
            && manifest.next_id == 1
            && manifest.active == 0
            && manifest.cursor == 8
            && manifest.files == [0]
            && manifest.runs.is_empty()
        {
            if fs::read_dir(&path)?.any(|entry| {
                entry.is_ok_and(|e| {
                    e.file_name() != "writer.lock"
                        && e.file_name() != "native.manifest"
                        && !manifest_temporary(&e.file_name().to_string_lossy())
                })
            }) {
                return Err(Error::Corrupt(
                    "incomplete native initialization has other files",
                ));
            }
            drop(Journal::open(&data_path(&path, 0), 0)?);
        }
        let cache = Arc::new(Mutex::new(Cache {
            clock: 0,
            readers: BTreeMap::new(),
        }));
        let mut files = BTreeMap::new();
        for id in &manifest.files {
            let file = data_path(&path, *id);
            if !file.is_file() {
                return Err(Error::Corrupt("missing native data file"));
            }
            files.insert(
                *id,
                Arc::new(DataFile {
                    path: file,
                    id: *id,
                    retired: AtomicBool::new(false),
                    cache: Arc::downgrade(&cache),
                }),
            );
        }
        let mut runs = Vec::new();
        for (id, cutoff) in &manifest.runs {
            let file = run_path(&path, *id);
            let run = Run::open(&file)?;
            runs.push(RunView {
                file: Arc::new(RunFile {
                    id: *id,
                    path: file,
                    run,
                    retired: AtomicBool::new(false),
                }),
                cutoff: *cutoff,
            });
        }
        let mut journal = Journal::open(&data_path(&path, manifest.active), manifest.active)?;
        let recovered = journal.recover_from(manifest.cursor)?;
        let context = Arc::new(Context {
            path: path.clone(),
            _lock: lock,
            cache,
            active: Mutex::new((manifest.active, journal.reader())),
            cache_limit: options.reader_cache,
        });
        let mut state = State {
            version: manifest.version,
            generation: manifest.generation,
            floor: manifest.floor,
            mem: Arc::new(Memtable::new()),
            mem_bytes: 0,
            runs,
            files: Arc::new(files),
        };
        for transaction in recovered {
            state.apply(transaction.action, transaction.entries)?;
        }
        let native = Self {
            context,
            state,
            journal,
            next_id: manifest.next_id,
            active: manifest.active,
            checkpoint_generation: manifest.generation,
            collected_floor: manifest.collected_floor,
            collection_tag: manifest.collection_tag,
            pending: Pending::new(),
            poisoned: false,
            options,
        };
        native.clean_orphans()?;
        Ok(native)
    }
    fn clean_orphans(&self) -> Result<()> {
        for entry in fs::read_dir(&self.context.path)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if manifest_temporary(name) && entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
                continue;
            }
            let index = name
                .strip_prefix("native-index-")
                .and_then(|v| v.strip_suffix(".sst"));
            let data = name
                .strip_prefix("native-data-")
                .and_then(|v| v.strip_suffix(".log"));
            if let Some(id) = index
                .or(data)
                .filter(|v| v.len() == 16)
                .and_then(|v| u64::from_str_radix(v, 16).ok())
            {
                let retained = if index.is_some() {
                    self.state.runs.iter().any(|v| v.file.id == id)
                } else {
                    self.state.files.contains_key(&id)
                };
                if !retained && entry.file_type()?.is_file() {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }
    fn readable(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
    pub fn version(&self) -> u32 {
        self.state.version
    }
    pub fn retention_floor(&self) -> u32 {
        self.state.floor
    }
    /// Caller-defined retention policy completed at the current durable floor.
    /// A floor published before an interrupted collection has no current tag.
    pub fn collection_tag(&self) -> Option<u64> {
        (self.collected_floor == self.state.floor).then_some(self.collection_tag)
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn get(&self, table: &str, key: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        self.readable()?;
        self.state.check_read(max)?;
        if max.is_none_or(|v| v >= self.state.version) {
            if let Some(value) = self.pending.get(&(table.into(), Key(key.into()))) {
                return Ok(value.clone());
            }
        }
        self.state
            .get(&self.context, table, key, max.unwrap_or(u32::MAX))
    }
    pub fn scan(&self, table: &str, prefix: &[u8], max: Option<u32>) -> Result<Vec<Row>> {
        self.scan_impl(table, Some(prefix), &[], None, max)
    }
    pub fn scan_range(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.scan_impl(table, None, start, end, max)
    }
    fn scan_impl(
        &self,
        table: &str,
        prefix: Option<&[u8]>,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.readable()?;
        self.state.check_read(max)?;
        let rows = self
            .state
            .rows(table, prefix, start, end, max.unwrap_or(u32::MAX))?;
        let mut values = resolve(&self.context, &self.state, rows)?;
        overlay(
            &mut values,
            &self.pending,
            table,
            prefix,
            start,
            end,
            self.state.version,
            max,
        );
        Ok(visible(values))
    }
    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        self.readable()?;
        let writes = writes.into_iter().collect::<Vec<_>>();
        for write in &writes {
            validate(write)?;
        }
        for write in writes {
            self.pending
                .insert((write.table, Key(write.key)), write.value);
        }
        Ok(())
    }
    pub fn commit_batch(
        &mut self,
        next: u32,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<()> {
        self.readable()?;
        if next <= self.state.version || next == u32::MAX {
            return Err(Error::Version {
                current: self.state.version,
                requested: next,
            });
        }
        let mut pending = self.pending.clone();
        for write in writes {
            validate(&write)?;
            pending.insert((write.table, Key(write.key)), write.value);
        }
        let changes = pending
            .iter()
            .map(|((t, k), v)| Change {
                version: self.state.version,
                table: t,
                key: &k.0,
                value: v.as_deref(),
            })
            .collect::<Vec<_>>();
        self.publish(self.state.version, next, self.state.floor, &changes)?;
        self.pending.clear();
        Ok(())
    }
    fn publish(&mut self, cut: u32, next: u32, floor: u32, changes: &[Change<'_>]) -> Result<()> {
        let action = Action {
            generation: self
                .state
                .generation
                .checked_add(1)
                .ok_or(Error::InvalidEntry)?,
            expected: self.state.version,
            cut,
            next,
            floor,
        };
        let result = (|| {
            let entries = self.journal.append(action, changes)?;
            self.state.apply(action, entries)?;
            if self.state.mem_bytes >= self.options.memtable_bytes
                || self.journal.end() >= self.options.segment_bytes
                || self.state.generation - self.checkpoint_generation >= 100000
            {
                self.checkpoint_inner(false)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        self.readable()?;
        self.state.check_revert(version)?;
        self.publish(version, version, self.state.floor, &[])?;
        self.pending.clear();
        Ok(())
    }
    pub fn fork(&self, version: u32) -> Result<Snapshot> {
        self.readable()?;
        self.state.check_revert(version)?;
        if self.has_pending() {
            return Err(Error::SnapshotMismatch);
        }
        let mut local = Database::memory();
        local.version = version;
        Ok(Snapshot {
            context: self.context.clone(),
            base: self.state.clone(),
            cutoff: version,
            local,
        })
    }
    pub fn replace_suffix(&mut self, version: u32, branch: &Snapshot) -> Result<()> {
        self.readable()?;
        self.state.check_revert(version)?;
        if self.has_pending()
            || branch.local.has_pending()
            || branch.cutoff != version
            || !Arc::ptr_eq(&self.context, &branch.context)
            || self.state.generation != branch.base.generation
        {
            return Err(Error::SnapshotMismatch);
        }
        let changes = branch
            .local
            .tables
            .iter()
            .flat_map(|(t, rows)| {
                rows.iter().flat_map(move |(k, versions)| {
                    versions.iter().map(move |(v, value)| Change {
                        version: *v,
                        table: t,
                        key: &k.0,
                        value: value.as_deref(),
                    })
                })
            })
            .collect::<Vec<_>>();
        self.publish(version, branch.local.version, self.state.floor, &changes)
    }
    /// Discard older history, preserving state at version-1 and all later
    /// versions. Existing snapshots pin old files until they are released.
    pub fn purge_before(&mut self, version: u32) -> Result<PurgeReport> {
        self.readable()?;
        self.state.check_revert(version)?;
        if self.has_pending() {
            return Err(Error::SnapshotMismatch);
        }
        if version == self.state.floor && version == self.collected_floor {
            let bytes = value_bytes(&self.state)?;
            return Ok(PurgeReport {
                floor: version,
                old_value_bytes: bytes,
                new_value_bytes: bytes,
                retired_files: 0,
                retained_records: self
                    .state
                    .runs
                    .iter()
                    .map(|r| r.file.run.metadata().records)
                    .sum::<u64>()
                    + self.state.mem.len() as u64,
            });
        }
        self.purge_before_with(version, |_, _| Ok(true))
    }
    /// A caller can additionally retire complete immutable key spaces, such as
    /// old block bodies. The caller must keep its private consensus context and
    /// independently enforce the RPC boundary. The predicate runs once per key.
    pub fn purge_before_with(
        &mut self,
        version: u32,
        keep: impl FnMut(&str, &[u8]) -> Result<bool>,
    ) -> Result<PurgeReport> {
        self.purge_before_with_tag(version, 0, keep)
    }
    /// Record a caller's policy only after the collected files are durable.
    /// Unlike `purge_before`, predicates may change at the same version, so the
    /// caller checks `collection_tag()` when it wants to avoid a repeated copy.
    pub fn purge_before_with_tag(
        &mut self,
        version: u32,
        tag: u64,
        mut keep: impl FnMut(&str, &[u8]) -> Result<bool>,
    ) -> Result<PurgeReport> {
        self.readable()?;
        self.state.check_revert(version)?;
        if self.has_pending() {
            return Err(Error::SnapshotMismatch);
        }
        let result = (|| {
            self.checkpoint_inner(false)?;
            if version > self.state.floor {
                self.publish(self.state.version, self.state.version, version, &[])?;
            }
            self.collect_history(tag, &mut keep)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn collect_history(
        &mut self,
        tag: u64,
        keep: &mut impl FnMut(&str, &[u8]) -> Result<bool>,
    ) -> Result<PurgeReport> {
        let source = self.state.clone();
        let old_value_bytes = value_bytes(&source)?;
        let mut next_id = self.next_id;
        let index_id = take_id(&mut next_id)?;
        let index_path = run_path(&self.context.path, index_id);
        let mut builder = Builder::new(&self.context.path, 0)?;
        let views = source
            .runs
            .iter()
            .map(|v| View {
                run: &v.file.run,
                cutoff: v.cutoff,
            })
            .collect::<Vec<_>>();
        let mut files = BTreeMap::new();
        let mut batch = Vec::new();
        let mut batch_bytes = 0;
        let limit = self.options.segment_bytes.clamp(4096, 64 * 1024 * 1024) as usize;
        let mut last_key = None;
        let mut keep_key = true;
        let mut count = 0;
        for entry in sorted::Merged::new(&views, (source.floor > 0).then_some(source.floor))? {
            let entry = entry?;
            let key = (entry.write.table.clone(), Key(entry.write.key.clone()));
            if last_key.as_ref() != Some(&key) {
                keep_key = keep(&entry.write.table, &entry.write.key)?;
                last_key = Some(key);
            }
            if !keep_key {
                continue;
            }
            let value = entry
                .write
                .value
                .as_ref()
                .map(|encoded| {
                    self.context
                        .value(&source.files, ValueRef::decode(encoded)?)
                })
                .transpose()?;
            let size = entry.write.table.len()
                + entry.write.key.len()
                + 32
                + value.as_ref().map_or(0, Vec::len);
            if !batch.is_empty() && (batch_bytes + size > limit || batch.len() >= 65536) {
                write_gc_batch(
                    &self.context,
                    source.version,
                    &mut next_id,
                    &mut batch,
                    &mut files,
                    &mut builder,
                )?;
                batch_bytes = 0;
            }
            batch_bytes += size;
            batch.push((entry, value));
            count += 1;
        }
        if !batch.is_empty() {
            write_gc_batch(
                &self.context,
                source.version,
                &mut next_id,
                &mut batch,
                &mut files,
                &mut builder,
            )?;
        }
        let runs = if count == 0 {
            drop(builder);
            Vec::new()
        } else {
            let run = builder.finish(&index_path)?;
            vec![RunView {
                file: Arc::new(RunFile {
                    id: index_id,
                    path: index_path,
                    run,
                    retired: AtomicBool::new(false),
                }),
                cutoff: u32::MAX,
            }]
        };
        let active = take_id(&mut next_id)?;
        let path = data_path(&self.context.path, active);
        let journal = Journal::open(&path, active)?;
        files.insert(
            active,
            Arc::new(DataFile {
                path,
                id: active,
                retired: AtomicBool::new(false),
                cache: Arc::downgrade(&self.context.cache),
            }),
        );
        let state = State {
            version: source.version,
            generation: source.generation,
            floor: source.floor,
            mem: Arc::new(Memtable::new()),
            mem_bytes: 0,
            runs,
            files: Arc::new(files),
        };
        let manifest = Manifest {
            version: state.version,
            generation: state.generation,
            floor: state.floor,
            collected_floor: state.floor,
            collection_tag: tag,
            next_id,
            active,
            cursor: 8,
            files: state.files.keys().copied().collect(),
            runs: state.runs.iter().map(|v| (v.file.id, v.cutoff)).collect(),
        };
        manifest.save(&self.context.path.join("native.manifest"), false)?;
        *self
            .context
            .active
            .lock()
            .map_err(|_| Error::Corrupt("native active reader"))? = (active, journal.reader());
        self.journal = journal;
        self.state = state;
        self.next_id = next_id;
        self.active = active;
        self.checkpoint_generation = self.state.generation;
        self.collected_floor = self.state.floor;
        self.collection_tag = tag;
        for file in source.runs.iter().map(|v| &v.file) {
            file.retired.store(true, Ordering::Relaxed);
        }
        for file in source.files.values() {
            file.retired.store(true, Ordering::Relaxed);
        }
        let new_value_bytes = value_bytes(&self.state)?;
        Ok(PurgeReport {
            floor: self.state.floor,
            old_value_bytes,
            new_value_bytes,
            retired_files: source.files.len(),
            retained_records: count,
        })
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        self.readable()?;
        if self.has_pending() {
            return Err(Error::SnapshotMismatch);
        }
        let result = self.checkpoint_inner(false);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn checkpoint_inner(&mut self, purge: bool) -> Result<()> {
        let mut state = self.state.clone();
        let mut next_id = self.next_id;
        let mut removed = Vec::new();
        if !state.mem.is_empty() {
            let id = next_id;
            next_id = next_id.checked_add(1).ok_or(Error::InvalidEntry)?;
            let mut builder = Builder::new(&self.context.path, 0)?;
            for ((t, k, Reverse(v)), value) in state.mem.iter() {
                builder.push(&Entry {
                    version: *v,
                    write: Write {
                        table: t.clone(),
                        key: k.0.clone(),
                        value: value.map(|r| r.encode().to_vec()),
                    },
                })?;
            }
            let path = run_path(&self.context.path, id);
            let run = builder.finish(&path)?;
            state.runs.push(RunView {
                file: Arc::new(RunFile {
                    id,
                    path,
                    run,
                    retired: AtomicBool::new(false),
                }),
                cutoff: u32::MAX,
            });
            state.mem = Arc::new(Memtable::new());
            state.mem_bytes = 0;
        }
        loop {
            let group = if purge && state.runs.len() > 1 {
                Some((
                    0,
                    state.runs.len(),
                    state
                        .runs
                        .iter()
                        .map(|v| v.file.run.metadata().level)
                        .max()
                        .unwrap(),
                ))
            } else {
                find_group(&state.runs, self.options.level_factor)
            };
            let Some((start, count, level)) = group else {
                break;
            };
            let id = next_id;
            next_id = next_id.checked_add(1).ok_or(Error::InvalidEntry)?;
            let path = run_path(&self.context.path, id);
            let views = state.runs[start..start + count]
                .iter()
                .map(|v| View {
                    run: &v.file.run,
                    cutoff: v.cutoff,
                })
                .collect::<Vec<_>>();
            let merged = sorted::merge(
                &path,
                level.checked_add(1).ok_or(Error::InvalidEntry)?,
                &views,
                (state.floor > 0).then_some(state.floor),
            )?;
            removed.extend(state.runs.drain(start..start + count).map(|v| v.file));
            if let Some(run) = merged {
                state.runs.insert(
                    start,
                    RunView {
                        file: Arc::new(RunFile {
                            id,
                            path,
                            run,
                            retired: AtomicBool::new(false),
                        }),
                        cutoff: u32::MAX,
                    },
                );
            }
            if purge {
                break;
            }
        }
        // Retiring a whole masked run needs no record rewrite.
        state.runs.retain(|view| {
            let keep = view.cutoff > view.file.run.metadata().min_version;
            if !keep {
                removed.push(view.file.clone());
            }
            keep
        });
        let mut replacement = None;
        let mut active = self.active;
        let mut cursor = self.journal.end();
        if cursor >= self.options.segment_bytes {
            active = next_id;
            next_id = next_id.checked_add(1).ok_or(Error::InvalidEntry)?;
            let path = data_path(&self.context.path, active);
            let journal = Journal::open(&path, active)?;
            cursor = 8;
            Arc::make_mut(&mut state.files).insert(
                active,
                Arc::new(DataFile {
                    path,
                    id: active,
                    retired: AtomicBool::new(false),
                    cache: Arc::downgrade(&self.context.cache),
                }),
            );
            replacement = Some(journal);
        }
        let manifest = Manifest {
            version: state.version,
            generation: state.generation,
            floor: state.floor,
            collected_floor: self.collected_floor,
            collection_tag: self.collection_tag,
            next_id,
            active,
            cursor,
            files: state.files.keys().copied().collect(),
            runs: state.runs.iter().map(|v| (v.file.id, v.cutoff)).collect(),
        };
        manifest.save(&self.context.path.join("native.manifest"), false)?;
        if let Some(journal) = replacement {
            *self
                .context
                .active
                .lock()
                .map_err(|_| Error::Corrupt("native active reader"))? = (active, journal.reader());
            self.journal = journal;
        }
        self.state = state;
        self.next_id = next_id;
        self.active = active;
        self.checkpoint_generation = self.state.generation;
        for file in removed {
            file.retired.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}
fn find_group(runs: &[RunView], factor: usize) -> Option<(usize, usize, u32)> {
    let mut start = 0;
    while start < runs.len() {
        let level = runs[start].file.run.metadata().level;
        let count = runs[start..]
            .iter()
            .take_while(|v| v.file.run.metadata().level == level)
            .count();
        if count > factor {
            return Some((start, factor, level));
        }
        start += count;
    }
    None
}
type Values = BTreeMap<Key, (u32, Option<Vec<u8>>)>;
fn resolve(context: &Context, state: &State, rows: Rows) -> Result<Values> {
    rows.into_iter()
        .map(|(key, (version, value))| {
            Ok((
                key,
                (
                    version,
                    value.map(|v| context.value(&state.files, v)).transpose()?,
                ),
            ))
        })
        .collect()
}
fn matches(key: &Key, prefix: Option<&[u8]>, start: &[u8], end: Option<&[u8]>) -> bool {
    prefix.is_none_or(|p| key.0.starts_with(p))
        && *key >= Key(start.into())
        && end.is_none_or(|end| *key < Key(end.into()))
}
#[allow(clippy::too_many_arguments)]
fn overlay(
    rows: &mut Values,
    pending: &Pending,
    table: &str,
    prefix: Option<&[u8]>,
    start: &[u8],
    end: Option<&[u8]>,
    version: u32,
    max: Option<u32>,
) {
    if max.is_none_or(|v| v >= version) {
        for ((t, key), value) in pending {
            if t == table && matches(key, prefix, start, end) {
                rows.insert(key.clone(), (version, value.clone()));
            }
        }
    }
}
fn visible(rows: Values) -> Vec<Row> {
    rows.into_iter()
        .filter_map(|(key, (version, value))| {
            Some(Row {
                key: key.0,
                version,
                value: value?,
            })
        })
        .collect()
}
impl Snapshot {
    pub fn version(&self) -> u32 {
        self.local.version()
    }
    pub fn has_pending(&self) -> bool {
        self.local.has_pending()
    }
    pub fn retention_floor(&self) -> u32 {
        self.base.floor
    }
    pub fn get(&self, table: &str, key: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        self.base.check_read(max)?;
        if let Some((_, value)) = self.local.row(table, &Key(key.into()), max) {
            return Ok(value.clone());
        }
        if self.cutoff == 0 {
            return Ok(None);
        }
        self.base.get(
            &self.context,
            table,
            key,
            max.unwrap_or(u32::MAX).min(self.cutoff - 1),
        )
    }
    pub fn scan(&self, table: &str, prefix: &[u8], max: Option<u32>) -> Result<Vec<Row>> {
        self.scan_impl(table, Some(prefix), &[], None, max)
    }
    pub fn scan_range(
        &self,
        table: &str,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.scan_impl(table, None, start, end, max)
    }
    fn scan_impl(
        &self,
        table: &str,
        prefix: Option<&[u8]>,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.base.check_read(max)?;
        let mut rows = if self.cutoff == 0 {
            Values::new()
        } else {
            resolve(
                &self.context,
                &self.base,
                self.base.rows(
                    table,
                    prefix,
                    start,
                    end,
                    max.unwrap_or(u32::MAX).min(self.cutoff - 1),
                )?,
            )?
        };
        if let Some(local) = self.local.tables.get(table) {
            for (key, versions) in local {
                if matches(key, prefix, start, end) {
                    if let Some((v, value)) = versions.range(..=max.unwrap_or(u32::MAX)).next_back()
                    {
                        rows.insert(key.clone(), (*v, value.clone()));
                    }
                }
            }
        }
        overlay(
            &mut rows,
            &self.local.pending,
            table,
            prefix,
            start,
            end,
            self.local.version,
            max,
        );
        Ok(visible(rows))
    }
    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        self.local.apply(writes)
    }
    pub fn commit_batch(
        &mut self,
        next: u32,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<()> {
        self.local.commit_batch(next, writes)
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        if version < self.base.floor {
            return Err(Error::Pruned {
                floor: self.base.floor,
                requested: version,
            });
        }
        self.local.revert(version)?;
        self.cutoff = self.cutoff.min(version);
        Ok(())
    }
    pub fn fork(&self, version: u32) -> Result<Self> {
        if version < self.base.floor {
            return Err(Error::Pruned {
                floor: self.base.floor,
                requested: version,
            });
        }
        let local = self.local.fork(version)?;
        Ok(Self {
            context: self.context.clone(),
            base: self.base.clone(),
            cutoff: self.cutoff.min(version),
            local,
        })
    }
}

fn manifest_temporary(name: &str) -> bool {
    (name.starts_with("native-manifest-") || name.starts_with("native-index-build-"))
        && name.ends_with(".tmp")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PurgeReport {
    pub floor: u32,
    pub old_value_bytes: u64,
    pub new_value_bytes: u64,
    pub retired_files: usize,
    pub retained_records: u64,
}
fn take_id(next: &mut u64) -> Result<u64> {
    let id = *next;
    *next = next.checked_add(1).ok_or(Error::InvalidEntry)?;
    Ok(id)
}
fn value_bytes(state: &State) -> Result<u64> {
    state.files.values().try_fold(0u64, |total, file| {
        total
            .checked_add(fs::metadata(&file.path)?.len())
            .ok_or(Error::InvalidEntry)
    })
}
fn write_gc_batch(
    context: &Context,
    version: u32,
    next_id: &mut u64,
    batch: &mut Vec<(Entry, Option<Vec<u8>>)>,
    files: &mut BTreeMap<u64, Arc<DataFile>>,
    builder: &mut Builder,
) -> Result<()> {
    let id = take_id(next_id)?;
    let path = data_path(&context.path, id);
    let mut journal = Journal::open(&path, id)?;
    let changes = batch
        .iter()
        .map(|(entry, value)| Change {
            version: entry.version,
            table: &entry.write.table,
            key: &entry.write.key,
            value: value.as_deref(),
        })
        .collect::<Vec<_>>();
    let entries = journal.append(
        Action {
            generation: 1,
            expected: 0,
            cut: 0,
            next: version,
            floor: 0,
        },
        &changes,
    )?;
    for entry in entries {
        builder.push(&Entry {
            version: entry.version,
            write: Write {
                table: entry.table,
                key: entry.key,
                value: entry.value.map(|r| r.encode().to_vec()),
            },
        })?;
    }
    files.insert(
        id,
        Arc::new(DataFile {
            path,
            id,
            retired: AtomicBool::new(false),
            cache: Arc::downgrade(&context.cache),
        }),
    );
    batch.clear();
    Ok(())
}

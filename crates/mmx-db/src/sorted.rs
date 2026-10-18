//! Immutable sorted files, following C++ Table's records and resident offset
//! index. Values are written sequentially; snapshots share open files and their
//! indexes. Opening a file reads its header/index, never all value payloads.
use crate::{validate, Error, Key, Result, Write, MAX_FRAME};
use std::{
    cmp::{Ordering, Reverse},
    collections::BTreeMap,
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write as IoWrite},
    path::Path,
    sync::Arc,
};

const MAGIC: &[u8; 8] = b"MMXSST\0\x01";
const HEADER: u64 = 64;
const RECORD_HEADER: usize = 18;
type OrderKey = (String, Key, Reverse<u32>);

/// `None` is a tombstone; an empty value is a distinct committed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub version: u32,
    pub write: Write,
}
impl Entry {
    fn order_key(&self) -> OrderKey {
        (
            self.write.table.clone(),
            Key(self.write.key.clone()),
            Reverse(self.version),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub level: u32,
    pub min_version: u32,
    pub max_version: u32,
    pub records: u64,
    pub data_bytes: u64,
}
struct FileData {
    file: File,
    offsets: Vec<u64>,
    metadata: Metadata,
}
#[derive(Clone)]
pub struct Run(Arc<FileData>);

/// An exclusive version limit lets a rollback mask a file's future records
/// without rewriting that file. `u32::MAX` includes every stored version.
pub struct View<'a> {
    pub run: &'a Run,
    pub cutoff: u32,
}

/// Merge chronological views with bounded memory: one current record per file.
/// Later views win duplicate table/key/version entries. A retention boundary
/// keeps all newer versions and one preceding value or tombstone per key. The
/// owning database must durably record and enforce its corresponding read and
/// rollback floor before retiring any inputs; this primitive publishes one file.
pub fn merge(
    path: &Path,
    level: u32,
    views: &[View<'_>],
    retained_from: Option<u32>,
) -> Result<Option<Run>> {
    let mut builder = Builder::new(crate::parent_dir(path), level)?;
    let mut count = 0;
    for entry in Merged::new(views, retained_from)? {
        builder.push(&entry?)?;
        count += 1;
    }
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(builder.finish(path)?))
}

/// A streaming merge for compaction, import, and value reclamation. Holds one
/// current record per input file, irrespective of the complete history size.
pub struct Merged {
    cursors: Vec<Cursor>,
    cutoffs: Vec<u32>,
    heap: BTreeMap<(OrderKey, Reverse<usize>), Entry>,
    previous: Option<OrderKey>,
    key: Option<(String, Key)>,
    baseline: bool,
    retained_from: Option<u32>,
    failed: bool,
}
impl Merged {
    pub fn new(views: &[View<'_>], retained_from: Option<u32>) -> Result<Self> {
        let mut cursors = views
            .iter()
            .map(|view| view.run.entries())
            .collect::<Vec<_>>();
        let mut heap = BTreeMap::new();
        for (index, cursor) in cursors.iter_mut().enumerate() {
            if let Some(entry) = cursor.next().transpose()? {
                heap.insert((entry.order_key(), Reverse(index)), entry);
            }
        }
        Ok(Self {
            cursors,
            cutoffs: views.iter().map(|v| v.cutoff).collect(),
            heap,
            previous: None,
            key: None,
            baseline: false,
            retained_from,
            failed: false,
        })
    }
}
impl Iterator for Merged {
    type Item = Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        while let Some(((order, Reverse(index)), entry)) = self.heap.pop_first() {
            match self.cursors[index].next().transpose() {
                Ok(Some(next)) => {
                    self.heap.insert((next.order_key(), Reverse(index)), next);
                }
                Ok(None) => {}
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
            if entry.version >= self.cutoffs[index] || self.previous.as_ref() == Some(&order) {
                continue;
            }
            if self.key.as_ref() != Some(&(order.0.clone(), order.1.clone())) {
                self.key = Some((order.0.clone(), order.1.clone()));
                self.baseline = false;
            }
            let old = self
                .retained_from
                .is_some_and(|floor| entry.version < floor);
            let keep = !old || !self.baseline;
            if old {
                self.baseline = true;
            }
            self.previous = Some(order);
            if keep {
                return Some(Ok(entry));
            }
        }
        None
    }
}

/// An unpublished file is removed on drop. Publication is an atomic rename
/// after the complete records, offset index, and checksums have reached disk.
pub struct Builder {
    temporary: tempfile::NamedTempFile,
    output: BufWriter<File>,
    offsets: Vec<u64>,
    metadata: Metadata,
    previous: Option<OrderKey>,
    poisoned: bool,
}
impl Builder {
    pub fn new(directory: &Path, level: u32) -> Result<Self> {
        let temporary = tempfile::NamedTempFile::new_in(directory)?;
        let mut output = BufWriter::with_capacity(64 * 1024, temporary.reopen()?);
        output.write_all(&[0; HEADER as usize])?;
        Ok(Self {
            temporary,
            output,
            offsets: Vec::new(),
            metadata: Metadata {
                level,
                min_version: u32::MAX,
                max_version: 0,
                records: 0,
                data_bytes: HEADER,
            },
            previous: None,
            poisoned: false,
        })
    }
    /// Tables and size-first keys ascend; versions of a key descend, as in C++.
    pub fn push(&mut self, entry: &Entry) -> Result<()> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        validate(&entry.write)?;
        if entry.version == u32::MAX {
            return Err(Error::InvalidEntry);
        }
        let key = entry.order_key();
        if self.previous.as_ref().is_some_and(|prev| *prev >= key) {
            return Err(Error::Corrupt("sorted file record order"));
        }
        let new_key = self
            .previous
            .as_ref()
            .is_none_or(|prev| (&prev.0, &prev.1) != (&key.0, &key.1));
        let result = self.write_record(entry);
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        if new_key {
            self.offsets.push(self.metadata.data_bytes);
        }
        self.metadata.data_bytes += record_size(&entry.write);
        self.metadata.records += 1;
        self.metadata.min_version = self.metadata.min_version.min(entry.version);
        self.metadata.max_version = self.metadata.max_version.max(entry.version);
        self.previous = Some(key);
        Ok(())
    }
    fn write_record(&mut self, entry: &Entry) -> Result<()> {
        let write = &entry.write;
        let mut header = Vec::with_capacity(RECORD_HEADER);
        header.extend((write.table.len() as u16).to_le_bytes());
        header.extend((write.key.len() as u32).to_le_bytes());
        header.extend(entry.version.to_le_bytes());
        header.extend(
            write
                .value
                .as_ref()
                .map_or(u32::MAX, |v| v.len() as u32)
                .to_le_bytes(),
        );
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&header);
        checksum.update(write.table.as_bytes());
        checksum.update(&write.key);
        header.extend(checksum.finalize().to_le_bytes());
        self.output.write_all(&header)?;
        self.output.write_all(write.table.as_bytes())?;
        self.output.write_all(&write.key)?;
        if let Some(value) = &write.value {
            self.output.write_all(value)?;
        }
        self.output.write_all(
            &crc32fast::hash(write.value.as_deref().unwrap_or_default()).to_le_bytes(),
        )?;
        Ok(())
    }
    pub fn finish(mut self, path: &Path) -> Result<Run> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.offsets.is_empty() {
            return Err(Error::InvalidEntry);
        }
        let mut checksum = crc32fast::Hasher::new();
        for offset in &self.offsets {
            let bytes = offset.to_le_bytes();
            checksum.update(&bytes);
            self.output.write_all(&bytes)?;
        }
        self.output.write_all(&checksum.finalize().to_le_bytes())?;
        self.output.flush()?;
        let mut header = [0; HEADER as usize];
        header[..8].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&self.metadata.records.to_le_bytes());
        header[16..24].copy_from_slice(&(self.offsets.len() as u64).to_le_bytes());
        header[24..32].copy_from_slice(&self.metadata.data_bytes.to_le_bytes());
        header[32..36].copy_from_slice(&self.metadata.min_version.to_le_bytes());
        header[36..40].copy_from_slice(&self.metadata.max_version.to_le_bytes());
        header[40..44].copy_from_slice(&self.metadata.level.to_le_bytes());
        let sum = crc32fast::hash(&header[..60]);
        header[60..].copy_from_slice(&sum.to_le_bytes());
        self.output.seek(SeekFrom::Start(0))?;
        self.output.write_all(&header)?;
        self.output.flush()?;
        self.output.get_ref().sync_all()?;
        drop(self.output);
        let file = self
            .temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
        File::open(crate::parent_dir(path))?.sync_all()?;
        // Reuse the index just written, avoiding an immediate read-back.
        Ok(Run(Arc::new(FileData {
            file,
            offsets: self.offsets,
            metadata: self.metadata,
        })))
    }
}
fn record_size(write: &Write) -> u64 {
    RECORD_HEADER as u64
        + write.table.len() as u64
        + write.key.len() as u64
        + write.value.as_ref().map_or(0, |v| v.len() as u64)
        + 4
}

/// Each cursor has its own position and shares only an immutable descriptor.
/// No seek on a cloned descriptor can change another reader's position.
struct Section {
    file: Arc<FileData>,
    position: u64,
    end: u64,
}
impl Read for Section {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let length = buffer.len().min((self.end - self.position) as usize);
        #[cfg(unix)]
        let count = {
            use std::os::unix::fs::FileExt;
            self.file
                .file
                .read_at(&mut buffer[..length], self.position)?
        };
        #[cfg(windows)]
        let count = {
            use std::os::windows::fs::FileExt;
            self.file
                .file
                .seek_read(&mut buffer[..length], self.position)?
        };
        self.position += count as u64;
        Ok(count)
    }
}
struct RecordKey {
    table: String,
    key: Vec<u8>,
    version: u32,
    value_size: Option<usize>,
}
fn read_key(input: &mut impl Read) -> Result<RecordKey> {
    let mut header = [0; RECORD_HEADER];
    input.read_exact(&mut header)?;
    let table_size = u16::from_le_bytes(header[..2].try_into().unwrap()) as usize;
    let key_size = u32::from_le_bytes(header[2..6].try_into().unwrap()) as usize;
    let version = u32::from_le_bytes(header[6..10].try_into().unwrap());
    let value_size = u32::from_le_bytes(header[10..14].try_into().unwrap());
    if table_size == 0
        || key_size > MAX_FRAME
        || (value_size != u32::MAX && value_size as usize > MAX_FRAME)
        || version == u32::MAX
    {
        return Err(Error::Corrupt("sorted file record size/version"));
    }
    let mut table = vec![0; table_size];
    let mut key = vec![0; key_size];
    input.read_exact(&mut table)?;
    input.read_exact(&mut key)?;
    let mut checksum = crc32fast::Hasher::new();
    checksum.update(&header[..14]);
    checksum.update(&table);
    checksum.update(&key);
    if checksum.finalize() != u32::from_le_bytes(header[14..].try_into().unwrap()) {
        return Err(Error::Corrupt("sorted file key checksum"));
    }
    Ok(RecordKey {
        table: String::from_utf8(table).map_err(|_| Error::Corrupt("sorted file table name"))?,
        key,
        version,
        value_size: (value_size != u32::MAX).then_some(value_size as usize),
    })
}
fn read_value(input: &mut impl Read, size: Option<usize>) -> Result<Option<Vec<u8>>> {
    let mut value = size.map(|size| vec![0; size]);
    if let Some(value) = &mut value {
        input.read_exact(value)?;
    }
    let mut checksum = [0; 4];
    input.read_exact(&mut checksum)?;
    if crc32fast::hash(value.as_deref().unwrap_or_default()) != u32::from_le_bytes(checksum) {
        return Err(Error::Corrupt("sorted file value checksum"));
    }
    Ok(value)
}
impl Run {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let mut input = &file;
        let mut header = [0; HEADER as usize];
        input.read_exact(&mut header)?;
        if &header[..8] != MAGIC
            || crc32fast::hash(&header[..60])
                != u32::from_le_bytes(header[60..].try_into().unwrap())
            || header[44..60] != [0; 16]
        {
            return Err(Error::Corrupt("sorted file header"));
        }
        let records = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let keys = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let data_bytes = u64::from_le_bytes(header[24..32].try_into().unwrap());
        let metadata = Metadata {
            level: u32::from_le_bytes(header[40..44].try_into().unwrap()),
            min_version: u32::from_le_bytes(header[32..36].try_into().unwrap()),
            max_version: u32::from_le_bytes(header[36..40].try_into().unwrap()),
            records,
            data_bytes,
        };
        if records < keys
            || keys == 0
            || data_bytes <= HEADER
            || keys
                .checked_mul(8)
                .and_then(|size| size.checked_add(data_bytes))
                .and_then(|size| size.checked_add(4))
                != Some(file_size)
            || metadata.min_version > metadata.max_version
            || metadata.max_version == u32::MAX
        {
            return Err(Error::Corrupt("sorted file bounds"));
        }
        input.seek(SeekFrom::Start(data_bytes))?;
        let mut input = BufReader::with_capacity(64 * 1024, input);
        let mut offsets = Vec::new();
        let mut checksum = crc32fast::Hasher::new();
        for _ in 0..keys {
            let mut bytes = [0; 8];
            input.read_exact(&mut bytes)?;
            checksum.update(&bytes);
            let offset = u64::from_le_bytes(bytes);
            if offset < HEADER
                || offset >= data_bytes
                || offsets.last().is_some_and(|previous| *previous >= offset)
            {
                return Err(Error::Corrupt("sorted file index offset"));
            }
            offsets.push(offset);
        }
        let mut sum = [0; 4];
        input.read_exact(&mut sum)?;
        if offsets[0] != HEADER || checksum.finalize() != u32::from_le_bytes(sum) {
            return Err(Error::Corrupt("sorted file index checksum"));
        }
        drop(input);
        Ok(Self(Arc::new(FileData {
            file,
            offsets,
            metadata,
        })))
    }
    pub fn metadata(&self) -> Metadata {
        self.0.metadata
    }
    pub fn unique_keys(&self) -> usize {
        self.0.offsets.len()
    }
    fn section(&self, offset: u64) -> Section {
        Section {
            file: self.0.clone(),
            position: offset,
            end: self.0.metadata.data_bytes,
        }
    }
    fn lower_bound(&self, table: &str, key: &[u8]) -> Result<usize> {
        let (mut low, mut high) = (0, self.0.offsets.len());
        while low < high {
            let middle = low + (high - low) / 2;
            let record = read_key(&mut BufReader::with_capacity(
                256,
                self.section(self.0.offsets[middle]),
            ))?;
            if compare_key(&record.table, &record.key, table, key).is_lt() {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    }
    /// Missing key and a committed deletion remain distinguishable to a merger.
    pub fn get(&self, table: &str, key: &[u8], max: u32) -> Result<Option<Entry>> {
        let index = self.lower_bound(table, key)?;
        let Some(offset) = self.0.offsets.get(index) else {
            return Ok(None);
        };
        let end = self
            .0
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.0.metadata.data_bytes);
        let mut section = self.section(*offset);
        section.end = end;
        while section.position < end {
            let record = read_key(&mut section)?;
            if record.table != table || record.key != key {
                return Ok(None);
            }
            if record.version <= max {
                let value = read_value(&mut section, record.value_size)?;
                return Ok(Some(Entry {
                    version: record.version,
                    write: Write {
                        table: record.table,
                        key: record.key,
                        value,
                    },
                }));
            }
            let skip = record.value_size.unwrap_or(0) as u64 + 4;
            if skip > end - section.position {
                return Err(Error::Corrupt("sorted file value bounds"));
            }
            section.position += skip;
        }
        Ok(None)
    }
    pub fn entries(&self) -> Cursor {
        self.cursor(HEADER)
    }
    pub fn entries_from(&self, table: &str, key: &[u8]) -> Result<Cursor> {
        let index = self.lower_bound(table, key)?;
        let offset = self
            .0
            .offsets
            .get(index)
            .copied()
            .unwrap_or(self.0.metadata.data_bytes);
        Ok(self.cursor(offset))
    }
    fn cursor(&self, offset: u64) -> Cursor {
        Cursor {
            input: BufReader::with_capacity(64 * 1024, self.section(offset)),
            position: offset,
            end: self.0.metadata.data_bytes,
        }
    }
}
fn compare_key(a_table: &str, a: &[u8], b_table: &str, b: &[u8]) -> Ordering {
    a_table
        .cmp(b_table)
        .then(a.len().cmp(&b.len()))
        .then(a.cmp(b))
}
pub struct Cursor {
    input: BufReader<Section>,
    position: u64,
    end: u64,
}
impl Iterator for Cursor {
    type Item = Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.position == self.end {
            return None;
        }
        let result = (|| {
            let record = read_key(&mut self.input)?;
            let value = read_value(&mut self.input, record.value_size)?;
            let write = Write {
                table: record.table,
                key: record.key,
                value,
            };
            self.position += record_size(&write);
            Ok(Entry {
                version: record.version,
                write,
            })
        })();
        if result.is_err() {
            self.position = self.end;
        }
        Some(result)
    }
}

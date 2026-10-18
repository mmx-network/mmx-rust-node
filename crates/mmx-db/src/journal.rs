//! Append committed values once; sorted files can store small value references.
//! Frames are streamed, atomically sealed, and synced. Recovery reads only the
//! requested uncheckpointed suffix and never allocates a whole replacement.
use crate::{Error, Result, MAX_FRAME};
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
const MAGIC: &[u8; 8] = b"MMXLOG\x01\n";
const SEAL: &[u8; 8] = b"MMXDONE\x01";
const FRAME_HEADER: u64 = 16;
const FOOTER: u64 = 12;
const ACTION_SIZE: u64 = 32;
const ENTRY_SIZE: u64 = 18;

/// Atomic replacement of [cut, expected) with the supplied versioned entries.
/// A normal commit uses cut=expected. Revert uses next=cut and no entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub generation: u64,
    pub expected: u32,
    pub cut: u32,
    pub next: u32,
    /// Earliest allowed database version, enforced by the owning database.
    pub floor: u32,
}
impl Action {
    fn valid(self) -> bool {
        self.generation != 0
            && self.next != u32::MAX
            && self.expected != u32::MAX
            && self.floor <= self.cut
            && self.cut <= self.expected
            && self.cut <= self.next
    }
    fn follows(self, previous: Self) -> bool {
        previous.generation.checked_add(1) == Some(self.generation)
            && self.expected == previous.next
            && self.floor >= previous.floor
    }
}
#[derive(Clone, Copy)]
pub struct Change<'a> {
    pub version: u32,
    pub table: &'a str,
    pub key: &'a [u8],
    pub value: Option<&'a [u8]>,
}
impl Change<'_> {
    fn valid(self, action: Action) -> bool {
        self.version >= action.cut
            && self.version < action.next
            && valid_parts(self.table, self.key.len(), self.value.map(|v| v.len()))
    }
}
fn valid_parts(table: &str, key: usize, value: Option<usize>) -> bool {
    !table.is_empty()
        && table.len() <= u16::MAX as usize
        && table
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && key <= MAX_FRAME
        && value.is_none_or(|v| v <= MAX_FRAME)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueRef {
    pub file_id: u64,
    pub offset: u64,
    pub size: u32,
    pub checksum: u32,
}
impl ValueRef {
    pub fn encode(self) -> [u8; 24] {
        let mut out = [0; 24];
        out[..8].copy_from_slice(&self.file_id.to_le_bytes());
        out[8..16].copy_from_slice(&self.offset.to_le_bytes());
        out[16..20].copy_from_slice(&self.size.to_le_bytes());
        out[20..].copy_from_slice(&self.checksum.to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 24 {
            return Err(Error::Corrupt("value reference size"));
        }
        let value = Self {
            file_id: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            size: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            checksum: u32::from_le_bytes(bytes[20..].try_into().unwrap()),
        };
        if value.offset < 8 || value.size as usize > MAX_FRAME {
            return Err(Error::Corrupt("value reference bounds"));
        }
        Ok(value)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indexed {
    pub version: u32,
    pub table: String,
    pub key: Vec<u8>,
    pub value: Option<ValueRef>,
}
#[derive(Debug)]
pub struct Recovered {
    pub action: Action,
    pub entries: Vec<Indexed>,
}
struct Shared {
    file: File,
    id: u64,
    end: AtomicU64,
}
#[derive(Clone)]
pub struct Reader(Arc<Shared>);
impl Reader {
    /// Open a sealed segment lazily, without replaying its committed payloads.
    /// A database can bound its descriptor cache independently of retained files.
    pub fn open(path: &Path, id: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        let end = file.metadata()?.len();
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Corrupt("journal segment header"));
        }
        Ok(Self(Arc::new(Shared {
            file,
            id,
            end: AtomicU64::new(end),
        })))
    }
    pub fn value(&self, value: ValueRef) -> Result<Vec<u8>> {
        let end = self.0.end.load(Ordering::Acquire);
        if value.file_id != self.0.id
            || value.offset < 8
            || value.size as usize > MAX_FRAME
            || value
                .offset
                .checked_add(u64::from(value.size))
                .is_none_or(|v| v > end)
        {
            return Err(Error::Corrupt("value reference bounds"));
        }
        let mut section = Section {
            file: self.clone(),
            position: value.offset,
            end,
        };
        let mut bytes = vec![0; value.size as usize];
        section.read_exact(&mut bytes)?;
        if crc32fast::hash(&bytes) != value.checksum {
            return Err(Error::Corrupt("journal value checksum"));
        }
        Ok(bytes)
    }
}
struct Section {
    file: Reader,
    position: u64,
    end: u64,
}
impl Read for Section {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let size = out.len().min((self.end - self.position) as usize);
        #[cfg(unix)]
        let count = {
            use std::os::unix::fs::FileExt;
            self.file.0.file.read_at(&mut out[..size], self.position)?
        };
        #[cfg(windows)]
        let count = {
            use std::os::windows::fs::FileExt;
            self.file
                .0
                .file
                .seek_read(&mut out[..size], self.position)?
        };
        self.position += count as u64;
        Ok(count)
    }
}
pub struct Journal {
    reader: Reader,
    output: BufWriter<File>,
    end: u64,
    ready: bool,
    poisoned: bool,
    previous: Option<Action>,
}
impl Journal {
    /// An existing file requires recover_from() before appending. The owning
    /// database also validates generation/version against its durable manifest.
    pub fn open(path: &Path, id: u64) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock().map_err(std::io::Error::from)?;
        let mut end = file.metadata()?.len();
        let fresh = end == 0;
        if fresh {
            file.write_all(MAGIC)?;
            file.sync_all()?;
            File::open(crate::parent_dir(path))?.sync_all()?;
            end = 8;
        }
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::Corrupt("journal segment header"));
        }
        let mut output = BufWriter::with_capacity(64 * 1024, file.try_clone()?);
        output.seek(SeekFrom::Start(end))?;
        Ok(Self {
            reader: Reader(Arc::new(Shared {
                file,
                id,
                end: AtomicU64::new(end),
            })),
            output,
            end,
            ready: fresh,
            poisoned: false,
            previous: None,
        })
    }
    pub fn reader(&self) -> Reader {
        self.reader.clone()
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn append(&mut self, action: Action, changes: &[Change<'_>]) -> Result<Vec<Indexed>> {
        if !self.ready || self.poisoned {
            return Err(Error::Poisoned);
        }
        if !action.valid()
            || self.previous.is_some_and(|p| !action.follows(p))
            || changes.iter().any(|c| !c.valid(action))
        {
            return Err(Error::InvalidEntry);
        }
        let size = changes
            .iter()
            .try_fold(ACTION_SIZE, |size, c| {
                size.checked_add(
                    ENTRY_SIZE
                        + c.table.len() as u64
                        + c.key.len() as u64
                        + c.value.map_or(0, |v| v.len() as u64),
                )
            })
            .ok_or(Error::InvalidEntry)?;
        let end = self
            .end
            .checked_add(FRAME_HEADER)
            .and_then(|v| v.checked_add(size))
            .and_then(|v| v.checked_add(FOOTER))
            .ok_or(Error::InvalidEntry)?;
        self.ready = false;
        self.poisoned = true;
        let mut indexes = Vec::with_capacity(changes.len());
        self.output.write_all(&size.to_le_bytes())?;
        self.output.write_all(&(!size).to_le_bytes())?;
        let mut hash = crc32fast::Hasher::new();
        let mut write = |bytes: &[u8]| -> std::io::Result<()> {
            self.output.write_all(bytes)?;
            hash.update(bytes);
            Ok(())
        };
        write(&action.generation.to_le_bytes())?;
        write(&action.expected.to_le_bytes())?;
        write(&action.cut.to_le_bytes())?;
        write(&action.next.to_le_bytes())?;
        write(&action.floor.to_le_bytes())?;
        write(&(changes.len() as u64).to_le_bytes())?;
        let mut position = self.end + FRAME_HEADER + ACTION_SIZE;
        for change in changes {
            let checksum = crc32fast::hash(change.value.unwrap_or_default());
            write(&change.version.to_le_bytes())?;
            write(&(change.table.len() as u16).to_le_bytes())?;
            write(&(change.key.len() as u32).to_le_bytes())?;
            write(
                &change
                    .value
                    .map_or(u32::MAX, |v| v.len() as u32)
                    .to_le_bytes(),
            )?;
            write(&checksum.to_le_bytes())?;
            write(change.table.as_bytes())?;
            write(change.key)?;
            position += ENTRY_SIZE + change.table.len() as u64 + change.key.len() as u64;
            let value = change.value.map(|value| ValueRef {
                file_id: self.reader.0.id,
                offset: position,
                size: value.len() as u32,
                checksum,
            });
            if let Some(value) = change.value {
                write(value)?;
                position += value.len() as u64;
            }
            indexes.push(Indexed {
                version: change.version,
                table: change.table.into(),
                key: change.key.into(),
                value,
            });
        }
        self.output.write_all(&hash.finalize().to_le_bytes())?;
        self.output.write_all(SEAL)?;
        self.output.flush()?;
        self.output.get_ref().sync_all()?;
        self.end = end;
        self.previous = Some(action);
        self.reader.0.end.store(end, Ordering::Release);
        self.ready = true;
        self.poisoned = false;
        Ok(indexes)
    }
    pub fn recover_from(&mut self, offset: u64) -> Result<Vec<Recovered>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        self.ready = false;
        if offset < 8 || offset > self.end {
            return Err(Error::Corrupt("journal checkpoint position"));
        }
        self.poisoned = true;
        let mut position = offset;
        let mut recovered = Vec::new();
        let mut previous = None;
        while self.end - position >= FRAME_HEADER {
            let mut section = Section {
                file: self.reader.clone(),
                position,
                end: self.end,
            };
            let size = read_u64(&mut section)?;
            let guard = read_u64(&mut section)?;
            if guard != !size || size < ACTION_SIZE {
                return Err(Error::Corrupt("journal segment length guard"));
            }
            let Some(end) = position
                .checked_add(FRAME_HEADER)
                .and_then(|v| v.checked_add(size))
                .and_then(|v| v.checked_add(FOOTER))
            else {
                return Err(Error::Corrupt("journal segment frame bounds"));
            };
            if end > self.end {
                break;
            }
            section.end = end - FOOTER;
            let mut input = HashInput {
                input: BufReader::with_capacity(64 * 1024, section),
                hash: crc32fast::Hasher::new(),
                position: position + FRAME_HEADER,
            };
            let action = Action {
                generation: read_u64(&mut input)?,
                expected: read_u32(&mut input)?,
                cut: read_u32(&mut input)?,
                next: read_u32(&mut input)?,
                floor: read_u32(&mut input)?,
            };
            if !action.valid() || previous.is_some_and(|p| !action.follows(p)) {
                return Err(Error::Corrupt("journal segment action"));
            }
            let count = read_u64(&mut input)?;
            if count > (size - ACTION_SIZE) / ENTRY_SIZE {
                return Err(Error::Corrupt("journal segment entry count"));
            }
            let mut entries = Vec::new();
            for _ in 0..count {
                let version = read_u32(&mut input)?;
                let mut table_size = [0; 2];
                input.read_exact(&mut table_size)?;
                let table_size = u16::from_le_bytes(table_size) as usize;
                let key_size = read_u32(&mut input)? as usize;
                let value_size = read_u32(&mut input)?;
                let checksum = read_u32(&mut input)?;
                if table_size == 0
                    || key_size > MAX_FRAME
                    || (value_size != u32::MAX && value_size as usize > MAX_FRAME)
                {
                    return Err(Error::Corrupt("journal segment entry size"));
                }
                let mut table = vec![0; table_size];
                let mut key = vec![0; key_size];
                input.read_exact(&mut table)?;
                input.read_exact(&mut key)?;
                let table = String::from_utf8(table)
                    .map_err(|_| Error::Corrupt("journal segment table name"))?;
                let value = (value_size != u32::MAX).then_some(ValueRef {
                    file_id: self.reader.0.id,
                    offset: input.position,
                    size: value_size,
                    checksum,
                });
                if version < action.cut
                    || version >= action.next
                    || !valid_parts(&table, key.len(), value.map(|v| v.size as usize))
                {
                    return Err(Error::Corrupt("journal segment entry version"));
                }
                let mut remaining = value.map_or(0, |v| v.size as usize);
                let mut buffer = [0; 8192];
                let mut value_hash = crc32fast::Hasher::new();
                while remaining > 0 {
                    let size = remaining.min(buffer.len());
                    input.read_exact(&mut buffer[..size])?;
                    value_hash.update(&buffer[..size]);
                    remaining -= size;
                }
                if value_hash.finalize() != checksum {
                    return Err(Error::Corrupt("journal segment value checksum"));
                }
                entries.push(Indexed {
                    version,
                    table,
                    key,
                    value,
                });
            }
            if input.position != end - FOOTER {
                return Err(Error::Corrupt("journal segment trailing payload"));
            }
            let sum = input.hash.finalize();
            let mut footer = Section {
                file: self.reader.clone(),
                position: end - FOOTER,
                end,
            };
            let supplied = read_u32(&mut footer)?;
            let mut seal = [0; 8];
            footer.read_exact(&mut seal)?;
            if supplied != sum || &seal != SEAL {
                return Err(Error::Corrupt("journal segment frame checksum/seal"));
            }
            recovered.push(Recovered { action, entries });
            previous = Some(action);
            position = end;
        }
        if position != self.end {
            self.reader.0.file.set_len(position)?;
            self.reader.0.file.sync_all()?;
        }
        self.output.seek(SeekFrom::Start(position))?;
        self.end = position;
        self.reader.0.end.store(position, Ordering::Release);
        self.previous = previous;
        self.ready = true;
        self.poisoned = false;
        Ok(recovered)
    }
}
struct HashInput<R> {
    input: R,
    hash: crc32fast::Hasher,
    position: u64,
}
impl<R: Read> Read for HashInput<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(out)?;
        self.hash.update(&out[..count]);
        self.position += count as u64;
        Ok(count)
    }
}
fn read_u32(input: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}
fn read_u64(input: &mut impl Read) -> std::io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

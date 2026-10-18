use crate::{Error, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
const MAGIC: &[u8; 8] = b"MMXNDB\x02\n";
const PREVIOUS: &[u8; 8] = b"MMXNDB\x01\n";
#[derive(Clone)]
pub(super) struct Manifest {
    pub version: u32,
    pub generation: u64,
    pub floor: u32,
    pub collected_floor: u32,
    pub collection_tag: u64,
    pub next_id: u64,
    pub active: u64,
    pub cursor: u64,
    pub files: Vec<u64>,
    pub runs: Vec<(u64, u32)>,
}
impl Manifest {
    pub fn read(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        let file = File::open(path)?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(Error::Corrupt("native manifest size"));
        }
        file.take(64 * 1024 * 1024).read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 12 || (&bytes[..8] != MAGIC && &bytes[..8] != PREVIOUS) {
            return Err(Error::Corrupt("native manifest header"));
        }
        let end = bytes.len() - 4;
        if crc32fast::hash(&bytes[..end]) != u32::from_le_bytes(bytes[end..].try_into().unwrap()) {
            return Err(Error::Corrupt("native manifest checksum"));
        }
        let mut input = Input(&bytes[8..end]);
        let mut out = Self {
            version: input.u32()?,
            generation: input.u64()?,
            floor: input.u32()?,
            collected_floor: if &bytes[..8] == MAGIC {
                input.u32()?
            } else {
                0
            },
            collection_tag: if &bytes[..8] == MAGIC {
                input.u64()?
            } else {
                0
            },
            next_id: input.u64()?,
            active: input.u64()?,
            cursor: input.u64()?,
            files: Vec::new(),
            runs: Vec::new(),
        };
        let count = input.u32()?;
        if count as usize > input.0.len() / 8 {
            return Err(Error::Corrupt("native manifest files"));
        }
        for _ in 0..count {
            out.files.push(input.u64()?);
        }
        let count = input.u32()?;
        if count as usize > input.0.len() / 12 {
            return Err(Error::Corrupt("native manifest runs"));
        }
        for _ in 0..count {
            out.runs.push((input.u64()?, input.u32()?));
        }
        if !input.0.is_empty()
            || out.version == u32::MAX
            || out.floor > out.version
            || out.collected_floor > out.floor
            || out.cursor < 8
            || out.files.binary_search(&out.active).is_err()
            || out.files.windows(2).any(|pair| pair[0] >= pair[1])
            || out
                .files
                .iter()
                .chain(out.runs.iter().map(|r| &r.0))
                .any(|id| *id >= out.next_id)
            || out
                .files
                .iter()
                .chain(out.runs.iter().map(|r| &r.0))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != out.files.len() + out.runs.len()
        {
            return Err(Error::Corrupt("native manifest bounds"));
        }
        Ok(out)
    }
    pub fn save(&self, path: &Path, initial: bool) -> Result<()> {
        save_bytes(path, &self.encode()?, initial)
    }
    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(self.version.to_le_bytes());
        bytes.extend(self.generation.to_le_bytes());
        bytes.extend(self.floor.to_le_bytes());
        bytes.extend(self.collected_floor.to_le_bytes());
        bytes.extend(self.collection_tag.to_le_bytes());
        bytes.extend(self.next_id.to_le_bytes());
        bytes.extend(self.active.to_le_bytes());
        bytes.extend(self.cursor.to_le_bytes());
        bytes.extend(
            u32::try_from(self.files.len())
                .map_err(|_| Error::InvalidEntry)?
                .to_le_bytes(),
        );
        for id in &self.files {
            bytes.extend(id.to_le_bytes());
        }
        bytes.extend(
            u32::try_from(self.runs.len())
                .map_err(|_| Error::InvalidEntry)?
                .to_le_bytes(),
        );
        for (id, cutoff) in &self.runs {
            bytes.extend(id.to_le_bytes());
            bytes.extend(cutoff.to_le_bytes());
        }
        bytes.extend(crc32fast::hash(&bytes).to_le_bytes());
        Ok(bytes)
    }
}
pub(super) fn save_bytes(path: &Path, bytes: &[u8], initial: bool) -> Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix("native-manifest-")
        .suffix(".tmp")
        .tempfile_in(crate::parent_dir(path))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if initial {
        file.persist_noclobber(path).map_err(|e| e.error)?;
    } else {
        file.persist(path).map_err(|e| e.error)?;
    }
    File::open(crate::parent_dir(path))?.sync_all()?;
    Ok(())
}
struct Input<'a>(&'a [u8]);
impl Input<'_> {
    fn take(&mut self, size: usize) -> Result<&[u8]> {
        if self.0.len() < size {
            return Err(Error::Corrupt("native manifest truncated"));
        }
        let (head, tail) = self.0.split_at(size);
        self.0 = tail;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

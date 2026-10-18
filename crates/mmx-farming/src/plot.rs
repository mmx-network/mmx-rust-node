//! Native MMX HDD/SSD plots. Handles are scoped to a lookup; parks are read
//! positionally and reused within its proof backtrace, never copied to a DB.
use crate::{
    encoding::{decode, line_point, read_bits},
    error, Result,
};
use mmx_consensus::{pos, proof, Record};
use mmx_types::mmx;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use vnx::binary::Reader;

const MAX_PARK: usize = 4 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Quality {
    pub index: u64,
    pub meta: [u8; 48],
    pub proof: Vec<u32>,
}
#[derive(Clone, Debug)]
pub struct Prover {
    path: PathBuf,
    pub header: mmx::PlotHeader,
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}
impl Prover {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        let length = metadata.len();
        let modified = metadata.modified().ok();
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        let registry = mmx_types::vnx_registry()?;
        let max_schemas = registry.len() + 128;
        let mut reader =
            Reader::new(file.take(1024 * 1024), registry).with_limits(vnx::binary::Limits {
                max_depth: 32,
                max_container: 4096,
                max_string_bytes: 65536,
                max_value_bytes: 1024 * 1024,
                max_schemas,
            });
        let value = reader.read()?;
        if value["__type"] != "mmx.PlotHeader" {
            return Err(error("invalid plot header type"));
        }
        let header: mmx::PlotHeader = vnx::serde_json::from_value(value)?;
        let out = Self {
            path,
            header,
            length,
            modified,
            #[cfg(unix)]
            inode,
        };
        out.validate()?;
        Ok(out)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// A reload reuses unchanged immutable plot headers, like the C++ plot map.
    pub fn unchanged(&self) -> bool {
        let Ok(m) = std::fs::metadata(&self.path) else {
            return false;
        };
        if m.len() != self.length || self.modified.is_none() || m.modified().ok() != self.modified {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (m.dev(), m.ino()) != self.inode {
                return false;
            }
        }
        true
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    fn validate(&self) -> Result<()> {
        let h = &self.header;
        if h.version != 0
            || !(8..=32).contains(&h.ksize)
            || h.xbits <= 0
            || h.xbits > h.ksize
            || h.ksize - h.xbits > 30
            || h.num_entries_y == 0
            || h.table_offset_pd.len() != 7
            || !(1..=64).contains(&h.entry_bits_x)
        {
            return Err(error("invalid or unsupported plot header"));
        }
        for (size, bytes, bits) in [
            (h.park_size_x, h.park_bytes_x, h.entry_bits_x),
            (h.park_size_y, h.park_bytes_y, h.ksize),
            (h.park_size_pd, h.park_bytes_pd, h.ksize),
            (
                if h.has_meta { h.park_size_meta } else { 1 },
                if h.has_meta {
                    h.park_bytes_meta
                } else {
                    MAX_PARK as i32
                },
                if h.has_meta { 12 * h.ksize } else { 1 },
            ),
        ] {
            if size <= 0
                || size > 131072
                || bytes <= 0
                || bytes as usize > MAX_PARK
                || (bits as u64 > bytes as u64 * 8)
            {
                return Err(error("invalid plot park dimensions"));
            }
        }
        if h.park_bytes_y < 4
            || h.park_size_x as u64 * h.entry_bits_x as u64 > h.park_bytes_x as u64 * 8
            || h.park_size_pd as u64 * h.ksize as u64 > h.park_bytes_pd as u64 * 8
            || (h.has_meta
                && h.park_size_meta as u64 * 12 * h.ksize as u64 > h.park_bytes_meta as u64 * 8)
        {
            return Err(error("plot park capacity mismatch"));
        }
        if h.plot_size != self.length {
            return Err(error("plot size does not match header"));
        }
        for (offset, bytes) in std::iter::once((h.table_offset_x, h.park_bytes_x))
            .chain(std::iter::once((h.table_offset_y, h.park_bytes_y)))
            .chain(h.table_offset_pd.iter().map(|o| (*o, h.park_bytes_pd)))
            .chain(
                h.has_meta
                    .then_some((h.table_offset_meta, h.park_bytes_meta)),
            )
        {
            self.range(offset, bytes as usize)?;
        }
        let mut p = Record::from_generated(&mmx::ProofOfSpaceOG {
            ksize: h.ksize as u8,
            seed: h.seed.clone(),
            farmer_key: h.farmer_key.clone(),
            ..Default::default()
        })?;
        if let Some(contract) = &h.contract {
            p.name = "mmx.ProofOfSpaceNFT".into();
            p.fields.insert(
                "contract".into(),
                mmx_consensus::Value::Bytes(contract.0.to_vec()),
            );
        }
        if proof::calc_plot_id(&p)? != h.plot_id.0 {
            return Err(error("invalid plot id"));
        }
        Ok(())
    }
    fn range(&self, offset: u64, count: usize) -> Result<()> {
        if count > MAX_PARK
            || offset
                .checked_add(count as u64)
                .is_none_or(|end| end > self.length)
        {
            return Err(error("plot read out of bounds"));
        }
        Ok(())
    }
    fn read(&self, file: &File, offset: u64, count: usize) -> Result<Vec<u8>> {
        self.range(offset, count)?;
        let mut bytes = vec![0; count];
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.read_exact_at(&mut bytes, offset)?;
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom};
            let mut handle = file.try_clone()?;
            handle.seek(SeekFrom::Start(offset))?;
            handle.read_exact(&mut bytes)?;
        }
        Ok(bytes)
    }
    fn offset(base: u64, park: u64, bytes: i32) -> Result<u64> {
        base.checked_add(
            park.checked_mul(bytes as u64)
                .ok_or_else(|| error("park offset overflow"))?,
        )
        .ok_or_else(|| error("park offset overflow"))
    }
    pub fn qualities(
        &self,
        challenge: &[u8; 32],
        filter: u32,
        max_f1: usize,
    ) -> Result<Vec<Quality>> {
        let h = &self.header;
        if filter > 30 {
            return Err(error("invalid plot filter"));
        }
        let file = File::open(&self.path)?;
        let mask = (1u64 << h.ksize) - 1;
        let begin = u64::from(u32::from_le_bytes(challenge[..4].try_into()?)) & mask;
        let end = begin + (1u64 << filter);
        let parks = h.num_entries_y.div_ceil(h.park_size_y as u64);
        // lower_bound(first Y >= begin), then include the preceding park. This
        // preserves matches that straddle parks, including repeated Y values.
        let (mut lo, mut hi) = (0, parks);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let y = read_bits(
                &self.read(
                    &file,
                    Self::offset(h.table_offset_y, mid, h.park_bytes_y)?,
                    4,
                )?,
                0,
                h.ksize as usize,
            )?;
            if y < begin {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut result = Vec::new();
        let mut meta_park: Option<(u64, Vec<u8>)> = None;
        for park in lo.saturating_sub(1)..parks {
            let data = self.read(
                &file,
                Self::offset(h.table_offset_y, park, h.park_bytes_y)?,
                h.park_bytes_y as usize,
            )?;
            let mut y = read_bits(&data[..4], 0, h.ksize as usize)?;
            if y >= end {
                break;
            }
            let count =
                (h.num_entries_y - park * h.park_size_y as u64).min(h.park_size_y as u64) as usize;
            let deltas = decode(&data[4..], count.saturating_sub(1), 0)?;
            for i in 0..count {
                if i > 0 {
                    y = y
                        .checked_add(u64::from(deltas[i - 1]))
                        .ok_or_else(|| error("Y overflow"))?;
                }
                if y > mask {
                    return Err(error("Y outside plot ksize"));
                }
                if y >= end {
                    return Ok(result);
                }
                if y < begin {
                    continue;
                }
                if result.len() >= 4096 {
                    return Err(error("too many plot qualities"));
                }
                let index = park * h.park_size_y as u64 + i as u64;
                let quality = if h.has_meta {
                    let park = index / h.park_size_meta as u64;
                    if meta_park.as_ref().is_none_or(|(id, _)| *id != park) {
                        meta_park = Some((
                            park,
                            self.read(
                                &file,
                                Self::offset(h.table_offset_meta, park, h.park_bytes_meta)?,
                                h.park_bytes_meta as usize,
                            )?,
                        ));
                    }
                    let data = &meta_park.as_ref().unwrap().1;
                    let mut meta = [0; 48];
                    for (j, word) in meta.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let value = read_bits(
                            data,
                            ((index % h.park_size_meta as u64) as usize * 12 + j)
                                * h.ksize as usize,
                            h.ksize as usize,
                        )? as u32;
                        word.copy_from_slice(&value.to_le_bytes());
                    }
                    Quality {
                        index,
                        meta,
                        proof: Vec::new(),
                    }
                } else {
                    self.full_proof_with(&file, index, max_f1)?
                };
                result.push(quality);
            }
        }
        Ok(result)
    }
    pub fn full_proof(&self, index: u64, max_f1: usize) -> Result<Quality> {
        self.full_proof_with(&File::open(&self.path)?, index, max_f1)
    }
    fn full_proof_with(&self, file: &File, index: u64, max_f1: usize) -> Result<Quality> {
        let h = &self.header;
        if index >= h.num_entries_y {
            return Err(error("proof index out of range"));
        }
        let mut pointers = vec![index];
        for base in &h.table_offset_pd {
            let mut cache = BTreeMap::new();
            let mut next = Vec::with_capacity(pointers.len() * 2);
            for pointer in pointers {
                let park = pointer / h.park_size_pd as u64;
                let within = (pointer % h.park_size_pd as u64) as usize;
                if let std::collections::btree_map::Entry::Vacant(e) = cache.entry(park) {
                    e.insert(self.read(
                        file,
                        Self::offset(*base, park, h.park_bytes_pd)?,
                        h.park_bytes_pd as usize,
                    )?);
                }
                let data = &cache[&park];
                let position = read_bits(data, within * h.ksize as usize, h.ksize as usize)?;
                let offsets = decode(data, within + 1, h.park_size_pd as usize * h.ksize as usize)?;
                next.push(position);
                next.push(
                    position
                        .checked_add(u64::from(*offsets.last().unwrap()))
                        .ok_or_else(|| error("proof pointer overflow"))?,
                );
            }
            pointers = next;
        }
        let mut prefixes = BTreeSet::new();
        let mut cache = BTreeMap::new();
        for pointer in pointers {
            let park = pointer / h.park_size_x as u64;
            let within = (pointer % h.park_size_x as u64) as usize;
            if let std::collections::btree_map::Entry::Vacant(e) = cache.entry(park) {
                e.insert(self.read(
                    file,
                    Self::offset(h.table_offset_x, park, h.park_bytes_x)?,
                    h.park_bytes_x as usize,
                )?);
            }
            let value = read_bits(
                &cache[&park],
                within * h.entry_bits_x as usize,
                h.entry_bits_x as usize,
            )?;
            let (a, b) = line_point(value, h.xbits < h.ksize)?;
            if u64::from(a) >= (1u64 << h.xbits) || u64::from(b) >= (1u64 << h.xbits) {
                return Err(error("X outside plot range"));
            }
            prefixes.extend([a, b]);
        }
        let bits = (h.ksize - h.xbits) as u32;
        let count = prefixes
            .len()
            .checked_shl(bits)
            .ok_or_else(|| error("proof work overflow"))?;
        if count > max_f1 {
            return Err(error("compressed proof exceeds configured CPU work limit"));
        }
        let xs: Vec<u32> = prefixes
            .into_iter()
            .flat_map(|x| (0..(1u32 << bits)).map(move |i| (x << bits) | i))
            .collect();
        let mut entries = pos::compute(&xs, &h.plot_id.0, h.ksize as u8)?;
        if entries.len() != 1 {
            return Err(error("expected exactly one proof"));
        }
        let entry = entries.remove(0);
        Ok(Quality {
            index,
            meta: entry.meta,
            proof: entry.proof,
        })
    }
}

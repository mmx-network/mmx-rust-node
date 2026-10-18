//! Resumable, bounded-memory import from the legacy SQLite history table.
//! Payloads follow physical row order; only their small references are sorted.
use super::*;
use rusqlite::{params_from_iter, types::Value, Connection, OpenFlags};
use std::io::Read;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"MMXIMP\x01\n";
const MARKER: &str = "native.import";

#[derive(Clone, Copy)]
pub struct Options {
    pub batch_bytes: usize,
    pub segment_bytes: u64,
    /// Approximate aggregate payload/key transfer rate, counting reads+writes.
    /// `None` disables pacing; the default is 4 MiB/s.
    pub bytes_per_second: Option<u64>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            batch_bytes: 1024 * 1024,
            segment_bytes: 64 * 1024 * 1024,
            bytes_per_second: Some(4 * 1024 * 1024),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub version: u32,
    pub rows: u64,
    pub value_bytes: u64,
    pub transfer_bytes: u64,
    pub resumed_rows: u64,
    pub finished: bool,
}
#[derive(Clone)]
enum Cursor {
    Start,
    Row(i64),
    Key(String, Vec<u8>, u32),
}
#[derive(Clone)]
struct State {
    source: Vec<u8>,
    source_generation: u64,
    without_rowid: bool,
    cursor: Cursor,
    report: Report,
    native: Manifest,
}
impl State {
    fn save(&self, path: &Path, initial: bool) -> Result<()> {
        let mut out = MAGIC.to_vec();
        blob(&mut out, &self.source)?;
        out.extend(self.source_generation.to_le_bytes());
        out.push(u8::from(self.without_rowid));
        match &self.cursor {
            Cursor::Start => out.push(0),
            Cursor::Row(row) => {
                out.push(1);
                out.extend(row.to_le_bytes());
            }
            Cursor::Key(table, key, version) => {
                out.push(2);
                blob(&mut out, table.as_bytes())?;
                blob(&mut out, key)?;
                out.extend(version.to_le_bytes());
            }
        }
        out.extend(self.report.rows.to_le_bytes());
        out.extend(self.report.value_bytes.to_le_bytes());
        out.extend(self.report.transfer_bytes.to_le_bytes());
        blob(&mut out, &self.native.encode()?)?;
        out.extend(crc32fast::hash(&out).to_le_bytes());
        manifest::save_bytes(path, &out, initial)
    }
    fn read(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(Error::Corrupt("import manifest size"));
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024).read_to_end(&mut bytes)?;
        if bytes.len() < 12 || &bytes[..8] != MAGIC {
            return Err(Error::Corrupt("import manifest header"));
        }
        let end = bytes.len() - 4;
        if crc32fast::hash(&bytes[..end]) != u32::from_le_bytes(bytes[end..].try_into().unwrap()) {
            return Err(Error::Corrupt("import manifest checksum"));
        }
        let mut input = Input(&bytes[8..end]);
        let source = input.blob()?.to_vec();
        let source_generation = input.u64()?;
        let without_rowid = match input.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(Error::Corrupt("import source layout")),
        };
        let cursor = match input.take(1)?[0] {
            0 => Cursor::Start,
            1 => Cursor::Row(input.u64()? as i64),
            2 => Cursor::Key(
                String::from_utf8(input.blob()?.to_vec())
                    .map_err(|_| Error::Corrupt("import table name"))?,
                input.blob()?.to_vec(),
                input.u32()?,
            ),
            _ => return Err(Error::Corrupt("import cursor")),
        };
        let mut report = Report {
            rows: input.u64()?,
            value_bytes: input.u64()?,
            transfer_bytes: input.u64()?,
            ..Default::default()
        };
        let native = Manifest::decode(input.blob()?)?;
        report.version = native.version;
        if !input.0.is_empty()
            || native.floor != 0
            || native.collected_floor != 0
            || native.collection_tag != 0
            || matches!(cursor, Cursor::Row(_)) && without_rowid
            || matches!(cursor, Cursor::Key(..)) && !without_rowid
            || matches!(cursor, Cursor::Start) != (report.rows == 0)
        {
            return Err(Error::Corrupt("import manifest bounds"));
        }
        Ok(Self {
            source,
            source_generation,
            without_rowid,
            cursor,
            report,
            native,
        })
    }
}

/// Import into a new directory, or resume its `native.import` checkpoint.
/// The source writer lock freezes its logical version. Interrupted targets are
/// refused by Native::open; a complete native manifest is published only at EOF.
/// The callback can cancel with an error, including while transfer pacing waits.
pub fn indexed(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    options: Options,
    mut progress: impl FnMut(Report) -> Result<()>,
) -> Result<Report> {
    if options.batch_bytes == 0
        || options.batch_bytes > 64 * 1024 * 1024
        || options.segment_bytes < 8
        || options.bytes_per_second.is_some_and(|rate| rate < 65536)
    {
        return Err(Error::InvalidEntry);
    }
    let source = fs::canonicalize(source)?;
    let source_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(source.join("writer.lock"))?;
    source_lock.try_lock().map_err(std::io::Error::from)?;
    let conn = Connection::open_with_flags(
        source.join("state.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-4096; PRAGMA temp_store=MEMORY; PRAGMA mmap_size=0; BEGIN;")?;
    let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let format: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != crate::indexed::APPLICATION_ID || format != 1 {
        return Err(Error::Corrupt("foreign indexed import source"));
    }
    let (version, generation) = crate::indexed::meta(&conn)?;
    let without_rowid: bool = conn.query_row(
        "SELECT wr FROM pragma_table_list WHERE schema='main' AND name='history'",
        [],
        |r| r.get(0),
    )?;
    let destination = std::path::absolute(destination)?;
    if destination.exists() && fs::canonicalize(&destination)? == source {
        return Err(Error::InvalidEntry);
    }
    if !destination.exists() {
        fs::create_dir(&destination)?;
        File::open(crate::parent_dir(&destination))?.sync_all()?;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(destination.join("writer.lock"))?;
    lock.try_lock().map_err(std::io::Error::from)?;
    let marker = destination.join(MARKER);
    let mut state = if marker.exists() {
        State::read(&marker)?
    } else {
        if fs::read_dir(&destination)?.any(|e| e.is_ok_and(|e| e.file_name() != "writer.lock")) {
            return Err(Error::Corrupt("nonempty import destination"));
        }
        let state = State {
            source: source.as_os_str().as_encoded_bytes().to_vec(),
            source_generation: generation,
            without_rowid,
            cursor: Cursor::Start,
            report: Report {
                version,
                ..Default::default()
            },
            native: Manifest {
                version,
                generation: 0,
                floor: 0,
                collected_floor: 0,
                collection_tag: 0,
                next_id: 1,
                active: 0,
                cursor: 8,
                files: vec![0],
                runs: vec![],
            },
        };
        state.save(&marker, true)?;
        state
    };
    if state.source != source.as_os_str().as_encoded_bytes()
        || state.source_generation != generation
        || state.native.version != version
        || state.without_rowid != without_rowid
    {
        return Err(Error::Corrupt("import source changed since checkpoint"));
    }
    state.report.resumed_rows = state.report.rows;
    let published = destination.join("native.manifest");
    if published.exists() {
        if Manifest::read(&published)?.encode()? != state.native.encode()? {
            return Err(Error::Corrupt("published import manifest mismatch"));
        }
        finish(&destination, &marker)?;
        state.report.finished = true;
        progress(state.report)?;
        return Ok(state.report);
    }
    clean(&destination, &state.native)?;
    let active = data_path(&destination, state.native.active);
    if !active.exists() && state.report.rows == 0 {
        drop(Journal::open(&active, state.native.active)?);
    }
    let file = OpenOptions::new().write(true).open(&active)?;
    if file.metadata()?.len() < state.native.cursor {
        return Err(Error::Corrupt("import value file truncated"));
    }
    file.set_len(state.native.cursor)?;
    drop(file);
    let mut runs = state
        .native
        .runs
        .iter()
        .map(|(id, cutoff)| {
            let path = run_path(&destination, *id);
            Ok(RunView {
                file: Arc::new(RunFile {
                    id: *id,
                    run: Run::open(&path)?,
                    path,
                    retired: AtomicBool::new(false),
                }),
                cutoff: *cutoff,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let started = Instant::now();
    let initial_bytes = state.report.transfer_bytes;
    progress(state.report)?;
    loop {
        let (entries, cursor, value_bytes, transfer) = batch(&conn, &state, options.batch_bytes)?;
        if entries.is_empty() {
            break;
        }
        let mut next = state.clone();
        next.cursor = cursor;
        next.report.rows = next
            .report
            .rows
            .checked_add(entries.len() as u64)
            .ok_or(Error::InvalidEntry)?;
        next.report.value_bytes = next
            .report
            .value_bytes
            .checked_add(value_bytes)
            .ok_or(Error::InvalidEntry)?;
        next.report.transfer_bytes = next
            .report
            .transfer_bytes
            .checked_add(transfer)
            .ok_or(Error::InvalidEntry)?;
        if next.native.cursor >= options.segment_bytes {
            let id = take_id(&mut next.native.next_id)?;
            next.native.active = id;
            next.native.cursor = 8;
            next.native.files.push(id);
        }
        let mut journal = Journal::open(
            &data_path(&destination, next.native.active),
            next.native.active,
        )?;
        journal.recover_from(next.native.cursor)?;
        let changes = entries
            .iter()
            .map(|e| Change {
                version: e.version,
                table: &e.write.table,
                key: &e.write.key,
                value: e.write.value.as_deref(),
            })
            .collect::<Vec<_>>();
        let generation = next
            .native
            .generation
            .checked_add(1)
            .ok_or(Error::InvalidEntry)?;
        let mut indexes = journal.append(
            Action {
                generation,
                expected: if next.native.generation == 0 {
                    0
                } else {
                    version
                },
                cut: 0,
                next: version,
                floor: 0,
            },
            &changes,
        )?;
        next.native.generation = generation;
        next.native.cursor = journal.end();
        drop(journal);
        indexes.sort_by(|a, b| {
            a.table
                .cmp(&b.table)
                .then_with(|| a.key.len().cmp(&b.key.len()))
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| b.version.cmp(&a.version))
        });
        let id = take_id(&mut next.native.next_id)?;
        let mut builder = Builder::new(&destination, 0)?;
        for e in indexes {
            builder.push(&Entry {
                version: e.version,
                write: Write {
                    table: e.table,
                    key: e.key,
                    value: e.value.map(|r| r.encode().to_vec()),
                },
            })?;
        }
        let path = run_path(&destination, id);
        let run = builder.finish(&path)?;
        let mut next_runs = runs.clone();
        next_runs.push(RunView {
            file: Arc::new(RunFile {
                id,
                path,
                run,
                retired: AtomicBool::new(false),
            }),
            cutoff: u32::MAX,
        });
        let mut removed = Vec::new();
        while let Some((start, count, level)) = find_group(&next_runs, 4) {
            let id = take_id(&mut next.native.next_id)?;
            let path = run_path(&destination, id);
            let views = next_runs[start..start + count]
                .iter()
                .map(|v| View {
                    run: &v.file.run,
                    cutoff: v.cutoff,
                })
                .collect::<Vec<_>>();
            let run = sorted::merge(
                &path,
                level.checked_add(1).ok_or(Error::InvalidEntry)?,
                &views,
                None,
            )?
            .ok_or(Error::Corrupt("empty import merge"))?;
            removed.extend(next_runs.drain(start..start + count).map(|v| v.file));
            next_runs.insert(
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
        next.native.runs = next_runs.iter().map(|v| (v.file.id, v.cutoff)).collect();
        next.save(&marker, false)?;
        state = next;
        runs = next_runs;
        for run in removed {
            run.retired.store(true, Ordering::Relaxed);
        }
        progress(state.report)?;
        if let Some(rate) = options.bytes_per_second {
            let target = Duration::from_secs_f64(
                (state.report.transfer_bytes - initial_bytes) as f64 / rate as f64,
            );
            while started.elapsed() < target {
                std::thread::sleep(
                    target
                        .saturating_sub(started.elapsed())
                        .min(Duration::from_millis(100)),
                );
                progress(state.report)?;
            }
        }
    }
    state.native.save(&published, true)?;
    finish(&destination, &marker)?;
    state.report.finished = true;
    progress(state.report)?;
    Ok(state.report)
}

fn batch(conn: &Connection, state: &State, limit: usize) -> Result<(Vec<Entry>, Cursor, u64, u64)> {
    let mut args = Vec::new();
    let condition = match &state.cursor {
        Cursor::Start => "",
        Cursor::Row(row) => {
            args.push(Value::Integer(*row));
            " WHERE rowid>?1"
        }
        Cursor::Key(table, key, version) => {
            args.extend([
                Value::Text(table.clone()),
                Value::Integer(key.len() as i64),
                Value::Blob(key.clone()),
                Value::Integer(i64::from(*version)),
            ]);
            " WHERE (t,n,k,v)>(?1,?2,?3,?4)"
        }
    };
    let (id, order) = if state.without_rowid {
        ("0", "t,n,k,v")
    } else {
        ("rowid", "rowid")
    };
    let mut query = conn.prepare(&format!(
        "SELECT {id},t,n,k,v,value FROM history{condition} ORDER BY {order} LIMIT 65536"
    ))?;
    let mut rows = query.query(params_from_iter(args))?;
    let mut entries = Vec::new();
    let mut cursor = state.cursor.clone();
    let mut values = 0u64;
    let mut bytes = 0u64;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let table: String = row.get(1)?;
        let len: i64 = row.get(2)?;
        let key: Vec<u8> = row.get(3)?;
        let version: u32 = row.get(4)?;
        let value: Option<Vec<u8>> = row.get(5)?;
        if len != key.len() as i64 || version >= state.native.version {
            return Err(Error::Corrupt("indexed import row bounds"));
        }
        cursor = if state.without_rowid {
            Cursor::Key(table.clone(), key.clone(), version)
        } else {
            Cursor::Row(id)
        };
        let write = Write { table, key, value };
        crate::validate(&write)?;
        let payload = write.value.as_ref().map_or(0, Vec::len) as u64;
        values += payload;
        bytes += payload + write.table.len() as u64 + write.key.len() as u64 + 32;
        entries.push(Entry { version, write });
        if bytes >= limit as u64 {
            break;
        }
    }
    Ok((entries, cursor, values, bytes * 2))
}
fn finish(path: &Path, marker: &Path) -> Result<()> {
    fs::remove_file(marker)?;
    File::open(path)?.sync_all()?;
    Ok(())
}
fn clean(path: &Path, native: &Manifest) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let index = name
            .strip_prefix("native-index-")
            .and_then(|s| s.strip_suffix(".sst"));
        let data = name
            .strip_prefix("native-data-")
            .and_then(|s| s.strip_suffix(".log"));
        let orphan = index
            .or(data)
            .filter(|s| s.len() == 16)
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .is_some_and(|id| {
                if index.is_some() {
                    !native.runs.iter().any(|r| r.0 == id)
                } else {
                    native.files.binary_search(&id).is_err()
                }
            });
        if (orphan || manifest_temporary(name)) && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    out.extend(
        u32::try_from(bytes.len())
            .map_err(|_| Error::InvalidEntry)?
            .to_le_bytes(),
    );
    out.extend(bytes);
    Ok(())
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.0.len() < len {
            return Err(Error::Corrupt("import manifest truncated"));
        }
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn blob(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

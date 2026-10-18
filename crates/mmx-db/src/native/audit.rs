//! Offline, paced comparison of imported tables and their rollback views.
use super::Native;
use crate::{Error, Result, Row};
use rusqlite::{params, Connection, OpenFlags};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub struct Options {
    /// Maximum source keys in one comparison range, including tombstones.
    pub keys_per_batch: usize,
    /// Aggregate logical key/value bytes read from both databases.
    pub bytes_per_second: Option<u64>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            keys_per_batch: 16,
            bytes_per_second: Some(2 * 1024 * 1024),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Exclusive database version: block height + 1 for the selected ledger.
    pub version: u32,
    pub keys: u64,
    pub visible_rows: u64,
    pub transfer_bytes: u64,
    pub finished: bool,
}

/// Holds the source writer lock and a read-only SQLite transaction. The native
/// writer is locked too; historical checks revert RAM snapshots, never disk.
pub struct Verifier {
    source: Connection,
    native: Native,
    _source_lock: File,
}
impl Verifier {
    pub fn open(source: impl AsRef<Path>, destination: impl AsRef<Path>) -> Result<Self> {
        let source = fs::canonicalize(source)?;
        let destination = fs::canonicalize(destination)?;
        if source == destination || !destination.join("native.manifest").is_file() {
            return Err(Error::InvalidEntry);
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(source.join("writer.lock"))?;
        lock.try_lock().map_err(std::io::Error::from)?;
        let conn = Connection::open_with_flags(
            source.join("state.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-4096; PRAGMA temp_store=MEMORY; PRAGMA mmap_size=0; BEGIN;")?;
        let app: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        let format: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if app != crate::indexed::APPLICATION_ID || format != 1 {
            return Err(Error::Corrupt("foreign indexed audit source"));
        }
        let (version, _) = crate::indexed::meta(&conn)?;
        let native = Native::open(destination)?;
        if native.version() != version || native.retention_floor() != 0 {
            return Err(Error::Corrupt("import audit version or retention floor"));
        }
        Ok(Self {
            source: conn,
            native,
            _source_lock: lock,
        })
    }
    pub fn version(&self) -> u32 {
        self.native.version()
    }

    /// Compare every visible row in a named table, including its write version.
    /// Source tombstones set range boundaries but must have no native row.
    /// Whole ranges also detect extra destination keys, including before/after
    /// all source keys. Tables not requested by the caller are not audited.
    pub fn table(
        &self,
        table: &str,
        version: u32,
        options: Options,
        mut progress: impl FnMut(Report) -> Result<()>,
    ) -> Result<Report> {
        if table.is_empty()
            || options.keys_per_batch == 0
            || options.keys_per_batch > 1024
            || options.bytes_per_second.is_some_and(|rate| rate < 65536)
        {
            return Err(Error::InvalidEntry);
        }
        let mut snapshot = self.native.fork(self.version())?;
        snapshot.revert(version)?;
        // The covering primary-key index supplies bounded key groups. Fetch
        // payloads only after discarding the next range's lookahead key.
        let mut query = self.source.prepare_cached(
            "SELECT k,MAX(v) FROM history INDEXED BY sqlite_autoindex_history_1
              WHERE t=?1 AND v<?2 AND (n,k)>=(?3,?4)
              GROUP BY n,k ORDER BY n,k LIMIT ?5",
        )?;
        let mut value_query = self
            .source
            .prepare_cached("SELECT value FROM history WHERE t=?1 AND n=?2 AND k=?3 AND v=?4")?;
        let mut report = Report {
            version,
            ..Default::default()
        };
        let mut start = Vec::new();
        loop {
            // Pacing restarts for each bounded range: a pause earns no credit
            // that could cause a subsequent read burst.
            let started = Instant::now();
            let before = report.transfer_bytes;
            let mut rows = query
                .query_map(
                    params![
                        table,
                        version,
                        start.len(),
                        start,
                        options.keys_per_batch + 1
                    ],
                    |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, u32>(1)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let end = if rows.len() > options.keys_per_batch {
                Some(rows.pop().unwrap().0)
            } else {
                None
            };
            let mut expected = Vec::new();
            for (key, version) in rows {
                let value: Option<Vec<u8>> =
                    value_query.query_row(params![table, key.len(), key, version], |r| r.get(0))?;
                report.keys += 1;
                report.transfer_bytes +=
                    key.len() as u64 + value.as_ref().map_or(0, |v| v.len() as u64);
                if let Some(value) = value {
                    expected.push(Row {
                        key,
                        value,
                        version,
                    });
                }
            }
            let actual = snapshot.scan_range(table, &start, end.as_deref(), None)?;
            report.transfer_bytes += actual
                .iter()
                .map(|r| (r.key.len() + r.value.len()) as u64)
                .sum::<u64>();
            if actual != expected {
                return Err(Error::Corrupt("import audit table contents differ"));
            }
            report.visible_rows += actual.len() as u64;
            if let Some(rate) = options.bytes_per_second {
                let duration =
                    Duration::from_secs_f64((report.transfer_bytes - before) as f64 / rate as f64);
                while let Some(remaining) = duration.checked_sub(started.elapsed()) {
                    progress(report)?;
                    std::thread::sleep(remaining.min(Duration::from_millis(100)));
                }
            }
            report.finished = end.is_none();
            progress(report)?;
            if let Some(end) = end {
                start = end;
            } else {
                return Ok(report);
            }
        }
    }
}

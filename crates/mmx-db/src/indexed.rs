//! On-disk MVCC, retaining every committed version. SQLite's WAL transaction
//! provides atomic publication/recovery without a logical branch-size limit.
use crate::{validate, Database, Error, Key, Pending, Result, Row, Write};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const APPLICATION_ID: i64 = 0x4D4D5852;
const CHECKPOINT_BYTES: u64 = 64 * 1024 * 1024;
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);
const SCHEMA: &str = "CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=0),version INTEGER NOT NULL,generation INTEGER NOT NULL);
    INSERT INTO meta VALUES(0,0,0);
    CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v));
    CREATE INDEX history_version ON history(v);
    CREATE INDEX history_key ON history(t,k,v);
    PRAGMA application_id=1296914514; PRAGMA user_version=1;";
type Rows = BTreeMap<Key, (u32, Option<Vec<u8>>)>;
pub(crate) struct Disk {
    conn: Connection,
    path: PathBuf,
    lock: Arc<File>,
    identity: Arc<()>,
    generation: u64,
    pub version: u32,
    pub pending: Pending,
    poisoned: bool,
    _temporary: Option<tempfile::TempDir>,
    staging_root: PathBuf,
    last_checkpoint: Cell<Instant>,
}
pub(crate) struct Snapshot {
    base: Arc<Base>,
    cutoff: u32,
    pub local: Disk,
}
struct Base {
    conn: Mutex<Connection>,
    identity: Arc<()>,
    generation: u64,
    _lock: Arc<File>,
}
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
    )?;
    Ok(())
}
fn meta(conn: &Connection) -> Result<(u32, u64)> {
    let (version, generation): (u64, u64) =
        conn.query_row("SELECT version,generation FROM meta WHERE id=0", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let version = u32::try_from(version).map_err(|_| Error::Corrupt("indexed version overflow"))?;
    if version == u32::MAX {
        return Err(Error::Corrupt("indexed version limit"));
    }
    Ok((version, generation))
}
impl Disk {
    pub fn open(path: &Path) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let mut parents = Vec::new();
        for dir in path.ancestors().take_while(|p| !p.exists()) {
            if let Some(p) = dir.parent() {
                parents.push(p.to_owned());
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
        let existed = path.join("state.sqlite").exists();
        // Import the previous Rust journal once, without ever allowing two
        // writable implementations to reopen diverging copies afterward.
        let legacy = if !existed && path.join("journal.mmx").exists() {
            Some(Database::open(&path)?)
        } else {
            None
        };
        if !existed {
            // Publish a complete database name only after initial schema/import
            // commits and closes. A crash cannot leave a half-initialized live DB.
            let temporary = tempfile::NamedTempFile::new_in(&path)?;
            let mut conn = Connection::open(temporary.path())?;
            conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
            let tx = conn.transaction()?;
            tx.execute_batch(SCHEMA)?;
            if let Some(legacy) = &legacy {
                let mut insert = tx.prepare("INSERT INTO history VALUES(?1,?2,?3,?4,?5)")?;
                for (t, rows) in &legacy.tables {
                    for (k, versions) in rows {
                        for (v, value) in versions {
                            insert.execute(params![t, k.0.len(), &k.0, v, value])?;
                        }
                    }
                }
                drop(insert);
                tx.execute("UPDATE meta SET version=?1", [legacy.version])?;
            }
            tx.commit()?;
            drop(conn);
            temporary
                .persist_noclobber(path.join("state.sqlite"))
                .map_err(|e| e.error)?;
            File::open(&path)?.sync_all()?;
        }
        let conn = Connection::open(path.join("state.sqlite"))?;
        let id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if id != APPLICATION_ID {
            return Err(Error::Corrupt("foreign indexed database"));
        }
        let format: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if format != 1 {
            return Err(Error::Corrupt("indexed database format"));
        }
        configure(&conn)?;
        let staging = path.join("staging");
        fs::create_dir_all(&staging)?;
        for entry in fs::read_dir(&staging)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("branch-"))
                && entry.file_type()?.is_dir()
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
        let (version, generation) = meta(&conn)?;
        Ok(Self {
            conn,
            path,
            lock: Arc::new(lock),
            identity: Arc::new(()),
            generation,
            version,
            pending: BTreeMap::new(),
            poisoned: false,
            _temporary: None,
            staging_root: staging,
            last_checkpoint: Cell::new(Instant::now()),
        })
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn optimize_layout(&mut self) -> Result<bool> {
        self.readable()?;
        let without_rowid: bool = self.conn.query_row(
            "SELECT wr FROM pragma_table_list WHERE schema='main' AND name='history'",
            [],
            |row| row.get(0),
        )?;
        if !without_rowid {
            return Ok(false);
        }
        // A rewrite cannot invalidate a detached branch's publication epoch.
        if !self.pending.is_empty() || Arc::strong_count(&self.identity) != 1 {
            return Err(Error::SnapshotMismatch);
        }
        let generation = self.generation.checked_add(1).ok_or(Error::InvalidEntry)?;
        let result = (|| -> Result<()> {
            let transaction = self.conn.transaction()?;
            transaction.execute_batch(
                "ALTER TABLE history RENAME TO history_legacy;
                 CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v));
                 INSERT INTO history SELECT * FROM history_legacy ORDER BY t,n,k,v;
                 DROP TABLE history_legacy;
                 CREATE INDEX history_version ON history(v);
                 CREATE INDEX history_key ON history(t,k,v);")?;
            transaction.execute("UPDATE meta SET generation=?1 WHERE id=0", [generation])?;
            transaction.commit()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.generation = generation;
        Ok(true)
    }
    pub fn commit_batch(
        &mut self,
        next: u32,
        writes: impl IntoIterator<Item = Write>,
    ) -> Result<()> {
        self.commit(next, writes)
    }
    fn stage(&self, version: u32) -> Result<Disk> {
        self.readable()?;
        let directory = tempfile::Builder::new()
            .prefix("branch-")
            .tempdir_in(&self.staging_root)?;
        let path = directory.path().to_owned();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path.join("writer.lock"))?;
        lock.try_lock().map_err(std::io::Error::from)?;
        let conn = Connection::open(path.join("state.sqlite"))?;
        // Speculative state has no durable reader until replace() copies it
        // into the primary WAL transaction. Even initial schema construction
        // can discard its writes after a crash, avoiding primary-open fsyncs.
        conn.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA synchronous=OFF;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute("UPDATE meta SET version=?1", [version])?;
        Ok(Disk {
            conn,
            path,
            lock: Arc::new(lock),
            identity: Arc::new(()),
            generation: 0,
            version,
            pending: BTreeMap::new(),
            poisoned: false,
            _temporary: Some(directory),
            staging_root: self.staging_root.clone(),
            last_checkpoint: Cell::new(Instant::now()),
        })
    }
    pub fn get(&self, t: &str, k: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        self.readable()?;
        if max.is_none_or(|v| v >= self.version) {
            if let Some(value) = self.pending.get(&(t.to_owned(), Key(k.to_vec()))) {
                return Ok(value.clone());
            }
        }
        Ok(get(&self.conn, t, k, max.unwrap_or(u32::MAX))?.and_then(|(_, v)| v))
    }
    pub fn scan(
        &self,
        t: &str,
        prefix: Option<&[u8]>,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.readable()?;
        let mut rows = scan(&self.conn, t, prefix, start, end, max.unwrap_or(u32::MAX))?;
        overlay(
            &mut rows,
            t,
            &self.pending,
            self.version,
            max,
            prefix,
            start,
            end,
        );
        Ok(visible(rows))
    }
    fn readable(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
    pub fn apply(&mut self, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        self.readable()?;
        let mut pending = self.pending.clone();
        for w in writes {
            validate(&w)?;
            pending.insert((w.table, Key(w.key)), w.value);
        }
        self.pending = pending;
        Ok(())
    }
    pub fn commit(&mut self, next: u32, writes: impl IntoIterator<Item = Write>) -> Result<()> {
        self.readable()?;
        if next <= self.version || next == u32::MAX {
            return Err(Error::Version {
                current: self.version,
                requested: next,
            });
        }
        let mut pending = self.pending.clone();
        for w in writes {
            validate(&w)?;
            pending.insert((w.table, Key(w.key)), w.value);
        }
        let generation = self.generation.checked_add(1).ok_or(Error::InvalidEntry)?;
        let result = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            let mut insert =
                tx.prepare_cached("INSERT OR REPLACE INTO history VALUES(?1,?2,?3,?4,?5)")?;
            for ((t, k), value) in &pending {
                insert.execute(params![t, k.0.len(), &k.0, self.version, value])?;
            }
            drop(insert);
            tx.execute(
                "UPDATE meta SET version=?1,generation=?2 WHERE id=0",
                params![next, generation],
            )?;
            tx.commit()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.version = next;
        self.generation = generation;
        self.pending.clear();
        Ok(())
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        self.readable()?;
        if version > self.version {
            return Err(Error::Version {
                current: self.version,
                requested: version,
            });
        }
        let generation = self.generation.checked_add(1).ok_or(Error::InvalidEntry)?;
        let result = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            tx.execute("DELETE FROM history WHERE v>=?1", [version])?;
            tx.execute(
                "UPDATE meta SET version=?1,generation=?2 WHERE id=0",
                params![version, generation],
            )?;
            tx.commit()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.version = version;
        self.generation = generation;
        self.pending.clear();
        Ok(())
    }
    pub fn fork(&self, version: u32) -> Result<Snapshot> {
        self.readable()?;
        if !self.pending.is_empty() || version > self.version {
            return Err(Error::SnapshotMismatch);
        }
        // Publication's automatic checkpoint runs while this branch's base
        // reader still pins the previous WAL end. Checkpoint in the reader gap
        // before the next branch instead, so the next write can recycle it.
        self.checkpoint_if_due(CHECKPOINT_BYTES, CHECKPOINT_INTERVAL)?;
        let conn = Connection::open_with_flags(
            self.path.join("state.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("BEGIN DEFERRED")?;
        if meta(&conn)? != (self.version, self.generation) {
            return Err(Error::SnapshotMismatch);
        }
        let local = self.stage(version)?;
        Ok(Snapshot {
            base: Arc::new(Base {
                conn: Mutex::new(conn),
                identity: self.identity.clone(),
                generation: self.generation,
                _lock: self.lock.clone(),
            }),
            cutoff: version,
            local,
        })
    }
    fn checkpoint_if_due(&self, minimum_bytes: u64, interval: Duration) -> Result<()> {
        if self.last_checkpoint.get().elapsed() < interval {
            return Ok(());
        }
        self.last_checkpoint.set(Instant::now());
        let bytes = match fs::metadata(self.path.join("state.sqlite-wal")) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        if bytes >= minimum_bytes {
            // PASSIVE never waits for another reader to finish and may leave
            // frames pinned by an intentionally long-lived snapshot. Both
            // checkpoint and commits use the existing FULL-synchronous writer.
            self.conn
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
        }
        Ok(())
    }
    pub fn replace(&mut self, version: u32, branch: &Snapshot) -> Result<()> {
        self.readable()?;
        branch.local.readable()?;
        if !self.pending.is_empty()
            || branch.local.has_pending()
            || branch.cutoff != version
            || branch.local.version < version
            || branch.local.version == u32::MAX
            || !Arc::ptr_eq(&self.identity, &branch.base.identity)
            || self.generation != branch.base.generation
        {
            return Err(Error::SnapshotMismatch);
        }
        let generation = self.generation.checked_add(1).ok_or(Error::InvalidEntry)?;
        self.conn.execute(
            "ATTACH DATABASE ?1 AS branch_delta",
            [branch
                .local
                .path
                .join("state.sqlite")
                .to_str()
                .ok_or(Error::InvalidEntry)?],
        )?;
        let result = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            let began = Instant::now();
            tx.execute("DELETE FROM history WHERE v>=?1", [version])?;
            let deleted = Instant::now();
            tx.execute(
                "INSERT INTO history SELECT * FROM branch_delta.history ORDER BY t,n,k,v",
                [],
            )?;
            let copied = Instant::now();
            tx.execute(
                "UPDATE meta SET version=?1,generation=?2 WHERE id=0",
                params![branch.local.version, generation],
            )?;
            tx.commit()?;
            if std::env::var_os("MMX_PROFILE_REPLAY").is_some() {
                eprintln!(
                    "Replay SQL: delete_ms={:.3} copy_ms={:.3} commit_ms={:.3}",
                    deleted.duration_since(began).as_secs_f64() * 1e3,
                    copied.duration_since(deleted).as_secs_f64() * 1e3,
                    copied.elapsed().as_secs_f64() * 1e3
                );
            }
            Ok(())
        })();
        let detach = self.conn.execute("DETACH DATABASE branch_delta", []);
        let result = result.and_then(|_| {
            detach?;
            Ok(())
        });
        if result.is_err() {
            self.poisoned = true;
        }
        result?;
        self.version = branch.local.version;
        self.generation = generation;
        Ok(())
    }
}
impl Snapshot {
    pub fn get(&self, t: &str, k: &[u8], max: Option<u32>) -> Result<Option<Vec<u8>>> {
        self.local.readable()?;
        if max.is_none_or(|v| v >= self.local.version) {
            if let Some(value) = self.local.pending.get(&(t.to_owned(), Key(k.to_vec()))) {
                return Ok(value.clone());
            }
        }
        if let Some((_, value)) = get(&self.local.conn, t, k, max.unwrap_or(u32::MAX))? {
            return Ok(value);
        }
        if self.cutoff == 0 {
            return Ok(None);
        }
        let conn = self
            .base
            .conn
            .lock()
            .map_err(|_| Error::Corrupt("snapshot lock poisoned"))?;
        Ok(get(&conn, t, k, max.unwrap_or(u32::MAX).min(self.cutoff - 1))?.and_then(|(_, v)| v))
    }
    pub fn scan(
        &self,
        t: &str,
        prefix: Option<&[u8]>,
        start: &[u8],
        end: Option<&[u8]>,
        max: Option<u32>,
    ) -> Result<Vec<Row>> {
        self.local.readable()?;
        let mut rows = if self.cutoff == 0 {
            BTreeMap::new()
        } else {
            let conn = self
                .base
                .conn
                .lock()
                .map_err(|_| Error::Corrupt("snapshot lock poisoned"))?;
            scan(
                &conn,
                t,
                prefix,
                start,
                end,
                max.unwrap_or(u32::MAX).min(self.cutoff - 1),
            )?
        };
        let changes = scan(
            &self.local.conn,
            t,
            prefix,
            start,
            end,
            max.unwrap_or(u32::MAX),
        )?;
        rows.extend(changes);
        overlay(
            &mut rows,
            t,
            &self.local.pending,
            self.local.version,
            max,
            prefix,
            start,
            end,
        );
        Ok(visible(rows))
    }
    pub fn fork(&self, version: u32) -> Result<Self> {
        if self.local.has_pending() || version > self.local.version {
            return Err(Error::SnapshotMismatch);
        }
        let local = self.local.stage(version)?;
        if version >= self.cutoff {
            local.conn.execute(
                "ATTACH DATABASE ?1 AS parent_delta",
                [self
                    .local
                    .path
                    .join("state.sqlite")
                    .to_str()
                    .ok_or(Error::InvalidEntry)?],
            )?;
            local.conn.execute(
                "INSERT INTO history SELECT * FROM parent_delta.history WHERE v<?1",
                [version],
            )?;
            local.conn.execute("DETACH DATABASE parent_delta", [])?;
        }
        Ok(Self {
            base: self.base.clone(),
            cutoff: self.cutoff.min(version),
            local,
        })
    }
    pub fn revert(&mut self, version: u32) -> Result<()> {
        self.local.revert(version)?;
        self.cutoff = self.cutoff.min(version);
        Ok(())
    }
}
fn get(conn: &Connection, t: &str, k: &[u8], max: u32) -> Result<Option<(u32, Option<Vec<u8>>)>> {
    let mut query=conn.prepare_cached("SELECT v,value FROM history WHERE t=?1 AND n=?2 AND k=?3 AND v<=?4 ORDER BY v DESC LIMIT 1")?;
    Ok(query
        .query_row(params![t, k.len(), k, max], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}
fn scan(
    conn: &Connection,
    t: &str,
    prefix: Option<&[u8]>,
    start: &[u8],
    end: Option<&[u8]>,
    max: u32,
) -> Result<Rows> {
    let end_len = end.map(|v| v.len());
    let mut upper = prefix.map(ToOwned::to_owned);
    if let Some(value) = &mut upper {
        while value.last() == Some(&255) {
            value.pop();
        }
        if let Some(last) = value.last_mut() {
            *last += 1;
        } else {
            upper = None;
        }
    }
    let source = if prefix.is_some() {
        "history INDEXED BY history_key"
    } else {
        "history INDEXED BY sqlite_autoindex_history_1"
    };
    let prefix_condition = if prefix.is_some() {
        "AND k>=?7 AND (?8 IS NULL OR k<?8)"
    } else {
        "AND ?7 IS NULL AND ?8 IS NULL"
    };
    let sql = format!(
        "SELECT h.k,h.v,h.value FROM history h JOIN
        (SELECT n,k,MAX(v) v FROM {source} WHERE t=?1 AND v<=?2
            AND (n,k)>=(?3,?4)
            AND (?5 IS NULL OR (n,k)<(?5,?6))
            {prefix_condition}
            GROUP BY n,k) x ON h.t=?1 AND h.n=x.n AND h.k=x.k AND h.v=x.v ORDER BY h.n,h.k"
    );
    let mut query = conn.prepare_cached(&sql)?;
    let rows = query.query_map(
        params![t, max, start.len(), start, end_len, end, prefix, upper],
        |r| Ok((Key(r.get(0)?), (r.get(1)?, r.get(2)?))),
    )?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
fn matches(k: &[u8], prefix: Option<&[u8]>, start: &[u8], end: Option<&[u8]>) -> bool {
    let compare = |a: &[u8], b: &[u8]| a.len().cmp(&b.len()).then(a.cmp(b));
    prefix.is_none_or(|p| k.starts_with(p))
        && compare(k, start).is_ge()
        && end.is_none_or(|end| compare(k, end).is_lt())
}
#[allow(clippy::too_many_arguments)]
fn overlay(
    rows: &mut Rows,
    t: &str,
    pending: &Pending,
    version: u32,
    max: Option<u32>,
    prefix: Option<&[u8]>,
    start: &[u8],
    end: Option<&[u8]>,
) {
    if max.is_none_or(|v| v >= version) {
        for ((name, key), value) in pending {
            if name == t && matches(&key.0, prefix, start, end) {
                rows.insert(key.clone(), (version, value.clone()));
            }
        }
    }
}
fn visible(rows: Rows) -> Vec<Row> {
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

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn failed_sql_publication_rolls_back_deleted_suffix_and_every_insert() {
        let dir = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(dir.path()).unwrap();
        disk.commit(1, [Write::put("t", b"old", b"retained")])
            .unwrap();
        let mut branch = disk.fork(0).unwrap();
        branch
            .local
            .commit_batch(1, [Write::put("t", b"new", b"partial")])
            .unwrap();
        branch
            .local
            .commit_batch(2, [Write::put("trip", b"x", b"reject")])
            .unwrap();
        disk.conn.execute_batch("CREATE TRIGGER reject_branch BEFORE INSERT ON history WHEN NEW.t='trip' BEGIN SELECT RAISE(ABORT,'injected publish failure'); END;").unwrap();
        assert!(disk.replace(0, &branch).is_err());
        assert_eq!(disk.version, 1);
        assert!(matches!(disk.get("t", b"old", None), Err(Error::Poisoned)));
        drop(branch);
        drop(disk);
        let disk = Disk::open(dir.path()).unwrap();
        assert_eq!(disk.version, 1);
        assert_eq!(
            disk.get("t", b"old", None).unwrap(),
            Some(b"retained".to_vec())
        );
        assert_eq!(disk.get("t", b"new", None).unwrap(), None);
    }
    #[test]
    fn failed_sql_commit_keeps_pending_writes_until_reopen_and_never_advances_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(dir.path()).unwrap();
        disk.commit(1, [Write::put("t", b"x", b"old")]).unwrap();
        disk.apply([Write::put("t", b"pending", b"local")]).unwrap();
        disk.conn.execute_batch("PRAGMA query_only=ON").unwrap();
        assert!(disk.commit(2, [Write::put("t", b"x", b"bad")]).is_err());
        assert_eq!(disk.version, 1);
        assert_eq!(disk.pending.len(), 1);
        drop(disk);
        let disk = Disk::open(dir.path()).unwrap();
        assert_eq!(disk.version, 1);
        assert_eq!(disk.get("t", b"x", None).unwrap(), Some(b"old".to_vec()));
        assert_eq!(disk.get("t", b"pending", None).unwrap(), None);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn legacy(path: &Path) -> Disk {
        fs::create_dir_all(path).unwrap();
        let conn = Connection::open(path.join("state.sqlite")).unwrap();
        let schema = SCHEMA.replace(
            "PRIMARY KEY(t,n,k,v));",
            "PRIMARY KEY(t,n,k,v)) WITHOUT ROWID;",
        );
        conn.execute_batch(&schema).unwrap();
        drop(conn);
        let disk = Disk::open(path).unwrap();
        assert!(is_without_rowid(&disk.conn));
        disk
    }
    fn is_without_rowid(conn: &Connection) -> bool {
        conn.query_row(
            "SELECT wr FROM pragma_table_list WHERE schema='main' AND name='history'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }
    fn compare(disk: &Disk, memory: &Database) {
        assert_eq!(disk.version, memory.version());
        for max in [None, Some(0), Some(1), Some(7), Some(20), Some(u32::MAX)] {
            for table in ["a", "b", "missing"] {
                for key in [b"".as_slice(), b"a", b"aa", b"\xff"] {
                    assert_eq!(
                        disk.get(table, key, max).unwrap(),
                        memory.get(table, key, max).map(ToOwned::to_owned)
                    );
                }
                assert_eq!(
                    disk.scan(table, Some(&[]), &[], None, max).unwrap(),
                    memory.scan(table, &[], max)
                );
            }
        }
    }
    #[test]
    fn legacy_layout_rewrite_preserves_all_versions_tombstones_and_snapshot_guards() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = legacy(directory.path());
        let mut memory = Database::memory();
        for height in 0..21u32 {
            let writes: Vec<_> = [b"".as_slice(), b"a", b"aa", b"\xff"]
                .iter()
                .enumerate()
                .map(|(i, key)| {
                    let table = if i % 2 == 0 { "a" } else { "b" };
                    if (height + i as u32).is_multiple_of(4) {
                        Write::delete(table, *key)
                    } else {
                        Write::put(table, *key, vec![height as u8; 5300 + i])
                    }
                })
                .collect();
            disk.commit_batch(height + 1, writes.clone()).unwrap();
            memory.commit_batch(height + 1, writes).unwrap();
        }
        let branch = disk.fork(disk.version).unwrap();
        assert!(matches!(
            disk.optimize_layout(),
            Err(Error::SnapshotMismatch)
        ));
        drop(branch);
        disk.apply([Write::put("a", b"pending", b"pending")])
            .unwrap();
        assert!(matches!(
            disk.optimize_layout(),
            Err(Error::SnapshotMismatch)
        ));
        disk.revert(21).unwrap();
        let generation = disk.generation;
        assert!(disk.optimize_layout().unwrap());
        assert!(!is_without_rowid(&disk.conn));
        assert_eq!(disk.generation, generation + 1);
        assert!(!disk.optimize_layout().unwrap());
        compare(&disk, &memory);
        drop(disk);
        let mut disk = Disk::open(directory.path()).unwrap();
        compare(&disk, &memory);
        // Exercise a detached suffix from the rewritten layout, then every
        // rewind through genesis against the independent journal backend.
        let mut branch = disk.fork(21).unwrap();
        branch
            .local
            .commit_batch(22, [Write::put("a", b"aa", b"new")])
            .unwrap();
        disk.replace(21, &branch).unwrap();
        drop(branch);
        memory
            .commit_batch(22, [Write::put("a", b"aa", b"new")])
            .unwrap();
        compare(&disk, &memory);
        for height in (0..=22).rev() {
            disk.revert(height).unwrap();
            memory.revert(height).unwrap();
            compare(&disk, &memory);
        }
    }
    #[test]
    fn failed_layout_rewrite_rolls_back_ddl_and_requires_writer_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = legacy(directory.path());
        disk.commit_batch(1, [Write::put("a", b"a", vec![7; 65536])])
            .unwrap();
        let pages: u32 = disk
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        disk.conn
            .execute_batch(&format!("PRAGMA max_page_count={pages}"))
            .unwrap();
        assert!(disk.optimize_layout().is_err());
        assert!(matches!(disk.get("a", b"a", None), Err(Error::Poisoned)));
        assert!(is_without_rowid(&disk.conn));
        drop(disk);
        let mut disk = Disk::open(directory.path()).unwrap();
        assert_eq!(disk.version, 1);
        assert_eq!(disk.get("a", b"a", None).unwrap(), Some(vec![7; 65536]));
        assert!(disk.optimize_layout().unwrap());
        assert_eq!(disk.get("a", b"a", None).unwrap(), Some(vec![7; 65536]));
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    #[test]
    fn checkpoints_in_reader_gaps_recycle_the_wal_preserve_snapshots_and_all_rollback_versions() {
        let directory = tempfile::tempdir().unwrap();
        let mut disk = Disk::open(directory.path()).unwrap();
        disk.conn
            .execute_batch("PRAGMA wal_autocheckpoint=0")
            .unwrap();
        let value = vec![19; 16 * 1024];
        for height in 0..12 {
            let mut branch = disk.fork(height).unwrap();
            branch
                .local
                .commit_batch(
                    height + 1,
                    [Write::put("t", height.to_be_bytes(), value.as_slice())],
                )
                .unwrap();
            disk.replace(height, &branch).unwrap();
        }
        let snapshot = disk.fork(12).unwrap();
        disk.commit(13, [Write::put("t", b"extra", b"first")])
            .unwrap();
        disk.checkpoint_if_due(1, Duration::ZERO).unwrap();
        let pinned_bytes = fs::metadata(directory.path().join("state.sqlite-wal"))
            .unwrap()
            .len();
        disk.commit(14, [Write::put("t", b"extra", b"second")])
            .unwrap();
        assert!(
            fs::metadata(directory.path().join("state.sqlite-wal"))
                .unwrap()
                .len()
                > pinned_bytes
        );
        assert_eq!(snapshot.get("t", b"extra", None).unwrap(), None);
        assert_eq!(
            snapshot.get("t", &11u32.to_be_bytes(), None).unwrap(),
            Some(value.clone())
        );
        drop(snapshot);
        disk.checkpoint_if_due(1, Duration::ZERO).unwrap();
        let recyclable_bytes = fs::metadata(directory.path().join("state.sqlite-wal"))
            .unwrap()
            .len();
        disk.commit(15, [Write::put("t", b"extra", b"third")])
            .unwrap();
        assert!(
            fs::metadata(directory.path().join("state.sqlite-wal"))
                .unwrap()
                .len()
                <= recyclable_bytes
        );
        drop(disk);
        let mut disk = Disk::open(directory.path()).unwrap();
        assert_eq!(
            disk.get("t", b"extra", None).unwrap(),
            Some(b"third".to_vec())
        );
        for height in (0..=12u32).rev() {
            disk.revert(height).unwrap();
            assert_eq!(disk.get("t", b"extra", None).unwrap(), None);
            for key in 0..12u32 {
                assert_eq!(
                    disk.get("t", &key.to_be_bytes(), None).unwrap(),
                    (key < height).then(|| value.clone())
                );
            }
        }
    }
}

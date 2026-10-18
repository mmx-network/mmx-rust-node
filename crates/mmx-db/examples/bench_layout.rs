//! Compare physical SQLite layouts using an isolated, validated node snapshot.
//! Replays its actual versioned writes through Store, then checks every row and
//! representative deep rollbacks against the unchanged source.
use mmx_db::{Store, Write};
use rusqlite::{params, Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
    time::Instant,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn same_history(source: &Connection, data: &Path, cutoff: u32) -> Result<()> {
    let target =
        Connection::open_with_flags(data.join("state.sqlite"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let sql = "SELECT t,n,k,v,value FROM history WHERE v<?1 ORDER BY t,n,k,v";
    let mut a = source.prepare(sql)?;
    let mut b = target.prepare("SELECT t,n,k,v,value FROM history ORDER BY t,n,k,v")?;
    let mut a = a.query([cutoff])?;
    let mut b = b.query([])?;
    loop {
        match (a.next()?, b.next()?) {
            (None, None) => break,
            (Some(a), Some(b)) => {
                assert_eq!(a.get::<_, String>(0)?, b.get::<_, String>(0)?);
                assert_eq!(a.get::<_, i64>(1)?, b.get::<_, i64>(1)?);
                assert_eq!(a.get::<_, Vec<u8>>(2)?, b.get::<_, Vec<u8>>(2)?);
                assert_eq!(a.get::<_, u32>(3)?, b.get::<_, u32>(3)?);
                assert_eq!(
                    a.get::<_, Option<Vec<u8>>>(4)?,
                    b.get::<_, Option<Vec<u8>>>(4)?
                );
            }
            _ => return Err("layout history has missing or extra rows".into()),
        }
    }
    Ok(())
}
fn prepare(source: &Connection, data: &Path, cutoff: u32, without_rowid: bool) -> Result<()> {
    std::fs::create_dir_all(data)?;
    let mut target = Connection::open(data.join("state.sqlite"))?;
    // Preparation is disposable; measured Store publications use FULL WAL.
    target.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;
        CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=0),version INTEGER NOT NULL,generation INTEGER NOT NULL);")?;
    target.execute("INSERT INTO meta VALUES(0,?1,0)", [cutoff])?;
    target.execute_batch(&format!("CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v)){};", if without_rowid { " WITHOUT ROWID" } else { "" }))?;
    let transaction = target.transaction()?;
    {
        let mut insert = transaction.prepare("INSERT INTO history VALUES(?1,?2,?3,?4,?5)")?;
        let mut query =
            source.prepare("SELECT t,n,k,v,value FROM history WHERE v<?1 ORDER BY t,n,k,v")?;
        let mut rows = query.query([cutoff])?;
        while let Some(row) = rows.next()? {
            insert.execute(params![
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, Option<Vec<u8>>>(4)?
            ])?;
        }
    }
    transaction.commit()?;
    target.execute_batch(
        "CREATE INDEX history_version ON history(v);
        CREATE INDEX history_key ON history(t,k,v);
        PRAGMA application_id=1296914514; PRAGMA user_version=1;",
    )?;
    Ok(())
}
fn run(source: &Connection, root: &Path, from: u32, to: u32, without_rowid: bool) -> Result<()> {
    let label = if without_rowid {
        "without_rowid"
    } else {
        "rowid"
    };
    let data = root.join(label);
    prepare(source, &data, from + 1, without_rowid)?;
    let mut versions = BTreeMap::<u32, Vec<Write>>::new();
    let mut query = source.prepare("SELECT t,k,v,value FROM history INDEXED BY history_version WHERE v>?1 AND v<=?2 ORDER BY v,t,n,k")?;
    let mut rows = query.query(params![from, to])?;
    while let Some(row) = rows.next()? {
        versions.entry(row.get(2)?).or_default().push(Write {
            table: row.get(0)?,
            key: row.get(1)?,
            value: row.get(3)?,
        });
    }
    let mut store = Store::open_indexed(&data)?;
    let mut next = from + 1;
    let mut staging = 0.0;
    let mut publication = 0.0;
    let started = Instant::now();
    while next <= to {
        let end = next.saturating_add(128).min(to + 1);
        let began = Instant::now();
        let mut branch = store.fork(next)?;
        for height in next..end {
            branch.commit_batch(
                height + 1,
                versions.get(&height).cloned().unwrap_or_default(),
            )?;
        }
        staging += began.elapsed().as_secs_f64();
        let began = Instant::now();
        store.replace_suffix(next, &branch)?;
        publication += began.elapsed().as_secs_f64();
        next = end;
    }
    let elapsed = started.elapsed().as_secs_f64();
    same_history(source, &data, to + 1)?;
    for height in [to - 1, from, 1, 0] {
        store.revert(height + 1)?;
        same_history(source, &data, height + 1)?;
    }
    drop(store);
    println!("{{\"layout\":\"{label}\",\"sqlite\":\"{}\",\"from_height\":{from},\"to_height\":{to},\"blocks\":{},\"seconds\":{elapsed:.6},\"staging_seconds\":{staging:.6},\"publication_seconds\":{publication:.6},\"history_and_rollback_match\":true}}", rusqlite::version(), to - from);
    Ok(())
}
fn main() -> Result<()> {
    let mut source = None;
    let mut output = None;
    let mut from = 4000u32;
    let mut to = 6000u32;
    let mut blocks = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing benchmark argument")?;
        match arg.as_str() {
            "--source-dir" => source = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--from-height" => from = value.parse()?,
            "--to-height" => to = value.parse()?,
            "--blocks" => blocks = Some(value.parse::<u32>()?),
            _ => return Err(format!("unknown benchmark argument {arg}").into()),
        }
    }
    if blocks.is_none() && (from < 1 || from >= to || to >= u32::MAX - 1) {
        return Err("require 1 <= from < to < u32::MAX-1".into());
    }
    let output = output.ok_or("missing --output")?;
    std::fs::create_dir_all(&output)?;
    let temporary = tempfile::Builder::new()
        .prefix("layout-bench-")
        .tempdir_in(output)?;
    let source = Connection::open_with_flags(
        source
            .ok_or("missing --source-dir")?
            .join("chain/selected/state.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    source.execute_batch("BEGIN DEFERRED")?;
    let version: u32 =
        source.query_row("SELECT version FROM meta WHERE id=0", [], |row| row.get(0))?;
    if let Some(blocks) = blocks {
        to = version.checked_sub(1).ok_or("source has no blocks")?;
        from = to
            .checked_sub(blocks)
            .filter(|&from| from > 0)
            .ok_or("invalid --blocks range")?;
        if blocks == 0 {
            return Err("--blocks must be positive".into());
        }
    }
    if version != to + 1 {
        return Err("source must be a frozen snapshot ending at --to-height".into());
    }
    for without_rowid in [true, false] {
        run(&source, temporary.path(), from, to, without_rowid)?;
    }
    Ok(())
}

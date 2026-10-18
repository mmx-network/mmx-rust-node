//! Read-only comparison of range-join plans using the node's bundled SQLite.
use mmx_node::Result;
use rusqlite::{params, Connection, OpenFlags, StatementStatus};
use std::{path::PathBuf, time::Instant};
use vnx::serde_json::json;

fn main() -> Result<()> {
    let mut database = None;
    let mut table = "fork_order".to_owned();
    let mut key_bytes = 8usize;
    let mut tail = 152usize;
    let mut rounds = 3u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing scan benchmark argument")?;
        match arg.as_str() {
            "--database" => database = Some(PathBuf::from(value)),
            "--table" => table = value,
            "--key-bytes" => key_bytes = value.parse()?,
            "--tail" => tail = value.parse()?,
            "--rounds" => rounds = value.parse()?,
            _ => return Err(format!("unknown scan benchmark argument: {arg}").into()),
        }
    }
    if key_bytes == 0 || tail == 0 || rounds == 0 {
        return Err("key bytes, tail and rounds must be positive".into());
    }
    let conn = Connection::open_with_flags(
        database.ok_or("missing --database")?,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.execute_batch("BEGIN DEFERRED")?;
    let version: u32 = conn.query_row("SELECT version FROM meta", [], |row| row.get(0))?;
    let mut statement = conn
        .prepare("SELECT DISTINCT k FROM history WHERE t=?1 AND n=?2 ORDER BY k DESC LIMIT ?3")?;
    let keys = statement
        .query_map(params![table, key_bytes, tail], |row| {
            row.get::<_, Vec<u8>>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let start = keys.last().ok_or("no matching range keys")?;
    let max = version.checked_sub(1).ok_or("empty database")?;
    let none: Option<Vec<u8>> = None;
    let end_len: Option<u64> = None;
    let values = params![table, max, start.len(), start, end_len, none, none, none];
    let range = "(SELECT n,k,MAX(v) v FROM history INDEXED BY sqlite_autoindex_history_1
        WHERE t=?1 AND v<=?2 AND (n,k)>=(?3,?4)
        AND (?5 IS NULL OR (n,k)<(?5,?6)) AND ?7 IS NULL AND ?8 IS NULL
        GROUP BY n,k) x";
    let original = format!(
        "SELECT h.k,h.v,h.value FROM history h JOIN {range}
        ON h.t=?1 AND h.n=x.n AND h.k=x.k AND h.v=x.v ORDER BY h.n,h.k"
    );
    let bounded = format!(
        "SELECT h.k,h.v,h.value FROM {range} CROSS JOIN history h
        WHERE h.t=?1 AND h.n=x.n AND h.k=x.k AND h.v=x.v ORDER BY h.n,h.k"
    );
    let mut expected = None;
    for (name, sql) in [
        ("original", &original),
        ("bounded", &bounded),
        ("bounded", &bounded),
        ("original", &original),
    ] {
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map(values, |row| row.get::<_, String>(3))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut query = conn.prepare(sql)?;
        query.reset_status(StatementStatus::VmStep);
        let began = Instant::now();
        let mut count = 0;
        for _ in 0..rounds {
            let rows = query
                .query_map(values, |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            count = rows.len();
            if let Some(expected) = &expected {
                if expected != &rows {
                    return Err("range queries returned different rows/versions/tombstones".into());
                }
            } else {
                expected = Some(rows);
            }
        }
        println!(
            "{}",
            json!({"sqlite":rusqlite::version(),"query":name,"table":table,
                "rows":count,"rounds":rounds,"milliseconds_per_call":began.elapsed().as_secs_f64()*1000.0/f64::from(rounds),
                "vm_steps_per_call":query.get_status(StatementStatus::VmStep)/rounds as i32,"identical_rows":true,"plan":plan})
        );
    }
    Ok(())
}

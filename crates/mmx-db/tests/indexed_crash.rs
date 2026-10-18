//! Keep crash-worker spawning in its own integration-test process. A fork can
//! briefly retain another test thread's writer-lock descriptor until exec,
//! making immediate drop/reopen assertions fail despite correct RAII ownership.
use mmx_db::{Store, Write};

#[test]
fn indexed_crash_worker() {
    let Ok(path) = std::env::var("MMX_INDEXED_CRASH_DIR") else {
        return;
    };
    let phase = std::env::var("MMX_INDEXED_CRASH_PHASE").unwrap();
    let mut db = Store::open_indexed(&path).unwrap();
    db.commit_batch(1, [Write::put("t", b"old", b"old")])
        .unwrap();
    if phase.starts_with("layout_") {
        db.commit_batch(2, [Write::put("t", b"deleted", vec![42; 262144])])
            .unwrap();
        db.commit_batch(3, [Write::delete("t", b"deleted")])
            .unwrap();
        if phase == "layout_committed" {
            assert!(db.optimize_layout().unwrap());
        } else {
            let conn = rusqlite::Connection::open(std::path::Path::new(&path).join("state.sqlite"))
                .unwrap();
            // Exercise process loss after all DDL and copied rows have reached
            // the WAL, but before its transaction commits.
            conn.execute_batch(
                "PRAGMA cache_size=1; BEGIN IMMEDIATE;
                 ALTER TABLE history RENAME TO history_legacy;
                 CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v));
                 INSERT INTO history SELECT * FROM history_legacy ORDER BY t,n,k,v;
                 DROP TABLE history_legacy;
                 CREATE INDEX history_version ON history(v);
                 CREATE INDEX history_key ON history(t,k,v);
                 UPDATE meta SET generation=generation+1;",
            )
            .unwrap();
            std::mem::forget(conn);
        }
    } else if phase == "committed" {
        let mut branch = db.fork(0).unwrap();
        branch
            .commit_batch(2, [Write::put("t", b"new", b"new")])
            .unwrap();
        db.replace_suffix(0, &branch).unwrap();
    } else {
        let conn =
            rusqlite::Connection::open(std::path::Path::new(&path).join("state.sqlite")).unwrap();
        conn.execute_batch(
            "PRAGMA cache_size=1; BEGIN IMMEDIATE; DELETE FROM history; UPDATE meta SET version=2;",
        )
        .unwrap();
        for i in 0..32u32 {
            conn.execute(
                "INSERT INTO history VALUES('t',4,?1,1,zeroblob(262144))",
                [i.to_be_bytes()],
            )
            .unwrap();
        }
        std::mem::forget(conn);
    }
    // Deliberately bypass every Drop and SQLite close/checkpoint, as after process loss.
    std::process::exit(7);
}

#[test]
fn indexed_layout_process_loss_preserves_complete_schema_history_and_metadata() {
    for phase in ["layout_uncommitted", "layout_committed"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta(id INTEGER PRIMARY KEY CHECK(id=0),version INTEGER NOT NULL,generation INTEGER NOT NULL);
             INSERT INTO meta VALUES(0,0,0);
             CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v)) WITHOUT ROWID;
             CREATE INDEX history_version ON history(v);
             CREATE INDEX history_key ON history(t,k,v);
             PRAGMA application_id=1296914514; PRAGMA user_version=1;",
        )
        .unwrap();
        drop(conn);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "indexed_crash_worker", "--nocapture"])
            .env("MMX_INDEXED_CRASH_DIR", dir.path())
            .env("MMX_INDEXED_CRASH_PHASE", phase)
            .output()
            .unwrap();
        assert_eq!(
            status.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let committed = phase == "layout_committed";
        let conn = rusqlite::Connection::open(&path).unwrap();
        let without_rowid: bool = conn
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema='main' AND name='history'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(without_rowid, !committed);
        assert_eq!(
            conn.query_row("SELECT version,generation FROM meta", [], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, u64>(1)?))
            })
            .unwrap(),
            (3, 3 + u64::from(committed))
        );
        drop(conn);
        let mut db = Store::open_indexed(dir.path()).unwrap();
        assert_eq!(db.version(), 3);
        assert_eq!(db.get("t", b"old", None).unwrap(), Some(b"old".to_vec()));
        assert_eq!(db.get("t", b"deleted", None).unwrap(), None);
        assert_eq!(
            db.get("t", b"deleted", Some(1)).unwrap(),
            Some(vec![42; 262144])
        );
        db.revert(1).unwrap();
        assert_eq!(db.get("t", b"old", None).unwrap(), Some(b"old".to_vec()));
        assert_eq!(db.get("t", b"deleted", None).unwrap(), None);
        db.revert(0).unwrap();
        assert!(db.scan("t", &[], None).unwrap().is_empty());
    }
}
#[test]
fn indexed_process_loss_recovers_complete_old_or_new_branch_without_partial_metadata() {
    for phase in ["uncommitted", "committed"] {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "indexed_crash_worker", "--nocapture"])
            .env("MMX_INDEXED_CRASH_DIR", dir.path())
            .env("MMX_INDEXED_CRASH_PHASE", phase)
            .output()
            .unwrap();
        assert_eq!(
            status.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let db = Store::open_indexed(dir.path()).unwrap();
        assert_eq!(db.version(), if phase == "committed" { 2 } else { 1 });
        assert_eq!(
            db.get("t", b"old", None).unwrap(),
            (phase == "uncommitted").then(|| b"old".to_vec())
        );
        assert_eq!(
            db.get("t", b"new", None).unwrap(),
            (phase == "committed").then(|| b"new".to_vec())
        );
        assert_eq!(db.scan("t", &[], None).unwrap().len(), 1);
    }
}

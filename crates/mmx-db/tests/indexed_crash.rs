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
    if phase == "committed" {
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

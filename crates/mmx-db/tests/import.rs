use mmx_db::{
    native::import::{self, Options},
    Error, Store, Write,
};
use std::{fs, io::Write as IoWrite, path::Path};

fn options() -> Options {
    Options {
        batch_bytes: 96,
        segment_bytes: 4096,
        bytes_per_second: None,
    }
}
fn writes(height: u32) -> Vec<Write> {
    [b"".as_slice(), b"a", b"aa", b"z", b"\xff", b"\xff\xff\x00"]
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            let table = if i % 2 == 0 { "state" } else { "bodies" };
            if (height + i as u32).is_multiple_of(5) {
                Write::delete(table, key)
            } else {
                Write::put(
                    table,
                    key,
                    vec![height as u8; if i == 0 { 1024 } else { i }],
                )
            }
        })
        .collect()
}
fn source(path: &Path, without_rowid: bool) -> Store {
    let mut source = Store::open_indexed(path).unwrap();
    let mut memory = Store::memory();
    for height in 0..18u32 {
        source.commit_batch(height + 1, writes(height)).unwrap();
        memory.commit_batch(height + 1, writes(height)).unwrap();
    }
    drop(source);
    let conn = rusqlite::Connection::open(path.join("state.sqlite")).unwrap();
    let suffix = if without_rowid { " WITHOUT ROWID" } else { "" };
    conn.execute_batch(&format!("BEGIN;
        ALTER TABLE history RENAME TO old_history;
        CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v)){suffix};
        INSERT INTO history SELECT * FROM old_history ORDER BY v DESC,t DESC,n DESC,k DESC;
        DROP TABLE old_history;
        CREATE INDEX history_version ON history(v);
        CREATE INDEX history_key ON history(t,k,v);
        COMMIT;")).unwrap();
    memory
}
fn compare(memory: &Store, disk: &Store) {
    assert_eq!(disk.version(), memory.version());
    for max in [None, Some(0), Some(3), Some(7), Some(17), Some(u32::MAX)] {
        for table in ["state", "bodies", "missing"] {
            for prefix in [b"".as_slice(), b"a", b"\xff"] {
                assert_eq!(
                    disk.scan(table, prefix, max).unwrap(),
                    memory.scan(table, prefix, max).unwrap()
                );
            }
            for key in [b"".as_slice(), b"a", b"aa", b"z", b"\xff", b"\xff\xff\x00"] {
                assert_eq!(
                    disk.get(table, key, max).unwrap(),
                    memory.get(table, key, max).unwrap()
                );
            }
            assert_eq!(
                disk.scan_range(table, b"a", Some(b"zz"), max).unwrap(),
                memory.scan_range(table, b"a", Some(b"zz"), max).unwrap()
            );
        }
    }
}

#[test]
fn physical_and_without_rowid_imports_preserve_all_versions_reads_writes_and_every_rollback() {
    for without_rowid in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("source");
        let to = root.path().join("native");
        let mut memory = source(&from, without_rowid);
        let original = fs::read(from.join("state.sqlite")).unwrap();
        let report = import::indexed(&from, &to, options(), |_| Ok(())).unwrap();
        assert!(report.finished);
        assert_eq!(
            (report.version, report.rows, report.resumed_rows),
            (18, 108, 0)
        );
        assert_eq!(fs::read(from.join("state.sqlite")).unwrap(), original);
        assert!(!to.join("native.import").exists());
        let mut disk = Store::open_native(&to).unwrap();
        compare(&memory, &disk);
        // An imported newest value/tombstone can live in the oldest input run.
        // New memtable values must still override that shuffled history.
        disk.commit_batch(19, writes(91)).unwrap();
        memory.commit_batch(19, writes(91)).unwrap();
        compare(&memory, &disk);
        for height in (0..=19).rev() {
            disk.revert(height).unwrap();
            memory.revert(height).unwrap();
            compare(&memory, &disk);
            disk.checkpoint().unwrap();
            drop(disk);
            disk = Store::open_native(&to).unwrap();
            compare(&memory, &disk);
        }
    }
}

#[test]
fn interrupted_import_resumes_without_recopying_committed_payloads_or_exposing_partial_state() {
    use std::fs::OpenOptions;
    for without_rowid in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("source");
        let to = root.path().join("native");
        let memory = source(&from, without_rowid);
        let mut rows = 0;
        let error = import::indexed(&from, &to, options(), |report| {
            assert!(Store::open_native(&to).is_err());
            rows = report.rows;
            if rows >= 11 {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "cancel import",
                )));
            }
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(error, Error::Io(_)));
        assert!(!to.join("native.manifest").exists());
        assert!(Store::open_native(&to).is_err());
        let mut files: Vec<_> = fs::read_dir(&to)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|s| s == "log"))
            .collect();
        files.sort();
        let active = files.last().unwrap();
        let retained = fs::read(active).unwrap();
        OpenOptions::new()
            .append(true)
            .open(active)
            .unwrap()
            .write_all(b"unpublished torn tail")
            .unwrap();
        let orphan = to.join("native-data-FFFFFFFFFFFFFFFF.log");
        fs::write(&orphan, b"unpublished segment").unwrap();
        let temporary = to.join("native-index-build-interrupted.tmp");
        fs::write(&temporary, b"unpublished index").unwrap();
        let report = import::indexed(&from, &to, options(), |_| Ok(())).unwrap();
        assert_eq!((report.rows, report.resumed_rows), (108, rows));
        assert!(!orphan.exists() && !temporary.exists());
        assert_eq!(&fs::read(active).unwrap()[..retained.len()], &retained);
        let disk = Store::open_native(&to).unwrap();
        compare(&memory, &disk);
    }
}

#[test]
fn transfer_pacing_does_not_accumulate_credit_during_a_long_pause() {
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("source");
    let to = root.path().join("native");
    source(&from, false);
    let rate = 65536;
    let pause = Duration::from_millis(500);
    let started = Instant::now();
    let mut paused = false;
    let report = import::indexed(
        &from,
        &to,
        Options {
            bytes_per_second: Some(rate),
            ..options()
        },
        |report| {
            if report.rows > 0 && !paused {
                paused = true;
                std::thread::sleep(pause);
            }
            Ok(())
        },
    )
    .unwrap();
    // One already-active batch can absorb its own delay. No later batch may
    // spend that idle time. The largest fixture batch transfers under 4 KiB.
    let minimum = Duration::from_secs_f64(report.transfer_bytes as f64 / rate as f64) + pause
        - Duration::from_secs_f64(4096.0 / rate as f64);
    assert!(
        started.elapsed() >= minimum,
        "paused time bypassed subsequent batch pacing"
    );
}

#[test]
fn import_rejects_active_writers_changed_sources_and_foreign_or_completed_targets() {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("source");
    let to = root.path().join("native");
    source(&from, false);
    let mut writer = Store::open_indexed(&from).unwrap();
    assert!(import::indexed(&from, &to, options(), |_| Ok(())).is_err());
    assert!(!to.exists());
    drop(writer);
    assert!(import::indexed(&from, &to, options(), |r| if r.rows > 0 {
        Err(Error::InvalidEntry)
    } else {
        Ok(())
    })
    .is_err());
    let before = fs::read(to.join("native.import")).unwrap();
    writer = Store::open_indexed(&from).unwrap();
    writer
        .commit_batch(19, [Write::put("state", b"new", b"changed")])
        .unwrap();
    drop(writer);
    assert!(import::indexed(&from, &to, options(), |_| Ok(())).is_err());
    assert_eq!(fs::read(to.join("native.import")).unwrap(), before);
    assert!(import::indexed(&from, &from, options(), |_| Ok(())).is_err());
    let foreign = root.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), b"foreign data").unwrap();
    assert!(import::indexed(&from, &foreign, options(), |_| Ok(())).is_err());
    assert_eq!(fs::read(foreign.join("keep")).unwrap(), b"foreign data");
    let completed = root.path().join("completed");
    import::indexed(&from, &completed, options(), |_| Ok(())).unwrap();
    assert!(import::indexed(&from, &completed, options(), |_| Ok(())).is_err());
    let empty = root.path().join("empty");
    drop(Store::open_indexed(&empty).unwrap());
    let empty_native = root.path().join("empty-native");
    let report = import::indexed(&empty, &empty_native, options(), |_| Ok(())).unwrap();
    assert_eq!((report.version, report.rows), (0, 0));
    let db = Store::open_native(empty_native).unwrap();
    assert_eq!(db.version(), 0);
}

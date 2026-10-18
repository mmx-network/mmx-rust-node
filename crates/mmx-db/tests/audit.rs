use mmx_db::{
    native::{audit, import},
    Error, Store, Write,
};
use std::{fs, path::Path};

fn fixture(root: &Path, without_rowid: bool) {
    let path = root.join("source");
    let mut source = Store::open_indexed(&path).unwrap();
    for v in 0..6 {
        let mut writes = Vec::new();
        for key in [b"".as_slice(), b"a", b"z", b"aa", b"\xff", b"\xff\xff"] {
            writes.push(if (v + key.len()).is_multiple_of(3) {
                Write::delete("state", key)
            } else {
                Write::put("state", key, [v as u8])
            });
        }
        writes.push(Write::put("other", b"key", b"unchanged"));
        writes.push(Write::put("other", b"empty", b""));
        writes.push(Write::put("single", b"z", b"value"));
        writes.push(Write::put("deleted", b"key", b""));
        writes.push(Write::delete("deleted", b"key"));
        source.commit_batch(v as u32 + 1, writes).unwrap();
    }
    drop(source);
    if without_rowid {
        let conn = rusqlite::Connection::open(path.join("state.sqlite")).unwrap();
        conn.execute_batch("BEGIN;
            ALTER TABLE history RENAME TO old_history;
            CREATE TABLE history(t TEXT NOT NULL,n INTEGER NOT NULL,k BLOB NOT NULL,v INTEGER NOT NULL,value BLOB,PRIMARY KEY(t,n,k,v)) WITHOUT ROWID;
            INSERT INTO history SELECT * FROM old_history;
            DROP TABLE old_history;
            CREATE INDEX history_version ON history(v);
            CREATE INDEX history_key ON history(t,k,v);
            COMMIT;").unwrap();
    }
    import::indexed(
        &path,
        root.join("native"),
        import::Options {
            bytes_per_second: None,
            ..Default::default()
        },
        |_| Ok(()),
    )
    .unwrap();
}
fn options() -> audit::Options {
    audit::Options {
        keys_per_batch: 2,
        bytes_per_second: None,
    }
}

#[test]
fn compares_complete_ranges_tombstones_and_every_rollback_without_changing_either_database() {
    for without_rowid in [false, true] {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), without_rowid);
        let source = root.path().join("source");
        let native = root.path().join("native");
        let original = fs::read(source.join("state.sqlite")).unwrap();
        let manifest = fs::read(native.join("native.manifest")).unwrap();
        let audit = audit::Verifier::open(&source, &native).unwrap();
        assert_eq!(audit.version(), 6);
        for v in 0..=6 {
            for table in ["state", "other", "deleted", "missing"] {
                let r = audit.table(table, v, options(), |_| Ok(())).unwrap();
                assert!(r.finished);
                assert_eq!(r.version, v);
                if table == "state" && v > 0 {
                    assert_eq!(r.keys, 6);
                }
                if table == "deleted" || table == "missing" {
                    assert_eq!(r.visible_rows, 0);
                }
            }
        }
        assert!(audit.table("state", 7, options(), |_| Ok(())).is_err());
        drop(audit);
        assert_eq!(fs::read(source.join("state.sqlite")).unwrap(), original);
        assert_eq!(fs::read(native.join("native.manifest")).unwrap(), manifest);
        assert_eq!(Store::open_native(&native).unwrap().version(), 6);
    }
}

#[test]
fn rejects_extra_missing_changed_and_deleted_rows_including_unbounded_edges() {
    for change in [
        Write::put("single", b"a", b"extra before first source key"),
        Write::put("state", b"\x00", b"extra between source keys"),
        Write::put("state", b"zzzz", b"extra after last source key"),
        Write::put("missing", b"extra", b"extra in an empty table"),
        Write::put("deleted", b"key", b"resurrected tombstone"),
        Write::delete("other", b"key"),
        Write::put("other", b"key", b"changed"),
    ] {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), false);
        let table = change.table.clone();
        let source = root.path().join("source");
        let native = root.path().join("native");
        let mut target = Store::open_native(&native).unwrap();
        target.revert(5).unwrap();
        let conn = rusqlite::Connection::open(source.join("state.sqlite")).unwrap();
        let mut query = conn
            .prepare("SELECT t,k,value FROM history WHERE v=5")
            .unwrap();
        let original = query
            .query_map([], |r| {
                Ok(Write {
                    table: r.get(0)?,
                    key: r.get(1)?,
                    value: r.get(2)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        target
            .commit_batch(6, original.into_iter().chain([change]))
            .unwrap();
        drop(target);
        let audit = audit::Verifier::open(&source, &native).unwrap();
        assert!(matches!(
            audit.table(&table, 6, options(), |_| Ok(())),
            Err(Error::Corrupt(_))
        ));
    }
}

#[test]
fn refuses_live_writers_and_can_cancel_a_comparison() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path(), false);
    let source = root.path().join("source");
    let native = root.path().join("native");
    let writer = Store::open_indexed(&source).unwrap();
    assert!(audit::Verifier::open(&source, &native).is_err());
    drop(writer);
    let writer = Store::open_native(&native).unwrap();
    assert!(audit::Verifier::open(&source, &native).is_err());
    drop(writer);
    let audit = audit::Verifier::open(&source, &native).unwrap();
    let mut called = 0;
    assert!(audit
        .table("state", 6, options(), |_| {
            called += 1;
            Err(Error::InvalidEntry)
        })
        .is_err());
    assert_eq!(called, 1);
    assert!(
        audit
            .table("state", 6, options(), |_| Ok(()))
            .unwrap()
            .finished
    );
}

use mmx_db::{Database, Error, Store, Write};
use std::fs;
fn compare(memory: &Store, disk: &Store) {
    assert_eq!(memory.version(), disk.version());
    assert_eq!(memory.has_pending(), disk.has_pending());
    for max in [None, Some(0), Some(1), Some(3), Some(17), Some(u32::MAX)] {
        for table in ["a", "b", "absent"] {
            assert_eq!(
                memory.scan(table, &[], max).unwrap(),
                disk.scan(table, &[], max).unwrap()
            );
            for prefix in [b"".as_slice(), b"a", b"\xff", b"\xff\xff"] {
                assert_eq!(
                    memory.scan(table, prefix, max).unwrap(),
                    disk.scan(table, prefix, max).unwrap()
                );
            }
            for (start, end) in [
                (b"".as_slice(), None),
                (b"a", Some(b"zz".as_slice())),
                (b"aa", Some(b"z".as_slice())),
            ] {
                assert_eq!(
                    memory.scan_range(table, start, end, max).unwrap(),
                    disk.scan_range(table, start, end, max).unwrap()
                );
            }
            for key in [b"".as_slice(), b"a", b"aa", b"zz", b"\xff", b"\xff\xff\x00"] {
                assert_eq!(
                    memory.get(table, key, max).unwrap(),
                    disk.get(table, key, max).unwrap()
                );
            }
        }
    }
}
#[test]
fn indexed_mvcc_matches_journal_pending_tombstones_gaps_ranges_reopen_and_every_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = Store::open_indexed(dir.path()).unwrap();
    let mut memory = Store::memory();
    let keys = [b"".as_slice(), b"a", b"aa", b"zz", b"\xff", b"\xff\xff\x00"];
    for height in 0..18u32 {
        let writes: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if (height + i as u32).is_multiple_of(4) {
                    Write::delete(if i % 2 == 0 { "a" } else { "b" }, *k)
                } else {
                    Write::put(
                        if i % 2 == 0 { "a" } else { "b" },
                        *k,
                        [height as u8, i as u8],
                    )
                }
            })
            .collect();
        disk.apply(writes.clone()).unwrap();
        memory.apply(writes).unwrap();
        compare(&memory, &disk);
        let next = height + 1;
        disk.commit(next).unwrap();
        memory.commit(next).unwrap();
        if height % 5 == 0 {
            drop(disk);
            disk = Store::open_indexed(dir.path()).unwrap();
            compare(&memory, &disk);
        }
    }
    for height in (0..=18).rev() {
        disk.revert(height).unwrap();
        memory.revert(height).unwrap();
        compare(&memory, &disk);
        drop(disk);
        disk = Store::open_indexed(dir.path()).unwrap();
        compare(&memory, &disk);
    }
}
#[test]
fn indexed_snapshots_share_old_disk_history_isolate_reads_and_adopt_suffix_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_indexed(dir.path()).unwrap();
    db.commit_batch(1, [Write::put("t", b"old", b"zero")])
        .unwrap();
    db.commit_batch(2, [Write::put("t", b"later", b"one")])
        .unwrap();
    let mut branch = db.fork(1).unwrap();
    assert_eq!(
        branch.get("t", b"old", None).unwrap(),
        Some(b"zero".to_vec())
    );
    assert_eq!(branch.get("t", b"later", None).unwrap(), None);
    branch
        .commit_batch(
            2,
            [
                Write::put("t", b"new", b"branch"),
                Write::delete("t", b"old"),
            ],
        )
        .unwrap();
    db.replace_suffix(1, &branch).unwrap();
    assert_eq!(db.get("t", b"old", None).unwrap(), None);
    assert_eq!(
        db.get("t", b"old", Some(0)).unwrap(),
        Some(b"zero".to_vec())
    );
    assert_eq!(db.get("t", b"later", None).unwrap(), None);
    assert_eq!(db.get("t", b"new", None).unwrap(), Some(b"branch".to_vec()));
    assert!(db.replace_suffix(1, &branch).is_err()); // stale generation, even if the same bytes
    drop(branch);
    drop(db);
    let mut db = Store::open_indexed(dir.path()).unwrap();
    let mut branch = db.fork(2).unwrap();
    db.revert(0).unwrap();
    assert_eq!(
        branch.get("t", b"new", None).unwrap(),
        Some(b"branch".to_vec())
    );
    branch
        .commit_batch(3, [Write::put("t", b"new", b"later")])
        .unwrap();
    assert!(db.replace_suffix(2, &branch).is_err());
    drop(db);
    assert!(Store::open_indexed(dir.path()).is_err());
    drop(branch);
    assert!(Store::open_indexed(dir.path()).is_ok());
}
#[test]
fn indexed_import_preserves_legacy_history_once_and_prevents_diverging_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = Database::open(dir.path()).unwrap();
    old.commit_batch(1, [Write::put("t", b"x", b"old")])
        .unwrap();
    old.commit_batch(3, [Write::put("t", b"x", b"new")])
        .unwrap();
    drop(old);
    let mut db = Store::open_indexed(dir.path()).unwrap();
    assert_eq!(db.version(), 3);
    assert_eq!(db.get("t", b"x", Some(0)).unwrap(), Some(b"old".to_vec()));
    assert_eq!(db.get("t", b"x", Some(1)).unwrap(), Some(b"new".to_vec()));
    assert!(Database::open(dir.path()).is_err());
    db.revert(1).unwrap();
    drop(db);
    let db = Store::open_indexed(dir.path()).unwrap();
    assert_eq!(db.version(), 1);
    assert_eq!(db.get("t", b"x", None).unwrap(), Some(b"old".to_vec()));
    assert!(fs::metadata(dir.path().join("journal.mmx")).unwrap().len() > 8);
}
#[test]
fn indexed_replacement_exceeds_journal_frame_limit_and_preserves_all_versions() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_indexed(dir.path()).unwrap();
    db.commit(1).unwrap();
    let mut branch = db.fork(1).unwrap();
    let mut value = vec![0; 1024 * 1024];
    for height in 1..=65u32 {
        value[0] = height as u8;
        branch
            .commit_batch(
                height + 1,
                [Write::put("large", height.to_be_bytes(), value.as_slice())],
            )
            .unwrap();
    }
    db.replace_suffix(1, &branch).unwrap();
    drop(branch);
    drop(db);
    let mut db = Store::open_indexed(dir.path()).unwrap();
    assert_eq!(db.version(), 66);
    for height in 1..=65u32 {
        assert_eq!(
            db.get("large", &height.to_be_bytes(), None)
                .unwrap()
                .unwrap()[0],
            height as u8
        );
    }
    db.revert(33).unwrap();
    assert!(db
        .get("large", &33u32.to_be_bytes(), None)
        .unwrap()
        .is_none());
    assert!(db
        .get("large", &32u32.to_be_bytes(), None)
        .unwrap()
        .is_some());
}
#[test]
fn indexed_rejects_foreign_sqlite_and_other_writers_without_modifying_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open_indexed(dir.path()).unwrap();
    assert!(Store::open_indexed(dir.path()).is_err());
    drop(db);
    let file = dir.path().join("state.sqlite");
    let conn = rusqlite::Connection::open(file).unwrap();
    conn.pragma_update(None, "application_id", 123).unwrap();
    drop(conn);
    assert!(matches!(
        Store::open_indexed(dir.path()),
        Err(Error::Corrupt("foreign indexed database"))
    ));
}

#[test]
fn indexed_nested_snapshots_survive_parent_drop_and_rewind_before_the_original_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_indexed(dir.path()).unwrap();
    db.commit_batch(1, [Write::put("t", b"x", b"zero")])
        .unwrap();
    db.commit_batch(2, [Write::put("t", b"x", b"one")]).unwrap();
    let mut parent = db.fork(1).unwrap();
    parent
        .commit_batch(2, [Write::put("t", b"x", b"branch-one")])
        .unwrap();
    parent
        .commit_batch(3, [Write::put("t", b"x", b"branch-two")])
        .unwrap();
    let child = parent.fork(2).unwrap();
    drop(parent);
    assert_eq!(
        child.get("t", b"x", None).unwrap(),
        Some(b"branch-one".to_vec())
    );
    assert_eq!(
        child.get("t", b"x", Some(0)).unwrap(),
        Some(b"zero".to_vec())
    );
    db.replace_suffix(1, &child).unwrap();
    drop(child);
    let mut branch = db.fork(2).unwrap();
    branch.revert(0).unwrap();
    branch
        .commit_batch(1, [Write::put("t", b"x", b"new-zero")])
        .unwrap();
    db.replace_suffix(0, &branch).unwrap();
    drop(branch);
    assert_eq!(db.get("t", b"x", None).unwrap(), Some(b"new-zero".to_vec()));
    assert_eq!(fs::read_dir(dir.path().join("staging")).unwrap().count(), 0);
    drop(db);
    fs::create_dir(dir.path().join("staging/branch-interrupted")).unwrap();
    fs::write(
        dir.path().join("staging/branch-interrupted/delta"),
        b"unpublished",
    )
    .unwrap();
    fs::create_dir(dir.path().join("staging/keep-me")).unwrap();
    let _db = Store::open_indexed(dir.path()).unwrap();
    assert!(!dir.path().join("staging/branch-interrupted").exists());
    assert!(dir.path().join("staging/keep-me").exists());
}

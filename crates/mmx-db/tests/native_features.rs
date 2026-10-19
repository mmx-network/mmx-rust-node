use mmx_db::{Store, Write};

#[test]
fn presence_uses_only_indexes_and_respects_tombstones_empty_values_and_snapshot_cutoffs() {
    use std::os::unix::fs::FileExt;
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
    let value = b"index-only-existence-without-payload-reads".repeat(1024);
    db.commit_batch(1, [Write::put("t", b"key", value.clone())])
        .unwrap();
    db.checkpoint().unwrap();
    // Damage a value in this tiny temporary database after checkpointing its
    // reference index. Presence must not open/read/checksum this payload.
    let mut damaged = 0;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("native-data-")
        {
            let bytes = std::fs::read(&path).unwrap();
            if let Some(offset) = bytes.windows(value.len()).position(|v| v == value) {
                let offset = offset + value.len() - 1;
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .write_at(&[bytes[offset] ^ 1], offset as u64)
                    .unwrap();
                damaged += 1;
            }
        }
    }
    assert_eq!(damaged, 1);
    assert!(db.contains("t", b"key", None).unwrap());
    assert!(db.get("t", b"key", None).is_err());
    let mut snapshot = db.fork(1).unwrap();
    assert!(snapshot.contains("t", b"key", None).unwrap());
    snapshot.remove("t", b"key").unwrap();
    assert!(!snapshot.contains("t", b"key", None).unwrap());
    assert!(snapshot.contains("t", b"key", Some(0)).unwrap());
    snapshot.insert("t", b"key", b"").unwrap();
    assert!(snapshot.contains("t", b"key", None).unwrap());
    assert_eq!(snapshot.get("t", b"key", None).unwrap(), Some(vec![]));
    snapshot.commit(2).unwrap();
    let nested = snapshot.fork(2).unwrap();
    assert!(nested.contains("t", b"key", None).unwrap());
    snapshot.revert(0).unwrap();
    assert!(!snapshot.contains("t", b"key", None).unwrap());
    assert!(!snapshot.contains("t", b"missing", None).unwrap());
    assert!(nested.contains("t", b"key", None).unwrap());
    db.remove("t", b"key").unwrap();
    assert!(!db.contains("t", b"key", None).unwrap());
    assert!(db.contains("t", b"key", Some(0)).unwrap());
}

#[test]
fn invalid_commit_batches_preserve_pending_and_committed_state_in_both_backends() {
    let directory = tempfile::tempdir().unwrap();
    for mut db in [Store::memory(), Store::open(directory.path()).unwrap()] {
        db.commit_batch(1, [Write::put("ledger", b"old", b"committed")])
            .unwrap();
        db.insert("ledger", b"pending", b"original").unwrap();
        let writes = || {
            [
                Write::put("ledger", b"new", b"balance"),
                Write::put("chain_meta", b"tip", b"new block"),
            ]
        };
        assert!(db.commit_batch(1, writes()).is_err());
        assert!(db
            .commit_batch(
                2,
                writes()
                    .into_iter()
                    .chain([Write::put("invalid/table", b"key", b"value")])
            )
            .is_err());
        assert_eq!(db.version(), 1);
        assert_eq!(
            db.get("ledger", b"pending", None).unwrap(),
            Some(b"original".to_vec())
        );
        assert_eq!(
            db.get("ledger", b"old", None).unwrap(),
            Some(b"committed".to_vec())
        );
        assert!(db.get("ledger", b"new", None).unwrap().is_none());
        assert!(db.get("chain_meta", b"tip", None).unwrap().is_none());
        db.commit_batch(2, writes()).unwrap();
        assert!(!db.has_pending());
        assert_eq!(
            db.get("ledger", b"new", None).unwrap(),
            Some(b"balance".to_vec())
        );
    }
}
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
                    memory.get(table, key, max).unwrap().is_some(),
                    disk.contains(table, key, max).unwrap()
                );
                assert_eq!(
                    memory.get(table, key, max).unwrap(),
                    disk.get(table, key, max).unwrap()
                );
            }
        }
    }
}
#[test]
fn native_mvcc_matches_memory_pending_tombstones_gaps_ranges_reopen_and_every_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native(dir.path()).unwrap();
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
            disk = Store::open_native(dir.path()).unwrap();
            compare(&memory, &disk);
        }
    }
    for height in (0..=18).rev() {
        disk.revert(height).unwrap();
        memory.revert(height).unwrap();
        compare(&memory, &disk);
        drop(disk);
        disk = Store::open_native(dir.path()).unwrap();
        compare(&memory, &disk);
    }
}
#[test]
fn native_snapshots_share_old_disk_history_isolate_reads_and_adopt_suffix_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
    db.commit_batch(1, [Write::put("t", b"old", b"zero")])
        .unwrap();
    db.commit_batch(2, [Write::put("t", b"later", b"one")])
        .unwrap();
    let mut branch = db.fork(1).unwrap();
    assert!(!dir.path().join("staging").exists());
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
    assert!(!dir.path().join("staging").exists());
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
    let mut db = Store::open_native(dir.path()).unwrap();
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
    assert!(Store::open_native(dir.path()).is_err());
    drop(branch);
    assert!(Store::open_native(dir.path()).is_ok());
}

#[test]
fn memory_replay_delta_preserves_pending_tombstones_ranges_and_pinned_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native(dir.path()).unwrap();
    let mut memory = Store::memory();
    let changes = |height: u32| {
        [b"".as_slice(), b"a", b"aa", b"zz", b"\xff", b"\xff\xff\x00"]
            .into_iter()
            .enumerate()
            .map(|(index, key)| {
                let table = if index % 2 == 0 { "a" } else { "b" };
                if (height + index as u32).is_multiple_of(3) {
                    Write::delete(table, key)
                } else {
                    Write::put(table, key, [height as u8, index as u8])
                }
            })
            .collect::<Vec<_>>()
    };
    for height in 0..18 {
        disk.commit_batch(height + 1, changes(height)).unwrap();
        memory.commit_batch(height + 1, changes(height)).unwrap();
    }
    let mut branch = disk.fork(7).unwrap();
    let mut expected = memory.fork(7).unwrap();
    for height in 7..13 {
        branch.apply(changes(height + 18)).unwrap();
        expected.apply(changes(height + 18)).unwrap();
        compare(&expected, &branch);
        branch.commit(height + 1).unwrap();
        expected.commit(height + 1).unwrap();
        compare(&expected, &branch);
    }
    let child = branch.fork(9).unwrap();
    let mut expected_child = expected.fork(9).unwrap();
    compare(&expected_child, &child);
    disk.replace_suffix(7, &child).unwrap();
    memory.replace_suffix(7, &expected_child).unwrap();
    compare(&memory, &disk);
    // Publication and a subsequent primary rollback cannot change pinned reads.
    disk.revert(0).unwrap();
    compare(&expected, &branch);
    compare(&expected_child, &child);
    drop(child);
    branch.revert(1).unwrap();
    expected.revert(1).unwrap();
    branch.commit_batch(2, changes(30)).unwrap();
    expected.commit_batch(2, changes(30)).unwrap();
    compare(&expected, &branch);
    let mut child = branch.fork(2).unwrap();
    expected_child = expected.fork(2).unwrap();
    drop(branch);
    child.commit_batch(3, changes(31)).unwrap();
    expected_child.commit_batch(3, changes(31)).unwrap();
    compare(&expected_child, &child);
    assert!(!dir.path().join("staging").exists());
}
#[test]
fn native_replacement_exceeds_single_frame_size_and_preserves_all_versions() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
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
    let mut db = Store::open_native(dir.path()).unwrap();
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
fn native_nested_snapshots_survive_parent_drop_and_rewind_before_the_original_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
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
    assert!(!dir.path().join("staging").exists());
    drop(db);
    let db = Store::open_native(dir.path()).unwrap();
    assert_eq!(db.get("t", b"x", None).unwrap(), Some(b"new-zero".to_vec()));
}

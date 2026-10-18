use mmx_db::{native::Options, Error, Store, Write};
use std::fs;
fn options() -> Options {
    Options {
        memtable_bytes: 256,
        segment_bytes: 2048,
        level_factor: 2,
        reader_cache: 2,
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
                    memory.get(table, key, max).unwrap(),
                    disk.get(table, key, max).unwrap()
                );
            }
        }
    }
}

fn changes(height: u32) -> Vec<Write> {
    [b"".as_slice(), b"a", b"aa", b"zz", b"\xff", b"\xff\xff\x00"]
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let table = if i % 2 == 0 { "a" } else { "b" };
            if (height + i as u32).is_multiple_of(4) {
                Write::delete(table, *k)
            } else {
                Write::put(table, *k, [height as u8, i as u8])
            }
        })
        .collect()
}
#[test]
fn native_versions_pending_scans_compaction_recovery_and_every_rollback_match_memory() {
    let directory = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native_with_options(directory.path(), options()).unwrap();
    let mut memory = Store::memory();
    for height in 0..18 {
        disk.apply(changes(height)).unwrap();
        memory.apply(changes(height)).unwrap();
        compare(&memory, &disk);
        disk.commit(height + 1).unwrap();
        memory.commit(height + 1).unwrap();
        compare(&memory, &disk);
        if height % 4 == 0 {
            drop(disk);
            disk = Store::open_native_with_options(directory.path(), options()).unwrap();
            compare(&memory, &disk);
        }
    }
    for height in (0..=18).rev() {
        disk.revert(height).unwrap();
        memory.revert(height).unwrap();
        compare(&memory, &disk);
        disk.checkpoint().unwrap();
        drop(disk);
        disk = Store::open(directory.path()).unwrap();
        compare(&memory, &disk);
    }
    assert!(!directory.path().join("state.sqlite").exists());
    assert!(Store::open(directory.path()).is_err());
}
#[test]
fn native_forks_pin_files_reject_stale_bases_and_publish_only_one_validated_suffix() {
    let directory = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native_with_options(directory.path(), options()).unwrap();
    let mut memory = Store::memory();
    for height in 0..10 {
        disk.commit_batch(height + 1, changes(height)).unwrap();
        memory.commit_batch(height + 1, changes(height)).unwrap();
    }
    let pinned = disk.fork(10).unwrap();
    let expected_pinned = memory.fork(10).unwrap();
    let mut branch = disk.fork(4).unwrap();
    let mut expected = memory.fork(4).unwrap();
    for height in 4..8 {
        branch
            .commit_batch(height + 1, changes(height + 20))
            .unwrap();
        expected
            .commit_batch(height + 1, changes(height + 20))
            .unwrap();
    }
    let mut child = branch.fork(6).unwrap();
    let mut expected_child = expected.fork(6).unwrap();
    drop(branch);
    child.commit_batch(7, changes(50)).unwrap();
    expected_child.commit_batch(7, changes(50)).unwrap();
    compare(&expected_child, &child);
    disk.checkpoint().unwrap();
    compare(&expected_pinned, &pinned);
    disk.replace_suffix(4, &child).unwrap();
    memory.replace_suffix(4, &expected_child).unwrap();
    compare(&memory, &disk);
    assert!(disk.replace_suffix(4, &child).is_err());
    disk.checkpoint().unwrap();
    compare(&expected_pinned, &pinned);
    compare(&expected_child, &child);
    drop(disk);
    assert!(Store::open_native(directory.path()).is_err());
    drop(pinned);
    drop(child);
    let disk = Store::open_native(directory.path()).unwrap();
    compare(&memory, &disk);
}
#[test]
fn native_nested_rewind_to_genesis_cannot_resurrect_a_masked_future() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = Store::open_native_with_options(directory.path(), options()).unwrap();
    db.commit_batch(1, [Write::put("a", b"a", b"zero")])
        .unwrap();
    db.commit_batch(2, [Write::put("a", b"a", b"one")]).unwrap();
    db.checkpoint().unwrap();
    let mut parent = db.fork(1).unwrap();
    parent
        .commit_batch(2, [Write::put("a", b"a", b"branch-one")])
        .unwrap();
    let mut child = parent.fork(2).unwrap();
    drop(parent);
    child.revert(0).unwrap();
    child
        .commit_batch(1, [Write::put("a", b"a", b"new-zero")])
        .unwrap();
    db.replace_suffix(0, &child).unwrap();
    drop(child);
    db.checkpoint().unwrap();
    drop(db);
    let db = Store::open_native(directory.path()).unwrap();
    assert_eq!(db.version(), 1);
    assert_eq!(db.get("a", b"a", None).unwrap(), Some(b"new-zero".to_vec()));
    assert_eq!(
        db.get("a", b"a", Some(99)).unwrap(),
        Some(b"new-zero".to_vec())
    );
}
#[test]
fn legacy_foreign_and_corrupt_manifests_are_rejected_without_fallback_or_modification() {
    let directory = tempfile::tempdir().unwrap();
    for marker in ["state.sqlite", "journal.mmx", "native.import"] {
        let path = directory.path().join(marker);
        fs::write(&path, b"unsupported format").unwrap();
        assert!(matches!(
            Store::open(directory.path()),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"unsupported format");
        fs::remove_file(path).unwrap();
    }
    let other = tempfile::tempdir().unwrap();
    let mut native = Store::open_native(other.path()).unwrap();
    native.commit_batch(1, changes(0)).unwrap();
    native.checkpoint().unwrap();
    drop(native);
    let path = other.path().join("native.manifest");
    let mut bytes = fs::read(&path).unwrap();
    bytes[10] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(Store::open(other.path()), Err(Error::Corrupt(_))));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn failed_checkpoint_recovers_durable_values_and_removes_its_unpublished_index() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(directory.path()).unwrap();
    db.commit_batch(1, [Write::put("a", b"a", b"durable")])
        .unwrap();
    let meta = directory.path().join("native.manifest");
    let saved = directory.path().join("saved.manifest");
    fs::rename(&meta, &saved).unwrap();
    fs::create_dir(&meta).unwrap();
    assert!(db.checkpoint().is_err());
    assert!(matches!(db.get("a", b"a", None), Err(Error::Poisoned)));
    drop(db);
    fs::remove_dir(&meta).unwrap();
    fs::rename(&saved, &meta).unwrap();
    let mut db = Store::open_native(directory.path()).unwrap();
    assert_eq!(db.version(), 1);
    assert_eq!(db.get("a", b"a", None).unwrap(), Some(b"durable".to_vec()));
    db.checkpoint().unwrap();
    drop(db);
    assert_eq!(
        Store::open_native(directory.path())
            .unwrap()
            .get("a", b"a", None)
            .unwrap(),
        Some(b"durable".to_vec())
    );
}

#[test]
fn initial_manifest_can_recover_the_gap_before_its_empty_value_file_is_created() {
    let directory = tempfile::tempdir().unwrap();
    let db = Store::open_native(directory.path()).unwrap();
    drop(db);
    fs::remove_file(directory.path().join("native-data-0000000000000000.log")).unwrap();
    fs::write(
        directory.path().join("native-manifest-interrupted.tmp"),
        b"partial",
    )
    .unwrap();
    let mut db = Store::open_native(directory.path()).unwrap();
    assert_eq!(db.version(), 0);
    db.commit_batch(1, [Write::put("a", b"a", b"value")])
        .unwrap();
    drop(db);
    assert_eq!(
        Store::open_native(directory.path())
            .unwrap()
            .get("a", b"a", None)
            .unwrap(),
        Some(b"value".to_vec())
    );
}

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

fn populated(directory: &std::path::Path) -> (Store, Store) {
    let mut disk = Store::open_native_with_options(directory, options()).unwrap();
    let mut memory = Store::memory();
    for height in 0..18 {
        disk.commit_batch(height + 1, changes(height)).unwrap();
        memory.commit_batch(height + 1, changes(height)).unwrap();
    }
    (disk, memory)
}
fn data_files(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("native-data-")
        })
        .collect()
}

#[test]
fn value_rewrite_preserves_every_version_tombstone_snapshot_and_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let (mut disk, memory) = populated(directory.path());
    let pinned = disk.fork(disk.version()).unwrap();
    let report = disk
        .rewrite_values(
            71,
            |_, _| Ok(true),
            |_, _, mut value| {
                value.push(42);
                Ok(value)
            },
        )
        .unwrap();
    assert_eq!(report.floor, 0);
    assert_eq!(disk.version(), memory.version());
    compare(&memory, &pinned);
    drop(disk);
    drop(pinned);
    let mut disk = Store::open_native_with_options(directory.path(), options()).unwrap();
    assert_eq!(disk.retention_floor(), 0);
    for height in (0..=memory.version()).rev() {
        disk.revert(height).unwrap();
        for table in ["a", "b"] {
            let mut expected = if height == 0 {
                vec![]
            } else {
                memory.scan(table, &[], Some(height - 1)).unwrap()
            };
            for row in &mut expected {
                row.value.push(42);
            }
            assert_eq!(disk.scan(table, &[], None).unwrap(), expected);
        }
    }
}

#[test]
fn failed_value_rewrite_never_publishes_partial_values_or_advances_history_floor() {
    let directory = tempfile::tempdir().unwrap();
    let (mut disk, memory) = populated(directory.path());
    let mut calls = 0;
    assert!(disk
        .rewrite_values(
            72,
            |_, _| Ok(true),
            |_, _, _| {
                calls += 1;
                if calls == 7 {
                    return Err(Error::Corrupt("injected rewrite failure"));
                }
                Ok(vec![9])
            }
        )
        .is_err());
    drop(disk);
    let disk = Store::open_native_with_options(directory.path(), options()).unwrap();
    assert_eq!(disk.retention_floor(), 0);
    assert_ne!(disk.collection_tag(), Some(72));
    compare(&memory, &disk);
}
fn retained(memory: &Store, disk: &Store, floor: u32) {
    assert_eq!(memory.version(), disk.version());
    assert_eq!(disk.retention_floor(), floor);
    for max in [None, Some(floor - 1), Some(floor), Some(17), Some(u32::MAX)] {
        for table in ["a", "b", "absent"] {
            for prefix in [b"".as_slice(), b"a", b"\xff", b"\xff\xff"] {
                assert_eq!(
                    memory.scan(table, prefix, max).unwrap(),
                    disk.scan(table, prefix, max).unwrap()
                );
            }
            for key in [b"".as_slice(), b"a", b"aa", b"zz", b"\xff", b"\xff\xff\x00"] {
                assert_eq!(
                    memory.get(table, key, max).unwrap(),
                    disk.get(table, key, max).unwrap()
                );
            }
            assert_eq!(
                memory.scan_range(table, b"a", Some(b"zz"), max).unwrap(),
                disk.scan_range(table, b"a", Some(b"zz"), max).unwrap()
            );
        }
    }
}
#[test]
fn purge_preserves_boundary_state_history_tombstones_and_pinned_snapshots_then_reclaims_files() {
    let directory = tempfile::tempdir().unwrap();
    let (mut disk, mut memory) = populated(directory.path());
    let snapshot = disk.fork(18).unwrap();
    let expected = memory.fork(18).unwrap();
    let old_files = data_files(directory.path());
    let report = disk.purge_before(8).unwrap();
    assert_eq!(report.floor, 8);
    assert!(report.new_value_bytes < report.old_value_bytes);
    assert!(report.retired_files > 0);
    retained(&memory, &disk, 8);
    compare(&expected, &snapshot);
    assert!(
        old_files.iter().all(|p| p.exists()),
        "snapshots lost pinned value files"
    );
    assert!(matches!(
        disk.get("a", b"a", Some(6)),
        Err(Error::Pruned { floor: 8, .. })
    ));
    assert!(matches!(
        disk.revert(7),
        Err(Error::Pruned { floor: 8, .. })
    ));
    assert!(matches!(disk.fork(7), Err(Error::Pruned { floor: 8, .. })));
    assert!(disk.replace_suffix(7, &snapshot).is_err());
    drop(snapshot);
    assert!(
        old_files.iter().all(|p| !p.exists()),
        "retired values stayed allocated after snapshot release"
    );
    let bytes = data_files(directory.path())
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .sum::<u64>();
    assert_eq!(bytes, report.new_value_bytes);
    let no_op = disk.purge_before(8).unwrap();
    assert_eq!(no_op.retired_files, 0);
    assert_eq!(no_op.new_value_bytes, no_op.old_value_bytes);
    disk.revert(8).unwrap();
    memory.revert(8).unwrap();
    retained(&memory, &disk, 8);
    disk.commit_batch(9, changes(40)).unwrap();
    memory.commit_batch(9, changes(40)).unwrap();
    retained(&memory, &disk, 8);
    drop(disk);
    let disk = Store::open_native(directory.path()).unwrap();
    retained(&memory, &disk, 8);
}
#[test]
fn interrupted_collection_keeps_a_durable_floor_and_can_retry_without_resurrecting_history() {
    let directory = tempfile::tempdir().unwrap();
    let (mut disk, mut memory) = populated(directory.path());
    disk.commit_batch(19, [Write::put("a", b"", vec![9; 16384])])
        .unwrap();
    memory
        .commit_batch(19, [Write::put("a", b"", vec![9; 16384])])
        .unwrap();
    let before_files = data_files(directory.path()).len();
    let mut calls = 0;
    assert!(disk
        .purge_before_with_tag(8, 42, |_, _| {
            calls += 1;
            if calls == 2 {
                return Err(Error::Corrupt("injected retention failure"));
            }
            Ok(true)
        })
        .is_err());
    assert!(matches!(disk.get("a", b"a", None), Err(Error::Poisoned)));
    assert!(
        data_files(directory.path()).len() > before_files,
        "failure did not leave a staged value segment"
    );
    drop(disk);
    let mut disk = Store::open_native(directory.path()).unwrap();
    assert_eq!(disk.collection_tag(), None);
    assert_eq!(
        data_files(directory.path()).len(),
        before_files,
        "unpublished GC segment was not removed"
    );
    retained(&memory, &disk, 8);
    assert!(matches!(
        disk.revert(0),
        Err(Error::Pruned { floor: 8, .. })
    ));
    let report = disk.purge_before_with_tag(8, 42, |_, _| Ok(true)).unwrap();
    assert!(report.retired_files > 0);
    retained(&memory, &disk, 8);
    drop(disk);
    let disk = Store::open_native(directory.path()).unwrap();
    assert_eq!(disk.collection_tag(), Some(42));
    retained(&memory, &disk, 8);
}

#[test]
fn version_one_manifest_reopens_and_upgrades_to_durable_collection_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native(directory.path()).unwrap();
    for height in 0..4u32 {
        disk.commit_batch(
            height + 1,
            [Write::put("state", b"key", height.to_le_bytes())],
        )
        .unwrap();
    }
    disk.checkpoint().unwrap();
    drop(disk);
    let path = directory.path().join("native.manifest");
    let mut manifest = fs::read(&path).unwrap();
    assert_eq!(&manifest[..8], b"MMXNDB\x02\n");
    // Version one had no collected_floor / caller policy marker.
    manifest.drain(24..36);
    manifest[6] = 1;
    let end = manifest.len() - 4;
    let checksum = crc32fast::hash(&manifest[..end]);
    manifest[end..].copy_from_slice(&checksum.to_le_bytes());
    fs::write(&path, manifest).unwrap();
    let mut disk = Store::open_native(directory.path()).unwrap();
    assert_eq!(
        disk.get("state", b"key", Some(0)).unwrap(),
        Some(0u32.to_le_bytes().to_vec())
    );
    disk.purge_before_with_tag(3, 17, |_, _| Ok(true)).unwrap();
    drop(disk);
    let mut disk = Store::open_native(directory.path()).unwrap();
    assert_eq!(disk.retention_floor(), 3);
    assert_eq!(disk.collection_tag(), Some(17));
    assert!(disk.revert(2).is_err());
    assert_eq!(
        disk.get("state", b"key", Some(2)).unwrap(),
        Some(2u32.to_le_bytes().to_vec())
    );
    assert_eq!(&fs::read(path).unwrap()[..8], b"MMXNDB\x02\n");
}

#[test]
fn purge_skips_unneeded_payloads_and_reclaims_corrupt_expired_values() {
    use mmx_db::{journal::ValueRef, sorted::Run};
    use std::io::{Seek, SeekFrom, Write as IoWrite};
    let directory = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native(directory.path()).unwrap();
    for height in 0..4u32 {
        disk.commit_batch(
            height + 1,
            [Write::put("state", b"key", vec![height as u8; 4096])],
        )
        .unwrap();
    }
    disk.checkpoint().unwrap();
    let index = fs::read_dir(directory.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|v| v == "sst"))
        .unwrap();
    let run = Run::open(&index).unwrap();
    let old = run
        .get("state", b"key", 0)
        .unwrap()
        .unwrap()
        .write
        .value
        .unwrap();
    let reference = ValueRef::decode(&old).unwrap();
    drop(run);
    let path = directory
        .path()
        .join(format!("native-data-{:016X}.log", reference.file_id));
    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(reference.offset)).unwrap();
    file.write_all(&[255]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(disk.get("state", b"key", Some(0)).is_err());
    disk.purge_before(3).unwrap();
    assert!(!path.exists());
    assert_eq!(
        disk.get("state", b"key", Some(2)).unwrap(),
        Some(vec![2; 4096])
    );
    assert_eq!(
        disk.get("state", b"key", None).unwrap(),
        Some(vec![3; 4096])
    );
    drop(disk);
    let disk = Store::open_native(directory.path()).unwrap();
    assert_eq!(disk.retention_floor(), 3);
    assert_eq!(
        disk.get("state", b"key", None).unwrap(),
        Some(vec![3; 4096])
    );
}
#[test]
fn whole_key_retention_is_distinct_from_version_pruning_and_pending_or_backwards_purges_fail() {
    let directory = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native(directory.path()).unwrap();
    disk.commit_batch(
        1,
        [
            Write::put("body", b"old", vec![1; 16384]),
            Write::put("state", b"current", b"unchanged"),
        ],
    )
    .unwrap();
    disk.commit_batch(2, [Write::put("body", b"new", vec![2; 16384])])
        .unwrap();
    disk.apply([Write::put("state", b"pending", b"staged")])
        .unwrap();
    assert!(disk.purge_before(1).is_err());
    disk.revert(2).unwrap();
    let old_files = data_files(directory.path());
    let report = disk
        .purge_before_with(1, |table, key| Ok(!(table == "body" && key == b"old")))
        .unwrap();
    assert!(report.old_value_bytes > report.new_value_bytes + 15000);
    assert!(old_files.iter().all(|p| !p.exists()));
    assert!(disk.get("body", b"old", None).unwrap().is_none());
    assert_eq!(
        disk.get("body", b"new", None).unwrap(),
        Some(vec![2; 16384])
    );
    assert_eq!(
        disk.get("state", b"current", None).unwrap(),
        Some(b"unchanged".to_vec())
    );
    assert!(disk.purge_before(0).is_err());
    assert!(disk.purge_before(3).is_err());
    drop(disk);
    let disk = Store::open_native(directory.path()).unwrap();
    assert!(disk.get("body", b"old", None).unwrap().is_none());
    assert_eq!(disk.retention_floor(), 1);
}

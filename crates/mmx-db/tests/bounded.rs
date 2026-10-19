use mmx_db::{native::Options, Error, ReadBudget, Store, Write};
fn budget(rows: usize) -> ReadBudget {
    ReadBudget {
        rows,
        keys: 1_000_000,
        bytes: 16 * 1024 * 1024,
    }
}
fn key(n: u8) -> Vec<u8> {
    if n.is_multiple_of(3) {
        vec![n]
    } else {
        vec![0, n]
    }
}
fn compare(reference: &Store, disk: &Store) {
    for max in [None, Some(0), Some(3), Some(17), Some(u32::MAX)] {
        for (start, end) in [
            (vec![], None),
            (vec![0], Some(vec![0, 8])),
            (vec![0, 4], Some(vec![0, 4])),
            (vec![0, 4], None),
        ] {
            let expected = reference
                .scan_range("rows", &start, end.as_deref(), max)
                .unwrap();
            for limit in [0, 1, 3, 100] {
                for db in [reference, disk] {
                    assert_eq!(
                        db.scan_range_bounded(
                            "rows",
                            &start,
                            end.as_deref(),
                            max,
                            &mut budget(limit)
                        )
                        .unwrap(),
                        expected.iter().take(limit).cloned().collect::<Vec<_>>()
                    );
                }
            }
        }
        for n in 0..12 {
            assert_eq!(
                disk.get_bounded("rows", &key(n), max, &mut budget(1))
                    .unwrap(),
                reference.get("rows", &key(n), max).unwrap()
            );
        }
    }
}
#[test]
fn streaming_ranges_match_memory_history_tombstones_pending_empty_values_rewind_and_nested_snapshots(
) {
    let dir = tempfile::tempdir().unwrap();
    let mut disk = Store::open_native_with_options(
        dir.path(),
        Options {
            memtable_bytes: 256,
            segment_bytes: 2048,
            level_factor: 2,
            reader_cache: 2,
        },
    )
    .unwrap();
    let mut memory = Store::memory();
    for height in 0..20u32 {
        let writes: Vec<_> = (0..12)
            .map(|n| {
                if (u32::from(n) + height) % 7 == 0 {
                    Write::delete("rows", key(n))
                } else {
                    Write::put("rows", key(n), vec![height as u8; (n % 4) as usize])
                }
            })
            .collect();
        for db in [&mut disk, &mut memory] {
            db.commit_batch(height + 1, writes.clone()).unwrap();
        }
    }
    for db in [&mut disk, &mut memory] {
        db.apply([
            Write::delete("rows", key(3)),
            Write::put("rows", key(4), b"pending"),
        ])
        .unwrap();
    }
    compare(&memory, &disk);
    for db in [&mut disk, &mut memory] {
        db.revert(7).unwrap();
    }
    compare(&memory, &disk);
    let mut left = disk.fork(7).unwrap();
    let mut right = memory.fork(7).unwrap();
    for db in [&mut left, &mut right] {
        db.commit_batch(
            8,
            [
                Write::put("rows", key(4), b"branch"),
                Write::delete("rows", key(6)),
            ],
        )
        .unwrap();
        db.insert("rows", &key(9), b"").unwrap();
    }
    compare(&right, &left);
    for db in [&mut left, &mut right] {
        db.revert(5).unwrap();
    }
    let left = left.fork(4).unwrap();
    let right = right.fork(4).unwrap();
    compare(&right, &left);
    disk.commit_batch(8, [Write::put("rows", key(4), b"different parent")])
        .unwrap();
    compare(&right, &left);
}
#[test]
fn index_and_hidden_version_work_is_bounded_after_historical_reads_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
    for height in 0..64u32 {
        db.commit_batch(
            height + 1,
            [Write::put("rows", b"key", height.to_le_bytes())],
        )
        .unwrap();
    }
    db.checkpoint().unwrap();
    let small = || ReadBudget {
        rows: 1,
        keys: 8,
        bytes: 1024,
    };
    assert!(matches!(
        db.get_bounded("rows", b"key", Some(0), &mut small()),
        Err(Error::ReadBudget)
    ));
    assert!(matches!(
        db.scan_range_bounded("rows", b"key", Some(b"kez"), Some(0), &mut small()),
        Err(Error::ReadBudget)
    ));
    assert_eq!(
        db.get_bounded("rows", b"key", None, &mut budget(1))
            .unwrap(),
        Some(63u32.to_le_bytes().to_vec())
    );
    let pinned = db.fork(1).unwrap();
    assert!(matches!(
        pinned.get_bounded("rows", b"key", None, &mut small()),
        Err(Error::ReadBudget)
    ));
    db.revert(1).unwrap();
    assert!(matches!(
        db.get_bounded("rows", b"key", None, &mut small()),
        Err(Error::ReadBudget)
    ));
    assert_eq!(
        db.get_bounded("rows", b"key", None, &mut budget(1))
            .unwrap(),
        Some(0u32.to_le_bytes().to_vec())
    );
    drop(pinned);
    drop(db);
    let db = Store::open_native(dir.path()).unwrap();
    assert_eq!(
        db.get_bounded("rows", b"key", None, &mut budget(1))
            .unwrap(),
        Some(0u32.to_le_bytes().to_vec())
    );
}
#[test]
fn byte_budget_precedes_payload_reads_limits_stop_and_overlays_hide_corrupt_obsolete_values() {
    use std::{fs::OpenOptions, os::unix::fs::FileExt};
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
    db.commit_batch(
        1,
        [
            Write::put("rows", b"a", b"small"),
            Write::put("rows", b"z", vec![173; 65536]),
        ],
    )
    .unwrap();
    db.checkpoint().unwrap();
    let path = dir.path().join("native-data-0000000000000000.log");
    let bytes = std::fs::read(&path).unwrap();
    let offset = bytes.windows(32).position(|v| v == [173; 32]).unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .write_at(&[172], offset as u64)
        .unwrap();
    assert!(matches!(db.get("rows", b"z", None), Err(Error::Corrupt(_))));
    let small = || ReadBudget {
        rows: 100,
        keys: 1000,
        bytes: 1024,
    };
    assert!(matches!(
        db.get_bounded("rows", b"z", None, &mut small()),
        Err(Error::ReadBudget)
    ));
    assert!(matches!(
        db.scan_range_bounded("rows", b"z", None, None, &mut small()),
        Err(Error::ReadBudget)
    ));
    let rows = db
        .scan_range_bounded("rows", b"", None, None, &mut budget(1))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, b"a");
    assert!(db
        .scan_range_bounded("rows", b"", None, None, &mut budget(0))
        .unwrap()
        .is_empty());
    let mut quota = ReadBudget {
        rows: 1,
        keys: 1000,
        bytes: 4096,
    };
    assert_eq!(
        db.get_bounded("rows", b"a", None, &mut quota).unwrap(),
        Some(b"small".to_vec())
    );
    let consumed = 4096 - quota.bytes;
    assert!(consumed >= 6);
    quota.bytes = consumed - 1;
    assert!(matches!(
        db.get_bounded("rows", b"a", None, &mut quota),
        Err(Error::ReadBudget)
    ));
    let mut branch = db.fork(1).unwrap();
    branch.insert("rows", b"z", b"new").unwrap();
    let rows = branch
        .scan_range_bounded("rows", b"", None, None, &mut small())
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].value, b"new");
    assert_eq!(
        branch
            .get_bounded("rows", b"z", None, &mut small())
            .unwrap(),
        Some(b"new".to_vec())
    );
}
#[test]
fn tombstone_scans_exhaust_the_work_budget_and_never_truncate_a_total() {
    for disk in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut db = if disk {
            Store::open_native(dir.path()).unwrap()
        } else {
            Store::memory()
        };
        db.commit_batch(
            1,
            (0..64u32).map(|n| Write::delete("rows", n.to_be_bytes())),
        )
        .unwrap();
        let mut quota = ReadBudget {
            rows: 1,
            keys: 8,
            bytes: 1024,
        };
        assert!(matches!(
            db.scan_range_bounded("rows", &[], None, None, &mut quota),
            Err(Error::ReadBudget)
        ));
        assert_eq!(quota.keys, 0);
        assert!(db
            .scan_range_bounded("rows", &[], None, None, &mut budget(100))
            .unwrap()
            .is_empty());
    }
}
#[test]
fn index_probe_bytes_are_bounded_before_allocating_an_unrelated_large_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open_native(dir.path()).unwrap();
    db.commit_batch(
        1,
        [
            Write::put("balances", [0; 64], [0; 16]),
            Write::put("vm_entries", vec![2; 512 * 1024], []),
        ],
    )
    .unwrap();
    db.checkpoint().unwrap();
    let quota = || ReadBudget {
        rows: 1,
        keys: 1000,
        bytes: 4096,
    };
    assert!(matches!(
        db.get_bounded("balances", &[0; 64], None, &mut quota()),
        Err(Error::ReadBudget)
    ));
    assert!(matches!(
        db.scan_range_bounded("balances", &[0; 64], None, None, &mut quota()),
        Err(Error::ReadBudget)
    ));
    assert_eq!(
        db.get_bounded("balances", &[0; 64], None, &mut budget(1))
            .unwrap(),
        Some(vec![0; 16])
    );
}

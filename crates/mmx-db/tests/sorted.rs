use mmx_db::{
    sorted::{merge, Builder, Entry, Run, View},
    Write,
};
use std::{fs, path::Path};

fn fixture(path: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    for table in ["a", "b"] {
        for key in [b"".as_slice(), b"a", b"z", b"\xff", b"aa", b"\xff\xff"] {
            for version in [9, 4, 0] {
                entries.push(Entry {
                    version,
                    write: if version == 9 {
                        Write::delete(table, key)
                    } else if version == 4 {
                        Write::put(table, key, [])
                    } else {
                        Write::put(table, key, [0, 255, key.len() as u8])
                    },
                });
            }
        }
    }
    let mut builder = Builder::new(path.parent().unwrap(), 2).unwrap();
    for entry in &entries {
        builder.push(entry).unwrap();
    }
    let run = builder.finish(path).unwrap();
    assert_eq!(run.metadata().records, entries.len() as u64);
    assert_eq!(run.metadata().level, 2);
    assert_eq!(run.unique_keys(), 12);
    assert_eq!(
        run.entries().collect::<mmx_db::Result<Vec<_>>>().unwrap(),
        entries
    );
    entries
}

#[test]
fn merge_masks_rolled_back_suffix_and_purge_keeps_each_boundary_value_or_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("first.sst");
    let entries = fixture(&first_path);
    let first = Run::open(&first_path).unwrap();
    let mut builder = Builder::new(directory.path(), 0).unwrap();
    for (key, version, value) in [
        (b"a".as_slice(), 7, Some(b"replacement".as_slice())),
        (b"a", 4, None),
        (b"z", 6, None),
    ] {
        builder
            .push(&Entry {
                version,
                write: Write {
                    table: "a".into(),
                    key: key.into(),
                    value: value.map(ToOwned::to_owned),
                },
            })
            .unwrap();
    }
    let second = builder
        .finish(&directory.path().join("second.sst"))
        .unwrap();
    let views = [
        View {
            run: &first,
            cutoff: 9,
        },
        View {
            run: &second,
            cutoff: u32::MAX,
        },
    ];
    let full = merge(&directory.path().join("full.sst"), 1, &views, None)
        .unwrap()
        .unwrap();
    let pruned = merge(&directory.path().join("pruned.sst"), 1, &views, Some(5))
        .unwrap()
        .unwrap();
    assert_eq!(pruned.metadata().records, 14);
    for max in 5..12 {
        for table in ["a", "b"] {
            for key in [b"".as_slice(), b"a", b"z", b"\xff", b"aa", b"\xff\xff"] {
                assert_eq!(
                    full.get(table, key, max).unwrap(),
                    pruned.get(table, key, max).unwrap()
                );
            }
        }
    }
    // An explicit deletion at the boundary wins a duplicate old empty value.
    assert_eq!(pruned.get("a", b"a", 5).unwrap().unwrap().write.value, None);
    assert_eq!(
        pruned.get("a", b"a", 7).unwrap().unwrap().write.value,
        Some(b"replacement".to_vec())
    );
    assert_eq!(pruned.get("a", b"z", 6).unwrap().unwrap().write.value, None);
    assert!(pruned.entries().all(|entry| entry.unwrap().version >= 4));
    // Existing snapshots keep the entire old history while new output is built.
    assert_eq!(
        first.entries().collect::<mmx_db::Result<Vec<_>>>().unwrap(),
        entries
    );
    let none = merge(
        &directory.path().join("empty.sst"),
        0,
        &[View {
            run: &first,
            cutoff: 0,
        }],
        Some(5),
    )
    .unwrap();
    assert!(none.is_none());
    assert!(!directory.path().join("empty.sst").exists());
}

#[test]
fn native_size_first_order_history_tombstones_and_independent_snapshot_cursors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.sst");
    let entries = fixture(&path);
    let run = Run::open(&path).unwrap();
    for max in [0, 1, 3, 4, 8, 9, u32::MAX] {
        for table in ["a", "b", "absent"] {
            for key in [b"".as_slice(), b"a", b"\xff", b"aa", b"absent"] {
                let expected = entries.iter().find(|entry| {
                    entry.write.table == table && entry.write.key == key && entry.version <= max
                });
                assert_eq!(run.get(table, key, max).unwrap().as_ref(), expected);
            }
        }
    }
    let snapshot = run.clone();
    let mut first = run.entries();
    let mut second = snapshot.entries_from("a", b"aa").unwrap();
    assert_eq!(first.next().unwrap().unwrap(), entries[0]);
    assert_eq!(second.next().unwrap().unwrap(), entries[12]);
    assert_eq!(first.next().unwrap().unwrap(), entries[1]);
    drop(run);
    assert_eq!(second.next().unwrap().unwrap(), entries[13]);
    assert!(snapshot.entries_from("z", b"").unwrap().next().is_none());
    assert_eq!(
        snapshot.get("a", b"a", 4).unwrap().unwrap().write.value,
        Some(vec![])
    );
    assert_eq!(
        snapshot.get("a", b"a", 9).unwrap().unwrap().write.value,
        None
    );
}

#[test]
fn unsorted_duplicate_or_unpublished_files_cannot_replace_a_committed_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.sst");
    let entries = fixture(&path);
    let saved = fs::read(&path).unwrap();
    let mut builder = Builder::new(directory.path(), 0).unwrap();
    builder.push(&entries[0]).unwrap();
    assert!(builder.push(&entries[0]).is_err());
    builder.push(&entries[1]).unwrap();
    assert!(builder.push(&entries[0]).is_err());
    assert!(builder.finish(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), saved);
    assert!(Builder::new(directory.path(), 0)
        .unwrap()
        .finish(&directory.path().join("empty.sst"))
        .is_err());
    let mut abandoned = Builder::new(directory.path(), 0).unwrap();
    abandoned.push(&entries[0]).unwrap();
    drop(abandoned);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn corruption_in_headers_indexes_keys_values_and_truncation_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("0.sst");
    fixture(&path);
    let saved = fs::read(&path).unwrap();
    for offset in [0, 8, 24, 60, saved.len() - 5, saved.len() - 1] {
        let mut damaged = saved.clone();
        damaged[offset] ^= 1;
        fs::write(&path, &damaged).unwrap();
        assert!(
            Run::open(&path).is_err(),
            "accepted damaged offset {offset}"
        );
    }
    for length in [0, 63, 64, saved.len() - 1] {
        fs::write(&path, &saved[..length]).unwrap();
        assert!(Run::open(&path).is_err());
    }
    // Header/index remain valid: the first record key checksum is damaged.
    let mut damaged = saved.clone();
    damaged[64 + 14] ^= 1;
    fs::write(&path, &damaged).unwrap();
    let run = Run::open(&path).unwrap();
    assert!(run.get("a", b"", 9).is_err());
    assert!(run.entries().next().unwrap().is_err());
    // The old value is the third record for the empty key, after two empty
    // payloads. Its bytes must be checked even after an index-only reopen.
    let mut damaged = saved.clone();
    damaged[64 + 2 * (18 + 1 + 4) + 18 + 1] ^= 1;
    fs::write(&path, &damaged).unwrap();
    let run = Run::open(&path).unwrap();
    assert!(run.get("a", b"", 0).is_err());
    let mut cursor = run.entries();
    assert!(cursor.next().unwrap().is_ok());
    assert!(cursor.next().unwrap().is_ok());
    assert!(cursor.next().unwrap().is_err());
    assert!(cursor.next().is_none());
}

use mmx_db::{
    journal::{Action, Change, Journal, Reader, ValueRef},
    sorted::{merge, Builder, Entry, View},
    Write,
};

#[test]
fn compaction_moves_only_references_and_preserves_empty_values_and_tombstones() {
    let directory = tempfile::tempdir().unwrap();
    let log_path = directory.path().join("0.log");
    let mut journal = Journal::open(&log_path, 7).unwrap();
    let old = vec![1; 1024 * 1024];
    let new = vec![2; 1024 * 1024];
    let indexes = journal
        .append(
            Action {
                generation: 1,
                expected: 0,
                cut: 0,
                next: 4,
                floor: 0,
            },
            &[
                Change {
                    version: 3,
                    table: "values",
                    key: b"key",
                    value: Some(&new),
                },
                Change {
                    version: 2,
                    table: "values",
                    key: b"key",
                    value: None,
                },
                Change {
                    version: 1,
                    table: "values",
                    key: b"key",
                    value: Some(b""),
                },
                Change {
                    version: 0,
                    table: "values",
                    key: b"key",
                    value: Some(&old),
                },
            ],
        )
        .unwrap();
    let log_size = journal.end();
    assert!((2 * 1024 * 1024..2 * 1024 * 1024 + 1024).contains(&log_size));
    let mut builder = Builder::new(directory.path(), 0).unwrap();
    for index in indexes {
        builder
            .push(&Entry {
                version: index.version,
                write: Write {
                    table: index.table,
                    key: index.key,
                    value: index.value.map(|value| value.encode().to_vec()),
                },
            })
            .unwrap();
    }
    let run_path = directory.path().join("0.sst");
    let run = builder.finish(&run_path).unwrap();
    assert!(std::fs::metadata(&run_path).unwrap().len() < 1024);
    let merged_path = directory.path().join("1.sst");
    let merged = merge(
        &merged_path,
        1,
        &[View {
            run: &run,
            cutoff: u32::MAX,
        }],
        Some(2),
    )
    .unwrap()
    .unwrap();
    assert_eq!(merged.metadata().records, 3);
    assert!(std::fs::metadata(&merged_path).unwrap().len() < 1024);
    assert_eq!(
        journal.end(),
        log_size,
        "compaction rewrote original value payloads"
    );
    drop(journal);
    let reader = Reader::open(&log_path, 7).unwrap();
    for (height, expected) in [
        (1, Some(b"".as_slice())),
        (2, None),
        (3, Some(new.as_slice())),
    ] {
        let entry = merged.get("values", b"key", height).unwrap().unwrap();
        let value = entry
            .write
            .value
            .map(|encoded| reader.value(ValueRef::decode(&encoded).unwrap()).unwrap());
        assert_eq!(value.as_deref(), expected);
    }
    // The prior immutable view still owns its old reference after compaction.
    let prior = run
        .get("values", b"key", 0)
        .unwrap()
        .unwrap()
        .write
        .value
        .unwrap();
    assert_eq!(
        reader.value(ValueRef::decode(&prior).unwrap()).unwrap(),
        old
    );
}

//! Behavioral ports of test/test_database.cpp plus durability / atomicity tests.
use mmx_db::{Database, Error, Write};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};

fn exercise(db: &mut Database) {
    for i in 0u32..100 {
        db.insert("table", &(i * 2).to_be_bytes(), &(i as u64).to_be_bytes())
            .unwrap();
    }
    db.commit(1).unwrap();
    for i in (0u32..200).rev() {
        db.insert(
            "table",
            &(i * 2).to_be_bytes(),
            &(i as u64 + 1).to_be_bytes(),
        )
        .unwrap();
    }
    db.commit(2).unwrap();
    for i in 0u32..200 {
        assert_eq!(
            db.get("table", &(i * 2).to_be_bytes(), None),
            Some((i as u64 + 1).to_be_bytes().as_slice())
        );
        assert_eq!(
            db.get("table", &(i * 2).to_be_bytes(), Some(0)),
            (i < 100).then_some((i as u64).to_be_bytes().as_slice())
        );
    }
    let rows = db.scan("table", &[], None);
    assert_eq!(rows.len(), 200);
    assert!(rows.windows(2).all(|w| w[0].key < w[1].key));
    db.revert(1).unwrap();
    assert_eq!(db.scan("table", &[], None).len(), 100);
    for i in 0u32..100 {
        assert_eq!(
            db.get("table", &(i * 2).to_be_bytes(), None),
            Some((i as u64).to_be_bytes().as_slice())
        );
    }
}

#[test]
fn original_table_version_and_revert_cases() {
    exercise(&mut Database::memory());
}

#[test]
fn original_table_cases_survive_reopen() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut db = Database::open(path.path()).unwrap();
        exercise(&mut db);
    }
    let db = Database::open(path.path()).unwrap();
    assert_eq!(db.version(), 1);
    assert_eq!(db.scan("table", &[], None).len(), 100);
}

#[test]
fn pending_writes_and_duplicate_keys() {
    let mut db = Database::memory();
    db.insert("t", b"key", b"first").unwrap();
    db.insert("t", b"key", b"last").unwrap();
    assert_eq!(db.get("t", b"key", Some(0)), Some(b"last".as_slice()));
    db.commit(10).unwrap();
    db.insert("t", b"key", b"pending").unwrap();
    assert_eq!(db.get("t", b"key", Some(9)), Some(b"last".as_slice()));
    assert_eq!(db.get("t", b"key", Some(10)), Some(b"pending".as_slice()));
    db.revert(10).unwrap();
    assert_eq!(db.get("t", b"key", None), Some(b"last".as_slice()));
}

#[test]
fn delete_empty_value_and_historical_scan() {
    let mut db = Database::memory();
    db.insert("t", b"a", b"").unwrap();
    db.insert("t", b"ab", b"two").unwrap();
    db.commit(1).unwrap();
    db.remove("t", b"a").unwrap();
    db.insert("t", b"ac", b"three").unwrap();
    db.commit(2).unwrap();
    assert_eq!(db.get("t", b"a", None), None);
    assert_eq!(db.get("t", b"a", Some(0)), Some(b"".as_slice()));
    assert_eq!(
        db.scan("t", b"a", Some(0))
            .iter()
            .map(|r| r.key.as_slice())
            .collect::<Vec<_>>(),
        [b"a".as_slice(), b"ab".as_slice()]
    );
    db.revert(1).unwrap();
    assert_eq!(db.get("t", b"a", None), Some(b"".as_slice()));
    assert_eq!(db.get("t", b"ac", None), None);
}

#[test]
fn original_size_first_comparator() {
    let mut db = Database::memory();
    for key in [b"aa".as_slice(), b"z", b"a", b"aaa", b""] {
        db.insert("t", key, key).unwrap();
    }
    let keys: Vec<_> = db.scan("t", b"", None).into_iter().map(|r| r.key).collect();
    assert_eq!(
        keys,
        [
            b"".to_vec(),
            b"a".to_vec(),
            b"z".to_vec(),
            b"aa".to_vec(),
            b"aaa".to_vec()
        ]
    );
}

#[test]
fn batches_validate_before_mutation() {
    let mut db = Database::memory();
    assert!(db
        .apply([
            Write::put("valid", b"a", b"b"),
            Write::put("../invalid", b"c", b"d")
        ])
        .is_err());
    assert!(!db.has_pending());
}

#[test]
fn version_validation_and_empty_commits() {
    let mut db = Database::memory();
    for next in [0, u32::MAX] {
        assert!(matches!(db.commit(next), Err(Error::Version { .. })));
    }
    assert!(db.revert(1).is_err());
    db.commit(5).unwrap();
    assert!(db.commit(5).is_err());
    db.revert(0).unwrap();
    assert_eq!(db.version(), 0);
}

#[test]
fn commits_are_atomic_across_tables_and_pending_is_not_durable() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut db = Database::open(path.path()).unwrap();
        db.apply([Write::put("a", b"k", b"one"), Write::put("b", b"k", b"two")])
            .unwrap();
        db.commit(1).unwrap();
        db.insert("a", b"k", b"not committed").unwrap();
    }
    let db = Database::open(path.path()).unwrap();
    assert_eq!(db.get("a", b"k", None), Some(b"one".as_slice()));
    assert_eq!(db.get("b", b"k", None), Some(b"two".as_slice()));
}

#[test]
fn only_one_writer_can_open_a_database() {
    let path = tempfile::tempdir().unwrap();
    let db = Database::open(path.path()).unwrap();
    assert!(Database::open(path.path()).is_err());
    drop(db);
    assert!(Database::open(path.path()).is_ok());
}

#[test]
fn every_truncated_final_frame_recovers_the_previous_commit() {
    let path = tempfile::tempdir().unwrap();
    let journal = path.path().join("journal.mmx");
    let mut db = Database::open(path.path()).unwrap();
    db.insert("a", b"key", b"old").unwrap();
    db.commit(1).unwrap();
    let previous_len = std::fs::metadata(&journal).unwrap().len() as usize;
    db.apply([
        Write::put("a", b"key", b"new"),
        Write::put("b", b"key", b"new"),
    ])
    .unwrap();
    db.commit(2).unwrap();
    drop(db);
    let bytes = std::fs::read(&journal).unwrap();
    for end in previous_len..bytes.len() {
        std::fs::write(&journal, &bytes[..end]).unwrap();
        let mut db = Database::open(path.path()).unwrap();
        assert_eq!(db.version(), 1, "cut at {end}");
        assert_eq!(db.get("a", b"key", None), Some(b"old".as_slice()));
        assert_eq!(db.get("b", b"key", None), None);
        db.insert("a", b"key", b"after recovery").unwrap();
        db.commit(2).unwrap();
        drop(db);
        assert_eq!(
            Database::open(path.path()).unwrap().get("a", b"key", None),
            Some(b"after recovery".as_slice())
        );
    }
}

#[test]
fn completed_frame_checksum_failure_is_reported() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut db = Database::open(path.path()).unwrap();
        db.insert("a", b"k", b"value").unwrap();
        db.commit(1).unwrap();
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.path().join("journal.mmx"))
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    assert!(matches!(
        Database::open(path.path()),
        Err(Error::Corrupt("journal checksum mismatch"))
    ));
}

#[test]
fn rollback_and_fork_are_durable() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut db = Database::open(path.path()).unwrap();
        db.insert("t", b"k", b"zero").unwrap();
        db.commit(1).unwrap();
        db.insert("t", b"k", b"old fork").unwrap();
        db.commit(2).unwrap();
        db.revert(1).unwrap();
        db.insert("t", b"k", b"new fork").unwrap();
        db.commit(3).unwrap();
    }
    let db = Database::open(path.path()).unwrap();
    assert_eq!(db.get("t", b"k", Some(0)), Some(b"zero".as_slice()));
    assert_eq!(db.get("t", b"k", Some(1)), Some(b"new fork".as_slice()));
    assert_eq!(db.version(), 3);
}

#[test]
fn nested_new_database_directory_can_be_reopened() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one/two/three");
    let mut db = Database::open(&path).unwrap();
    db.insert("t", b"k", b"v").unwrap();
    db.commit(1).unwrap();
    drop(db);
    assert_eq!(
        Database::open(path).unwrap().get("t", b"k", None),
        Some(b"v".as_slice())
    );
}

#[test]
fn corrupted_record_length_is_not_mistaken_for_a_torn_tail() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut db = Database::open(path.path()).unwrap();
        db.insert("t", b"k", b"v").unwrap();
        db.commit(1).unwrap();
    }
    let journal = path.path().join("journal.mmx");
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes[8] ^= 0x40;
    std::fs::write(&journal, bytes).unwrap();
    assert!(matches!(
        Database::open(path.path()),
        Err(Error::Corrupt("journal frame length guard"))
    ));
}

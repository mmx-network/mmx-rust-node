//! Behavioral ports of test/test_database.cpp plus durability / atomicity tests.
use mmx_db::{Database, Error, Write};

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
fn range_snapshots_preserve_size_first_order_pending_tombstones_and_history() {
    let mut db = Database::memory();
    for key in [b"z".as_slice(), b"aa", b"az", b"bba"] {
        db.insert("t", key, key).unwrap();
    }
    db.commit(1).unwrap();
    db.remove("t", b"az").unwrap();
    db.insert("t", b"ab", b"new").unwrap();
    let keys = |rows: Vec<mmx_db::Row>| rows.into_iter().map(|r| r.key).collect::<Vec<_>>();
    assert_eq!(
        keys(db.scan_range("t", b"aa", Some(b"bba"), None)),
        vec![b"aa".to_vec(), b"ab".to_vec()]
    );
    assert_eq!(
        keys(db.scan_range("t", b"aa", Some(b"bba"), Some(0))),
        vec![b"aa".to_vec(), b"az".to_vec()]
    );
    assert_eq!(
        keys(db.scan_range("t", b"z", Some(b"aa"), None)),
        vec![b"z".to_vec()]
    );
    assert!(db.scan_range("t", b"aa", Some(b"z"), None).is_empty());
}

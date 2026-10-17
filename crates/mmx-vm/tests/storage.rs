//! StorageDB / storage_tests.cpp behavior and new atomic-batch checks.
use mmx_vm::{
    storage::{self, StorageWrite},
    value::{FLAG_CONST, FLAG_KEY},
    Storage, StorageDb, Value, Var, MEM_STATIC,
};

fn write(address: u64, key: Option<u64>, value: impl Into<Value>) -> StorageWrite {
    StorageWrite {
        address,
        key,
        var: Var::new(value),
    }
}

#[test]
fn scalar_entries_key_lookup_and_isolation() {
    let mut db = StorageDb::memory();
    let contract = [1; 32];
    let key = Value::string("key");
    let mut key_write = write(MEM_STATIC + 1, None, key.clone());
    key_write.var.flags = FLAG_CONST | FLAG_KEY;
    db.apply(
        &contract,
        &[
            write(MEM_STATIC, None, 1337u64),
            key_write,
            write(MEM_STATIC + 2, Some(MEM_STATIC + 1), Value::string("value")),
        ],
    )
    .unwrap();
    assert_eq!(
        db.read(&contract, MEM_STATIC, None).unwrap().unwrap().value,
        Value::from(1337u64)
    );
    assert_eq!(db.lookup(&contract, &key).unwrap(), Some(MEM_STATIC + 1));
    assert_eq!(
        db.read(&contract, MEM_STATIC + 2, Some(MEM_STATIC + 1))
            .unwrap()
            .unwrap()
            .value,
        Value::string("value")
    );
    assert!(db.read(&[2; 32], MEM_STATIC, None).unwrap().is_none());
    assert_eq!(db.lookup(&[2; 32], &key).unwrap(), None);
    assert_eq!(
        db.find_range(&contract, MEM_STATIC, MEM_STATIC + 2, None)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn original_key_layouts_are_preserved() {
    let contract = [0xAB; 32];
    let (table, key) = storage::data_key(&contract, 0x123456789ABCDEF0, Some(0xFEDCBA9876543210));
    assert_eq!(table, "storage_entries");
    assert_eq!(key.len(), 36);
    assert_eq!(&key[..20], &[0xAB; 20]);
    assert_eq!(&key[20..28], &0x123456789ABCDEF0u64.to_be_bytes());
    assert_eq!(&key[28..], &0xFEDCBA9876543210u64.to_be_bytes());
}

#[test]
fn entries_remain_sorted_and_can_be_read_historically() {
    let mut db = StorageDb::memory();
    for key in [256, 1, 65536, 255] {
        db.apply(&[0; 32], &[write(MEM_STATIC, Some(key), key)])
            .unwrap();
    }
    db.database_mut().commit(1).unwrap();
    db.apply(&[0; 32], &[write(MEM_STATIC, Some(1), 100u64)])
        .unwrap();
    db.database_mut().commit(2).unwrap();
    let entries = db.entries(&[0; 32], MEM_STATIC, Some(0)).unwrap();
    assert_eq!(
        entries.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        [1, 255, 256, 65536]
    );
    assert_eq!(entries[0].1.value, Value::from(1u64));
    assert_eq!(
        db.read_at(&[0; 32], MEM_STATIC, Some(1), None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(100u64)
    );
}

#[test]
fn invalid_storage_batch_has_no_partial_writes() {
    let mut db = StorageDb::memory();
    assert!(db
        .apply(
            &[0; 32],
            &[
                write(MEM_STATIC, None, 1u64),
                write(MEM_STATIC + 1, None, Value::Ref(1))
            ]
        )
        .is_err());
    assert!(!db.database().has_pending());
    let mut bad_key = write(MEM_STATIC + 1, None, Value::string("key"));
    bad_key.var.flags = FLAG_KEY;
    assert!(db
        .apply(&[0; 32], &[write(MEM_STATIC, None, 1u64), bad_key])
        .is_err());
    assert!(!db.database().has_pending());
}

#[test]
fn storage_round_trips_all_value_types_on_disk() {
    let path = tempfile::tempdir().unwrap();
    let values = [
        Value::Nil,
        Value::Bool(false),
        Value::Bool(true),
        Value::Ref(MEM_STATIC),
        Value::Uint(mmx_vm::U256::MAX),
        Value::string("hello"),
        Value::Binary(vec![0, 255]),
        Value::Array {
            address: MEM_STATIC,
            size: 2,
        },
        Value::Map {
            address: MEM_STATIC,
        },
    ];
    {
        let mut db = StorageDb::open(path.path()).unwrap();
        for (i, value) in values.iter().enumerate() {
            db.apply(
                &[0; 32],
                &[write(MEM_STATIC + i as u64, None, value.clone())],
            )
            .unwrap();
        }
        db.database_mut().commit(1).unwrap();
    }
    let db = StorageDb::open(path.path()).unwrap();
    for (i, value) in values.iter().enumerate() {
        assert_eq!(
            &db.read(&[0; 32], MEM_STATIC + i as u64, None)
                .unwrap()
                .unwrap()
                .value,
            value
        );
    }
}

#[test]
fn balance_snapshots_survive_reopen_and_rollback_with_full_address_isolation() {
    let directory = tempfile::tempdir().unwrap();
    let contract = [1; 32];
    let mut other = contract;
    other[31] = 2;
    let currency = [3; 32];
    {
        let mut db = StorageDb::open(directory.path()).unwrap();
        assert_eq!(db.get_balance(&contract, &currency).unwrap(), None);
        db.set_balance(&contract, &currency, u128::MAX).unwrap();
        db.set_balance(&other, &currency, 7).unwrap();
        db.database_mut().commit(1).unwrap();
        db.set_balance(&contract, &currency, 12).unwrap();
        db.database_mut().commit(2).unwrap();
    }
    {
        let mut db = StorageDb::open(directory.path()).unwrap();
        assert_eq!(
            db.balance_at(&contract, &currency, Some(0)).unwrap(),
            Some(u128::MAX)
        );
        assert_eq!(db.get_balance(&contract, &currency).unwrap(), Some(12));
        assert_eq!(db.get_balance(&other, &currency).unwrap(), Some(7));
        db.database_mut().revert(1).unwrap();
    }
    let db = StorageDb::open(directory.path()).unwrap();
    assert_eq!(
        db.get_balance(&contract, &currency).unwrap(),
        Some(u128::MAX)
    );
}

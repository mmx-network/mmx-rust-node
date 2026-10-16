//! Behavioral ports of the heap/array/map cases in vm/storage_tests.cpp.
use mmx_vm::{
    engine::Error,
    instruction::{HARD_FAIL, REF_A, REF_B},
    Engine, Instruction as I, Opcode as O, StorageDb, Value, MEM_HEAP, MEM_STACK as T,
    MEM_STATIC as S,
};

fn array() -> Value {
    Value::Array {
        address: 0,
        size: 0,
    }
}
fn map() -> Value {
    Value::Map { address: 0 }
}
fn new(storage: &mut StorageDb, read_only: bool) -> Engine<'_, StorageDb> {
    let mut engine = Engine::new([0; 32], storage, read_only, 1, 1_000_000);
    engine.init().unwrap();
    engine
}
fn op(code: O, flags: u8, a: u64, b: u64, c: u64) -> I {
    I::new(code, [a as u32, b as u32, c as u32, 0]).with_flags(flags)
}
fn ret() -> I {
    I::new(O::Ret, [0; 4])
}

#[test]
fn original_stack_is_not_persisted() {
    let mut storage = StorageDb::memory();
    {
        let mut vm = new(&mut storage, false);
        vm.write(T, 1337u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    assert_eq!(new(&mut storage, true).read(T).unwrap(), None);
}

#[test]
fn original_heap_and_pinned_stored_objects() {
    let mut storage = StorageDb::memory();
    let address;
    {
        let mut vm = new(&mut storage, false);
        address = vm.alloc().unwrap();
        vm.write(address, 1337u64).unwrap();
        vm.write(S, Value::Ref(address)).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = new(&mut storage, true);
        assert_eq!(vm.read(S).unwrap(), Some(Value::Ref(address)));
        assert_eq!(vm.read(address).unwrap(), Some(Value::from(1337u64)));
    }
    {
        let mut vm = new(&mut storage, false);
        vm.erase(S).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(2).unwrap();
    let mut vm = new(&mut storage, true);
    assert_eq!(vm.read(S).unwrap(), None);
    assert_eq!(vm.read(address).unwrap(), Some(Value::from(1337u64)));
}

#[test]
fn original_heap_erase_and_recursive_erase() {
    let mut storage = StorageDb::memory();
    let (a, b);
    {
        let mut vm = new(&mut storage, false);
        a = vm.alloc().unwrap();
        b = vm.alloc().unwrap();
        vm.write(a, map()).unwrap();
        vm.write(b, map()).unwrap();
        vm.write(S, Value::Ref(a)).unwrap();
        vm.write_key(a, &Value::from(1337u64), Value::Ref(b))
            .unwrap();
        vm.write_key(b, &Value::from(1337u64), 1337u64).unwrap();
        vm.erase(S).unwrap();
        for address in [S, a, b] {
            assert_eq!(vm.read(address).unwrap(), None);
        }
        assert_eq!(vm.read_key(b, &Value::from(1337u64)).unwrap(), None);
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    let mut vm = new(&mut storage, true);
    for address in [S, a, b] {
        assert_eq!(vm.read(address).unwrap(), None);
    }
}

#[test]
fn references_alias_and_reclaim_only_on_the_last_unref() {
    let mut storage = StorageDb::memory();
    let mut vm = new(&mut storage, false);
    let a = vm.alloc().unwrap();
    let b = vm.alloc().unwrap();
    vm.write(a, 1u64).unwrap();
    vm.write(b, 2u64).unwrap();
    vm.write(S, Value::Ref(a)).unwrap();
    vm.copy(S + 1, S).unwrap();
    vm.write(S, Value::Ref(b)).unwrap();
    assert_eq!(vm.read(a).unwrap(), Some(Value::from(1u64)));
    vm.erase(S + 1).unwrap();
    assert_eq!(vm.read(a).unwrap(), None);
    vm.erase(S).unwrap();
    assert_eq!(vm.read(b).unwrap(), None);
}

#[test]
fn original_array_push_copy_pop_and_reopen() {
    let path = tempfile::tempdir().unwrap();
    {
        let mut storage = StorageDb::open(path.path()).unwrap();
        {
            let mut vm = new(&mut storage, false);
            vm.write(S, array()).unwrap();
            vm.push_back(S, 1u64).unwrap();
            vm.commit().unwrap();
        }
        storage.database_mut().commit(1).unwrap();
        {
            let mut vm = new(&mut storage, false);
            vm.push_back(S, Value::string("test")).unwrap();
            vm.commit().unwrap();
        }
        storage.database_mut().commit(2).unwrap();
    }
    let mut storage = StorageDb::open(path.path()).unwrap();
    {
        let mut vm = new(&mut storage, true);
        vm.copy(T, S).unwrap();
        assert!(matches!(
            vm.read(T).unwrap(),
            Some(Value::Array {
                address: T,
                size: 2
            })
        ));
        assert_eq!(vm.read_entry(T, 0).unwrap(), Some(Value::from(1u64)));
        assert_eq!(vm.read_entry(T, 1).unwrap(), Some(Value::string("test")));
    }
    {
        let mut vm = new(&mut storage, false);
        vm.pop_back(T, S).unwrap();
        assert_eq!(vm.read(T).unwrap(), Some(Value::string("test")));
        vm.commit().unwrap();
    }
    storage.database_mut().commit(3).unwrap();
    let mut vm = new(&mut storage, true);
    assert!(matches!(
        vm.read(S).unwrap(),
        Some(Value::Array {
            address: S,
            size: 1
        })
    ));
    assert_eq!(vm.read_entry(S, 0).unwrap(), Some(Value::from(1u64)));
    assert_eq!(vm.read_entry(S, 1).unwrap(), None);
}

#[test]
fn original_map_copy_update_delete_reinsert_and_nil() {
    let mut storage = StorageDb::memory();
    let one = Value::from(1u64);
    let address_key = Value::Binary(vec![0; 32]);
    {
        let mut vm = new(&mut storage, false);
        vm.write(S, map()).unwrap();
        vm.write_key(S, &one, true).unwrap();
        vm.write_key(S, &address_key, address_key.clone()).unwrap();
        vm.copy(T, S).unwrap();
        assert_eq!(vm.read_key(T, &one).unwrap(), Some(Value::Bool(true)));
        assert_eq!(
            vm.read_key(T, &address_key).unwrap(),
            Some(address_key.clone())
        );
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = new(&mut storage, false);
        assert_eq!(vm.read_key(S, &one).unwrap(), Some(Value::Bool(true)));
        vm.write_key(S, &one, 2u64).unwrap();
        vm.erase_key(S, &address_key).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(2).unwrap();
    {
        let mut vm = new(&mut storage, false);
        assert_eq!(vm.read_key(S, &one).unwrap(), Some(Value::from(2u64)));
        assert_eq!(vm.read_key(S, &address_key).unwrap(), None);
        vm.erase_key(S, &one).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(3).unwrap();
    {
        let mut vm = new(&mut storage, false);
        assert_eq!(vm.read_key(S, &one).unwrap(), None);
        vm.write_key(S, &one, 3u64).unwrap();
        vm.write_key(S, &address_key, Value::Nil).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(4).unwrap();
    let mut vm = new(&mut storage, true);
    assert_eq!(vm.read_key(S, &one).unwrap(), Some(Value::from(3u64)));
    assert_eq!(vm.read_key(S, &address_key).unwrap(), Some(Value::Nil));
}

#[test]
fn original_lookup_and_delete_write_cases() {
    let mut storage = StorageDb::memory();
    let key = Value::string("test");
    let address;
    {
        let mut vm = new(&mut storage, false);
        address = vm.lookup(&key, false).unwrap().unwrap();
        vm.write(S, map()).unwrap();
        vm.write_key(S, &key, 1337u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = new(&mut storage, false);
        assert_eq!(vm.lookup(&key, true).unwrap(), Some(address));
        assert_eq!(vm.read_key(S, &key).unwrap(), Some(Value::from(1337u64)));
        vm.erase_key(S, &key).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(2).unwrap();
    {
        let mut vm = new(&mut storage, false);
        assert_eq!(vm.read_key(S, &key).unwrap(), None);
        vm.write_key(S, &key, 111u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(3).unwrap();
    assert_eq!(
        new(&mut storage, true).read_key(S, &key).unwrap(),
        Some(Value::from(111u64))
    );
}

#[test]
fn nested_collections_survive_reopen_and_rollback() {
    let path = tempfile::tempdir().unwrap();
    let nested;
    {
        let mut storage = StorageDb::open(path.path()).unwrap();
        {
            let mut vm = new(&mut storage, false);
            nested = vm.alloc().unwrap();
            vm.write(nested, array()).unwrap();
            vm.push_back(nested, 7u64).unwrap();
            vm.write(S, map()).unwrap();
            vm.write_key(S, &Value::string("nested"), Value::Ref(nested))
                .unwrap();
            vm.commit().unwrap();
        }
        storage.database_mut().commit(1).unwrap();
        {
            let mut vm = new(&mut storage, false);
            vm.push_back(nested, 8u64).unwrap();
            vm.commit().unwrap();
        }
        storage.database_mut().commit(2).unwrap();
    }
    let mut storage = StorageDb::open(path.path()).unwrap();
    storage.database_mut().revert(1).unwrap();
    let mut vm = new(&mut storage, true);
    assert_eq!(
        vm.read_key(S, &Value::string("nested")).unwrap(),
        Some(Value::Ref(nested))
    );
    assert!(matches!(
        vm.read(nested).unwrap(),
        Some(Value::Array { size: 1, .. })
    ));
    assert_eq!(vm.read_entry(nested, 0).unwrap(), Some(Value::from(7u64)));
}

#[test]
fn collection_opcodes_and_reference_flags() {
    let mut storage = StorageDb::memory();
    let mut vm = Engine::new([0; 32], &mut storage, false, 1, 1_000_000);
    vm.write(1, array()).unwrap();
    vm.write(2, 7u64).unwrap();
    vm.write(3, 0u64).unwrap();
    vm.init().unwrap();
    vm.run(
        &[
            op(O::Clone, 0, T + 1, 1, 0),
            op(O::PushBack, REF_A, T + 1, 2, 0),
            op(O::Get, REF_B, T, T + 1, 3),
            op(O::Set, REF_A, T + 1, 3, T),
            op(O::Size, REF_B, S, T + 1, 0),
            op(O::Concat, REF_B | 4, T + 2, T + 1, T + 1),
            op(O::PopBack, REF_B, T, T + 2, 0),
            ret(),
        ],
        0,
    )
    .unwrap();
    assert_eq!(vm.read(T).unwrap(), Some(Value::from(7u64)));
    assert_eq!(vm.read(S).unwrap(), Some(Value::from(1u64)));
    // RET clears both references; their new heap arrays are reclaimed.
    assert_eq!(vm.read(MEM_HEAP + 0x1000).unwrap(), None);
    vm.commit().unwrap();
}

#[test]
fn collection_failures_prevent_commit() {
    for (code, flags) in [(O::Get, HARD_FAIL), (O::Set, 0)] {
        let mut storage = StorageDb::memory();
        let mut vm = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        vm.write(1, array()).unwrap();
        vm.write(2, 0u64).unwrap();
        vm.init().unwrap();
        let program = if code == O::Get {
            op(code, flags, T, 1, 2)
        } else {
            op(code, flags, 1, 2, 2)
        };
        assert!(vm.run(&[program, ret()], 0).is_err());
        assert!(vm.commit().is_err());
        assert!(!storage.database().has_pending());
    }
    let mut storage = StorageDb::memory();
    let mut vm = new(&mut storage, false);
    assert!(matches!(
        vm.lookup(&Value::Nil, false),
        Err(Error::InvalidKey)
    ));
}

#[test]
fn entry_writes_do_not_load_an_old_value_before_overwriting_it() {
    let mut storage = StorageDb::memory();
    {
        let mut vm = new(&mut storage, false);
        vm.write(S, map()).unwrap();
        vm.write_key(S, &Value::from(1u64), 1u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    let mut vm = new(&mut storage, false);
    vm.read(S).unwrap();
    let key = vm.lookup(&Value::from(1u64), true).unwrap().unwrap();
    let gas = vm.gas_used;
    vm.write_entry(S, key, 2u64).unwrap();
    // The new uint entry incurs allocation cost, not a storage-read charge.
    assert_eq!(vm.gas_used - gas, 80);
    assert_eq!(vm.read_entry(S, key).unwrap(), Some(Value::from(2u64)));
}

#[test]
fn value_size_limit_and_failed_update_are_atomic() {
    let mut storage = StorageDb::memory();
    let mut vm = new(&mut storage, false);
    vm.write(S, Value::Binary(vec![0; mmx_vm::MAX_VALUE_BYTES]))
        .unwrap();
    assert!(matches!(
        vm.write(S, Value::Binary(vec![0; mmx_vm::MAX_VALUE_BYTES + 1])),
        Err(Error::ValueSize)
    ));
    assert!(vm.commit().is_err());
    assert!(!storage.database().has_pending());
}

#[test]
fn clone_preserves_aliasing_of_nested_references() {
    let mut storage = StorageDb::memory();
    let mut vm = new(&mut storage, false);
    let child = vm.alloc().unwrap();
    vm.write(child, 1u64).unwrap();
    vm.write(S, array()).unwrap();
    vm.push_back(S, Value::Ref(child)).unwrap();
    vm.clone_value(T, S).unwrap();
    let cloned = match vm.read(T).unwrap().unwrap() {
        Value::Ref(address) => address,
        _ => panic!(),
    };
    assert_eq!(vm.read_entry(cloned, 0).unwrap(), Some(Value::Ref(child)));
    vm.erase(S).unwrap();
    assert_eq!(vm.read(child).unwrap(), Some(Value::from(1u64)));
    vm.erase(T).unwrap();
    assert_eq!(vm.read(child).unwrap(), None);
}

#[test]
fn failed_composite_commit_preserves_the_preexisting_state() {
    let mut storage = StorageDb::memory();
    let key = Value::from(1u64);
    {
        let mut vm = new(&mut storage, false);
        vm.write(S, map()).unwrap();
        vm.write_key(S, &key, 1u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = new(&mut storage, false);
        vm.write_key(S, &key, 2u64).unwrap();
        let child = vm.alloc().unwrap();
        vm.write(child, array()).unwrap();
        vm.push_back(child, 3u64).unwrap();
        vm.write(S + 1, Value::Ref(child)).unwrap();
        vm.gas_limit = vm.gas_used + 2400; // enough for the root reference, not the counter and entries
        assert!(matches!(vm.commit(), Err(Error::OutOfGas { .. })));
        assert!(vm.commit().is_err());
    }
    assert!(!storage.database().has_pending());
    let mut vm = new(&mut storage, true);
    assert_eq!(vm.read_key(S, &key).unwrap(), Some(Value::from(1u64)));
    assert_eq!(vm.read(S + 1).unwrap(), None);
}

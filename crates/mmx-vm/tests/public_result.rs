use mmx_vm::{Engine, InputValue, StorageDb, Value, MEM_STACK, MEM_STATIC};

#[test]
fn public_result_preserves_boundary_types_and_bounds_nested_values() {
    let mut storage = StorageDb::memory();
    {
        let mut vm = Engine::new([0; 32], &mut storage, false, 1, 1_000_000);
        vm.init().unwrap();
        let values = InputValue::Array(vec![
            InputValue::Binary(vec![1, 2]),
            InputValue::String(b"1234".to_vec()),
            InputValue::Map(vec![(
                InputValue::String(b"n".to_vec()),
                InputValue::Uint(42.into()),
            )]),
        ]);
        vm.assign_input(MEM_STACK, &values).unwrap();
        assert_eq!(vm.read_result(MEM_STACK, 10).unwrap(), values);
        assert!(vm.read_result(MEM_STACK, 3).is_err());
        // A cyclic reference must terminate without allocating unbounded output.
        let at = vm.alloc().unwrap();
        vm.write(at, Value::Ref(at)).unwrap();
        assert!(vm.read_result(at, 1000).is_err());
        vm.write(MEM_STATIC, Value::Map { address: 0 }).unwrap();
        vm.write_key(MEM_STATIC, &Value::from(1u64), 7u64).unwrap();
        vm.commit().unwrap();
    }
    storage.database_mut().commit(1).unwrap();
    let mut vm = Engine::new([0; 32], &mut storage, true, 1, 1_000_000);
    vm.init().unwrap();
    assert!(
        vm.read_result(MEM_STATIC, 100).is_err(),
        "stored maps need a bounded storage query"
    );
}

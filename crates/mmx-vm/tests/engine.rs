use mmx_vm::{
    engine::Error,
    instruction::{CATCH_OVERFLOW, REF_A},
    storage::StorageWrite,
    Engine, Instruction as I, Opcode as O, Storage, StorageDb, Value, Var, MEM_EXTERN, MEM_STACK,
    MEM_STATIC, U256,
};

fn instr(code: O, a: u64, b: u64, c: u64) -> I {
    I::new(code, [a as u32, b as u32, c as u32, 0])
}
fn ret() -> I {
    I::new(O::Ret, [0; 4])
}

#[test]
fn original_ten_thousand_iteration_loop() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, true, 1, 10_000_000);
    for (address, value) in [(1, 0u64), (2, 1u64), (3, 10_000u64)] {
        engine.write(address, value).unwrap();
    }
    engine.init().unwrap();
    let code = [
        instr(O::Copy, MEM_STACK, 1, 0),
        instr(O::CmpGe, MEM_STACK + 1, MEM_STACK, 3),
        instr(O::JumpIf, 5, MEM_STACK + 1, 0),
        instr(O::Add, MEM_STACK, MEM_STACK, 2),
        instr(O::Jump, 1, 0, 0),
        ret(),
    ];
    engine.run(&code, 0).unwrap();
    assert_eq!(
        engine.read(MEM_STACK).unwrap(),
        Some(Value::from(10_000u64))
    );
    engine.commit().unwrap();
    assert!(!storage.database().has_pending());
}

#[test]
fn call_frames_use_relative_stack_addresses_and_keep_return_slot() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.write(1, 42u64).unwrap();
    engine.init().unwrap();
    let code = [
        instr(O::Call, 4, 8, 0),
        instr(O::Copy, MEM_STATIC, MEM_STACK + 8, 0),
        ret(),
        instr(O::Nop, 0, 0, 0),
        instr(O::Copy, MEM_STACK, 1, 0),
        instr(O::Copy, MEM_STACK + 1, 1, 0),
        ret(),
    ];
    engine.run(&code, 0).unwrap();
    assert_eq!(engine.read(MEM_STATIC).unwrap(), Some(Value::from(42u64)));
    assert_eq!(engine.read(MEM_STACK + 9).unwrap(), None);
    engine.commit().unwrap();
    assert_eq!(
        storage
            .read(&[0; 32], MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(42u64)
    );
    assert!(storage.read(&[0; 32], MEM_STACK, None).unwrap().is_none());
}

#[test]
fn indirect_jump_and_jump_not() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.write(1, 3u64).unwrap();
    engine.write(2, false).unwrap();
    engine.init().unwrap();
    let code = [
        instr(O::Jump, 1, 0, 0).with_flags(REF_A),
        instr(O::Fail, 0, 0, 0),
        ret(),
        instr(O::JumpNot, 2, 2, 0),
    ];
    engine.run(&code, 0).unwrap();
}

#[test]
fn constants_and_external_inputs_are_frozen_at_execution() {
    for address in [1, MEM_EXTERN] {
        let mut storage = StorageDb::memory();
        let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        engine.write(1, 1u64).unwrap();
        engine.write(2, 2u64).unwrap();
        engine.write(MEM_EXTERN, 0u64).unwrap();
        engine.init().unwrap();
        assert!(
            matches!(engine.run(&[instr(O::Copy, address, 2, 0), ret()], 0), Err(Error::ReadOnly(dst)) if dst == address)
        );
    }
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.write(1, 1u64).unwrap();
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Add, 1, 1, 1), ret()], 0),
        Err(Error::ReadOnly(1))
    ));
}

#[test]
fn failed_contract_cannot_publish_earlier_writes() {
    let mut storage = StorageDb::memory();
    {
        let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        engine.write(1, 99u64).unwrap();
        engine.write(2, Value::string("stop")).unwrap();
        engine.init().unwrap();
        let error = engine
            .run(
                &[instr(O::Copy, MEM_STATIC, 1, 0), instr(O::Fail, 2, 7, 0)],
                0,
            )
            .unwrap_err();
        assert!(matches!(error, Error::Fail { code: 7, message } if message == "stop"));
        assert_eq!(engine.error_code, 7);
        assert_eq!(engine.error_addr, Some(1));
        assert!(engine.commit().is_err());
    }
    assert!(!storage.database().has_pending());
}

#[test]
fn commit_gas_is_preflighted_for_the_entire_batch() {
    let mut storage = StorageDb::memory();
    {
        let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        engine.write(1, 99u64).unwrap();
        engine.init().unwrap();
        engine
            .run(
                &[
                    instr(O::Copy, MEM_STATIC, 1, 0),
                    instr(O::Copy, MEM_STATIC + 1, 1, 0),
                    ret(),
                ],
                0,
            )
            .unwrap();
        engine.gas_limit = engine.gas_used + 3600; // enough for exactly one uint storage write
        assert!(matches!(engine.commit(), Err(Error::OutOfGas { .. })));
        assert!(engine.commit().is_err());
    }
    assert!(!storage.database().has_pending());
}

#[test]
fn execution_gas_stops_endless_loop_without_publishing() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 200);
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Jump, 0, 0, 0)], 0),
        Err(Error::OutOfGas { .. })
    ));
    assert_eq!(engine.error_addr, Some(0));
    assert!(engine.commit().is_err());
    assert!(!storage.database().has_pending());
}

#[test]
fn read_only_storage_protects_loaded_cells_and_rejects_commit_of_new_cells() {
    let mut storage = StorageDb::memory();
    storage
        .apply(
            &[0; 32],
            &[StorageWrite {
                address: MEM_STATIC,
                key: None,
                var: Var::new(1u64),
            }],
        )
        .unwrap();
    storage.database_mut().commit(1).unwrap();
    for address in [MEM_STATIC, MEM_STATIC + 1] {
        let mut engine = Engine::new([0; 32], &mut storage, true, 1, 100_000);
        engine.write(1, 2u64).unwrap();
        engine.init().unwrap();
        let result = engine.run(&[instr(O::Copy, address, 1, 0), ret()], 0);
        if address == MEM_STATIC {
            assert!(matches!(result, Err(Error::ReadOnly(_))));
        } else {
            result.unwrap();
        }
        assert!(engine.commit().is_err());
    }
    assert!(!storage.database().has_pending());
}

#[test]
fn clear_loads_persistent_cell_and_retains_its_history() {
    let mut storage = StorageDb::memory();
    storage
        .apply(
            &[0; 32],
            &[StorageWrite {
                address: MEM_STATIC,
                key: None,
                var: Var::new(42u64),
            }],
        )
        .unwrap();
    storage.database_mut().commit(1).unwrap();
    {
        let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        engine.init().unwrap();
        engine
            .run(&[instr(O::Clr, MEM_STATIC, 0, 0), ret()], 0)
            .unwrap();
        assert_eq!(engine.read(MEM_STATIC).unwrap(), None);
        engine.commit().unwrap();
    }
    storage.database_mut().commit(2).unwrap();
    let mut engine = Engine::new([0; 32], &mut storage, true, 1, 100_000);
    assert_eq!(engine.read(MEM_STATIC).unwrap(), None);
    assert_eq!(
        storage
            .read_at(&[0; 32], MEM_STATIC, None, Some(0))
            .unwrap()
            .unwrap()
            .value,
        Value::from(42u64)
    );
}

#[test]
fn multiply_overflow_hardfork_regression_from_original_tests() {
    let missed = (U256::one() << 255) + 1;
    for (protocol, flags, fail) in [
        (0, CATCH_OVERFLOW, false),
        (1, CATCH_OVERFLOW, true),
        (1, 0, false),
    ] {
        let mut storage = StorageDb::memory();
        let mut engine = Engine::new([0; 32], &mut storage, false, protocol, 100_000);
        engine.write(1, missed).unwrap();
        engine.write(2, 2u64).unwrap();
        engine.init().unwrap();
        let result = engine.run(
            &[instr(O::Mul, MEM_STACK, 1, 2).with_flags(flags), ret()],
            0,
        );
        if fail {
            assert!(matches!(result, Err(Error::Overflow)));
        } else {
            result.unwrap();
            assert_eq!(engine.read(MEM_STACK).unwrap(), Some(Value::from(2u64)));
        }
    }
}

#[test]
fn binary_concat_slice_and_sha256_known_answer() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.write(1, Value::string("abc")).unwrap();
    engine.init().unwrap();
    engine
        .run(
            &[
                instr(O::Concat, MEM_STACK + 1, 1, 1),
                I::new(
                    O::Memcpy,
                    [(MEM_STACK + 2) as u32, (MEM_STACK + 1) as u32, 3, 3],
                ),
                instr(O::Sha256, MEM_STACK, MEM_STACK + 2, 0),
                ret(),
            ],
            0,
        )
        .unwrap();
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let bytes = match engine.read(MEM_STACK).unwrap().unwrap() {
        Value::Binary(bytes) => bytes,
        _ => panic!(),
    };
    assert_eq!(
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        expected
    );
}

#[test]
fn invalid_memory_type_and_instruction_fail_closed() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    assert!(engine.run(&[ret()], 0).is_err());
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Copy, MEM_STACK, 7, 0), ret()], 0),
        Err(Error::Missing(7))
    ));
    assert!(engine.commit().is_err());
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Send, 0, 0, 0)], 0),
        Err(Error::UnsupportedOpcode(O::Send))
    ));
    assert!(engine.commit().is_err());
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[ret()], 1),
        Err(Error::InstructionPointer(1))
    ));
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.write(MEM_STACK, Value::Ref(MEM_STATIC)).unwrap();
    assert_eq!(
        engine.read(MEM_STACK).unwrap(),
        Some(Value::Ref(MEM_STATIC))
    );
}

#[test]
fn persistent_counter_survives_reopen_and_revert() {
    let path = tempfile::tempdir().unwrap();
    for expected in 1u64..=3 {
        let mut storage = StorageDb::open(path.path()).unwrap();
        let next = storage.database().version() + 1;
        let is_new = storage.read(&[0; 32], MEM_STATIC, None).unwrap().is_none();
        {
            let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
            engine.write(1, 1u64).unwrap();
            if is_new {
                engine.write(MEM_STATIC, 0u64).unwrap();
            }
            engine.init().unwrap();
            engine
                .run(
                    &[
                        instr(O::Add, MEM_STATIC, MEM_STATIC, 1).with_flags(CATCH_OVERFLOW),
                        ret(),
                    ],
                    0,
                )
                .unwrap();
            assert_eq!(
                engine.read(MEM_STATIC).unwrap(),
                Some(Value::from(expected))
            );
            engine.commit().unwrap();
        }
        storage.database_mut().commit(next).unwrap();
    }
    let mut storage = StorageDb::open(path.path()).unwrap();
    storage.database_mut().revert(1).unwrap();
    assert_eq!(
        storage
            .read(&[0; 32], MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(1u64)
    );
}

#[test]
fn recursion_and_stack_limits_terminate_calls() {
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Call, 0, 0, 0)], 0),
        Err(Error::RecursionOverflow)
    ));
    assert!(engine.commit().is_err());
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    engine.init().unwrap();
    assert!(matches!(
        engine.run(&[instr(O::Call, 0, mmx_vm::STACK_SIZE, 0)], 0),
        Err(Error::StackOverflow)
    ));
    assert!(engine.commit().is_err());
}

#[test]
fn scalar_fail_messages_match_reference_formatting() {
    for (value, expected) in [
        (Value::Nil, "null"),
        (Value::Bool(false), "false"),
        (Value::from(42u64), "42"),
        (Value::Binary(vec![0, 255]), "0x00ff"),
    ] {
        let mut storage = StorageDb::memory();
        let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
        engine.write(1, value).unwrap();
        engine.init().unwrap();
        assert!(
            matches!(engine.run(&[instr(O::Fail, 1, 0, 0)], 0), Err(Error::Fail { message, .. }) if message == expected)
        );
    }
}

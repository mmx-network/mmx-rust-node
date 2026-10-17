use mmx_execution::{execute, Context, Contract, Invocation};
use mmx_types::mmx::contract::{method_t, Binary, Executable};
use mmx_vm::{
    instruction,
    storage::{AtomicStorage, TransactionBatch},
    Address, InputValue, Instruction, Opcode, Storage, StorageDb, Value, Var, MEM_STACK,
    MEM_STATIC,
};
fn address(id: u8) -> Address {
    let mut out = [0; 32];
    out[0] = id;
    out
}
fn op(code: Opcode, args: &[u64]) -> Instruction {
    let mut out = [0; 4];
    for (a, b) in out.iter_mut().zip(args) {
        *a = *b as u32;
    }
    Instruction::new(code, out)
}
fn fixture(
    storage: &mut StorageDb,
    root: Vec<Instruction>,
    child: Vec<Instruction>,
    child_const: bool,
) {
    let mut writes = TransactionBatch::default();
    for (id, code, is_const) in [(1, root, false), (2, child, child_const)] {
        let binary = Binary {
            constant: [
                Var::new(Value::Nil).encode(false, false).unwrap(),
                Var::new(1u64).encode(false, false).unwrap(),
                Var::new(Value::string("dep")).encode(false, false).unwrap(),
                Var::new(Value::string("run")).encode(false, false).unwrap(),
            ]
            .concat(),
            binary: instruction::encode(&code).unwrap(),
            methods: [(
                "run".to_owned(),
                method_t {
                    name: "run".to_owned(),
                    is_public: true,
                    is_const,
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let mut executable = Executable {
            binary: address(100 + id).into(),
            name: format!("contract{id}"),
            ..Default::default()
        };
        if id == 1 {
            executable
                .depends
                .insert("dep".to_owned(), address(2).into());
        }
        writes
            .contracts
            .push((address(id), Contract::encode(&executable, 10).unwrap()));
        writes
            .contracts
            .push((address(100 + id), Contract::encode(&binary, 20).unwrap()));
    }
    storage.apply_transaction(&writes).unwrap();
}
#[test]
fn child_commits_share_one_transaction_and_rollback_with_the_parent() {
    for fail in [false, true] {
        let mut storage = StorageDb::memory();
        let mut code = vec![
            op(Opcode::Copy, &[MEM_STACK + 6, MEM_STACK + 1]),
            op(Opcode::RemoteCall, &[2, 3, 5, 1]),
        ];
        if fail {
            code.push(op(Opcode::Fail, &[0, 7]));
        } else {
            code.push(op(Opcode::Copy, &[MEM_STATIC, 1]));
            code.push(op(Opcode::Ret, &[]));
        }
        fixture(
            &mut storage,
            code,
            vec![
                op(Opcode::Copy, &[MEM_STATIC, MEM_STACK + 1]),
                op(Opcode::Ret, &[]),
            ],
            false,
        );
        storage.database_mut().commit(1).unwrap();
        let mut call = Invocation::new(address(1), "run");
        call.args.push(InputValue::Uint(42u64.into()));
        let result = execute(&mut storage, &Context::default(), &[call]);
        if fail {
            assert!(result.is_err());
            assert!(storage
                .read(&address(2), MEM_STATIC, None)
                .unwrap()
                .is_none());
            assert!(!storage.database().has_pending());
        } else {
            result.unwrap();
            assert_eq!(
                storage
                    .read(&address(2), MEM_STATIC, None)
                    .unwrap()
                    .unwrap()
                    .value,
                Value::from(42u64)
            );
            assert_eq!(
                storage
                    .read(&address(1), MEM_STATIC, None)
                    .unwrap()
                    .unwrap()
                    .value,
                Value::from(1u64)
            );
        }
    }
}
#[test]
fn const_child_returns_are_deep_copied_after_key_initialization() {
    let mut storage = StorageDb::memory();
    fixture(
        &mut storage,
        vec![
            op(Opcode::Copy, &[MEM_STACK + 6, MEM_STACK + 1]),
            op(Opcode::RemoteCall, &[2, 3, 5, 1]),
            op(Opcode::Copy, &[MEM_STATIC, MEM_STACK + 5]),
            op(Opcode::Ret, &[]),
        ],
        vec![
            op(Opcode::Copy, &[MEM_STACK, MEM_STACK + 1]),
            op(Opcode::Ret, &[]),
        ],
        true,
    );
    let mut call = Invocation::new(address(1), "run");
    call.args.push(InputValue::Map(vec![(
        InputValue::String(b"run".to_vec()),
        InputValue::Array(vec![InputValue::Uint(42u64.into())]),
    )]));
    execute(&mut storage, &Context::default(), &[call]).unwrap();
    let Value::Ref(map) = storage
        .read(&address(1), MEM_STATIC, None)
        .unwrap()
        .unwrap()
        .value
    else {
        panic!("return ref")
    };
    let key = storage.lookup(&address(1), &Value::string("run")).unwrap();
    // The key is a binary constant, so it need not be in the backend index.
    let entry = storage
        .read(&address(1), map, Some(key.unwrap_or(3)))
        .unwrap()
        .unwrap();
    assert!(matches!(entry.value, Value::Ref(_)));
}
#[test]
fn native_const_methods_discard_their_own_writes() {
    let mut storage = StorageDb::memory();
    fixture(
        &mut storage,
        vec![op(Opcode::Ret, &[])],
        vec![op(Opcode::Copy, &[MEM_STATIC, 1]), op(Opcode::Ret, &[])],
        true,
    );
    execute(
        &mut storage,
        &Context::default(),
        &[Invocation::new(address(2), "run")],
    )
    .unwrap();
    assert!(storage
        .read(&address(2), MEM_STATIC, None)
        .unwrap()
        .is_none());
}

#[test]
fn execution_survives_reopen_and_can_fork_after_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let run = |storage: &mut StorageDb, value: u64| {
        let mut call = Invocation::new(address(1), "run");
        call.args.push(InputValue::Uint(value.into()));
        execute(storage, &Context::default(), &[call]).unwrap();
    };
    {
        let mut storage = StorageDb::open(directory.path()).unwrap();
        fixture(
            &mut storage,
            vec![
                op(Opcode::Copy, &[MEM_STACK + 6, MEM_STACK + 1]),
                op(Opcode::RemoteCall, &[2, 3, 5, 1]),
                op(Opcode::Copy, &[MEM_STATIC, 1]),
                op(Opcode::Ret, &[]),
            ],
            vec![
                op(Opcode::Copy, &[MEM_STATIC, MEM_STACK + 1]),
                op(Opcode::Ret, &[]),
            ],
            false,
        );
        storage.database_mut().commit(1).unwrap();
        run(&mut storage, 42);
        storage.database_mut().commit(2).unwrap();
    }
    {
        let mut storage = StorageDb::open(directory.path()).unwrap();
        assert_eq!(
            storage
                .read(&address(2), MEM_STATIC, None)
                .unwrap()
                .unwrap()
                .value,
            Value::from(42u64)
        );
        storage.database_mut().revert(1).unwrap();
        assert!(storage
            .read(&address(2), MEM_STATIC, None)
            .unwrap()
            .is_none());
        run(&mut storage, 84);
        storage.database_mut().commit(2).unwrap();
    }
    let storage = StorageDb::open(directory.path()).unwrap();
    assert_eq!(
        storage
            .read(&address(2), MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(84u64)
    );
    assert_eq!(
        storage
            .read(&address(1), MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(1u64)
    );
}

#[test]
fn dependency_depth_width_and_duplicate_aliases_match_native_rules() {
    use mmx_execution::prepare_locks;
    let mut storage = StorageDb::memory();
    let put = |storage: &mut StorageDb, id: u8, children: &[u8]| {
        let e = Executable {
            binary: address(101).into(),
            depends: children
                .iter()
                .enumerate()
                .map(|(i, c)| (format!("alias{i}"), address(*c).into()))
                .collect(),
            ..Default::default()
        };
        storage
            .apply_transaction(&TransactionBatch {
                contracts: vec![(address(id), Contract::encode(&e, 0).unwrap())],
                ..Default::default()
            })
            .unwrap();
    };
    put(&mut storage, 1, &[2, 2]);
    put(&mut storage, 2, &[]);
    let mut context = Context {
        max_rcall_width: 1,
        ..Default::default()
    };
    let calls = [Invocation::new(address(1), "run")];
    assert_eq!(
        prepare_locks(&storage, &context, &calls).unwrap(),
        [address(1), address(2)].into()
    );
    context.max_rcall_width = 10;
    put(&mut storage, 2, &[3]);
    put(&mut storage, 3, &[4]);
    put(&mut storage, 4, &[]);
    assert_eq!(prepare_locks(&storage, &context, &calls).unwrap().len(), 4);
    context.max_rcall_width = 2;
    assert!(prepare_locks(&storage, &context, &calls).is_err());
    context.max_rcall_width = 10;
    put(&mut storage, 4, &[5]);
    put(&mut storage, 5, &[]);
    assert!(prepare_locks(&storage, &context, &calls).is_err());
    put(&mut storage, 4, &[1]);
    assert!(prepare_locks(&storage, &context, &calls).is_err());
}

#[test]
fn a_later_failure_preserves_state_already_pending_before_this_batch() {
    use mmx_vm::storage::StorageWrite;
    let mut storage = StorageDb::memory();
    fixture(
        &mut storage,
        vec![op(Opcode::Copy, &[MEM_STATIC, 1]), op(Opcode::Ret, &[])],
        vec![op(Opcode::Ret, &[])],
        false,
    );
    storage.database_mut().commit(1).unwrap();
    storage
        .apply(
            &address(9),
            &[StorageWrite {
                address: MEM_STATIC,
                key: None,
                var: Var::new(73u64),
            }],
        )
        .unwrap();
    let calls = [
        Invocation::new(address(1), "run"),
        Invocation::new(address(2), "missing"),
    ];
    assert!(execute(&mut storage, &Context::default(), &calls).is_err());
    assert!(storage
        .read(&address(1), MEM_STATIC, None)
        .unwrap()
        .is_none());
    assert_eq!(
        storage
            .read(&address(9), MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(73u64)
    );
}

#[test]
fn prepared_execution_can_abort_or_include_final_settlement_in_one_commit() {
    use mmx_execution::prepare;
    use mmx_vm::storage::BalanceWrite;
    let mut storage = StorageDb::memory();
    fixture(
        &mut storage,
        vec![op(Opcode::Copy, &[MEM_STATIC, 1]), op(Opcode::Ret, &[])],
        vec![op(Opcode::Ret, &[])],
        false,
    );
    storage.database_mut().commit(1).unwrap();
    let calls = [Invocation::new(address(1), "run")];
    {
        let pending = prepare(&mut storage, &Context::default(), &calls).unwrap();
        assert!(pending.receipt().total_cost > 0);
        assert!(pending
            .storage()
            .read(&address(1), MEM_STATIC, None)
            .unwrap()
            .is_none());
        // A transaction validator can reject fees/outputs here by dropping it.
    }
    assert!(!storage.database().has_pending());
    let mut pending = prepare(&mut storage, &Context::default(), &calls).unwrap();
    pending.batch_mut().balances.push(BalanceWrite {
        contract: address(9),
        currency: address(0),
        amount: 7,
    });
    pending.commit().unwrap();
    assert_eq!(
        storage
            .read(&address(1), MEM_STATIC, None)
            .unwrap()
            .unwrap()
            .value,
        Value::from(1u64)
    );
    assert_eq!(
        storage.get_balance(&address(9), &address(0)).unwrap(),
        Some(7)
    );
}

use mmx_vm::{
    instruction::REF_B, Address, Engine, Error, Host, Instruction, Opcode, Storage, StorageDb,
    Value, MEM_STACK, MEM_STATIC,
};

fn op(code: Opcode, args: &[u64]) -> Instruction {
    let mut operands = [0; 4];
    for (dst, src) in operands.iter_mut().zip(args) {
        *dst = *src as u32;
    }
    Instruction::new(code, operands)
}
fn ret() -> Instruction {
    op(Opcode::Ret, &[])
}

#[test]
fn failed_execution_hides_staged_outputs_and_preserves_ledger() {
    let currency = [7; 32];
    let contract = [1; 32];
    let mut storage = StorageDb::memory();
    storage.set_balance(&contract, &currency, 10).unwrap();
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = Engine::new(contract, &mut storage, false, 1, 100_000);
        vm.write(1, Value::Binary([2; 32].to_vec())).unwrap();
        vm.write(2, 7u64).unwrap();
        vm.write(3, Value::Binary(currency.to_vec())).unwrap();
        vm.write(4, Value::Nil).unwrap();
        vm.write(5, Value::string("fail after sending")).unwrap();
        vm.init().unwrap();
        assert!(matches!(
            vm.run(
                &[
                    op(Opcode::Send, &[1, 2, 3, 4]),
                    op(Opcode::Mint, &[1, 2, 4]),
                    op(Opcode::Fail, &[5, 17]),
                ],
                0
            ),
            Err(Error::Fail { code: 17, .. })
        ));
        assert!(vm.outputs().is_err());
        assert!(vm.commit().is_err());
    }
    assert_eq!(storage.get_balance(&contract, &currency).unwrap(), Some(10));
    assert!(!storage.database().has_pending());
}

#[test]
fn successful_transfers_are_staged_and_cached_by_currency_value() {
    let contract = [3; 32];
    let currency = [7; 32];
    let mut storage = StorageDb::memory();
    storage.set_balance(&contract, &currency, 10).unwrap();
    storage.database_mut().commit(1).unwrap();
    {
        let mut vm = Engine::new(contract, &mut storage, true, 1, 100_000);
        vm.write(1, Value::Binary([2; 32].to_vec())).unwrap();
        vm.write(2, 7u64).unwrap();
        vm.write(3, Value::Binary(currency.to_vec())).unwrap();
        vm.write(4, Value::string("memo")).unwrap();
        vm.write(5, Value::Binary(currency.to_vec())).unwrap();
        vm.init().unwrap();
        vm.run(
            &[
                op(Opcode::Send, &[1, 2, 3, 4]),
                op(Opcode::Mint, &[1, 2, 4]),
                op(Opcode::Balance, &[MEM_STACK, 5]),
                ret(),
            ],
            0,
        )
        .unwrap();
        assert_eq!(vm.read(MEM_STACK).unwrap(), Some(Value::from(3u64)));
        let (sent, minted) = vm.outputs().unwrap();
        assert_eq!((sent.len(), minted.len()), (1, 1));
        assert_eq!(sent[0].contract, currency);
        assert_eq!(sent[0].amount, 7);
        assert_eq!(minted[0].contract, contract);
        assert_eq!(sent[0].memo.as_deref(), Some(b"memo".as_slice()));
        vm.commit().unwrap();
    }
    assert_eq!(storage.get_balance(&contract, &currency).unwrap(), Some(10));
    assert!(!storage.database().has_pending());
}

struct ContractHost {
    calls: usize,
    child: StorageDb,
    child_gas: u64,
}
impl Host<StorageDb> for ContractHost {
    fn remote_calls_enabled(&self) -> bool {
        true
    }
    fn contract_reads_enabled(&self) -> bool {
        true
    }
    fn remote_call(
        &mut self,
        vm: &mut Engine<'_, StorageDb>,
        name: &[u8],
        method: &[u8],
        nargs: u32,
    ) -> mmx_vm::Result<()> {
        assert_eq!(
            (name, method, nargs),
            (b"dep".as_slice(), b"increment".as_slice(), 1)
        );
        self.calls += 1;
        let stack = vm.stack_address()?;
        let arg = vm.read(stack + 1)?.ok_or(Error::Missing(stack + 1))?;
        let mut child = Engine::new(
            [9; 32],
            &mut self.child,
            false,
            vm.protocol_version,
            vm.gas_limit.saturating_sub(vm.gas_used),
        );
        child.write(1, 1u64)?;
        child.write(MEM_STACK + 1, arg)?;
        child.init()?;
        let result = child.run(&[op(Opcode::Add, &[MEM_STACK, MEM_STACK + 1, 1]), ret()], 0);
        self.child_gas = child.gas_used;
        let value = if result.is_ok() {
            child.read(MEM_STACK)?
        } else {
            None
        };
        // Nested execution is charged on both success and failure.
        vm.charge_gas(self.child_gas)?;
        result?;
        vm.write(stack, value.unwrap())
    }
    fn read_contract(
        &mut self,
        vm: &mut Engine<'_, StorageDb>,
        address: &Address,
        field: &[u8],
        dst: u64,
    ) -> mmx_vm::Result<()> {
        assert_eq!(*address, [9; 32]);
        assert_eq!(field, b"answer");
        vm.charge_gas(2000)?;
        vm.write(dst, 42u64)
    }
}

#[test]
fn remote_calls_can_execute_child_bytecode_and_read_contract_fields() {
    let mut storage = StorageDb::memory();
    let mut host = ContractHost {
        calls: 0,
        child: StorageDb::memory(),
        child_gas: 0,
    };
    let mut vm = Engine::new([1; 32], &mut storage, false, 1, 100_000);
    vm.write(1, Value::string("dep")).unwrap();
    vm.write(2, Value::string("increment")).unwrap();
    vm.write(3, Value::Binary([9; 32].to_vec())).unwrap();
    vm.write(4, Value::string("answer")).unwrap();
    vm.write(MEM_STACK + 6, 40u64).unwrap();
    vm.init().unwrap();
    vm.run_with_host(
        &[
            op(Opcode::RemoteCall, &[1, 2, 5, 1]),
            op(Opcode::Copy, &[MEM_STATIC, MEM_STACK + 5]),
            op(Opcode::ContractRead, &[MEM_STATIC + 1, 3, 4]),
            ret(),
        ],
        0,
        &mut host,
    )
    .unwrap();
    assert_eq!(host.calls, 1);
    assert!(host.child_gas > 0);
    assert_eq!(vm.read(MEM_STATIC).unwrap(), Some(Value::from(41u64)));
    assert_eq!(vm.read(MEM_STATIC + 1).unwrap(), Some(Value::from(42u64)));
    assert_eq!(vm.read(MEM_STACK + 6).unwrap(), None);
    vm.commit().unwrap();
}

#[test]
fn disabled_logs_skip_values_but_still_resolve_indirect_operands() {
    let mut storage = StorageDb::memory();
    let mut vm = Engine::new([0; 32], &mut storage, false, 1, 1000);
    vm.init().unwrap();
    vm.run(
        &[
            op(Opcode::Log, &[1, 999]),
            op(Opcode::Event, &[999, 998]),
            ret(),
        ],
        0,
    )
    .unwrap();
    let mut vm = Engine::new([0; 32], &mut storage, false, 1, 1000);
    vm.init().unwrap();
    assert!(matches!(
        vm.run(&[op(Opcode::Log, &[1, 999]).with_flags(REF_B), ret()], 0),
        Err(Error::Missing(999))
    ));
}

#[test]
fn child_failure_charges_parent_gas_and_prevents_publication() {
    let mut storage = StorageDb::memory();
    let mut host = ContractHost {
        calls: 0,
        child: StorageDb::memory(),
        child_gas: 0,
    };
    {
        let mut vm = Engine::new([1; 32], &mut storage, false, 1, 100_000);
        vm.write(1, Value::string("dep")).unwrap();
        vm.write(2, Value::string("increment")).unwrap();
        vm.write(MEM_STACK + 1, 40u64).unwrap();
        vm.init().unwrap();
        let used = vm.gas_used;
        vm.gas_limit = used + 150;
        assert!(matches!(
            vm.run_with_host(
                &[op(Opcode::RemoteCall, &[1, 2, 0, 1]), ret()],
                0,
                &mut host
            ),
            Err(Error::OutOfGas { .. })
        ));
        assert_eq!(vm.gas_used, used + 50 + host.child_gas);
        assert_eq!(vm.error_addr, Some(0));
        assert!(vm.commit().is_err());
        assert!(vm.outputs().is_err());
    }
    assert!(!storage.database().has_pending());
}

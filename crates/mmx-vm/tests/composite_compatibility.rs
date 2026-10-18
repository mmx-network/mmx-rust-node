//! Independently generated C++ Engine + StorageDB command traces. Compare
//! returned values, heap addresses, failure gas/codes/PCs, and persistence bytes.
use mmx_vm::{
    storage::{self, StorageWrite},
    Address, Engine, Error, Host, Instruction, Opcode, Storage, StorageDb, Value, Var, MEM_EXTERN,
    MEM_HEAP, MEM_STACK, MEM_STATIC, U256,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

struct State {
    storage: StorageDb,
    writes: String,
}
struct Recorder(Rc<RefCell<State>>);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn raw(value: Option<Value>) -> String {
    value.map_or_else(
        || "-".to_owned(),
        |v| hex(&Var::new(v).encode(false, false).unwrap()),
    )
}

impl Storage for Recorder {
    fn get_balance(&self, c: &Address, currency: &Address) -> storage::Result<Option<u128>> {
        self.0.borrow().storage.get_balance(c, currency)
    }
    fn read(&self, c: &Address, a: u64, k: Option<u64>) -> storage::Result<Option<Var>> {
        self.0.borrow().storage.read(c, a, k)
    }
    fn lookup(&self, c: &Address, value: &Value) -> storage::Result<Option<u64>> {
        self.0.borrow().storage.lookup(c, value)
    }
    fn apply(&mut self, c: &Address, writes: &[StorageWrite]) -> storage::Result<()> {
        let mut state = self.0.borrow_mut();
        state.storage.apply(c, writes)?;
        for write in writes {
            state.writes.push_str(&format!(
                "{},{},{};",
                write.address,
                write.key.map_or_else(|| "-".to_owned(), |k| k.to_string()),
                hex(&write.var.encode(false, true).unwrap())
            ));
        }
        Ok(())
    }
}

fn address(text: &str, aliases: &BTreeMap<String, u64>) -> u64 {
    if let Some((head, tail)) = text.split_once('+') {
        return address(head, aliases) + address(tail, aliases);
    }
    match text {
        "S" => MEM_STATIC,
        "T" => MEM_STACK,
        "H" => MEM_HEAP,
        "E" => MEM_EXTERN,
        "N" => MEM_HEAP + 1,
        _ if text.starts_with('@') => aliases[&text[1..]],
        _ if text.starts_with("0x") => u64::from_str_radix(&text[2..], 16).unwrap(),
        _ => text.parse().unwrap(),
    }
}

fn value(text: &str, aliases: &BTreeMap<String, u64>) -> Value {
    match text {
        "n" => Value::Nil,
        "t" => Value::Bool(true),
        "f" => Value::Bool(false),
        "m" => Value::Map { address: 0 },
        _ => {
            let data = &text[2..];
            match &text[..1] {
                "u" => Value::Uint(if let Some(data) = data.strip_prefix("0x") {
                    U256::from_str_radix(data, 16).unwrap()
                } else {
                    U256::from_dec_str(data).unwrap()
                }),
                "r" => Value::Ref(address(data, aliases)),
                "a" => Value::Array {
                    address: 0,
                    size: address(data, aliases) as u32,
                },
                "s" | "b" => {
                    let bytes = data
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
                        .collect();
                    if text.starts_with('s') {
                        Value::String(bytes)
                    } else {
                        Value::Binary(bytes)
                    }
                }
                _ => panic!("invalid test value: {text}"),
            }
        }
    }
}

fn program(text: &str, aliases: &BTreeMap<String, u64>) -> Vec<Instruction> {
    text.split(';')
        .map(|text| {
            let f: Vec<_> = text.split(',').collect();
            let code = match f[0] {
                "NOP" => Opcode::Nop,
                "CLR" => Opcode::Clr,
                "COPY" => Opcode::Copy,
                "CLONE" => Opcode::Clone,
                "CALL" => Opcode::Call,
                "RET" => Opcode::Ret,
                "GET" => Opcode::Get,
                "SET" => Opcode::Set,
                "ERASE" => Opcode::Erase,
                "PUSH_BACK" => Opcode::PushBack,
                "POP_BACK" => Opcode::PopBack,
                "CONCAT" => Opcode::Concat,
                "SIZE" => Opcode::Size,
                "TYPE" => Opcode::Type,
                "FAIL" => Opcode::Fail,
                "CONV" => Opcode::Conv,
                "MEMCPY" => Opcode::Memcpy,
                "SHA256" => Opcode::Sha256,
                "VERIFY" => Opcode::Verify,
                "LOG" => Opcode::Log,
                "EVENT" => Opcode::Event,
                "SEND" => Opcode::Send,
                "MINT" => Opcode::Mint,
                "BALANCE" => Opcode::Balance,
                "RCALL" => Opcode::RemoteCall,
                "CREAD" => Opcode::ContractRead,
                _ => panic!("invalid test opcode: {}", f[0]),
            };
            let mut args = [0; 4];
            for (i, arg) in f.iter().skip(2).enumerate() {
                args[i] = address(arg, aliases) as u32;
            }
            Instruction::new(code, args).with_flags(address(f[1], aliases) as u8)
        })
        .collect()
}

#[derive(Default)]
struct TestHost {
    enabled: bool,
    effects: String,
}
impl Host<Recorder> for TestHost {
    fn logs_enabled(&self) -> bool {
        self.enabled
    }
    fn events_enabled(&self) -> bool {
        self.enabled
    }
    fn remote_calls_enabled(&self) -> bool {
        self.enabled
    }
    fn contract_reads_enabled(&self) -> bool {
        self.enabled
    }
    fn log(
        &mut self,
        _: &mut Engine<'_, Recorder>,
        level: u32,
        message: &[u8],
    ) -> mmx_vm::Result<()> {
        self.effects.push_str(&format!(
            "L,{level},{};",
            raw(Some(Value::String(message.to_vec())))
        ));
        Ok(())
    }
    fn event(
        &mut self,
        vm: &mut Engine<'_, Recorder>,
        name: &[u8],
        data: u64,
    ) -> mmx_vm::Result<()> {
        self.effects.push_str(&format!(
            "E,{},{};",
            raw(Some(Value::String(name.to_vec()))),
            raw(vm.read(data)?)
        ));
        Ok(())
    }
    fn remote_call(
        &mut self,
        vm: &mut Engine<'_, Recorder>,
        name: &[u8],
        method: &[u8],
        nargs: u32,
    ) -> mmx_vm::Result<()> {
        self.effects.push_str(&format!(
            "R,{},{},{nargs};",
            raw(Some(Value::String(name.to_vec()))),
            raw(Some(Value::String(method.to_vec())))
        ));
        if method == b"fail" {
            return Err(Error::State("host rejected call"));
        }
        let stack = vm.stack_address()?;
        if nargs > 0 {
            vm.copy(stack, stack + 1)?;
        } else {
            vm.write(stack, Value::from(42u64))?;
        }
        Ok(())
    }
    fn read_contract(
        &mut self,
        vm: &mut Engine<'_, Recorder>,
        address: &Address,
        field: &[u8],
        dst: u64,
    ) -> mmx_vm::Result<()> {
        self.effects.push_str(&format!(
            "C,{},{};",
            raw(Some(Value::Binary(address.to_vec()))),
            raw(Some(Value::String(field.to_vec())))
        ));
        vm.write(dst, Value::from(1337u64))
    }
}

fn execute(
    vm: &mut Engine<'_, Recorder>,
    aliases: &mut BTreeMap<String, u64>,
    command: &str,
    host: &mut TestHost,
    state: &Rc<RefCell<State>>,
) -> Result<String, Error> {
    let f: Vec<_> = command.split(' ').collect();
    match f[0] {
        "hooks" => host.enabled = f[1] == "1",
        "balance" => {
            let Value::Binary(currency) = value(f[1], aliases) else {
                panic!("balance currency");
            };
            state.borrow_mut().storage.set_balance(
                &[0; 32],
                &currency.try_into().unwrap(),
                f[2].parse().unwrap(),
            )?;
        }
        "signature" => {
            use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
            let mut secret = [0; 32];
            secret[31] = 1;
            let key = SigningKey::from_slice(&secret).unwrap();
            let mut msg = [0; 32];
            msg[0] = 42;
            let sig: Signature = key.sign_prehash(&msg).unwrap();
            vm.write(1, Value::Binary(msg.to_vec()))?;
            vm.write(
                2,
                Value::Binary(
                    key.verifying_key()
                        .to_encoded_point(true)
                        .as_bytes()
                        .to_vec(),
                ),
            )?;
            vm.write(3, Value::Binary(sig.to_bytes().to_vec()))?;
            return Ok(format!(
                "{},{},{}",
                raw(vm.read(1)?),
                raw(vm.read(2)?),
                raw(vm.read(3)?)
            ));
        }
        "effects" => {
            let mut effects = host.effects.clone();
            let (outputs, minted) = vm.outputs()?;
            for (kind, values) in [("S", outputs), ("M", minted)] {
                for out in values {
                    effects.push_str(&format!(
                        "{kind},{},{},{},{};",
                        raw(Some(Value::Binary(out.contract.to_vec()))),
                        raw(Some(Value::Binary(out.address.to_vec()))),
                        out.amount,
                        raw(out.memo.clone().map(Value::String))
                    ));
                }
            }
            return Ok(if effects.is_empty() {
                "-".to_owned()
            } else {
                effects
            });
        }
        "init" => vm.init()?,
        "budget" => vm.gas_limit = vm.gas_used + address(f[1], aliases),
        "write" => vm.write(address(f[1], aliases), value(f[2], aliases))?,
        "assign" => vm.assign(address(f[1], aliases), value(f[2], aliases))?,
        "alloc" => {
            let a = vm.alloc()?;
            aliases.insert(f[1].to_owned(), a);
            return Ok(a.to_string());
        }
        "lookup" => {
            return Ok(vm
                .lookup(&value(f[2], aliases), f[1] == "1")?
                .unwrap_or(0)
                .to_string())
        }
        "write_entry" => vm.write_entry(
            address(f[1], aliases),
            address(f[2], aliases),
            value(f[3], aliases),
        )?,
        "write_key" => vm.write_key(
            address(f[1], aliases),
            &value(f[2], aliases),
            value(f[3], aliases),
        )?,
        "push" => vm.push_back(address(f[1], aliases), value(f[2], aliases))?,
        "pop" => vm.pop_back(address(f[1], aliases), address(f[2], aliases))?,
        "copy" => vm.copy(address(f[1], aliases), address(f[2], aliases))?,
        "clone" => vm.clone_value(address(f[1], aliases), address(f[2], aliases))?,
        "erase" => vm.erase(address(f[1], aliases))?,
        "erase_entry" => vm.erase_entry(address(f[1], aliases), address(f[2], aliases))?,
        "erase_key" => vm.erase_key(address(f[1], aliases), &value(f[2], aliases))?,
        "read" => return Ok(raw(vm.read(address(f[1], aliases))?)),
        "read_entry" => {
            return Ok(raw(
                vm.read_entry(address(f[1], aliases), address(f[2], aliases))?
            ))
        }
        "read_key" => {
            return Ok(raw(
                vm.read_key(address(f[1], aliases), &value(f[2], aliases))?
            ))
        }
        "commit" => vm.commit()?,
        "run" => vm.run_with_host(&program(f[1], aliases), 0, host)?,
        _ => panic!("invalid test command: {command}"),
    }
    Ok("-".to_owned())
}

fn check(
    row: &[&str],
    result: Result<String, Error>,
    gas: u64,
    code: u32,
    pc: Option<u64>,
    state: &Rc<RefCell<State>>,
) {
    let context = format!("{}: {}", row[0], row[1]);
    let diagnostic = format!("{result:?}");
    let actual = match result {
        Ok(value) => value,
        Err(_) => "ERR".to_owned(),
    };
    assert_eq!(actual, row[2], "result {context}: {diagnostic}");
    assert_eq!(gas.to_string(), row[3], "gas {context}");
    assert_eq!(code.to_string(), row[4], "error code {context}");
    assert_eq!(
        pc.map_or_else(|| "-".to_owned(), |pc| pc.to_string()),
        row[5],
        "error PC {context}"
    );
    let state = state.borrow();
    let writes = if state.writes.is_empty() {
        "-"
    } else {
        &state.writes
    };
    assert_eq!(writes, row[6], "storage writes {context}");
}

fn scenario(rows: &[Vec<&str>]) {
    let state = Rc::new(RefCell::new(State {
        storage: StorageDb::memory(),
        writes: String::new(),
    }));
    let mut aliases = BTreeMap::new();
    let mut cursor = 0;
    while cursor < rows.len() {
        let row = &rows[cursor];
        let f: Vec<_> = row[1].split(' ').collect();
        state.borrow_mut().writes.clear();
        if f[0] == "new" {
            let mut recorder = Recorder(state.clone());
            let mut vm = Engine::new([0; 32], &mut recorder, f[1] == "1", 1, 1_000_000);
            check(row, Ok("-".to_owned()), 0, 0, None, &state);
            let mut host = TestHost::default();
            cursor += 1;
            while cursor < rows.len()
                && !["new", "durable", "revert"]
                    .contains(&rows[cursor][1].split(' ').next().unwrap())
            {
                let row = &rows[cursor];
                state.borrow_mut().writes.clear();
                let result = execute(&mut vm, &mut aliases, row[1], &mut host, &state);
                if result.is_err() && row[1] == "commit" {
                    assert!(
                        state.borrow().writes.is_empty(),
                        "failed Rust commit published writes"
                    );
                    let version = state.borrow().storage.database().version();
                    state
                        .borrow_mut()
                        .storage
                        .database_mut()
                        .revert(version)
                        .unwrap();
                }
                check(
                    row,
                    result,
                    vm.gas_used,
                    vm.error_code,
                    vm.error_addr,
                    &state,
                );
                cursor += 1;
            }
        } else {
            let version = address(f[1], &aliases) as u32;
            let mut borrowed = state.borrow_mut();
            match f[0] {
                "durable" => borrowed.storage.database_mut().commit(version).unwrap(),
                "revert" => borrowed.storage.database_mut().revert(version).unwrap(),
                _ => panic!("command outside an invocation: {}", row[1]),
            }
            drop(borrowed);
            check(row, Ok("-".to_owned()), 0, 0, None, &state);
            cursor += 1;
        }
    }
}

#[test]
fn composite_state_gas_and_failures_match_cpp() {
    replay(include_str!("fixtures/composites.tsv"));
}

#[test]
fn feature_state_gas_and_failures_match_cpp() {
    replay(include_str!("fixtures/features.tsv"));
}

fn replay(fixture: &str) {
    let rows: Vec<Vec<&str>> = fixture
        .lines()
        .map(|line| line.split('\t').collect())
        .collect();
    assert!(!rows.is_empty());
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && rows[end][0] == rows[start][0] {
            end += 1;
        }
        scenario(&rows[start..end]);
        start = end;
    }
}

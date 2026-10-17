//! The VNX contract envelope carries MMX's existing bytecode and constants.
use mmx_types::{
    mmx::contract::{method_t, Binary},
    vnx_registry, VnxType,
};
use mmx_vm::{instruction, Engine, Instruction, Opcode, StorageDb, Value, Var, MEM_STACK};

#[test]
fn vnx_contract_binary_roundtrips_and_executes_mmx_bytecode() {
    let code = [
        Instruction::new(Opcode::Copy, [MEM_STACK as u32, 1, 0, 0]),
        Instruction::new(Opcode::Ret, [0; 4]),
    ];
    let mut constants = Var::new(Value::Nil).encode(false, false).unwrap();
    constants.extend(Var::new(42u64).encode(false, false).unwrap());
    let mut binary = Binary {
        name: "answer".to_owned(),
        binary: instruction::encode(&code).unwrap(),
        constant: constants,
        ..Default::default()
    };
    binary.methods.insert(
        "answer".to_owned(),
        method_t {
            name: "answer".to_owned(),
            is_public: true,
            ..Default::default()
        },
    );
    let encoded = binary.encode(&vnx_registry().unwrap()).unwrap();
    let decoded = Binary::decode(&encoded).unwrap();
    assert_eq!(decoded, binary);
    let (program, consumed) = instruction::decode(&decoded.binary).unwrap();
    assert_eq!(consumed, decoded.binary.len());
    let mut storage = StorageDb::memory();
    let mut engine = Engine::new([0; 32], &mut storage, false, 1, 100_000);
    let mut pos = 0;
    let mut address = 0;
    while pos < decoded.constant.len() {
        let (var, consumed) = Var::decode(&decoded.constant[pos..], false, false).unwrap();
        engine.assign(address, var.value).unwrap();
        pos += consumed;
        address += 1;
    }
    engine.init().unwrap();
    engine
        .run(&program, decoded.methods.get("answer").unwrap().entry_point)
        .unwrap();
    assert_eq!(engine.read(MEM_STACK).unwrap(), Some(Value::from(42u64)));
}

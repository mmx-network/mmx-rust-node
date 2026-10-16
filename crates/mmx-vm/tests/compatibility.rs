//! Ports of vm/engine_tests.cpp cases, backed by independently generated C++
//! fixtures. Regenerate with tools/generate_fixtures.py; no C++ needed to test.
use mmx_vm::{
    instruction::{self, CATCH_OVERFLOW, REF_A, REF_C},
    Engine, Instruction as I, Opcode as O, StorageDb, Value, Var, MEM_EXTERN, MEM_STACK,
    MEM_STATIC, U256,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
        .collect()
}

fn values() -> Vec<Value> {
    let mut values = vec![
        Value::Nil,
        Value::Bool(false),
        Value::Bool(true),
        Value::Ref(u64::MAX),
    ];
    for integer in [
        U256::zero(),
        U256::one(),
        U256::from(2),
        U256::from(0xFFFF),
        U256::one() << 16,
        U256::one() << 32,
        (U256::one() << 64) - 1,
        U256::one() << 64,
        (U256::one() << 128) - 1,
        U256::one() << 128,
        (U256::one() << 255) + 1,
        U256::MAX,
    ] {
        values.push(Value::Uint(integer));
    }
    values.extend([
        Value::string(""),
        Value::string("test"),
        Value::string("z"),
        Value::string("aa"),
        Value::Binary(vec![0, 1, 255, 128]),
        Value::Binary(vec![]),
        Value::String(vec![0, 1, 255, 128]),
        Value::Array {
            address: 0x123456789ABCDEF0,
            size: u32::MAX,
        },
        Value::Map { address: u64::MAX },
    ]);
    values
}

#[test]
fn original_value_serialize_cases_match_cpp_in_all_metadata_modes() {
    let values = values();
    let mut count = 0;
    for line in include_str!("fixtures/values.tsv").lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let value = &values[fields[0].parse::<usize>().unwrap()];
        let rc = fields[1] == "1";
        let flags = fields[2] == "1";
        let var = Var {
            value: value.clone(),
            flags: 0x1F,
            ref_count: 0x12345678,
        };
        let raw = unhex(fields[3]);
        assert_eq!(var.encode(rc, flags).unwrap(), raw, "fixture {line}");
        let (decoded, consumed) = Var::decode(&raw, rc, flags).unwrap();
        assert_eq!(consumed, raw.len());
        assert_eq!(&decoded.value, value);
        assert_eq!(decoded.flags, if flags { 0x1F } else { 0 });
        assert_eq!(decoded.ref_count, if rc { 0x12345678 } else { 0 });
        if let Value::Array { size, .. } = decoded.value {
            assert_eq!(size, u32::MAX);
        }
        count += 1;
    }
    assert_eq!(count, values.len() * 4);
}

fn code() -> Vec<I> {
    vec![
        I::new(O::Copy, [MEM_STACK as u32, 1, 0, 0]),
        I::new(O::Add, [MEM_STATIC as u32, MEM_STACK as u32, 2, 0]).with_flags(CATCH_OVERFLOW),
        I::new(O::JumpIf, [0x7FFF, MEM_EXTERN as u32, 0, 0]).with_flags(REF_A),
        I::new(O::Conv, [255, 256, 0x7FFE, u32::MAX]).with_flags(REF_C),
        I::new(O::Ret, [0; 4]),
        I::new(O::Nop, [0; 4]),
        I::new(O::Ret, [0; 4]),
    ]
}

#[test]
fn mmx_bytecode_is_byte_for_byte_compatible_with_cpp() {
    let fixture = unhex(include_str!("fixtures/bytecode.hex").trim());
    assert_eq!(instruction::encode(&code()).unwrap(), fixture);
    assert_eq!(
        instruction::decode(&fixture).unwrap(),
        (code(), fixture.len())
    );
}

#[test]
fn extended_symbol_table_and_raw_values_round_trip() {
    let mut code = Vec::new();
    for i in 0..500 {
        let instr = I::new(O::Copy, [i + 1000, i + 2000, 0, 0]);
        code.extend([instr, instr]);
    }
    for value in [0, 254, 255, 256, 0x7FFE, 0x7FFF, 0xFFFF, u32::MAX] {
        code.push(I::new(O::Jump, [value, 0, 0, 0]));
    }
    let raw = instruction::encode(&code).unwrap();
    assert_eq!(instruction::decode(&raw).unwrap(), (code, raw.len()));
}

#[test]
fn every_truncated_value_and_bytecode_is_rejected() {
    for value in values() {
        let bytes = Var::new(value).encode(true, true).unwrap();
        for end in 0..bytes.len() {
            assert!(
                Var::decode(&bytes[..end], true, true).is_err(),
                "cut {end} of {}",
                hex(&bytes)
            );
        }
    }
    let bytes = instruction::encode(&code()).unwrap();
    for end in 0..bytes.len() {
        assert!(instruction::decode(&bytes[..end]).is_err(), "cut {end}");
    }
}

#[test]
fn invalid_counts_tags_and_symbol_indices_are_rejected() {
    assert!(Var::decode(&[255], false, false).is_err());
    assert!(Var::decode(&[5, 255, 255, 255, 255], false, false).is_err());
    assert!(instruction::decode(&[2, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
    assert!(instruction::decode(&[1, 255, 255, 255, 255]).is_err());
    assert!(instruction::decode(&[1, 0, 0, 0, 0, 255, 255, 255, 255]).is_err());
    assert!(instruction::decode(&[1, 0, 0, 0, 0, 1, 0, 0, 0, 0]).is_err());
    assert!(
        instruction::decode(&[1, 0, 0, 0, 0, 1, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0]).is_err()
    );
}

#[test]
fn original_compare_clone_and_truthiness_cases() {
    let values = values();
    for value in &values {
        assert_eq!(value, &value.clone());
        assert!(value >= value);
        assert!(value <= value);
    }
    assert!(Value::Nil < Value::Bool(false));
    assert!(Value::Bool(false) < Value::Bool(true));
    assert!(Value::Ref(0) < Value::Ref(1));
    assert!(Value::from(0u64) < Value::from(1u64));
    assert!(Value::string("z") < Value::string("aa")); // Length precedes content.
    assert_eq!(
        Value::Array {
            address: 1,
            size: 0
        },
        Value::Array {
            address: 1,
            size: 99
        }
    );
    for value in [
        Value::Nil,
        Value::Bool(false),
        Value::from(0u64),
        Value::string(""),
        Value::Binary(vec![]),
    ] {
        assert!(!value.is_true());
    }
    for value in [
        Value::Bool(true),
        Value::from(1u64),
        Value::string("x"),
        Value::Ref(0),
        Value::Array {
            address: 0,
            size: 0,
        },
    ] {
        assert!(value.is_true());
    }
}

#[test]
fn scalar_execution_results_and_gas_match_cpp() {
    let mut count = 0;
    for line in include_str!("fixtures/engine.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let op = O::try_from(f[2].parse::<u8>().unwrap()).unwrap();
        let mut storage = StorageDb::memory();
        let mut engine = Engine::new(
            [0; 32],
            &mut storage,
            false,
            f[1].parse().unwrap(),
            10_000_000,
        );
        engine
            .write(1, Var::decode(&unhex(f[4]), false, false).unwrap().0.value)
            .unwrap();
        engine
            .write(2, Var::decode(&unhex(f[5]), false, false).unwrap().0.value)
            .unwrap();
        engine.init().unwrap();
        let program = [
            I::new(op, [MEM_STACK as u32, 1, 2, 0]).with_flags(f[3].parse().unwrap()),
            I::new(O::Ret, [0; 4]),
        ];
        let result = engine.run(&program, 0);
        if f[6] == "ERR" {
            assert!(result.is_err(), "expected C++ failure: {line}");
        } else {
            result.unwrap_or_else(|error| panic!("fixture {line}: {error}"));
            let value = engine.read(MEM_STACK).unwrap().unwrap();
            assert_eq!(
                hex(&Var::new(value).encode(false, false).unwrap()),
                f[6],
                "fixture {line}"
            );
            assert_eq!(
                engine.gas_used,
                f[7].parse::<u64>().unwrap(),
                "gas fixture {line}"
            );
        }
        count += 1;
    }
    assert!(count > 10_000);
}

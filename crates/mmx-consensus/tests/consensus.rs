use mmx_consensus::{Record, StreamEncoder, ValidationParams, Value, Variant};
use mmx_types::mmx;

#[test]
fn generated_declarations_keep_native_fields() {
    let p = ValidationParams::default();
    for r in [
        Record::from_generated(&mmx::Transaction::default()).unwrap(),
        Record::from_generated(&mmx::contract::Executable::default()).unwrap(),
        Record::from_generated(&mmx::contract::Binary::default()).unwrap(),
        Record::from_generated(&mmx::contract::Data::default()).unwrap(),
        Record::from_generated(&mmx::contract::WebData::default()).unwrap(),
        Record::from_generated(&mmx::solution::PubKey::default()).unwrap(),
        Record::from_generated(&mmx::solution::MultiSig::default()).unwrap(),
        Record::from_generated(&mmx::exec_result_t::default()).unwrap(),
        Record::from_generated(&mmx::operation::Deposit::default()).unwrap(),
    ] {
        r.calc_hash(false, 0)
            .unwrap_or_else(|e| panic!("{}: {e}; {r:?}", r.name));
        r.is_valid(&p).unwrap();
    }
    let mut tx = Record::from_generated(&mmx::Transaction::default()).unwrap();
    tx.fields.insert("nonce".into(), Value::Uint(1));
    tx.fields
        .insert("static_cost".into(), Value::Uint(tx.calc_cost(&p).unwrap()));
    tx.fields.insert(
        "id".into(),
        Value::Bytes(tx.calc_hash(false, 0).unwrap().to_vec()),
    );
    tx.fields.insert(
        "content_hash".into(),
        Value::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    assert!(tx.is_valid(&p).unwrap());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn result<T: std::fmt::Display>(value: mmx_consensus::Result<T>) -> String {
    value.map_or_else(|_| "ERR".into(), |v| v.to_string())
}

#[test]
fn native_consensus_hashes_validity_costs_and_serialization() {
    let mut count = 0;
    for line in include_str!("fixtures/native.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let label = f[0];
        let mut p = ValidationParams {
            network: b"mainnet".to_vec(),
            ..Default::default()
        };
        if f[1] == "1" {
            p.min_txfee = 17;
            p.min_txfee_io = 19;
            p.min_txfee_memo = 23;
            p.min_txfee_byte = 29;
            p.min_txfee_exec = 31;
            p.min_txfee_sign = 37;
            p.min_txfee_deploy = 41;
            p.min_txfee_depend = 43;
        }
        if f[1] == "2" {
            p.min_txfee = u64::MAX;
            p.min_txfee_io = u64::MAX;
            p.min_txfee_memo = u64::MAX;
            p.min_txfee_byte = u64::MAX;
            p.min_txfee_exec = u64::MAX;
            p.min_txfee_sign = u64::MAX;
            p.min_txfee_deploy = u64::MAX;
            p.min_txfee_depend = u64::MAX;
        }
        let record = Record::decode(&unhex(f[2])).unwrap_or_else(|e| panic!("{label}: {e}"));
        let encoded = record
            .encode()
            .unwrap_or_else(|e| panic!("{label}: encode {e}"));
        let roundtrip =
            Record::decode(&encoded).unwrap_or_else(|e| panic!("{label}: roundtrip {e}"));
        assert_eq!(roundtrip, record, "{label}: lossless VNX roundtrip");
        let compact = record.encode_compact().unwrap();
        assert_eq!(
            Record::decode_compact(&compact).unwrap(),
            record,
            "{label}: compact storage roundtrip"
        );
        let valid = record.is_valid(&p).map(|v| u8::from(v).to_string());
        assert_eq!(result(valid), f[3], "{label}: validity");
        assert_eq!(result(record.calc_cost(&p)), f[4], "{label}: cost");
        if f[5] != "-" {
            assert_eq!(result(record.num_bytes()), f[5], "{label}: num_bytes");
        }
        for version in 0..=1 {
            for full in [false, true] {
                let index = 6 + version as usize * 2 + usize::from(full);
                let actual = record.calc_hash(full, version).map(|h| hex(&h));
                assert_eq!(
                    result(actual),
                    f[index],
                    "{label}: hash version={version}, full={full}"
                );
            }
        }
        if record.name == "mmx.Transaction" {
            for full in [false, true] {
                assert_eq!(
                    hex(&record.hash_serialize(full).unwrap()),
                    f[10 + usize::from(full)],
                    "{label}: serialization full={full}"
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 824, "missing native fixtures");
}

#[test]
fn continuous_wire_streams_round_trip_the_entire_native_corpus_without_repeating_declarations() {
    let records: Vec<_> = include_str!("fixtures/native.tsv")
        .lines()
        .map(|line| Record::decode(&unhex(line.split('\t').nth(2).unwrap())).unwrap())
        .collect();
    assert_eq!(records.len(), 824);
    let mut encoder = StreamEncoder::default();
    let mut registry = vnx::Registry::new();
    for pass in 0..2 {
        for record in &records {
            let bytes = encoder.encode(record).unwrap();
            assert_eq!(
                Record::decode_with_registry(&bytes, &mut registry).unwrap(),
                *record
            );
            if pass == 1 {
                assert_eq!(bytes, record.encode_compact().unwrap(), "{}", record.name);
            }
        }
    }
}

#[test]
fn failed_stream_encoding_preserves_existing_declarations_and_does_not_announce_new_ones() {
    let first = Record::from_generated(&mmx::Transaction::default()).unwrap();
    let second = Record::from_generated(&mmx::contract::Executable::default()).unwrap();
    let mut broken = second.clone();
    broken.fields.pop_first().unwrap();
    let mut encoder = StreamEncoder::default();
    assert_eq!(encoder.encode(&first).unwrap(), first.encode().unwrap());
    assert!(encoder.encode(&broken).is_err());
    assert_eq!(
        encoder.encode(&first).unwrap(),
        first.encode_compact().unwrap()
    );
    assert_eq!(encoder.encode(&second).unwrap(), second.encode().unwrap());
}

#[test]
fn compact_storage_requires_known_schema_identities_and_a_complete_payload() {
    let record = Record::from_generated(&mmx::Transaction::default()).unwrap();
    let compact = record.encode_compact().unwrap();
    assert!(compact.len() < record.encode().unwrap().len());
    // Descriptor-free storage must not silently enable trusted schema fallback
    // on the ordinary standalone native-wire decoder.
    assert!(Record::decode(&compact).is_err());
    for end in [0, 1, 2, 9, compact.len() - 1] {
        assert!(Record::decode_compact(&compact[..end]).is_err());
    }
    let mut trailing = compact.clone();
    trailing.push(0);
    assert!(Record::decode_compact(&trailing).is_err());
    let mut unknown = compact.clone();
    unknown[2..10].fill(0);
    assert!(Record::decode_compact(&unknown).is_err());
    assert_eq!(Record::decode_compact(&compact).unwrap(), record);
}

#[test]
fn metadata_keeps_signedness_and_non_json_identity() {
    let unsigned = Variant::decode(&[1, 0, 1, 0, 1]).unwrap();
    let signed = Variant::decode(&[1, 0, 5, 0, 1]).unwrap();
    assert!(unsigned.is_json() && signed.is_json());
    let mut a = Record::from_generated(&mmx::contract::Data::default()).unwrap();
    a.fields.insert("value".into(), Value::Variant(unsigned));
    let mut b = a.clone();
    b.fields.insert("value".into(), Value::Variant(signed));
    assert_eq!(
        a.calc_hash(false, 0).unwrap(),
        b.calc_hash(false, 0).unwrap()
    );
    assert_ne!(
        a.calc_hash(false, 1).unwrap(),
        b.calc_hash(false, 1).unwrap()
    );
    // A native byte list is an array for hashing but is forbidden JSON. The
    // version-1 non-native preface prevents colliding with a JSON array.
    let binary = Variant::decode(&[2, 0, 12, 0, 1, 0, 1, 0, 0, 0, 1]).unwrap();
    let array = Variant::decode(&[2, 0, 12, 0, 17, 0, 1, 0, 0, 0, 1, 0, 1, 0, 1]).unwrap();
    assert!(!binary.is_json());
    assert!(array.is_json());
    a.fields.insert("value".into(), Value::Variant(binary));
    b.fields.insert("value".into(), Value::Variant(array));
    assert_eq!(
        a.calc_hash(false, 0).unwrap(),
        b.calc_hash(false, 0).unwrap()
    );
    assert_ne!(
        a.calc_hash(false, 1).unwrap(),
        b.calc_hash(false, 1).unwrap()
    );
}

#[test]
fn unsigned_id_and_full_hash_have_different_coverage() {
    let mut tx = Record::from_generated(&mmx::Transaction {
        inputs: vec![mmx::txin_t::default()],
        ..Default::default()
    })
    .unwrap();
    let id = tx.calc_hash(false, 0).unwrap();
    let full = tx.calc_hash(true, 0).unwrap();
    let Value::Sequence(inputs) = tx.fields.get_mut("inputs").unwrap() else {
        unreachable!()
    };
    let Value::Record(first) = &mut inputs[0] else {
        unreachable!()
    };
    first.fields.insert("flags".into(), Value::Uint(255));
    first.fields.insert("solution".into(), Value::Uint(0));
    assert_eq!(tx.calc_hash(false, 0).unwrap(), id);
    assert_ne!(tx.calc_hash(true, 0).unwrap(), full);
    let mut a = mmx::exec_result_t {
        inputs: vec![mmx::txin_t::default()],
        ..Default::default()
    };
    let hash = Record::from_generated(&a)
        .unwrap()
        .calc_hash(false, 0)
        .unwrap();
    // Execution-result input flags/solutions are excluded even in full hashes.
    a.inputs[0].flags = 255;
    a.inputs[0].solution = 0;
    assert_eq!(
        Record::from_generated(&a)
            .unwrap()
            .calc_hash(true, 1)
            .unwrap(),
        hash
    );
}

#[test]
fn malformed_and_unknown_schema_inputs_fail_explicitly() {
    use vnx::VnxType;
    let registry = mmx_types::vnx_registry().unwrap();
    let tx = mmx::Transaction::default();
    let bytes = tx.encode(&registry).unwrap();
    for size in 0..bytes.len() {
        assert!(Record::decode(&bytes[..size]).is_err(), "truncation {size}");
    }
    let mut trailing = bytes;
    trailing.push(0);
    assert!(Record::decode(&trailing).is_err());
    let mut schema = registry.by_name("mmx.Transaction").unwrap().clone();
    // A forged current hash must not make a different integer width trusted.
    schema
        .fields
        .iter_mut()
        .find(|f| f.name == "version")
        .unwrap()
        .code = vec![4];
    schema
        .fields
        .iter_mut()
        .find(|f| f.name == "version")
        .unwrap()
        .data_size = 8;
    let mut forged = vnx::Registry::new();
    for code in registry.iter() {
        forged
            .register(if code.code_hash == schema.code_hash {
                schema.clone()
            } else {
                code.clone()
            })
            .unwrap();
    }
    let mut writer = vnx::binary::Writer::new(Vec::new(), forged);
    writer
        .write_typed(&tx.to_value().unwrap(), schema.code_hash)
        .unwrap();
    assert!(matches!(
        Record::decode(&writer.into_inner()),
        Err(mmx_consensus::Error::Unsupported(_))
    ));
    for raw in [
        &[][..],
        &[0, 0],
        &[1, 0, 32, 0, 255, 255, 255, 255],
        &[33, 0],
    ] {
        assert!(Variant::decode(raw).is_err());
    }
}

#[test]
fn pointer_fields_apply_native_dynamic_casts() {
    use vnx::{serde_json::json, VnxType};
    let registry = mmx_types::vnx_registry().unwrap();
    let unrelated = json!({"__type":"mmx.contract.Data", "value":1});
    let tx = mmx::Transaction {
        execute: vec![unrelated.clone()],
        solutions: vec![unrelated],
        deploy: json!({"__type":"mmx.operation.Execute", "method":"run"}),
        ..Default::default()
    };
    let decoded = Record::decode(&tx.encode(&registry).unwrap()).unwrap();
    assert_eq!(
        decoded.field("execute").unwrap().seq().unwrap(),
        &[Value::Null]
    );
    assert_eq!(
        decoded.field("solutions").unwrap().seq().unwrap(),
        &[Value::Null]
    );
    assert_eq!(decoded.field("deploy").unwrap(), &Value::Null);
    assert!(!decoded.is_valid(&ValidationParams::default()).unwrap());
}

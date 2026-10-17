use mmx_types::{mmx, vnx as wire_vnx, vnx_registry};
use vnx::{serde_json::json, VnxType};

fn fixture(text: &str) -> Vec<u8> {
    text.trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
        .collect()
}

fn roundtrip<T: VnxType + PartialEq + std::fmt::Debug>(value: &T) {
    let bytes = value.encode(&vnx_registry().unwrap()).unwrap();
    assert_eq!(T::decode(&bytes).unwrap(), *value);
}

#[test]
fn generated_type_identities_match_the_native_cpp_snapshot() {
    let registry = vnx_registry().unwrap();
    let mut count = 0;
    for line in include_str!("fixtures/native_hashes.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let code = registry.by_name(f[0]).unwrap();
        assert_eq!(
            code.type_hash,
            u64::from_str_radix(f[1], 16).unwrap(),
            "{} type hash",
            f[0]
        );
        assert_eq!(
            code.code_hash,
            u64::from_str_radix(f[2], 16).unwrap(),
            "{} code hash",
            f[0]
        );
        count += 1;
    }
    assert_eq!(count, 638);
    assert_eq!(registry.len(), 644);
    assert_eq!(mmx::error_code_e::TXFEE_OVERRUN.value, 3169221647);
}

#[test]
fn native_cpp_wire_fixtures_decode_into_our_generated_types() {
    let key = mmx::KeyFile::decode(&fixture(include_str!(
        "../../../vendor/vnx-rust/crates/vnx/tests/fixtures/key.hex"
    )))
    .unwrap();
    assert_eq!(key.seed_value[0], 1);
    assert_eq!(key.seed_value[31], 32);
    roundtrip(&key);
    let params = mmx::ChainParams::decode(&fixture(include_str!(
        "../../../vendor/vnx-rust/crates/vnx/tests/fixtures/params.hex"
    )))
    .unwrap();
    assert_eq!(params.network, "mainnet");
    roundtrip(&params);
    let header = mmx::BlockHeader::decode(&fixture(include_str!(
        "../../../vendor/vnx-rust/crates/vnx/tests/fixtures/block.hex"
    )))
    .unwrap();
    assert_eq!(header.height, 5050001);
    roundtrip(&header);
    let request = wire_vnx::Request::decode(&fixture(include_str!(
        "../../../vendor/vnx-rust/crates/vnx/tests/fixtures/request.hex"
    )))
    .unwrap();
    assert_eq!(request.request_id, 42);
    assert_eq!(request.method["__type"], "mmx.Node.get_height");
    roundtrip(&request);
}

#[test]
fn transaction_defaults_addresses_and_amounts_keep_their_wire_layout() {
    let mut tx = mmx::Transaction::default();
    assert_eq!(tx.expires, u32::MAX);
    assert_eq!(tx.fee_ratio, 1024);
    tx.network = "mainnet".to_owned();
    tx.sender = Some(vnx::Fixed([0x42; 32]));
    tx.outputs.push(mmx::txout_t {
        address: vnx::Fixed([0xAB; 32]),
        amount: vnx::Fixed(u128::MAX.to_le_bytes()),
        memo: Some("wire test".to_owned()),
        ..Default::default()
    });
    roundtrip(&tx);
    assert_eq!(u128::from_le_bytes(tx.outputs[0].amount.0), u128::MAX);
}

#[test]
fn contract_types_aliases_collections_and_unknown_enums_roundtrip() {
    let mut binary = mmx::contract::Binary {
        name: "counter".to_owned(),
        ..Default::default()
    };
    binary.methods.insert(
        "increment".to_owned(),
        mmx::contract::method_t {
            name: "increment".to_owned(),
            is_public: true,
            entry_point: 17,
            ..Default::default()
        },
    );
    roundtrip(&binary);
    let mut contract = mmx::contract::Executable::default();
    assert_eq!(contract.init_method, "init");
    contract.init_args.push(json!({"owner": [1, 2, 3]}));
    contract
        .depends
        .insert("token".to_owned(), vnx::Fixed([7; 32]));
    roundtrip(&contract);
    let request = wire_vnx::Request::from_value(
        json!({"arguments": mmx::Node_get_height::default().to_value().unwrap()}),
    )
    .unwrap();
    assert_eq!(request.method["__type"], "mmx.Node.get_height");
    roundtrip(&request);
    roundtrip(&mmx::node_type_e { value: 0xDEADBEEF });
}

#[test]
fn generated_mmx_client_uses_the_vnx_runtime() {
    let runtime = vnx::runtime::Runtime::new().unwrap();
    let mut service = vnx::runtime::Methods::new();
    service.add("mmx.Node.get_height", None, |args, _| {
        let _request = mmx::Node_get_height::from_value(args)?;
        mmx::Node_get_height_return { _ret_0: 42 }.to_value()
    });
    runtime.register("Node", service).unwrap();
    let client = mmx::NodeClient::new(runtime.client("Node"));
    assert_eq!(client.get_height().unwrap(), 42);
    runtime.shutdown();
}

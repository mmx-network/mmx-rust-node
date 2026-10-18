use mmx_consensus::{Record, Value, Variant};
use mmx_types::mmx;

fn dynamic(code: &[u16], payload: &[u8]) -> Vec<u8> {
    let mut bytes = (code.len() as u16).to_le_bytes().to_vec();
    for word in code {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(payload);
    bytes
}
fn data_payload(value: &[u8]) -> Vec<u8> {
    let mut data = Record::from_generated(&mmx::contract::Data::default())
        .unwrap()
        .encode_compact()
        .unwrap();
    assert!(data.ends_with(&[1, 0, 0, 0]));
    data.truncate(data.len() - 4);
    data.extend(value);
    data
}
#[test]
fn tiny_zero_width_collections_and_nested_arrays_are_rejected_before_expansion() {
    for count in [10_000_u32, 10_000_000] {
        let bytes = dynamic(&[12, 0], &count.to_le_bytes());
        assert!(Variant::decode(&bytes)
            .unwrap_err()
            .to_string()
            .contains("allocation limit"));
        assert!(Record::decode_compact(&data_payload(&bytes)).is_err());
    }
    // Individual dimensions are small; aggregate expansion is what must fail.
    let bytes = dynamic(&[11, 16, 11, 16, 11, 16, 0], &[]);
    assert!(Variant::decode(&bytes).is_err());
    assert!(Record::decode_compact(&data_payload(&bytes)).is_err());
}
#[test]
fn ordinary_dense_values_and_small_zero_width_values_keep_exact_bytes() {
    let mut payload = 25_000_u32.to_le_bytes().to_vec();
    payload.extend(vec![1; 25_000]);
    let bytes = dynamic(&[12, 31], &payload);
    let data = Record::decode_compact(&data_payload(&bytes)).unwrap();
    let Value::Variant(value) = data.field("value").unwrap() else {
        panic!()
    };
    assert_eq!(value.bytes(), bytes);
    assert_eq!(value.value().seq().unwrap().len(), 25_000);
    assert!(data.memory_size() > data.encode_compact().unwrap().len());
    assert_eq!(
        Variant::decode(&dynamic(&[12, 0], &10_u32.to_le_bytes()))
            .unwrap()
            .value()
            .seq()
            .unwrap()
            .len(),
        10
    );
}

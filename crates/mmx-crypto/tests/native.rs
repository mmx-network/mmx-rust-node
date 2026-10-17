use mmx_crypto::{public_key_address, verify, VerifyError};
fn bytes<const N: usize>(text: &str) -> [u8; N] {
    let mut result = [0; N];
    assert_eq!(text.len(), N * 2);
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
    }
    result
}
#[test]
fn native_verification_parsing_address_hashes_and_cache_eviction() {
    let mut count = 0;
    for line in include_str!("fixtures/native.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let key = bytes(f[2]);
        assert_eq!(
            public_key_address(&key),
            bytes::<32>(f[4]),
            "{} address",
            f[0]
        );
        let actual = match verify(&bytes(f[1]), &key, &bytes(f[3])) {
            Ok(true) => "1",
            Ok(false) => "0",
            Err(VerifyError::SignatureEncoding) => "SIG",
            Err(VerifyError::PublicKeyEncoding) => "KEY",
        };
        assert_eq!(actual, f[5], "{} verify", f[0]);
        count += 1;
    }
    assert_eq!(count, 418);
}

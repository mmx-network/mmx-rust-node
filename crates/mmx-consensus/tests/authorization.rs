use mmx_consensus::{authorization::*, Error, Hash, Record};
use mmx_vm::address::encode;
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn bytes<const N: usize>(text: &str) -> [u8; N] {
    unhex(text).try_into().unwrap()
}
fn record(text: &str) -> Option<Record> {
    (text != "-").then(|| Record::decode(&unhex(text)).unwrap())
}
fn native_message(error: &AuthError, contract_name: &str) -> String {
    let AuthError::InvalidSolution(reason) = error else {
        return match error {
            AuthError::Input(Error::Input(message)) => message.clone(),
            _ => error.to_string(),
        };
    };
    let message = match reason {
        InvalidSolution::WrongPublicKey { expected, actual } => {
            if contract_name == "mmx.contract.MultiSig" {
                format!("wrong public key for {}", encode(expected))
            } else {
                format!(
                    "wrong public key: {} != {}",
                    encode(actual),
                    encode(expected)
                )
            }
        }
        InvalidSolution::InvalidSignature { owner: Some(owner) } => {
            format!("invalid signature for {}", encode(owner))
        }
        InvalidSolution::NoSuchOwner(owner) => format!("no such owner: {}", encode(owner)),
        InvalidSolution::InvalidSignatureType(owner) => {
            format!("invalid signature type for {}", encode(owner))
        }
        _ => reason.to_string(),
    };
    format!("invalid solution ({message})")
}
#[test]
fn native_contract_authentication_and_transaction_signature_selection() {
    // The native generator verifies these first. Replay the same cache history
    // before authorization, including eviction of the initially-zero tuple.
    for line in include_str!("../../mmx-crypto/tests/fixtures/native.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let _ = mmx_crypto::verify(&bytes(f[1]), &bytes(f[2]), &bytes(f[3]));
    }
    let mut count = 0;
    for line in include_str!("fixtures/authorization.tsv").lines() {
        let f: Vec<_> = line.split('\t').collect();
        let label = f[0];
        let version = f[2].parse().unwrap();
        let txid = bytes(f[3]);
        let value = record(f[4]).unwrap();
        let second = record(f[5]);
        let mut cost: u64 = f[7].parse().unwrap();
        let limit: u64 = f[8].parse().unwrap();
        let mut lookups = Vec::new();
        let lookup = |address: &Hash| -> AuthResult<Option<Record>> {
            lookups.push(*address);
            if cost.wrapping_add(1017) > limit {
                return Err(Error::Input("not enough gas to read contract".into()).into());
            }
            cost = cost.wrapping_add(1017);
            Ok(second.clone())
        };
        let mut contract_name = "mmx.contract.PubKey";
        let result = match f[1] {
            "contract" => {
                contract_name = &value.name;
                validate_solution(&value, second.as_ref(), &txid, version)
            }
            "sender" => authorize_sender(&value).map(|_| ()),
            "input" => {
                let index = f[6].parse::<usize>().unwrap();
                let input = value.field("inputs").unwrap().seq().unwrap()[index]
                    .record()
                    .unwrap();
                if input.field("flags").unwrap().uint().unwrap() & 1 != 0 {
                    if let Some(r) = &second {
                        contract_name = &r.name;
                    }
                }
                authorize_input(&value, index, lookup)
            }
            "user" => {
                if let Some(r) = &second {
                    contract_name = &r.name;
                }
                authorize_operation_user(&value, f[6].parse().unwrap(), lookup).map(|_| ())
            }
            mode => panic!("unknown mode {mode}"),
        };
        let (class, message) = match result {
            Ok(()) => ("ok", "-".to_owned()),
            Err(error) => (
                if error.is_static_failure() {
                    "static"
                } else {
                    "dynamic"
                },
                native_message(&error, contract_name),
            ),
        };
        assert_eq!(class, f[11], "{label}: class; {message}");
        assert_eq!(message, f[12], "{label}: reason and ordering");
        assert_eq!(cost.to_string(), f[9], "{label}: lookup cost");
        let calls: Vec<Hash> = if f[10] == "-" {
            vec![]
        } else {
            f[10].split(',').map(bytes).collect()
        };
        assert_eq!(lookups, calls, "{label}: lookup order");
        count += 1;
    }
    assert_eq!(count, 1232);
}

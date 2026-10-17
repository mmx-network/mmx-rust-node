use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use mmx_consensus::{authorization::*, Hash, Record, ValidationParams, Value};
use mmx_types::mmx;
use std::collections::BTreeMap;
fn key(n: u8) -> SigningKey {
    let mut s = [0; 32];
    s[31] = n;
    SigningKey::from_slice(&s).unwrap()
}
fn address(n: u8) -> Hash {
    mmx_crypto::public_key_address(
        &key(n)
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    )
}
fn solution(n: u8, id: &Hash) -> Record {
    let signer = key(n);
    let sig: Signature = signer.sign_prehash(id).unwrap();
    let mut r = Record::from_generated(&mmx::solution::PubKey::default()).unwrap();
    r.fields.insert(
        "pubkey".into(),
        Value::Bytes(
            signer
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .to_vec(),
        ),
    );
    r.fields
        .insert("signature".into(), Value::Bytes(sig.to_bytes().to_vec()));
    r
}
fn optional_address(a: Hash) -> Value {
    Value::Optional(Some(Box::new(Value::Bytes(a.to_vec()))))
}
fn base(
    version: u32,
    sender: Hash,
    input: Hash,
    flags: u8,
    input_solution: u16,
    user: Hash,
    user_solution: u16,
) -> Record {
    let mut tx = Record::from_generated(&mmx::Transaction {
        version,
        network: "mainnet".into(),
        nonce: 1,
        max_fee_amount: 1_000_000,
        ..Default::default()
    })
    .unwrap();
    tx.fields.insert("sender".into(), optional_address(sender));
    let mut i = Record::from_generated(&mmx::txin_t::default()).unwrap();
    i.fields
        .insert("address".into(), Value::Bytes(input.to_vec()));
    i.fields
        .insert("amount".into(), Value::Bytes(10u128.to_le_bytes().to_vec()));
    i.fields.insert("flags".into(), Value::Uint(flags.into()));
    i.fields
        .insert("solution".into(), Value::Uint(input_solution.into()));
    tx.fields
        .insert("inputs".into(), Value::Sequence(vec![Value::Record(i)]));
    let mut out = Record::from_generated(&mmx::txout_t::default()).unwrap();
    out.fields
        .insert("address".into(), Value::Bytes([4; 32].to_vec()));
    out.fields
        .insert("amount".into(), Value::Bytes(10u128.to_le_bytes().to_vec()));
    tx.fields
        .insert("outputs".into(), Value::Sequence(vec![Value::Record(out)]));
    let mut op = Record::from_generated(&mmx::operation::Execute::default()).unwrap();
    op.fields
        .insert("address".into(), Value::Bytes([9; 32].to_vec()));
    op.fields
        .insert("method".into(), Value::String(b"run".to_vec()));
    op.fields.insert("user".into(), optional_address(user));
    op.fields
        .insert("solution".into(), Value::Uint(user_solution.into()));
    tx.fields
        .insert("execute".into(), Value::Sequence(vec![Value::Record(op)]));
    tx
}
fn seal_content(tx: &mut Record, p: &ValidationParams) {
    tx.fields
        .insert("static_cost".into(), Value::Uint(tx.calc_cost(p).unwrap()));
    tx.fields.insert(
        "content_hash".into(),
        Value::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
}
#[test]
fn signed_pubkey_transactions_pass_static_and_all_authorization_steps() {
    let p = ValidationParams {
        network: b"mainnet".to_vec(),
        ..Default::default()
    };
    for version in [0, 1] {
        let owner = address(1);
        let mut tx = base(version, owner, owner, 0, 0, owner, 0);
        let id = tx.calc_hash(false, 0).unwrap();
        tx.fields.insert("id".into(), Value::Bytes(id.to_vec()));
        tx.fields.insert(
            "solutions".into(),
            Value::Sequence(vec![Value::Record(solution(1, &id))]),
        );
        seal_content(&mut tx, &p);
        assert!(tx.is_valid(&p).unwrap());
        assert_eq!(authorize_sender(&tx).unwrap(), owner);
        authorize_input(&tx, 0, |_| {
            panic!("unflagged input must not fetch a contract")
        })
        .unwrap();
        assert_eq!(
            authorize_operation_user(&tx, 0, |a| {
                assert_eq!(*a, owner);
                Ok(None)
            })
            .unwrap(),
            Some(owner)
        );
        let mut tampered = tx.clone();
        tampered.fields.insert("nonce".into(), Value::Uint(2));
        assert!(!tampered.is_valid(&p).unwrap()); // authentication requires prior static validation
        let mut bad = solution(1, &id);
        bad.fields
            .insert("signature".into(), Value::Bytes(vec![0; 64]));
        tx.fields.insert(
            "solutions".into(),
            Value::Sequence(vec![Value::Record(bad)]),
        );
        seal_content(&mut tx, &p);
        assert!(tx.is_valid(&p).unwrap());
        assert!(matches!(
            authorize_sender(&tx),
            Err(AuthError::InvalidSolution(
                InvalidSolution::InvalidSignature { .. }
            ))
        ));
    }
}
#[test]
fn signed_multisig_inputs_and_users_follow_transaction_version_rules() {
    let p = ValidationParams {
        network: b"mainnet".to_vec(),
        ..Default::default()
    };
    let contract_address = [7; 32];
    let mut contract = Record::from_generated(&mmx::contract::MultiSig::default()).unwrap();
    contract
        .fields
        .insert("num_required".into(), Value::Uint(2));
    contract.fields.insert(
        "owners".into(),
        Value::Sequence(vec![
            Value::Bytes(address(1).to_vec()),
            Value::Bytes(address(2).to_vec()),
        ]),
    );
    for version in [0, 1] {
        let sender = address(3);
        let mut tx = base(version, sender, contract_address, 1, 1, contract_address, 1);
        let id = tx.calc_hash(false, 0).unwrap();
        tx.fields.insert("id".into(), Value::Bytes(id.to_vec()));
        let mut multisig = Record::from_generated(&mmx::solution::MultiSig::default()).unwrap();
        multisig
            .fields
            .insert("num_required".into(), Value::Uint(2));
        multisig.fields.insert(
            "solutions".into(),
            Value::Map(BTreeMap::from([
                (
                    Value::Bytes(address(1).to_vec()),
                    Value::Record(solution(1, &id)),
                ),
                (
                    Value::Bytes(address(2).to_vec()),
                    Value::Record(solution(2, &id)),
                ),
            ])),
        );
        tx.fields.insert(
            "solutions".into(),
            Value::Sequence(vec![
                Value::Record(solution(3, &id)),
                Value::Record(multisig.clone()),
            ]),
        );
        seal_content(&mut tx, &p);
        assert!(tx.is_valid(&p).unwrap());
        assert_eq!(authorize_sender(&tx).unwrap(), sender);
        authorize_input(&tx, 0, |a| {
            assert_eq!(*a, contract_address);
            Ok(Some(contract.clone()))
        })
        .unwrap();
        assert_eq!(
            authorize_operation_user(&tx, 0, |a| {
                assert_eq!(*a, contract_address);
                Ok(Some(contract.clone()))
            })
            .unwrap(),
            Some(contract_address)
        );
        multisig
            .fields
            .insert("num_required".into(), Value::Uint(1));
        tx.fields.insert(
            "solutions".into(),
            Value::Sequence(vec![
                Value::Record(solution(3, &id)),
                Value::Record(multisig),
            ]),
        );
        seal_content(&mut tx, &p);
        assert!(tx.is_valid(&p).unwrap());
        let result = authorize_input(&tx, 0, |_| Ok(Some(contract.clone())));
        if version == 0 {
            result.unwrap();
        } else {
            assert!(matches!(
                result,
                Err(AuthError::InvalidSolution(
                    InvalidSolution::WrongThreshold {
                        expected: 2,
                        actual: 1
                    }
                ))
            ));
        }
    }
}

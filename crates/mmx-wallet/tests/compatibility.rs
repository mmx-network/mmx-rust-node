use mmx_wallet::*;
use serde_json::Value;
use std::collections::BTreeMap;
fn reference() -> Value {
    // Generated with ECDSA_Wallet::complete(), then re-signed via sign_off()
    // after setting max_fee_amount = cost_to_fee(static_cost + gas_limit,
    // fee_ratio), so the C++ reference includes the final signature cost.
    serde_json::from_str(include_str!("fixtures/reference.json")).unwrap()
}
fn seed() -> [u8; 32] {
    std::array::from_fn(|i| (i + 1) as u8)
}
#[test]
fn derivation_matches_cpp() {
    let v = reference();
    assert_eq!(fingerprint(&seed(), None), v["fingerprint"]);
    assert_eq!(fingerprint(&seed(), Some("")), v["fingerprint_empty_pass"]);
    assert_eq!(
        fingerprint(&seed(), Some("test 世界 🔑")),
        v["fingerprint_pass"]
    );
    assert_eq!(seed_to_words(&seed()), v["mnemonic"]);
    assert_eq!(
        words_to_seed(v["mnemonic"].as_str().unwrap()).unwrap(),
        seed()
    );
    let w = Wallet::derive(&seed(), "test 世界 🔑", 7, 3).unwrap();
    assert_eq!(serde_json::to_value(&w.addresses).unwrap(), v["addresses"]);
}
#[test]
fn keyfile_matches_cpp() {
    let bytes = hex::decode(include_str!("fixtures/keyfile.hex").trim()).unwrap();
    let key = KeyFile::decode(&bytes).unwrap();
    assert_eq!(key.seed_value, seed());
    assert_eq!(key.encode(), bytes);
    for pass in [None, Some(""), Some("test 世界 🔑")] {
        let key = KeyFile::new(seed(), pass);
        let restored = KeyFile::decode(&key.encode()).unwrap();
        assert_eq!(restored.seed_value, seed());
        assert_eq!(restored.finger_print, key.finger_print);
        assert_eq!(restored.requires_passphrase(), pass.is_some());
        restored.unlock(pass.unwrap_or(""), 7, 3).unwrap();
    }
    for end in 0..bytes.len() {
        assert!(KeyFile::decode(&bytes[..end]).is_err());
    }
}
#[test]
fn transactions_match_cpp() {
    let reference = reference();
    let params = ChainParams::default();
    let w = Wallet::derive(&seed(), "test 世界 🔑", 7, 3).unwrap();
    let balances = BTreeMap::from([
        ((w.addresses[0], Address::default()), 9000000),
        ((w.addresses[1], Address::default()), 12000000),
    ]);
    for (i, height) in [5050000].iter().enumerate() {
        let tx = w
            .transfer(
                &balances,
                &params,
                *height,
                Output {
                    address: w.addresses[2],
                    contract: Address::default(),
                    amount: 15000000,
                    memo: Some("memo 世界".into()),
                },
                &TransferOptions {
                    nonce: Some(123456789),
                    fee_ratio: 1536,
                    expire_delta: 123,
                    ..Default::default()
                },
            )
            .unwrap();
        #[cfg(feature = "native")]
        {
            let native = tx.to_record().unwrap();
            assert_eq!(native.calc_hash(false, 0).unwrap(), tx.id);
            assert_eq!(native.calc_hash(true, 0).unwrap(), tx.content_hash);
            assert_eq!(
                native.hash_serialize(false).unwrap(),
                tx.hash_serialize(false).unwrap()
            );
            let validation = mmx_consensus::ValidationParams {
                network: params.network.as_bytes().to_vec(),
                ..Default::default()
            };
            assert!(native.is_valid(&validation).unwrap());
            assert_eq!(
                native.calc_cost(&validation).unwrap(),
                u64::from(tx.static_cost)
            );
            let decoded = mmx_consensus::Record::decode(&native.encode().unwrap()).unwrap();
            assert_eq!(decoded, native);
        }
        let expected = &reference["transactions"][i];
        let id: Vec<u8> = expected["id"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(
            tx.id.as_slice(),
            id,
            "transaction ID version {}",
            tx.version
        );
        let hash: Vec<u8> = expected["content_hash"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(
            tx.content_hash.as_slice(),
            hash,
            "content hash version {}",
            tx.version
        );
        let bytes = serde_json::to_vec(&tx).unwrap();
        let restored: Transaction = serde_json::from_slice(&bytes).unwrap();
        restored.verify(&params).unwrap();
        let mut changed = restored.clone();
        changed.outputs[0].amount += 1;
        assert!(changed.verify(&params).is_err());
    }
}
#[test]
fn exact_amounts_and_addresses() {
    assert_eq!(
        Address::default().to_string(),
        "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev"
    );
    for a in reference()["addresses"].as_array().unwrap() {
        let text = a.as_str().unwrap();
        assert_eq!(text.parse::<Address>().unwrap().to_string(), text);
    }
    for (s, n) in [
        ("1e-6", 1),
        ("1.2300000", 1230000),
        ("900719925.474099", 900719925474099),
    ] {
        assert_eq!(parse_amount(s, 6).unwrap(), n);
    }
    for s in [
        "0",
        "-1",
        "1.0000001",
        "1e999",
        "340282366920938463463374607431768211456",
    ] {
        assert!(parse_amount(s, 6).is_err());
    }
    assert_eq!(
        format_amount(u128::MAX, 6).unwrap(),
        "340282366920938463463374607431768.211455"
    );
}

#[test]
fn legacy_and_json_keyfiles_restore_the_same_wallet() {
    let bytes = hex::decode(include_str!("fixtures/legacy-keyfile.hex").trim()).unwrap();
    let legacy = KeyFile::decode(&bytes).unwrap();
    assert_eq!(legacy.seed_value, seed());
    assert_eq!(legacy.finger_print, None);
    let key = KeyFile::new(seed(), Some("test 世界 🔑"));
    let json = serde_json::to_vec(&key).unwrap();
    let restored = KeyFile::decode(&json).unwrap();
    assert_eq!(restored.fingerprint(), key.fingerprint());
    assert!(restored.unlock("wrong", 0, 1).is_err());
}

#[test]
fn signatures_are_verified_even_if_content_hash_is_recomputed() {
    let params = ChainParams::default();
    let wallet = Wallet::derive(&seed(), "", 0, 1).unwrap();
    let balances = BTreeMap::from([((wallet.addresses[0], Address::default()), 10000000)]);
    let mut tx = wallet
        .transfer(
            &balances,
            &params,
            5050000,
            Output {
                address: wallet.addresses[0],
                contract: Address::default(),
                amount: 1000000,
                memo: None,
            },
            &TransferOptions::default(),
        )
        .unwrap();
    tx.solutions[0].signature[0] ^= 1;
    tx.content_hash = tx.calc_hash(true).unwrap();
    assert!(matches!(
        tx.verify(&params),
        Err(Error::InvalidTransaction("signature verification failed"))
    ));
}

#[test]
fn token_transfer_uses_a_separate_native_fee_payer() {
    let params = ChainParams::default();
    let wallet = Wallet::derive(&seed(), "", 0, 2).unwrap();
    let token = Address([1; 32]);
    let balances = BTreeMap::from([
        ((wallet.addresses[0], token), 1000),
        ((wallet.addresses[1], Address::default()), 10000000),
    ]);
    let tx = wallet
        .transfer(
            &balances,
            &params,
            5050000,
            Output {
                address: wallet.addresses[1],
                contract: token,
                amount: 1000,
                memo: None,
            },
            &TransferOptions::default(),
        )
        .unwrap();
    assert_eq!(tx.sender, Some(wallet.addresses[1]));
    assert_eq!(tx.inputs[0].address, wallet.addresses[0]);
    assert_eq!(tx.inputs[0].solution, 1);
    assert_eq!(tx.solutions.len(), 2);
    tx.verify(&params).unwrap();
    let no_fee = BTreeMap::from([((wallet.addresses[0], token), 1000)]);
    let no_fee_tx = wallet
        .transfer(
            &no_fee,
            &params,
            5050000,
            tx.outputs[0].clone(),
            &TransferOptions::default(),
        )
        .unwrap();
    assert_eq!(no_fee_tx.sender, Some(wallet.addresses[0]));
    no_fee_tx.verify(&params).unwrap();
}

#[test]
fn zero_gas_fee_includes_signature_and_leaves_affordability_to_rpc() {
    let wallet = Wallet::derive(&seed(), "", 0, 1).unwrap();
    let params = ChainParams::default();
    let output = Output {
        address: wallet.addresses[0],
        contract: Address::default(),
        amount: 1_000_000,
        memo: None,
    };
    // Include zero and insufficient remaining fee balances: these still sign,
    // since RPC validation is responsible for fee-payer affordability.
    for remaining in [0, 300, 1299, 1300] {
        let balances = BTreeMap::from([(
            (wallet.addresses[0], Address::default()),
            output.amount + remaining,
        )]);
        let mut tx = wallet
            .transfer(
                &balances,
                &params,
                5050000,
                output.clone(),
                &TransferOptions {
                    gas_limit: 0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(tx.solutions.len(), 1);
        assert_eq!(tx.static_cost, 1300);
        assert_eq!(tx.max_fee_amount, 1300);
        tx.verify(&params).unwrap();
        tx.max_fee_amount = 300;
        assert!(matches!(
            tx.verify(&params),
            Err(Error::InvalidTransaction(
                "maximum fee is below the static fee"
            ))
        ));
    }
}

#[test]
fn fee_counts_distinct_input_and_sender_signatures() {
    let wallet = Wallet::derive(&seed(), "", 0, 3).unwrap();
    let params = ChainParams::default();
    let token = Address([1; 32]);
    let output = Output {
        address: wallet.addresses[0],
        contract: token,
        amount: 1500,
        memo: None,
    };
    for (payer, signatures, expected_cost) in [(1, 2, 2400), (2, 3, 3400)] {
        let balances = BTreeMap::from([
            ((wallet.addresses[0], token), 1000),
            ((wallet.addresses[1], token), 900),
            ((wallet.addresses[payer], Address::default()), 1),
        ]);
        let tx = wallet
            .transfer(
                &balances,
                &params,
                5050000,
                output.clone(),
                &TransferOptions {
                    fee_ratio: 1536,
                    gas_limit: 99,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(tx.sender, Some(wallet.addresses[payer]));
        assert_eq!(tx.solutions.len(), signatures);
        assert_eq!(tx.static_cost, expected_cost);
        assert_eq!(tx.max_fee_amount, (expected_cost + 99) * 1536 / 1024);
        tx.verify(&params).unwrap();
    }
}

#[cfg(feature = "native")]
#[test]
fn native_completion_matches_cpp_transfer_and_supports_both_transaction_versions() {
    use mmx_consensus::{authorization, Record, Value as N};
    use mmx_types::mmx;
    let w = Wallet::derive(&seed(), "test 世界 🔑", 7, 3).unwrap();
    let balances = BTreeMap::from([
        ((w.addresses[0], Address::default()), 9_000_000),
        ((w.addresses[1], Address::default()), 12_000_000),
    ]);
    let mut params = mmx::ChainParams {
        hardfork2_height: 0,
        network: ChainParams::default().network,
        ..Default::default()
    };
    let simple = w
        .transfer(
            &balances,
            &ChainParams::default(),
            5_050_000,
            Output {
                address: w.addresses[2],
                contract: Address::default(),
                amount: 15_000_000,
                memo: Some("memo 世界".into()),
            },
            &TransferOptions {
                nonce: Some(123456789),
                fee_ratio: 1536,
                expire_delta: 123,
                ..Default::default()
            },
        )
        .unwrap();
    let mut template = Record::from_generated(&mmx::Transaction::default()).unwrap();
    template.fields.insert("note".into(), N::Uint(858544509));
    template.fields.insert(
        "outputs".into(),
        N::Sequence(vec![N::Record(
            Record::from_generated(&mmx::txout_t {
                address: w.addresses[2].0.into(),
                amount: 15_000_000_u128.to_le_bytes().into(),
                memo: Some("memo 世界".into()),
                ..Default::default()
            })
            .unwrap(),
        )]),
    );
    let options = mmx::spend_options_t {
        nonce: Some(123456789),
        fee_ratio: 1536,
        expire_delta: Some(123),
        ..Default::default()
    };
    let tx = w
        .complete_native(&template, &balances, &params, 5_050_000, &options)
        .unwrap();
    assert_eq!(tx, simple.to_record().unwrap());
    authorization::authorize_sender(&tx).unwrap();
    params.hardfork2_height = u32::MAX;
    let old = w
        .complete_native(&template, &balances, &params, 5_050_000, &options)
        .unwrap();
    assert_eq!(old.field("version").unwrap().uint().unwrap(), 0);
    assert!(old.is_valid(&(&params).into()).unwrap());
    authorization::authorize_sender(&old).unwrap();
}
#[cfg(feature = "native")]
#[test]
fn completion_accounts_for_deposits_shared_user_signatures_and_existing_inputs() {
    use mmx_consensus::{authorization, Record, Value as N};
    use mmx_types::mmx;
    let w = Wallet::derive(&seed(), "", 0, 2).unwrap();
    let params = mmx::ChainParams {
        hardfork2_height: 0,
        ..Default::default()
    };
    let balances = BTreeMap::from([((w.addresses[0], Address::default()), 10_000_000)]);
    let mut tx = Record::from_generated(&mmx::Transaction::default()).unwrap();
    tx.fields.insert(
        "execute".into(),
        N::Sequence(vec![N::Record(
            Record::from_generated(&mmx::operation::Deposit {
                address: [8; 32].into(),
                method: "deposit".into(),
                amount: 1000_u128.to_le_bytes().into(),
                user: Some(w.addresses[0].0.into()),
                ..Default::default()
            })
            .unwrap(),
        )]),
    );
    let signed = w
        .complete_native(
            &tx,
            &balances,
            &params,
            1,
            &mmx::spend_options_t {
                gas_limit: 0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(signed.field("solutions").unwrap().seq().unwrap().len(), 1);
    assert_eq!(signed.field("inputs").unwrap().seq().unwrap().len(), 1);
    assert_eq!(
        signed.field("max_fee_amount").unwrap(),
        signed.field("static_cost").unwrap()
    );
    authorization::authorize_sender(&signed).unwrap();
    let op = signed.field("execute").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    assert_eq!(op.field("solution").unwrap().uint().unwrap(), 0);
    let mut insufficient = balances.clone();
    insufficient.insert((w.addresses[0], Address::default()), 1);
    assert!(w
        .complete_native(&tx, &insufficient, &params, 1, &Default::default())
        .is_err());
    let mut existing = tx.clone();
    existing.fields.insert(
        "inputs".into(),
        N::Sequence(vec![N::Record(
            Record::from_generated(&mmx::txin_t {
                address: w.addresses[0].0.into(),
                amount: 20_000_000_u128.to_le_bytes().into(),
                ..Default::default()
            })
            .unwrap(),
        )]),
    );
    assert!(w
        .complete_native(&existing, &balances, &params, 1, &Default::default())
        .is_err());
}

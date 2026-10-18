use mmx_consensus::{header, proof, response, vote, Record, Value};
use mmx_farming::{encoding, farmer::Farmer, service, wallet::Account};
use mmx_types::{mmx, VnxType};
use mmx_wallet::{Address, FarmerKey, KeyFile};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use vnx::{
    net::{NetworkClient, Server, ServerOptions},
    runtime::{Runtime, Session},
    serde_json::json,
};
fn seed() -> [u8; 32] {
    std::array::from_fn(|i| (i + 1) as u8)
}
fn key() -> FarmerKey {
    FarmerKey::derive(&seed()).unwrap()
}
fn record_proof() -> Record {
    let mut p = Record::from_generated(&mmx::ProofOfSpaceOG {
        ksize: 32,
        seed: [7; 32].into(),
        farmer_key: key().public_key().into(),
        ..Default::default()
    })
    .unwrap();
    p.fields.insert(
        "plot_id".into(),
        Value::Bytes(proof::calc_plot_id(&p).unwrap().to_vec()),
    );
    p
}
#[test]
fn farmer_identity_matches_independent_cpp_kdf_and_signed_messages_verify() {
    assert_eq!(
        hex::encode(key().secret_bytes().as_slice()),
        "8fb3a130f85eee1969992d60242ba1f01c4ffd65d8b83d0b98587bc3beaee175"
    );
    let hash = [123; 32];
    let k = key();
    let sig = k.sign(&hash).unwrap();
    assert!(mmx_crypto::verify(&hash, &k.public_key(), &sig).unwrap());
    assert!(!mmx_crypto::verify(&[124; 32], &k.public_key(), &sig).unwrap());
    assert_eq!(sig, k.sign(&hash).unwrap());
}
#[test]
fn farmer_signs_native_envelopes_blocks_and_domain_separated_votes() {
    let f = Farmer::new([key()], Some(Address([9; 32])));
    let mut r = Record::from_generated(&mmx::ProofResponse {
        harvester: "test".into(),
        vdf_height: 42,
        ..Default::default()
    })
    .unwrap();
    r.fields
        .insert("proof".into(), Value::Record(record_proof()));
    r.fields.insert(
        "hash".into(),
        Value::Bytes(r.calc_hash(false, 0).unwrap().to_vec()),
    );
    r.fields.insert(
        "content_hash".into(),
        Value::Bytes(response::content_hash(&r).unwrap().to_vec()),
    );
    let signed = f.sign_response(&r).unwrap();
    response::authenticate(&signed).unwrap();
    assert!(response::is_valid(&signed).unwrap());
    let mut invalid = r.clone();
    invalid
        .fields
        .insert("hash".into(), Value::Bytes(vec![0; 32]));
    assert!(f.sign_response(&invalid).is_err());
    let params = mmx::ChainParams::default();
    let mut b = Record::from_generated(&mmx::BlockHeader::default()).unwrap();
    b.fields.insert(
        "proof".into(),
        Value::Sequence(vec![Value::Record(record_proof())]),
    );
    b.fields.insert(
        "reward_contract".into(),
        Value::Optional(Some(Box::new(Value::Bytes(vec![8; 32])))),
    );
    let signed = f.sign_block(&b, &params).unwrap();
    header::authenticate(&signed).unwrap();
    assert_eq!(
        signed.field("hash").unwrap().bytes().unwrap(),
        header::calc_hash(&signed, &params).unwrap()
    );
    assert_eq!(
        signed
            .field("reward_addr")
            .unwrap()
            .optional()
            .unwrap()
            .unwrap()
            .bytes()
            .unwrap(),
        [9; 32]
    );
    assert_eq!(
        signed
            .field("reward_account")
            .unwrap()
            .optional()
            .unwrap()
            .unwrap()
            .bytes()
            .unwrap(),
        [9; 32]
    );
    let mut v = Record::from_generated(&mmx::ValidatorVote {
        hash: [3; 32].into(),
        farmer_key: key().public_key().into(),
        ..Default::default()
    })
    .unwrap();
    v.fields.insert(
        "farmer_sig".into(),
        Value::Bytes(f.sign_vote(&v).unwrap().to_vec()),
    );
    v.fields.insert(
        "content_hash".into(),
        Value::Bytes(vote::content_hash(&v).unwrap().to_vec()),
    );
    assert!(vote::is_valid(&v).unwrap());
    assert!(Farmer::new([], None).sign_vote(&v).is_err());
    assert!(f
        .sign_block(
            &Record::from_generated(&mmx::BlockHeader::default()).unwrap(),
            &params
        )
        .is_err());
}
#[test]
fn delta_codes_cross_words_and_reject_truncation() {
    let mut data = vec![0; 512];
    let mut bit = 63;
    for s in 0..=48u8 {
        let index = s / 3;
        let rem = s % 3;
        let count = if s == 48 { 32 } else { 2 * index as usize + 2 };
        for i in 0..count {
            let value = if i < (2 * index as usize).min(32) {
                1
            } else {
                (rem >> (i - 2 * index as usize)) & 1
            };
            data[(bit + i) / 8] |= value << ((bit + i) % 8);
        }
        bit += count;
    }
    assert_eq!(
        encoding::decode(&data, 49, 63).unwrap(),
        (0..=48).collect::<Vec<_>>()
    );
    assert!(encoding::decode(&[255], 1, 0).is_err());
    assert!(encoding::read_bits(&[0], 1, 8).is_err());
    for (x, y) in [(1, 0), (255, 1), (u32::MAX, u32::MAX - 1)] {
        let t = u64::from(x) * u64::from(x - 1) / 2 + u64::from(y);
        assert_eq!(encoding::line_point(t, false).unwrap(), (x, y));
    }
    assert_eq!(encoding::line_point(1, true).unwrap(), (0, 0));
    assert!(encoding::line_point(u64::MAX, false).is_err());
}
#[test]
fn wallet_lock_timeout_retains_farmer_identity_and_addresses() {
    let mut a = Account::new(KeyFile::new(seed(), Some("correct")), 7, 3, "test".into()).unwrap();
    assert!(a.is_locked());
    assert!(a.addresses().is_empty());
    assert!(a.unlock("wrong").is_err());
    a.unlock("correct").unwrap();
    let addresses = a.addresses().to_vec();
    let k = a.farmer_key().unwrap().public_key();
    let sol = a
        .wallet()
        .unwrap()
        .sign_message(addresses[0], &[22; 32])
        .unwrap();
    assert!(mmx_crypto::verify(&[22; 32], &sol.pubkey, &sol.signature).unwrap());
    a.timeout = Duration::ZERO;
    assert!(a.is_locked());
    assert!(a.wallet().is_err());
    assert_eq!(a.addresses(), addresses);
    assert_eq!(a.farmer_key().unwrap().public_key(), k);
}
#[test]
fn vnx_wallet_rpc_enforces_permissions_and_never_exposes_private_service_on_lan() {
    let accounts = Arc::new(Mutex::new(BTreeMap::from([(
        0,
        Account::new(KeyFile::new(seed(), None), 0, 2, "test".into()).unwrap(),
    )])));
    let runtime = Runtime::new().unwrap();
    runtime
        .register("Wallet", service::wallet_methods(accounts))
        .unwrap();
    assert!(service::bind_private(
        "0.0.0.0:0".parse().unwrap(),
        runtime.clone(),
        service::private_options(["Wallet".into()])
    )
    .is_err());
    let mut server = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        runtime,
        mmx_types::vnx_registry().unwrap(),
        ServerOptions {
            services: ["Wallet".into()].into(),
            session: Session::new("read", ["mmx.permission_e.REMOTE".into()]),
            ..Default::default()
        },
    )
    .unwrap();
    let client = NetworkClient::connect(
        server.endpoint(),
        "Wallet",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let address = client
        .call_value("mmx.Wallet.get_address", json!({"index":0,"offset":0}))
        .unwrap();
    assert_eq!(address["_ret_0"].as_array().unwrap().len(), 32);
    assert!(client
        .call_value("mmx.Wallet.get_farmer_keys", json!({"index":0}))
        .is_err());
    assert!(client
        .call_value("mmx.Wallet.unlock", json!({"index":0,"passphrase":""}))
        .is_err());
    client.close();
    server.shutdown();
}
#[test]
fn farm_reports_replace_by_identity_expire_and_detect_overflow() {
    let mut f = Farmer::new([], None);
    let info = mmx::FarmInfo {
        harvester: Some("one".into()),
        total_bytes: 100,
        plot_count: [(32, 1)].into(),
        ..Default::default()
    };
    f.report(info.clone()).unwrap();
    f.report(info).unwrap();
    assert_eq!(f.farm_info().unwrap().total_bytes, 100);
    f.report(mmx::FarmInfo {
        harvester: Some("two".into()),
        total_bytes: u64::MAX,
        ..Default::default()
    })
    .unwrap();
    assert!(f.farm_info().is_err());
    f.harvester_timeout = Duration::ZERO;
    assert_eq!(f.farm_info().unwrap().total_bytes, 0);
}

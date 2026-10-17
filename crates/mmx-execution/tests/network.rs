use mmx_consensus::{Record, Value as V};
use mmx_execution::chain::{
    genesis,
    network::{self, Network},
    ChainState,
};
use mmx_types::{mmx, VnxType};
use std::{
    fs,
    path::{Path, PathBuf},
};
use vnx::{
    config::Config,
    serde_json::{self, Value},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/network")
}
fn bytes(value: &Value) -> Vec<u8> {
    vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()
}
fn record(value: &Value) -> Record {
    Record::decode(&bytes(value)).unwrap()
}
fn copy_dir(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target)
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn original_network_files_match_native_genesis_and_reopen() {
    let configs: Vec<Value> = include_str!("fixtures/network.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .filter(|r: &Value| r["kind"] == "config")
        .collect();
    for line in include_str!("fixtures/chain.jsonl").lines().take(2) {
        let row: Value = serde_json::from_str(line).unwrap();
        let name = row["name"].as_str().unwrap();
        let network = Network::load(root(), name).unwrap();
        assert_eq!(
            network.params,
            mmx::ChainParams::decode(&bytes(&row["params"])).unwrap()
        );
        let native_config = configs.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(
            network.params,
            mmx::ChainParams::decode(&bytes(&native_config["wire"])).unwrap()
        );
        for (binary, expected) in network
            .assets
            .binaries
            .iter()
            .zip(row["binaries"].as_array().unwrap())
        {
            assert_eq!(binary, &record(expected));
        }
        for (asset, expected) in [
            (&network.assets.testnet_rewards, &row["rewards"]),
            (&network.assets.project_relay, &row["relay"]),
        ] {
            assert_eq!(
                asset.as_ref(),
                (!expected.as_str().unwrap().is_empty())
                    .then(|| record(expected))
                    .as_ref()
            );
        }
        let block = genesis::build(&network.params, &network.assets).unwrap();
        assert_eq!(block, record(&row["block"]));
        let directory = tempfile::tempdir().unwrap();
        let mut chain = ChainState::open(directory.path(), network.params.clone()).unwrap();
        chain.initialize(&network.assets).unwrap();
        let tip = chain.tip().unwrap().unwrap();
        assert_eq!(
            tip.hash.0.as_slice(),
            block.field("hash").unwrap().bytes().unwrap()
        );
        drop(chain);
        let reopened = ChainState::open(directory.path(), network.params).unwrap();
        assert_eq!(reopened.tip().unwrap(), Some(tip));
    }
}

#[test]
fn layered_config_overrides_choose_the_configured_network() {
    for line in include_str!("fixtures/network.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["kind"] == "config" {
            let network = Network::load(root(), row["name"].as_str().unwrap()).unwrap();
            assert_eq!(
                network.params,
                mmx::ChainParams::decode(&bytes(&row["wire"])).unwrap()
            );
        }
    }
    let config = Config::new();
    config.load_tree(root().join("config/testnet13")).unwrap();
    config.set("chain.params.port", 12345.into()).unwrap();
    config
        .set("chain.params.hardfork2_height", 42.into())
        .unwrap();
    let network = Network::from_config(&config, root().join("data")).unwrap();
    assert_eq!(network.params.port, 12345);
    assert_eq!(network.params.hardfork2_height, 42);
    assert_eq!(network.params.network, "testnet13");
    assert_eq!(network.params.reward_adjust_tick, 10000); // unknown min_reward_adjust is ignored
    assert!(network.assets.testnet_rewards.is_none());
    assert!(network.assets.project_relay.is_none());
    assert!(network::load_params(&Config::new())
        .unwrap_err()
        .to_string()
        .contains("missing chain parameters"));
}

#[test]
fn invalid_config_cycles_and_unsupported_text_fail_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config/broken");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("parent"), "../broken/").unwrap();
    assert!(Network::load(directory.path(), "broken")
        .unwrap_err()
        .to_string()
        .contains("cycle"));
    fs::remove_file(config.join("parent")).unwrap();
    fs::create_dir(config.join("chain")).unwrap();
    fs::write(config.join("chain/params.json"), "{port:").unwrap();
    assert!(Network::load(directory.path(), "broken").is_err());
    let config = Config::new();
    config.set_text("chain.params", "{port:1.5}").unwrap();
    assert!(network::load_params(&config)
        .unwrap_err()
        .to_string()
        .contains("unsupported"));
}

#[test]
fn damaged_files_report_the_path_and_cannot_publish_genesis() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    copy_dir(&root().join("data/testnet13"), &data);
    let network = Network::load(root(), "testnet13").unwrap();
    let path = data.join(network::BINARY_FILES[0]);
    let original = fs::read(&path).unwrap();
    let mut damaged = Record::decode(&original).unwrap();
    damaged
        .fields
        .insert("content_hash".into(), V::Bytes(vec![0; 32]));
    fs::write(&path, damaged.encode().unwrap()).unwrap();
    let assets = network::load_assets(&data, "testnet13").unwrap();
    let mut chain = ChainState::memory(network.params);
    assert!(chain.initialize(&assets).is_err());
    assert!(chain.tip().unwrap().is_none());
    assert_eq!(chain.storage().database().version(), 0);
    assert!(!chain.storage().database().has_pending());
    for data_bytes in [&original[..original.len() - 1], &[0u8][..]] {
        fs::write(&path, data_bytes).unwrap();
        assert!(network::load_assets(&data, "testnet13")
            .unwrap_err()
            .to_string()
            .contains(network::BINARY_FILES[0]));
    }
    fs::write(
        &path,
        Record::from_generated(&mmx::ChainParams::default())
            .unwrap()
            .encode()
            .unwrap(),
    )
    .unwrap();
    assert!(network::load_assets(&data, "testnet13")
        .unwrap_err()
        .to_string()
        .contains("expected mmx.Transaction"));
    fs::remove_file(&path).unwrap();
    assert!(network::load_assets(&data, "testnet13")
        .unwrap_err()
        .to_string()
        .contains(network::BINARY_FILES[0]));
}

#[test]
fn mainnet_requires_extra_files_and_other_networks_ignore_them() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path();
    copy_dir(&root().join("data/mainnet"), data);
    let rewards = data.join("tx_testnet_rewards.json");
    fs::write(&rewards, "{broken").unwrap();
    assert!(network::load_assets(data, "mainnet")
        .unwrap_err()
        .to_string()
        .contains("tx_testnet_rewards.json"));
    assert!(network::load_assets(data, "testnet13")
        .unwrap()
        .testnet_rewards
        .is_none());
    fs::copy(
        root().join("data/mainnet/tx_testnet_rewards.json"),
        &rewards,
    )
    .unwrap();
    fs::remove_file(data.join("tx_project_relay.dat")).unwrap();
    assert!(network::load_assets(data, "mainnet")
        .unwrap_err()
        .to_string()
        .contains("tx_project_relay.dat"));
}

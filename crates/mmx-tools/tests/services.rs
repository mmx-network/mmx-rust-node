#![cfg(unix)]
use mmx_consensus::{proof, response, Record};
use mmx_farming::service;
use mmx_types::{mmx, VnxType};
use mmx_wallet::{FarmerKey, KeyFile};
use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use vnx::{
    net::{NetworkClient, Server, ServerOptions},
    runtime::{Methods, OverflowPolicy, Runtime, Session},
    serde_json::{self, json, Value},
};

// Every signal is restricted to a child started here and rechecks its complete
// /proc identity. No project daemon, real wallet or node data is used.
struct Daemon {
    child: Child,
    exe: PathBuf,
    cwd: PathBuf,
    argv: Vec<u8>,
    start: String,
    logs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
fn start_time(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
impl Daemon {
    fn start(exe: &str, args: &[OsString], cwd: &Path) -> (Self, String) {
        let (daemon, line) = Self::start_line(exe, args, cwd);
        (daemon, line.split_whitespace().last().unwrap().into())
    }
    fn start_line(exe: &str, args: &[OsString], cwd: &Path) -> (Self, String) {
        Self::start_line_env(exe, args, cwd, &[])
    }
    fn start_line_env(
        exe: &str,
        args: &[OsString],
        cwd: &Path,
        environment: &[(&str, &Path)],
    ) -> (Self, String) {
        use std::os::unix::ffi::OsStrExt;
        let exe = fs::canonicalize(exe).unwrap();
        let cwd = fs::canonicalize(cwd).unwrap();
        let mut argv = exe.as_os_str().as_bytes().to_vec();
        argv.push(0);
        for a in args {
            argv.extend(a.as_bytes());
            argv.push(0);
        }
        let mut command = Command::new(&exe);
        command
            .args(args)
            .current_dir(&cwd)
            .env("RAYON_NUM_THREADS", "1")
            .env("MMX_HOME", &cwd)
            .env_remove("MMX_DATA")
            .env_remove("MMX_NETWORK");
        for (key, value) in environment {
            command.env(key, value);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = start_time(child.id()).unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = logs.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = reader.read_line(&mut line);
            captured.lock().unwrap().push(line.clone());
            let _ = tx.send((result, line));
            for line in reader.lines().map_while(std::result::Result::ok) {
                let mut logs = captured.lock().unwrap();
                if logs.len() >= 128 {
                    logs.remove(0);
                }
                logs.push(line);
            }
        });
        let daemon = Self {
            child,
            exe,
            cwd,
            argv,
            start,
            logs,
        };
        let (result, line) = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("daemon startup timed out");
        assert!(result.unwrap() > 0, "daemon exited during startup");
        (daemon, line)
    }
    fn line(&self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(line) = self
                .logs
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.starts_with(prefix))
                .cloned()
            {
                return line;
            }
            assert!(Instant::now() < deadline, "missing startup line {prefix}");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn owns(&self) -> bool {
        let p = format!("/proc/{}", self.child.id());
        fs::read_link(format!("{p}/exe")).ok().as_ref() == Some(&self.exe)
            && fs::read_link(format!("{p}/cwd")).ok().as_ref() == Some(&self.cwd)
            && fs::read(format!("{p}/cmdline")).ok().as_ref() == Some(&self.argv)
            && start_time(self.child.id()).as_ref() == Some(&self.start)
    }
    fn shutdown_successfully(mut self) {
        assert!(self.owns());
        assert!(Command::new("kill")
            .args(["-s", "INT", &self.child.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "daemon shutdown failed: {status}");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not shut down promptly"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        if !self.owns() {
            return;
        }
        let status = Command::new("kill")
            .args(["-s", "INT", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if self.owns() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn client(endpoint: &str, name: &str) -> NetworkClient {
    NetworkClient::connect(
        &endpoint.parse().unwrap(),
        name,
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap()
}
#[test]
fn actual_wallet_daemon_and_mmx_utility_share_the_native_wallet_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let key = KeyFile::new([1; 32], None);
    let path = dir.path().join("wallet.dat");
    fs::write(&path, key.encode()).unwrap();
    let (_daemon, endpoint) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-wallet"),
        &[
            "--key-file".into(),
            path.into_os_string(),
            "--num-addresses".into(),
            "2".into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
        ],
        dir.path(),
    );
    let client = client(&endpoint, "Wallet");
    assert_eq!(
        client
            .call_value("mmx.Wallet.is_locked", json!({"index":0}))
            .unwrap()["_ret_0"],
        true
    );
    client
        .call_value("mmx.Wallet.unlock", json!({"index":0,"passphrase":""}))
        .unwrap();
    let address = client
        .call_value("mmx.Wallet.get_address", json!({"index":0,"offset":1}))
        .unwrap()["_ret_0"]
        .clone();
    let output = Command::new(env!("CARGO_BIN_EXE_mmx"))
        .args([
            "wallet-rpc",
            "get_address",
            "--endpoint",
            &endpoint,
            "--args",
            "{\"index\":0,\"offset\":1}",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        address
    );
    client.close();
}
#[test]
fn actual_farmer_and_harvester_deliver_a_consensus_valid_signed_cpp_plot_proof() {
    let f: Value =
        serde_json::from_str(include_str!("../../mmx-farming/tests/fixtures.json")).unwrap();
    let mut params = mmx::ChainParams::decode(
        &vnx::util::from_hex_string(f["proof"]["params"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    params.network = "test".into();
    let native = service::to_json(&Record::from_generated(&params).unwrap()).unwrap();
    let runtime = Runtime::new().unwrap();
    let mut methods = Methods::new();
    methods.add("mmx.Node.get_params", None, move |_, _| Ok(native.clone()));
    runtime.register("Node", methods).unwrap();
    let received = runtime
        .subscribe("farmer.proof", 8, OverflowPolicy::Reject, false)
        .unwrap();
    let mut node = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        runtime.clone(),
        mmx_types::vnx_registry().unwrap(),
        ServerOptions {
            services: ["Node".into()].into(),
            export_topics: ["harvester.challenges".into()].into(),
            publish_topics: ["farmer.proof".into()].into(),
            session: Session::new(
                "test",
                [
                    "vnx.permission_e.VIEW".into(),
                    "vnx.permission_e.PUBLISH".into(),
                ],
            ),
            ..Default::default()
        },
    )
    .unwrap();
    let wallet = Runtime::new().unwrap();
    let mut methods = Methods::new();
    let mut scalar = [0; 32];
    scalar[31] = 1;
    let key = FarmerKey::from_bytes(&scalar).unwrap();
    let public = key.public_key();
    methods.add(
        "mmx.Wallet.get_all_farmer_keys",
        Some("mmx.permission_e.SPENDING".into()),
        move |_, _| Ok(json!([[scalar, public.as_slice()]])),
    );
    methods.add("mmx.Wallet.get_all_accounts", None, |_, _| {
        Ok(serde_json::to_value(vec![mmx::account_info_t {
            address: Some([7; 32].into()),
            ..Default::default()
        }])?)
    });
    wallet.register("Wallet", methods).unwrap();
    let auth = vnx::auth::AuthenticationServer::new();
    auth.add_user(vnx::auth::User {
        name: "farmer-test".into(),
        hashed_password: vnx::auth::final_hash(&vnx::auth::password_hash("wallet-test-password")),
        permissions: ["mmx.permission_e.SPENDING".into()].into(),
        ..Default::default()
    })
    .unwrap();
    let mut wallet = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        wallet,
        mmx_types::vnx_registry().unwrap(),
        ServerOptions {
            services: ["Wallet".into()].into(),
            authenticator: Some(std::sync::Arc::new(move |name, hash| {
                auth.authenticate_hash(name, hash)
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let unauthorized = client(&wallet.endpoint().to_string(), "Wallet");
    assert!(unauthorized
        .call_value("mmx.Wallet.get_all_farmer_keys", json!({}))
        .is_err());
    unauthorized.close();
    let dir = tempfile::tempdir().unwrap();
    let password = dir.path().join("wallet-password");
    fs::write(&password, b"wallet-test-password\n").unwrap();
    let plot_dir = dir.path().join("plots");
    fs::create_dir(&plot_dir).unwrap();
    fs::write(
        plot_dir.join("native.plot"),
        include_bytes!("../../mmx-farming/tests/native/native-0.plot"),
    )
    .unwrap();
    let (farmer, farmer_line) = Daemon::start_line(
        env!("CARGO_BIN_EXE_mmx-farmer"),
        &[
            "--wallet".into(),
            wallet.endpoint().to_string().into(),
            "--wallet-username".into(),
            "farmer-test".into(),
            "--wallet-password-file".into(),
            password.into_os_string(),
            "--node".into(),
            node.endpoint().to_string().into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
            "--harvester-listen".into(),
            "0.0.0.0:0".into(),
        ],
        dir.path(),
    );
    let farmer_endpoint = farmer_line
        .split_whitespace()
        .nth(3)
        .unwrap()
        .replace("0.0.0.0:", "127.0.0.1:");
    let remote = client(&farmer_endpoint, "Farmer");
    assert!(remote
        .call_value(
            "mmx.Farmer.sign_vote",
            json!({"vote":{"__type":"mmx.ValidatorVote"}})
        )
        .is_err());
    remote.close();
    let config = dir.path().join("config");
    fs::create_dir(&config).unwrap();
    fs::write(
        config.join("node"),
        serde_json::to_vec(&farmer_endpoint).unwrap(),
    )
    .unwrap();
    fs::write(config.join("Harvester.json"),serde_json::to_vec(&json!({"plot_dirs":[plot_dir],"num_threads":1,"my_name":"remote fixture","reload_interval":0})).unwrap()).unwrap();
    let (harvester, _) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-harvester"),
        &[
            "-c".into(),
            config.into_os_string(),
            "--listen".into(),
            "127.0.0.1:0".into(),
            "--threads".into(),
            "1".into(),
        ],
        dir.path(),
    );
    let challenge = mmx::Challenge {
        vdf_height: f["proof"]["height"].as_u64().unwrap() as u32,
        challenge: vnx::util::from_hex_string(f["proof"]["challenge"].as_str().unwrap())
            .unwrap()
            .try_into()
            .map(vnx::Fixed)
            .unwrap(),
        difficulty: 1,
    };
    runtime
        .publish("harvester.challenges", challenge.to_value().unwrap(), false)
        .unwrap();
    let signed = service::from_json(
        &received
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .value,
    )
    .unwrap();
    assert!(response::is_valid(&signed).unwrap());
    response::authenticate(&signed).unwrap();
    proof::verify(
        signed.field("proof").unwrap().record().unwrap(),
        &params,
        &challenge.challenge.0,
        challenge.difficulty,
        challenge.vdf_height,
    )
    .unwrap();
    drop(harvester);
    drop(farmer);
    wallet.shutdown();
    node.shutdown();
}

#[test]
fn actual_wallet_spends_through_native_node_and_reserves_pending_balances() {
    use mmx_consensus::{Hash, Value as N};
    use mmx_execution::chain::{forks::ForkChain, genesis::Assets, network::Network};
    use mmx_node::{
        node::hash,
        rpc,
        sync::{self, Source},
        Node,
    };
    use mmx_vm::Storage;
    use mmx_wallet::Wallet;
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let seed = [71; 32];
    let keys = Wallet::derive(&seed, "protected", 0, 2).unwrap();
    let sender = keys.addresses[0].0;
    let recipient = keys.addresses[1].0;
    let keyfile = dir.path().join("test-wallet.dat");
    fs::write(&keyfile, KeyFile::new(seed, Some("protected")).encode()).unwrap();
    let g: Value = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|r| r["name"] == "synthetic")
        .unwrap();
    let decode = |v: &Value| {
        Record::decode(&vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()).unwrap()
    };
    let mut bins: Vec<_> = g["binaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode)
        .collect();
    let tx = bins.last_mut().unwrap();
    tx.fields.insert(
        "outputs".into(),
        N::Sequence(vec![N::Record(
            Record::from_generated(&mmx::txout_t {
                address: sender.into(),
                amount: 30_000_000_u128.to_le_bytes().into(),
                ..Default::default()
            })
            .unwrap(),
        )]),
    );
    tx.fields.insert(
        "id".into(),
        N::Bytes(tx.calc_hash(false, 0).unwrap().to_vec()),
    );
    tx.fields.insert(
        "content_hash".into(),
        N::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    let params = mmx::ChainParams::decode(
        &vnx::util::from_hex_string(g["params"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let contract = hash(&bins[1], "id").unwrap();
    let network = Network {
        params,
        assets: Assets {
            binaries: bins.try_into().unwrap(),
            testnet_rewards: None,
            project_relay: None,
        },
    };
    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets).unwrap();
    let genesis = chain
        .block(&chain.selected().hash_at(0).unwrap().unwrap())
        .unwrap()
        .unwrap();
    let node = Arc::new(Mutex::new(
        Node::from_chain(chain, k256_for_test()).unwrap(),
    ));
    struct Genesis(Record);
    impl Source for Genesis {
        fn height(&mut self) -> mmx_node::Result<u32> {
            Ok(0)
        }
        fn hash_at(&mut self, h: u32) -> mmx_node::Result<Option<Hash>> {
            Ok(if h == 0 {
                Some(hash(&self.0, "hash")?)
            } else {
                None
            })
        }
        fn block_at(&mut self, h: u32) -> mmx_node::Result<Option<Record>> {
            Ok((h == 0).then(|| self.0.clone()))
        }
    }
    sync::synchronize_pipelined(
        &mut Genesis(genesis),
        &node,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let mut server = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), node.clone()).unwrap();
    let args = [
        "--key-file".into(),
        keyfile.into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
        "--num-addresses".into(),
        "2".into(),
        "--node".into(),
        server.endpoint().to_string().into(),
    ];
    let (_daemon, endpoint) = Daemon::start(env!("CARGO_BIN_EXE_mmx-wallet"), &args, dir.path());
    let wallet = client(&endpoint, "Wallet");
    let call = |name: &str, args: Value| {
        wallet
            .call_value(&format!("mmx.Wallet.{name}"), args)
            .map(|r| r["_ret_0"].clone())
    };
    let options = json!({"nonce":500,"passphrase":"protected","gas_limit":100_000});
    let args = json!({"index":0,"amount":1000_u128.to_le_bytes(),"currency":([0u8;32]),"dst_addr":recipient,"options":options});
    let sent = service::from_json(&call("send", args.clone()).unwrap()).unwrap();
    assert_eq!(
        call("is_locked", json!({"index":0})).unwrap(),
        true,
        "temporary passphrase must re-lock"
    );
    assert_eq!(node.lock().unwrap().pending_transactions(), 1);
    assert_eq!(
        node.lock()
            .unwrap()
            .chain()
            .selected()
            .storage()
            .get_balance(&sender, &[0; 32])
            .unwrap(),
        Some(30_000_000)
    );
    let balance = call("get_balance", json!({"index":0,"currency":([0u8;32])})).unwrap();
    let total: u128 = u128::from_le_bytes(
        serde_json::from_value::<vnx::Fixed<u8, 16>>(balance["total"].clone())
            .unwrap()
            .0,
    );
    let available: u128 = u128::from_le_bytes(
        serde_json::from_value::<vnx::Fixed<u8, 16>>(balance["spendable"].clone())
            .unwrap()
            .0,
    );
    assert_eq!(total, 30_000_000);
    assert!(available < total - 1000);
    let id = hash(&sent, "id").unwrap();
    let n = client(&server.endpoint().to_string(), "Node");
    assert!(!n
        .call_value("mmx.Node.get_transaction", json!({"id":id,"pending":true}))
        .unwrap()["_ret_0"]
        .is_null());
    assert!(n
        .call_value("mmx.Node.get_transaction", json!({"id":id,"pending":false}))
        .unwrap()["_ret_0"]
        .is_null());
    // Explicit repeat submission is idempotent in node and wallet reservations.
    wallet
        .call_value(
            "mmx.Wallet.unlock",
            json!({"index":0,"passphrase":"protected"}),
        )
        .unwrap();
    call(
        "send_off",
        json!({"index":0,"tx":service::to_json(&sent).unwrap()}),
    )
    .unwrap();
    assert_eq!(
        call("get_balance", json!({"index":0,"currency":([0u8;32])})).unwrap(),
        balance
    );
    let mut preview = args.clone();
    preview["options"]["auto_send"] = json!(false);
    preview["options"]["nonce"] = json!(501);
    let preview = service::from_json(&call("send", preview).unwrap()).unwrap();
    assert!(preview
        .field("exec_result")
        .unwrap()
        .optional()
        .unwrap()
        .is_some());
    assert_eq!(node.lock().unwrap().pending_transactions(), 1);
    let completed=call("complete",json!({"index":0,"tx":{"__type":"mmx.Transaction","outputs":[{"address":recipient,"amount":10_u128.to_le_bytes()}]},"options":{"gas_limit":100_000}})).unwrap();
    assert!(completed["exec_result"].is_null());
    let signed = call("sign_off", json!({"index":0,"tx":completed,"options":{}})).unwrap();
    assert_eq!(signed, completed);
    assert_eq!(
        node.lock().unwrap().pending_transactions(),
        1,
        "complete must not submit"
    );
    let execute=call("execute",json!({"index":0,"address":contract,"method":"run","args":[1],"user":0,"options":{"auto_send":false,"gas_limit":100_000}})).unwrap();
    assert!(!execute["exec_result"]["did_fail"].as_bool().unwrap());
    let deposit=call("deposit",json!({"index":0,"address":contract,"method":"run","args":[2],"amount":100_u128.to_le_bytes(),"currency":([0u8;32]),"options":{"auto_send":false,"gas_limit":100_000}})).unwrap();
    assert!(!deposit["exec_result"]["did_fail"].as_bool().unwrap());
    let deploy=call("deploy",json!({"index":0,"contract":{"__type":"mmx.contract.Data","value":{"example":123}},"options":{"auto_send":false,"gas_limit":100_000}})).unwrap();
    assert!(!deploy["exec_result"]["did_fail"].as_bool().unwrap());
    let many=call("send_many",json!({"index":0,"amounts":[[recipient,10_u128.to_le_bytes()],[recipient,20_u128.to_le_bytes()]],"currency":([0u8;32]),"options":{"auto_send":false,"gas_limit":100_000}})).unwrap();
    assert_eq!(many["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(node.lock().unwrap().pending_transactions(), 1);
    // Existing source inputs must respect the same reservation-adjusted balance.
    let too_much = json!({"index":0,"amount":30_000_000_u128.to_le_bytes(),"src_addr":sender,"dst_addr":recipient,"currency":([0u8;32]),"options":options});
    assert!(call("send_from", too_much).is_err());
    call("lock", json!({"index":0})).unwrap();
    let mut invalid = args.clone();
    invalid["amount"] = json!(30_000_000_u128.to_le_bytes());
    assert!(call("send", invalid).is_err());
    assert_eq!(
        call("is_locked", json!({"index":0})).unwrap(),
        true,
        "failed temporary spend must also relock"
    );
    let mut bad = args;
    bad["options"]["passphrase"] = json!("wrong");
    assert!(call("send", bad).is_err());
    assert_eq!(call("is_locked", json!({"index":0})).unwrap(), true);
    n.close();
    wallet.close();
    server.shutdown();
}

fn k256_for_test() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[1; 32]).unwrap()
}

fn synthetic_network() -> (mmx_execution::chain::network::Network, Value, Vec<Value>) {
    use mmx_execution::chain::{genesis::Assets, network::Network};
    let rows: Vec<Value> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let g = rows
        .iter()
        .find(|r| r["name"] == "synthetic")
        .unwrap()
        .clone();
    let steps = rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    let decode = |v: &Value| {
        Record::decode(&vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()).unwrap()
    };
    let network = Network {
        params: mmx::ChainParams::decode(
            &vnx::util::from_hex_string(g["params"].as_str().unwrap()).unwrap(),
        )
        .unwrap(),
        assets: Assets {
            binaries: g["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(decode)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            testnet_rewards: None,
            project_relay: None,
        },
    };
    (network, g, steps)
}
fn synced_test_node(
    network: &mmx_execution::chain::network::Network,
    identity: u8,
) -> std::sync::Arc<std::sync::Mutex<mmx_node::Node>> {
    use mmx_execution::chain::forks::ForkChain;
    use mmx_node::{
        node::hash,
        sync::{self, Source},
        Node,
    };
    use std::sync::{atomic::AtomicBool, Arc, Mutex};
    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets).unwrap();
    let genesis = chain
        .block(&chain.selected().hash_at(0).unwrap().unwrap())
        .unwrap()
        .unwrap();
    struct Genesis(Record);
    impl Source for Genesis {
        fn height(&mut self) -> mmx_node::Result<u32> {
            Ok(0)
        }
        fn hash_at(&mut self, h: u32) -> mmx_node::Result<Option<[u8; 32]>> {
            Ok((h == 0).then(|| hash(&self.0, "hash").unwrap()))
        }
        fn block_at(&mut self, h: u32) -> mmx_node::Result<Option<Record>> {
            Ok((h == 0).then(|| self.0.clone()))
        }
    }
    let node = Arc::new(Mutex::new(
        Node::from_chain(
            chain,
            k256::ecdsa::SigningKey::from_slice(&[identity; 32]).unwrap(),
        )
        .unwrap(),
    ));
    sync::synchronize_pipelined(
        &mut Genesis(genesis),
        &node,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    node
}

#[test]
fn timelord_daemon_uses_native_intervals_and_only_verified_work_advances_the_node() {
    use mmx_node::rpc;
    let (mut network, _, _) = synthetic_network();
    network.params.vdf_segment_size = 100_000;
    network.params.initial_time_diff = 1;
    network.params.time_diff_divider = 1;
    network.params.time_diff_constant = 200_000;
    network.params.max_vdf_count = 4;
    network.params.infuse_delay = 1;
    let state = synced_test_node(&network, 17);
    let requests = state.lock().unwrap().timelord_requests().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].input.is_some());
    assert!(requests[1].input.is_none());
    let mut node = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), state.clone()).unwrap();
    let observer = client(&node.endpoint().to_string(), "Node");
    let mut secret = [0; 32];
    secret[31] = 2;
    let forged = mmx_timelord::identity::Identity::from_bytes(&secret)
        .unwrap()
        .sign(mmx::ProofOfTime {
            vdf_height: 1,
            start: requests[0].start,
            num_iters: requests[0].end - requests[0].start,
            segment_size: network.params.vdf_segment_size,
            input: requests[0].input.clone().unwrap(),
            prev: requests[0].infuse.clone(),
            segments: vec![[0; 32].into(); 2],
            ..Default::default()
        })
        .unwrap();
    observer
        .publish("timelord.proof", forged.to_value().unwrap())
        .unwrap();
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(20));
        state.lock().unwrap().tick().unwrap();
    }
    assert_eq!(
        state
            .lock()
            .unwrap()
            .call(&mmx_node::node::record("mmx.Node.get_vdf_height", []))
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .uint()
            .unwrap(),
        0
    );
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    fs::create_dir(&config).unwrap();
    fs::write(config.join("TimeLord.json"),json!({"max_history":4096,"cpu":"auto","reward_addr":mmx_wallet::Address([7;32]).to_string()}).to_string()).unwrap();
    let storage = dir.path().join("identity");
    let args = vec![
        "-c".into(),
        config.into_os_string(),
        "--node".into(),
        node.endpoint().to_string().into(),
        "--reward-addr".into(),
        "MMX".into(),
        "--storage-path".into(),
        storage.clone().into_os_string(),
        "--listen".into(),
        "127.0.0.1:0".into(),
    ];
    let (daemon, _) = Daemon::start_line(env!("CARGO_BIN_EXE_mmx-timelord"), &args, dir.path());
    let endpoint = daemon
        .line("TimeLord VNX RPC ")
        .split_whitespace()
        .last()
        .unwrap()
        .to_string();
    let control = client(&endpoint, "TimeLord");
    assert!(observer
        .call_value("mmx.TimeLord.stop_vdf", json!({}))
        .is_err());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut n = state.lock().unwrap();
        n.tick().unwrap();
        let reply = n
            .call(&mmx_node::node::record("mmx.Node.get_vdf_height", []))
            .unwrap();
        let peak = reply.field("_ret_0").unwrap().uint().unwrap();
        if peak >= 1 {
            break;
        }
        drop(n);
        assert!(
            Instant::now() < deadline,
            "timelord work never advanced the verified VDF peak"
        );
        thread::sleep(Duration::from_millis(20));
    }
    control
        .call_value("mmx.TimeLord.stop_vdf", json!({}))
        .unwrap();
    let key = fs::read(storage.join("timelord_sk.dat")).unwrap();
    assert_eq!(key.len(), 42);
    control.close();
    daemon.shutdown_successfully();
    let (alias, _) = Daemon::start_line(env!("CARGO_BIN_EXE_mmx_timelord"), &args, dir.path());
    alias.line("TimeLord VNX RPC ");
    assert_eq!(fs::read(storage.join("timelord_sk.dat")).unwrap(), key);
    alias.shutdown_successfully();
    observer.close();
    node.shutdown();
}

#[test]
fn remote_plot_to_farmer_to_rust_node_produces_a_live_block() {
    use mmx_node::{production::Worker, rpc};
    use std::sync::{atomic::AtomicBool, Arc};
    let (network, _, steps) = synthetic_network();
    let state = synced_test_node(&network, 7);
    let mut node = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), state.clone()).unwrap();
    let runtime = Runtime::new().unwrap();
    let mut methods = Methods::new();
    let mut scalar = [0; 32];
    scalar[31] = 1;
    let key = FarmerKey::from_bytes(&scalar).unwrap();
    let public = key.public_key();
    methods.add("mmx.Wallet.get_all_farmer_keys", None, move |_, _| {
        Ok(json!([[scalar, public.as_slice()]]))
    });
    methods.add("mmx.Wallet.get_all_accounts", None, |_, _| {
        Ok(json!([{"address":([7u8;32])}]))
    });
    runtime.register("Wallet", methods).unwrap();
    let mut wallet = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        runtime,
        mmx_types::vnx_registry().unwrap(),
        ServerOptions {
            services: ["Wallet".into()].into(),
            ..Default::default()
        },
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let plots = dir.path().join("plots");
    fs::create_dir(&plots).unwrap();
    fs::write(
        plots.join("native.plot"),
        include_bytes!("../../mmx-farming/tests/native/native-0.plot"),
    )
    .unwrap();
    let (farmer, line) = Daemon::start_line(
        env!("CARGO_BIN_EXE_mmx-farmer"),
        &[
            "--wallet".into(),
            wallet.endpoint().to_string().into(),
            "--node".into(),
            node.endpoint().to_string().into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
            "--harvester-listen".into(),
            "0.0.0.0:0".into(),
        ],
        dir.path(),
    );
    let private = line.split_whitespace().last().unwrap();
    let remote = line
        .split_whitespace()
        .nth(3)
        .unwrap()
        .replace("0.0.0.0:", "127.0.0.1:");
    let (harvester, _) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-harvester"),
        &[
            "--farmer".into(),
            remote.into(),
            "--plot-dir".into(),
            plots.into_os_string(),
            "--threads".into(),
            "1".into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
        ],
        dir.path(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let mut worker = Worker::start(state.clone(), private.parse().unwrap(), stop, 0).unwrap();
    state
        .lock()
        .unwrap()
        .receive(
            &Record::decode(
                &vnx::util::from_hex_string(steps[0]["vdf"].as_str().unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while state
        .lock()
        .unwrap()
        .chain()
        .selected()
        .tip()
        .unwrap()
        .unwrap()
        .height
        == 0
    {
        assert!(
            Instant::now() < deadline,
            "remote harvester did not produce a block"
        );
        state.lock().unwrap().tick().unwrap();
        thread::sleep(Duration::from_millis(20));
    }
    let guard = state.lock().unwrap();
    let tip = guard.chain().selected().tip().unwrap().unwrap();
    let block = guard.chain().block(&tip.hash.0).unwrap().unwrap();
    assert!(mmx_consensus::header::is_valid(&block, &network.params).unwrap());
    mmx_consensus::header::authenticate(&block).unwrap();
    assert_eq!(tip.height, 1);
    drop(guard);
    worker.shutdown();
    drop(harvester);
    drop(farmer);
    wallet.shutdown();
    node.shutdown();
}

#[test]
fn node_starts_configured_wallet_farmer_remote_harvester_and_timelord_services() {
    configured_services(false, false, false);
}

#[test]
fn mmx_home_auto_loads_native_layers_and_passes_wallet_and_network_paths_to_children() {
    configured_services(true, false, false);
}

#[test]
fn explicit_network_and_data_dir_override_home_selectors_and_configured_database_path() {
    configured_services(true, true, false);
}

#[test]
fn native_router_file_with_local_overrides_starts_a_configured_listener_and_fixed_only_sync() {
    configured_services(false, false, true);
}

#[test]
fn standalone_wallet_discovers_home_and_role_config_with_cli_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let home = root.join("wallet home");
    let installation = root.join("installation");
    for path in [
        home.join("config/local"),
        home.join("keys"),
        installation.join("config/chosen"),
        installation.join("config/wallet"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(home.join("NETWORK"), "chosen\n").unwrap();
    fs::write(
        installation.join("config/chosen/Wallet.json"),
        r#"{"num_addresses":7,"key_files":["missing.dat"]}"#,
    )
    .unwrap();
    fs::write(
        installation.join("config/wallet/Wallet.json"),
        r#"{"storage_path":"keys"}"#,
    )
    .unwrap();
    let wrong_listener = root.join("must-not-be-created.sock");
    fs::write(
        home.join("config/local/Wallet.json"),
        json!({"key_files":["wallet.dat"],"listen":format!("unix://{}",wrong_listener.display())})
            .to_string(),
    )
    .unwrap();
    fs::write(
        home.join("keys/wallet.dat"),
        KeyFile::new([91; 32], None).encode(),
    )
    .unwrap();
    let endpoint = format!("unix://{}", root.join("wallet.sock").display());
    let (daemon, _) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-wallet"),
        &[
            "--home".into(),
            home.into_os_string(),
            "--mmx-root".into(),
            installation.into_os_string(),
            "--listen".into(),
            endpoint.clone().into(),
            "--num-addresses".into(),
            "2".into(),
        ],
        root,
    );
    let wallet = client(&endpoint, "Wallet");
    assert_eq!(
        wallet
            .call_value("mmx.Wallet.get_all_addresses", json!({"index":0}))
            .unwrap()["_ret_0"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!wrong_listener.exists());
    wallet.close();
    daemon.shutdown_successfully();
}

fn configured_services(from_home: bool, overrides: bool, router_overrides: bool) {
    use mmx_execution::chain::network::BINARY_FILES;
    let (mut network, _, _) = synthetic_network();
    network.params.vdf_segment_size = 100_000;
    network.params.initial_time_diff = 1;
    network.params.time_diff_divider = 1;
    network.params.time_diff_constant = 200_000;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let home = if from_home {
        root.join("separate home")
    } else {
        root.to_owned()
    };
    let base = root.join("config").join(if from_home {
        "selected-network"
    } else {
        "base"
    });
    let local = home.join("config/local");
    let state = root.join(if overrides {
        "explicit network directory"
    } else {
        "state disk/selected-network"
    });
    let data = root.join("data").join(&network.params.network);
    for path in [&base, &local, &data] {
        fs::create_dir_all(path).unwrap();
    }
    if from_home {
        fs::write(
            home.join("NETWORK"),
            if overrides {
                "../invalid-selector\n"
            } else {
                "selected-network\n"
            },
        )
        .unwrap();
        fs::write(
            local.join("MMX_DATA"),
            root.join("state disk").to_str().unwrap(),
        )
        .unwrap();
        // Network, role, local order: the role supplies wallet storage while
        // local overrides the network's address count.
        let role = root.join("config/node");
        fs::create_dir_all(&role).unwrap();
        fs::create_dir_all(home.join("keys")).unwrap();
        fs::write(role.join("Wallet.json"), r#"{"storage_path":"keys"}"#).unwrap();
        fs::write(base.join("Wallet.json"), r#"{"num_addresses":7}"#).unwrap();
        if overrides {
            fs::write(
                local.join("Node.json"),
                r#"{"database_path":"configured-ledger"}"#,
            )
            .unwrap();
        }
    }
    fs::write(
        base.join("chain.json"),
        serde_json::to_vec(&json!({"params":network.params})).unwrap(),
    )
    .unwrap();
    for (name, tx) in BINARY_FILES.iter().zip(&network.assets.binaries) {
        fs::write(data.join(name), tx.encode().unwrap()).unwrap();
    }
    let mut peers = vec![];
    for id in 8..11 {
        peers.push(
            mmx_node::peer::Server::bind("127.0.0.1:0", synced_test_node(&network, id)).unwrap(),
        );
    }
    if router_overrides {
        fs::write(
            base.join("Router.json"),
            include_str!("../../mmx-node/tests/fixtures/router-default.json"),
        )
        .unwrap();
        fs::write(
            local.join("Router.json"),
            json!({
                "host":"127.0.0.1", "port":0, "max_connections":4,
                "num_peers_out":1, "min_sync_peers":1,
                "fixed_peers":[peers[0].address().to_string()]
            })
            .to_string(),
        )
        .unwrap();
    } else {
        fs::write(
            base.join("Router.json"),
            serde_json::to_vec(
                &json!({"seed_peers":peers.iter().map(|p|p.address().to_string()).collect::<Vec<_>>()}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    let endpoint = |name: &str| format!("unix://{}", root.join(name).display());
    let wallet = endpoint("wallet.sock");
    let farmer = endpoint("farmer.sock");
    let harvesters = endpoint("remote.sock");
    let harvester = endpoint("harvester.sock");
    let timelord = endpoint("timelord.sock");
    fs::write(local.join("timelord"), "true").unwrap();
    fs::write(
        local.join("TimeLord.json"),
        json!({"listen":timelord,"storage_path":if from_home { PathBuf::from("timelord-identity") } else {root.join("timelord-identity")},"max_history":4096})
            .to_string(),
    )
    .unwrap();
    fs::write(
        if from_home {
            home.join("keys/wallet.dat")
        } else {
            root.join("wallet.dat")
        },
        KeyFile::new([83; 32], None).encode(),
    )
    .unwrap();
    fs::write(
        local.join("Wallet.json"),
        serde_json::to_vec(&json!({"key_files":["wallet.dat"],"num_addresses":2,"listen":wallet}))
            .unwrap(),
    )
    .unwrap();
    fs::write(
        local.join("Farmer.json"),
        serde_json::to_vec(&json!({"private_endpoint":farmer,"harvester_endpoint":harvesters}))
            .unwrap(),
    )
    .unwrap();
    fs::write(
        local.join("Harvester.json"),
        serde_json::to_vec(&json!({"plot_dirs":[],"num_threads":1,"listen":harvester})).unwrap(),
    )
    .unwrap();
    // An explicit remote listener also enables the local harvester route,
    // without requiring the global C++ allow_remote switch.
    let binary = Path::new(env!("CARGO_BIN_EXE_mmx"))
        .parent()
        .unwrap()
        .join("mmx-node");
    let mut args = vec!["--mmx-root".into(), root.as_os_str().into()];
    if !from_home {
        args.extend([
            "-c".into(),
            base.into_os_string(),
            local.into_os_string(),
            "--data-dir".into(),
            root.join("rust-data").into_os_string(),
        ]);
    }
    if overrides {
        args.extend([
            "--network".into(),
            "selected-network".into(),
            "--data-dir".into(),
            state.join("cli-ledger").into_os_string(),
        ]);
    }
    if !router_overrides {
        args.extend(["--p2p-listen".into(), "127.0.0.1:0".into()]);
    }
    args.extend([
        "--rpc-listen".into(),
        "127.0.0.1:0".into(),
        "--http-listen".into(),
        "127.0.0.1:0".into(),
    ]);
    let mut environment = vec![("MMX_HOME", home.as_path())];
    if overrides {
        environment.push(("MMX_NETWORK", state.as_path()));
    }
    let (daemon, _) = Daemon::start_line_env(binary.to_str().unwrap(), &args, root, &environment);
    let line = daemon.line("P2P ");
    if router_overrides {
        let address: std::net::SocketAddr = line
            .split_whitespace()
            .nth(1)
            .unwrap()
            .trim_end_matches(',')
            .parse()
            .unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert_ne!(
            address.port(),
            0,
            "configured port 0 must allocate a listener"
        );
    }
    daemon.line("Wallet VNX RPC ");
    daemon.line("TimeLord VNX RPC ");
    if from_home {
        assert!(state
            .join(if overrides {
                "cli-ledger/identity.key"
            } else {
                "db/identity.key"
            })
            .is_file());
        assert!(state.join("timelord-identity/timelord_sk.dat").is_file());
        assert!(!root.join("mmx-data").exists());
        assert!(!root.join("wallet.dat").exists());
        assert!(!state.join("configured-ledger").exists());
    }
    let time_control = client(&timelord, "TimeLord");
    time_control
        .call_value("mmx.TimeLord.stop_vdf", json!({}))
        .unwrap();
    // The remote surface and the local harvester both announce this prefix;
    // wait for the control endpoint itself before querying it.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !root.join("harvester.sock").exists() {
        assert!(Instant::now() < deadline, "harvester did not start");
        thread::sleep(Duration::from_millis(20));
    }
    let node_endpoint = line
        .split_whitespace()
        .nth(4)
        .unwrap()
        .trim_end_matches(',');
    let node = client(node_endpoint, "Node");
    let wallets = client(&wallet, "Wallet");
    assert_eq!(
        wallets
            .call_value("mmx.Wallet.get_all_addresses", json!({"index":0}))
            .unwrap()["_ret_0"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let remote = client(&harvesters, "Farmer");
    assert_eq!(
        remote
            .call_value("mmx.Farmer.get_farmer_keys", json!({}))
            .unwrap()["_ret_0"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(remote
        .call_value(
            "mmx.Farmer.sign_vote",
            json!({"vote":{"__type":"mmx.ValidatorVote"}})
        )
        .is_err());
    let control = client(&harvester, "Harvester");
    assert_eq!(
        control
            .call_value("mmx.Harvester.get_total_bytes", json!({}))
            .unwrap()["_ret_0"],
        0
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while node
        .call_value("mmx.Node.get_synced_height", json!({}))
        .unwrap()["_ret_0"]
        .is_null()
    {
        assert!(
            Instant::now() < deadline,
            "node did not sync from its configured seed/fixed peers"
        );
        thread::sleep(Duration::from_millis(50));
    }
    control.close();
    time_control.close();
    remote.close();
    wallets.close();
    node.close();
    drop(daemon);
    for peer in &mut peers {
        peer.shutdown();
    }
}

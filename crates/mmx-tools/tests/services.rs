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
        use std::os::unix::ffi::OsStrExt;
        let exe = fs::canonicalize(exe).unwrap();
        let cwd = fs::canonicalize(cwd).unwrap();
        let mut argv = exe.as_os_str().as_bytes().to_vec();
        argv.push(0);
        for a in args {
            argv.extend(a.as_bytes());
            argv.push(0);
        }
        let mut child = Command::new(&exe)
            .args(args)
            .current_dir(&cwd)
            .env("RAYON_NUM_THREADS", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = start_time(child.id()).unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let r = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send((r, line));
        });
        let daemon = Self {
            child,
            exe,
            cwd,
            argv,
            start,
        };
        let (result, line) = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("daemon startup timed out");
        assert!(result.unwrap() > 0, "daemon exited during startup");
        (daemon, line.split_whitespace().last().unwrap().into())
    }
    fn owns(&self) -> bool {
        let p = format!("/proc/{}", self.child.id());
        fs::read_link(format!("{p}/exe")).ok().as_ref() == Some(&self.exe)
            && fs::read_link(format!("{p}/cwd")).ok().as_ref() == Some(&self.cwd)
            && fs::read(format!("{p}/cmdline")).ok().as_ref() == Some(&self.argv)
            && start_time(self.child.id()).as_ref() == Some(&self.start)
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
    methods.add("mmx.Wallet.get_all_farmer_keys", None, move |_, _| {
        Ok(json!([[scalar, public.as_slice()]]))
    });
    methods.add("mmx.Wallet.get_all_accounts", None, |_, _| {
        Ok(serde_json::to_value(vec![mmx::account_info_t {
            address: Some([7; 32].into()),
            ..Default::default()
        }])?)
    });
    wallet.register("Wallet", methods).unwrap();
    let mut wallet = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        wallet,
        mmx_types::vnx_registry().unwrap(),
        ServerOptions {
            services: ["Wallet".into()].into(),
            ..Default::default()
        },
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let plot_dir = dir.path().join("plots");
    fs::create_dir(&plot_dir).unwrap();
    fs::write(
        plot_dir.join("native.plot"),
        include_bytes!("../../mmx-farming/tests/native/native-0.plot"),
    )
    .unwrap();
    let (farmer, farmer_endpoint) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-farmer"),
        &[
            "--wallet".into(),
            wallet.endpoint().to_string().into(),
            "--node".into(),
            node.endpoint().to_string().into(),
            "--listen".into(),
            "127.0.0.1:0".into(),
        ],
        dir.path(),
    );
    let (harvester, _) = Daemon::start(
        env!("CARGO_BIN_EXE_mmx-harvester"),
        &[
            "--farmer".into(),
            farmer_endpoint.into(),
            "--plot-dir".into(),
            plot_dir.into_os_string(),
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

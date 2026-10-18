use clap::Parser;
use mmx_farming::{farmer::Farmer, harvester::Harvester, service, wallet::Account, Result};
use mmx_types::mmx;
use mmx_wallet::{Address, FarmerKey, KeyFile};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use vnx::{
    net::NetworkClient,
    runtime::{OverflowPolicy, Runtime},
    serde_json::{self, json, Value},
};

fn err(s: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(s.into()).into()
}
fn stopping() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    Ok(stop)
}
fn connect(address: &str, service: &str) -> Result<NetworkClient> {
    Ok(NetworkClient::connect(
        &address.parse()?,
        service,
        mmx_types::vnx_registry()?,
        Duration::from_secs(10),
    )?)
}
fn call(client: &NetworkClient, method: &str, args: Value) -> Result<Value> {
    Ok(client.call_value(method, args)?["_ret_0"].clone())
}
fn key_file(path: &Path) -> Result<KeyFile> {
    let mut bytes = Vec::new();
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    Ok(KeyFile::decode(&bytes)?)
}
fn params(root: &Path, network: &str) -> Result<mmx::ChainParams> {
    Ok(mmx_execution::chain::network::Network::load(root, network)?.params)
}

#[derive(Parser)]
#[command(
    name = "mmx-wallet",
    about = "Local native VNX wallet service; create/import key files with mmx wallet"
)]
pub struct WalletArgs {
    #[arg(long, required = true)]
    key_file: Vec<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:11335")]
    listen: String,
    #[arg(long, default_value_t = 0)]
    account: u32,
    #[arg(long, default_value_t = 100)]
    num_addresses: u32,
    #[arg(long, default_value_t = 600)]
    lock_timeout: u64,
}
pub fn wallet_main() -> Result<()> {
    let args = WalletArgs::parse();
    let mut accounts = BTreeMap::new();
    for (index, path) in args.key_file.iter().enumerate() {
        let mut account = Account::new(
            key_file(path)?,
            args.account,
            args.num_addresses,
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )?;
        account.timeout = Duration::from_secs(args.lock_timeout);
        accounts.insert(u32::try_from(index)?, account);
    }
    let accounts = Arc::new(Mutex::new(accounts));
    let runtime = Runtime::new()?;
    runtime.register("Wallet", service::wallet_methods(accounts.clone()))?;
    let mut server = service::bind_private(
        args.listen.parse()?,
        runtime,
        service::private_options(["Wallet".into()]),
    )?;
    println!("Wallet VNX RPC {}", server.endpoint());
    let stop = stopping()?;
    while !stop.load(Ordering::Relaxed) {
        for account in accounts
            .lock()
            .map_err(|_| err("wallet lock poisoned"))?
            .values_mut()
        {
            account.expire();
        }
        thread::sleep(Duration::from_millis(100));
    }
    server.shutdown();
    Ok(())
}
#[derive(Parser)]
#[command(
    name = "mmx-farmer",
    about = "Native farmer signer and harvester topic relay"
)]
pub struct FarmerArgs {
    #[arg(long, default_value = "127.0.0.1:11335")]
    wallet: String,
    #[arg(long, default_value = "127.0.0.1:11331")]
    node: String,
    #[arg(long, default_value = "127.0.0.1:11333")]
    listen: String,
    #[arg(long)]
    reward_address: Option<Address>,
    #[arg(long, default_value = "../mmx-node")]
    mmx_root: PathBuf,
    #[arg(long, default_value = "mainnet")]
    network: String,
}
fn farmer_keys(wallet: &NetworkClient) -> Result<Vec<FarmerKey>> {
    let value = call(wallet, "mmx.Wallet.get_all_farmer_keys", json!({}))?;
    value
        .as_array()
        .ok_or_else(|| err("invalid wallet farmer keys"))?
        .iter()
        .map(|pair| {
            let pair = pair
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or_else(|| err("invalid farmer key pair"))?;
            let secret: vnx::Fixed<u8, 32> = serde_json::from_value(pair[0].clone())?;
            let public: vnx::Fixed<u8, 33> = serde_json::from_value(pair[1].clone())?;
            let key = FarmerKey::from_bytes(&secret.0)?;
            if key.public_key() != public.0 {
                return Err(err("farmer secret/public mismatch"));
            }
            Ok(key)
        })
        .collect()
}
pub fn farmer_main() -> Result<()> {
    let args = FarmerArgs::parse();
    let params = params(&args.mmx_root, &args.network)?;
    let wallet = connect(&args.wallet, "Wallet")?;
    let keys = farmer_keys(&wallet)?;
    if keys.is_empty() {
        return Err(err("wallet has no farmer identities"));
    }
    let mut reward = args.reward_address;
    if reward.is_none() {
        let all = call(&wallet, "mmx.Wallet.get_all_accounts", json!({}))?;
        for a in all
            .as_array()
            .ok_or_else(|| err("invalid wallet accounts"))?
        {
            if !a["address"].is_null() {
                let address: vnx::Fixed<u8, 32> = serde_json::from_value(a["address"].clone())?;
                reward = Some(Address(address.0));
                break;
            }
        }
    }
    let farmer = Arc::new(Mutex::new(Farmer::new(keys, reward)));
    let runtime = Runtime::new()?;
    let mac = vnx::util::random_nonzero_u64()?;
    runtime.register(
        "Farmer",
        service::farmer_methods(farmer.clone(), params, mac),
    )?;
    let proofs = runtime.subscribe("harvester.proof", 64, OverflowPolicy::DropOldest, false)?;
    let reports = runtime.subscribe("harvester.info", 64, OverflowPolicy::DropOldest, false)?;
    let node = connect(&args.node, "Node")?;
    // Do not request replay: an old challenge must never trigger fresh disk work.
    let challenges =
        node.subscribe("harvester.challenges", 8, OverflowPolicy::DropOldest, false)?;
    let mut options = service::private_options(["Farmer".into()]);
    options.publish_topics = ["harvester.proof".into(), "harvester.info".into()].into();
    options.export_topics = ["harvester.challenges".into(), "farmer.proof".into()].into();
    let mut server = service::bind_private(args.listen.parse()?, runtime.clone(), options)?;
    println!("Farmer VNX RPC {}", server.endpoint());
    let stop = stopping()?;
    let mut updated = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        while let Some(sample) = challenges.try_recv()? {
            if !sample.replay
                && vnx::time::now_micros().saturating_sub(sample.recv_time) <= 10_000_000
            {
                runtime.publish("harvester.challenges", sample.value, false)?;
            }
        }
        while let Some(sample) = reports.try_recv()? {
            match serde_json::from_value::<mmx::FarmInfo>(sample.value) {
                Ok(info) => {
                    if let Err(e) = farmer
                        .lock()
                        .map_err(|_| err("farmer lock poisoned"))?
                        .report(info)
                    {
                        eprintln!("Harvester report: {e}");
                    }
                }
                Err(e) => eprintln!("Invalid farm report: {e}"),
            }
        }
        while let Some(sample) = proofs.try_recv()? {
            let signed = (|| -> Result<Value> {
                let r = service::from_json(&sample.value)?;
                let signed = farmer
                    .lock()
                    .map_err(|_| err("farmer lock poisoned"))?
                    .sign_response(&r)?;
                service::to_json(&signed)
            })();
            match signed {
                Ok(value) => {
                    runtime.publish("farmer.proof", value.clone(), false)?;
                    node.publish("farmer.proof", value)?;
                }
                Err(e) => eprintln!("Rejected harvester proof: {e}"),
            }
        }
        if updated.elapsed() >= Duration::from_secs(60) {
            match farmer_keys(&wallet) {
                Ok(keys) => {
                    for key in keys {
                        farmer
                            .lock()
                            .map_err(|_| err("farmer lock poisoned"))?
                            .add_key(key);
                    }
                }
                Err(e) => eprintln!("Wallet key refresh: {e}"),
            }
            updated = Instant::now();
        }
        thread::sleep(Duration::from_millis(20));
    }
    node.close();
    wallet.close();
    server.shutdown();
    Ok(())
}
#[derive(Parser)]
#[command(name = "mmx-harvester", about = "Native MMX disk plot harvester")]
pub struct HarvesterArgs {
    #[arg(long, required = true)]
    plot_dir: Vec<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:11333")]
    farmer: String,
    #[arg(long, default_value = "127.0.0.1:11334")]
    listen: String,
    #[arg(long, default_value = "harvester")]
    name: String,
    #[arg(long, default_value_t = 65536)]
    max_f1: usize,
    #[arg(long, default_value_t = 2)]
    threads: usize,
    #[arg(long, default_value_t = 3600)]
    reload_interval: u64,
    #[arg(long, default_value = "../mmx-node")]
    mmx_root: PathBuf,
    #[arg(long, default_value = "mainnet")]
    network: String,
}
pub fn harvester_main() -> Result<()> {
    let args = HarvesterArgs::parse();
    if args.threads == 0 || args.threads > 256 {
        return Err(err("--threads must be 1..256"));
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build_global()?;
    let params = params(&args.mmx_root, &args.network)?;
    let farmer = connect(&args.farmer, "Farmer")?;
    let keys: Vec<vnx::Fixed<u8, 33>> =
        serde_json::from_value(call(&farmer, "mmx.Farmer.get_farmer_keys", json!({}))?)?;
    let mut harvester = Harvester::new(args.name, keys.into_iter().map(|k| k.0))?;
    harvester.directories = args.plot_dir.into_iter().collect();
    harvester.max_f1 = args.max_f1;
    harvester.reload();
    for (path, e) in &harvester.failures {
        eprintln!("{}: {e}", path.display());
    }
    let harvester = Arc::new(Mutex::new(harvester));
    let runtime = Runtime::new()?;
    runtime.register("Harvester", service::harvester_methods(harvester.clone()))?;
    let mut server = service::bind_private(
        args.listen.parse()?,
        runtime,
        service::private_options(["Harvester".into()]),
    )?;
    let challenges =
        farmer.subscribe("harvester.challenges", 1, OverflowPolicy::DropOldest, false)?;
    println!("Harvester VNX RPC {}", server.endpoint());
    let stop = stopping()?;
    let (mut reported, mut reloaded) = (Instant::now() - Duration::from_secs(10), Instant::now());
    while !stop.load(Ordering::Relaxed) {
        if reported.elapsed() >= Duration::from_secs(10) {
            let info = harvester
                .lock()
                .map_err(|_| err("harvester lock poisoned"))?
                .farm_info()?;
            farmer.publish("harvester.info", serde_json::to_value(info)?)?;
            reported = Instant::now();
        }
        if args.reload_interval > 0
            && reloaded.elapsed() >= Duration::from_secs(args.reload_interval)
        {
            let keys: Vec<vnx::Fixed<u8, 33>> =
                serde_json::from_value(call(&farmer, "mmx.Farmer.get_farmer_keys", json!({}))?)?;
            let mut h = harvester
                .lock()
                .map_err(|_| err("harvester lock poisoned"))?;
            h.farmer_keys = keys.into_iter().map(|k| k.0).collect();
            h.reload();
            reloaded = Instant::now();
        }
        if let Some(sample) = challenges.try_recv()? {
            if sample.replay
                || vnx::time::now_micros().saturating_sub(sample.recv_time) > 10_000_000
            {
                continue;
            }
            let challenge: mmx::Challenge = serde_json::from_value(sample.value)?;
            let started = Instant::now();
            match harvester
                .lock()
                .map_err(|_| err("harvester lock poisoned"))?
                .lookup(&challenge, &params)
            {
                Ok(proofs) => {
                    for proof in proofs {
                        if started.elapsed() > Duration::from_secs(10) {
                            eprintln!("Discarding late proof");
                            break;
                        }
                        farmer.publish("harvester.proof", service::to_json(&proof)?)?;
                    }
                }
                Err(e) => eprintln!("Plot lookup: {e}"),
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    farmer.close();
    server.shutdown();
    Ok(())
}

#[derive(Parser)]
#[command(
    name = "mmx",
    about = "MMX native RPC utility. Use `mmx wallet --help` for the standalone wallet CLI"
)]
struct RpcArgs {
    service: String,
    method: String,
    #[arg(long)]
    endpoint: Option<String>,
    /// Native VNX JSON object, or '-' to read an object from stdin.
    #[arg(long, default_value = "{}")]
    args: String,
}
pub fn utility_main() -> Result<i32> {
    let mut raw: Vec<String> = std::env::args().collect();
    if raw.get(1).is_some_and(|s| s == "wallet") {
        raw.remove(1);
        return Ok(mmxwallet::main_from(raw));
    }
    let a = RpcArgs::parse();
    let service = match a.service.as_str() {
        "node" => "Node",
        "farmer" => "Farmer",
        "harvester" => "Harvester",
        "wallet-rpc" => "Wallet",
        _ => {
            return Err(err(
                "service must be node, farmer, harvester, wallet-rpc or wallet",
            ))
        }
    };
    let port = match service {
        "Farmer" => 11333,
        "Harvester" => 11334,
        "Wallet" => 11335,
        _ => 11331,
    };
    let args = if a.args == "-" {
        let mut b = String::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_string(&mut b)?;
        if b.len() > 1024 * 1024 {
            return Err(err("RPC input exceeds 1 MiB"));
        }
        b
    } else {
        a.args
    };
    let args: Value = serde_json::from_str(&args)?;
    if !args.is_object() {
        return Err(err("RPC arguments must be an object"));
    }
    let client = connect(
        &a.endpoint.unwrap_or_else(|| format!("127.0.0.1:{port}")),
        service,
    )?;
    let value = call(&client, &format!("mmx.{service}.{}", a.method), args)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    client.close();
    Ok(0)
}

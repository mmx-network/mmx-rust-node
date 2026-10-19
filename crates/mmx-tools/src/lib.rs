mod config;
use clap::{CommandFactory, FromArgMatches, Parser};
use mmx_farming::{
    farmer::Farmer,
    harvester::{ChallengeQueue, Harvester},
    service,
    wallet::Account,
    Result,
};
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
    net::{NetworkClient, Server},
    runtime::{Methods, OverflowPolicy, Runtime},
    serde_json::{self, json, Value},
    VnxType,
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
fn params(node: &NetworkClient, expected: Option<&str>) -> Result<mmx::ChainParams> {
    let params: mmx::ChainParams =
        serde_json::from_value(call(node, "mmx.Node.get_params", json!({}))?)?;
    if params.network.is_empty() || expected.is_some_and(|n| n != params.network) {
        return Err(err("node network does not match requested network"));
    }
    Ok(params)
}

#[derive(Parser)]
#[command(
    name = "mmx-wallet",
    about = "Local native VNX wallet service; create/import key files with mmx wallet"
)]
pub struct WalletArgs {
    #[command(flatten)]
    layers: config::Layers,
    /// Native node VNX endpoint for balances, validation and spending.
    #[arg(long)]
    node: Option<String>,
    #[arg(long)]
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
    let matches = WalletArgs::command().get_matches();
    let mut args = WalletArgs::from_arg_matches(&matches)?;
    let cfg = args.layers.load()?;
    config::apply(&matches, &cfg, "listen", "Wallet.listen", &mut args.listen)?;
    config::apply(
        &matches,
        &cfg,
        "key_file",
        "Wallet.key_files",
        &mut args.key_file,
    )?;
    config::apply(
        &matches,
        &cfg,
        "num_addresses",
        "Wallet.num_addresses",
        &mut args.num_addresses,
    )?;
    config::apply(
        &matches,
        &cfg,
        "lock_timeout",
        "Wallet.lock_timeout_sec",
        &mut args.lock_timeout,
    )?;
    if args.key_file.is_empty() {
        return Err(err("supply --key-file or Wallet.key_files in -c CONFIG"));
    }
    args.key_file = args
        .key_file
        .iter()
        .map(|p| args.layers.relative(p))
        .collect();
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
    let node = args
        .node
        .as_deref()
        .map(|address| {
            connect(address, "Node")
                .map(|client| Arc::new(client) as Arc<dyn mmx_farming::spending::WalletNode>)
        })
        .transpose()?;
    runtime.register(
        "Wallet",
        service::wallet_methods_with_node(accounts.clone(), node),
    )?;
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
    #[command(flatten)]
    layers: config::Layers,
    /// Restricted challenge/proof listener for remote harvesters.
    #[arg(long)]
    harvester_listen: Option<String>,
    /// Require configured VNX user credentials on the harvester listener.
    #[arg(long)]
    require_harvester_login: bool,
    #[arg(long, default_value = "127.0.0.1:11335")]
    wallet: String,
    /// VNX login for a wallet that protects farmer-key access.
    #[arg(long)]
    wallet_username: Option<String>,
    /// Read the wallet login password from this file, never a command argument.
    #[arg(long)]
    wallet_password_file: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:11331")]
    node: String,
    #[arg(long, default_value = "127.0.0.1:11333")]
    listen: String,
    #[arg(long)]
    reward_address: Option<Address>,
    #[arg(long)]
    network: Option<String>,
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
fn wallet_reward(wallet: &NetworkClient) -> Result<Option<Address>> {
    let all = call(wallet, "mmx.Wallet.get_all_accounts", json!({}))?;
    for account in all
        .as_array()
        .ok_or_else(|| err("invalid wallet accounts"))?
    {
        if !account["address"].is_null() {
            let address: vnx::Fixed<u8, 32> = serde_json::from_value(account["address"].clone())?;
            if address.0 != [0; 32] {
                return Ok(Some(Address(address.0)));
            }
        }
    }
    Ok(None)
}
pub fn farmer_main() -> Result<()> {
    let matches = FarmerArgs::command().get_matches();
    let mut args = FarmerArgs::from_arg_matches(&matches)?;
    let cfg = args.layers.load()?;
    config::apply(
        &matches,
        &cfg,
        "wallet_username",
        "Farmer.wallet_username",
        &mut args.wallet_username,
    )?;
    config::apply(
        &matches,
        &cfg,
        "wallet_password_file",
        "Farmer.wallet_password_file",
        &mut args.wallet_password_file,
    )?;
    if args.wallet_username.is_some() != args.wallet_password_file.is_some() {
        return Err(err(
            "wallet login requires both --wallet-username and --wallet-password-file",
        ));
    }
    config::apply(
        &matches,
        &cfg,
        "harvester_listen",
        "Farmer.harvester_endpoint",
        &mut args.harvester_listen,
    )?;
    config::apply(
        &matches,
        &cfg,
        "listen",
        "Farmer.private_endpoint",
        &mut args.listen,
    )?;
    config::apply(
        &matches,
        &cfg,
        "reward_address",
        "Farmer.reward_addr",
        &mut args.reward_address,
    )?;
    if args.harvester_listen.is_none() && cfg.read::<bool>("allow_remote")? == Some(true) {
        args.harvester_listen = Some(config::endpoint(
            &cfg.read::<String>("endpoint")?
                .unwrap_or_else(|| "0.0.0.0".into()),
            11333,
        )?);
    }
    if args.harvester_listen.is_some()
        && matches.value_source("listen") != Some(clap::parser::ValueSource::CommandLine)
        && cfg.read::<String>("Farmer.private_endpoint")?.is_none()
    {
        args.listen = "127.0.0.1:11332".into();
    }
    let node = connect(&args.node, "Node")?;
    let params = params(&node, args.network.as_deref())?;
    let wallet = connect(&args.wallet, "Wallet")?;
    if let (Some(username), Some(path)) = (&args.wallet_username, &args.wallet_password_file) {
        let mut password = zeroize::Zeroizing::new(String::new());
        File::open(args.layers.relative(path))?
            .take(4097)
            .read_to_string(&mut password)?;
        if password.len() > 4096 {
            return Err(err("wallet password file exceeds 4 KiB"));
        }
        wallet.login(username, password.trim_end_matches(['\r', '\n']))?;
    }
    let keys = farmer_keys(&wallet)?;
    if keys.is_empty() {
        return Err(err("wallet has no farmer identities"));
    }
    let mut reward = args
        .reward_address
        .filter(|address| *address != Address::default());
    if reward.is_none() {
        reward = wallet_reward(&wallet)?;
    }
    let mut farmer = Farmer::new(keys, reward);
    if let Some(seconds) = cfg.read::<u64>("Farmer.harvester_timeout")? {
        farmer.harvester_timeout = Duration::from_secs(seconds);
    }
    let farmer = Arc::new(Mutex::new(farmer));
    let runtime = Runtime::new()?;
    let mac = vnx::util::random_nonzero_u64()?;
    let native_params = service::to_json(&mmx_consensus::Record::from_generated(&params)?)?;
    let mut methods = Methods::new();
    methods.add(
        "mmx.Node.get_params",
        Some("mmx.permission_e.REMOTE".into()),
        move |_, _| Ok(native_params.clone()),
    );
    runtime.register("Node", methods)?;
    runtime.register(
        "Farmer",
        service::farmer_methods(farmer.clone(), params, mac),
    )?;
    let proofs = runtime.subscribe("harvester.proof", 64, OverflowPolicy::DropOldest, false)?;
    let reports = runtime.subscribe("harvester.info", 64, OverflowPolicy::DropOldest, false)?;
    // Do not request replay: an old challenge must never trigger fresh disk work.
    let challenges = node.subscribe(
        "harvester.challenges",
        128,
        OverflowPolicy::DropOldest,
        false,
    )?;
    let mut options = service::private_options(["Farmer".into(), "Node".into()]);
    options.publish_topics = ["harvester.proof".into(), "harvester.info".into()].into();
    options.export_topics = ["harvester.challenges".into(), "farmer.proof".into()].into();
    let mut server = service::bind_private(args.listen.parse()?, runtime.clone(), options)?;
    let mut remote = args
        .harvester_listen
        .as_ref()
        .map(|endpoint| -> Result<Server> {
            let auth = vnx::auth::AuthenticationServer::from_config(&cfg)?;
            Ok(Server::bind(
                endpoint.parse()?,
                runtime.clone(),
                mmx_types::vnx_registry()?,
                service::harvester_options(auth, args.require_harvester_login),
            )?)
        })
        .transpose()?;
    if let Some(remote) = &remote {
        println!(
            "Harvester VNX RPC {} / Farmer VNX RPC {}",
            remote.endpoint(),
            server.endpoint()
        );
    } else {
        println!("Farmer VNX RPC {}", server.endpoint());
    }
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
                    if let Err(e) = node.publish("farmer.proof", value) {
                        eprintln!("Node proof delivery: {e}");
                    }
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
            // Protected wallets learn their spending addresses on first unlock.
            // Resolve the default then, without restarting the farmer.
            if farmer
                .lock()
                .map_err(|_| err("farmer lock poisoned"))?
                .reward_address
                .is_none()
            {
                match wallet_reward(&wallet) {
                    Ok(address) => {
                        farmer
                            .lock()
                            .map_err(|_| err("farmer lock poisoned"))?
                            .reward_address = address;
                    }
                    Err(e) => eprintln!("Wallet reward refresh: {e}"),
                }
            }
            updated = Instant::now();
        }
        thread::sleep(Duration::from_millis(20));
    }
    node.close();
    wallet.close();
    if let Some(remote) = &mut remote {
        remote.shutdown();
    }
    server.shutdown();
    Ok(())
}
#[derive(Parser)]
#[command(name = "mmx-harvester", about = "Native MMX disk plot harvester")]
pub struct HarvesterArgs {
    #[command(flatten)]
    layers: config::Layers,
    #[arg(long)]
    username: Option<String>,
    /// Read the VNX login password from this file.
    #[arg(long)]
    password_file: Option<PathBuf>,
    #[arg(long)]
    plot_dir: Vec<PathBuf>,
    #[arg(long, alias = "node", default_value = "127.0.0.1:11333")]
    farmer: String,
    #[arg(long, default_value = "127.0.0.1:11334")]
    listen: String,
    #[arg(long, default_value = "harvester")]
    name: String,
    #[arg(long, default_value_t = 65536)]
    max_f1: usize,
    /// OpenCL GPU for compressed proof computation; -1 uses the CPU.
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    opencl_device: i32,
    #[arg(long)]
    opencl_platform: Option<String>,
    #[arg(long, default_value_t = 2)]
    threads: usize,
    /// Log each completed challenge's proof count and lookup time.
    #[arg(long)]
    log_lookups: bool,
    #[arg(long, default_value_t = 3600)]
    reload_interval: u64,
    #[arg(long)]
    network: Option<String>,
}
pub fn harvester_main() -> Result<()> {
    let matches = HarvesterArgs::command().get_matches();
    let mut args = HarvesterArgs::from_arg_matches(&matches)?;
    let cfg = args.layers.load()?;
    config::apply(
        &matches,
        &cfg,
        "opencl_device",
        "Harvester.opencl_device",
        &mut args.opencl_device,
    )?;
    config::apply(
        &matches,
        &cfg,
        "opencl_platform",
        "opencl.platform",
        &mut args.opencl_platform,
    )?;
    if args.opencl_device < -1 {
        return Err(err("--opencl-device must be -1 or a GPU index"));
    }
    config::apply(
        &matches,
        &cfg,
        "listen",
        "Harvester.listen",
        &mut args.listen,
    )?;
    config::apply(
        &matches,
        &cfg,
        "plot_dir",
        "Harvester.plot_dirs",
        &mut args.plot_dir,
    )?;
    config::apply(&matches, &cfg, "name", "Harvester.my_name", &mut args.name)?;
    config::apply(
        &matches,
        &cfg,
        "threads",
        "Harvester.num_threads",
        &mut args.threads,
    )?;
    config::apply(
        &matches,
        &cfg,
        "reload_interval",
        "Harvester.reload_interval",
        &mut args.reload_interval,
    )?;
    config::apply(&matches, &cfg, "farmer", "node", &mut args.farmer)?;
    args.farmer = config::remote_endpoint(&args.farmer, 11333);
    if args.plot_dir.is_empty() && args.layers.paths.is_empty() {
        return Err(err("supply --plot-dir or Harvester.plot_dirs in -c CONFIG"));
    }
    if args.username.is_none() {
        args.username = cfg.read("Proxy.username")?;
    }
    let password = if let Some(path) = &args.password_file {
        Some(
            std::fs::read_to_string(path)?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        )
    } else {
        cfg.read::<String>("Proxy.password")?
    };
    let login = |client: &NetworkClient| -> Result<()> {
        if let Some(user) = &args.username {
            client.login(
                user,
                password
                    .as_deref()
                    .ok_or_else(|| err("supply --password-file or Proxy.password"))?,
            )?;
        }
        Ok(())
    };
    if args.threads == 0 || args.threads > 256 {
        return Err(err("--threads must be 1..256"));
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build_global()?;
    let node = connect(&args.farmer, "Node")?;
    login(&node)?;
    let params = params(&node, args.network.as_deref())?;
    node.close();
    let farmer = connect(&args.farmer, "Farmer")?;
    login(&farmer)?;
    let keys: Vec<vnx::Fixed<u8, 33>> =
        serde_json::from_value(call(&farmer, "mmx.Farmer.get_farmer_keys", json!({}))?)?;
    let mut harvester = Harvester::new(args.name, keys.into_iter().map(|k| k.0))?;
    harvester.directories = args.plot_dir.into_iter().collect();
    harvester.max_f1 = args.max_f1;
    let device_name: Option<String> = cfg.read("Harvester.opencl_device_name")?;
    harvester.set_accelerator(mmx_consensus::accelerator::Accelerator::configured(
        args.opencl_device,
        args.opencl_platform.as_deref(),
        device_name.as_deref(),
    ));
    if let Some(v) = cfg.read("Harvester.max_recursion")? {
        harvester.max_recursion = v;
    }
    if let Some(v) = cfg.read("Harvester.recursive_search")? {
        harvester.recursive = v;
    }
    if let Some(v) = cfg.read::<Vec<PathBuf>>("Harvester.dir_blacklist")? {
        harvester.blacklist = v.into_iter().collect();
    }
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
    let challenges = farmer.subscribe(
        "harvester.challenges",
        128,
        OverflowPolicy::DropOldest,
        false,
    )?;
    println!("Harvester VNX RPC {}", server.endpoint());
    let stop = stopping()?;
    let (mut reported, mut reloaded) = (Instant::now() - Duration::from_secs(10), Instant::now());
    let mut reload_retry = false;
    let mut queue = ChallengeQueue::new(Duration::from_secs(10));
    while !stop.load(Ordering::Relaxed) {
        if reported.elapsed() >= Duration::from_secs(10) {
            let info = harvester
                .lock()
                .map_err(|_| err("harvester lock poisoned"))?
                .farm_info()?;
            if let Err(e) = farmer.publish("harvester.info", info.to_value()?) {
                eprintln!("Farmer report delivery: {e}");
            }
            reported = Instant::now();
        }
        if args.reload_interval > 0
            && reloaded.elapsed()
                >= Duration::from_secs(if reload_retry {
                    args.reload_interval.min(60)
                } else {
                    args.reload_interval
                })
        {
            let keys = call(&farmer, "mmx.Farmer.get_farmer_keys", json!({}))
                .and_then(|value| Ok(serde_json::from_value::<Vec<vnx::Fixed<u8, 33>>>(value)?));
            match keys {
                Ok(keys) => {
                    let mut h = harvester
                        .lock()
                        .map_err(|_| err("harvester lock poisoned"))?;
                    h.farmer_keys = keys.into_iter().map(|k| k.0).collect();
                    h.reload();
                    queue.clear();
                    reloaded = Instant::now();
                    reload_retry = false;
                }
                Err(e) => {
                    eprintln!("Farmer key refresh: {e}");
                    // Keep the current plots during a temporary remote outage.
                    reloaded = Instant::now();
                    reload_retry = true;
                }
            }
        }
        while let Some(sample) = challenges.try_recv()? {
            if sample.replay {
                continue;
            }
            let age = Duration::from_micros(
                vnx::time::now_micros()
                    .saturating_sub(sample.recv_time)
                    .max(0) as u64,
            );
            match mmx::Challenge::from_value(sample.value) {
                Ok(challenge) => {
                    queue.push(challenge, age);
                }
                Err(e) => eprintln!("Invalid farming challenge: {e}"),
            }
        }
        if let Some((challenge, remaining)) = queue.pop_with_remaining() {
            let started = Instant::now();
            match harvester
                .lock()
                .map_err(|_| err("harvester lock poisoned"))?
                .lookup_for(&challenge, &params, remaining)
            {
                Ok(proofs) => {
                    let found = proofs.len();
                    let mut published = 0;
                    for proof in proofs {
                        if started.elapsed() > remaining {
                            eprintln!("Discarding late proof");
                            break;
                        }
                        if let Err(e) = farmer.publish("harvester.proof", service::to_json(&proof)?)
                        {
                            eprintln!("Farmer proof delivery: {e}");
                        } else {
                            published += 1;
                        }
                    }
                    if args.log_lookups {
                        eprintln!(
                            "Lookup VDF {}: {found} proofs, {published} published, {} ms",
                            challenge.vdf_height,
                            started.elapsed().as_millis()
                        );
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

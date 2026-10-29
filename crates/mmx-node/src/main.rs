#[cfg(unix)]
mod services;
use mmx_execution::chain::network::Network;
use mmx_node::{peer, production, rpc, sync, Node, Result};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

const USAGE: &str = "mmx-node [--mmx-root PATH] [--network mainnet] [--data-dir PATH]
  [--peer HOST[:PORT]]... [--offline] [--sync-once] [--rollback-to HEIGHT]
  [--purge-before HEIGHT] [--compact-storage]
  [--no-services] [--farmer ENDPOINT] [--reward-vote -1|0|1] [-c CONFIG]...
  [--opencl-device INDEX|-1] [--opencl-platform NAME]
  [--p2p-listen HOST:PORT] [--rpc-listen HOST:PORT] [--http-listen HOST:PORT]
  [--rpc-cache-mb MIB]

Defaults: --mmx-root ../mmx-node; network from $MMX_HOME/NETWORK or mainnet.
  MMX_HOME defaults to the working directory; local config is MMX_HOME/config/local.
  MMX_NETWORK overrides the network state directory, otherwise MMX_DATA/<network>
  (MMX_DATA comes from MMX_HOME/config/local/MMX_DATA, the env, or MMX_HOME).
  Ledger: <network directory>/db, or Node.database_path; --data-dir overrides both.
  P2P Router.host:<network port> (127.0.0.1 without config), VNX RPC 127.0.0.1:11331,
  HTTP JSON-RPC http://127.0.0.1:11380/rpc.
OpenCL GPU 0 is used when available, with CPU fallback; --opencl-device -1 disables it.
Historical sync uses configured native seed peers unless --peer is supplied.
--offline serves the stored ledger without contacting peers.
The in-process RPC cache defaults to 64 MiB; --rpc-cache-mb or Node.rpc_cache_size_mb sets its budget.
--sync-once exits after coordinated sync. Router.min_sync_peers / num_peers_out control peer counts.
--p2p-listen overrides Router.host / port. --rollback-to keeps that height.
--compact-storage reclaims legacy VDF data and duplicate bodies, preserves all history, and exits.
History is retained by default. --purge-before reclaims older history before serving,
keeps HEIGHT available, and prevents RPC reads or rollback below HEIGHT (native databases).";
struct Options {
    configs: Vec<PathBuf>,
    no_services: bool,
    farmer: Option<String>,
    reward_vote: Option<i8>,
    root: PathBuf,
    network: Option<String>,
    data: Option<PathBuf>,
    peers: Vec<String>,
    offline: bool,
    once: bool,
    rollback: Option<u32>,
    purge: Option<u32>,
    compact: bool,
    p2p: Option<String>,
    rpc: String,
    http: String,
    rpc_cache_mb: Option<usize>,
    opencl_device: Option<i32>,
    opencl_platform: Option<String>,
}
fn options() -> Result<Option<Options>> {
    let mut o = Options {
        configs: vec![],
        no_services: false,
        farmer: None,
        reward_vote: None,
        root: mmx_config::installation_root(),
        network: None,
        data: None,
        peers: vec![],
        offline: false,
        once: false,
        rollback: None,
        purge: None,
        compact: false,
        p2p: None,
        rpc: "127.0.0.1:11331".into(),
        http: "127.0.0.1:11380".into(),
        rpc_cache_mb: None,
        opencl_device: None,
        opencl_platform: None,
    };
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--offline" => o.offline = true,
            "--no-services" => o.no_services = true,
            "--sync-once" => o.once = true,
            "--compact-storage" => o.compact = true,
            _ => {
                let value = args
                    .next()
                    .ok_or_else(|| std::io::Error::other(format!("missing value for {arg}")))?;
                match arg.as_str() {
                    "--mmx-root" => o.root = value.into(),
                    "-c" | "--config" => {
                        o.configs.push(value.into());
                        while args.peek().is_some_and(|s| !s.starts_with('-')) {
                            o.configs.push(args.next().unwrap().into());
                        }
                    }
                    "--farmer" => o.farmer = Some(value),
                    "--reward-vote" => o.reward_vote = Some(value.parse()?),
                    "--network" => o.network = Some(value),
                    "--data-dir" => o.data = Some(value.into()),
                    "--peer" => o.peers.push(value),
                    "--rollback-to" => o.rollback = Some(value.parse()?),
                    "--purge-before" => o.purge = Some(value.parse()?),
                    "--p2p-listen" => o.p2p = Some(value),
                    "--rpc-listen" => o.rpc = value,
                    "--http-listen" => o.http = value,
                    "--rpc-cache-mb" => o.rpc_cache_mb = Some(value.parse()?),
                    "--opencl-device" => o.opencl_device = Some(value.parse()?),
                    "--opencl-platform" => o.opencl_platform = Some(value),
                    _ => {
                        return Err(std::io::Error::other(format!(
                            "unknown option: {arg}\n{USAGE}"
                        ))
                        .into())
                    }
                }
            }
        }
    }
    if o.offline && o.once {
        return Err(std::io::Error::other("--offline and --sync-once cannot be combined").into());
    }
    if o.compact && (o.rollback.is_some() || o.purge.is_some() || o.once) {
        return Err(std::io::Error::other(
            "--compact-storage cannot be combined with rollback, purge or sync-once",
        )
        .into());
    }
    Ok(Some(o))
}
fn main() -> Result<()> {
    let Some(mut options) = options()? else {
        return Ok(());
    };
    let layout = mmx_config::Layout::from_env(None, options.network.as_deref())?;
    let config = vnx::config::Config::new();
    let config_paths = if options.configs.is_empty() {
        let paths = layout.config_paths(&options.root, "node");
        if paths.is_empty() {
            return Err(std::io::Error::other(
                "no network configuration found; specify --mmx-root or -c",
            )
            .into());
        }
        paths
    } else {
        options.configs.clone()
    };
    for path in &config_paths {
        config.load_tree(path)?;
    }
    if let Some(path) = config.read::<PathBuf>("Router.storage_path")? {
        config.set(
            "Router.storage_path",
            vnx::serde_json::to_value(layout.network_path(&path))?,
        )?;
    }
    let configured = !options.configs.is_empty() || layout.local_config().is_dir();
    let network = Network::from_config(&config, options.root.join("data"))?;
    let router = mmx_node::router::RouterConfig::from_config(&config)?;
    let remote_harvesters = config.read::<bool>("allow_remote")?.unwrap_or(false)
        || config
            .read::<String>("Farmer.harvester_endpoint")?
            .is_some();
    if configured
        && config.read::<bool>("farmer")?.unwrap_or(true)
        && config.read::<bool>("wallet")?.unwrap_or(true)
        && options.farmer.is_none()
    {
        options.farmer = Some(
            config
                .read::<String>("Farmer.private_endpoint")?
                .unwrap_or_else(|| {
                    if remote_harvesters {
                        "127.0.0.1:11332".into()
                    } else {
                        "127.0.0.1:11333".into()
                    }
                }),
        );
    }
    if options.reward_vote.is_none() {
        options.reward_vote = config.read("Node.reward_vote")?;
    }
    if options.peers.is_empty() && !options.offline {
        if let Some(peers) = config.get("Router.seed_peers")? {
            options.peers = vnx::serde_json::from_value(peers)?;
        }
    }
    let port = u16::try_from(network.params.port)?;
    let peers: Vec<_> = options
        .peers
        .iter()
        .map(|address| mmx_node::router::endpoint(address, port))
        .collect::<Result<_>>()?;
    let data = match options.data {
        Some(path) => path,
        None => layout.network_path(
            &config
                .read::<PathBuf>("Node.database_path")?
                .unwrap_or_else(|| "db".into()),
        ),
    };
    let mut state = Node::open(&data, &network)?;
    if options.compact {
        let (archive, selected) = state.compact_storage()?;
        for (name, report) in [("archive", archive), ("selected", selected)] {
            println!(
                "Compacted {name}: {} -> {} value bytes, {} records, history floor {}",
                report.old_value_bytes,
                report.new_value_bytes,
                report.retained_records,
                report.floor
            );
        }
        return Ok(());
    }
    let cache_mb = options
        .rpc_cache_mb
        .or(config.read("Node.rpc_cache_size_mb")?)
        .unwrap_or(64);
    state.set_rpc_cache_limit(
        cache_mb
            .checked_mul(1024 * 1024)
            .ok_or("RPC cache size overflow")?,
    );
    state.configure_router(&config)?;
    if !options.offline
        && peers.is_empty()
        && router.fixed_peers.is_empty()
        && state.known_peer_addresses()?.is_empty()
    {
        return Err(std::io::Error::other(
            "no seed, fixed or cached peers; specify --peer or --offline",
        )
        .into());
    }
    if let Some(height) = options.rollback {
        state.rollback_to(height)?;
    }
    if let Some(height) = options.purge {
        state.purge_before(height)?;
        println!("Purged history before height {height}");
    }
    let device = options
        .opencl_device
        .or(config.read("Node.opencl_device")?)
        .unwrap_or(0);
    let platform = options.opencl_platform.or(config.read("opencl.platform")?);
    let name: Option<String> = config.read("Node.opencl_device_name")?;
    if device < -1 {
        return Err(std::io::Error::other("--opencl-device must be -1 or a GPU index").into());
    }
    let accelerator = mmx_consensus::accelerator::Accelerator::configured(
        device,
        platform.as_deref(),
        name.as_deref(),
    );
    state.enable_background_vdf_with(accelerator)?;
    state.enable_background_transactions()?;
    let tip = state.chain().selected().tip()?.ok_or("missing tip")?;
    println!(
        "Opened {} at height {} / VDF {}, root {}",
        network.params.network,
        tip.height,
        tip.vdf_height,
        state.root_height()
    );
    let node = Arc::new(Mutex::new(state));
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let mut p2p = options
        .p2p
        .or_else(|| router.listen_address(port))
        .map(|address| peer::Server::bind(&address, node.clone()))
        .transpose()?;
    let mut vnx = rpc::vnx_server(options.rpc.parse()?, node.clone())?;
    let mut http = rpc::HttpServer::bind(&options.http, node.clone())?;
    println!(
        "P2P {}, VNX RPC {}, HTTP http://{}/rpc",
        p2p.as_ref()
            .map(|server| server.address().to_string())
            .unwrap_or_else(|| "disabled".into()),
        vnx.endpoint(),
        http.address()
    );
    #[cfg(unix)]
    let mut services = if !options.offline && !options.once && !options.no_services && configured {
        let paths = config_paths.clone();
        Some(services::Services::start(
            &config,
            &paths,
            &layout,
            &network.params.network,
            &vnx.endpoint().to_string(),
            options.farmer.as_deref().unwrap_or("127.0.0.1:11333"),
        )?)
    } else {
        None
    };
    let mut farming = if !options.offline {
        options
            .farmer
            .as_ref()
            .map(|endpoint| {
                production::Worker::start(
                    node.clone(),
                    endpoint.parse()?,
                    stop.clone(),
                    options.reward_vote.unwrap_or(0),
                )
            })
            .transpose()?
    } else {
        None
    };
    let mut workers = Vec::new();
    {
        let node = node.clone();
        let stop = stop.clone();
        workers.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for _ in 0..10 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if let Err(e) = node.lock().unwrap().tick() {
                    eprintln!("Node update failed: {e}");
                    stop.store(true, Ordering::Relaxed);
                }
            }
        }));
    }
    if !options.offline {
        let sync_node = node.clone();
        let sync_stop = stop.clone();
        workers.push(thread::spawn(move || {
            if let Err(e) = sync::peers::run(peers, port, &sync_node, &sync_stop, options.once) {
                eprintln!("Sync coordinator failed: {e}");
                sync_stop.store(true, Ordering::Relaxed);
            }
        }));
    }

    while !stop.load(Ordering::Relaxed) {
        #[cfg(unix)]
        if let Some(services) = &mut services {
            if let Err(e) = services.check() {
                eprintln!("Service startup/runtime failure: {e}");
                stop.store(true, Ordering::Relaxed);
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    #[cfg(unix)]
    if let Some(services) = &mut services {
        services.shutdown();
    }
    if let Some(farming) = &mut farming {
        farming.shutdown();
    }
    if let Some(server) = &mut p2p {
        server.shutdown();
    }
    http.shutdown();
    vnx.shutdown();
    for worker in workers {
        let _ = worker.join();
    }
    println!("Node stopped");
    Ok(())
}

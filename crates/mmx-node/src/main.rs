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
  [--purge-before HEIGHT]
  [--no-services] [--farmer ENDPOINT] [--reward-vote -1|0|1] [-c CONFIG]...
  [--opencl-device INDEX|-1] [--opencl-platform NAME]
  [--p2p-listen HOST:PORT] [--rpc-listen HOST:PORT] [--http-listen HOST:PORT]

Defaults: --mmx-root ../mmx-node --network mainnet --data-dir ./mmx-data
  P2P 127.0.0.1:<network port>, VNX RPC 127.0.0.1:11331,
  HTTP JSON-RPC http://127.0.0.1:11380/rpc.
OpenCL GPU 0 is used when available, with CPU fallback; --opencl-device -1 disables it.
Historical sync uses configured native seed peers unless --peer is supplied.
--offline serves the stored ledger without contacting peers.
--sync-once exits after coordinated sync. Sync requires 3–8 distinct synced peers. --rollback-to keeps that height.
History is retained by default. --purge-before reclaims older history before serving,
keeps HEIGHT available, and prevents RPC reads or rollback below HEIGHT (native databases).";
struct Options {
    configs: Vec<PathBuf>,
    no_services: bool,
    farmer: Option<String>,
    reward_vote: Option<i8>,
    root: PathBuf,
    network: String,
    data: PathBuf,
    peers: Vec<String>,
    offline: bool,
    once: bool,
    rollback: Option<u32>,
    purge: Option<u32>,
    p2p: Option<String>,
    rpc: String,
    http: String,
    opencl_device: Option<i32>,
    opencl_platform: Option<String>,
}
fn options() -> Result<Option<Options>> {
    let mut o = Options {
        configs: vec![],
        no_services: false,
        farmer: None,
        reward_vote: None,
        root: if std::path::Path::new("config").is_dir() && std::path::Path::new("data").is_dir() {
            ".".into()
        } else {
            "../mmx-node".into()
        },
        network: "mainnet".into(),
        data: "mmx-data".into(),
        peers: vec![],
        offline: false,
        once: false,
        rollback: None,
        purge: None,
        p2p: None,
        rpc: "127.0.0.1:11331".into(),
        http: "127.0.0.1:11380".into(),
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
                    "--network" => o.network = value,
                    "--data-dir" => o.data = value.into(),
                    "--peer" => o.peers.push(value),
                    "--rollback-to" => o.rollback = Some(value.parse()?),
                    "--purge-before" => o.purge = Some(value.parse()?),
                    "--p2p-listen" => o.p2p = Some(value),
                    "--rpc-listen" => o.rpc = value,
                    "--http-listen" => o.http = value,
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
    Ok(Some(o))
}
fn main() -> Result<()> {
    let Some(mut options) = options()? else {
        return Ok(());
    };
    let config = vnx::config::Config::new();
    let config_paths = if options.configs.is_empty() {
        vec![options.root.join("config").join(&options.network)]
    } else {
        options.configs.clone()
    };
    for path in &config_paths {
        config.load_tree(path)?;
    }
    let network = Network::from_config(&config, options.root.join("data"))?;
    let remote_harvesters = config.read::<bool>("allow_remote")?.unwrap_or(false)
        || config
            .read::<String>("Farmer.harvester_endpoint")?
            .is_some();
    if !options.configs.is_empty()
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
        if options.peers.is_empty() {
            return Err(std::io::Error::other(
                "no seed peers configured; specify --peer or --offline",
            )
            .into());
        }
    }
    let port = u16::try_from(network.params.port)?;
    let peers: Vec<_> = options
        .peers
        .iter()
        .map(|s| {
            if s.contains(':') {
                s.clone()
            } else {
                format!("{s}:{port}")
            }
        })
        .collect();
    let mut state = Node::open(&options.data, &network)?;
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
    let mut p2p = peer::Server::bind(
        &options.p2p.unwrap_or_else(|| format!("127.0.0.1:{port}")),
        node.clone(),
    )?;
    let mut vnx = rpc::vnx_server(options.rpc.parse()?, node.clone())?;
    let mut http = rpc::HttpServer::bind(&options.http, node.clone())?;
    println!(
        "P2P {}, VNX RPC {}, HTTP http://{}/rpc",
        p2p.address(),
        vnx.endpoint(),
        http.address()
    );
    #[cfg(unix)]
    let mut services =
        if !options.offline && !options.once && !options.no_services && !options.configs.is_empty()
        {
            let paths = config_paths.clone();
            Some(services::Services::start(
                &config,
                &paths,
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
    p2p.shutdown();
    http.shutdown();
    vnx.shutdown();
    for worker in workers {
        let _ = worker.join();
    }
    println!("Node stopped");
    Ok(())
}

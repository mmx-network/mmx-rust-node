use mmx_execution::chain::network::Network;
use mmx_node::{peer, rpc, sync, Node, Result};
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
  [--p2p-listen HOST:PORT] [--rpc-listen HOST:PORT] [--http-listen HOST:PORT]

Defaults: --mmx-root ../mmx-node --network mainnet --data-dir ./mmx-data
  P2P 127.0.0.1:<network port>, VNX RPC 127.0.0.1:11331,
  HTTP JSON-RPC http://127.0.0.1:11380/rpc.
Historical sync uses configured native seed peers unless --peer is supplied.
--offline serves the stored ledger without contacting peers.
--sync-once exits after coordinated sync. Sync requires 3–8 distinct synced peers. --rollback-to keeps that height.
History is retained by default. --purge-before reclaims older history before serving,
keeps HEIGHT available, and prevents RPC reads or rollback below HEIGHT (native databases).";
struct Options {
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
}
fn options() -> Result<Option<Options>> {
    let mut o = Options {
        root: "../mmx-node".into(),
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
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--offline" => o.offline = true,
            "--sync-once" => o.once = true,
            _ => {
                let value = args
                    .next()
                    .ok_or_else(|| std::io::Error::other(format!("missing value for {arg}")))?;
                match arg.as_str() {
                    "--mmx-root" => o.root = value.into(),
                    "--network" => o.network = value,
                    "--data-dir" => o.data = value.into(),
                    "--peer" => o.peers.push(value),
                    "--rollback-to" => o.rollback = Some(value.parse()?),
                    "--purge-before" => o.purge = Some(value.parse()?),
                    "--p2p-listen" => o.p2p = Some(value),
                    "--rpc-listen" => o.rpc = value,
                    "--http-listen" => o.http = value,
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
    let network = Network::load(&options.root, &options.network)?;
    if options.peers.is_empty() && !options.offline {
        let config = vnx::config::Config::new();
        config.load_tree(options.root.join("config").join(&options.network))?;
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
    state.enable_background_vdf()?;
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
        thread::sleep(Duration::from_millis(200));
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

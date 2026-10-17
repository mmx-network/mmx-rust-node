use mmx_execution::chain::network::Network;
use mmx_node::{peer, rpc, sync, Node, Result};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "mmx-node [--mmx-root PATH] [--network mainnet] [--data-dir PATH]
  [--peer HOST[:PORT]]... [--offline] [--sync-once] [--rollback-to HEIGHT]
  [--p2p-listen HOST:PORT] [--rpc-listen HOST:PORT] [--http-listen HOST:PORT]

Defaults: --mmx-root ../mmx-node --network mainnet --data-dir ./mmx-data
  P2P 127.0.0.1:<network port>, VNX RPC 127.0.0.1:11331,
  HTTP JSON-RPC http://127.0.0.1:11380/rpc.
Historical sync uses configured native seed peers unless --peer is supplied.
--offline serves the stored ledger without contacting peers.
--sync-once exits after reaching a peer tip. --rollback-to keeps that height.
All heights remain available for rollback; databases retain every ledger version.";
struct Options {
    root: PathBuf,
    network: String,
    data: PathBuf,
    peers: Vec<String>,
    offline: bool,
    once: bool,
    rollback: Option<u32>,
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
    let port = network.params.port;
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
        // More than one peer can contribute competing branches; ledger updates
        // are serialized while network requests run outside the node lock.
        for address in peers.into_iter().take(8) {
            let node = node.clone();
            let stop = stop.clone();
            let once = options.once;
            workers.push(thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let attempt = || -> Result<()> {
                        let mut peer = peer::Client::connect(&address, node.clone())?;
                        println!("Connected to {address}");
                        let mut queried = Instant::now() - Duration::from_secs(10);
                        let mut changed = Instant::now();
                        let mut last_height = 0;
                        while !stop.load(Ordering::Relaxed) {
                            if !node.lock().unwrap().is_synced() {
                                let progress = sync::synchronize(&mut peer, &node, &stop, |p| {
                                    println!(
                                        "Sync {address}: {} / {}, downloaded {} since fork {}, {:.1} blocks/s; download {:.3}s archive {:.3}s update {:.3}s (proofs {:.3}s replay {:.3}s)",
                                        p.selected, p.target, p.downloaded, p.common,
                                        f64::from(p.downloaded) / p.elapsed.as_secs_f64(),
                                        p.download_time.as_secs_f64(), p.archive_time.as_secs_f64(),
                                        p.update_time.as_secs_f64(), p.verification_time.as_secs_f64(),
                                        p.replay_time.as_secs_f64()
                                    )
                                })?;
                                println!(
                                    "Peer {address}: selected {} / {}",
                                    progress.selected, progress.target
                                );
                                if node.lock().unwrap().is_synced() && once {
                                    stop.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                            peer.poll()?;
                            if queried.elapsed() > Duration::from_secs(10) {
                                let remote = peer.height()?;
                                let mut n = node.lock().unwrap();
                                let current =
                                    n.chain().selected().tip()?.ok_or("missing tip")?.height;
                                if current != last_height {
                                    changed = Instant::now();
                                    last_height = current;
                                }
                                if remote > current && changed.elapsed() > Duration::from_secs(60) {
                                    // Same sync-loss recovery as native Node: historical
                                    // admission is re-enabled only after losing live progress.
                                    n.begin_resync();
                                }
                                queried = Instant::now();
                            }
                        }
                        Ok(())
                    };
                    if let Err(e) = attempt() {
                        if !stop.load(Ordering::Relaxed) {
                            eprintln!("Peer {address}: {e}");
                        }
                    }
                    for _ in 0..20 {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }));
        }
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

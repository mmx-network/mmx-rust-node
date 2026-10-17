//! Benchmark-only native peer serving a previously validated, frozen snapshot.
//! Independent identities allow the production three-peer scheduler to be used
//! without a single-peer exception or an external moving mainnet tip.
use mmx_consensus::{Hash, Record};
use mmx_execution::chain::network::Network;
use mmx_node::{
    peer, rpc,
    sync::{self, Source},
    Node, Result,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

struct Snapshot(Arc<Mutex<Node>>);
impl Source for Snapshot {
    fn height(&mut self) -> Result<u32> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .chain()
            .selected()
            .tip()?
            .ok_or("missing tip")?
            .height)
    }
    fn hash_at(&mut self, height: u32) -> Result<Option<Hash>> {
        Ok(self.0.lock().unwrap().chain().selected().hash_at(height)?)
    }
    fn block_at(&mut self, _: u32) -> Result<Option<Record>> {
        Err("snapshot must not download any blocks".into())
    }
}
fn main() -> Result<()> {
    let mut root = PathBuf::from("../mmx-node");
    let mut data = None;
    let mut height = None;
    let mut p2p = "127.0.0.1:0".to_owned();
    let mut http = "127.0.0.1:0".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--offline" {
            continue;
        }
        let value = args.next().ok_or("missing snapshot argument")?;
        match arg.as_str() {
            "--mmx-root" => root = value.into(),
            "--data-dir" => data = Some(PathBuf::from(value)),
            "--rollback-to" => height = Some(value.parse()?),
            "--p2p-listen" => p2p = value,
            "--http-listen" => http = value,
            "--rpc-listen" => (), // Common benchmark launcher; VNX RPC is unused.
            _ => return Err(format!("unknown snapshot argument {arg}").into()),
        }
    }
    let network = Network::load(root, "mainnet")?;
    let mut state = Node::open(data.ok_or("missing --data-dir")?, &network)?;
    state.rollback_to(height.ok_or("missing --rollback-to")?)?;
    let node = Arc::new(Mutex::new(state));
    let stop = Arc::new(AtomicBool::new(false));
    // Rebuild the selected verification window from our own durable archive.
    // This source cannot download anything or alter the stored selected tip.
    sync::synchronize(&mut Snapshot(node.clone()), &node, &stop, |_| {})?;
    let mut server = peer::Server::bind(&p2p, node.clone())?;
    let mut http = rpc::HttpServer::bind(&http, node)?;
    println!(
        "P2P {}, VNX RPC disabled, HTTP http://{}/rpc",
        server.address(),
        http.address()
    );
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    http.shutdown();
    server.shutdown();
    Ok(())
}

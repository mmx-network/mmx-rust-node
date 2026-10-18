//! Native VNX reads and bounded farming topics, plus a JSON-RPC 2.0 HTTP
//! presentation of the reads. Peer block transport retains lossless records.
use crate::{error, Node, Result};
use mmx_consensus::Record;
use std::{
    io::{Cursor, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use vnx::{
    binary::{Reader, Writer},
    net::{Endpoint, Server, ServerOptions},
    runtime::{Methods, OverflowPolicy, Runtime, Session},
    serde_json::{self, json, Value},
    VnxType,
};

pub const METHODS: &[&str] = &[
    "get_params",
    "get_height",
    "get_vdf_height",
    "get_synced_height",
    "get_synced_vdf_height",
    "get_vdf_peak",
    "get_genesis_hash",
    "get_block",
    "get_block_at",
    "get_header",
    "get_header_at",
    "get_block_hash",
    "get_block_hash_ex",
    "get_transaction",
    "get_transactions",
    "get_tx_height",
    "get_tx_ids_at",
    "get_contract",
    "get_contract_for",
    "get_balance",
];

fn dispatch(node: &Arc<Mutex<Node>>, method: &str, mut args: Value) -> Result<Value> {
    if !args.is_object() {
        return Err(error("RPC parameters must be an object"));
    }
    if !METHODS.contains(&method) {
        return Err(error(format!("unsupported read method: {method}")));
    }
    args["__type"] = json!(format!("mmx.Node.{method}"));
    let registry = mmx_types::vnx_registry()?;
    let schema = registry.by_name(&format!("mmx.Node.{method}"))?;
    for field in &schema.fields {
        if args.get(&field.name).is_none() && field.value.is_empty() {
            return Err(error(format!("missing parameter: {}", field.name)));
        }
    }
    let mut writer = Writer::new(Vec::new(), registry.clone());
    writer.write(&args)?;
    let request = Record::decode(&writer.into_inner())?;
    let response = node
        .lock()
        .map_err(|_| error("node lock poisoned"))?
        .call(&request)?
        .encode()?;
    let response = Reader::new(Cursor::new(response), registry)
        .with_utf8_lossy(true)
        .read()?;
    Ok(response["_ret_0"].clone())
}
pub struct VnxServer {
    server: Server,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl VnxServer {
    pub fn endpoint(&self) -> &Endpoint {
        self.server.endpoint()
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.server.shutdown();
    }
}
impl Drop for VnxServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
pub fn vnx_server(endpoint: Endpoint, node: Arc<Mutex<Node>>) -> Result<VnxServer> {
    let runtime = Runtime::new()?;
    let mut methods = Methods::new();
    for &method in METHODS {
        let node = node.clone();
        methods.add(
            format!("mmx.Node.{method}"),
            Some("mmx.permission_e.PUBLIC".into()),
            move |args, _| {
                dispatch(&node, method, args).map_err(|e| vnx::Error::Remote(e.to_string()))
            },
        );
    }
    runtime.register("Node", methods)?;
    let incoming = runtime.subscribe("farmer.proof", 64, OverflowPolicy::DropOldest, false)?;
    let inbox = node
        .lock()
        .map_err(|_| error("node lock poisoned"))?
        .inbox();
    let server = Server::bind(
        endpoint,
        runtime.clone(),
        mmx_types::vnx_registry()?,
        ServerOptions {
            services: ["Node".into()].into(),
            session: Session::new(
                "public",
                [
                    "mmx.permission_e.PUBLIC".into(),
                    "vnx.permission_e.VIEW".into(),
                    "vnx.permission_e.PUBLISH".into(),
                ],
            ),
            export_topics: ["harvester.challenges".into()].into(),
            publish_topics: ["farmer.proof".into()].into(),
            ..Default::default()
        },
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let running = stop.clone();
    let worker = thread::spawn(move || {
        let mut published = std::collections::BTreeSet::new();
        let mut checked = std::time::Instant::now() - Duration::from_secs(1);
        while !running.load(Ordering::Relaxed) {
            while let Ok(Some(sample)) = incoming.try_recv() {
                if sample.replay {
                    continue;
                }
                let decoded = (|| -> Result<()> {
                    let mut writer = Writer::new(Vec::new(), mmx_types::vnx_registry()?);
                    writer.write(&sample.value)?;
                    let bytes = writer.into_inner();
                    if bytes.len() > 64 * 1024 {
                        return Err(error("farmer proof exceeds 64 KiB"));
                    }
                    let record = Record::decode(&bytes)?;
                    if record.name != "mmx.ProofResponse" {
                        return Err(error("expected farmer proof response"));
                    }
                    inbox.submit(record, bytes.len())
                })();
                if let Err(e) = decoded {
                    eprintln!("Farmer proof handoff: {e}");
                }
            }
            if checked.elapsed() >= Duration::from_secs(1) {
                if let Ok(n) = node.try_lock() {
                    match n.farming_challenges() {
                        Ok(challenges) => {
                            let mut current = std::collections::BTreeSet::new();
                            for challenge in challenges {
                                let identity = (
                                    challenge.vdf_height,
                                    challenge.challenge.0,
                                    challenge.difficulty,
                                );
                                current.insert(identity);
                                if !published.contains(&identity) {
                                    match challenge.to_value().and_then(|v| {
                                        runtime.publish("harvester.challenges", v, false)
                                    }) {
                                        Ok(()) => (),
                                        Err(e) => eprintln!("Challenge publication: {e}"),
                                    }
                                }
                            }
                            published = current;
                        }
                        Err(e) => eprintln!("Farming challenge lookup: {e}"),
                    }
                }
                checked = std::time::Instant::now();
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    Ok(VnxServer {
        server,
        stop,
        worker: Some(worker),
    })
}

fn request(node: &Arc<Mutex<Node>>, r: Value) -> Option<Value> {
    let id = r.get("id").cloned().unwrap_or(Value::Null);
    let failure = |code, message: String| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
    if r["jsonrpc"] != "2.0"
        || !r["method"].is_string()
        || !(id.is_null() || id.is_string() || id.is_number())
    {
        return Some(failure(-32600, "Invalid Request".into()));
    }
    let method = r["method"]
        .as_str()
        .unwrap()
        .strip_prefix("mmx.Node.")
        .unwrap_or(r["method"].as_str().unwrap());
    let result = if method == "get_status" {
        (|| -> Result<Value> {
            let n = node.lock().map_err(|_| error("node lock poisoned"))?;
            let tip = n
                .chain
                .selected()
                .tip()?
                .ok_or_else(|| error("missing node tip"))?;
            let inbox = n.inbox().stats()?;
            Ok(
                json!({"height":tip.height,"vdf_height":tip.vdf_height,"root_height":n.root,"earliest_height":n.earliest_height(),
                "synced":n.synced,"sync_peers":n.sync_peers,"network":n.chain.selected().params().network,
                "sync_body_requests":n.sync_body_requests,"sync_body_retries":n.sync_body_retries,
                "sync_backpressure_retries":n.sync_backpressure_retries,
                "hash":vnx::util::to_hex_string(&tip.hash.0,false,false),
                "queued_messages":inbox.messages,"queued_bytes":inbox.bytes,"dropped_messages":inbox.dropped}),
            )
        })()
    } else {
        dispatch(
            node,
            method,
            r.get("params").cloned().unwrap_or_else(|| json!({})),
        )
    };
    r.get("id")?;
    Some(match result {
        Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
        Err(e) => failure(
            if METHODS.contains(&method) {
                -32000
            } else {
                -32601
            },
            e.to_string(),
        ),
    })
}
pub fn json_request(node: &Arc<Mutex<Node>>, body: &[u8]) -> Option<Value> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Array(items)) if !items.is_empty() => {
            let replies: Vec<_> = items.into_iter().filter_map(|r| request(node, r)).collect();
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        Ok(r) => request(node, r),
        Err(_) => {
            Some(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}))
        }
    }
}
pub struct HttpServer {
    address: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl HttpServer {
    pub fn bind(address: &str, node: Arc<Mutex<Node>>) -> Result<Self> {
        let server = tiny_http::Server::http(address)?;
        let address = server.server_addr().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let thread = thread::spawn(move || {
            while !running.load(Ordering::Relaxed) {
                let mut req = match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(r)) => r,
                    Ok(None) => continue,
                    Err(_) => break,
                };
                let response = if req.method() != &tiny_http::Method::Post {
                    tiny_http::Response::from_string("POST JSON-RPC 2.0 requests to /rpc\n")
                        .with_status_code(405)
                } else if req.url() != "/rpc" && req.url() != "/" {
                    tiny_http::Response::from_string("Not found").with_status_code(404)
                } else {
                    let mut body = Vec::new();
                    let parsed = if req
                        .as_reader()
                        .take(1024 * 1024 + 1)
                        .read_to_end(&mut body)
                        .is_err()
                        || body.len() > 1024 * 1024
                    {
                        Some(
                            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Request body exceeds 1 MiB or is truncated"}}),
                        )
                    } else {
                        json_request(&node, &body)
                    };
                    match parsed {
                        Some(v) => tiny_http::Response::from_string(v.to_string()),
                        None => tiny_http::Response::from_string("").with_status_code(204),
                    }
                };
                let response = response.with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
                let _ = req.respond(response);
            }
        });
        Ok(Self {
            address,
            stop,
            thread: Some(thread),
        })
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

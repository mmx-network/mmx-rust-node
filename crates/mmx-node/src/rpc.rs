//! Native VNX ledger RPCs, transaction admission and bounded farming topics,
//! plus a JSON-RPC 2.0 HTTP presentation. Peer block transport retains lossless records.
use crate::{error, Node, Result};
pub(crate) mod cache;
pub(crate) mod history;
pub use cache::Stats as CacheStats;
use mmx_consensus::Record;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vnx::{
    binary::{Reader, Writer},
    net::{Endpoint, Server, ServerOptions},
    runtime::{Methods, OverflowPolicy, Runtime, Session},
    serde_json::{self, json, Value},
    VnxType,
};

pub(crate) fn from_json_record(value: &Value) -> Result<Record> {
    let mut writer = Writer::new(Vec::new(), mmx_types::vnx_registry()?);
    writer.write(value)?;
    Ok(Record::decode(&writer.into_inner())?)
}
pub(crate) fn to_json_record(record: &Record) -> Result<Value> {
    Ok(Reader::new(Cursor::new(record.encode()?), mmx_types::vnx_registry()?).read()?)
}
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
    "get_history",
    "get_history_memo",
    "get_contract",
    "get_contract_for",
    "get_balance",
    "get_all_balances",
    "get_balances",
    "get_total_balance",
    "get_total_balances",
    "validate",
    "add_transaction",
];

// Shared by HTTP and native RPC. Capture only immutable database handles while
// holding the ledger lock; query work and encoding run on the request's worker.
pub(crate) struct ReadPermit(Arc<AtomicUsize>);
impl ReadPermit {
    pub(crate) fn acquire(workers: &Arc<AtomicUsize>) -> Result<Self> {
        let mut current = workers.load(Ordering::Acquire);
        loop {
            if current >= 2 {
                return Err(error("node RPC workers busy"));
            }
            match workers.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self(workers.clone())),
                Err(observed) => current = observed,
            }
        }
    }
}
impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct ReadTask {
    storage: mmx_vm::StorageDb,
    context: mmx_execution::transaction::TransactionContext,
    _permit: ReadPermit,
}
impl ReadTask {
    fn capture(node: &Node) -> Result<Self> {
        let permit = ReadPermit::acquire(&node.rpc_workers)?;
        let selected = node.chain.selected();
        let db = selected.storage().database();
        Ok(Self {
            storage: mmx_vm::StorageDb::new(db.fork(db.version())?),
            context: mmx_execution::transaction::TransactionContext {
                height: selected
                    .tip()?
                    .ok_or_else(|| error("missing node tip"))?
                    .height
                    .checked_add(1)
                    .ok_or_else(|| error("height overflow"))?,
                params: selected.params().clone(),
                locked_contracts: None,
            },
            _permit: permit,
        })
    }
    fn call(&mut self, request: &Record) -> Result<Record> {
        if request.name == "mmx.Node.validate" {
            Ok(crate::node::record(
                "mmx.Node.validate.return",
                [(
                    "_ret_0".into(),
                    mmx_consensus::Value::Record(
                        crate::transactions::validate_snapshot(
                            &mut self.storage,
                            &self.context,
                            request.field("tx")?.record()?,
                        )?
                        .to_record(),
                    ),
                )],
            ))
        } else {
            crate::transactions::balance_response(&self.storage, request)
        }
    }
}

fn lock_node_until(node: &Mutex<Node>, end: Instant) -> Result<std::sync::MutexGuard<'_, Node>> {
    loop {
        if Instant::now() >= end {
            return Err(error("RPC request deadline"));
        }
        match node.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(error("node lock poisoned")),
            Err(std::sync::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(1)),
        }
    }
}
enum Payload {
    Json(Value),
    Serialized(Vec<u8>),
}
impl Payload {
    fn into_json(self) -> Result<Value> {
        match self {
            Self::Json(value) => Ok(value),
            Self::Serialized(bytes) => Ok(serde_json::from_slice(&bytes)?),
        }
    }
    fn into_bytes(self) -> Result<Vec<u8>> {
        match self {
            Self::Serialized(bytes) => Ok(bytes),
            Self::Json(value) => Ok(serde_json::to_vec(&value)?),
        }
    }
}
#[cfg(test)]
fn dispatch(node: &Arc<Mutex<Node>>, method: &str, args: Value) -> Result<Value> {
    dispatch_payload_until(
        node,
        method,
        args,
        Instant::now() + Duration::from_secs(5),
        true,
        None,
    )?
    .into_json()
}
fn dispatch_payload_until(
    node: &Arc<Mutex<Node>>,
    method: &str,
    mut args: Value,
    end: Instant,
    public_http: bool,
    front_cache: Option<&cache::Shared>,
) -> Result<Payload> {
    if !args.is_object() {
        return Err(error("RPC parameters must be an object"));
    }
    if !METHODS.contains(&method) {
        return Err(error(format!("unsupported node method: {method}")));
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
    if public_http {
        if let Some(cache) = front_cache {
            if Instant::now() >= end {
                return Err(error("RPC request deadline"));
            }
            if let Some(bytes) = cache::lookup(cache, &request)? {
                return Ok(Payload::Serialized(bytes));
            }
        }
    }
    if method == "add_transaction" {
        let task = {
            let n = lock_node_until(node, end)?;
            n.capture_admission(
                request.field("tx")?.record()?,
                request.field("pre_validate")?.boolean()?,
                true,
            )?
        };
        if let Some(mut task) = task {
            let result = task.validate()?;
            lock_node_until(node, end)?.publish_transaction(task, result)?;
        }
        return Ok(Payload::Json(Value::Null));
    }
    let (response, fill) = {
        let n = lock_node_until(node, end)?;
        let (cached, fill) = if public_http {
            cache::probe(&n, &request)?
        } else {
            (None, None)
        };
        if let Some(response) = cached {
            drop(n);
            return Ok(Payload::Serialized(response));
        } else {
            let response = if history::is_method(&request.name) {
                let task = history::Task::capture(&n, &request)?;
                drop(n);
                task.call(&request, end)?
            } else if method == "validate" || crate::transactions::is_balance_method(&request.name)
            {
                let mut task = ReadTask::capture(&n)?;
                drop(n);
                task.call(&request)?
            } else {
                let response = n.call(&request)?;
                drop(n);
                response
            };
            if response.memory_size() > MAX_RESPONSE_BYTES {
                return Err(error("RPC response exceeds byte budget"));
            }
            (response, fill)
        }
    };
    let value = response_json(&response)?;
    if public_http {
        let bytes = serde_json::to_vec(&value)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(error("RPC response exceeds byte budget"));
        }
        if let Some(fill) = fill {
            fill.finish_json(&bytes);
        }
        Ok(Payload::Serialized(bytes))
    } else {
        Ok(Payload::Json(value))
    }
}
pub(crate) fn response_json(response: &Record) -> Result<Value> {
    if response.memory_size() > MAX_RESPONSE_BYTES {
        return Err(error("RPC response exceeds byte budget"));
    }
    let response = response.encode()?;
    if response.len() > MAX_RESPONSE_BYTES {
        return Err(error("RPC response exceeds byte budget"));
    }
    let mut response = Reader::new(Cursor::new(response), mmx_types::vnx_registry()?)
        .with_limits(vnx::binary::Limits {
            max_allocated_bytes: MAX_RESPONSE_BYTES,
            ..Default::default()
        })
        .with_utf8_lossy(true)
        .read()?;
    Ok(response
        .as_object_mut()
        .and_then(|v| v.remove("_ret_0"))
        .unwrap_or(Value::Null))
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
            Some(
                if method == "add_transaction" {
                    "vnx.permission_e.PUBLISH"
                } else {
                    "mmx.permission_e.PUBLIC"
                }
                .into(),
            ),
            move |args, _| {
                dispatch_payload_until(
                    &node,
                    method,
                    args,
                    Instant::now() + Duration::from_secs(5),
                    false,
                    None,
                )
                .and_then(Payload::into_json)
                .map_err(|e| vnx::Error::Remote(e.to_string()))
            },
        );
    }
    runtime.register("Node", methods)?;
    let incoming = runtime.subscribe("farmer.proof", 64, OverflowPolicy::DropOldest, false)?;
    let timelord = runtime.subscribe("timelord.proof", 16, OverflowPolicy::DropOldest, false)?;
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
            export_topics: ["harvester.challenges".into(), "timelord.requests".into()].into(),
            publish_topics: ["farmer.proof".into(), "timelord.proof".into()].into(),
            ..Default::default()
        },
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let running = stop.clone();
    let worker = thread::spawn(move || {
        let mut published = std::collections::BTreeSet::new();
        let mut checked = std::time::Instant::now() - Duration::from_secs(1);
        let mut interval_stamp = None;
        let mut interval_requests = Vec::<mmx_types::mmx::IntervalRequest>::new();
        let mut interval_sent = std::time::Instant::now() - Duration::from_secs(2);
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
            for _ in 0..16 {
                let Ok(Some(sample)) = timelord.try_recv() else {
                    break;
                };
                if sample.replay {
                    continue;
                }
                let decoded = (|| -> Result<()> {
                    let record = from_json_record(&sample.value)?;
                    if record.name != "mmx.ProofOfTime" {
                        return Err(error("expected timelord proof of time"));
                    }
                    let size = record.memory_size().max(record.encode_compact()?.len());
                    if size > 32 * 1024 * 1024 {
                        return Err(error("timelord proof exceeds 32 MiB"));
                    }
                    // Local/remote timelords receive no trust shortcut: normal
                    // envelope, signature, history and work checks run on tick.
                    inbox.submit(record, size)
                })();
                if let Err(e) = decoded {
                    eprintln!("Timelord proof handoff: {e}");
                }
            }
            if let Ok(n) = node.try_lock() {
                let stamp = (n.synced, n.timelord_tip, n.timelord_revision);
                if interval_stamp != Some(stamp) {
                    match n.timelord_requests() {
                        Ok(requests) => {
                            interval_requests = requests;
                            interval_stamp = Some(stamp);
                            interval_sent = std::time::Instant::now() - Duration::from_secs(2);
                        }
                        Err(e) => eprintln!("Timelord interval lookup: {e}"),
                    }
                }
            }
            if interval_sent.elapsed() >= Duration::from_secs(2) {
                for request in &interval_requests {
                    if let Err(e) = request
                        .to_value()
                        .and_then(|v| runtime.publish("timelord.requests", v, false))
                    {
                        eprintln!("Timelord interval publication: {e}");
                    }
                }
                interval_sent = std::time::Instant::now();
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

fn request_bytes(
    node: &Arc<Mutex<Node>>,
    r: Value,
    end: Instant,
    front_cache: Option<&cache::Shared>,
) -> Option<Vec<u8>> {
    let id = r.get("id").cloned().unwrap_or(Value::Null);
    if !bounded_id(&id) {
        return Some(limit_error_bytes(Value::Null, "RPC id exceeds 128 bytes"));
    }
    let failure = |code, message: String| json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}});
    if r["jsonrpc"] != "2.0"
        || !r["method"].is_string()
        || !(id.is_null() || id.is_string() || id.is_number())
    {
        return Some(
            failure(-32600, "Invalid Request".into())
                .to_string()
                .into_bytes(),
        );
    }
    let method = r["method"]
        .as_str()
        .unwrap()
        .strip_prefix("mmx.Node.")
        .unwrap_or(r["method"].as_str().unwrap());
    let result = if method == "get_status" {
        (|| -> Result<Value> {
            let n = lock_node_until(node, end)?;
            let tip = n
                .chain
                .selected()
                .tip()?
                .ok_or_else(|| error("missing node tip"))?;
            let inbox = n.inbox().stats()?;
            let cache = n.rpc_cache_stats();
            Ok(
                json!({"height":tip.height,"vdf_height":tip.vdf_height,"root_height":n.root,"earliest_height":n.earliest_height(),
                "pending_transactions":n.pending_transactions(),"synced":n.synced,"sync_peers":n.sync_peers,"network":n.chain.selected().params().network,
                "sync_body_requests":n.sync_body_requests,"sync_body_retries":n.sync_body_retries,
                "sync_backpressure_retries":n.sync_backpressure_retries,
                "hash":vnx::util::to_hex_string(&tip.hash.0,false,false),
                "queued_messages":inbox.messages,"queued_bytes":inbox.bytes,"dropped_messages":inbox.dropped,
                "rpc_cache":{"limit_bytes":cache.limit_bytes,"used_bytes":cache.used_bytes,"entries":cache.entries,
                    "hits":cache.hits,"misses":cache.misses,"evictions":cache.evictions}}),
            )
        })().and_then(|value| Ok(serde_json::to_vec(&value)?))
    } else {
        dispatch_payload_until(
            node,
            method,
            r.get("params").cloned().unwrap_or_else(|| json!({})),
            end,
            true,
            front_cache,
        )
        .and_then(Payload::into_bytes)
    };
    r.get("id")?;
    Some(match result {
        Ok(value) => {
            // The result is already serialized JSON from the cache or miss
            // handler. Only the request-specific envelope needs encoding.
            let mut reply = b"{\"jsonrpc\":\"2.0\",\"id\":".to_vec();
            serde_json::to_writer(&mut reply, &id).expect("JSON value serialization");
            reply.extend_from_slice(b",\"result\":");
            reply.extend_from_slice(&value);
            reply.push(b'}');
            reply
        }
        Err(e) => failure(
            if METHODS.contains(&method) {
                -32000
            } else {
                -32601
            },
            e.to_string(),
        )
        .to_string()
        .into_bytes(),
    })
}
const MAX_BATCH_CALLS: usize = 32;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
fn bounded_id(id: &Value) -> bool {
    // Arbitrary-precision numeric IDs and escaped strings also need a wire
    // bound, so the reserved final-error space cannot be bypassed.
    serde_json::to_writer(ResponseBudget(128), id).is_ok()
}
fn limit_error(id: Value, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}})
}
fn limit_error_bytes(id: Value, message: &str) -> Vec<u8> {
    limit_error(id, message).to_string().into_bytes()
}
struct ResponseBudget(usize);
impl std::io::Write for ResponseBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::other("RPC response exceeds byte budget"));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn json_request(node: &Arc<Mutex<Node>>, body: &[u8]) -> Option<Value> {
    json_request_until(node, body, Instant::now() + Duration::from_secs(5))
}
pub(crate) fn json_request_until(
    node: &Arc<Mutex<Node>>,
    body: &[u8],
    end: Instant,
) -> Option<Value> {
    json_response_until(node, body, end, None)
        .map(|bytes| serde_json::from_slice(&bytes).expect("generated JSON-RPC response"))
}
pub(crate) fn json_response_until(
    node: &Arc<Mutex<Node>>,
    body: &[u8],
    end: Instant,
    front_cache: Option<&cache::Shared>,
) -> Option<Vec<u8>> {
    json_response_budget(node, body, end, MAX_RESPONSE_BYTES, front_cache)
}
#[cfg(test)]
fn json_request_budget(
    node: &Arc<Mutex<Node>>,
    body: &[u8],
    end: Instant,
    bytes: usize,
) -> Option<Value> {
    json_response_budget(node, body, end, bytes, None)
        .map(|bytes| serde_json::from_slice(&bytes).expect("generated JSON-RPC response"))
}
fn json_response_budget(
    node: &Arc<Mutex<Node>>,
    body: &[u8],
    end: Instant,
    bytes: usize,
    front_cache: Option<&cache::Shared>,
) -> Option<Vec<u8>> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Array(items)) if !items.is_empty() => {
            if items.len() > MAX_BATCH_CALLS {
                return Some(limit_error_bytes(Value::Null, "RPC batch exceeds 32 calls"));
            }
            // Reserve room for punctuation and a final bounded error. Check
            // notifications too: they consume execution work without a reply.
            let mut budget = ResponseBudget(bytes.saturating_sub(512));
            let mut replies = Vec::new();
            for r in items {
                let id = r
                    .get("id")
                    .filter(|v| bounded_id(v))
                    .cloned()
                    .unwrap_or(Value::Null);
                if Instant::now() >= end {
                    replies.push(limit_error_bytes(id, "RPC request deadline"));
                    break;
                }
                if let Some(reply) = request_bytes(node, r, end, front_cache) {
                    if std::io::Write::write_all(&mut budget, &reply).is_err() {
                        replies.push(limit_error_bytes(id, "RPC response exceeds byte budget"));
                        break;
                    }
                    replies.push(reply);
                }
            }
            if replies.is_empty() {
                None
            } else {
                let mut out = vec![b'['];
                for (i, reply) in replies.into_iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    out.extend(reply);
                }
                out.push(b']');
                Some(out)
            }
        }
        Ok(r) => {
            if Instant::now() >= end {
                return Some(limit_error_bytes(Value::Null, "RPC request deadline"));
            }
            request_bytes(node, r, end, front_cache).map(|reply| {
                if reply.len() > bytes {
                    limit_error_bytes(Value::Null, "RPC response exceeds byte budget")
                } else {
                    reply
                }
            })
        }
        Err(_) => Some(
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}})
                .to_string()
                .into_bytes(),
        ),
    }
}
pub use crate::http::HttpServer;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mmx_consensus::Value as N;
    use mmx_execution::chain::{forks::ForkChain, genesis::Assets};
    use mmx_types::{mmx, VnxType};

    #[test]
    fn concurrent_rpc_permits_never_overbook_and_always_release() {
        let workers = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let active = Arc::new(AtomicUsize::new(0));
        thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..128 {
                        if let Ok(permit) = ReadPermit::acquire(&workers) {
                            assert!(active.fetch_add(1, Ordering::AcqRel) < 2);
                            thread::yield_now();
                            active.fetch_sub(1, Ordering::AcqRel);
                            drop(permit);
                        }
                    }
                });
            }
        });
        assert_eq!(workers.load(Ordering::Acquire), 0);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    pub(crate) fn fixture() -> (Arc<Mutex<Node>>, Record, Record) {
        fixture_at(None)
    }
    pub(crate) fn fixture_at(path: Option<&std::path::Path>) -> (Arc<Mutex<Node>>, Record, Record) {
        let rows: Vec<Value> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let raw = |v: &Value| vnx::util::from_hex_string(v.as_str().unwrap()).unwrap();
        let genesis = rows.iter().find(|r| r["name"] == "synthetic").unwrap();
        let params = mmx::ChainParams::decode(&raw(&genesis["params"])).unwrap();
        let assets = Assets {
            binaries: genesis["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| Record::decode(&raw(v)).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            testnet_rewards: None,
            project_relay: None,
        };
        let mut chain = match path {
            Some(path) => ForkChain::open_native(path, params).unwrap(),
            None => ForkChain::memory(params),
        };
        chain.initialize(&assets).unwrap();
        let step = &rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"][0];
        (
            Arc::new(Mutex::new(
                Node::from_chain(
                    chain,
                    k256::ecdsa::SigningKey::from_slice(&[1; 32]).unwrap(),
                )
                .unwrap(),
            )),
            Record::decode(&raw(&step["block"])).unwrap(),
            Record::decode(&raw(&step["vdf"])).unwrap(),
        )
    }
    #[test]
    fn batches_share_call_byte_and_execution_deadline_budgets() {
        let (node, _, _) = fixture();
        let call = json!({"jsonrpc":"2.0","id":1,"method":"get_block_at","params":{"height":0}});
        let body = serde_json::to_vec(&vec![call.clone(); 8]).unwrap();
        let reply = json_request_budget(
            &node,
            &body,
            Instant::now() + Duration::from_secs(5),
            20_000,
        )
        .unwrap();
        assert_eq!(reply.as_array().unwrap().len(), 2);
        assert!(reply[0]["result"].is_object());
        assert!(reply[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("byte budget"));
        assert!(serde_json::to_vec(&reply).unwrap().len() <= 20_000);
        let _locked = node.lock().unwrap();
        // Both checks happen before any dispatch, even for notifications.
        let notifications =
            serde_json::to_vec(&vec![json!({"jsonrpc":"2.0","method":"get_height"}); 33]).unwrap();
        assert!(
            json_request(&node, &notifications).unwrap()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("32 calls")
        );
        assert!(
            json_request_until(&node, &body, Instant::now()).unwrap()[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("deadline")
        );
        let numeric_id = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"get_height\"}}",
            "9".repeat(1024)
        );
        let oversized = json_request(&node, numeric_id.as_bytes()).unwrap();
        assert!(oversized["id"].is_null());
        assert!(oversized["error"]["message"]
            .as_str()
            .unwrap()
            .contains("128 bytes"));
        assert!(!bounded_id(&json!("\n".repeat(100))));
    }
    #[test]
    fn submission_executes_without_live_lock_and_rejects_changed_or_rewound_snapshots() {
        for disk in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (node, block, vdf) = fixture_at(disk.then_some(dir.path()));
            let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
                .record()
                .unwrap();
            let mut n = node.lock().unwrap();
            n.synced = true;
            let mut task = n.capture_admission(tx, true, true).unwrap().unwrap();
            let (sent, received) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                let result = task.validate().unwrap();
                sent.send((task, result)).unwrap();
            });
            // The worker must finish even while this thread owns the mutex.
            let (task, result) = received.recv_timeout(Duration::from_secs(3)).unwrap();
            worker.join().unwrap();
            let root = n.chain.selected().hash_at(0).unwrap().unwrap();
            n.chain.admit(&block, &[vdf]).unwrap();
            n.chain.update(root, None).unwrap();
            if disk {
                // Exercise Native's generation check even after an ABA rewind.
                n.chain.rollback_to(0).unwrap();
            }
            assert!(n
                .publish_transaction(task, result)
                .unwrap_err()
                .to_string()
                .contains("snapshot changed"));
            assert_eq!(n.pending_transactions(), 0);
            assert_eq!(n.rpc_workers.load(Ordering::Acquire), 0);
        }
    }
    #[test]
    fn snapshot_tasks_release_ledger_lock_retain_parent_and_bound_shared_workers() {
        let (node, block, vdf) = fixture();
        let mut first = ReadTask::capture(&node.lock().unwrap()).unwrap();
        let second = ReadTask::capture(&node.lock().unwrap()).unwrap();
        assert!(ReadTask::capture(&node.lock().unwrap()).is_err());
        assert!(dispatch(
            &node,
            "get_balances",
            json!({"address":vec![0;32], "whitelist":[], "limit":-1})
        )
        .unwrap_err()
        .to_string()
        .contains("workers busy"));
        assert_eq!(dispatch(&node, "get_height", json!({})).unwrap(), 0);
        let mut owner = [0u8; 32];
        owner[0] = 9;
        let request = from_json_record(&json!({
            "__type":"mmx.Node.get_all_balances", "addresses":vec![owner], "whitelist":[], "limit":-1
        })).unwrap();
        let before = first.call(&request).unwrap();
        let mut n = node.lock().unwrap();
        let root = n.chain.selected().hash_at(0).unwrap().unwrap();
        n.chain.admit(&block, &[vdf]).unwrap();
        n.chain.update(root, None).unwrap();
        assert_eq!(n.chain.selected().tip().unwrap().unwrap().height, 1);
        assert_ne!(n.call(&request).unwrap(), before);
        // Deliberately retain the live ledger lock during the entire query.
        let (sent, received) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            sent.send(first.call(&request).unwrap()).unwrap();
        });
        assert_eq!(
            received.recv_timeout(Duration::from_secs(3)).unwrap(),
            before
        );
        worker.join().unwrap();
        assert_eq!(second.context.height, 1);
        assert_eq!(second.storage.database().version(), 1);
        assert_eq!(n.rpc_workers.load(Ordering::Acquire), 1);
        drop(second);
        assert_eq!(n.rpc_workers.load(Ordering::Acquire), 0);
        drop(n);
        assert!(dispatch(
            &node,
            "get_balances",
            json!({"address":vec![0;32], "whitelist":[], "limit":-1})
        )
        .is_ok());
    }
    #[test]
    fn public_validation_uses_parent_snapshot_and_releases_slots_on_failure() {
        let (node, block, _) = fixture();
        let mut tx = block.field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap()
            .clone();
        tx.fields.insert("exec_result".into(), N::Optional(None));
        let params = node.lock().unwrap().chain.selected().params().clone();
        tx.fields.insert(
            "static_cost".into(),
            N::Uint(tx.calc_cost(&(&params).into()).unwrap()),
        );
        tx.fields.insert(
            "content_hash".into(),
            N::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
        );
        let expected = node
            .lock()
            .unwrap()
            .validate_transaction(&tx)
            .unwrap()
            .to_record();
        let result = dispatch(
            &node,
            "validate",
            json!({"tx": to_json_record(&tx).unwrap()}),
        )
        .unwrap();
        let mut expected_json = to_json_record(&expected).unwrap();
        expected_json.as_object_mut().unwrap().remove("__type");
        assert_eq!(result, expected_json);
        assert_eq!(node.lock().unwrap().rpc_workers.load(Ordering::Acquire), 0);
        tx.fields.insert(
            "exec_result".into(),
            N::Optional(Some(Box::new(N::Record(expected)))),
        );
        assert!(dispatch(
            &node,
            "validate",
            json!({"tx":to_json_record(&tx).unwrap()})
        )
        .unwrap_err()
        .to_string()
        .contains("exec_result not null"));
        assert_eq!(node.lock().unwrap().rpc_workers.load(Ordering::Acquire), 0);
    }
}

//! Native VNX ledger RPCs, transaction admission and bounded farming topics,
//! plus a JSON-RPC 2.0 HTTP presentation. Peer block transport retains lossless records.
use crate::{error, Node, Result};
use mmx_consensus::Record;
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
struct ReadPermit(Arc<AtomicUsize>);
impl ReadPermit {
    fn acquire(workers: &Arc<AtomicUsize>) -> Result<Self> {
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

fn dispatch(node: &Arc<Mutex<Node>>, method: &str, mut args: Value) -> Result<Value> {
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
    let mut task = if method == "validate" || crate::transactions::is_balance_method(&request.name)
    {
        let n = node.lock().map_err(|_| error("node lock poisoned"))?;
        Some(ReadTask::capture(&n)?)
    } else {
        None
    };
    let response = if let Some(task) = &mut task {
        task.call(&request)?.encode()?
    } else {
        let mut n = node.lock().map_err(|_| error("node lock poisoned"))?;
        if method == "add_transaction" {
            n.add_transaction(
                request.field("tx")?.record()?,
                request.field("pre_validate")?.boolean()?,
            )?;
            return Ok(Value::Null);
        }
        n.call(&request)?.encode()?
    };
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
            Some(
                if method == "add_transaction" {
                    "vnx.permission_e.PUBLISH"
                } else {
                    "mmx.permission_e.PUBLIC"
                }
                .into(),
            ),
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
                "pending_transactions":n.pending_transactions(),"synced":n.synced,"sync_peers":n.sync_peers,"network":n.chain.selected().params().network,
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
pub use crate::http::HttpServer;

#[cfg(test)]
mod tests {
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

    fn fixture() -> (Arc<Mutex<Node>>, Record, Record) {
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
        let mut chain = ForkChain::memory(params);
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

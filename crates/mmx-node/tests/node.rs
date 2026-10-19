use k256::ecdsa::SigningKey;
use mmx_consensus::{Hash, Record, Value as V};
use mmx_execution::chain::{forks::ForkChain, genesis::Assets, network::Network};
use mmx_node::{
    node::{hash, record},
    peer, rpc,
    sync::{self, Source},
    Node, Result,
};
use mmx_types::{mmx, VnxType};
use mmx_vm::Storage;
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Duration,
};
use vnx::serde_json::{self, json, Value};
fn raw(v: &Value) -> Vec<u8> {
    vnx::util::from_hex_string(v.as_str().unwrap()).unwrap()
}
fn wire(v: &Value) -> Record {
    Record::decode(&raw(v)).unwrap()
}
fn fixture() -> (Network, Value, Vec<Value>, Vec<Value>) {
    let rows: Vec<Value> = include_str!("../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let g = rows
        .iter()
        .find(|r| r["name"] == "synthetic")
        .unwrap()
        .clone();
    let steps = rows.iter().find(|r| r["kind"] == "chain").unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    let deep = rows.iter().find(|r| r["kind"] == "deep_fork").unwrap()["steps"]
        .as_array()
        .unwrap()
        .clone();
    let network = Network {
        params: mmx::ChainParams::decode(&raw(&g["params"])).unwrap(),
        assets: Assets {
            binaries: g["binaries"]
                .as_array()
                .unwrap()
                .iter()
                .map(wire)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            testnet_rewards: None,
            project_relay: None,
        },
    };
    (network, g, steps, deep)
}
fn node(network: &Network) -> Arc<Mutex<Node>> {
    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets).unwrap();
    let mut n = Node::from_chain(chain, SigningKey::from_slice(&[1; 32]).unwrap()).unwrap();
    // These functional transport tests need deterministic delivery. Credit
    // probability and withholding are covered with controlled samples separately.
    let config = vnx::config::Config::new();
    config.set("Router.max_tx_upload", json!(0)).unwrap();
    n.configure_router(&config).unwrap();
    Arc::new(Mutex::new(n))
}

#[test]
fn unsolicited_orphans_do_not_write_disk_and_valid_live_blocks_still_apply() {
    use mmx_farming::farmer::Farmer;
    use mmx_wallet::Address;
    let (network, g, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let n = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks: vec![wire(&g["block"])],
            wrong: false,
        },
        &n,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    fn files(path: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut out = vec![];
        for e in std::fs::read_dir(path).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(files(&p));
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
        out.sort();
        out
    }
    let before = files(dir.path());
    let original = wire(&steps[0]["block"]);
    let reward = original
        .field("reward_addr")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let farmer = Farmer::new([farmer_scalar()], Some(Address(reward)));
    let mut bad = original.clone();
    bad.fields.insert("prev".into(), V::Bytes(vec![222; 32]));
    let mut proof = bad.field("proof").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .clone();
    proof.fields.insert("proof_xs".into(), V::Sequence(vec![]));
    bad.fields
        .insert("proof".into(), V::Sequence(vec![V::Record(proof)]));
    let mut ids = vec![];
    for _ in 0..4 {
        let block = farmer.sign_block(&bad, &network.params).unwrap();
        ids.push(hash(&block, "hash").unwrap());
        n.lock().unwrap().receive(&block).unwrap();
    }
    assert_eq!(n.lock().unwrap().pending_blocks(), 4);
    assert_eq!(
        files(dir.path()),
        before,
        "unknown-parent blocks stay in RAM"
    );
    for id in ids {
        assert!(n.lock().unwrap().chain().block(&id).unwrap().is_none());
    }
    // A known parent and real VDF still cannot authorize an empty space proof.
    bad.fields
        .insert("prev".into(), original.field("prev").unwrap().clone());
    let bad = farmer.sign_block(&bad, &network.params).unwrap();
    n.lock().unwrap().receive(&bad).unwrap();
    n.lock().unwrap().receive(&wire(&steps[0]["vdf"])).unwrap();
    assert!(n
        .lock()
        .unwrap()
        .chain()
        .block(&hash(&bad, "hash").unwrap())
        .unwrap()
        .is_none());
    assert_eq!(
        files(dir.path()),
        before,
        "invalid space work must never be archived"
    );
    for i in 0..8 {
        let mut waiting = original.clone();
        waiting
            .fields
            .insert("vdf_output".into(), V::Bytes(vec![100 + i; 32]));
        let waiting = farmer.sign_block(&waiting, &network.params).unwrap();
        n.lock().unwrap().receive(&waiting).unwrap();
    }
    // More blocks waiting for VDF work than the per-tick ledger budget cannot
    // stop a later block with available work from being admitted immediately.
    n.lock().unwrap().receive(&original).unwrap();
    assert_eq!(
        n.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        1
    );
}
#[test]
fn farmer_response_replays_and_unsigned_future_envelopes_cannot_grow_the_queue() {
    let (network, _, steps, _) = fixture();
    let n = node(&network);
    let response = native_farming_response();
    for _ in 0..32 {
        n.lock().unwrap().receive(&response).unwrap();
    }
    assert_eq!(n.lock().unwrap().pending_proofs(), 1);
    let mut bad = response.clone();
    bad.fields
        .insert("farmer_sig".into(), V::Bytes(vec![0; 64]));
    bad.fields.insert(
        "content_hash".into(),
        V::Bytes(
            mmx_consensus::response::content_hash(&bad)
                .unwrap()
                .to_vec(),
        ),
    );
    for _ in 0..32 {
        assert!(n.lock().unwrap().receive(&bad).is_err());
    }
    assert_eq!(n.lock().unwrap().pending_proofs(), 1);
    n.lock().unwrap().receive(&wire(&steps[0]["vdf"])).unwrap();
    assert_eq!(n.lock().unwrap().pending_proofs(), 0);
}
#[test]
fn authenticated_future_responses_have_a_height_window_and_decoded_byte_limit() {
    use mmx_farming::farmer::Farmer;
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let farmer = Farmer::new([farmer_scalar()], None);
    let template = native_farming_response();
    let sign = |mut response: Record| {
        response.fields.insert(
            "hash".into(),
            V::Bytes(response.calc_hash(false, 0).unwrap().to_vec()),
        );
        response.fields.insert(
            "content_hash".into(),
            V::Bytes(
                mmx_consensus::response::content_hash(&response)
                    .unwrap()
                    .to_vec(),
            ),
        );
        farmer.sign_response(&response).unwrap()
    };
    let mut distant = template.clone();
    distant
        .fields
        .insert("vdf_height".into(), V::Uint(u32::MAX.into()));
    let distant = sign(distant);
    mmx_consensus::response::authenticate(&distant).unwrap();
    assert!(n
        .lock()
        .unwrap()
        .receive(&distant)
        .unwrap_err()
        .to_string()
        .contains("too far ahead"));
    assert_eq!(n.lock().unwrap().pending_proofs(), 0);
    let mut size = 0;
    let mut rejected = false;
    let mut first = None;
    for i in 0u32..4097 {
        let mut response = template.clone();
        let mut proof = response.field("proof").unwrap().record().unwrap().clone();
        let mut seed = vec![0; 32];
        seed[..4].copy_from_slice(&i.to_le_bytes());
        proof.fields.insert("seed".into(), V::Bytes(seed));
        proof.fields.insert(
            "plot_id".into(),
            V::Bytes(mmx_consensus::proof::calc_plot_id(&proof).unwrap().to_vec()),
        );
        response.fields.insert("proof".into(), V::Record(proof));
        response
            .fields
            .insert("harvester".into(), V::String(vec![b'x'; 1023]));
        let response = sign(response);
        let charge = response
            .memory_size()
            .max(response.encode_compact().unwrap().len());
        assert!(charge < 64 * 1024);
        let before = n.lock().unwrap().pending_proofs();
        match n.lock().unwrap().receive(&response) {
            Ok(()) => size += charge,
            Err(e) => {
                assert!(e.to_string().contains("queue full"));
                assert_eq!(size / (1024 * 1024), 7);
                assert!(size + charge > 8 * 1024 * 1024);
                rejected = true;
            }
        }
        if rejected {
            assert_eq!(n.lock().unwrap().pending_proofs(), before);
            // Duplicates are harmless even when the byte quota is exhausted.
            n.lock().unwrap().receive(first.as_ref().unwrap()).unwrap();
            assert_eq!(n.lock().unwrap().pending_proofs(), before);
            break;
        }
        if first.is_none() {
            first = Some(response.clone());
        }
        n.lock().unwrap().receive(&response).unwrap();
        assert_eq!(n.lock().unwrap().pending_proofs(), before + 1);
    }
    assert!(
        rejected,
        "the decoded byte quota must bind before the entry cap"
    );
}
#[test]
fn peer_socket_has_only_native_router_methods_and_never_executes_validation() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let mut server = peer::Server::bind("127.0.0.1:0", n).unwrap();
    let mut connection = peer::Connection::connect(&server.address().to_string()).unwrap();
    for method in [
        record("mmx.Node.validate", [("tx".into(), V::Optional(None))]),
        record(
            "mmx.Node.get_all_balances",
            [
                ("addresses".into(), V::Sequence(vec![])),
                ("whitelist".into(), V::Sequence(vec![])),
                ("limit".into(), V::Int(100)),
            ],
        ),
        record("mmx.Node.get_params", []),
    ] {
        let name = method.name.clone();
        connection
            .send(&record(
                "mmx.Request",
                [
                    ("id".into(), V::Uint(1)),
                    ("method".into(), V::Record(method)),
                ],
            ))
            .unwrap();
        let reply = connection.receive(Duration::from_secs(2)).unwrap().unwrap();
        let result = reply.field("result").unwrap().record().unwrap();
        assert_eq!(result.name, "vnx.NoSuchMethod");
        assert_eq!(result.field("dst_mac").unwrap().uint().unwrap(), 0);
        assert_eq!(
            result.field("method").unwrap().text().unwrap(),
            name.as_bytes()
        );
    }
    connection
        .send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(2)),
                (
                    "method".into(),
                    V::Record(record("mmx.Node.get_height", [])),
                ),
            ],
        ))
        .unwrap();
    assert_eq!(
        connection
            .receive(Duration::from_secs(2))
            .unwrap()
            .unwrap()
            .field("result")
            .unwrap()
            .record()
            .unwrap()
            .name,
        "mmx.Node.get_height.return"
    );
    drop(connection);
    server.shutdown();
}
#[test]
fn http_deadlines_isolate_slow_clients_and_shutdown_cancels_reads() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let mut server =
        rpc::HttpServer::bind_with_timeout("127.0.0.1:0", n.clone(), Duration::from_millis(300))
            .unwrap();
    let mut slow = TcpStream::connect(server.address()).unwrap();
    slow.write_all(b"POST /rpc HTTP/1.1\r\nHost: slow\r\nContent-Length: 100\r\n\r\n{")
        .unwrap();
    let mut slow_header = TcpStream::connect(server.address()).unwrap();
    slow_header
        .write_all(b"POST /rpc HTTP/1.1\r\nHost: ")
        .unwrap();
    fn post(address: &str, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }
    let body = br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#;
    let request = format!(
        "POST /rpc HTTP/1.1\r\nHost: test\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        std::str::from_utf8(body).unwrap()
    );
    assert!(post(server.address(), request.as_bytes()).starts_with("HTTP/1.1 200"));
    for mut stalled in [slow, slow_header] {
        stalled
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert_eq!(
            stalled.read(&mut [0]).unwrap(),
            0,
            "absolute deadline closes incomplete headers and bodies"
        );
    }
    let chunked=format!("POST /rpc HTTP/1.1\r\nHost: test\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",body.len(),std::str::from_utf8(body).unwrap());
    assert!(post(server.address(), chunked.as_bytes()).starts_with("HTTP/1.1 200"));
    assert!(post(
        server.address(),
        b"POST /rpc HTTP/1.1\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n"
    )
    .starts_with("HTTP/1.1 400"));
    assert!(post(
        server.address(),
        b"POST /rpc HTTP/1.1\r\nContent-Length: 1048577\r\n\r\n"
    )
    .starts_with("HTTP/1.1 413"));
    server.shutdown();
    let mut server =
        rpc::HttpServer::bind_with_timeout("127.0.0.1:0", n, Duration::from_secs(30)).unwrap();
    let mut slow = TcpStream::connect(server.address()).unwrap();
    slow.write_all(b"POST /rpc HTTP/1.1\r\nHost: ").unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let started = std::time::Instant::now();
    server.shutdown();
    assert!(started.elapsed() < Duration::from_secs(1));
}
#[test]
fn transaction_validation_pool_rpc_and_confirmation_use_native_snapshots() {
    let (network, g, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let n = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    let mut source = MemorySource {
        blocks: vec![wire(&g["block"])],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    let block = wire(&steps[0]["block"]);
    let mut tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .clone();
    let receipt = tx
        .field("exec_result")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .clone();
    tx.fields.insert("exec_result".into(), V::Optional(None));
    tx.fields.insert(
        "static_cost".into(),
        V::Uint(tx.calc_cost(&(&network.params).into()).unwrap()),
    );
    tx.fields.insert(
        "content_hash".into(),
        V::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    let id = hash(&tx, "id").unwrap();
    let sender = tx
        .field("sender")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .clone();
    let files = || {
        fn visit(path: &std::path::Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(path).unwrap() {
                let path = e.unwrap().path();
                if path.is_dir() {
                    visit(&path, out);
                } else {
                    out.push((path.clone(), std::fs::read(path).unwrap()));
                }
            }
        }
        let mut out = vec![];
        visit(dir.path(), &mut out);
        out.sort();
        out
    };
    let before = files();
    {
        let mut node = n.lock().unwrap();
        assert!(node.is_synced());
        let result = node.validate_transaction(&tx).unwrap();
        assert_eq!(V::Record(result.to_record()), receipt);
        assert!(node.add_transaction(&tx, true).unwrap());
        assert!(!node.add_transaction(&tx, true).unwrap());
        assert_eq!(node.pending_transactions(), 1);
        for pending in [false, true] {
            let response = node
                .call(&record(
                    "mmx.Node.get_transaction",
                    [
                        ("id".into(), V::Bytes(id.to_vec())),
                        ("pending".into(), V::Bool(pending)),
                    ],
                ))
                .unwrap();
            assert_eq!(
                response
                    .field("_ret_0")
                    .unwrap()
                    .optional()
                    .unwrap()
                    .is_some(),
                pending
            );
        }
        let response = node
            .call(&record(
                "mmx.Node.get_total_balance",
                [
                    (
                        "addresses".into(),
                        V::Sequence(vec![sender.clone(), sender]),
                    ),
                    ("currency".into(), V::Bytes(vec![0; 32])),
                ],
            ))
            .unwrap();
        assert_eq!(
            u128::from_le_bytes(
                response
                    .field("_ret_0")
                    .unwrap()
                    .bytes()
                    .unwrap()
                    .try_into()
                    .unwrap()
            ),
            10_000_000
        );
        let mut bad = tx.clone();
        bad.fields.insert("id".into(), V::Bytes(vec![42; 32]));
        assert!(node.add_transaction(&bad, true).is_err());
        assert_eq!(node.pending_transactions(), 1);
    }
    assert_eq!(
        files(),
        before,
        "mempool and dry-run validation must not write database files"
    );
    // After the historical/live transition, new blocks arrive as broadcasts.
    n.lock().unwrap().receive(&wire(&steps[0]["vdf"])).unwrap();
    n.lock().unwrap().receive(&block).unwrap();
    assert_eq!(
        n.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        1
    );
    assert_eq!(n.lock().unwrap().pending_transactions(), 0);
}
fn check(node: &Arc<Mutex<Node>>, step: &Value) {
    let n = node.lock().unwrap();
    let s = n.chain().selected();
    assert_eq!(
        s.tip().unwrap().unwrap().hash.0,
        hash(&wire(&step["block"]), "hash").unwrap()
    );
    for b in step["state"]["balances"].as_array().unwrap() {
        let a: Hash = raw(&b["address"]).try_into().unwrap();
        let c: Hash = raw(&b["currency"]).try_into().unwrap();
        assert_eq!(
            s.storage().get_balance(&a, &c).unwrap(),
            Some(b["amount"].as_str().unwrap().parse().unwrap())
        );
    }
}
struct MemorySource {
    blocks: Vec<Record>,
    wrong: bool,
}
impl Source for MemorySource {
    fn height(&mut self) -> Result<u32> {
        Ok(self.blocks.len() as u32 - 1)
    }
    fn hash_at(&mut self, h: u32) -> Result<Option<Hash>> {
        self.blocks
            .get(h as usize)
            .map(|b| hash(b, "hash"))
            .transpose()
    }
    fn block_at(&mut self, h: u32) -> Result<Option<Record>> {
        let mut b = self.blocks.get(h as usize).cloned();
        if self.wrong {
            if let Some(b) = &mut b {
                b.fields.insert("prev".into(), V::Bytes(vec![42; 32]));
            }
        }
        Ok(b)
    }
}
#[test]
fn optional_purge_hides_expired_rpc_history_and_preserves_state_boundary_replay_and_restart() {
    let (network, g, steps, deep) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let n = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    let mut source = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&steps[0]["block"]),
            wire(&steps[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    let genesis = hash(&source.blocks[0], "hash").unwrap();
    let expired = hash(
        source.blocks[0].field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap(),
        "id",
    )
    .unwrap();
    let retained = hash(
        source.blocks[1].field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap(),
        "id",
    )
    .unwrap();
    let json_hash = |h: &Hash| json!(h);
    let tables = [
        "balances",
        "contracts",
        "storage",
        "storage_entries",
        "total_supply",
        "transactions",
    ];
    let database_files = || {
        ["selected", "archive"].map(|database| {
            let mut files: Vec<_> =
                std::fs::read_dir(directory.path().join("chain").join(database))
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect();
            files.sort();
            files
        })
    };
    let before: Vec<_> = {
        let node = n.lock().unwrap();
        tables
            .iter()
            .map(|t| {
                node.chain()
                    .selected()
                    .storage()
                    .database()
                    .scan(t, &[], None)
                    .unwrap()
            })
            .collect()
    };
    // Existing workers must observe the newly published boundary, including
    // the peer handler's lock-free genesis-height shortcut.
    let mut peer_server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
    let observer = node(&network);
    let mut peer_client =
        peer::Client::connect(&peer_server.address().to_string(), observer).unwrap();
    let mut vnx = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), n.clone()).unwrap();
    let client = vnx::net::NetworkClient::connect(
        vnx.endpoint(),
        "Node",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap();
    {
        let mut node = n.lock().unwrap();
        assert_eq!(node.earliest_height(), 0);
        node.purge_before(1).unwrap();
        let files = database_files();
        node.purge_before(1).unwrap();
        assert_eq!(
            files,
            database_files(),
            "a completed purge must not rewrite values"
        );
        assert_eq!(node.earliest_height(), 1);
        assert!(node.rollback_to(0).is_err());
        assert!(node.purge_before(0).is_err());
        assert!(node.purge_before(3).is_err());
        node.tick().unwrap();
        for (table, expected) in tables.iter().zip(&before) {
            assert_eq!(
                &node
                    .chain()
                    .selected()
                    .storage()
                    .database()
                    .scan(table, &[], None)
                    .unwrap(),
                expected
            );
        }
        // Duplicate protection and deployed contracts remain ledger state;
        // the expired full transaction payload is physically excluded.
        assert!(node
            .chain()
            .selected()
            .storage()
            .get_transaction_index(&expired)
            .unwrap()
            .is_some());
        assert!(node
            .chain()
            .selected()
            .transaction(&expired)
            .unwrap()
            .is_none());
    }
    let request = |method: &str, params: Value| {
        let result = rpc::json_request(
            &n,
            &serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
                .unwrap(),
        )
        .unwrap();
        assert!(result.get("error").is_none(), "{method}: {result}");
        result["result"].clone()
    };
    for (method, params) in [
        ("get_block_at", json!({"height":0})),
        ("get_header_at", json!({"height":0})),
        ("get_block_hash", json!({"height":0})),
        ("get_block_hash_ex", json!({"height":0})),
        ("get_block", json!({"hash":json_hash(&genesis)})),
        ("get_header", json!({"hash":json_hash(&genesis)})),
        (
            "get_transaction",
            json!({"id":json_hash(&expired),"pending":false}),
        ),
        (
            "get_transaction",
            json!({"id":json_hash(&expired),"pending":true}),
        ),
        ("get_tx_height", json!({"id":json_hash(&expired)})),
    ] {
        assert!(request(method, params.clone()).is_null(), "{method}");
        assert!(
            client
                .call_value(&format!("mmx.Node.{method}"), params)
                .unwrap()["_ret_0"]
                .is_null(),
            "{method}"
        );
    }
    assert_eq!(request("get_tx_ids_at", json!({"height":0})), json!([]));
    let batch = request(
        "get_transactions",
        json!({"ids":[json_hash(&expired),json_hash(&retained),json_hash(&expired)]}),
    );
    assert!(batch[0].is_null() && !batch[1].is_null() && batch[2].is_null());
    assert_eq!(
        request("get_tx_height", json!({"id":json_hash(&retained)})),
        1
    );
    assert!(!request("get_header_at", json!({"height":1})).is_null());
    assert_eq!(request("get_status", json!({}))["earliest_height"], 1);
    assert_eq!(request("get_genesis_hash", json!({})), json_hash(&genesis));
    assert!(peer_client.hash_at(0).unwrap().is_none());
    assert!(peer_client.block_at(0).unwrap().is_none());
    assert_eq!(
        peer_client.hash_at(1).unwrap(),
        Some(hash(&source.blocks[1], "hash").unwrap())
    );
    drop(client);
    vnx.shutdown();
    drop(peer_client);
    peer_server.shutdown();
    // A fork below the floor is a local retention limit, not permission to
    // truncate state or reinterpret it as a consensus-invalid block.
    let mut older_fork = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&deep[0]["block"]),
            wire(&deep[1]["block"]),
        ],
        wrong: false,
    };
    assert!(sync::synchronize(&mut older_fork, &n, &AtomicBool::new(false), |_| {}).is_err());
    check(&n, &steps[1]);
    drop(n);
    let n = Arc::new(Mutex::new(Node::open(directory.path(), &network).unwrap()));
    {
        let mut node = n.lock().unwrap();
        assert_eq!(node.earliest_height(), 1);
        let files = database_files();
        node.purge_before(1).unwrap();
        assert_eq!(
            files,
            database_files(),
            "reopen must preserve the completion marker"
        );
        assert!(node.rollback_to(0).is_err());
        node.rollback_to(1).unwrap();
        node.tick().unwrap();
    }
    check(&n, &steps[0]);
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    check(&n, &steps[1]);
    n.lock().unwrap().purge_before(2).unwrap();
    drop(n);
    let mut node = Node::open(directory.path(), &network).unwrap();
    assert_eq!(node.earliest_height(), 2);
    assert!(node.rollback_to(1).is_err());
    node.rollback_to(2).unwrap();
    node.tick().unwrap();
    assert!(node
        .chain()
        .selected()
        .transaction(&retained)
        .unwrap()
        .is_none());
}

#[test]
fn socket_sync_deep_fork_rollback_restart_and_rpc_read_the_same_native_ledger() {
    let (network, g, steps, deep) = fixture();
    let serving = node(&network);
    let mut initial = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&steps[0]["block"]),
            wire(&steps[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut initial, &serving, &AtomicBool::new(false), |_| {}).unwrap();
    let mut server = peer::Server::bind("127.0.0.1:0", serving.clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let receiving = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    let mut client =
        peer::Client::connect(&server.address().to_string(), receiving.clone()).unwrap();
    let progress =
        sync::synchronize_pipelined(&mut client, &receiving, &AtomicBool::new(false), |_| {})
            .unwrap();
    assert_eq!(
        (progress.common, progress.downloaded, progress.selected),
        (0, 2, 2)
    );
    assert!(receiving.lock().unwrap().is_synced());
    check(&receiving, &steps[1]);
    // The response returns the exact lossless record, independently of RPC JSON.
    assert_eq!(client.block_at(2).unwrap(), Some(wire(&steps[1]["block"])));
    let id = client.call(record("mmx.Router.get_id", [])).unwrap();
    let info = client.call(record("mmx.Router.get_info", [])).unwrap();
    assert_eq!(info.record().unwrap().field("id").unwrap(), &id);
    let signed = client
        .call(record(
            "mmx.Router.sign_msg",
            [("msg".into(), V::Bytes(vec![3; 32]))],
        ))
        .unwrap();
    let pair = signed.seq().unwrap();
    assert!(mmx_crypto::verify(
        &[3; 32],
        pair[0].bytes().unwrap().try_into().unwrap(),
        pair[1].bytes().unwrap().try_into().unwrap()
    )
    .unwrap());
    let mut vnx = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), receiving.clone()).unwrap();
    let rpc_client = vnx::net::NetworkClient::connect(
        vnx.endpoint(),
        "Node",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(
        rpc_client
            .call_value("mmx.Node.get_height", json!({}))
            .unwrap()["_ret_0"],
        2
    );
    assert_eq!(
        rpc_client
            .call_value("mmx.Node.get_synced_height", json!({}))
            .unwrap()["_ret_0"],
        2
    );
    let mut http = rpc::HttpServer::bind("127.0.0.1:0", receiving.clone()).unwrap();
    let request = json!({"jsonrpc":"2.0","id":7,"method":"get_status"}).to_string();
    let mut tcp = TcpStream::connect(http.address()).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    write!(tcp,"POST /rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",request.len(),request).unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("\"height\":2"));
    // Native alternate-root fixture replaces both selected blocks, with all old
    // branches retained and no explicit pre-validation rollback.
    serving.lock().unwrap().begin_resync();
    let mut alternate = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&deep[0]["block"]),
            wire(&deep[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut alternate, &serving, &AtomicBool::new(false), |_| {}).unwrap();
    check(&serving, &deep[1]);
    receiving.lock().unwrap().begin_resync();
    let progress =
        sync::synchronize_pipelined(&mut client, &receiving, &AtomicBool::new(false), |_| {})
            .unwrap();
    assert_eq!((progress.common, progress.downloaded), (0, 2));
    check(&receiving, &deep[1]);
    http.shutdown();
    vnx.shutdown();
    drop(rpc_client);
    drop(client);
    server.shutdown();
    drop(serving);
    receiving.lock().unwrap().rollback_to(0).unwrap();
    check(&receiving, &g);
    drop(receiving);
    let receiving = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    check(&receiving, &g);
    assert!(receiving
        .lock()
        .unwrap()
        .chain()
        .block(&hash(&wire(&steps[1]["block"]), "hash").unwrap())
        .unwrap()
        .is_some());
    let mut source = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&deep[0]["block"]),
            wire(&deep[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &receiving, &AtomicBool::new(false), |_| {}).unwrap();
    check(&receiving, &deep[1]);
}
#[test]
fn sync_rejects_wrong_genesis_discontinuous_blocks_and_cancelled_download_without_rewind() {
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    // Native historical download ignores unrelated live block broadcasts.
    n.lock()
        .unwrap()
        .receive(&wire(&steps[0]["block"]))
        .unwrap();
    n.lock().unwrap().tick().unwrap();
    check(&n, &g);
    assert!(n
        .lock()
        .unwrap()
        .chain()
        .block(&hash(&wire(&steps[0]["block"]), "hash").unwrap())
        .unwrap()
        .is_none());
    let mut source = MemorySource {
        blocks: vec![wire(&g["block"]), wire(&steps[0]["block"])],
        wrong: true,
    };
    assert!(sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).is_err());
    check(&n, &g);
    source.wrong = false;
    assert!(sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(true), |_| {}).is_err());
    check(&n, &g);
    source.blocks[1]
        .fields
        .insert("time_stamp".into(), V::Int(i64::MAX));
    let failure =
        sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(failure.to_string().contains("too far in the future"));
    check(&n, &g);
    source.blocks[1] = wire(&steps[0]["block"]);
    source.blocks[0]
        .fields
        .insert("hash".into(), V::Bytes(vec![1; 32]));
    assert!(sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).is_err());
    check(&n, &g);
}
#[test]
fn frame_endianness_truncation_bad_sizes_and_rpc_errors() {
    let (network, g, _, _) = fixture();
    let n = node(&network);
    let value = wire(&g["block"]);
    let frame = peer::encode_frame(&value).unwrap();
    assert_eq!(peer::decode_frame(&frame).unwrap(), value);
    let mut alternate = frame.clone();
    alternate[..4].reverse();
    alternate[4..8].reverse();
    assert_eq!(peer::decode_frame(&alternate).unwrap(), value);
    for cut in [0, 1, 7, 8, frame.len() - 1] {
        assert!(peer::decode_frame(&frame[..cut]).is_err());
    }
    let mut bad = frame;
    bad[4] ^= 1;
    assert!(peer::decode_frame(&bad).is_err());
    assert_eq!(
        rpc::json_request(&n, b"{").unwrap()["error"]["code"],
        -32700
    );
    let batch = json!([
 {"jsonrpc":"2.0","id":1,"method":"get_height"},
 {"jsonrpc":"2.0","id":2,"method":"get_balance","params":{}},
 {"jsonrpc":"2.0","id":3,"method":"rollback_to","params":{"height":0}},
 {"jsonrpc":"2.0","method":"get_height"}]);
    let reply = rpc::json_request(&n, batch.to_string().as_bytes()).unwrap();
    assert_eq!(reply.as_array().unwrap().len(), 3);
    assert_eq!(reply[0]["result"], 0);
    assert!(reply[1].get("error").is_some());
    assert_eq!(reply[2]["error"]["code"], -32601);
}

#[test]
fn continuing_native_stream_reuses_type_codes_and_preserves_tuple_bytes() {
    let value = record(
        "mmx.Router.sign_msg.return",
        [(
            "_ret_0".into(),
            V::Sequence(vec![V::Bytes(vec![2; 33]), V::Bytes(vec![3; 64])]),
        )],
    );
    let full = value.encode().unwrap();
    let mut registry = vnx::Registry::new();
    assert_eq!(
        Record::decode_with_registry(&full, &mut registry).unwrap(),
        value
    );
    let mut writer = vnx::binary::Writer::new(Vec::new(), mmx_types::vnx_registry().unwrap())
        .with_type_codes(false);
    writer
        .write(&json!({"__type":"mmx.Router.sign_msg.return","_ret_0":[vec![2;33],vec![3;64]]}))
        .unwrap();
    let cached = writer.into_inner();
    assert!(Record::decode(&cached).is_err());
    assert_eq!(
        Record::decode_with_registry(&cached, &mut registry).unwrap(),
        value
    );
}

#[test]
fn farmer_response_waits_for_verified_future_vdf_then_live_block_uses_the_same_evidence() {
    live_vdf_evidence(false, false);
}
#[test]
fn background_vdf_releases_farmer_response_and_authenticates_the_live_block() {
    live_vdf_evidence(true, false);
}
#[test]
fn opencl_vdf_releases_farmer_response_and_authenticates_the_live_block() {
    if std::env::var("MMX_OPENCL_TESTS").as_deref() == Ok("1") {
        live_vdf_evidence(true, true);
    }
}
fn live_vdf_evidence(background: bool, gpu: bool) {
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    if background {
        let accelerator = gpu.then(|| {
            std::sync::Arc::new(
                mmx_consensus::accelerator::Accelerator::new(
                    0,
                    None,
                    std::env::var("MMX_OPENCL_ALL_DEVICES").as_deref() != Ok("1"),
                )
                .unwrap(),
            )
        });
        n.lock()
            .unwrap()
            .enable_background_vdf_with(accelerator)
            .unwrap();
    }
    let row: Value = include_str!("../../mmx-consensus/tests/fixtures/responses.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|r| r["name"] == "extensions1_0")
        .unwrap();
    let response = wire(&row["wire"]);
    n.lock().unwrap().receive(&response).unwrap();
    assert_eq!(n.lock().unwrap().pending_proofs(), 1);
    let mut bad = response.clone();
    bad.fields
        .insert("content_hash".into(), V::Bytes(vec![0; 32]));
    assert!(n.lock().unwrap().receive(&bad).is_err());
    assert_eq!(n.lock().unwrap().pending_proofs(), 1);
    // An unsigned metadata point never establishes work or releases the queue.
    let metadata = Record::from_generated(&mmx::VDF_Point::default()).unwrap();
    n.lock().unwrap().receive(&metadata).unwrap();
    assert_eq!(n.lock().unwrap().pending_proofs(), 1);
    n.lock().unwrap().receive(&wire(&steps[0]["vdf"])).unwrap();
    if background {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while n.lock().unwrap().pending_proofs() != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "VDF worker did not complete"
            );
            n.lock().unwrap().tick().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(n.lock().unwrap().pending_proofs(), 0);
    check(&n, &g);
    let height = n
        .lock()
        .unwrap()
        .call(&record("mmx.Node.get_vdf_height", []))
        .unwrap();
    assert_eq!(height.field("_ret_0").unwrap(), &V::Uint(1));
    let peak = n
        .lock()
        .unwrap()
        .call(&record("mmx.Node.get_vdf_peak", []))
        .unwrap();
    assert_eq!(
        peak.field("_ret_0").unwrap(),
        wire(&steps[0]["block"]).field("vdf_output").unwrap()
    );
    // Complete an empty historical round to transition to live mode. The new
    // block still needs and uses the independently verified PoT, not metadata.
    let mut source = MemorySource {
        blocks: vec![wire(&g["block"])],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    assert!(n.lock().unwrap().is_synced());
    n.lock()
        .unwrap()
        .receive(&wire(&steps[0]["block"]))
        .unwrap();
    check(&n, &steps[0]);
}

#[test]
fn peer_broadcast_io_continues_while_the_ledger_is_locked_then_validates_on_tick() {
    use std::{net::TcpListener, sync::mpsc, thread};
    let (network, _, steps, _) = fixture();
    let n = node(&network);
    let proof = wire(&steps[0]["vdf"]);
    let mut invalid = proof.clone();
    invalid
        .fields
        .insert("content_hash".into(), V::Bytes(vec![0; 32]));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let sending = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut connection = peer::Connection::new(socket).unwrap();
        connection.send(&invalid).unwrap();
        drop(connection);
        let (socket, _) = listener.accept().unwrap();
        let mut connection = peer::Connection::new(socket).unwrap();
        connection.send(&proof).unwrap();
    });
    let mut bad_client = peer::Client::connect(&address.to_string(), n.clone()).unwrap();
    let mut client = peer::Client::connect(&address.to_string(), n.clone()).unwrap();
    let locked = n.lock().unwrap();
    let (done, complete) = mpsc::channel();
    let receiving = thread::spawn(move || {
        // Native rejects the malformed envelope at the peer boundary, without
        // waiting for the ledger or affecting another peer's valid work.
        assert!(bad_client
            .poll()
            .unwrap_err()
            .to_string()
            .contains("invalid VDF"));
        drop(bad_client);
        client.poll().unwrap();
        done.send(()).unwrap();
    });
    // A synchronous callback would block here waiting for the ledger mutex.
    complete.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        locked
            .call(&record("mmx.Node.get_vdf_height", []))
            .unwrap()
            .field("_ret_0")
            .unwrap(),
        &V::Uint(0)
    );
    drop(locked);
    receiving.join().unwrap();
    sending.join().unwrap();
    n.lock().unwrap().tick().unwrap();
    assert_eq!(
        n.lock()
            .unwrap()
            .call(&record("mmx.Node.get_vdf_height", []))
            .unwrap()
            .field("_ret_0")
            .unwrap(),
        &V::Uint(1)
    );
    assert_eq!(
        n.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
}

#[test]
fn downloading_a_competing_branch_keeps_the_native_root_until_an_explicit_rewind() {
    let (network, g, steps, deep) = fixture();
    let mut chain = ForkChain::memory(network.params.clone());
    chain.initialize(&network.assets).unwrap();
    for step in &steps[..2] {
        chain.admit(&wire(&step["block"]), &[]).unwrap();
    }
    chain
        .update_sync_at(hash(&wire(&g["block"]), "hash").unwrap(), None, 0)
        .unwrap();
    let n = Arc::new(Mutex::new(
        Node::from_chain(chain, SigningKey::from_slice(&[2; 32]).unwrap()).unwrap(),
    ));
    assert_eq!(n.lock().unwrap().root_height(), 2); // native restart: commit_delay=0
    let mut source = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&deep[0]["block"]),
            wire(&deep[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(n.lock().unwrap().root_height(), 2);
    assert!(!n.lock().unwrap().is_synced());
    check(&n, &steps[1]);
    // Native fork choice requires a candidate above the existing root. Download
    // must not silently rebase policy to genesis just to select a peer's branch.
    n.lock().unwrap().rollback_to(0).unwrap();
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    check(&n, &deep[1]);
    assert!(n.lock().unwrap().is_synced());
}
#[test]
fn a_moving_peer_tip_does_not_prematurely_enable_live_admission() {
    struct Advancing {
        source: MemorySource,
        queries: u32,
    }
    impl Source for Advancing {
        fn height(&mut self) -> Result<u32> {
            self.queries += 1;
            Ok(if self.queries == 1 { 1 } else { 2 })
        }
        fn hash_at(&mut self, h: u32) -> Result<Option<Hash>> {
            self.source.hash_at(h)
        }
        fn block_at(&mut self, h: u32) -> Result<Option<Record>> {
            self.source.block_at(h)
        }
    }
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    let mut source = Advancing {
        source: MemorySource {
            blocks: vec![
                wire(&g["block"]),
                wire(&steps[0]["block"]),
                wire(&steps[1]["block"]),
            ],
            wrong: false,
        },
        queries: 0,
    };
    let first =
        sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(first.selected, 1);
    assert!(!n.lock().unwrap().is_synced());
    let second =
        sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(second.downloaded, 1);
    assert!(n.lock().unwrap().is_synced());
    check(&n, &steps[1]);
}

#[test]
fn native_peer_pipeline_correlates_reordered_returns_and_services_peer_requests() {
    let (network, _, steps, _) = fixture();
    let n = node(&network);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected: Vec<_> = steps[..2].iter().map(|s| Some(wire(&s["block"]))).collect();
    let replies = expected.clone();
    let thread = std::thread::spawn(move || {
        let mut c = peer::Connection::new(listener.accept().unwrap().0).unwrap();
        let mut requests = Vec::new();
        for _ in 0..2 {
            requests.push(c.receive(Duration::from_secs(2)).unwrap().unwrap());
        }
        c.send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(99)),
                ("method".into(), V::Record(record("mmx.Router.get_id", []))),
            ],
        ))
        .unwrap();
        for r in requests.into_iter().rev() {
            let height =
                mmx_node::node::number(r.field("method").unwrap().record().unwrap(), "height")
                    .unwrap();
            let result = record(
                "mmx.Node.get_block_at.return",
                [(
                    "_ret_0".into(),
                    V::Record(replies[(height - 1) as usize].clone().unwrap()),
                )],
            );
            c.send(&record(
                "mmx.Return",
                [
                    ("id".into(), r.field("id").unwrap().clone()),
                    ("result".into(), V::Record(result)),
                ],
            ))
            .unwrap();
        }
        let response = c.receive(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(mmx_node::node::number(&response, "id").unwrap(), 99);
        assert_eq!(
            response.field("result").unwrap().record().unwrap().name,
            "mmx.Router.get_id.return"
        );
    });
    let mut client = peer::Client::connect(&address.to_string(), n).unwrap();
    assert_eq!(client.blocks_at(1, 2).unwrap(), expected);
    thread.join().unwrap();
}

#[test]
fn coordinated_socket_sync_authenticates_three_ids_and_uses_native_content_hash_pairs() {
    let (network, g, steps, _) = fixture();
    let receiving = node(&network);
    let genesis = hash(&wire(&g["block"]), "hash").unwrap();
    let own_id: Hash = receiving
        .lock()
        .unwrap()
        .call(&record("mmx.Router.get_id", []))
        .unwrap()
        .field("_ret_0")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let mut servers = Vec::new();
    let mut sources = Vec::new();
    let blocks = vec![
        wire(&g["block"]),
        wire(&steps[0]["block"]),
        wire(&steps[1]["block"]),
    ];
    for key in 2..=4 {
        let mut chain = ForkChain::memory(network.params.clone());
        chain.initialize(&network.assets).unwrap();
        let serving = Arc::new(Mutex::new(
            Node::from_chain(chain, SigningKey::from_slice(&[key; 32]).unwrap()).unwrap(),
        ));
        let mut initial = MemorySource {
            blocks: blocks.clone(),
            wrong: false,
        };
        sync::synchronize(&mut initial, &serving, &AtomicBool::new(false), |_| {}).unwrap();
        let server = peer::Server::bind("127.0.0.1:0", serving).unwrap();
        let mut client =
            peer::Client::connect(&server.address().to_string(), receiving.clone()).unwrap();
        let (id, height) = client.qualify(genesis, own_id).unwrap();
        assert_eq!(height, 2);
        assert!(client.known_peers().unwrap().is_empty());
        let hashes = client.hash_pairs(1, 2).unwrap();
        assert_eq!(
            hashes[0],
            Some(peer::BlockHash {
                block: hash(&blocks[1], "hash").unwrap(),
                content: hash(&blocks[1], "content_hash").unwrap()
            })
        );
        sources.push((id, client));
        servers.push(server);
    }
    let mut pool = sync::peers::PeerPool::new(sources);
    assert_eq!(pool.len(), 3);
    let progress =
        sync::peers::synchronize_peers(&mut pool, &receiving, &AtomicBool::new(false), |_| {})
            .unwrap();
    assert_eq!(progress.selected, 2);
    assert!(receiving.lock().unwrap().is_synced());
    check(&receiving, &steps[1]);
    let mut client =
        peer::Client::connect(&servers[0].address().to_string(), receiving.clone()).unwrap();
    let id = pool.ids()[0];
    // Authenticating with the peer's own ID must reject self-connections.
    let actual_id: Hash = client
        .call(record("mmx.Router.get_id", []))
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    assert!(client.qualify(genesis, actual_id).is_err());
    assert!(client.qualify([7; 32], own_id).is_err());
    assert_ne!(id, own_id);
    drop(client);
    drop(pool);
    for server in &mut servers {
        server.shutdown();
    }
}

#[test]
fn offline_rewinds_stay_at_the_requested_height_across_ticks_and_restarts() {
    let (network, g, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let mut n = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    let mut source = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&steps[0]["block"]),
            wire(&steps[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    check(&n, &steps[1]);
    for height in [1, 0] {
        n.lock().unwrap().rollback_to(height).unwrap();
        for _ in 0..2 {
            n.lock().unwrap().tick().unwrap();
        }
        check(&n, if height == 0 { &g } else { &steps[0] });
        drop(n);
        n = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
        n.lock().unwrap().tick().unwrap();
        check(&n, if height == 0 { &g } else { &steps[0] });
        assert!(n
            .lock()
            .unwrap()
            .chain()
            .block(&hash(&wire(&steps[1]["block"]), "hash").unwrap())
            .unwrap()
            .is_some());
    }
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    check(&n, &steps[1]);
}

#[test]
fn native_backpressure_retries_only_rejected_requests_and_keeps_successes_and_the_connection() {
    let (network, _, steps, _) = fixture();
    let n = node(&network);
    let mut expected: Vec<_> = steps[..2].iter().map(|s| Some(wire(&s["block"]))).collect();
    expected.resize(peer::MAX_BODY_REQUESTS, None);
    let replies = expected.clone();
    let ids: Vec<_> = replies
        .iter()
        .enumerate()
        .map(|(i, b)| {
            b.as_ref()
                .map(|b| hash(b, "hash").unwrap())
                .unwrap_or([100 + i as u8; 32])
        })
        .collect();
    let wanted = ids.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut c = peer::Connection::new(listener.accept().unwrap().0).unwrap();
        let requests: Vec<_> = (0..peer::MAX_BODY_REQUESTS)
            .map(|_| c.receive(Duration::from_secs(2)).unwrap().unwrap())
            .collect();
        let second = &requests[1];
        let send = |c: &mut peer::Connection, id: V, result: Record| {
            c.send(&record(
                "mmx.Return",
                [("id".into(), id), ("result".into(), V::Record(result))],
            ))
            .unwrap();
        };
        send(
            &mut c,
            second.field("id").unwrap().clone(),
            Record::from_generated(&mmx_types::vnx::OverflowException::default()).unwrap(),
        );
        c.send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(99)),
                ("method".into(), V::Record(record("mmx.Router.get_id", []))),
            ],
        ))
        .unwrap();
        for (index, request) in requests.iter().enumerate().rev() {
            if index != 1 {
                send(
                    &mut c,
                    request.field("id").unwrap().clone(),
                    record(
                        "mmx.Node.get_block.return",
                        [(
                            "_ret_0".into(),
                            replies[index].clone().map(V::Record).unwrap_or(V::Null),
                        )],
                    ),
                );
            }
        }
        let mut answered = false;
        loop {
            let request = c.receive(Duration::from_secs(2)).unwrap().unwrap();
            if request.name == "mmx.Return" {
                assert_eq!(mmx_node::node::number(&request, "id").unwrap(), 99);
                answered = true;
                continue;
            }
            let method = request.field("method").unwrap().record().unwrap();
            assert_eq!(method.name, "mmx.Node.get_block");
            assert_eq!(
                hash(method, "hash").unwrap(),
                wanted[1],
                "successful block must not be requested twice"
            );
            assert_ne!(request.field("id").unwrap(), second.field("id").unwrap());
            send(
                &mut c,
                request.field("id").unwrap().clone(),
                record(
                    "mmx.Node.get_block.return",
                    [("_ret_0".into(), V::Record(replies[1].clone().unwrap()))],
                ),
            );
            break;
        }
        assert!(answered);
        let height = c.receive(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(
            height.field("method").unwrap().record().unwrap().name,
            "mmx.Node.get_height"
        );
        send(
            &mut c,
            height.field("id").unwrap().clone(),
            record(
                "mmx.Node.get_height.return",
                [("_ret_0".into(), V::Uint(2))],
            ),
        );
    });
    let mut client = peer::Client::connect(&address.to_string(), n).unwrap();
    assert!(client
        .blocks_by_hash(&vec![[0; 32]; peer::MAX_BODY_REQUESTS + 1])
        .is_err());
    assert_eq!(client.blocks_by_hash(&ids).unwrap(), expected);
    assert_eq!(client.backpressure_retries(), 1);
    assert_eq!(client.height().unwrap(), 2);
    server.join().unwrap();
}

#[test]
fn native_metadata_backpressure_preserves_other_replies_and_restores_the_full_next_batch() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut c = peer::Connection::new(listener.accept().unwrap().0).unwrap();
        let reply = |c: &mut peer::Connection, request: &Record, overflow: bool| {
            let result = if overflow {
                Record::from_generated(&mmx_types::vnx::OverflowException::default()).unwrap()
            } else {
                let h = mmx_node::node::number(
                    request.field("method").unwrap().record().unwrap(),
                    "height",
                )
                .unwrap();
                record(
                    "mmx.Node.get_block_hash_ex.return",
                    [(
                        "_ret_0".into(),
                        V::Optional(Some(Box::new(V::Sequence(vec![
                            V::Bytes(vec![h as u8; 32]),
                            V::Bytes(vec![(h ^ 128) as u8; 32]),
                        ])))),
                    )],
                )
            };
            c.send(&record(
                "mmx.Return",
                [
                    ("id".into(), request.field("id").unwrap().clone()),
                    ("result".into(), V::Record(result)),
                ],
            ))
            .unwrap();
        };
        let mut requests = Vec::new();
        for _ in 0..128 {
            requests.push(c.receive(Duration::from_secs(2)).unwrap().unwrap());
        }
        let mut rejected = std::collections::BTreeSet::new();
        for request in requests.iter().rev() {
            let height = mmx_node::node::number(
                request.field("method").unwrap().record().unwrap(),
                "height",
            )
            .unwrap();
            let overflow = height % 16 == 4;
            if overflow {
                rejected.insert(height);
            }
            reply(&mut c, request, overflow);
        }
        for _ in 0..8 {
            let request = c.receive(Duration::from_secs(2)).unwrap().unwrap();
            let height = mmx_node::node::number(
                request.field("method").unwrap().record().unwrap(),
                "height",
            )
            .unwrap();
            assert!(
                rejected.remove(&height),
                "only rejected metadata requests may be resent"
            );
            reply(&mut c, &request, false);
        }
        assert!(rejected.is_empty());
        // A transient blocked socket must not force all subsequent windows into
        // a small, slow, round-trip-limited request batch.
        requests.clear();
        for _ in 0..128 {
            requests.push(c.receive(Duration::from_secs(2)).unwrap().unwrap());
        }
        for request in requests.iter().rev() {
            reply(&mut c, request, false);
        }
    });
    let mut client = peer::Client::connect(&address.to_string(), n).unwrap();
    for start in [100, 200] {
        let values = client.hash_pairs(start, 128).unwrap();
        for (index, value) in values.into_iter().enumerate() {
            let h = start + index as u32;
            assert_eq!(
                value,
                Some(peer::BlockHash {
                    block: [h as u8; 32],
                    content: [(h ^ 128) as u8; 32]
                })
            );
        }
    }
    assert_eq!(client.backpressure_retries(), 8);
    server.join().unwrap();
}

#[test]
fn full_hash_windows_pipeline_before_replies_and_correlate_reordered_returns() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut c = peer::Connection::new(listener.accept().unwrap().0).unwrap();
        let requests: Vec<_> = (0..128)
            .map(|_| c.receive(Duration::from_secs(2)).unwrap().unwrap())
            .collect();
        assert!(c.receive(Duration::from_millis(10)).unwrap().is_none());
        for request in requests.into_iter().rev() {
            let height = mmx_node::node::number(
                request.field("method").unwrap().record().unwrap(),
                "height",
            )
            .unwrap();
            c.send(&record(
                "mmx.Return",
                [
                    ("id".into(), request.field("id").unwrap().clone()),
                    (
                        "result".into(),
                        V::Record(record(
                            "mmx.Node.get_block_hash_ex.return",
                            [(
                                "_ret_0".into(),
                                V::Optional(Some(Box::new(V::Sequence(vec![
                                    V::Bytes(vec![height as u8; 32]),
                                    V::Bytes(vec![(height ^ 128) as u8; 32]),
                                ])))),
                            )],
                        )),
                    ),
                ],
            ))
            .unwrap();
        }
    });
    let mut client = peer::Client::connect(&address.to_string(), n).unwrap();
    let replies = client.hash_pairs(1000, 128).unwrap();
    assert_eq!(replies.len(), 128);
    for (offset, actual) in replies.into_iter().enumerate() {
        let height = 1000 + offset as u32;
        assert_eq!(
            actual,
            Some(peer::BlockHash {
                block: [height as u8; 32],
                content: [(height ^ 128) as u8; 32],
            })
        );
    }
    server.join().unwrap();
}

#[test]
fn peer_identity_and_published_height_replies_do_not_wait_for_the_ledger_and_follow_rewind() {
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
    let mut c = peer::Connection::new(TcpStream::connect(server.address()).unwrap()).unwrap();
    let query = |c: &mut peer::Connection, method: Record| {
        c.send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(77)),
                ("method".into(), V::Record(method)),
            ],
        ))
        .unwrap();
        let response = c.receive(Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(mmx_node::node::number(&response, "id").unwrap(), 77);
        response
            .field("result")
            .unwrap()
            .record()
            .unwrap()
            .field("_ret_0")
            .unwrap()
            .clone()
    };
    {
        let _ledger = n.lock().unwrap();
        assert_eq!(
            query(&mut c, record("mmx.Node.get_height", []))
                .uint()
                .unwrap(),
            0
        );
        assert!(query(&mut c, record("mmx.Node.get_synced_height", []))
            .optional()
            .unwrap()
            .is_none());
        let id = query(&mut c, record("mmx.Router.get_id", []));
        let info = query(&mut c, record("mmx.Router.get_info", []));
        assert_eq!(info.record().unwrap().field("id").unwrap(), &id);
        let signed = query(
            &mut c,
            record(
                "mmx.Router.sign_msg",
                [("msg".into(), V::Bytes(vec![8; 32]))],
            ),
        );
        let pair = signed.seq().unwrap();
        assert!(mmx_crypto::verify(
            &[8; 32],
            pair[0].bytes().unwrap().try_into().unwrap(),
            pair[1].bytes().unwrap().try_into().unwrap()
        )
        .unwrap());
        assert_eq!(
            mmx_crypto::public_key_address(pair[0].bytes().unwrap().try_into().unwrap()).as_slice(),
            id.bytes().unwrap()
        );
        let genesis = query(
            &mut c,
            record("mmx.Node.get_block_hash", [("height".into(), V::Uint(0))]),
        );
        assert_eq!(
            genesis.optional().unwrap().unwrap().bytes().unwrap(),
            hash(&wire(&g["block"]), "hash").unwrap()
        );
        for method in ["mmx.Node.get_block_hash", "mmx.Node.get_block_hash_ex"] {
            assert!(query(
                &mut c,
                record(method, [("height".into(), V::Uint(1_000_000))])
            )
            .optional()
            .unwrap()
            .is_none());
        }
        for method in ["mmx.Node.get_block_at", "mmx.Node.get_header_at"] {
            assert_eq!(
                query(
                    &mut c,
                    record(method, [("height".into(), V::Uint(1_000_000))])
                ),
                V::Null
            );
        }
    }
    let mut source = MemorySource {
        blocks: vec![
            wire(&g["block"]),
            wire(&steps[0]["block"]),
            wire(&steps[1]["block"]),
        ],
        wrong: false,
    };
    sync::synchronize(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    {
        let mut ledger = n.lock().unwrap();
        assert_eq!(
            query(&mut c, record("mmx.Node.get_height", []))
                .uint()
                .unwrap(),
            2
        );
        assert_eq!(
            query(&mut c, record("mmx.Node.get_synced_height", []))
                .optional()
                .unwrap()
                .unwrap()
                .uint()
                .unwrap(),
            2
        );
        ledger.begin_resync();
        assert!(query(&mut c, record("mmx.Node.get_synced_height", []))
            .optional()
            .unwrap()
            .is_none());
        ledger.rollback_to(0).unwrap();
        assert_eq!(
            query(&mut c, record("mmx.Node.get_height", []))
                .uint()
                .unwrap(),
            0
        );
        assert!(query(&mut c, record("mmx.Node.get_synced_height", []))
            .optional()
            .unwrap()
            .is_none());
        assert!(query(
            &mut c,
            record(
                "mmx.Node.get_block_hash_ex",
                [("height".into(), V::Uint(2))]
            )
        )
        .optional()
        .unwrap()
        .is_none());
        assert_eq!(
            query(
                &mut c,
                record("mmx.Node.get_block_at", [("height".into(), V::Uint(2))])
            ),
            V::Null
        );
    }
    drop(c);
    server.shutdown();
}

#[test]
fn outgoing_peer_can_answer_height_and_identity_queries_while_the_ledger_is_locked() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut c = peer::Connection::new(listener.accept().unwrap().0).unwrap();
        for method in [
            "mmx.Node.get_height",
            "mmx.Node.get_synced_height",
            "mmx.Router.get_id",
            "mmx.Router.get_info",
        ] {
            c.send(&record(
                "mmx.Request",
                [
                    ("id".into(), V::Uint(9)),
                    ("method".into(), V::Record(record(method, []))),
                ],
            ))
            .unwrap();
            let response = c.receive(Duration::from_secs(2)).unwrap().unwrap();
            assert_eq!(
                response.field("result").unwrap().record().unwrap().name,
                format!("{method}.return")
            );
        }
    });
    let mut client = peer::Client::connect(&address.to_string(), n.clone()).unwrap();
    {
        let _ledger = n.lock().unwrap();
        for _ in 0..4 {
            client.poll().unwrap();
        }
        server.join().unwrap();
    }
}

#[test]
fn native_rpc_farming_topics_publish_only_synced_challenges_and_bound_proof_handoff() {
    use vnx::{net::NetworkClient, runtime::OverflowPolicy};
    let (mut network, g, _, _) = fixture();
    network.params.challenge_delay = 3;
    let n = node(&network);
    assert!(n.lock().unwrap().farming_challenges().unwrap().is_empty());
    let mut server = rpc::vnx_server("127.0.0.1:0".parse().unwrap(), n.clone()).unwrap();
    let client = NetworkClient::connect(
        server.endpoint(),
        "Node",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let challenges = client
        .subscribe("harvester.challenges", 32, OverflowPolicy::Reject, false)
        .unwrap();
    assert!(challenges.recv_timeout(Duration::from_millis(50)).is_err());
    let mut source = MemorySource {
        blocks: vec![wire(&g["block"])],
        wrong: false,
    };
    sync::synchronize_pipelined(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    let expected = n.lock().unwrap().farming_challenges().unwrap();
    assert!(!expected.is_empty());
    let sample = challenges.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(sample.value["__type"], "mmx.Challenge");
    let actual: mmx::Challenge = serde_json::from_value(sample.value).unwrap();
    assert!(expected.contains(&actual));
    let state = n.lock().unwrap();
    let tip = state.chain().selected().tip().unwrap().unwrap();
    let (hash, diff) = mmx_consensus::transitions::history::challenge(
        state.chain(),
        &network.params,
        &tip,
        actual.vdf_height - tip.vdf_height,
    )
    .unwrap();
    assert_eq!((actual.challenge.0, actual.difficulty), (hash, diff));
    drop(state);
    client
        .publish("farmer.proof", json!({"__type":"mmx.ProofResponse"}))
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        let status =
            rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#).unwrap();
        if status["result"]["queued_messages"] == 1 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(20));
    }
    client.close();
    server.shutdown();
}

#[test]
fn pending_transaction_crosses_real_peer_transport_without_ledger_writes() {
    let (network, g, steps, _) = fixture();
    let a = node(&network);
    let b = node(&network);
    for n in [&a, &b] {
        sync::synchronize_pipelined(
            &mut MemorySource {
                blocks: vec![wire(&g["block"])],
                wrong: false,
            },
            n,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    }
    let block = wire(&steps[0]["block"]);
    let mut tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .clone();
    tx.fields.insert("exec_result".into(), V::Optional(None));
    tx.fields.insert(
        "static_cost".into(),
        V::Uint(tx.calc_cost(&(&network.params).into()).unwrap()),
    );
    tx.fields.insert(
        "content_hash".into(),
        V::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    a.lock().unwrap().add_transaction(&tx, true).unwrap();
    let mut server = peer::Server::bind("127.0.0.1:0", a.clone()).unwrap();
    let mut client = peer::Client::connect(&server.address().to_string(), b.clone()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while b.lock().unwrap().pending_transactions() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "transaction not relayed"
        );
        client.poll().unwrap();
        b.lock().unwrap().tick().unwrap();
    }
    assert_eq!(a.lock().unwrap().pending_transactions(), 1);
    assert!(!b.lock().unwrap().add_transaction(&tx, true).unwrap());
    assert_eq!(
        b.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
    drop(client);
    server.shutdown();
}

fn farmer_scalar() -> mmx_wallet::FarmerKey {
    let mut scalar = [0; 32];
    scalar[31] = 1;
    mmx_wallet::FarmerKey::from_bytes(&scalar).unwrap()
}
fn pending_fixture_tx(network: &Network, step: &Value) -> Record {
    let block = wire(&step["block"]);
    let mut tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap()
        .clone();
    tx.fields.insert("exec_result".into(), V::Optional(None));
    tx.fields.insert(
        "static_cost".into(),
        V::Uint(tx.calc_cost(&(&network.params).into()).unwrap()),
    );
    tx.fields.insert(
        "content_hash".into(),
        V::Bytes(tx.calc_hash(true, 0).unwrap().to_vec()),
    );
    tx
}
fn native_farming_response() -> Record {
    let row: Value = include_str!("../../mmx-consensus/tests/fixtures/responses.jsonl")
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .find(|r| r["name"] == "extensions1_0")
        .unwrap();
    wire(&row["wire"])
}

fn stronger_native_response() -> Record {
    let row: Value = serde_json::from_str(include_str!("fixtures/production.json")).unwrap();
    assert_eq!(row["verified"], true);
    wire(&row["response"])
}

fn ready_farming_node(network: &Network, genesis: &Value, step: &Value) -> Arc<Mutex<Node>> {
    let n = node(network);
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks: vec![wire(&genesis["block"])],
            wrong: false,
        },
        &n,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    n.lock().unwrap().receive(&wire(&step["vdf"])).unwrap();
    n
}

#[test]
fn verified_proof_announcements_relay_before_block_production_without_local_metadata_or_echoes() {
    let (network, genesis, steps, _) = fixture();
    let n = ready_farming_node(&network, &genesis, &steps[0]);
    let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
    let address = server.address().to_string();
    let mut sender = peer::Connection::connect(&address).unwrap();
    let mut observer = peer::Connection::connect(&address).unwrap();
    for connection in [&mut sender, &mut observer] {
        assert_eq!(
            connection
                .receive(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .name,
            "mmx.ProofOfTime"
        );
    }
    let original = native_farming_response();
    let mut private = original.clone();
    private
        .fields
        .insert("harvester".into(), V::String(b"private farm".to_vec()));
    private.fields.insert("farmer_addr".into(), V::Uint(12345));
    assert!(mmx_consensus::response::is_valid(&private).unwrap());
    sender.send(&private).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#).unwrap()
        ["result"]["queued_messages"]
        == 0
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        observer
            .receive(Duration::from_millis(50))
            .unwrap()
            .is_none(),
        "unverified announcements must not relay"
    );
    n.lock().unwrap().tick().unwrap();
    let public = observer.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(public.name, "mmx.ProofResponse");
    assert_eq!(
        hash(&public, "content_hash").unwrap(),
        hash(&original, "content_hash").unwrap()
    );
    assert!(public
        .field("harvester")
        .unwrap()
        .text()
        .unwrap()
        .is_empty());
    assert_eq!(public.field("farmer_addr").unwrap().uint().unwrap(), 0);
    assert!(mmx_consensus::response::is_valid(&public).unwrap());
    mmx_consensus::response::authenticate(&public).unwrap();
    assert!(
        sender
            .receive(Duration::from_millis(100))
            .unwrap()
            .is_none(),
        "proof must not echo to its sender"
    );
    assert_eq!(
        n.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .height,
        0
    );
    sender.send(&public).unwrap();
    // A following request is a transport barrier: the duplicate was decoded
    // without driving the ledger tick or letting it drain the input queue.
    sender
        .send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(881)),
                (
                    "method".into(),
                    V::Record(record("mmx.Node.get_height", [])),
                ),
            ],
        ))
        .unwrap();
    let reply = sender.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(reply.name, "mmx.Return");
    assert_eq!(reply.field("id").unwrap().uint().unwrap(), 881);
    assert_eq!(
        rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#).unwrap()
            ["result"]["queued_messages"],
        0
    );

    std::thread::sleep(Duration::from_millis(60));
    n.lock().unwrap().tick().unwrap();
    assert!(observer
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    server.shutdown();
}

fn signed_farming_vote(block: Hash, key: mmx_wallet::FarmerKey) -> Record {
    let public = key.public_key();
    let farmer = mmx_farming::farmer::Farmer::new([key], None);
    let mut vote = record(
        "mmx.ValidatorVote",
        [
            ("hash".into(), V::Bytes(block.to_vec())),
            ("farmer_key".into(), V::Bytes(public.to_vec())),
            ("farmer_sig".into(), V::Bytes(vec![0; 64])),
            ("content_hash".into(), V::Bytes(vec![0; 32])),
        ],
    );
    let signature = farmer.sign_vote(&vote).unwrap();
    vote.fields
        .insert("farmer_sig".into(), V::Bytes(signature.to_vec()));
    let id = mmx_consensus::vote::content_hash(&vote).unwrap();
    vote.fields
        .insert("content_hash".into(), V::Bytes(id.to_vec()));
    vote
}

#[test]
fn foreign_live_blocks_work_and_accepted_votes_relay_after_verification_without_echoes() {
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks: vec![wire(&g["block"])],
            wrong: false,
        },
        &n,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
    let address = server.address().to_string();
    let mut sender = peer::Connection::connect(&address).unwrap();
    let mut observer = peer::Connection::connect(&address).unwrap();
    let wait = |count| {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let status =
                rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#)
                    .unwrap();
            if status["result"]["queued_messages"].as_u64().unwrap() >= count {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // A real signed block cannot relay until its VDF and body are verified.
    sender.send(&wire(&steps[0]["block"])).unwrap();
    wait(1);
    n.lock().unwrap().tick().unwrap();
    assert!(observer
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    sender.send(&wire(&steps[0]["vdf"])).unwrap();
    wait(1);
    n.lock().unwrap().tick().unwrap();
    // Native randomized fanout assigns a separate rank to each publication;
    // verification order does not constrain their arrival order on this peer.
    let initial = [
        observer.receive(Duration::from_secs(3)).unwrap().unwrap(),
        observer.receive(Duration::from_secs(3)).unwrap().unwrap(),
    ];
    assert!(initial.contains(&wire(&steps[0]["vdf"])));
    assert!(initial.contains(&wire(&steps[0]["block"])));
    let block = wire(&steps[1]["block"]);
    let id = hash(&block, "hash").unwrap();
    let vote = signed_farming_vote(id, farmer_scalar());
    // Unknown-block votes wait; a different valid signer still cannot vote.
    let mut scalar = [0; 32];
    scalar[31] = 2;
    let unauthorized = signed_farming_vote(id, mmx_wallet::FarmerKey::from_bytes(&scalar).unwrap());
    for message in [&vote, &vote, &unauthorized] {
        sender.send(message).unwrap();
    }
    wait(2);
    n.lock().unwrap().tick().unwrap();
    assert_eq!(n.lock().unwrap().chain().pending_votes(), 2);
    assert!(observer
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    sender.send(&block).unwrap();
    sender.send(&wire(&steps[1]["vdf"])).unwrap();
    wait(2);
    n.lock().unwrap().tick().unwrap();
    let mut received = vec![];
    for _ in 0..3 {
        received.push(observer.receive(Duration::from_secs(3)).unwrap().unwrap());
    }
    assert!(received.contains(&vote));
    assert!(received.contains(&block));
    assert!(received.iter().any(|r| r.name == "mmx.ProofOfTime"));
    assert!(!received.contains(&unauthorized));
    assert_eq!(n.lock().unwrap().chain().vote_count(&id), 1);
    assert_eq!(n.lock().unwrap().chain().pending_votes(), 0);
    for message in [&vote, &block, &wire(&steps[1]["vdf"])] {
        sender.send(message).unwrap();
    }
    sender
        .send(&record(
            "mmx.Request",
            [
                ("id".into(), V::Uint(882)),
                (
                    "method".into(),
                    V::Record(record("mmx.Node.get_height", [])),
                ),
            ],
        ))
        .unwrap();
    let reply = sender.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(reply.name, "mmx.Return");
    assert_eq!(reply.field("id").unwrap().uint().unwrap(), 882);
    assert_eq!(
        rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#).unwrap()
            ["result"]["queued_messages"],
        0
    );
    n.lock().unwrap().tick().unwrap();
    assert!(observer
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    assert!(sender
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    server.shutdown();
    // Historical catchup is silent even though the same bodies are valid.
    let historical = node(&network);
    let mut server = peer::Server::bind("127.0.0.1:0", historical.clone()).unwrap();
    let mut observer = peer::Connection::connect(&server.address().to_string()).unwrap();
    let mut blocks = vec![wire(&g["block"])];
    blocks.extend(steps.iter().take(2).map(|s| wire(&s["block"])));
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks,
            wrong: false,
        },
        &historical,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert!(observer
        .receive(Duration::from_millis(100))
        .unwrap()
        .is_none());
    server.shutdown();
}

#[test]
fn malformed_vote_does_not_poison_replay_cache_and_replays_do_not_grow_pending_work() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let vote = signed_farming_vote([222; 32], farmer_scalar());
    let mut bad = vote.clone();
    bad.fields
        .insert("farmer_sig".into(), V::Bytes(vec![0; 64]));
    assert!(n.lock().unwrap().receive(&bad).is_err());
    for _ in 0..128 {
        n.lock().unwrap().receive(&vote).unwrap();
    }
    assert_eq!(n.lock().unwrap().chain().pending_votes(), 1);
    let mut oversized = signed_farming_vote([223; 32], farmer_scalar());
    oversized
        .fields
        .insert("extra".into(), V::Bytes(vec![0; 4096]));
    assert!(n.lock().unwrap().receive(&oversized).is_err());
    assert_eq!(n.lock().unwrap().chain().pending_votes(), 1);
}

#[test]
fn a_verified_farmer_proof_earns_one_early_block_relay_without_accepting_its_ledger_effects() {
    use mmx_farming::farmer::Farmer;
    use mmx_wallet::Address;
    let (network, genesis, steps, _) = fixture();
    let response = native_farming_response();
    let producer = ready_farming_node(&network, &genesis, &steps[0]);
    producer.lock().unwrap().receive(&response).unwrap();
    producer.lock().unwrap().tick().unwrap();
    let keys = [farmer_scalar().public_key()].into();
    let plan = producer
        .lock()
        .unwrap()
        .farming_plan(&keys, vnx::util::get_wall_time_millis(), 0)
        .unwrap()
        .unwrap();
    let unsigned = plan.build().unwrap();
    let first = Farmer::new([farmer_scalar()], Some(Address([7; 32])))
        .sign_block(&unsigned, &network.params)
        .unwrap();
    let second = Farmer::new([farmer_scalar()], Some(Address([8; 32])))
        .sign_block(&unsigned, &network.params)
        .unwrap();
    assert_ne!(
        hash(&first, "content_hash").unwrap(),
        hash(&second, "content_hash").unwrap()
    );
    for earned in [false, true] {
        let node = ready_farming_node(&network, &genesis, &steps[0]);
        let config = vnx::config::Config::new();
        config.set("Router.relay_target_ms", json!(0)).unwrap();
        node.lock().unwrap().configure_router(&config).unwrap();
        let mut server = peer::Server::bind("127.0.0.1:0", node.clone()).unwrap();
        let mut sender = peer::Connection::connect(&server.address().to_string()).unwrap();
        let mut observer = peer::Connection::connect(&server.address().to_string()).unwrap();
        for peer in [&mut sender, &mut observer] {
            assert_eq!(
                peer.receive(Duration::from_secs(3)).unwrap().unwrap().name,
                "mmx.ProofOfTime"
            );
        }
        if earned {
            node.lock().unwrap().receive(&response).unwrap();
            node.lock().unwrap().tick().unwrap();
            for peer in [&mut sender, &mut observer] {
                assert_eq!(
                    peer.receive(Duration::from_secs(3)).unwrap().unwrap().name,
                    "mmx.ProofResponse"
                );
            }
        }
        let owner = node.lock().unwrap();
        sender.send(&first).unwrap();
        if earned {
            assert_eq!(
                observer.receive(Duration::from_secs(3)).unwrap().unwrap(),
                first
            );
        } else {
            assert!(observer
                .receive(Duration::from_millis(150))
                .unwrap()
                .is_none());
        }
        // Same key and height, different fully signed content: spending the
        // permission on the first block must prevent a second early relay.
        sender.send(&second).unwrap();
        assert!(observer
            .receive(Duration::from_millis(150))
            .unwrap()
            .is_none());
        assert_eq!(owner.chain().selected().tip().unwrap().unwrap().height, 0);
        drop(owner);
        server.shutdown();
    }
}

#[test]
fn frozen_proof_context_rechecks_authentication_root_and_exact_height() {
    use mmx_consensus::response;
    let (network, genesis, steps, _) = fixture();
    let n = ready_farming_node(&network, &genesis, &steps[0]);
    let node = n.lock().unwrap();
    let tip = node.chain().selected().tip().unwrap().unwrap();
    let record = native_farming_response();
    let height = record.field("vdf_height").unwrap().uint().unwrap() as u32;
    let context =
        response::Verification::new(node.chain(), &network.params, &tip, 0, height, 1000).unwrap();
    let direct = response::verify(&record, &network.params, node.chain(), &tip, 0, 1000).unwrap();
    assert_eq!(context.verify(&record).unwrap().hash(), direct.hash());
    let different =
        response::Verification::new(node.chain(), &network.params, &tip, 0, height + 1, 1000)
            .unwrap();
    assert_eq!(
        different.verify(&record).unwrap_err().to_string(),
        "proof challenge context height mismatch"
    );
    let old =
        response::Verification::new(node.chain(), &network.params, &tip, height, height, 1000)
            .unwrap();
    assert_eq!(
        old.verify(&record).unwrap_err().to_string(),
        "proof too old"
    );
    let mut forged = record.clone();
    forged
        .fields
        .insert("farmer_sig".into(), V::Bytes(vec![0; 64]));
    forged.fields.insert(
        "content_hash".into(),
        V::Bytes(response::content_hash(&forged).unwrap().to_vec()),
    );
    assert!(response::is_valid(&forged).unwrap());
    assert!(context.verify(&forged).is_err());
    assert_eq!(
        context.verify(&forged).unwrap_err().to_string(),
        response::verify(&forged, &network.params, node.chain(), &tip, 0, 1000)
            .unwrap_err()
            .to_string()
    );
}

#[test]
fn stronger_native_peer_proof_vetoes_full_block_planning_and_an_inflight_signature() {
    use mmx_farming::farmer::Farmer;
    use mmx_wallet::Address;
    let (network, genesis, steps, _) = fixture();
    let weak = native_farming_response();
    let strong = stronger_native_response();
    let weak_hash =
        mmx_consensus::proof::calc_proof_hash(weak.field("proof").unwrap().record().unwrap())
            .unwrap();
    let strong_hash =
        mmx_consensus::proof::calc_proof_hash(strong.field("proof").unwrap().record().unwrap())
            .unwrap();
    assert!(strong_hash < weak_hash);
    let key = farmer_scalar();
    let keys = [key.public_key()].into();
    for during_signing in [false, true] {
        let n = ready_farming_node(&network, &genesis, &steps[0]);
        n.lock().unwrap().receive(&weak).unwrap();
        n.lock().unwrap().tick().unwrap();
        let now = vnx::util::get_wall_time_millis();
        let candidate = if during_signing {
            let plan = n
                .lock()
                .unwrap()
                .farming_plan(&keys, now, 0)
                .unwrap()
                .unwrap();
            let unsigned = plan.build().unwrap();
            let farmer = Farmer::new([farmer_scalar()], Some(Address([7; 32])));
            let signed = farmer.sign_block(&unsigned, &network.params).unwrap();
            Some(plan.with_signature(&unsigned, &signed).unwrap())
        } else {
            None
        };
        let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
        let mut sender = peer::Connection::connect(&server.address().to_string()).unwrap();
        let mut queued = 1;
        if !during_signing {
            // Place twenty authenticated but invalid space proofs before the
            // stronger proof in the queue's content-hash order. Production must
            // verify the entire eligible batch, not stop at the old 16 limit.
            let strong_content = hash(&strong, "content_hash").unwrap();
            for score in 1..1000 {
                let mut invalid = weak.clone();
                let mut proof = invalid.field("proof").unwrap().record().unwrap().clone();
                proof.fields.insert("score".into(), V::Uint(score + 1000));
                invalid.fields.insert("proof".into(), V::Record(proof));
                let id = invalid.calc_hash(false, 0).unwrap();
                invalid.fields.insert("hash".into(), V::Bytes(id.to_vec()));
                invalid.fields.insert(
                    "farmer_sig".into(),
                    V::Bytes(farmer_scalar().sign(&id).unwrap().to_vec()),
                );
                let content = mmx_consensus::response::content_hash(&invalid).unwrap();
                invalid
                    .fields
                    .insert("content_hash".into(), V::Bytes(content.to_vec()));
                if content < strong_content {
                    assert!(mmx_consensus::response::is_valid(&invalid).unwrap());
                    mmx_consensus::response::authenticate(&invalid).unwrap();
                    sender.send(&invalid).unwrap();
                    queued += 1;
                    if queued == 21 {
                        break;
                    }
                }
            }
            assert_eq!(queued, 21);
        }
        sender.send(&strong).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":1,"method":"get_status"}"#).unwrap()
            ["result"]["queued_messages"]
            .as_u64()
            .unwrap()
            < queued
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut state = n.lock().unwrap();
        if let Some(candidate) = candidate {
            let candidate_hash = hash(candidate.record(), "hash").unwrap();
            assert!(!state.accept_produced(candidate).unwrap());
            assert!(state.chain().block(&candidate_hash).unwrap().is_none());
        } else {
            assert!(state.farming_plan(&keys, now, 0).unwrap().is_none());
        }
        assert_eq!(state.chain().selected().tip().unwrap().unwrap().height, 0);
        let challenge = hash(
            strong.field("proof").unwrap().record().unwrap(),
            "challenge",
        )
        .unwrap();
        assert_eq!(state.chain().best_proof(&challenge), Some(strong_hash));
        assert!(state.farming_plan(&keys, now, 0).unwrap().is_none());
        // The best proof's owner can build; it includes both proofs in native
        // full-hash order rather than selecting only that farmer's proofs.
        let mut secret = [0; 32];
        secret[31] = 2;
        let winner = mmx_wallet::FarmerKey::from_bytes(&secret).unwrap();
        let plan = state
            .farming_plan(&[winner.public_key()].into(), now, 0)
            .unwrap()
            .unwrap();
        let unsigned = plan.build().unwrap();
        let proofs = unsigned.field("proof").unwrap().seq().unwrap();
        assert_eq!(proofs.len(), 2);
        assert_eq!(
            mmx_consensus::proof::calc_proof_hash(proofs[0].record().unwrap()).unwrap(),
            strong_hash
        );
        assert_eq!(
            mmx_consensus::proof::calc_proof_hash(proofs[1].record().unwrap()).unwrap(),
            weak_hash
        );
        let signed = Farmer::new([winner], Some(Address([7; 32])))
            .sign_block(&unsigned, &network.params)
            .unwrap();
        let candidate = plan.with_signature(&unsigned, &signed).unwrap();
        assert!(state.accept_produced(candidate).unwrap());
        drop(state);
        server.shutdown();
    }
}

#[test]
fn native_farming_votes_do_not_create_unsolicited_votes_for_ancestors() {
    let (network, genesis, steps, _) = fixture();
    let n = node(&network);
    let mut blocks = vec![wire(&genesis["block"])];
    blocks.extend(steps.iter().take(2).map(|step| wire(&step["block"])));
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks,
            wrong: false,
        },
        &n,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let mut state = n.lock().unwrap();
    let tip = state.chain().selected().tip().unwrap().unwrap();
    assert!(tip.height > state.root_height() + 1);
    let votes = state
        .farming_votes(&[farmer_scalar().public_key()].into(), 1_000_000)
        .unwrap();
    assert_eq!(votes.len(), 1);
    assert_eq!(hash(&votes[0], "hash").unwrap(), tip.hash.0);
    assert!(state
        .farming_votes(&[farmer_scalar().public_key()].into(), 1_100_000)
        .unwrap()
        .is_empty());
}

#[test]
fn native_production_retries_failed_attempts_but_never_resigns_a_created_block() {
    use mmx_farming::farmer::Farmer;
    use mmx_wallet::Address;
    let (network, genesis, steps, _) = fixture();
    let n = ready_farming_node(&network, &genesis, &steps[0]);
    let keys = [farmer_scalar().public_key()].into();
    let mut state = n.lock().unwrap();
    state.receive(&native_farming_response()).unwrap();
    state.tick().unwrap();
    let now = vnx::util::get_wall_time_millis();
    let first = state.farming_plan(&keys, now, 0).unwrap().unwrap();
    assert!(state.farming_plan(&keys, now, 0).unwrap().is_none());
    state.cancel_production(&first).unwrap();
    let retry = state.farming_plan(&keys, now, 0).unwrap().unwrap();
    let unsigned = retry.build().unwrap();
    let signed = Farmer::new([farmer_scalar()], Some(Address([7; 32])))
        .sign_block(&unsigned, &network.params)
        .unwrap();
    let candidate = retry.with_signature(&unsigned, &signed).unwrap();
    assert!(state.accept_produced(candidate).unwrap());
    // A stale failure callback must not release an already created proof.
    state.cancel_production(&first).unwrap();
    assert!(state.farming_plan(&keys, now, 0).unwrap().is_none());
}
#[test]
fn farming_candidate_matches_cpp_fields_uses_no_disk_writes_and_rejects_stale_signatures() {
    use mmx_farming::farmer::Farmer;
    use mmx_wallet::Address;
    let (network, g, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let n = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    sync::synchronize_pipelined(
        &mut MemorySource {
            blocks: vec![wire(&g["block"])],
            wrong: false,
        },
        &n,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let expected = wire(&steps[0]["block"]);
    let tx = pending_fixture_tx(&network, &steps[0]);
    let mut n = n.lock().unwrap();
    n.add_transaction(&tx, true).unwrap();
    n.receive(&native_farming_response()).unwrap();
    n.receive(&wire(&steps[0]["vdf"])).unwrap();
    let key = farmer_scalar();
    let keys = [key.public_key()].into();
    let files = || {
        fn walk(p: &std::path::Path, out: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
            for e in std::fs::read_dir(p).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.clone(), std::fs::read(p).unwrap()));
                }
            }
        }
        let mut out = vec![];
        walk(dir.path(), &mut out);
        out.sort();
        out
    };
    let before = files();
    let time = match expected.field("time_stamp").unwrap() {
        V::Int(n) => *n,
        _ => panic!(),
    };
    let plan = n.farming_plan(&keys, time, 0).unwrap().unwrap();
    let unsigned = plan.build().unwrap();
    for field in [
        "prev",
        "height",
        "time_stamp",
        "time_diff",
        "space_diff",
        "weight",
        "total_weight",
        "vdf_height",
        "vdf_count",
        "vdf_iters",
        "vdf_output",
        "vdf_reward_addr",
        "vdf_reward_payout",
        "proof_hash",
        "proof_chain",
        "challenge",
        "is_space_fork",
        "space_fork_len",
        "space_fork_proofs",
        "base_reward",
        "reward_vote_sum",
        "reward_vote_count",
        "txfee_buffer",
        "tx_count",
        "tx_fees",
        "tx_hash",
        "static_cost",
        "total_cost",
        "reward_amount",
        "tx_list",
    ] {
        assert_eq!(
            unsigned.field(field).unwrap(),
            expected.field(field).unwrap(),
            "{field}"
        );
    }
    let reward: [u8; 32] = expected
        .field("reward_addr")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let farmer = Farmer::new([key], Some(Address(reward)));
    let signed = farmer.sign_block(&unsigned, &network.params).unwrap();
    let candidate = plan.with_signature(&unsigned, &signed).unwrap();
    assert_eq!(
        files(),
        before,
        "candidate execution must not write ledger files"
    );
    let mut changed = signed.clone();
    changed
        .fields
        .insert("project_addr".into(), V::Bytes(vec![99; 32]));
    assert!(plan.with_signature(&unsigned, &changed).is_err());
    assert!(
        n.farming_plan(&keys, time, 0).unwrap().is_none(),
        "one concurrent signing attempt per proof"
    );
    let duplicate = plan.with_signature(&unsigned, &signed).unwrap();
    assert!(n.accept_produced(candidate).unwrap());
    assert!(
        !n.accept_produced(duplicate).unwrap(),
        "stale signing reply must be discarded"
    );
    assert_eq!(n.chain().selected().tip().unwrap().unwrap().height, 1);
    assert_eq!(n.pending_transactions(), 0);
    for b in steps[0]["state"]["balances"].as_array().unwrap() {
        assert_eq!(
            n.chain()
                .selected()
                .storage()
                .get_balance(
                    &raw(&b["address"]).try_into().unwrap(),
                    &raw(&b["currency"]).try_into().unwrap()
                )
                .unwrap(),
            Some(b["amount"].as_str().unwrap().parse().unwrap())
        );
    }
    n.rollback_to(0).unwrap();
    assert_eq!(
        n.chain()
            .selected()
            .storage()
            .get_balance(&reward, &[0; 32])
            .unwrap(),
        Some(10_000_000)
    );
}

#[test]
fn private_farmer_rpc_worker_produces_and_relays_a_live_block() {
    use mmx_farming::{farmer::Farmer, service};
    use mmx_wallet::Address;
    use vnx::runtime::Runtime;
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    let observer = node(&network);
    for state in [&n, &observer] {
        sync::synchronize_pipelined(
            &mut MemorySource {
                blocks: vec![wire(&g["block"])],
                wrong: false,
            },
            state,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    }
    let expected = wire(&steps[0]["block"]);
    let reward = expected
        .field("reward_addr")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let runtime = Runtime::new().unwrap();
    runtime
        .register(
            "Farmer",
            service::farmer_methods(
                Arc::new(Mutex::new(Farmer::new(
                    [farmer_scalar()],
                    Some(Address(reward)),
                ))),
                network.params.clone(),
                1,
            ),
        )
        .unwrap();
    let mut farmer = service::bind_private(
        "127.0.0.1:0".parse().unwrap(),
        runtime,
        service::private_options(["Farmer".into()]),
    )
    .unwrap();
    n.lock()
        .unwrap()
        .add_transaction(&pending_fixture_tx(&network, &steps[0]), true)
        .unwrap();
    n.lock()
        .unwrap()
        .receive(&native_farming_response())
        .unwrap();
    for state in [&n, &observer] {
        state
            .lock()
            .unwrap()
            .receive(&wire(&steps[0]["vdf"]))
            .unwrap();
    }
    let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
    let mut remote =
        peer::Client::connect(&server.address().to_string(), observer.clone()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut worker =
        mmx_node::production::Worker::start(n.clone(), farmer.endpoint().clone(), stop.clone(), 0)
            .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while observer
        .lock()
        .unwrap()
        .chain()
        .selected()
        .tip()
        .unwrap()
        .unwrap()
        .height
        == 0
    {
        assert!(
            std::time::Instant::now() < deadline,
            "produced block did not reach peer"
        );
        remote.poll().unwrap();
        observer.lock().unwrap().tick().unwrap();
    }
    assert_eq!(
        n.lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .hash,
        observer
            .lock()
            .unwrap()
            .chain()
            .selected()
            .tip()
            .unwrap()
            .unwrap()
            .hash
    );
    worker.shutdown();
    drop(remote);
    server.shutdown();
    farmer.shutdown();
}

#[test]
fn public_rpc_rejects_multiplicative_balance_work_and_oversized_transaction_batches() {
    let (network, _, _, _) = fixture();
    let n = node(&network);
    let status = rpc::json_request(&n, &serde_json::to_vec(&json!({
        "jsonrpc":"2.0", "id":1, "method":"get_transactions", "params":{"ids":vec![[0u8;32];129]}
    })).unwrap()).unwrap();
    assert!(status["error"]["message"]
        .as_str()
        .unwrap()
        .contains("128 ids"));
    let owners: Vec<_> = (0u32..101)
        .map(|v| {
            let mut a = [0u8; 32];
            a[..4].copy_from_slice(&v.to_le_bytes());
            a
        })
        .collect();
    let status = rpc::json_request(
        &n,
        &serde_json::to_vec(&json!({
            "jsonrpc":"2.0", "id":2, "method":"get_all_balances", "params":{
                "addresses":owners, "whitelist":owners, "limit":-1
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(status["error"]["message"]
        .as_str()
        .unwrap()
        .contains("10000 point reads"));
    let good = rpc::json_request(&n, br#"{"jsonrpc":"2.0","id":3,"method":"get_height"}"#).unwrap();
    assert_eq!(good["result"], 0);
}

#[test]
fn production_announcements_cross_outgoing_peers_while_rpc_or_partial_frames_are_blocked() {
    for partial_frame in [false, true] {
        let (network, genesis, steps, _) = fixture();
        let n = ready_farming_node(&network, &genesis, &steps[0]);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (entered, request_received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap().0;
            let mut raw = stream.try_clone().unwrap();
            let mut connection = peer::Connection::new(stream).unwrap();
            let request = loop {
                let message = connection.receive(Duration::from_secs(3)).unwrap().unwrap();
                if message.name == "mmx.Request" {
                    break message;
                }
                assert_eq!(message.name, "mmx.ProofOfTime");
            };
            assert_eq!(
                request.field("method").unwrap().record().unwrap().name,
                "mmx.Node.get_height"
            );
            let reply = record(
                "mmx.Return",
                [
                    ("id".into(), request.field("id").unwrap().clone()),
                    (
                        "result".into(),
                        V::Record(record(
                            "mmx.Node.get_height.return",
                            [("_ret_0".into(), V::Uint(0))],
                        )),
                    ),
                ],
            );
            let frame = peer::encode_frame(&reply).unwrap();
            if partial_frame {
                raw.write_all(&frame[..1]).unwrap();
            }
            entered.send(()).unwrap();
            let start = std::time::Instant::now();
            loop {
                let message = connection.receive(Duration::from_secs(2)).unwrap().unwrap();
                if message.name == "mmx.ProofResponse" {
                    assert!(mmx_consensus::response::is_valid(&message).unwrap());
                    assert!(start.elapsed() < Duration::from_secs(2));
                    break;
                }
                assert_eq!(message.name, "mmx.ProofOfTime");
            }
            // Neither a deliberately withheld reply nor a trickled frame can
            // hold up completed outgoing work on this same connection.
            if partial_frame {
                raw.write_all(&frame[1..]).unwrap();
            } else {
                connection.send(&reply).unwrap();
            }
        });
        let mut client = peer::Client::connect(&address.to_string(), n.clone()).unwrap();
        let request = std::thread::spawn(move || client.height().unwrap());
        request_received
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        n.lock()
            .unwrap()
            .receive(&native_farming_response())
            .unwrap();
        n.lock().unwrap().tick().unwrap();
        assert_eq!(request.join().unwrap(), 0);
        server.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn header_and_content_hash_rpcs_do_not_read_transaction_payloads() {
    use std::os::unix::fs::FileExt;
    let (network, _, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let mut chain = ForkChain::open_native(dir.path(), network.params.clone()).unwrap();
    chain.initialize(&network.assets).unwrap();
    let root = chain.selected().hash_at(0).unwrap().unwrap();
    let block = wire(&steps[0]["block"]);
    chain.admit(&block, &[wire(&steps[0]["vdf"])]).unwrap();
    chain.update(root, None).unwrap();
    let node = Node::from_chain(chain, SigningKey::from_slice(&[1; 32]).unwrap()).unwrap();
    let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let payload = tx.encode_compact().unwrap();
    let path = dir.path().join("archive/native-data-0000000000000000.log");
    let bytes = std::fs::read(&path).unwrap();
    let positions: Vec<_> = bytes
        .windows(payload.len())
        .enumerate()
        .filter(|(_, v)| *v == payload)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(positions.len(), 1);
    let offset = positions[0] + payload.len() - 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .write_at(&[bytes[offset] ^ 1], offset as u64)
        .unwrap();
    let id = hash(&block, "hash").unwrap();
    assert!(node
        .call(&record(
            "mmx.Node.get_block",
            [("hash".into(), V::Bytes(id.to_vec()))]
        ))
        .is_err());
    let mut expected = block.clone();
    expected.name = "mmx.BlockHeader".into();
    expected.fields.remove("tx_list");
    for request in [
        record(
            "mmx.Node.get_header",
            [("hash".into(), V::Bytes(id.to_vec()))],
        ),
        record("mmx.Node.get_header_at", [("height".into(), V::Uint(1))]),
    ] {
        assert_eq!(
            node.call(&request)
                .unwrap()
                .field("_ret_0")
                .unwrap()
                .record()
                .unwrap(),
            &expected
        );
    }
    let pair = node
        .call(&record(
            "mmx.Node.get_block_hash_ex",
            [("height".into(), V::Uint(1))],
        ))
        .unwrap();
    assert_eq!(
        pair.field("_ret_0")
            .unwrap()
            .optional()
            .unwrap()
            .unwrap()
            .seq()
            .unwrap(),
        &[
            V::Bytes(id.to_vec()),
            block.field("content_hash").unwrap().clone()
        ]
    );
}

#[test]
fn purged_peer_authentication_uses_genesis_identity_without_revealing_history() {
    let (network, g, steps, _) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let serving = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    sync::synchronize(
        &mut MemorySource {
            blocks: vec![
                wire(&g["block"]),
                wire(&steps[0]["block"]),
                wire(&steps[1]["block"]),
            ],
            wrong: false,
        },
        &serving,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    serving.lock().unwrap().purge_before(1).unwrap();
    let mut server = peer::Server::bind("127.0.0.1:0", serving).unwrap();
    let mut receiving_chain = ForkChain::memory(network.params.clone());
    receiving_chain.initialize(&network.assets).unwrap();
    let receiving = Arc::new(Mutex::new(
        Node::from_chain(receiving_chain, SigningKey::from_slice(&[2; 32]).unwrap()).unwrap(),
    ));
    let own: Hash = receiving
        .lock()
        .unwrap()
        .call(&record("mmx.Router.get_id", []))
        .unwrap()
        .field("_ret_0")
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let mut client = peer::Client::connect(&server.address().to_string(), receiving).unwrap();
    assert_eq!(client.hash_at(0).unwrap(), None);
    assert_eq!(client.block_at(0).unwrap(), None);
    let genesis = hash(&wire(&g["block"]), "hash").unwrap();
    let (id, height) = client.qualify(genesis, own).unwrap();
    assert_ne!(id, own);
    assert_eq!(height, 2);
    assert!(client.qualify([255; 32], own).is_err());
    assert_eq!(client.hash_at(0).unwrap(), None);
    assert_eq!(client.block_at(0).unwrap(), None);
    assert!(client.block_at(1).unwrap().is_some());
    drop(client);
    server.shutdown();
}

#[test]
fn rpc_submission_and_peer_gossip_keep_native_reliable_upload_semantics() {
    let (network, g, steps, _) = fixture();
    let tx = pending_fixture_tx(&network, &steps[0]);
    for local in [false, true] {
        let n = ready_farming_node(&network, &g, &steps[0]);
        let config = vnx::config::Config::new();
        config.set("Router.relay_target_ms", json!(0)).unwrap();
        config.set("Router.max_pending_cost", json!(0)).unwrap();
        n.lock().unwrap().configure_router(&config).unwrap();
        let mut server = peer::Server::bind("127.0.0.1:0", n.clone()).unwrap();
        let mut observer = peer::Connection::connect(&server.address().to_string()).unwrap();
        assert_eq!(
            observer
                .receive(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .name,
            "mmx.ProofOfTime"
        );
        if local {
            n.lock().unwrap().add_transaction(&tx, false).unwrap();
        } else {
            n.lock().unwrap().receive(&tx).unwrap();
        }
        assert_eq!(n.lock().unwrap().pending_transactions(), 1);
        if local {
            let sent = observer.receive(Duration::from_secs(3)).unwrap().unwrap();
            assert_eq!(sent.name, "mmx.Transaction");
            assert_eq!(
                hash(&sent, "content_hash").unwrap(),
                hash(&tx, "content_hash").unwrap()
            );
        } else {
            assert!(observer
                .receive(Duration::from_millis(200))
                .unwrap()
                .is_none());
        }
        drop(observer);
        server.shutdown();
    }
}

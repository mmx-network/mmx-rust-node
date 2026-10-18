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
    Arc::new(Mutex::new(
        Node::from_chain(chain, SigningKey::from_slice(&[1; 32]).unwrap()).unwrap(),
    ))
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
    live_vdf_evidence(false);
}
#[test]
fn background_vdf_releases_farmer_response_and_authenticates_the_live_block() {
    live_vdf_evidence(true);
}
fn live_vdf_evidence(background: bool) {
    let (network, g, steps, _) = fixture();
    let n = node(&network);
    if background {
        n.lock().unwrap().enable_background_vdf().unwrap();
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
        connection.send(&proof).unwrap();
    });
    let mut client = peer::Client::connect(&address.to_string(), n.clone()).unwrap();
    let locked = n.lock().unwrap();
    let (done, complete) = mpsc::channel();
    let receiving = thread::spawn(move || {
        client.poll().unwrap();
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

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
    sync::synchronize(&mut initial, &serving, &AtomicBool::new(false), |_| {}).unwrap();
    let mut server = peer::Server::bind("127.0.0.1:0", serving.clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let receiving = Arc::new(Mutex::new(Node::open(dir.path(), &network).unwrap()));
    let mut client =
        peer::Client::connect(&server.address().to_string(), receiving.clone()).unwrap();
    let progress =
        sync::synchronize(&mut client, &receiving, &AtomicBool::new(false), |_| {}).unwrap();
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
    sync::synchronize(&mut alternate, &serving, &AtomicBool::new(false), |_| {}).unwrap();
    check(&serving, &deep[1]);
    receiving.lock().unwrap().begin_resync();
    let progress =
        sync::synchronize(&mut client, &receiving, &AtomicBool::new(false), |_| {}).unwrap();
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
    sync::synchronize(&mut source, &receiving, &AtomicBool::new(false), |_| {}).unwrap();
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
    assert!(sync::synchronize(&mut source, &n, &AtomicBool::new(false), |_| {}).is_err());
    check(&n, &g);
    source.wrong = false;
    assert!(sync::synchronize(&mut source, &n, &AtomicBool::new(true), |_| {}).is_err());
    check(&n, &g);
    source.blocks[0]
        .fields
        .insert("hash".into(), V::Bytes(vec![1; 32]));
    assert!(sync::synchronize(&mut source, &n, &AtomicBool::new(false), |_| {}).is_err());
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
    let (network, g, steps, _) = fixture();
    let n = node(&network);
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
    sync::synchronize(&mut source, &n, &AtomicBool::new(false), |_| {}).unwrap();
    assert!(n.lock().unwrap().is_synced());
    n.lock()
        .unwrap()
        .receive(&wire(&steps[0]["block"]))
        .unwrap();
    check(&n, &steps[0]);
}

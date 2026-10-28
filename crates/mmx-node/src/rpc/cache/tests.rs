use super::*;
use crate::rpc::{
    dispatch, from_json_record, history,
    tests::{fixture, fixture_at},
};
use mmx_consensus::Hash;
use std::time::{Duration, Instant};
use vnx::serde_json::{json, Value as Json};

fn request(method: &str, mut args: Json) -> Record {
    args["__type"] = json!(format!("mmx.Node.{method}"));
    from_json_record(&args).unwrap()
}
fn cached(node: &Node, method: &str, args: Json) -> Json {
    let request = request(method, args);
    let wire = probe(node, &request)
        .unwrap()
        .0
        .expect("expected cache hit");
    vnx::serde_json::from_slice(&wire).unwrap()
}
fn apply(node: &mut Node, block: &Record, vdf: &Record) {
    node.chain.admit(block, std::slice::from_ref(vdf)).unwrap();
    let root = node.chain.selected().hash_at(0).unwrap().unwrap();
    let update = node.chain.update(root, None).unwrap();
    assert!(!update.applied.is_empty());
    node.peer_state()
        .publish(node.chain.selected().tip().unwrap().unwrap().height, false);
}
fn all_history(owners: Vec<Hash>) -> Json {
    json!({"addresses":owners,"filter":{"since":0,"until":u32::MAX,"limit":-1,"max_search":0,"currency":[],"type":null,"memo":null,"white_list":false,"with_pending":false}})
}

#[test]
fn byte_bound_lfu_oldest_ties_oversized_entries_and_index_cleanup() {
    let charge = ENTRY_BYTES + 3 + 10;
    let mut cache = Cache::new(BASE_BYTES + 3 * charge, 0);
    for k in 1..=3 {
        cache.insert(
            vec![k].into_boxed_slice(),
            vec![0; 10].into_boxed_slice(),
            Some(k.into()),
            false,
        );
    }
    assert_eq!(cache.stats.used_bytes, cache.stats.limit_bytes);
    cache.get(&[1]);
    cache.get(&[1]);
    cache.insert(
        vec![4].into_boxed_slice(),
        vec![0; 10].into_boxed_slice(),
        Some(4),
        false,
    );
    assert!(cache.entries.contains_key([1].as_slice()));
    assert!(!cache.entries.contains_key([2].as_slice()));
    assert_eq!(cache.stats.evictions, 1);
    cache.insert(
        vec![99].into_boxed_slice(),
        vec![0; cache.stats.limit_bytes].into_boxed_slice(),
        None,
        false,
    );
    assert_eq!(
        cache.stats.evictions, 1,
        "oversized values must not flush useful entries"
    );
    for k in 5..=250u8 {
        cache.insert(
            vec![k].into_boxed_slice(),
            vec![0; 10].into_boxed_slice(),
            Some(k.into()),
            false,
        );
        assert!(cache.stats.used_bytes <= cache.stats.limit_bytes);
        assert_eq!(cache.entries.len(), cache.order.len());
        assert_eq!(
            cache.entries.len(),
            cache
                .dependencies
                .values()
                .map(BTreeSet::len)
                .sum::<usize>()
        );
    }
    cache.selected(0, 1);
    assert!(cache.entries.is_empty());
    assert!(cache.order.is_empty());
    assert!(cache.dependencies.is_empty());
    assert_eq!(cache.stats.used_bytes, 0);
}

#[test]
fn publications_push_public_json_blocks_transactions_and_headers_without_database_reads_on_hits() {
    let (node, block, vdf) = fixture();
    let mut node = node.lock().unwrap();
    assert_eq!(node.rpc_cache_stats().limit_bytes, DEFAULT_BYTES);
    apply(&mut node, &block, &vdf);
    let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let methods = [
        ("get_block_at", json!({"height":1})),
        ("get_block", json!({"hash":hash(&block,"hash").unwrap()})),
        ("get_header_at", json!({"height":1})),
        ("get_header", json!({"hash":hash(&block,"hash").unwrap()})),
        ("get_block_hash", json!({"height":1})),
        ("get_block_hash_ex", json!({"height":1})),
        ("get_tx_ids_at", json!({"height":1})),
        ("get_tx_height", json!({"id":hash(tx,"id").unwrap()})),
        (
            "get_transaction",
            json!({"id":hash(tx,"id").unwrap(),"pending":false}),
        ),
    ];
    let expected: Vec<_> = methods
        .iter()
        .map(|(m, a)| {
            crate::rpc::response_json(&node.call(&request(m, a.clone())).unwrap()).unwrap()
        })
        .collect();
    // Deliberately replace the entire ledger with an uninitialized chain. A
    // hit must still succeed: even tip/index lookups would fail this check.
    let empty =
        mmx_execution::chain::forks::ForkChain::memory(node.chain.selected().params().clone());
    let selected = std::mem::replace(&mut node.chain, empty);
    for ((method, args), expected) in methods.into_iter().zip(expected) {
        assert_eq!(cached(&node, method, args), expected);
    }
    node.chain = selected;
    assert_eq!(node.rpc_cache_stats().hits, 9);
}

#[test]
fn batched_transactions_are_assembled_from_pushed_entries_without_ledger_reads() {
    let (node, block, vdf) = fixture();
    let mut n = node.lock().unwrap();
    apply(&mut n, &block, &vdf);
    let id = hash(
        block.field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap(),
        "id",
    )
    .unwrap();
    let query = request("get_transactions", json!({"ids":[id,id]}));
    let expected = crate::rpc::response_json(&n.call(&query).unwrap()).unwrap();
    let empty = mmx_execution::chain::forks::ForkChain::memory(n.chain.selected().params().clone());
    let chain = std::mem::replace(&mut n.chain, empty);
    assert_eq!(
        vnx::serde_json::from_slice::<Json>(&probe(&n, &query).unwrap().0.unwrap()).unwrap(),
        expected
    );
    n.chain = chain;
}

#[test]
fn public_http_writes_cached_json_directly_with_fresh_ids_batches_and_notifications() {
    use std::io::{Read, Write};
    let (node, block, vdf) = fixture();
    let (chain, payload) = {
        let mut n = node.lock().unwrap();
        apply(&mut n, &block, &vdf);
        let payload = probe(&n, &request("get_block_at", json!({"height":1})))
            .unwrap()
            .0
            .unwrap();
        assert!(vnx::serde_json::from_slice::<Json>(&payload)
            .unwrap()
            .is_object());
        let empty =
            mmx_execution::chain::forks::ForkChain::memory(n.chain.selected().params().clone());
        (std::mem::replace(&mut n.chain, empty), payload)
    };
    let mut server = crate::rpc::HttpServer::bind("127.0.0.1:0", node.clone()).unwrap();
    let send = |value: Json| {
        let body = vnx::serde_json::to_vec(&value).unwrap();
        let mut stream = std::net::TcpStream::connect(server.address()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(stream,"POST /rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        assert!(reply.starts_with(b"HTTP/1.1 200"));
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let header = std::str::from_utf8(&reply[..split]).unwrap();
        assert!(header.contains(&format!("Content-Length: {}\r\n", reply.len() - split)));
        let body = &reply[split..];
        assert!(body.windows(payload.len()).any(|w| w == payload));
        vnx::serde_json::from_slice::<Json>(body).unwrap()
    };
    let escaped_id = "quoted\" id\n";
    // Real HTTP hits must continue even while the node is replaying a chain
    // update under its ledger mutex. The selected ledger is also unavailable.
    let mut guard = node.lock().unwrap();
    let call =
        json!({"jsonrpc":"2.0","id":escaped_id,"method":"get_block_at","params":{"height":1}});
    let first = send(call.clone());
    assert_eq!(first["id"], escaped_id);
    assert_eq!(
        first["result"],
        vnx::serde_json::from_slice::<Json>(&payload).unwrap()
    );
    let mut notification = call.clone();
    notification.as_object_mut().unwrap().remove("id");
    let second = json!({"jsonrpc":"2.0","id":2,"method":"get_block_at","params":{"height":1}});
    let batch = send(json!([call, notification, second]));
    assert_eq!(batch.as_array().unwrap().len(), 2);
    assert_eq!(batch[1]["id"], 2);
    guard.chain = chain;
    guard.set_rpc_cache_limit(0);
    drop(guard);
    // Listener handles stay valid when the cache budget changes at runtime.
    assert_eq!(
        send(json!({"jsonrpc":"2.0","id":3,"method":"get_block_at","params":{"height":1}}))["id"],
        3
    );
    assert_eq!(node.lock().unwrap().rpc_cache_stats().entries, 0);
    node.lock().unwrap().set_rpc_cache_limit(DEFAULT_BYTES);
    assert_eq!(
        send(json!({"jsonrpc":"2.0","id":4,"method":"get_block_at","params":{"height":1}}))["id"],
        4
    );
    assert!(node.lock().unwrap().rpc_cache_stats().entries > 0);
    server.shutdown();
}

#[test]
fn actual_fork_discards_old_history_and_transaction_locations() {
    let (node, block, vdf) = fixture();
    let mut n = node.lock().unwrap();
    apply(&mut n, &block, &vdf);
    let old_tx = hash(
        block.field("tx_list").unwrap().seq().unwrap()[0]
            .record()
            .unwrap(),
        "id",
    )
    .unwrap();
    let args = all_history(vec![hash(
        &mmx_execution::chain::history::entries(&block, n.chain.selected().params()).unwrap()[0],
        "address",
    )
    .unwrap()]);
    let query = request("get_history", args);
    let (_, fill) = probe(&n, &query).unwrap();
    fill.unwrap().finish(&n.call(&query).unwrap()).unwrap();
    let corpus: Vec<Json> = include_str!("../../../../mmx-execution/tests/fixtures/chain.jsonl")
        .lines()
        .map(|s| vnx::serde_json::from_str(s).unwrap())
        .collect();
    let raw = |value: &Json| {
        Record::decode(&vnx::util::from_hex_string(value.as_str().unwrap()).unwrap()).unwrap()
    };
    let steps = corpus.iter().find(|r| r["kind"] == "deep_fork").unwrap()["steps"]
        .as_array()
        .unwrap();
    for step in steps {
        n.chain
            .admit(&raw(&step["block"]), &[raw(&step["vdf"])])
            .unwrap();
    }
    let root = n.chain.selected().hash_at(0).unwrap().unwrap();
    let update = n.chain.update(root, None).unwrap();
    assert_eq!(update.common_ancestor, Some(root));
    assert_eq!(n.chain.selected().tip().unwrap().unwrap().height, 2);
    assert!(probe(&n, &query).unwrap().0.is_none());
    let location = request("get_tx_height", json!({"id":old_tx}));
    // Native branches can contain the same transaction. A pushed location
    // must agree with its newly selected occurrence, not its old branch.
    if let Some(wire) = probe(&n, &location).unwrap().0 {
        assert_eq!(
            vnx::serde_json::from_slice::<Json>(&wire).unwrap(),
            crate::rpc::response_json(&n.call(&location).unwrap()).unwrap()
        );
    }
    assert!(probe(
        &n,
        &request("get_block", json!({"hash":hash(&block,"hash").unwrap()}))
    )
    .unwrap()
    .0
    .is_none());
    assert_eq!(
        cached(&n, "get_block_at", json!({"height":1})),
        crate::rpc::to_json_record(&raw(&steps[0]["block"])).unwrap()
    );
}

#[test]
fn public_json_cache_bypasses_local_vnx_and_pending_history_stays_current() {
    let (node, block, _) = fixture();
    let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let sender: Hash = tx
        .field("sender")
        .unwrap()
        .optional()
        .unwrap()
        .unwrap()
        .bytes()
        .unwrap()
        .try_into()
        .unwrap();
    let args = all_history(vec![sender]);
    let expected = dispatch(&node, "get_history", args.clone()).unwrap();
    let before = node.lock().unwrap().rpc_cache_stats().hits;
    let mut server = crate::rpc::vnx_server("127.0.0.1:0".parse().unwrap(), node.clone()).unwrap();
    let client = vnx::net::NetworkClient::connect(
        server.endpoint(),
        "Node",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_eq!(
        client
            .call_value("mmx.Node.get_history", args.clone())
            .unwrap()["_ret_0"],
        expected
    );
    assert_eq!(
        node.lock().unwrap().rpc_cache_stats().hits,
        before,
        "local VNX does not use the public HTTP cache"
    );
    assert_eq!(
        dispatch(&node, "get_history", args.clone()).unwrap(),
        expected
    );
    assert!(node.lock().unwrap().rpc_cache_stats().hits > before);
    let mut pending_args = args;
    pending_args["filter"]["with_pending"] = json!(true);
    let before_pending = dispatch(&node, "get_history", pending_args.clone()).unwrap();
    {
        let mut n = node.lock().unwrap();
        n.synced = true;
        assert!(n.add_transaction(tx, true).unwrap());
    }
    let pending = dispatch(&node, "get_history", pending_args).unwrap();
    assert!(pending.as_array().unwrap().len() > before_pending.as_array().unwrap().len());
    assert_eq!(pending[0]["is_pending"], true);
    drop(client);
    server.shutdown();
}

#[test]
fn history_pushes_forward_and_rewind_rejects_inflight_fills_including_same_hash_aba() {
    for disk in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (node, block, vdf) = fixture_at(disk.then_some(dir.path()));
        let owners: Vec<_> = mmx_execution::chain::history::entries(
            &block,
            node.lock().unwrap().chain.selected().params(),
        )
        .unwrap()
        .iter()
        .map(|r| hash(r, "address").unwrap())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
        let args = all_history(owners);
        let before = dispatch(&node, "get_history", args.clone()).unwrap();
        let balance = request(
            "get_balance",
            json!({"address":vec![9;32],"currency":vec![0;32]}),
        );
        let mut n = node.lock().unwrap();
        let (_, old_fill) = probe(&n, &balance).unwrap();
        let old_response = n.call(&balance).unwrap();
        apply(&mut n, &block, &vdf);
        let history_request = request("get_history", args.clone());
        let expected = n.call(&history_request).unwrap();
        assert_eq!(
            cached(&n, "get_history", args.clone()),
            crate::rpc::response_json(&expected).unwrap(),
            "address history must advance through push events"
        );
        old_fill.unwrap().finish(&old_response).unwrap();
        assert!(probe(&n, &balance).unwrap().0.is_none());
        let (_, fill) = probe(&n, &balance).unwrap();
        n.chain.rollback_to(0).unwrap();
        apply(&mut n, &block, &vdf); // Same tip hash at a new publication generation.
        fill.unwrap().finish(&old_response).unwrap();
        assert!(probe(&n, &balance).unwrap().0.is_none());
        n.chain.rollback_to(0).unwrap();
        assert_eq!(n.rpc_cache_stats().entries, 0);
        drop(n);
        assert_eq!(dispatch(&node, "get_history", args).unwrap(), before);
    }
}

#[test]
fn disabled_cache_pending_reads_and_validation_never_retain_dynamic_results() {
    let (node, block, vdf) = fixture();
    let mut n = node.lock().unwrap();
    let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let pending = request(
        "get_transaction",
        json!({"id":hash(tx,"id").unwrap(),"pending":true}),
    );
    apply(&mut n, &block, &vdf);
    assert!(probe(&n, &pending).unwrap().1.is_none());
    let mut args = all_history(vec![[1; 32]]);
    args["filter"]["with_pending"] = json!(true);
    assert!(probe(&n, &request("get_history", args))
        .unwrap()
        .1
        .is_none());
    let validate = request(
        "validate",
        json!({"tx":crate::rpc::to_json_record(tx).unwrap()}),
    );
    assert!(probe(&n, &validate).unwrap().1.is_none());
    n.set_rpc_cache_limit(0);
    assert_eq!(n.rpc_cache_stats().limit_bytes, 0);
    assert!(probe(&n, &request("get_block_at", json!({"height":1})))
        .unwrap()
        .0
        .is_none());
    n.set_rpc_cache_limit(2 * 1024 * 1024 * 1024);
    assert_eq!(n.rpc_cache_stats().limit_bytes, 2 * 1024 * 1024 * 1024);
}

#[test]
fn native_history_filters_memos_full_height_limit_and_detached_readers() {
    let (node, block, vdf) = fixture();
    let mut n = node.lock().unwrap();
    apply(&mut n, &block, &vdf);
    let rows = mmx_execution::chain::history::entries(&block, n.chain.selected().params()).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.field("type").unwrap().uint().unwrap())
            .collect::<Vec<_>>(),
        [
            mmx_types::mmx::tx_type_e::REWARD.value,
            mmx_types::mmx::tx_type_e::PROJECT_REWARD.value,
            mmx_types::mmx::tx_type_e::RECEIVE.value,
            mmx_types::mmx::tx_type_e::TXFEE.value,
            mmx_types::mmx::tx_type_e::SPEND.value,
        ]
        .map(u64::from),
        "native index ordinals put outputs before inputs"
    );
    let owners = rows
        .iter()
        .map(|r| hash(r, "address").unwrap())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut args = all_history(owners);
    let query = request("get_history", args.clone());
    let task = history::Task::capture(&n, &query).unwrap();
    let expected = task
        .call(&query, Instant::now() + Duration::from_secs(5))
        .unwrap();
    assert!(expected.field("_ret_0").unwrap().seq().unwrap().len() >= rows.len());
    args["filter"]["since"] = json!(1);
    args["filter"]["limit"] = json!(1);
    let one_height = n.call(&request("get_history", args.clone())).unwrap();
    assert_eq!(
        one_height.field("_ret_0").unwrap().seq().unwrap().len(),
        rows.len(),
        "keep every row at the cutoff height"
    );
    args["filter"]["limit"] = json!(0);
    assert!(n
        .call(&request("get_history", args.clone()))
        .unwrap()
        .field("_ret_0")
        .unwrap()
        .seq()
        .unwrap()
        .is_empty());
    args["filter"]["limit"] = json!(-1);
    args["filter"]["type"] = json!("TXFEE");
    let fees = n.call(&request("get_history", args.clone())).unwrap();
    assert!(!fees.field("_ret_0").unwrap().seq().unwrap().is_empty());
    for fee in fees.field("_ret_0").unwrap().seq().unwrap() {
        assert_eq!(
            fee.record().unwrap().field("type").unwrap().uint().unwrap(),
            u64::from(mmx_types::mmx::tx_type_e::TXFEE.value)
        );
    }
    args["filter"]["type"] = Json::Null;
    args["memo"] = json!("no-such-memo");
    assert!(n
        .call(&request("get_history_memo", args))
        .unwrap()
        .field("_ret_0")
        .unwrap()
        .seq()
        .unwrap()
        .is_empty());
    n.chain.rollback_to(0).unwrap();
    assert_eq!(
        task.call(&query, Instant::now() + Duration::from_secs(5))
            .unwrap(),
        expected,
        "detached reader pins the old branch"
    );
}

#[test]
fn history_index_and_public_retention_floor_survive_native_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (node, block, vdf) = fixture_at(Some(dir.path()));
    let (params, query, expected) = {
        let mut n = node.lock().unwrap();
        apply(&mut n, &block, &vdf);
        let params = n.chain.selected().params().clone();
        let owners = mmx_execution::chain::history::entries(&block, &params)
            .unwrap()
            .iter()
            .map(|r| hash(r, "address").unwrap())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let query = request("get_history", all_history(owners));
        n.purge_before(1).unwrap();
        assert_eq!(n.rpc_cache_stats().entries, 0);
        let expected = n.call(&query).unwrap();
        for row in expected.field("_ret_0").unwrap().seq().unwrap() {
            assert_eq!(number(row.record().unwrap(), "height").unwrap(), 1);
        }
        assert!(n
            .chain
            .selected()
            .storage()
            .database()
            .scan(mmx_execution::chain::history::TABLE, &[], None)
            .unwrap()
            .iter()
            .all(|row| !u32::from_be_bytes(row.key[32..36].try_into().unwrap()) >= 1));
        (params, query, expected)
    };
    drop(node);
    let chain = mmx_execution::chain::forks::ForkChain::open_native(dir.path(), params).unwrap();
    let n = Node::from_chain(
        chain,
        k256::ecdsa::SigningKey::from_slice(&[1; 32]).unwrap(),
    )
    .unwrap();
    assert_eq!(n.call(&query).unwrap(), expected);
}

#[test]
#[ignore = "bounded public HTTP JSON result timing; run with --ignored --nocapture"]
fn benchmark_cached_and_uncached_public_json() {
    let dir = tempfile::tempdir().unwrap();
    let (node, block, vdf) = fixture_at(Some(dir.path()));
    let mut n = node.lock().unwrap();
    apply(&mut n, &block, &vdf);
    let query = request("get_block_at", json!({"height":1}));
    let expected =
        vnx::serde_json::to_vec(&crate::rpc::response_json(&n.call(&query).unwrap()).unwrap())
            .unwrap();
    let start = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(
            vnx::serde_json::to_vec(&crate::rpc::response_json(&n.call(&query).unwrap()).unwrap())
                .unwrap(),
        );
    }
    let native = start.elapsed();
    let start = Instant::now();
    for _ in 0..1000 {
        let wire = probe(&n, &query).unwrap().0.unwrap();
        assert_eq!(wire, expected);
        std::hint::black_box(wire);
    }
    let cached = start.elapsed();
    eprintln!("1000 public JSON block results from a temporary ledger: uncached={native:?}, cached={cached:?}, payload={} bytes, charged={} bytes",expected.len(),n.rpc_cache_stats().used_bytes);
}

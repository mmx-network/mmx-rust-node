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
fn rest(node: &Arc<Mutex<Node>>, cache: &Shared, path: &str) -> crate::webapi::Response {
    crate::webapi::respond(
        node,
        Some(cache),
        "GET",
        path,
        &[],
        Instant::now() + Duration::from_secs(2),
    )
}
fn assert_rest_keys(cache: &Shared) {
    let cache = cache.lock().unwrap();
    assert!(!cache.entries.is_empty());
    for key in cache.entries.keys() {
        let key: Json = vnx::serde_json::from_slice(key).expect("REST cache key");
        assert_eq!(key[0], "WebAPI");
    }
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
        );
    }
    assert_eq!(cache.stats.used_bytes, cache.stats.limit_bytes);
    cache.get(&[1]);
    cache.get(&[1]);
    cache.insert(
        vec![4].into_boxed_slice(),
        vec![0; 10].into_boxed_slice(),
        Some(4),
    );
    assert!(cache.entries.contains_key([1].as_slice()));
    assert!(!cache.entries.contains_key([2].as_slice()));
    assert_eq!(cache.stats.evictions, 1);
    cache.insert(
        vec![99].into_boxed_slice(),
        vec![0; cache.stats.limit_bytes].into_boxed_slice(),
        None,
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
fn publications_only_seed_rendered_rest_responses_and_hits_bypass_the_ledger() {
    let (node, block, vdf) = fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    {
        let mut n = node.lock().unwrap();
        assert_eq!(n.rpc_cache_stats().limit_bytes, DEFAULT_BYTES);
        apply(&mut n, &block, &vdf);
    }
    web_ready_for_test(&cache);
    assert_rest_keys(&cache);
    let hash = vnx::util::to_hex_string(&hash(&block, "hash").unwrap(), false, false);
    let paths = [
        "/block?height=1".to_string(),
        format!("/block?hash={hash}"),
        "/header?height=1".to_string(),
    ];
    // The node mutex is held: these are final public responses with no internal
    // method calls, database queries or response conversion on a hit.
    let locked = node.lock().unwrap();
    for path in paths {
        let response = rest(&node, &cache, &path);
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let value: Json = vnx::serde_json::from_slice(&response.body).unwrap();
        assert_eq!(value["height"], 1);
        assert_eq!(value["hash"], hash);
    }
    drop(locked);
    assert_eq!(cache.lock().unwrap().stats.hits, 3);
}

#[test]
fn rest_misses_retain_only_the_final_response_not_internal_query_results() {
    let (node, _, _) = fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    web_ready_for_test(&cache);
    let block = rest(&node, &cache, "/block?height=0");
    assert_eq!(block.status, 200);
    let block: Json = vnx::serde_json::from_slice(&block.body).unwrap();
    let tx = block["tx_list"][0]["id"].as_str().unwrap();
    for path in [
        "/block?height=0".to_string(),
        format!("/transaction?id={tx}"),
    ] {
        node.lock().unwrap().set_rpc_cache_limit(DEFAULT_BYTES);
        let response = rest(&node, &cache, &path);
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        assert_rest_keys(&cache);
        let stats = cache.lock().unwrap().stats;
        assert_eq!(
            stats.entries, 1,
            "a REST miss must retain only its final response"
        );
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1, "internal queries must not probe the cache");
        let locked = node.lock().unwrap();
        let hit = rest(&node, &cache, &path);
        assert_eq!(hit.body, response.body);
        assert_eq!(hit.status, 200);
        drop(locked);
    }
}

#[test]
fn internal_json_rpc_and_vnx_calls_never_read_or_fill_the_rest_cache() {
    use std::io::{Read, Write};
    let (node, block, vdf) = fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    {
        let mut n = node.lock().unwrap();
        apply(&mut n, &block, &vdf);
    }
    web_ready_for_test(&cache);
    let tx = block.field("tx_list").unwrap().seq().unwrap()[0]
        .record()
        .unwrap();
    let txid = hash(tx, "id").unwrap();
    let methods = [
        ("get_block_at", json!({"height":1})),
        ("get_transaction", json!({"id":txid,"pending":false})),
        ("get_transactions", json!({"ids":[txid,txid]})),
        ("get_tx_height", json!({"id":txid})),
    ];
    let mut http = crate::rpc::HttpServer::bind("127.0.0.1:0", node.clone()).unwrap();
    let mut vnx = crate::rpc::vnx_server("127.0.0.1:0".parse().unwrap(), node.clone()).unwrap();
    let client = vnx::net::NetworkClient::connect(
        vnx.endpoint(),
        "Node",
        mmx_types::vnx_registry().unwrap(),
        Duration::from_secs(3),
    )
    .unwrap();
    let send = |value: Json| {
        let body = vnx::serde_json::to_vec(&value).unwrap();
        let mut socket = std::net::TcpStream::connect(http.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(socket, "POST /rpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        socket.write_all(&body).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        vnx::serde_json::from_str::<Json>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    // Check both a warm REST cache and a cold one. Neither native transport nor
    // the JSON-RPC presentation can serve or populate internal-method entries.
    for clear in [false, true] {
        if clear {
            node.lock().unwrap().set_rpc_cache_limit(DEFAULT_BYTES);
        }
        let before = cache.lock().unwrap().stats;
        for (method, args) in &methods {
            let expected = dispatch(&node, method, args.clone()).unwrap();
            for _ in 0..2 {
                let call =
                    json!({"jsonrpc":"2.0","id":"quoted\" id\n","method":method,"params":args});
                let first = send(call.clone());
                assert_eq!(first["result"], expected);
                assert_eq!(first["id"], call["id"]);
                let notification = json!({"jsonrpc":"2.0","method":method,"params":args});
                let batch = send(
                    json!([call, notification, {"jsonrpc":"2.0","id":2,"method":method,"params":args}]),
                );
                assert_eq!(batch.as_array().unwrap().len(), 2);
                assert_eq!(batch[1]["result"], expected);
                assert_eq!(
                    client
                        .call_value(&format!("mmx.Node.{method}"), args.clone())
                        .unwrap()["_ret_0"],
                    expected
                );
            }
        }
        assert_eq!(cache.lock().unwrap().stats, before);
    }
    drop(client);
    vnx.shutdown();
    http.shutdown();
}

#[test]
fn actual_fork_discards_old_public_blocks_and_state() {
    let (node, block, vdf) = fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    let mut n = node.lock().unwrap();
    apply(&mut n, &block, &vdf);
    web_ready_for_test(&cache);
    let old_hash = vnx::util::to_hex_string(&hash(&block, "hash").unwrap(), false, false);
    let old_key = crate::webapi::cache_key("/block", &[("hash".into(), old_hash)].into()).unwrap();
    assert!(cache
        .lock()
        .unwrap()
        .entries
        .contains_key(old_key.as_slice()));
    let state_key =
        crate::webapi::cache_key("/balance", &[("id".into(), "test".into())].into()).unwrap();
    web_probe(&cache, Some(&state_key))
        .fill
        .unwrap()
        .finish_json(b"[]");
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
    assert!(web_probe(&cache, Some(&old_key)).value.is_none());
    assert!(web_probe(&cache, Some(&state_key)).value.is_none());
    let key = crate::webapi::cache_key("/block", &[("height".into(), "1".into())].into()).unwrap();
    let value: Json =
        vnx::serde_json::from_slice(&web_probe(&cache, Some(&key)).value.unwrap()).unwrap();
    assert_eq!(
        value["hash"],
        vnx::util::to_hex_string(
            &hash(&raw(&steps[0]["block"]), "hash").unwrap(),
            false,
            false
        )
    );
}

#[test]
fn rollback_republication_and_reconfiguration_reject_inflight_rest_fills() {
    for disk in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (node, block, vdf) = fixture_at(disk.then_some(dir.path()));
        let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
        web_ready_for_test(&cache);
        let key =
            crate::webapi::cache_key("/balance", &[("id".into(), "test".into())].into()).unwrap();
        let mut n = node.lock().unwrap();
        let fill = web_probe(&cache, Some(&key)).fill.unwrap();
        apply(&mut n, &block, &vdf);
        fill.finish_json(b"[]");
        assert!(web_probe(&cache, Some(&key)).value.is_none());
        let fill = web_probe(&cache, Some(&key)).fill.unwrap();
        n.chain.rollback_to(0).unwrap();
        apply(&mut n, &block, &vdf); // Same tip hash, different publication epoch.
        fill.finish_json(b"[]");
        assert!(web_probe(&cache, Some(&key)).value.is_none());
        let fill = web_probe(&cache, Some(&key)).fill.unwrap();
        n.set_rpc_cache_limit(DEFAULT_BYTES);
        fill.finish_json(b"[]");
        assert_eq!(n.rpc_cache_stats().entries, 0);
        n.set_rpc_cache_limit(0);
        let probe = web_probe(&cache, Some(b"key"));
        assert!(probe.value.is_none() && probe.fill.is_none());
        assert_eq!(n.rpc_cache_stats().limit_bytes, 0);
        n.chain.rollback_to(0).unwrap();
        assert_eq!(n.rpc_cache_stats().entries, 0);
    }
}
#[test]
fn internal_pending_history_remains_current_without_retaining_responses() {
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
    let mut args = all_history(vec![sender]);
    args["filter"]["with_pending"] = json!(true);
    let before = dispatch(&node, "get_history", args.clone()).unwrap();
    let stats = node.lock().unwrap().rpc_cache_stats();
    {
        let mut n = node.lock().unwrap();
        n.synced = true;
        assert!(n.add_transaction(tx, true).unwrap());
    }
    let after = dispatch(&node, "get_history", args).unwrap();
    assert!(after.as_array().unwrap().len() > before.as_array().unwrap().len());
    assert_eq!(after[0]["is_pending"], true);
    assert_eq!(node.lock().unwrap().rpc_cache_stats(), stats);
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

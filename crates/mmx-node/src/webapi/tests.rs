use super::*;
use std::time::Duration;
fn get(node: &Arc<Mutex<Node>>, path: &str) -> Response {
    let cache = node.lock().unwrap().rpc_cache.clone();
    respond(
        node,
        cache.as_ref(),
        "GET",
        path,
        &[],
        Instant::now() + Duration::from_secs(2),
    )
}
#[test]
fn public_rest_sync_gate_native_rendering_and_database_free_cached_hits() {
    let (node, _, _) = crate::rpc::tests::fixture();
    assert_eq!(get(&node, "/wapi/node/info").status, 503);
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    rpc::cache::web_ready_for_test(&cache);
    let result = get(&node, "/wapi/block?height=0");
    assert_eq!(
        result.status,
        200,
        "{}",
        String::from_utf8_lossy(&result.body)
    );
    let block: Value = serde_json::from_slice(&result.body).unwrap();
    assert_eq!(block["height"], 0);
    assert!(block["hash"].is_string());
    assert!(block["project_addr"].as_str().unwrap().starts_with("mmx1"));
    assert!(block["total_weight"].is_string());
    assert!(block["tx_fees"]["amount"].is_string());
    let locked = node.lock().unwrap();
    let hit = respond(
        &node,
        Some(&cache),
        "GET",
        "/wapi/block?height=0",
        &[],
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(hit.status, 200);
    assert_eq!(hit.body, result.body);
    drop(locked);
    assert_eq!(get(&node, "/wapi/header?height=4294967295").status, 204);
    assert_eq!(get(&node, "/wapi/headers?limit=101").status, 400);
    assert_eq!(get(&node, "/wapi/wallet/seed").status, 403);
    assert_eq!(get(&node, "/wapi/address?id=invalid").status, 400);
    let info = get(&node, "/wapi/node/info");
    assert_eq!(info.status, 200, "{}", String::from_utf8_lossy(&info.body));
}
#[test]
fn declared_aliases_round_trip_public_transaction_json_without_changing_vm_arguments() {
    let params = mmx_types::mmx::ChainParams::default();
    let context = Context::new(params, 10);
    let input = json!({"__type":"mmx.txin_t","address":vec![1;32],"contract":vec![0;32],"amount":1234567u128.to_le_bytes().to_vec(),"memo":"a&b","solution":0});
    let output = context.render(&input).unwrap();
    assert_eq!(output["amount"], "1234567");
    assert_eq!(output["value"], 1.234567);
    assert_eq!(output["symbol"], "MMX");
    assert_eq!(
        output["contract"],
        "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev"
    );
    let back = render::native(&output, "mmx.txin_t", 0).unwrap();
    for key in ["address", "contract", "amount", "memo", "solution"] {
        assert_eq!(back[key], input[key], "{key}");
    }
    let args = json!({"args":["mmx1text","1234",{"user":"text"}]});
    assert_eq!(render::native(&args, "", 0).unwrap(), args);
}

#[test]
fn mainnet_rest_rendering_matches_captured_cpp_responses() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/webapi-mainnet.json")).unwrap();
    let native = &fixture["cases"][0]["native"];
    let params = mmx_types::mmx::ChainParams::decode(
        &rpc::from_json_record(native).unwrap().encode().unwrap(),
    )
    .unwrap();
    let context = Context::new(params, 1);
    for case in fixture["cases"].as_array().unwrap().iter().take(3) {
        let rendered = context.render(&case["native"]).unwrap();
        assert_eq!(rendered, case["public"], "{}", case["path"]);
    }
    let case = &fixture["cases"][3];
    assert_eq!(
        context.render(&case["native"]["deploy"]).unwrap(),
        case["public"]["deployed"]
    );
    let mut context = context.clone();
    context.height = case["public"]["confirm"].as_u64().unwrap() as u32 - 1;
    let block = json!({"hash":vnx::util::from_hex_string(case["public"]["block"].as_str().unwrap()).unwrap(),"time_stamp":case["public"]["time_stamp"]});
    let value = transaction_value(
        &context,
        &case["native"],
        &case["native"]["id"],
        &json!(0),
        &block,
    )
    .unwrap();
    for (key, expected) in case["public"].as_object().unwrap() {
        assert_eq!(&value[key], expected, "{key}");
    }
    assert_eq!(
        value.as_object().unwrap().len(),
        case["public"].as_object().unwrap().len()
    );
}

#[test]
fn existing_public_routes_cover_empty_markets_farmer_queries_balances_and_transactions() {
    let (node, _, _) = crate::rpc::tests::fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    rpc::cache::web_ready_for_test(&cache);
    let zero = "mmx1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdgytev";
    for path in ["/offers", "/trade_history", "/swap/list", "/farmers"] {
        let result = get(&node, path);
        assert_eq!(
            result.status,
            200,
            "{path}: {}",
            String::from_utf8_lossy(&result.body)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&result.body).unwrap(),
            json!([])
        );
    }
    let key = format!("02{}", "01".repeat(32));
    let result = get(&node, &format!("/farmer?id={key}"));
    assert_eq!(
        result.status,
        200,
        "{}",
        String::from_utf8_lossy(&result.body)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.body).unwrap()["block_count"],
        0
    );
    for path in [
        format!("/farmer/blocks?id={key}"),
        format!("/address/history?id={zero}"),
    ] {
        assert_eq!(get(&node, &path).status, 200);
    }
    let contract = get(&node, &format!("/contract/storage?id={zero}"));
    assert_eq!(contract.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&contract.body).unwrap(),
        json!({})
    );
    assert_eq!(get(&node, &format!("/balance?id={zero}")).status, 200);
    assert_eq!(get(&node, &format!("/plotnft?id={zero}")).status, 400);
    assert_eq!(
        get(&node, &format!("/contract/call?id={zero}&method=missing")).status,
        400
    );
    let block = get(&node, "/block?height=0");
    let block: Value = serde_json::from_slice(&block.body).unwrap();
    let txid = block["tx_list"][0]["id"].as_str().unwrap();
    let result = get(&node, &format!("/transaction?id={txid}"));
    assert_eq!(
        result.status,
        200,
        "{}",
        String::from_utf8_lossy(&result.body)
    );
    let tx: Value = serde_json::from_slice(&result.body).unwrap();
    assert_eq!(tx["id"], txid);
    assert_eq!(tx["confirm"], 1);
    assert!(tx["deployed"].is_object());
    let list = get(&node, "/transactions?height=0&limit=1");
    assert_eq!(list.status, 200, "{}", String::from_utf8_lossy(&list.body));
    assert_eq!(serde_json::from_slice::<Value>(&list.body).unwrap()[0], tx);
}

#[test]
fn accepted_blocks_push_public_history_and_blocks_and_rollback_removes_them() {
    let (node, block, vdf) = crate::rpc::tests::fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    rpc::cache::web_ready_for_test(&cache);
    let params = node.lock().unwrap().chain.selected().params().clone();
    let rows = mmx_execution::chain::history::entries(&block, &params).unwrap();
    let owner = crate::node::hash(&rows[0], "address").unwrap();
    let path = format!(
        "/address/history?id={}&limit=100",
        mmx_vm::address::encode(&owner)
    );
    let initial = get(&node, &path);
    assert_eq!(
        initial.status,
        200,
        "{}",
        String::from_utf8_lossy(&initial.body)
    );
    let before: Value = serde_json::from_slice(&initial.body).unwrap();
    {
        let mut n = node.lock().unwrap();
        n.chain.admit(&block, &[vdf]).unwrap();
        let root = n.chain.selected().hash_at(0).unwrap().unwrap();
        n.chain.update(root, None).unwrap();
        n.peer_state().publish(1, false);
    }
    let locked = node.lock().unwrap();
    let response = respond(
        &node,
        Some(&cache),
        "GET",
        &path,
        &[],
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let after: Value = serde_json::from_slice(&response.body).unwrap();
    assert!(after.as_array().unwrap().len() > before.as_array().unwrap().len());
    assert!(after[0]["address"].is_string());
    assert!(after[0]["amount"].is_string());
    assert!(after[0]["type"].is_string());
    assert!(after[0]["is_native"].is_boolean());
    let block = respond(
        &node,
        Some(&cache),
        "GET",
        "/block?height=1",
        &[],
        Instant::now() + Duration::from_millis(100),
    );
    assert_eq!(
        block.status,
        200,
        "{}",
        String::from_utf8_lossy(&block.body)
    );
    drop(locked);
    {
        let mut n = node.lock().unwrap();
        n.rollback_to(0).unwrap();
        let size = n.rpc_cache_stats().limit_bytes;
        n.set_rpc_cache_limit(size);
        rpc::cache::web_ready_for_test(&cache);
    }
    let block = get(&node, "/block?height=1");
    assert_eq!(block.status, 204);
    let restored = get(&node, &path);
    assert_eq!(restored.status, 200);
    assert_eq!(restored.body, initial.body);
    let stats = node.lock().unwrap().rpc_cache_stats();
    assert!(stats.used_bytes <= stats.limit_bytes);
}

#[test]
fn contract_calls_use_pinned_state_preserve_results_and_never_publish_vm_writes() {
    use mmx_vm::{
        storage::{AtomicStorage, ContractData, StorageWrite, TransactionBatch},
        Storage, StorageDb, Var,
    };
    let row: Value = serde_json::from_str(
        include_str!("../../../mmx-execution/tests/fixtures/nft.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let snapshot = &row["snapshot"];
    let raw = |value: &Value| vnx::util::from_hex_string(value.as_str().unwrap()).unwrap();
    let address = |value: &Value| -> [u8; 32] { raw(value).try_into().unwrap() };
    let mut db = StorageDb::memory();
    let mut batch = TransactionBatch::default();
    for contract in snapshot["contracts"].as_array().unwrap() {
        batch.contracts.push((
            address(&contract["address"]),
            ContractData {
                bytes: raw(&contract["wire"]),
                read_cost: 0,
            },
        ));
    }
    for cell in snapshot["cells"].as_array().unwrap() {
        batch.writes.push((
            address(&cell["contract"]),
            StorageWrite {
                address: cell["address"].as_u64().unwrap(),
                key: cell["key"].as_u64(),
                var: Var::decode(&raw(&cell["value"]), false, true).unwrap().0,
            },
        ));
    }
    db.apply_transaction(&batch).unwrap();
    db.database_mut().commit(1).unwrap();
    let params = mmx_types::mmx::ChainParams::decode(&raw(&snapshot["params"])).unwrap();
    let (node, _, _) = crate::rpc::tests::fixture();
    let request = Request {
        node: &node,
        cache: None,
        end: Instant::now() + Duration::from_secs(2),
        query: BTreeMap::new(),
        context: Context::new(params, 8),
        watch: None,
        bytes: Cell::new(0),
    };
    let mut view = storage::View::fixture(db, 8);
    let result = view
        .call(
            &request,
            address(&row["contract"]),
            "mmx_reward_target",
            &[Value::Null],
            None,
            None,
        )
        .unwrap();
    let target = mmx_vm::address::decode(result.as_str().unwrap().as_bytes()).unwrap();
    assert_eq!(target, address(&row["target"]));
    assert!(!view.storage.database().has_pending());
    for (id, write) in &batch.writes {
        assert_eq!(
            view.storage
                .read(id, write.address, write.key)
                .unwrap()
                .unwrap()
                .value,
            write.var.value
        );
    }
}

#[test]
fn http_rest_get_head_options_errors_and_json_rpc_share_the_listener() {
    use std::{
        io::{Read, Write},
        net::TcpStream,
    };
    let (node, _, _) = crate::rpc::tests::fixture();
    let cache = node.lock().unwrap().rpc_cache.clone().unwrap();
    rpc::cache::web_ready_for_test(&cache);
    let server = crate::http::HttpServer::bind("127.0.0.1:0", node).unwrap();
    let send = |method: &str, path: &str, body: &str| {
        let mut socket = TcpStream::connect(server.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut result = String::new();
        socket.read_to_string(&mut result).unwrap();
        result
    };
    let get = send("GET", "/wapi/block?height=0", "");
    assert!(get.starts_with("HTTP/1.1 200"));
    assert!(get.contains("Access-Control-Allow-Origin: *"));
    let head = send("HEAD", "/wapi/block?height=0", "");
    assert_eq!(
        head,
        get.split_once("\r\n\r\n").unwrap().0.to_owned() + "\r\n\r\n"
    );
    assert!(send("OPTIONS", "/wapi/transaction/broadcast", "").starts_with("HTTP/1.1 200"));
    assert!(send("GET", "/wapi/unknown", "").starts_with("HTTP/1.1 404"));
    assert!(send("GET", "/wapi/header?height=4294967295", "").starts_with("HTTP/1.1 204"));
    assert!(send("POST", "/wapi/transaction/validate", "{").starts_with("HTTP/1.1 400"));
    assert!(send("POST", "/wapi/transaction/broadcast", "{}").starts_with("HTTP/1.1 400"));
    let status = send(
        "POST",
        "/rpc",
        r#"{"jsonrpc":"2.0","id":1,"method":"get_status","params":{}}"#,
    );
    assert!(status.starts_with("HTTP/1.1 200"));
    let status: Value = serde_json::from_str(status.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        status["result"]["rpc_cache"]["limit_bytes"],
        64 * 1024 * 1024
    );
}

#[test]
fn pushed_public_transactions_use_final_batch_tip_and_can_be_served_with_ledger_locked() {
    let (node, _, _) = crate::rpc::tests::fixture();
    let n = node.lock().unwrap();
    let params = n.chain.selected().params().clone();
    let hash = n.chain.selected().hash_at(0).unwrap().unwrap();
    let block = n.chain.selected().block(&hash).unwrap().unwrap();
    let cache = n.rpc_cache.clone().unwrap();
    drop(n);
    rpc::cache::web_ready_for_test(&cache);
    let entries = pushed(&block, &params, 12).unwrap();
    let mut count = 0;
    for entry in entries {
        let key: Value = serde_json::from_slice(&entry.key).unwrap();
        if key[1] != "/transaction" {
            continue;
        }
        let value: Value = serde_json::from_slice(&entry.value).unwrap();
        assert_eq!(value["confirm"], 13);
        rpc::cache::web_probe(&cache, Some(&entry.key))
            .fill
            .unwrap()
            .finish_json(&entry.value);
        let path = format!("/transaction?id={}", value["id"].as_str().unwrap());
        let locked = node.lock().unwrap();
        let response = respond(
            &node,
            Some(&cache),
            "GET",
            &path,
            &[],
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.body, entry.value);
        drop(locked);
        count += 1;
    }
    assert!(count > 0);
}

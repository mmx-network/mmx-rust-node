//! Compare public reads with a native C++ node. Immutable results work during
//! sync; optional mutable-state checks require matching stable, synced tips.
use mmx_db::Store;
use mmx_node::Result;
use std::{collections::BTreeSet, path::Path, time::Duration};
use vnx::{
    net::NetworkClient,
    serde_json::{json, Value},
};

fn difference(left: &Value, right: &Value, path: &str) -> Option<String> {
    if left == right {
        return None;
    }
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
                let next = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(path) = difference(a, b, &next) {
                            return Some(path);
                        }
                    }
                    _ => return Some(next),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                if let Some(path) = difference(a, b, &format!("{path}/{index}")) {
                    return Some(path);
                }
            }
        }
        _ => (),
    }
    Some(path.to_owned())
}

fn require_state_tip(height: &Value, synced: &Value, expected: u32) -> Result<()> {
    if height.as_u64() != Some(u64::from(expected)) || synced != height {
        return Err(
            "mutable state audit requires both nodes synced at the pinned height; retry".into(),
        );
    }
    Ok(())
}

// Export while the Rust writer is stopped, then audit the immutable manifest
// against both running servers at the same synced tip.
fn export_state(source: &Path, output: &Path, samples: usize) -> Result<()> {
    let store = Store::open_native(source.join("chain/selected"))?;
    let height = store
        .version()
        .checked_sub(1)
        .ok_or("empty selected database")?;
    let hash = store
        .get("chain_height", &height.to_be_bytes(), None)?
        .ok_or("missing selected hash")?;
    if hash.len() != 32 {
        return Err("invalid selected hash".into());
    }
    let mut snapshot = json!({"format_version":1,"height":height,"hash":hash});
    for (table, size) in [("balances", 64), ("contracts", 32)] {
        let rows = store.scan(table, &[], None)?;
        if rows.is_empty() {
            return Err(format!("no {table} keys available for mutable state comparison").into());
        }
        let count = samples.min(rows.len());
        let mut selected = Vec::new();
        for i in 0..count {
            let row = &rows[if count > 1 {
                i * (rows.len() - 1) / (count - 1)
            } else {
                0
            }];
            if row.key.len() != size {
                return Err(format!("invalid {table} key length").into());
            }
            selected.push(if table == "balances" {
                json!({"key":row.key,"value":row.value})
            } else {
                json!(row.key)
            });
        }
        snapshot[table] = json!(selected);
    }
    std::fs::write(output, vnx::serde_json::to_vec_pretty(&snapshot)?)?;
    Ok(())
}

struct Audit {
    native: NetworkClient,
    rust: NetworkClient,
    comparisons: usize,
    methods: BTreeSet<String>,
}
impl Audit {
    fn check(&mut self, method: &str, args: Value) -> Result<Value> {
        let name = format!("mmx.Node.{method}");
        let native = self
            .native
            .call_value(&name, args.clone())
            .map_err(|e| format!("native {method} failed: {e}"))?;
        let rust = self
            .rust
            .call_value(&name, args.clone())
            .map_err(|e| format!("Rust {method} failed: {e}"))?;
        if let Some(path) = difference(&native, &rust, "") {
            return Err(format!("native RPC differs: {method}, args={args}, path={path}").into());
        }
        self.comparisons += 1;
        self.methods.insert(method.to_owned());
        Ok(native["_ret_0"].clone())
    }
    fn height(&mut self, height: u32) -> Result<()> {
        let hash = self.check("get_block_hash", json!({"height":height}))?;
        if !hash.is_array() {
            return Err(format!("missing canonical block at height {height}").into());
        }
        self.check("get_block_hash_ex", json!({"height":height}))?;
        let block = self.check("get_block_at", json!({"height":height}))?;
        self.check("get_block", json!({"hash":hash}))?;
        self.check("get_header_at", json!({"height":height}))?;
        self.check("get_header", json!({"hash":hash}))?;
        let ids = self.check("get_tx_ids_at", json!({"height":height}))?;
        let ids = ids.as_array().ok_or("invalid transaction ID result")?;
        let transactions = block["tx_list"].as_array().ok_or("invalid block result")?;
        if ids.len() != transactions.len()
            || ids.iter().zip(transactions).any(|(id, tx)| id != &tx["id"])
        {
            return Err(format!("native transaction IDs disagree with block at {height}").into());
        }
        for id in ids {
            self.check("get_transaction", json!({"id":id,"pending":false}))?;
            let actual = self.check("get_tx_height", json!({"id":id}))?;
            if actual != json!(height) {
                return Err(format!("wrong transaction height at {height}").into());
            }
        }
        // Duplicates and missing IDs must preserve the caller's vector order.
        let mut batch = ids.clone();
        if let Some(first) = ids.first() {
            batch.push(first.clone());
        }
        batch.push(json!([255u8; 32].to_vec()));
        self.check("get_transactions", json!({"ids":batch}))?;
        if self.check("get_block_hash", json!({"height":height}))? != hash {
            return Err(format!("canonical block changed during audit at {height}; retry").into());
        }
        println!(
            "{}",
            json!({"phase":"height_verified","height":height,"transactions":ids.len(),"canonical_hash":hash})
        );
        Ok(())
    }

    fn state_tip(&mut self, expected: u32) -> Result<Value> {
        let height = self
            .check("get_height", json!({}))
            .map_err(|e| format!("mutable audit tip check failed: {e}; retry"))?;
        let synced = self
            .check("get_synced_height", json!({}))
            .map_err(|e| format!("mutable audit sync check failed: {e}; retry"))?;
        require_state_tip(&height, &synced, expected)?;
        self.check("get_block_hash", json!({"height":expected}))
    }

    fn state(&mut self, source: &Path, samples: usize) -> Result<()> {
        let snapshot: Value = vnx::serde_json::from_slice(&std::fs::read(source)?)?;
        if snapshot["format_version"] != json!(1) {
            return Err("unsupported state audit manifest".into());
        }
        let height = u32::try_from(snapshot["height"].as_u64().ok_or("invalid audit height")?)?;
        let expected: Vec<u8> = vnx::serde_json::from_value(snapshot["hash"].clone())?;
        if expected.len() != 32 || self.state_tip(height)? != json!(expected) {
            return Err(
                "mutable state audit source differs from the servers' canonical tip".into(),
            );
        }
        let mut balance_count = 0;
        let mut contract_count = 0;
        let checked = (|| -> Result<()> {
            for (table, size) in [("balances", 64), ("contracts", 32)] {
                let rows = snapshot[table]
                    .as_array()
                    .ok_or("invalid state audit table")?;
                if rows.is_empty() {
                    return Err(
                        format!("no {table} keys available for mutable state comparison").into(),
                    );
                }
                let count = samples.min(rows.len());
                for i in 0..count {
                    let row = &rows[if count > 1 {
                        i * (rows.len() - 1) / (count - 1)
                    } else {
                        0
                    }];
                    let key: Vec<u8> = vnx::serde_json::from_value(if table == "balances" {
                        row["key"].clone()
                    } else {
                        row.clone()
                    })?;
                    if key.len() != size {
                        return Err(format!("invalid {table} key length").into());
                    }
                    if table == "balances" {
                        let value: Vec<u8> = vnx::serde_json::from_value(row["value"].clone())?;
                        if value.len() != 16 {
                            return Err("invalid stored balance length".into());
                        }
                        let amount = self.check(
                            "get_balance",
                            json!({"address":key[..32],"currency":key[32..]}),
                        )?;
                        if amount != json!(value) {
                            return Err(format!(
                                "RPC balance differs from pinned ledger for key {key:?}"
                            )
                            .into());
                        }
                        balance_count += 1;
                    } else {
                        self.check("get_contract", json!({"address":key}))?;
                        self.check("get_contract_for", json!({"address":key}))?;
                        contract_count += 1;
                    }
                }
            }
            Ok(())
        })();
        if self.state_tip(height)? != json!(expected) {
            return Err("canonical tip changed during mutable state comparison; retry".into());
        }
        checked?;
        println!(
            "{}",
            json!({"phase":"mutable_state_verified","height":height,
            "balance_keys_checked":balance_count,"contracts_checked":contract_count,
            "sample_limit_per_table":samples,"native_values_match":true})
        );
        Ok(())
    }
}
fn main() -> Result<()> {
    let mut native = "127.0.0.1:11330".to_owned();
    let mut rust = "127.0.0.1:12331".to_owned();
    let mut heights = BTreeSet::new();
    let mut state_source = None;
    let mut export_source = None;
    let mut output = None;
    let mut samples = 128usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing audit argument")?;
        match arg.as_str() {
            "--native-rpc" => native = value,
            "--rust-rpc" => rust = value,
            "--height" => {
                heights.insert(value.parse::<u32>()?);
            }
            "--export-state" => export_source = Some(std::path::PathBuf::from(value)),
            "--output" => output = Some(std::path::PathBuf::from(value)),
            "--state-from" => state_source = Some(std::path::PathBuf::from(value)),
            "--samples" => samples = value.parse()?,
            _ => return Err(format!("unknown audit argument: {arg}").into()),
        }
    }
    if samples == 0 {
        return Err("samples must be positive".into());
    }
    if let Some(source) = export_source {
        return export_state(&source, &output.ok_or("missing --output")?, samples);
    }
    if output.is_some() {
        return Err("--output requires --export-state".into());
    }
    let connect = |endpoint: &str| -> Result<NetworkClient> {
        NetworkClient::connect(
            &endpoint.parse()?,
            "Node",
            mmx_types::vnx_registry()?,
            Duration::from_secs(30),
        )
        .map_err(Into::into)
    };
    let mut audit = Audit {
        native: connect(&native)?,
        rust: connect(&rust)?,
        comparisons: 0,
        methods: BTreeSet::new(),
    };
    audit.check("get_genesis_hash", json!({}))?;
    audit.check("get_params", json!({}))?;
    let mutable_checked = if let Some(source) = state_source {
        audit.state(&source, samples)?;
        true
    } else {
        false
    };
    if heights.is_empty() {
        heights.insert(0);
        let mut common = u32::MAX;
        for client in [&audit.native, &audit.rust] {
            let height = client.call_value("mmx.Node.get_height", json!({}))?["_ret_0"]
                .as_u64()
                .ok_or("invalid node height")?;
            common = common.min(u32::try_from(height)?);
        }
        // The lower tip is the highest common fixed point during historical sync.
        heights.insert(common.saturating_sub(24));
    }
    let checked = heights.len();
    for height in heights {
        audit.height(height)?;
    }
    let missing = json!([255u8; 32].to_vec());
    audit.check("get_block", json!({"hash":missing}))?;
    audit.check("get_header", json!({"hash":missing}))?;
    audit.check("get_transaction", json!({"id":missing,"pending":false}))?;
    audit.check("get_tx_height", json!({"id":missing}))?;
    println!(
        "{}",
        json!({"phase":"complete","heights_checked":checked,"comparisons":audit.comparisons,"methods":audit.methods,"native_results_match":true,"mutable_state_checked":mutable_checked})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    };
    use vnx::{
        net::{Server, ServerOptions},
        runtime::{Methods, Runtime, Session},
    };

    #[test]
    fn comparison_preserves_order_and_missing_fields_and_reports_the_changed_path() {
        let a = json!({"result":[null,{"a/b~c":1}]} );
        assert_eq!(difference(&a, &a, ""), None);
        assert_eq!(
            difference(&a, &json!({"result":[null,{"a/b~c":2}]}), ""),
            Some("/result/1/a~1b~0c".into())
        );
        assert_eq!(
            difference(&json!({"a":null}), &json!({}), ""),
            Some("/a".into())
        );
        assert_eq!(
            difference(&json!([1, 2]), &json!([2, 1]), ""),
            Some("/0".into())
        );
    }

    #[test]
    fn mutable_audit_rejects_historical_unsynced_and_moving_tips() {
        assert!(require_state_tip(&json!(7), &json!(7), 7).is_ok());
        for (height, synced) in [
            (json!(7), Value::Null),
            (json!(7), json!(6)),
            (json!(8), json!(8)),
            (json!(-1), json!(-1)),
        ] {
            assert!(require_state_tip(&height, &synced, 7).is_err());
        }
    }

    #[test]
    fn mutable_network_audit_checks_the_pinned_ledger_and_rejects_tip_movement() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let selected = dir.path().join("chain/selected");
        std::fs::create_dir_all(&selected)?;
        let mut store = Store::open_native(&selected)?;
        store.commit(7)?;
        let mut zero = [1u8; 64];
        zero[..32].fill(3);
        store.commit_batch(
            8,
            [
                mmx_db::Write::put("chain_height", 7u32.to_be_bytes(), [42u8; 32]),
                mmx_db::Write::put("balances", [1u8; 64], 17u128.to_le_bytes()),
                mmx_db::Write::put("balances", zero, 0u128.to_le_bytes()),
                mmx_db::Write::put("contracts", [2u8; 32], [0]),
            ],
        )?;
        let snapshot = dir.path().join("state-audit.json");
        assert!(export_state(dir.path(), &snapshot, 128).is_err());
        drop(store);
        export_state(dir.path(), &snapshot, 128)?;
        let tip = Arc::new(AtomicU32::new(7));
        let advance = Arc::new(AtomicBool::new(false));
        let amounts = [Arc::new(AtomicU64::new(17)), Arc::new(AtomicU64::new(17))];
        let mut peers = Vec::new();
        for amount in &amounts {
            let runtime = Runtime::new()?;
            let mut methods = Methods::new();
            for name in [
                "get_height",
                "get_synced_height",
                "get_block_hash",
                "get_balance",
                "get_contract",
                "get_contract_for",
            ] {
                let tip = tip.clone();
                let advance = advance.clone();
                let amount = amount.clone();
                methods.add(format!("mmx.Node.{name}"), None, move |args, _| {
                    Ok(match name {
                        "get_height" | "get_synced_height" => json!(tip.load(Ordering::SeqCst)),
                        "get_block_hash" => json!([42u8; 32].to_vec()),
                        "get_balance" => {
                            if advance.load(Ordering::SeqCst) {
                                tip.store(8, Ordering::SeqCst);
                            }
                            let amount = if args["address"] == json!([3u8; 32].to_vec()) {
                                0
                            } else {
                                amount.load(Ordering::SeqCst)
                            };
                            json!(u128::from(amount).to_le_bytes().to_vec())
                        }
                        "get_contract_for" => {
                            json!({"__type":"mmx.contract.PubKey","address":args["address"]})
                        }
                        _ => Value::Null,
                    })
                });
            }
            runtime.register("Node", methods)?;
            let server = Server::bind(
                "127.0.0.1:0".parse()?,
                runtime.clone(),
                mmx_types::vnx_registry()?,
                ServerOptions {
                    services: ["Node".into()].into(),
                    session: Session::internal(),
                    ..Default::default()
                },
            )?;
            peers.push((server, runtime));
        }
        let connect = |index: usize| -> Result<NetworkClient> {
            Ok(NetworkClient::connect(
                peers[index].0.endpoint(),
                "Node",
                mmx_types::vnx_registry()?,
                Duration::from_secs(5),
            )?)
        };
        let mut audit = Audit {
            native: connect(0)?,
            rust: connect(1)?,
            comparisons: 0,
            methods: BTreeSet::new(),
        };
        audit.state(&snapshot, 128)?;
        amounts[1].store(18, Ordering::SeqCst);
        assert!(audit
            .state(&snapshot, 128)
            .unwrap_err()
            .to_string()
            .contains("native RPC differs"));
        amounts[0].store(18, Ordering::SeqCst);
        assert!(audit
            .state(&snapshot, 128)
            .unwrap_err()
            .to_string()
            .contains("pinned ledger"));
        for amount in &amounts {
            amount.store(17, Ordering::SeqCst);
        }
        advance.store(true, Ordering::SeqCst);
        assert!(audit
            .state(&snapshot, 128)
            .unwrap_err()
            .to_string()
            .contains("pinned height"));
        audit.native.close();
        audit.rust.close();
        for (server, runtime) in &mut peers {
            server.shutdown();
            runtime.shutdown();
        }
        Ok(())
    }
}

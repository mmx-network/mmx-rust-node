//! Compare immutable public RPC results with a native C++ node while syncing.
//! Both servers are read-only; fixed-height hashes are rechecked after each group.
use mmx_node::Result;
use std::{collections::BTreeSet, time::Duration};
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
}
fn main() -> Result<()> {
    let mut native = "127.0.0.1:11330".to_owned();
    let mut rust = "127.0.0.1:12331".to_owned();
    let mut heights = BTreeSet::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing audit argument")?;
        match arg.as_str() {
            "--native-rpc" => native = value,
            "--rust-rpc" => rust = value,
            "--height" => {
                heights.insert(value.parse::<u32>()?);
            }
            _ => return Err(format!("unknown audit argument: {arg}").into()),
        }
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
        json!({"phase":"complete","heights_checked":checked,"comparisons":audit.comparisons,"methods":audit.methods,"native_results_match":true,"mutable_state_checked":false})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

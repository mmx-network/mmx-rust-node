//! Compare native DB snapshots with the C++ historical storage API.
//! Stop the Rust writer first. Values omit VM flags/reference counts.
use mmx_consensus::Hash;
use mmx_db::Store;
use mmx_node::Result;
use mmx_vm::{Value, Var};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Duration, Instant},
};
use vnx::{
    net::NetworkClient,
    serde_json::{json, Value as Json},
};

fn bytes(value: &Json) -> Result<Vec<u8>> {
    value
        .as_array()
        .ok_or_else(|| "expected native byte array".into())
        .and_then(|items| {
            items
                .iter()
                .map(|item| Ok(u8::try_from(item.as_u64().ok_or("invalid native byte")?)?))
                .collect()
        })
}
fn check_hash(client: &NetworkClient, store: &Store, height: u32) -> Result<Hash> {
    let expected = store
        .get("chain_height", &height.to_be_bytes(), None)?
        .ok_or("missing selected hash")?;
    let native = client.call_value("mmx.Node.get_block_hash", json!({"height":height}))?;
    if bytes(&native["_ret_0"])? != expected {
        return Err(format!("native canonical hash differs at {height}").into());
    }
    Ok(expected.try_into().map_err(|_| "invalid selected hash")?)
}
fn expected(store: &Store, table: &str, key: &[u8], height: u32) -> Result<Vec<u8>> {
    let value = store.get(table, key, Some(height))?;
    let var = if let Some(value) = value {
        let (var, count) = Var::decode(&value, false, true)?;
        if count != value.len() {
            return Err("trailing stored VM value bytes".into());
        }
        var
    } else {
        Var::new(Value::Nil)
    };
    Ok(var.encode(false, false)?)
}
fn main() -> Result<()> {
    let mut source = None;
    let mut endpoint = "127.0.0.1:11330".to_owned();
    let mut samples = 128usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().ok_or("missing audit argument")?;
        match arg.as_str() {
            "--source-dir" => source = Some(PathBuf::from(value)),
            "--native-rpc" => endpoint = value,
            "--samples" => samples = value.parse()?,
            _ => return Err(format!("unknown audit argument: {arg}").into()),
        }
    }
    if samples == 0 {
        return Err("samples must be positive".into());
    }
    let store = Store::open_native(source.ok_or("missing --source-dir")?.join("chain/selected"))?;
    let tip = store
        .version()
        .checked_sub(1)
        .ok_or("empty selected database")?;
    let client = NetworkClient::connect(
        &endpoint.parse()?,
        "Node",
        mmx_types::vnx_registry()?,
        Duration::from_secs(30),
    )?;
    check_hash(&client, &store, 0)?;
    let canonical = check_hash(&client, &store, tip)?;
    let mut contracts = BTreeMap::<Vec<u8>, Hash>::new();
    for row in store.scan("contracts", &[], None)? {
        let address: Hash = row.key.try_into().map_err(|_| "invalid contract key")?;
        if contracts
            .insert(address[..20].to_vec(), address)
            .is_some_and(|previous| previous != address)
        {
            return Err("ambiguous contract storage prefix".into());
        }
    }
    let started = Instant::now();
    let mut total = 0usize;
    let mut all_contracts = BTreeSet::new();
    for (table, key_size, method) in [
        ("storage", 28, "mmx.Node.read_storage_var"),
        ("storage_entries", 36, "mmx.Node.read_storage_entry_var"),
    ] {
        let keys = store
            .scan(table, &[], None)?
            .into_iter()
            .map(|row| (row.key, row.version))
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Err(format!("no {table} keys available for native comparison").into());
        }
        let count = samples.min(keys.len());
        let mut checked = 0usize;
        for i in 0..count {
            let index = if count > 1 {
                i * (keys.len() - 1) / (count - 1)
            } else {
                0
            };
            let (key, version) = &keys[index];
            if key.len() != key_size {
                return Err(format!("invalid {table} key length").into());
            }
            let contract = contracts
                .get(&key[..20])
                .ok_or("storage key has no known contract")?;
            all_contracts.insert(*contract);
            let address = u64::from_be_bytes(key[20..28].try_into()?);
            let heights = BTreeSet::from([version.saturating_sub(1), *version, tip]);
            for height in heights {
                let mut request = json!({"contract":contract,"address":address,"height":height});
                if key_size == 36 {
                    request["key"] = json!(u64::from_be_bytes(key[28..].try_into()?));
                }
                let result = client.call_value(method, request)?;
                if bytes(&result["_ret_0"])? != expected(&store, table, key, height)? {
                    return Err(format!(
                        "native VM state differs: {table}, contract {}, cell {address}, height {height}",
                        mmx_vm::address::encode(contract)
                    )
                    .into());
                }
                checked += 1;
            }
        }
        total += checked;
        println!(
            "{}",
            json!({"phase":"table_verified","table":table,"available_keys":keys.len(),"keys_checked":count,"all_keys_checked":count==keys.len(),"historical_reads":checked})
        );
    }
    check_hash(&client, &store, tip)?;
    println!(
        "{}",
        json!({"phase":"complete","height":tip,"canonical_hash":vnx::util::to_hex_string(&canonical,false,false),"contracts_checked":all_contracts.len(),"historical_reads":total,"native_values_match":true,"seconds":started.elapsed().as_secs_f64()})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_values_include_the_requested_height_and_preserve_tombstones() {
        let mut store = Store::memory();
        for (height, number) in [(3, 1u64), (7, 2)] {
            store.commit(height).unwrap();
            let mut var = Var::new(number);
            var.flags = mmx_vm::value::FLAG_STORED;
            store
                .commit_batch(
                    height + 1,
                    [mmx_db::Write::put(
                        "storage",
                        b"a",
                        var.encode(false, true).unwrap(),
                    )],
                )
                .unwrap();
        }
        store.commit(10).unwrap();
        store
            .commit_batch(11, [mmx_db::Write::delete("storage", b"a")])
            .unwrap();
        for (height, number) in [
            (2, None),
            (3, Some(1u64)),
            (6, Some(1)),
            (7, Some(2)),
            (10, None),
        ] {
            let value = number.map_or(Value::Nil, |n| Value::Uint(n.into()));
            assert_eq!(
                expected(&store, "storage", b"a", height).unwrap(),
                Var::new(value).encode(false, false).unwrap()
            );
        }
        let mut corrupt = Store::memory();
        corrupt.commit(3).unwrap();
        corrupt
            .commit_batch(4, [mmx_db::Write::put("storage", b"a", [0, 0, 0])])
            .unwrap();
        assert!(expected(&corrupt, "storage", b"a", 3).is_err());
    }
}

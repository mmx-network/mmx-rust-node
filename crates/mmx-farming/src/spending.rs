//! Wallet spending bookkeeping. Balances are read fresh from a node; only
//! pending debits and explicit reservations are retained locally.
use crate::{
    error,
    service::{from_json, to_json},
    wallet::Account,
    Result,
};
use mmx_consensus::{Record, Value as N};
use mmx_types::mmx;
use mmx_wallet::{Address, Balances};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use vnx::{
    net::NetworkClient,
    runtime::Methods,
    serde_json::{self, json, Value},
};

pub trait WalletNode: Send + Sync {
    fn call(&self, method: &str, args: Value) -> Result<Value>;
}
impl WalletNode for NetworkClient {
    fn call(&self, method: &str, args: Value) -> Result<Value> {
        Ok(self.call_value(&format!("mmx.Node.{method}"), args)?["_ret_0"].clone())
    }
}
#[derive(Default)]
pub struct Spending {
    reserved: Balances,
    pending: BTreeMap<[u8; 32], Pending>,
}
struct Pending {
    expires: u32,
    debits: Balances,
}
fn bytes<const K: usize>(v: &Value) -> Result<[u8; K]> {
    Ok(serde_json::from_value::<vnx::Fixed<u8, K>>(v.clone())?.0)
}
fn addr(v: &N) -> Result<Address> {
    Ok(Address(v.bytes()?.try_into()?))
}
fn atomic(v: &N) -> Result<u128> {
    Ok(u128::from_le_bytes(v.bytes()?.try_into()?))
}
fn add(map: &mut Balances, key: (Address, Address), amount: u128) -> Result<()> {
    let n = map.entry(key).or_default();
    *n = n
        .checked_add(amount)
        .ok_or_else(|| error("reservation overflow"))?;
    Ok(())
}
fn anchor(node: &dyn WalletNode) -> Result<(u32, Value)> {
    let height = node
        .call("get_height", json!({}))?
        .as_u64()
        .ok_or_else(|| error("invalid node height"))?
        .try_into()?;
    let hash = node.call("get_block_hash", json!({"height":height}))?;
    if hash.is_null() {
        return Err(error("node has no canonical tip"));
    }
    Ok((height, hash))
}
fn fresh(a: &mut Account, node: &dyn WalletNode) -> Result<(u32, Balances, Balances)> {
    if a.addresses().is_empty() {
        return Err(error("unlock wallet once to derive its addresses"));
    }
    // Retain confirmed pending records until expiry: a later fork can make them
    // pending again. No ledger values survive this function.
    for _ in 0..3 {
        let before = anchor(node)?;
        let mut balances = Balances::new();
        for owners in a.addresses().chunks(512) {
            let rows=node.call("get_all_balances",json!({"addresses":owners.iter().map(|a|a.0).collect::<Vec<_>>(),"whitelist":[],"limit":-1}))?;
            for row in rows
                .as_array()
                .ok_or_else(|| error("invalid node balance map"))?
            {
                let owner = Address(bytes(&row[0][0])?);
                let currency = Address(bytes(&row[0][1])?);
                if !owners.contains(&owner) {
                    return Err(error("node returned an unrequested owner"));
                }
                balances.insert((owner, currency), u128::from_le_bytes(bytes(&row[1])?));
            }
        }
        let mut reserved = a.spending.reserved.clone();
        for (id, p) in &a.spending.pending {
            if p.expires > before.0 && node.call("get_tx_height", json!({"id":id}))?.is_null() {
                for (&key, &n) in &p.debits {
                    add(&mut reserved, key, n)?;
                }
            }
        }
        if anchor(node)? != before {
            continue;
        }
        a.spending.pending.retain(|_, p| p.expires > before.0);
        let mut spendable = balances.clone();
        for (key, n) in &reserved {
            if let Some(value) = spendable.get_mut(key) {
                *value = value.saturating_sub(*n);
            }
        }
        return Ok((before.0, spendable, balances));
    }
    Err(error(
        "node tip changed while reading wallet balances; retry",
    ))
}
fn remember(a: &mut Account, tx: &Record, fee: u32) -> Result<()> {
    let id: [u8; 32] = tx.field("id")?.bytes()?.try_into()?;
    if a.spending.pending.len() >= 1024 && !a.spending.pending.contains_key(&id) {
        return Err(error("wallet pending transaction limit reached"));
    }
    let mut debits = Balances::new();
    for v in tx.field("inputs")?.seq()? {
        let r = v.record()?;
        let owner = addr(r.field("address")?)?;
        if a.addresses().contains(&owner) {
            add(
                &mut debits,
                (owner, addr(r.field("contract")?)?),
                atomic(r.field("amount")?)?,
            )?;
        }
    }
    if let Some(sender) = tx.field("sender")?.optional()? {
        let sender = addr(sender)?;
        if a.addresses().contains(&sender) {
            add(&mut debits, (sender, Address::default()), fee.into())?;
        }
    }
    a.spending.pending.insert(
        id,
        Pending {
            expires: tx.field("expires")?.uint()?.try_into()?,
            debits,
        },
    );
    Ok(())
}
pub const METHODS: &[&str] = &[
    "send",
    "send_many",
    "send_from",
    "deploy",
    "execute",
    "deposit",
    "complete",
    "sign_off",
    "send_off",
    "get_balance",
    "get_balances",
    "reserve",
    "release",
    "release_all",
    "mark_spent",
    "reset_cache",
    "update_cache",
];
pub fn register(
    methods: &mut Methods,
    accounts: Arc<Mutex<BTreeMap<u32, Account>>>,
    node: Option<Arc<dyn WalletNode>>,
) {
    for &method in METHODS {
        let accounts = accounts.clone();
        let node = node.clone();
        let permission = if matches!(method, "get_balance" | "get_balances" | "update_cache") {
            "REMOTE"
        } else {
            "SPENDING"
        };
        methods.add(
            format!("mmx.Wallet.{method}"),
            Some(format!("mmx.permission_e.{permission}")),
            move |args, _| {
                (|| -> Result<Value> {
                    let mut accounts =
                        accounts.lock().map_err(|_| error("wallet lock poisoned"))?;
                    if method == "release_all" {
                        for a in accounts.values_mut() {
                            a.spending.reserved.clear();
                        }
                        return Ok(Value::Null);
                    }
                    let index: u32 = args["index"]
                        .as_u64()
                        .ok_or_else(|| error("missing index"))?
                        .try_into()?;
                    let a = accounts
                        .get_mut(&index)
                        .ok_or_else(|| error("wallet index out of range"))?;
                    let spending = matches!(
                        method,
                        "send"
                            | "send_many"
                            | "send_from"
                            | "deploy"
                            | "execute"
                            | "deposit"
                            | "complete"
                            | "sign_off"
                            | "send_off"
                    );
                    let options: mmx::spend_options_t = serde_json::from_value(
                        args.get("options").cloned().unwrap_or_else(|| json!({})),
                    )?;
                    let temporary = spending && a.is_locked() && options.passphrase.is_some();
                    if temporary {
                        a.unlock(options.passphrase.as_deref().unwrap())?;
                    }
                    let out = (|| {
                        if spending {
                            a.wallet()?;
                        }
                        dispatch(a, node.as_deref(), method, &args, &options)
                    })();
                    if temporary {
                        a.lock();
                    }
                    out
                })()
                .map_err(|e| vnx::Error::Remote(e.to_string()))
            },
        );
    }
}
fn dispatch(
    a: &mut Account,
    node: Option<&dyn WalletNode>,
    method: &str,
    args: &Value,
    options: &mmx::spend_options_t,
) -> Result<Value> {
    if matches!(method, "reserve" | "release" | "mark_spent") {
        let rows = args["amounts"]
            .as_array()
            .ok_or_else(|| error("invalid reservation map"))?;
        let mut next = a.spending.reserved.clone();
        for row in rows {
            let key = (Address(bytes(&row[0][0])?), Address(bytes(&row[0][1])?));
            if !a.addresses().contains(&key.0) {
                return Err(error("reservation address not owned"));
            }
            let amount = u128::from_le_bytes(bytes(&row[1])?);
            if method == "release" {
                let value = next.entry(key).or_default();
                *value = value.saturating_sub(amount);
            } else {
                add(&mut next, key, amount)?;
            }
        }
        next.retain(|_, v| *v > 0);
        if next.len() > 10000 {
            return Err(error("reservation limit reached"));
        }
        a.spending.reserved = next;
        return Ok(Value::Null);
    }
    let node =
        node.ok_or_else(|| error("wallet has no node configured; start with --node ENDPOINT"))?;
    if matches!(
        method,
        "get_balance" | "get_balances" | "reset_cache" | "update_cache"
    ) {
        let (_, spendable, totals) = fresh(a, node)?;
        if matches!(method, "reset_cache" | "update_cache") {
            return Ok(Value::Null);
        }
        let mut out: BTreeMap<Address, (u128, u128)> = BTreeMap::new();
        for ((_, currency), n) in totals {
            let value = out.entry(currency).or_default();
            value.0 = value
                .0
                .checked_add(n)
                .ok_or_else(|| error("balance overflow"))?;
        }
        for ((_, currency), n) in spendable {
            let value = out.entry(currency).or_default();
            value.1 = value
                .1
                .checked_add(n)
                .ok_or_else(|| error("balance overflow"))?;
        }
        let value = |total: u128, spendable: u128| -> Result<Value> {
            to_json(&Record::from_generated(&mmx::balance_t {
                total: total.to_le_bytes().into(),
                spendable: spendable.to_le_bytes().into(),
                reserved: total.saturating_sub(spendable).to_le_bytes().into(),
                ..Default::default()
            })?)
        };
        if method == "get_balance" {
            let (total, spendable) = out
                .get(&Address(bytes(&args["currency"])?))
                .copied()
                .unwrap_or_default();
            return value(total, spendable);
        }
        return Ok(Value::Array(
            out.into_iter()
                .map(|(c, (t, s))| Ok(json!([c.0, value(t, s)?])))
                .collect::<Result<_>>()?,
        ));
    }
    let params: mmx::ChainParams = serde_json::from_value(node.call("get_params", json!({}))?)?;
    if params.network.is_empty() {
        return Err(error("node network is empty"));
    }
    if method == "sign_off" {
        return to_json(
            &a.wallet()?
                .sign_native(&from_json(&args["tx"])?, &params, options)?,
        );
    }
    let (height, balances, _) = fresh(a, node)?;
    if node.call("get_synced_height", json!({}))?.is_null() {
        return Err(error("node is not synced"));
    }
    let mut tx = if matches!(method, "complete" | "send_off") {
        from_json(&args["tx"])?
    } else {
        template(a, node, method, args, options)?
    };
    if method != "send_off" {
        tx = a
            .wallet()?
            .complete_native(&tx, &balances, &params, height, options)?;
    }
    if method == "complete" {
        return to_json(&tx);
    }
    if method == "send_off" && tx.field("exec_result")?.optional()?.is_some() {
        tx.fields.insert("exec_result".into(), N::Optional(None));
        tx.fields.insert(
            "static_cost".into(),
            N::Uint(tx.calc_cost(&(&params).into())?),
        );
        tx.fields.insert(
            "content_hash".into(),
            N::Bytes(tx.calc_hash(true, 0)?.to_vec()),
        );
    }
    let receipt = from_json(&node.call("validate", json!({"tx":to_json(&tx)?}))?)?;
    if receipt.field("did_fail")?.boolean()? {
        return Err(error("transaction execution would fail"));
    }
    let fee: u32 = receipt.field("total_fee")?.uint()?.try_into()?;
    let submit = method == "send_off" || (method != "complete" && options.auto_send);
    if submit || options.mark_spent {
        // Keep reservations even if the send reply is lost. An explicit retry
        // of send_off with the same ID replaces, rather than adds, its debit.
        remember(a, &tx, fee)?;
    }
    if submit {
        node.call(
            "add_transaction",
            json!({"tx":to_json(&tx)?,"pre_validate":true}),
        )?;
    }
    if method == "send_off" {
        return Ok(Value::Null);
    }
    if !submit && method != "complete" {
        tx.fields.insert(
            "exec_result".into(),
            N::Optional(Some(Box::new(N::Record(receipt)))),
        );
    }
    to_json(&tx)
}
fn template(
    a: &Account,
    node: &dyn WalletNode,
    method: &str,
    args: &Value,
    options: &mmx::spend_options_t,
) -> Result<Record> {
    let mut tx = Record::from_generated(&mmx::Transaction::default())?;
    let note = match method {
        "send" | "send_many" => "TRANSFER",
        "send_from" => "WITHDRAW",
        "deploy" => "DEPLOY",
        "execute" => "EXECUTE",
        "deposit" => "DEPOSIT",
        _ => return Err(error("unsupported spending method")),
    };
    let note: mmx::tx_note_e = serde_json::from_value(json!(note))?;
    tx.fields.insert("note".into(), N::Uint(note.value.into()));
    let output = |dst: &Value, amount: &Value| -> Result<N> {
        Ok(N::Record(Record::from_generated(&mmx::txout_t {
            address: bytes::<32>(dst)?.into(),
            contract: bytes::<32>(&args["currency"])?.into(),
            amount: bytes::<16>(amount)?.into(),
            memo: options.memo.clone(),
        })?))
    };
    if matches!(method, "send" | "send_from") {
        tx.fields.insert(
            "outputs".into(),
            N::Sequence(vec![output(&args["dst_addr"], &args["amount"])?]),
        );
    }
    if method == "send_many" {
        let rows = args["amounts"]
            .as_array()
            .ok_or_else(|| error("invalid amounts"))?;
        if rows.is_empty() {
            return Err(error("empty recipient list"));
        }
        tx.fields.insert(
            "outputs".into(),
            N::Sequence(
                rows.iter()
                    .map(|r| output(&r[0], &r[1]))
                    .collect::<Result<_>>()?,
            ),
        );
    }
    if method == "send_from" {
        let source = Address(bytes(&args["src_addr"])?);
        // Contract withdrawals need owner resolution in the caller's options;
        // public-key source addresses need no IS_EXEC override.
        if !a.addresses().contains(&source) {
            return Err(error("contract send_from requires complete with owner_map"));
        }
        let input = mmx::txin_t {
            address: source.0.into(),
            contract: bytes::<32>(&args["currency"])?.into(),
            amount: bytes::<16>(&args["amount"])?.into(),
            ..Default::default()
        };
        let amount = u128::from_le_bytes(input.amount.0);
        let confirmed = node.call(
            "get_balance",
            json!({"address":source.0,"currency":input.contract.0}),
        )?;
        if u128::from_le_bytes(bytes(&confirmed)?) < amount {
            return Err(error("insufficient source balance"));
        }
        tx.fields.insert(
            "inputs".into(),
            N::Sequence(vec![N::Record(Record::from_generated(&input)?)]),
        );
    }
    if method == "deploy" {
        let contract = from_json(&args["contract"])?;
        tx.fields.insert("deploy".into(), N::Record(contract));
    }
    if matches!(method, "execute" | "deposit") {
        // The method schema converts each argument to a native Variant before
        // extracting it, so no hash serialization goes through a generic JSON object.
        let mut request = args.clone();
        request["__type"] = json!(format!("mmx.Wallet.{method}"));
        let request = from_json(&request)?;
        let mut op = if method == "execute" {
            Record::from_generated(&mmx::operation::Execute::default())?
        } else {
            Record::from_generated(&mmx::operation::Deposit::default())?
        };
        for key in ["address", "method", "args"] {
            op.fields.insert(key.into(), request.field(key)?.clone());
        }
        let user = if method == "execute" && !args["user"].is_null() {
            let i: usize = args["user"]
                .as_u64()
                .ok_or_else(|| error("invalid user offset"))?
                .try_into()?;
            Some(
                a.addresses()
                    .get(i)
                    .ok_or_else(|| error("user offset out of range"))?
                    .0,
            )
        } else {
            options.user.as_ref().map(|a| a.0)
        };
        op.fields.insert(
            "user".into(),
            N::Optional(user.map(|a| Box::new(N::Bytes(a.to_vec())))),
        );
        if method == "deposit" {
            for key in ["currency", "amount"] {
                op.fields.insert(key.into(), request.field(key)?.clone());
            }
        }
        tx.fields
            .insert("execute".into(), N::Sequence(vec![N::Record(op)]));
    }
    Ok(tx)
}

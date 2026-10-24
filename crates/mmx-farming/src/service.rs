//! Native VNX RPC services. Signing/spending services bind only to a loopback
//! address or a private Unix socket. Public node RPC never inherits these grants.
use crate::{error, farmer::Farmer, harvester::Harvester, wallet::Account, Result};
use mmx_consensus::Record;
use mmx_types::mmx;
use mmx_wallet::Address;
use std::{
    collections::BTreeMap,
    io::Cursor,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use vnx::{
    binary::{Reader, Writer},
    net::{Endpoint, Server, ServerOptions},
    runtime::{Methods, Runtime, Session},
    serde_json::{self, json, Value},
    VnxType,
};

pub fn to_json(record: &Record) -> Result<Value> {
    Ok(Reader::new(Cursor::new(record.encode()?), mmx_types::vnx_registry()?).read()?)
}
pub fn from_json(value: &Value) -> Result<Record> {
    let mut w = Writer::new(Vec::new(), mmx_types::vnx_registry()?);
    w.write(value)?;
    Ok(Record::decode(&w.into_inner())?)
}
fn generated<T: VnxType>(value: &T) -> Result<Value> {
    to_json(&Record::from_generated(value)?)
}
fn remote(e: Box<dyn std::error::Error + Send + Sync>) -> vnx::Error {
    vnx::Error::Remote(e.to_string())
}
fn result(value: Value) -> Value {
    value
}
fn bytes<const N: usize>(v: &Value) -> Result<[u8; N]> {
    Ok(serde_json::from_value::<vnx::Fixed<u8, N>>(v.clone())?.0)
}
fn number(v: &Value, key: &str) -> Result<u32> {
    v[key]
        .as_u64()
        .and_then(|x| x.try_into().ok())
        .ok_or_else(|| error(format!("invalid {key}")))
}
pub fn bind_private(
    endpoint: Endpoint,
    runtime: Runtime,
    options: ServerOptions,
) -> Result<Server> {
    if let Endpoint::Tcp(address) = &endpoint {
        let address: SocketAddr = address
            .parse()
            .map_err(|_| error("use an explicit loopback IP for private services"))?;
        if !address.ip().is_loopback() {
            return Err(error("signing services require a loopback endpoint"));
        }
    }
    let server = Server::bind(endpoint, runtime, mmx_types::vnx_registry()?, options)?;
    #[cfg(unix)]
    if let Endpoint::Unix(path) = server.endpoint() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(server)
}
pub const WALLET_METHODS: &[&str] = &[
    "get_address",
    "get_all_addresses",
    "get_account",
    "get_all_accounts",
    "find_wallet_by_addr",
    "is_locked",
    "lock",
    "unlock",
    "sign_msg",
    "get_farmer_keys",
    "get_all_farmer_keys",
    "get_master_seed",
    "get_mnemonic_seed",
    "export_wallet",
];
pub fn wallet_methods(accounts: Arc<Mutex<BTreeMap<u32, Account>>>) -> Methods {
    wallet_methods_with_node(accounts, None)
}
pub fn wallet_methods_with_node(
    accounts: Arc<Mutex<BTreeMap<u32, Account>>>,
    node: Option<Arc<dyn crate::spending::WalletNode>>,
) -> Methods {
    let mut methods = Methods::new();
    for &method in WALLET_METHODS {
        let accounts = accounts.clone();
        let sensitive = matches!(
            method,
            "sign_msg"
                | "get_farmer_keys"
                | "get_all_farmer_keys"
                | "get_master_seed"
                | "get_mnemonic_seed"
                | "export_wallet"
                | "unlock"
        );
        methods.add(
            format!("mmx.Wallet.{method}"),
            Some(format!(
                "mmx.permission_e.{}",
                if sensitive { "SPENDING" } else { "REMOTE" }
            )),
            move |args, _| {
                (|| -> Result<Value> {
                    let mut accounts =
                        accounts.lock().map_err(|_| error("wallet lock poisoned"))?;
                    for a in accounts.values_mut() {
                        a.expire();
                    }
                    match method {
                        "get_all_accounts" => {
                            return Ok(result(Value::Array(
                                accounts
                                    .iter()
                                    .map(|(i, a)| generated(&a.info(*i)))
                                    .collect::<Result<_>>()?,
                            )))
                        }
                        "get_all_farmer_keys" => {
                            return Ok(result(Value::Array(
                                accounts
                                    .values()
                                    .map(|a| {
                                        let k = a.farmer_key()?;
                                        Ok(json!([
                                            k.secret_bytes().as_slice(),
                                            k.public_key().as_slice()
                                        ]))
                                    })
                                    .collect::<Result<_>>()?,
                            )))
                        }
                        "get_all_addresses" if args["index"].as_i64() == Some(-1) => {
                            return Ok(result(json!(accounts
                                .values()
                                .flat_map(|a| a.addresses().iter().map(|a| a.0))
                                .collect::<Vec<_>>())))
                        }
                        "find_wallet_by_addr" => {
                            let address = Address(bytes(&args["address"])?);
                            return Ok(result(json!(accounts
                                .iter()
                                .find(|(_, a)| a.addresses().contains(&address))
                                .map(|(i, _)| i64::from(*i))
                                .unwrap_or(-1))));
                        }
                        _ => (),
                    }
                    let index = number(&args, "index")?;
                    let a = accounts
                        .get_mut(&index)
                        .ok_or_else(|| error("wallet index out of range"))?;
                    let value = match method {
                        "get_address" => json!(
                            a.addresses()
                                .get(number(&args, "offset")? as usize)
                                .ok_or_else(|| error(
                                    "address index out of range or wallet has not been unlocked"
                                ))?
                                .0
                        ),
                        "get_all_addresses" => {
                            json!(a.addresses().iter().map(|a| a.0).collect::<Vec<_>>())
                        }
                        "get_account" => generated(&a.info(index))?,
                        "is_locked" => json!(a.is_locked()),
                        "lock" => {
                            a.lock();
                            Value::Null
                        }
                        "unlock" => {
                            a.unlock(
                                args["passphrase"]
                                    .as_str()
                                    .ok_or_else(|| error("missing passphrase"))?,
                            )?;
                            Value::Null
                        }
                        "sign_msg" => {
                            let sol = a.wallet()?.sign_message(
                                Address(bytes(&args["address"])?),
                                &bytes(&args["msg"])?,
                            )?;
                            generated(&mmx::solution::PubKey {
                                version: sol.version,
                                pubkey: sol.pubkey.into(),
                                signature: sol.signature.into(),
                            })?
                        }
                        "get_farmer_keys" => {
                            let k = a.farmer_key()?;
                            json!([k.secret_bytes().as_slice(), k.public_key().as_slice()])
                        }
                        "get_master_seed" => json!(a.key_file.seed_value),
                        "get_mnemonic_seed" => {
                            json!(mmx_wallet::seed_to_words(&a.key_file.seed_value)
                                .split_whitespace()
                                .collect::<Vec<_>>())
                        }
                        "export_wallet" => generated(&mmx::KeyFile {
                            seed_value: a.key_file.seed_value.into(),
                            finger_print: a.key_file.finger_print.clone(),
                        })?,
                        _ => return Err(error("unsupported wallet method")),
                    };
                    Ok(if matches!(method, "lock" | "unlock") {
                        json!({})
                    } else {
                        result(value)
                    })
                })()
                .map_err(remote)
            },
        );
    }
    crate::spending::register(&mut methods, accounts, node);
    methods
}
pub const FARMER_METHODS: &[&str] = &[
    "get_farmer_keys",
    "get_farm_info",
    "get_mac_addr",
    "get_partial_diff",
    "get_partial_diffs",
    "sign_block",
    "sign_vote",
];
pub fn farmer_methods(farmer: Arc<Mutex<Farmer>>, params: mmx::ChainParams, mac: u64) -> Methods {
    let mut methods = Methods::new();
    for &method in FARMER_METHODS {
        let farmer = farmer.clone();
        let params = params.clone();
        methods.add(
            format!("mmx.Farmer.{method}"),
            Some(format!(
                "mmx.permission_e.{}",
                if method.starts_with("sign_") {
                    "SIGN_BLOCK"
                } else {
                    "REMOTE"
                }
            )),
            move |args, _| {
                (|| -> Result<Value> {
                    let f = farmer.lock().map_err(|_| error("farmer lock poisoned"))?;
                    let value = match method {
                        "get_farmer_keys" => json!(f
                            .public_keys()
                            .iter()
                            .map(|k| k.to_vec())
                            .collect::<Vec<_>>()),
                        "get_farm_info" => generated(&f.farm_info()?)?,
                        "get_mac_addr" => json!(mac),
                        "get_partial_diff" => json!(f
                            .partial_difficulties
                            .get(&Address(bytes(&args["plot_nft"])?))
                            .copied()
                            .unwrap_or(0)),
                        "get_partial_diffs" => {
                            let mut out = Vec::new();
                            for v in args["plot_nfts"]
                                .as_array()
                                .ok_or_else(|| error("invalid plot_nfts"))?
                            {
                                let a = bytes(v)?;
                                if let Some(d) = f.partial_difficulties.get(&Address(a)) {
                                    out.push(json!([a, d]));
                                }
                            }
                            json!(out)
                        }
                        "sign_block" => {
                            to_json(&f.sign_block(&from_json(&args["block"])?, &params)?)?
                        }
                        "sign_vote" => json!(f.sign_vote(&from_json(&args["vote"])?)?.as_slice()),
                        _ => return Err(error("unsupported farmer method")),
                    };
                    Ok(result(value))
                })()
                .map_err(remote)
            },
        );
    }
    methods
}
pub fn harvester_methods(harvester: Arc<Mutex<Harvester>>) -> Methods {
    let mut methods = Methods::new();
    for method in [
        "reload",
        "add_plot_dir",
        "rem_plot_dir",
        "get_farm_info",
        "get_total_bytes",
    ] {
        let harvester = harvester.clone();
        methods.add(
            format!("mmx.Harvester.{method}"),
            Some("mmx.permission_e.REMOTE".into()),
            move |args, _| {
                (|| -> Result<Value> {
                    let mut h = harvester
                        .lock()
                        .map_err(|_| error("harvester lock poisoned"))?;
                    match method {
                        "reload" => h.reload(),
                        "add_plot_dir" | "rem_plot_dir" => {
                            let path = std::path::PathBuf::from(
                                args["path"]
                                    .as_str()
                                    .ok_or_else(|| error("invalid plot path"))?,
                            );
                            if method == "add_plot_dir" {
                                h.directories.insert(path);
                            } else {
                                h.directories.remove(&path);
                            }
                            h.reload();
                        }
                        "get_farm_info" => return Ok(result(generated(&h.farm_info()?)?)),
                        "get_total_bytes" => return Ok(result(json!(h.farm_info()?.total_bytes))),
                        _ => return Err(error("unsupported harvester method")),
                    }
                    Ok(json!({}))
                })()
                .map_err(remote)
            },
        );
    }
    methods
}
pub fn private_options(services: impl IntoIterator<Item = String>) -> ServerOptions {
    ServerOptions {
        services: services.into_iter().collect(),
        session: Session::new(
            "local",
            [
                "mmx.permission_e.REMOTE".into(),
                "mmx.permission_e.SPENDING".into(),
                "mmx.permission_e.SIGN_BLOCK".into(),
                "vnx.permission_e.VIEW".into(),
                "vnx.permission_e.PUBLISH".into(),
            ],
        ),
        ..Default::default()
    }
}

/// Dedicated remote-harvester surface. Even a privileged configured login is
/// restricted to these topic/read grants; block signing stays on the private socket.
pub fn harvester_options(
    auth: vnx::auth::AuthenticationServer,
    require_login: bool,
) -> ServerOptions {
    let restrict = |mut session: Session| {
        session.permissions = [
            "mmx.permission_e.REMOTE",
            "vnx.permission_e.VIEW",
            "vnx.permission_e.PUBLISH",
        ]
        .map(str::to_owned)
        .into();
        session
    };
    let login = auth.clone();
    let validate = auth.clone();
    ServerOptions {
        services: ["Node".into(), "Farmer".into()].into(),
        session: if require_login {
            Session::anonymous()
        } else {
            restrict(Session::anonymous())
        },
        authenticator: Some(Arc::new(move |name, hash| {
            login.authenticate_hash(name, hash).map(restrict)
        })),
        session_validator: Some(Arc::new(move |id| {
            Ok(validate.get_session(id)?.map(restrict))
        })),
        session_releaser: Some(Arc::new(move |id| auth.logout(id))),
        export_topics: ["harvester.challenges".into()].into(),
        publish_topics: [
            "harvester.proof".into(),
            "harvester.info".into(),
            "harvester.lookups".into(),
            "harvester.partials".into(),
        ]
        .into(),
        max_connections: 64,
        receive_limits: vnx::binary::Limits {
            max_container: 4096,
            max_string_bytes: 4096,
            max_value_bytes: 256 * 1024,
            max_allocated_bytes: 1024 * 1024,
            max_schemas: 4096,
            max_schema_bytes: 512 * 1024,
            ..Default::default()
        },
        ..Default::default()
    }
}

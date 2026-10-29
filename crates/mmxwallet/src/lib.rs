mod config;
pub mod error;
mod remote;
mod rpc;
mod secrets;
pub mod service;
mod storage;
mod validate;

use clap::Parser;
use error::{invalid, rpc_invalid, Error, Result};
use mmx_wallet::{
    format_amount, seed_to_words, words_to_seed, Address, KeyFile, Output, Transaction,
    TransferOptions,
};
use rpc::Rpc;
use secrets::Secrets;
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
};
use zeroize::Zeroizing;

/// Standalone native MMX wallet using public HTTP(S) RPC.
#[derive(Parser)]
#[command(
    version,
    after_help = "Commands: create, import, list, use <INDEX|FINGERPRINT>, rename <NAME>, set-addresses <N>,\nmnemonic, get mnemonic, address, addresses, balance, history, send, broadcast, info,\ntransaction <TXID>, capabilities\n\nrename and set-addresses change the selected wallet (--wallet or the active one).\n\nWallet directory: $MMX_HOME or ~/.mmx (Windows: %USERPROFILE%\\.mmx)\nSettings: <wallet directory>/mmxwallet.json"
)]
struct Args {
    #[arg(default_value = "help")]
    command: String,
    subject: Option<String>,
    /// Chain server URL [default: the "rpc" setting in mmxwallet.json, else rpc.mmx.network]
    #[arg(long, short = 'r')]
    rpc: Option<String>,
    #[arg(long, short = 'f')]
    file: Option<PathBuf>,
    #[arg(long, short = 'w')]
    wallet: Option<String>,
    #[arg(long, short = 'a', allow_hyphen_values = true)]
    amount: Option<String>,
    #[arg(long, short = 't')]
    target: Option<String>,
    #[arg(long, short = 'x', default_value = "")]
    currency: String,
    #[arg(long, short = 'm', allow_hyphen_values = true)]
    memo: Option<String>,
    #[arg(long, short = 'k', default_value_t = 0)]
    offset: u32,
    /// Addresses to use, 1 to 100 [default: the wallet's setting (see set-addresses), else 10]
    #[arg(long, short = 'N')]
    num_addresses: Option<u32>,
    #[arg(long, default_value_t = 0)]
    account: u32,
    #[arg(long, default_value_t = 20)]
    limit: u32,
    #[arg(long, default_value_t = 1.0)]
    fee_ratio: f64,
    #[arg(long, default_value_t = 100)]
    expire_delta: u32,
    #[arg(long)]
    transaction: Option<PathBuf>,
    #[arg(long, short = 'y')]
    yes: bool,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    input_stdin: bool,
    #[arg(long)]
    non_interactive: bool,
    #[arg(long)]
    show_mnemonic: bool,
    #[arg(long)]
    with_passphrase: bool,
}
impl Args {
    fn non_interactive(&self) -> bool {
        self.json || self.input_stdin || self.non_interactive
    }
    fn rpc(&self) -> Result<Rpc> {
        Rpc::new(&self.rpc_url())
    }
    fn rpc_url(&self) -> String {
        if let Some(url) = &self.rpc {
            return url.clone();
        }
        storage::directory()
            .ok()
            .and_then(|dir| config::rpc(&config::read_lenient(&dir)).map(str::to_owned))
            .unwrap_or_else(|| rpc::DEFAULT_URL.to_owned())
    }
    /// The selected key file and, for a wallet in the wallet directory, its
    /// file name (which keys its settings).
    fn key_path(&self) -> Result<(PathBuf, Option<String>)> {
        if self.file.is_some() && self.wallet.is_some() {
            return Err(invalid("--file and --wallet cannot be used together"));
        }
        if let Some(path) = &self.file {
            let name = storage::directory()
                .ok()
                .and_then(|dir| storage::wallet_file_name(&dir, path));
            return Ok((path.clone(), name));
        }
        let dir = storage::directory()?;
        let (entries, unreadable) = storage::scan(&dir)?;
        // The configuration only names the active wallet, so a selected
        // wallet stays usable when the file is damaged.
        let active = match self.wallet {
            None => storage::active(&dir)?,
            Some(_) => None,
        };
        let path = storage::select(
            &entries,
            &unreadable,
            self.wallet.as_deref(),
            active.as_deref(),
            false,
        )?
        .path;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        Ok((path, name))
    }
    /// --num-addresses, else the wallet's setting, else the default.
    fn address_count(&self, wallet: Option<&str>) -> u32 {
        self.num_addresses.unwrap_or_else(|| {
            wallet
                .and_then(|id| {
                    let dir = storage::directory().ok()?;
                    Some(config::num_addresses(&config::read_lenient(&dir), id))
                })
                .unwrap_or(validate::DEFAULT_NUM_ADDRESSES)
        })
    }
    fn unlock(&self, key: &KeyFile, secrets: &Secrets, count: u32) -> Result<mmx_wallet::Wallet> {
        let pass = if key.requires_passphrase() {
            secrets.value("passphrase", self.non_interactive())?
        } else {
            Zeroizing::new(String::new())
        };
        Ok(key.unlock(&pass, self.account, count)?)
    }
    /// Observing a cached wallet never derives or keeps private keys.
    fn view_wallet(
        &self,
        path: &std::path::Path,
        key: &KeyFile,
        secrets: &Secrets,
        count: u32,
    ) -> Result<mmx_wallet::Wallet> {
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let id = path
            .file_name()
            .ok_or_else(|| invalid("invalid wallet file"))?
            .to_string_lossy();
        let fp = key.fingerprint();
        let config = config::read_lenient(dir);
        if let Some(addresses) = config::cached_addresses_for(&config, &id, &fp, self.account)
            .filter(|a| a.len() >= count as usize)
        {
            return Ok(mmx_wallet::Wallet::watch_only(
                addresses.into_iter().take(count as usize).collect(),
            )?);
        }
        let wallet = self.unlock(key, secrets, count)?;
        let addresses = wallet.addresses.clone();
        drop(wallet);
        // Caching is optional: an existing key remains usable when its
        // adjacent settings cannot be read or written.
        if let Ok(mut config) = config::read(dir) {
            config::set_cached_addresses_for(&mut config, &id, &fp, self.account, &addresses);
            let _ = config::save(dir, config);
        }
        Ok(mmx_wallet::Wallet::watch_only(addresses)?)
    }

    fn list_address(&self, entry: &storage::Entry, secrets: &Secrets) -> Result<Option<Address>> {
        let dir = entry
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        if let Some(id) = entry.path.file_name().and_then(|s| s.to_str()) {
            if let Some(addresses) = config::cached_addresses_for(
                &config::read_lenient(dir),
                id,
                &entry.fingerprint,
                self.account,
            ) {
                if let Some(address) = addresses.first() {
                    return Ok(Some(*address));
                }
            }
        }
        let passphrase = if entry.with_passphrase {
            match secrets.passphrase.as_deref() {
                Some(passphrase) => passphrase,
                None => return Ok(None),
            }
        } else {
            ""
        };
        let key = storage::read_key(&entry.path)?;
        match key.unlock(passphrase, self.account, 1) {
            Ok(wallet) => Ok(Some(wallet.addresses[0])),
            // A list can contain wallets with different passphrases. Show the
            // addresses unlocked by this input and leave the others locked.
            Err(mmx_wallet::Error::InvalidPassphrase) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
fn print_json(command: &str, mut v: Value) {
    v["schema_version"] = json!(1);
    v["command"] = json!(command);
    if v["status"].is_null() {
        v["status"] = json!("ok");
    }
    println!("{v}");
}
fn run(a: &Args) -> Result<()> {
    if a.command == "help" {
        use clap::CommandFactory;
        Args::command().print_help().map_err(Error::from)?;
        println!();
        return Ok(());
    }
    let mut secrets = if a.input_stdin {
        Secrets::read()?
    } else {
        Secrets::default()
    };
    if a.command == "capabilities" {
        print_json(
            &a.command,
            json!({"commands":["create","import","list","use","rename","set-addresses","mnemonic","get","address","addresses","balance","history","send","broadcast","info","transaction","capabilities"], "secret_input":"stdin-json-line", "memo_max_bytes":validate::MEMO_MAX_BYTES, "max_num_addresses":validate::MAX_NUM_ADDRESSES, "default_num_addresses":validate::DEFAULT_NUM_ADDRESSES, "wallet_names":true, "curl_override":false, "http_transport":"native-rust", "prepare_transaction":true, "key_file_encrypted":false}),
        );
        return Ok(());
    }
    if let Some(n) = a.num_addresses {
        validate::num_addresses(n)
            .map_err(|_| invalid("num-addresses must be between 1 and 100"))?;
    }
    if a.subject.is_some()
        && !matches!(
            a.command.as_str(),
            "get" | "use" | "transaction" | "rename" | "set-addresses"
        )
    {
        return Err(invalid("unexpected positional argument"));
    }
    match a.command.as_str() {
        "create" | "import" => {
            if a.wallet.is_some() {
                return Err(invalid(
                    "--wallet cannot be used when creating or importing",
                ));
            }
            let dir = if a.file.is_none() {
                Some(storage::directory()?)
            } else {
                None
            };
            // The new wallet becomes the active one in the configuration;
            // a damaged file fails here, before any secret is entered or
            // a key file is written.
            if let Some(dir) = &dir {
                config::read(dir)?;
            }
            let pass = if a.with_passphrase {
                let pass = secrets.value("passphrase", a.non_interactive())?;
                if !a.non_interactive()
                    && secrets.passphrase.is_none()
                    && *pass != *secrets::password("passphrase (again): ")?
                {
                    return Err(invalid("passphrase mismatch"));
                }
                Some(pass)
            } else {
                None
            };
            let pass_ref = pass.as_ref().map(|p| p.as_str());
            let key = if a.command == "create" {
                KeyFile::generate(pass_ref)
            } else {
                let words = secrets.value("mnemonic", a.non_interactive())?;
                KeyFile::new(words_to_seed(words.trim())?, pass_ref)
            };
            let fingerprint = key.fingerprint();
            let path = a.file.clone().unwrap_or_else(|| {
                dir.as_ref()
                    .unwrap()
                    .join(format!("wallet_{fingerprint}.dat"))
            });
            if let Some(dir) = &dir {
                if storage::wallets(dir)?
                    .iter()
                    .any(|w| w.fingerprint == fingerprint)
                {
                    return Err(Error::new("wallet_exists", "wallet already exists"));
                }
            }
            let wallet = key.unlock(pass_ref.unwrap_or(""), a.account, a.address_count(None))?;
            storage::save_key(&path, &key)?;
            if let (Some(parent), Some(id)) =
                (path.parent(), path.file_name().and_then(|s| s.to_str()))
            {
                // Adjacent settings are optional for an explicitly selected path.
                if let Ok(mut config) = config::read(parent) {
                    config::set_num_addresses(&mut config, id, a.address_count(None));
                    config::set_cached_addresses_for(
                        &mut config,
                        id,
                        &fingerprint,
                        a.account,
                        &wallet.addresses,
                    );
                    let _ = config::save(parent, config);
                }
            }
            if let Some(dir) = dir {
                storage::set_active(&dir, &path)?;
            }
            let mut v = json!({"wallet_file":storage::absolute(&path)?, "fingerprint":fingerprint, "address":wallet.addresses[0], "with_passphrase":key.requires_passphrase()});
            if a.json {
                if a.show_mnemonic {
                    v["mnemonic"] = json!(seed_to_words(&key.seed_value));
                }
                print_json(&a.command, v);
            } else {
                println!(
                    "{} wallet: {}\nFingerprint: {fingerprint}",
                    if a.command == "create" {
                        "Created"
                    } else {
                        "Imported"
                    },
                    path.display()
                );
                if a.command == "create" {
                    println!("Mnemonic: {}", seed_to_words(&key.seed_value));
                }
                println!("Address: {}", wallet.addresses[0]);
            }
        }
        "list" | "use" => {
            if a.file.is_some() || a.wallet.is_some() {
                return Err(invalid("list/use do not accept --file or --wallet"));
            }
            let dir = storage::directory()?;
            let (entries, unreadable) = storage::scan(&dir)?;
            if a.command == "use" {
                let selected = storage::select(
                    &entries,
                    &unreadable,
                    Some(
                        a.subject
                            .as_deref()
                            .ok_or_else(|| invalid("use requires INDEX or FINGERPRINT"))?,
                    ),
                    None,
                    true,
                )?;
                storage::set_active(&dir, &selected.path)?;
                if a.json {
                    print_json(
                        &a.command,
                        json!({"fingerprint":selected.fingerprint,"wallet_file":storage::absolute(&selected.path)?}),
                    );
                } else {
                    println!(
                        "Active wallet: {} ({})",
                        selected.fingerprint,
                        selected.path.display()
                    );
                }
            } else {
                let config = config::read(&dir)?;
                let selected =
                    storage::select(&entries, &unreadable, None, config::active(&config), false)
                        .ok();
                let file_name = |path: &std::path::Path| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                let mut rows = Vec::new();
                for (i, e) in entries.iter().enumerate() {
                    let active = selected.as_ref().is_some_and(|s| s.path == e.path);
                    let address = a.list_address(e, &secrets)?;
                    let file = file_name(&e.path);
                    let name = config::wallet_name(&config, &file);
                    rows.push(json!({"index":i,"fingerprint":e.fingerprint,"wallet_file":storage::absolute(&e.path)?,"with_passphrase":e.with_passphrase,"active":active,"address":address,"name":name,"num_addresses":config::num_addresses(&config, &file)}));
                    if !a.json {
                        println!(
                            "{}[{i}] {}  {file}  {}{}{}",
                            if active { "* " } else { "  " },
                            e.fingerprint,
                            address
                                .map(|a| a.to_string())
                                .unwrap_or_else(|| "[passphrase required]".into()),
                            if e.with_passphrase {
                                "  (passphrase)"
                            } else {
                                ""
                            },
                            name.map(|n| format!("  {n}")).unwrap_or_default()
                        );
                    }
                }
                let mut bad = Vec::new();
                for u in &unreadable {
                    bad.push(json!({"wallet_file":storage::absolute(&u.path)?,"code":u.error.code,"error":u.error.message}));
                    if !a.json {
                        println!(
                            "  [!] {}  unreadable: {}",
                            file_name(&u.path),
                            u.error.message.replace(char::is_control, " ")
                        );
                    }
                }
                if a.json {
                    print_json(
                        &a.command,
                        json!({"wallet_directory":storage::absolute(&dir)?,"wallets":rows,"unreadable":bad}),
                    );
                } else if entries.is_empty() && unreadable.is_empty() {
                    println!("No wallets found in {}", dir.display());
                }
            }
        }
        "rename" | "set-addresses" => {
            if a.file.is_some() {
                return Err(invalid(format!(
                    "{} changes a wallet in the wallet directory; select it with --wallet",
                    a.command
                )));
            }
            let subject = a.subject.as_deref().ok_or_else(|| {
                invalid(if a.command == "rename" {
                    "usage: mmxwallet rename <NAME>"
                } else {
                    "usage: mmxwallet set-addresses <N>"
                })
            })?;
            let (path, file) = a.key_path()?;
            let file = file.ok_or_else(|| invalid("invalid wallet filename"))?;
            let fingerprint = storage::read_key(&path)?.fingerprint();
            let dir = storage::directory()?;
            let mut config = config::read(&dir)?;
            let mut v = json!({"wallet_file":storage::absolute(&path)?,"fingerprint":fingerprint});
            if a.command == "rename" {
                let name = validate::name(subject)?;
                config::set_wallet_name(&mut config, &file, &name);
                config::save(&dir, config)?;
                if !a.json {
                    println!("Wallet {fingerprint} renamed to {name}");
                }
                v["name"] = json!(name);
            } else {
                let count = subject
                    .parse::<u32>()
                    .ok()
                    .and_then(|n| validate::num_addresses(n).ok())
                    .ok_or_else(|| invalid("number of addresses must be between 1 and 100"))?;
                let key = storage::read_key(&path)?;
                a.view_wallet(&path, &key, &secrets, count)?;
                config = config::read(&dir)?;
                config::set_num_addresses(&mut config, &file, count);
                config::save(&dir, config)?;
                if !a.json {
                    println!("Wallet {fingerprint} uses {count} addresses");
                }
                v["num_addresses"] = json!(count);
            }
            if a.json {
                print_json(&a.command, v);
            }
        }
        "info" => {
            let url = a.rpc_url();
            let v = Rpc::new(&url)?.get("/node/info")?;
            let name = rpc::text(&v, "name")?;
            let height = rpc::u32_field(&v, "height")?;
            let synced = rpc::boolean(&v, "is_synced")?;
            if a.json {
                print_json(
                    &a.command,
                    json!({"rpc":url,"network":name,"height":height,"is_synced":synced}),
                );
            } else {
                println!(
                    "RPC: {}\nNetwork: {name}\nHeight: {height}\nSynced: {}",
                    url.replace(char::is_control, " "),
                    if synced { "yes" } else { "no" }
                );
            }
        }
        "transaction" => transaction_status(a)?,
        "broadcast" => {
            let path = a
                .transaction
                .as_ref()
                .ok_or_else(|| invalid("broadcast requires --transaction PATH"))?;
            let mut bytes = Vec::new();
            fs::File::open(path)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(invalid("transaction file exceeds size limit"));
            }
            let tx: Transaction = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("invalid signed transaction file"))?;
            let rpc = a.rpc()?;
            let params = rpc.params()?;
            let height = rpc.height(&params)?;
            // The node runs a transaction at the next height.
            if tx.expires <= height {
                return Err(Error::new(
                    "transaction_expired",
                    "saved transaction has expired; prepare and review a new transaction",
                ));
            }
            tx.verify(&params)?;
            rpc.validate(&bytes, tx.max_fee_amount.into())?;
            rpc.post("/transaction/broadcast", &bytes)?;
            if a.json {
                print_json(
                    &a.command,
                    json!({"status":"broadcast","transaction_id":tx.id_hex(),"broadcast":true}),
                );
            } else {
                println!(
                    "Transaction ID: {}\nTransaction broadcast successfully.",
                    tx.id_hex()
                );
            }
        }
        "mnemonic" | "get" | "address" | "addresses" | "balance" | "history" | "send" => {
            if a.command == "get" && a.subject.as_deref() != Some("mnemonic") {
                return Err(invalid("usage: mmxwallet get mnemonic"));
            }
            let (path, file) = a.key_path()?;
            let count = a.address_count(file.as_deref());
            let key = storage::read_key(&path)?;
            // One prompt covers first-time public derivation and this send. The
            // process drops this zeroizing input and all signing keys on exit.
            if a.command == "send" && key.requires_passphrase() && secrets.passphrase.is_none() {
                let mut passphrase = secrets.value("passphrase", a.non_interactive())?;
                secrets.passphrase = Some(std::mem::take(&mut *passphrase));
            }
            match a.command.as_str() {
                "mnemonic" | "get" => {
                    let words = Zeroizing::new(seed_to_words(&key.seed_value));
                    if a.json {
                        print_json(
                            &a.command,
                            json!({"mnemonic":*words,"wallet_file":storage::absolute(&path)?,"fingerprint":key.fingerprint()}),
                        );
                    } else if a.command == "get" {
                        println!("{}", *words);
                    } else {
                        let w = a.unlock(&key, &secrets, 1)?;
                        println!("Address: {}\nMnemonic: {}", w.addresses[0], *words);
                    }
                }
                "address" | "addresses" => {
                    let w = a.view_wallet(&path, &key, &secrets, count)?;
                    if a.command == "address" {
                        let addr = w
                            .addresses
                            .get(a.offset as usize)
                            .ok_or_else(|| invalid("address offset exceeds num-addresses"))?;
                        if a.json {
                            print_json(
                                &a.command,
                                json!({"address":addr,"wallet_file":storage::absolute(&path)?}),
                            );
                        } else {
                            println!("{addr}");
                        }
                    } else if a.json {
                        print_json(
                            &a.command,
                            json!({"addresses":w.addresses,"wallet_file":storage::absolute(&path)?}),
                        );
                    } else {
                        for (i, addr) in w.addresses.iter().enumerate() {
                            println!("[{i}] {addr}");
                        }
                    }
                }
                _ => {
                    let rpc = a.rpc()?;
                    let params = rpc.params()?;
                    let w = a.view_wallet(&path, &key, &secrets, count)?;
                    if a.command == "history" {
                        if !(1..=1000).contains(&a.limit) {
                            return Err(invalid("limit must be between 1 and 1000"));
                        }
                        let height = rpc.height(&params)?;
                        let rows = remote::history(
                            &rpc,
                            &w.addresses,
                            &params,
                            &remote::Filter::parse(&a.currency)?,
                            a.limit,
                        )?;
                        if a.json {
                            print_json(
                                &a.command,
                                json!({"network":params.network,"current_height":height,"history":rows}),
                            );
                        } else {
                            if rows.is_empty() {
                                println!("No history.");
                            }
                            for row in rows.iter().rev() {
                                println!(
                                    "[{}] {} {} {} @ {} TX({}){}",
                                    if row["is_pending"] == true {
                                        "pending".to_owned()
                                    } else {
                                        row["height"].to_string()
                                    },
                                    display_text(&row["type"]),
                                    display_text(&row["amount"]),
                                    display_text(&row["symbol"]),
                                    display_text(&row["address"]),
                                    display_text(&row["txid"]),
                                    if row["memo"].is_null() {
                                        String::new()
                                    } else {
                                        format!(" Memo({})", row["memo"])
                                    }
                                );
                            }
                        }
                    } else {
                        let state = remote::State::fetch(&rpc, &w.addresses, &params)?;
                        if a.command == "balance" {
                            let rows = state.rows(&remote::Filter::parse(&a.currency)?, &params)?;
                            if a.json {
                                print_json(
                                    &a.command,
                                    json!({"network":params.network,"current_height":state.height,"balances":rows}),
                                );
                            } else {
                                for row in rows {
                                    println!(
                                        "{} {} ({})",
                                        display_text(&row["amount"]),
                                        display_text(&row["symbol"]),
                                        display_text(&row["amount_atomic"])
                                    );
                                }
                            }
                        } else {
                            send(a, &rpc, &params, &key, &mut secrets, count, &state)?;
                        }
                    }
                }
            }
        }
        _ => return Err(invalid(format!("unknown command: {}", a.command))),
    }
    Ok(())
}
fn display_text(v: &Value) -> String {
    v.as_str()
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn transaction_status(a: &Args) -> Result<()> {
    let id = a
        .subject
        .as_deref()
        .ok_or_else(|| invalid("transaction requires a transaction ID"))?
        .to_ascii_lowercase();
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "transaction requires a 64-character hexadecimal transaction ID",
        ));
    }
    let rpc = a.rpc()?;
    let params = rpc.params()?;
    let height = rpc.height(&params)?;
    let tx = rpc.get(&format!("/transaction?id={id}"))?;
    let mut confirmations = 0u64;
    let status = if tx.is_null() {
        "unknown"
    } else {
        if rpc::text(&tx, "id")?.to_ascii_lowercase() != id {
            return Err(rpc_invalid("RPC returned a different transaction ID"));
        }
        if !tx["height"].is_null() {
            let included = rpc::u32_field(&tx, "height")?;
            if included > height {
                return Err(rpc_invalid("transaction height exceeds RPC height"));
            }
            confirmations = height as u64 - included as u64 + 1;
            if rpc::boolean(&tx, "did_fail")? {
                "failed"
            } else {
                "included"
            }
        } else if rpc::u32_field(&tx, "expires")? < height {
            "expired"
        } else {
            "pending"
        }
    };
    if a.json {
        print_json(
            &a.command,
            json!({"transaction_id":id,"current_height":height,"network":params.network,"transaction":tx,"confirmations":confirmations,"status":status}),
        );
    } else {
        println!("Transaction ID: {id}\nStatus: {status}\nConfirmations: {confirmations}");
    }
    Ok(())
}
fn send(
    a: &Args,
    rpc: &Rpc,
    params: &mmx_wallet::ChainParams,
    key: &KeyFile,
    secrets: &mut Secrets,
    count: u32,
    state: &remote::State,
) -> Result<()> {
    let target: Address = a
        .target
        .as_deref()
        .ok_or_else(|| invalid("missing target address"))?
        .parse()?;
    let currency = remote::currency(&a.currency)?;
    let (decimals, symbol) = if currency == Address::default() {
        (params.decimals, "MMX")
    } else {
        let b = state
            .totals
            .get(&currency)
            .ok_or_else(|| invalid("wallet has no balance for requested currency"))?;
        (b.decimals, b.symbol.as_str())
    };
    let amount = mmx_wallet::parse_amount(
        a.amount
            .as_deref()
            .ok_or_else(|| invalid("missing amount"))?,
        decimals,
    )?;
    if !a.fee_ratio.is_finite() || a.fee_ratio <= 0.0 || a.fee_ratio > u32::MAX as f64 / 1024.0 {
        return Err(invalid("invalid fee ratio"));
    }
    let options = TransferOptions {
        fee_ratio: ((a.fee_ratio * 1024.0) as u32).max(1024),
        expire_delta: a.expire_delta,
        ..Default::default()
    };
    let tx = {
        let w = a.unlock(key, secrets, count)?;
        w.transfer(
            &state.balances,
            params,
            state.height,
            Output {
                address: target,
                contract: currency,
                amount,
                memo: a.memo.clone(),
            },
            &options,
        )?
    };
    drop(secrets.passphrase.take().map(Zeroizing::new));
    // Checked here so that nothing is sent before the transfer is confirmed.
    if !tx.fee_payer_covered(&state.balances) {
        return Err(if currency == Address::default() {
            Error::new(
                "insufficient_funds",
                "insufficient funds: not enough MMX for the amount and the fee",
            )
        } else {
            Error::new(
                "insufficient_fee",
                "insufficient funds: not enough MMX to pay the transaction fee",
            )
        });
    }
    // A simple transfer is charged exactly its static fee.
    let fee = tx.fee_amount();
    let bytes = serde_json::to_vec(&tx).expect("transaction serialization");
    if let Some(path) = &a.transaction {
        storage::save(path, &bytes, Some("transaction_exists"))?;
    }
    if !a.json {
        println!("Amount: {} {symbol}\nTarget: {target}\nFee: {} MMX\nExpires: {} (current height {})\nTransaction ID: {}", format_amount(amount, decimals)?, format_amount(fee, params.decimals)?, tx.expires, state.height, tx.id_hex());
    }
    let broadcast = a.yes || (!a.non_interactive() && confirm()?);
    if broadcast {
        // Only the confirmed transfer reaches the server. Its dry run stops
        // one that would fail (the node would still include it and charge
        // the fee) or cost more than shown.
        rpc.validate(&bytes, fee)?;
        rpc.post("/transaction/broadcast", &bytes)?;
    }
    if a.json {
        let mut result = json!({"status":if broadcast {"broadcast"} else {"prepared"},"transaction_id":tx.id_hex(),"amount":format_amount(amount,decimals)?,"amount_atomic":amount.to_string(),"currency":symbol,"currency_address":currency,"target":target,"fee":format_amount(fee,params.decimals)?,"fee_atomic":fee.to_string(),"max_fee_atomic":tx.max_fee_amount.to_string(),"network":params.network,"transaction_file":a.transaction.as_ref().map(|p|p.to_string_lossy().into_owned()).unwrap_or_default(),"decimals":decimals,"expires_height":tx.expires,"current_height":state.height,"broadcast":broadcast});
        if let Some(memo) = &a.memo {
            result["memo"] = json!(memo);
        }
        print_json(&a.command, result);
    } else {
        println!(
            "{}",
            if broadcast {
                "Transaction broadcast successfully."
            } else {
                "Transaction not broadcast."
            }
        );
    }
    Ok(())
}
fn confirm() -> Result<bool> {
    print!("Broadcast transaction? (y/N): ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y"))
}
fn report(command: &str, json_mode: bool, e: Error) {
    if json_mode {
        eprintln!(
            "{}",
            json!({"schema_version":1,"command":command,"status":"error","code":e.code,"error":e.message})
        );
    } else {
        eprintln!("Error: {e}");
    }
}
pub fn main_from(raw: Vec<String>) -> i32 {
    let json_mode = raw.iter().any(|a| a == "--json");
    let args = match Args::try_parse_from(&raw) {
        Ok(a) => a,
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            print!("{e}");
            return 0;
        }
        Err(_) => {
            let command = raw
                .get(1)
                .filter(|s| !s.starts_with('-'))
                .map(String::as_str)
                .unwrap_or("");
            report(
                command,
                json_mode,
                invalid("invalid command line arguments; use --help"),
            );
            return 1;
        }
    };
    if let Err(e) = run(&args) {
        report(&args.command, args.json, e);
        return 1;
    }
    0
}

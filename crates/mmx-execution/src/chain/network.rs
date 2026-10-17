//! Load the original network configuration and genesis files. File loading is
//! read-only; `genesis::build` validates the resulting bootstrap before use.
mod text;

use super::genesis::Assets;
use mmx_consensus::Record;
use mmx_types::mmx;
use std::{fs::File, io::Read, path::Path};
use vnx::config::Config;

/// Native `Node::init_chain` order, independent of directory enumeration.
pub const BINARY_FILES: [&str; 9] = [
    "tx_offer_binary.dat",
    "tx_swap_binary.dat",
    "tx_token_binary.dat",
    "tx_plot_nft_binary.dat",
    "tx_nft_binary.dat",
    "tx_template_binary.dat",
    "tx_escrow_binary.dat",
    "tx_time_lock_binary.dat",
    "tx_relay_binary.dat",
];

#[derive(Debug, thiserror::Error)]
#[error("network assets: {context}: {source}")]
pub struct Error {
    context: String,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}
pub type Result<T> = std::result::Result<T, Error>;
fn error(
    context: impl Into<String>,
    source: impl std::error::Error + Send + Sync + 'static,
) -> Error {
    Error {
        context: context.into(),
        source: Box::new(source),
    }
}
fn invalid(message: impl Into<String>) -> vnx::Error {
    vnx::Error::Invalid(message.into())
}

#[derive(Clone, Debug)]
pub struct Network {
    pub params: mmx::ChainParams,
    pub assets: Assets,
}
impl Network {
    /// Read `<root>/config/<configuration>` with VNX parent inheritance and
    /// overrides, then `<root>/data/<params.network>`. The directory name may
    /// differ from the configured network, as with native config overlays.
    pub fn load(root: impl AsRef<Path>, configuration: impl AsRef<Path>) -> Result<Self> {
        let config = Config::new();
        let path = root.as_ref().join("config").join(configuration);
        config
            .load_tree(&path)
            .map_err(|e| error(path.display().to_string(), e))?;
        Self::from_config(&config, root.as_ref().join("data"))
    }
    /// Use an already layered VNX configuration, including caller overrides.
    pub fn from_config(config: &Config, data_root: impl AsRef<Path>) -> Result<Self> {
        let params = load_params(config)?;
        let assets = load_assets(data_root.as_ref().join(&params.network), &params.network)?;
        Ok(Self { params, assets })
    }
}

pub fn load_params(config: &Config) -> Result<mmx::ChainParams> {
    let value = config
        .get("chain.params")
        .map_err(|e| error("chain.params", e))?
        .ok_or_else(|| error("chain.params", invalid("missing chain parameters")))?;
    text::params(value).map_err(|e| error("chain.params", e))
}

/// Read one network's data directory. Mainnet alone requires the rewards JSON
/// and project relay; other networks read exactly the nine binary transactions.
pub fn load_assets(directory: impl AsRef<Path>, network: &str) -> Result<Assets> {
    let dir = directory.as_ref();
    let mut binaries = Vec::with_capacity(BINARY_FILES.len());
    for name in BINARY_FILES {
        binaries.push(read_transaction(&dir.join(name))?);
    }
    let (testnet_rewards, project_relay) = if network == "mainnet" {
        let path = dir.join("tx_testnet_rewards.json");
        let data = read_bounded(&path)?;
        let text = std::str::from_utf8(&data).map_err(|e| error(path.display().to_string(), e))?;
        let value =
            vnx::config::parse_json(text).map_err(|e| error(path.display().to_string(), e))?;
        let rewards = text::rewards(value).map_err(|e| error(path.display().to_string(), e))?;
        (
            Some(rewards),
            Some(read_transaction(&dir.join("tx_project_relay.dat"))?),
        )
    } else {
        (None, None)
    };
    Ok(Assets {
        binaries: binaries.try_into().expect("nine bootstrap binaries"),
        testnet_rewards,
        project_relay,
    })
}

fn read_transaction(path: &Path) -> Result<Record> {
    let tx =
        Record::decode(&read_bounded(path)?).map_err(|e| error(path.display().to_string(), e))?;
    if tx.name != "mmx.Transaction" {
        return Err(error(
            path.display().to_string(),
            invalid("expected mmx.Transaction"),
        ));
    }
    Ok(tx)
}
fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    // The journal and VNX runtime have the same 64 MiB operational limit. Do
    // not allocate an unbounded file or rely on metadata (the file can grow).
    const LIMIT: u64 = 64 * 1024 * 1024;
    let read = || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = read().map_err(|e| error(path.display().to_string(), e))?;
    if bytes.len() as u64 > LIMIT {
        return Err(error(
            path.display().to_string(),
            invalid("asset file exceeds 64 MiB"),
        ));
    }
    Ok(bytes)
}

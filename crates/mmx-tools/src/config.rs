//! Existing VNX configuration layers, with explicit CLI options taking priority.
use crate::{err, Result};
use clap::{parser::ValueSource, ArgMatches, Args};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use vnx::config::Config;

#[derive(Args, Default)]
pub struct Layers {
    /// VNX configuration directories in overlay order, as with the C++ tools.
    #[arg(short='c',long="config",num_args=1..)]
    pub paths: Vec<PathBuf>,
    /// Wallet/config home; defaults to MMX_HOME or the current directory.
    #[arg(long)]
    pub home: Option<PathBuf>,
}
impl Layers {
    pub fn load(&self) -> Result<Config> {
        let config = Config::new();
        for path in &self.paths {
            config.load_tree(path)?;
        }
        Ok(config)
    }
    pub fn home(&self) -> PathBuf {
        self.home
            .clone()
            .or_else(|| std::env::var_os("MMX_HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn relative(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.home().join(path)
        }
    }
}
pub fn apply<T: DeserializeOwned>(
    matches: &ArgMatches,
    config: &Config,
    id: &str,
    key: &str,
    value: &mut T,
) -> Result<()> {
    if matches.value_source(id) != Some(ValueSource::CommandLine) {
        if let Some(v) = config.read(key)? {
            *value = v;
        }
    }
    Ok(())
}
pub fn endpoint(host: &str, port: u16) -> Result<String> {
    let host = if host == "localhost" || host.is_empty() {
        "127.0.0.1"
    } else {
        host
    };
    let ip: std::net::IpAddr = host
        .parse()
        .map_err(|_| err("listener endpoint must be an explicit IP address"))?;
    Ok(std::net::SocketAddr::new(ip, port).to_string())
}

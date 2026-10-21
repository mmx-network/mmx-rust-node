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
    /// Installation directory containing config/ and data/ (independent of MMX_HOME).
    #[arg(long)]
    pub mmx_root: Option<PathBuf>,
    /// Select a network instead of reading MMX_HOME/NETWORK.
    #[arg(long)]
    pub network: Option<String>,
}
impl Layers {
    pub fn load(&self, role: &str) -> Result<(Config, mmx_config::Layout)> {
        let layout = mmx_config::Layout::from_env(self.home.as_deref(), self.network.as_deref())?;
        let config = Config::new();
        let paths = if self.paths.is_empty() {
            layout.config_paths(
                &self
                    .mmx_root
                    .clone()
                    .unwrap_or_else(mmx_config::installation_root),
                role,
            )
        } else {
            self.paths.clone()
        };
        for path in &paths {
            config.load_tree(path)?;
        }
        Ok((config, layout))
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

pub fn remote_endpoint(value: &str, port: u16) -> String {
    if value.starts_with("unix://") || value.ends_with(".sock") {
        return value.into();
    }
    let raw = value.strip_prefix("tcp://").unwrap_or(value);
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return std::net::SocketAddr::new(ip, port).to_string();
    }
    if raw.starts_with(':') {
        return format!("127.0.0.1{raw}");
    }
    if !raw.contains(':') || raw.starts_with('[') && raw.ends_with(']') {
        return format!("{raw}:{port}");
    }
    raw.into()
}

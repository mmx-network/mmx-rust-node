//! Paths used by C++ activate.sh / run_*.sh and module startup.
//! Resolution reads only small selector files; it never initializes or migrates data.
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Layout {
    pub home: PathBuf,
    pub network: String,
    pub network_dir: PathBuf,
}

#[derive(Default)]
struct Environment {
    home: Option<OsString>,
    data: Option<OsString>,
    network_dir: Option<OsString>,
}

fn nonempty(value: Option<OsString>) -> Option<OsString> {
    value.filter(|v| !v.is_empty())
}
fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
fn selector(path: &Path) -> io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(io::Error::other(format!(
            "selector {} exceeds 64 KiB",
            path.display()
        )));
    }
    let value = String::from_utf8(bytes).map_err(io::Error::other)?;
    // Shell command substitution strips trailing newlines. Also accept CRLF.
    let value = value.trim_end_matches(['\r', '\n']);
    if value.contains(['\0', '\r', '\n']) {
        return Err(io::Error::other(format!(
            "invalid selector {}",
            path.display()
        )));
    }
    Ok(Some(value.to_owned()))
}

impl Layout {
    pub fn from_env(home: Option<&Path>, network: Option<&str>) -> io::Result<Self> {
        Self::resolve(
            &std::env::current_dir()?,
            home,
            network,
            Environment {
                home: std::env::var_os("MMX_HOME"),
                data: std::env::var_os("MMX_DATA"),
                network_dir: std::env::var_os("MMX_NETWORK"),
            },
        )
    }
    fn resolve(
        cwd: &Path,
        home: Option<&Path>,
        network: Option<&str>,
        env: Environment,
    ) -> io::Result<Self> {
        let home = absolute(
            cwd,
            home.map(Path::to_owned)
                .or_else(|| nonempty(env.home).map(PathBuf::from))
                .as_deref()
                .unwrap_or(cwd),
        );
        let network = match network {
            Some(value) => value.to_owned(),
            None => selector(&home.join("NETWORK"))?.unwrap_or_else(|| "mainnet".into()),
        };
        let mut parts = Path::new(&network).components();
        if !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
            || network.contains(['/', '\\', '\0', '\r', '\n'])
        {
            return Err(io::Error::other("network must be a single directory name"));
        }
        let network_dir = if let Some(path) = nonempty(env.network_dir) {
            absolute(cwd, Path::new(&path))
        } else {
            // activate.sh gives a present MMX_DATA file precedence over the env.
            let data = selector(&home.join("config/local/MMX_DATA"))?
                .map(OsString::from)
                .or(env.data);
            let data = nonempty(data)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.clone());
            absolute(cwd, &data).join(&network)
        };
        Ok(Self {
            home,
            network,
            network_dir,
        })
    }
    /// Default run_*.sh layer order. Explicit -c lists replace this list.
    /// Missing optional directories are not created or populated with secrets.
    pub fn config_paths(&self, root: &Path, role: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for path in [
            root.join("config").join(&self.network),
            root.join("config").join(role),
            self.home.join("config/local"),
        ] {
            if path.is_dir() && !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }
    pub fn local_config(&self) -> PathBuf {
        self.home.join("config/local")
    }
    pub fn wallet_path(&self, storage: &Path, key: &Path) -> PathBuf {
        absolute(&absolute(&self.home, storage), key)
    }
    pub fn network_path(&self, path: &Path) -> PathBuf {
        absolute(&self.network_dir, path)
    }
}

pub fn installation_root() -> PathBuf {
    if Path::new("config").is_dir() && Path::new("data").is_dir() {
        ".".into()
    } else {
        "../mmx-node".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn native_selectors_and_environment_precedence_with_spaces_and_no_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let home = cwd.join("mmx home");
        fs::create_dir_all(home.join("config/local")).unwrap();
        fs::write(home.join("NETWORK"), "testnet12\r\n").unwrap();
        fs::write(home.join("config/local/MMX_DATA"), "file data\n").unwrap();
        let env = || Environment {
            home: Some("mmx home".into()),
            data: Some("env data".into()),
            network_dir: None,
        };
        let paths = Layout::resolve(cwd, None, None, env()).unwrap();
        assert_eq!(paths.home, home);
        assert_eq!(paths.network, "testnet12");
        assert_eq!(paths.network_dir, cwd.join("file data/testnet12"));
        fs::remove_file(home.join("config/local/MMX_DATA")).unwrap();
        assert_eq!(
            Layout::resolve(cwd, None, None, env()).unwrap().network_dir,
            cwd.join("env data/testnet12")
        );
        let mut override_env = env();
        override_env.network_dir = Some("network override".into());
        let paths = Layout::resolve(cwd, None, Some("mainnet"), override_env).unwrap();
        assert_eq!(paths.network, "mainnet");
        assert_eq!(paths.network_dir, cwd.join("network override"));
        let paths = Layout::resolve(cwd, Some(cwd), None, env()).unwrap();
        assert_eq!(paths.home, cwd);
        assert_eq!(paths.network, "mainnet");
        fs::write(home.join("config/local/MMX_DATA"), "\n").unwrap();
        assert_eq!(
            Layout::resolve(cwd, None, None, env()).unwrap().network_dir,
            home.join("testnet12")
        );
    }

    #[test]
    fn defaults_layers_and_absolute_storage_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let paths = Layout::resolve(cwd, None, None, Environment::default()).unwrap();
        assert_eq!(paths.home, cwd);
        assert_eq!(paths.network_dir, cwd.join("mainnet"));
        let root = cwd.join("installation");
        let expected = [
            root.join("config/mainnet"),
            root.join("config/node"),
            cwd.join("config/local"),
        ];
        for p in &expected {
            fs::create_dir_all(p).unwrap();
        }
        assert_eq!(paths.config_paths(&root, "node"), expected);
        assert_eq!(
            paths.wallet_path(Path::new("keys"), Path::new("a.dat")),
            cwd.join("keys/a.dat")
        );
        let explicit = cwd.join("separate");
        assert_eq!(paths.wallet_path(Path::new("keys"), &explicit), explicit);
        assert_eq!(paths.network_path(&explicit), explicit);
        assert_eq!(paths.network_path(Path::new("db")), cwd.join("mainnet/db"));
        assert!(!cwd.join("NETWORK").exists());
    }

    #[test]
    fn malformed_selectors_fail_before_any_data_is_created() {
        let dir = tempfile::tempdir().unwrap();
        for value in [
            "",
            "..",
            "../mainnet",
            "/mainnet",
            "foo/bar",
            "mainnet\nother",
            "mainnet\\other",
        ] {
            fs::write(dir.path().join("NETWORK"), value).unwrap();
            assert!(
                Layout::resolve(dir.path(), None, None, Environment::default()).is_err(),
                "{value:?}"
            );
        }
        fs::write(dir.path().join("NETWORK"), vec![b'x'; 65537]).unwrap();
        assert!(Layout::resolve(dir.path(), None, None, Environment::default()).is_err());
        // Explicit CLI selection bypasses a bad selector.
        assert!(Layout::resolve(dir.path(), None, Some("mainnet"), Environment::default()).is_ok());
        assert!(!dir.path().join("mainnet").exists());
    }
}

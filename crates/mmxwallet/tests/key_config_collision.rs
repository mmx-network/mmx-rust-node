use mmx_wallet::{seed_to_words, KeyFile};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Fixture {
    root: tempfile::TempDir,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("selected");
        fs::create_dir(&directory).unwrap();
        Self { root, directory }
    }

    fn run(
        &self,
        command: &str,
        path: Option<&Path>,
        account: u32,
        input: Value,
        protected: bool,
    ) -> Output {
        let mut process = Command::new(env!("CARGO_BIN_EXE_mmxwallet"));
        process
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("USERPROFILE", self.root.path().join("home"))
            .env("MMX_HOME", self.root.path().join("managed"))
            .current_dir(self.root.path())
            .arg(command)
            .args([
                "--account",
                &account.to_string(),
                "--num-addresses",
                "2",
                "--json",
                "--input-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            process.arg("--file").arg(path);
        }
        if protected {
            process.arg("--with-passphrase");
        }
        let mut child = process.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        serde_json::to_writer(&mut stdin, &input).unwrap();
        stdin.write_all(b"\n").unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    }

    fn assert_reads(&self, path: &Path, key: &KeyFile, passphrase: Option<&str>) {
        let before = snapshot(&self.directory);
        for account in [0, 1] {
            let wallet = key.unlock(passphrase.unwrap_or(""), account, 2).unwrap();
            let expected: Vec<_> = wallet.addresses.iter().map(ToString::to_string).collect();
            for _ in 0..2 {
                if passphrase.is_some() {
                    for (input, code) in [
                        (json!({}), "passphrase_required"),
                        (json!({"passphrase":"wrong"}), "invalid_passphrase"),
                    ] {
                        let output = self.run("addresses", Some(path), account, input, false);
                        assert!(!output.status.success(), "credentials accepted");
                        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
                        assert_eq!(error["code"], code);
                        assert!(
                            snapshot(&self.directory) == before,
                            "files changed after credential failure"
                        );
                    }
                }
                let output = self.run(
                    "addresses",
                    Some(path),
                    account,
                    json!({"passphrase":passphrase.unwrap_or("")}),
                    false,
                );
                assert!(output.status.success(), "address command failed");
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["addresses"], json!(expected));
                assert!(
                    snapshot(&self.directory) == before,
                    "files changed after address read"
                );
            }
        }
        let output = self.run("mnemonic", Some(path), 0, json!({}), false);
        assert!(output.status.success(), "fingerprint command failed");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["fingerprint"], key.fingerprint());
        assert!(
            snapshot(&self.directory) == before,
            "files changed after fingerprint read"
        );
    }
}

#[derive(PartialEq)]
struct FileState {
    path: PathBuf,
    bytes: Vec<u8>,
    link: Option<PathBuf>,
    #[cfg(unix)]
    identity: (u64, u64, u32, u64),
}

fn snapshot(directory: &Path) -> Vec<FileState> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                let meta = fs::symlink_metadata(&path).unwrap();
                (meta.dev(), meta.ino(), meta.mode(), meta.nlink())
            };
            FileState {
                bytes: fs::read(&path).unwrap(),
                link: fs::read_link(&path).ok(),
                path,
                #[cfg(unix)]
                identity,
            }
        })
        .collect()
}

fn write_key(path: &Path, key: &KeyFile) {
    fs::write(path, serde_json::to_vec(key).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
    }
}

#[test]
fn json_configuration_named_keys_support_repeated_authenticated_reads() {
    for passphrase in [None, Some("collision-test")] {
        let fixture = Fixture::new();
        let path = fixture.directory.join("mmxwallet.json");
        let key = KeyFile::generate(passphrase);
        write_key(&path, &key);
        fixture.assert_reads(&path, &key, passphrase);
    }
}

#[test]
fn relative_aliases_preserve_configuration_named_keys() {
    for passphrase in [None, Some("relative-test")] {
        let fixture = Fixture::new();
        let key = KeyFile::generate(passphrase);
        write_key(&fixture.directory.join("mmxwallet.json"), &key);
        fixture.assert_reads(
            Path::new("selected/../selected/mmxwallet.json"),
            &key,
            passphrase,
        );
    }
}

#[cfg(unix)]
#[test]
fn key_and_configuration_links_preserve_content_and_identity() {
    use std::os::unix::fs::symlink;
    for passphrase in [None, Some("link-test")] {
        for kind in [
            "selected-symlink",
            "configuration-symlink",
            "configuration-hardlink",
            "other-key",
        ] {
            let fixture = Fixture::new();
            let key = KeyFile::generate(passphrase);
            let selected = fixture.directory.join("key.json");
            let config = fixture.directory.join("mmxwallet.json");
            match kind {
                "selected-symlink" => {
                    write_key(&config, &key);
                    symlink("mmxwallet.json", &selected).unwrap();
                }
                "configuration-symlink" => {
                    write_key(&selected, &key);
                    symlink("key.json", &config).unwrap();
                }
                "configuration-hardlink" => {
                    write_key(&selected, &key);
                    fs::hard_link(&selected, &config).unwrap();
                }
                "other-key" => {
                    write_key(&selected, &key);
                    write_key(&config, &KeyFile::generate(Some("other-key-test")));
                }
                _ => unreachable!(),
            }
            fixture.assert_reads(&selected, &key, passphrase);
        }
    }
}

#[test]
fn explicit_create_and_import_preserve_new_keys_and_adjacent_keys() {
    for passphrase in [None, Some("create-import-test")] {
        for command in ["create", "import"] {
            for adjacent_key in [false, true] {
                let fixture = Fixture::new();
                let path = fixture.directory.join(if adjacent_key {
                    "key.json"
                } else {
                    "mmxwallet.json"
                });
                let other = fixture.directory.join("mmxwallet.json");
                let other_before = if adjacent_key {
                    write_key(&other, &KeyFile::generate(None));
                    Some(fs::read(&other).unwrap())
                } else {
                    None
                };
                let imported = KeyFile::generate(passphrase);
                let output = fixture.run(
                    command,
                    Some(&path),
                    0,
                    json!({
                        "mnemonic": seed_to_words(&imported.seed_value),
                        "passphrase": passphrase.unwrap_or("")
                    }),
                    passphrase.is_some(),
                );
                assert!(output.status.success(), "key creation failed");
                let created: Value = serde_json::from_slice(&output.stdout).unwrap();
                let key = KeyFile::decode(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(created["fingerprint"], key.fingerprint());
                if command == "import" {
                    assert_eq!(key.fingerprint(), imported.fingerprint());
                }
                fixture.assert_reads(&path, &key, passphrase);
                if let Some(before) = other_before {
                    assert!(fs::read(&other).unwrap() == before, "adjacent key changed");
                }
                let before = snapshot(&fixture.directory);
                let output = fixture.run(
                    command,
                    Some(&path),
                    0,
                    json!({
                        "mnemonic": seed_to_words(&imported.seed_value),
                        "passphrase": passphrase.unwrap_or("")
                    }),
                    passphrase.is_some(),
                );
                assert!(!output.status.success(), "existing key replaced");
                let error: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["code"], "wallet_exists");
                assert!(
                    snapshot(&fixture.directory) == before,
                    "existing files changed"
                );
            }
        }
    }
}

#[test]
fn managed_creation_still_requires_valid_settings_and_sets_active_wallet() {
    let fixture = Fixture::new();
    let managed = fixture.root.path().join("managed");
    fs::create_dir(&managed).unwrap();
    let config = managed.join("mmxwallet.json");
    write_key(&config, &KeyFile::generate(None));
    let before = snapshot(&managed);
    for command in ["create", "import"] {
        let key = KeyFile::generate(None);
        let output = fixture.run(
            command,
            None,
            0,
            json!({"mnemonic": seed_to_words(&key.seed_value)}),
            false,
        );
        assert!(!output.status.success(), "invalid configuration accepted");
        assert!(snapshot(&managed) == before, "managed directory changed");
    }
    fs::write(&config, b"{\"extra\":true}").unwrap();
    let output = fixture.run("create", None, 0, json!({}), false);
    assert!(output.status.success(), "managed creation failed");
    let created: Value = serde_json::from_slice(&output.stdout).unwrap();
    let settings: Value = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    let path = Path::new(created["wallet_file"].as_str().unwrap());
    assert_eq!(
        settings["active_wallet"],
        path.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(settings["extra"], true);
}

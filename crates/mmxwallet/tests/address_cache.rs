use mmx_wallet::KeyFile;
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct Fixture {
    root: tempfile::TempDir,
    directory: PathBuf,
    key: PathBuf,
    binary: PathBuf,
    key_bytes: Vec<u8>,
    addresses: Vec<String>,
    fingerprint: String,
}

impl Fixture {
    fn new(passphrase: Option<&str>) -> Self {
        #[cfg(unix)]
        let root = if unsafe { libc::geteuid() } == 0 {
            tempfile::tempdir_in("/tmp").unwrap()
        } else {
            tempfile::tempdir().unwrap()
        };
        #[cfg(not(unix))]
        let root = tempfile::tempdir().unwrap();
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_mmxwallet"));
        #[cfg(unix)]
        let binary = if unsafe { libc::geteuid() } == 0 {
            use std::os::unix::fs::PermissionsExt;

            // The build directory itself may be below a root-only parent.
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let executable = root.path().join("mmxwallet");
            fs::copy(&binary, &executable).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            executable
        } else {
            binary
        };
        let directory = root.path().join("selected");
        fs::create_dir(&directory).unwrap();
        let key = directory.join("wallet.dat");
        let generated = KeyFile::generate(passphrase);
        let wallet = generated.unlock(passphrase.unwrap_or(""), 0, 2).unwrap();
        let addresses = wallet.addresses.iter().map(ToString::to_string).collect();
        let key_bytes = generated.encode();
        fs::write(&key, &key_bytes).unwrap();
        Self {
            root,
            directory,
            key,
            binary,
            key_bytes,
            addresses,
            fingerprint: generated.fingerprint(),
        }
    }

    fn config(&self) -> PathBuf {
        self.directory.join("mmxwallet.json")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("USERPROFILE", self.root.path().join("home"))
            .env("MMX_HOME", self.root.path().join("home/mmx"))
            .args(["addresses", "--file"])
            .arg(&self.key)
            .args(["--account", "0", "--num-addresses", "2", "--json"])
            .stdin(Stdio::null());
        command
    }

    fn assert_addresses(&self, output: &Output) {
        assert!(output.status.success(), "address command failed");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "ok");
        assert_eq!(value["addresses"], json!(self.addresses));
        self.assert_key_unchanged();
    }

    fn assert_key_unchanged(&self) {
        // Do not include key bytes in assertion diagnostics.
        assert!(
            fs::read(&self.key).unwrap() == self.key_bytes,
            "key changed"
        );
    }
}

fn run(mut command: Command, passphrase: Option<&str>) -> Output {
    if let Some(passphrase) = passphrase {
        let mut child = command
            .arg("--input-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        serde_json::to_writer(&mut input, &json!({"passphrase": passphrase})).unwrap();
        input.write_all(b"\n").unwrap();
        drop(input);
        child.wait_with_output().unwrap()
    } else {
        command.output().unwrap()
    }
}

#[cfg(unix)]
fn ensure_nonroot(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    if unsafe { libc::geteuid() } == 0 {
        // Only the wallet child drops credentials. The parent retains ownership
        // of the disposable fixture so it can restore permissions on failure.
        unsafe {
            command.pre_exec(|| {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(65534) != 0
                    || libc::setuid(65534) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::geteuid() != 65534 || libc::getegid() != 65534 {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                Ok(())
            });
        }
    }
}

#[cfg(unix)]
struct RestorePermissions(PathBuf, fs::Permissions);

#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, self.1.clone());
    }
}

#[cfg(unix)]
#[test]
fn first_address_read_with_read_only_parent() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(None);
    if unsafe { libc::geteuid() } == 0 {
        // A dropped child can traverse only this owned fixture and read its key.
        fs::set_permissions(&fixture.key, fs::Permissions::from_mode(0o444)).unwrap();
    }
    let key_mode = fs::metadata(&fixture.key).unwrap().permissions().mode();
    let _restore = RestorePermissions(
        fixture.directory.clone(),
        fs::metadata(&fixture.directory).unwrap().permissions(),
    );
    fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o555)).unwrap();
    let mut command = fixture.command();
    ensure_nonroot(&mut command);
    fixture.assert_addresses(&run(command, None));
    assert!(!fixture.config().exists());
    assert_eq!(
        fs::metadata(&fixture.directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(&fixture.key).unwrap().permissions().mode(),
        key_mode
    );
}

#[test]
fn malformed_adjacent_settings_are_preserved() {
    let fixture = Fixture::new(None);
    let damaged = b"{ damaged settings\n";
    fs::write(fixture.config(), damaged).unwrap();
    fixture.assert_addresses(&run(fixture.command(), None));
    assert_eq!(fs::read(fixture.config()).unwrap(), damaged);
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 2);
}

#[test]
fn healthy_cache_is_populated_and_usable_without_unlocking() {
    let passphrase = "address-cache-test";
    let fixture = Fixture::new(Some(passphrase));
    for (input, code) in [
        (None, "passphrase_required"),
        (Some("wrong"), "invalid_passphrase"),
    ] {
        let output = run(fixture.command(), input);
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["code"], code);
        assert!(!fixture.config().exists());
        fixture.assert_key_unchanged();
    }
    fixture.assert_addresses(&run(fixture.command(), Some(passphrase)));
    let bytes = fs::read(fixture.config()).unwrap();
    let config: Value = serde_json::from_slice(&bytes).unwrap();
    let cache = &config["wallets"]["wallet.dat"]["public_addresses"];
    assert_eq!(cache["fingerprint"], fixture.fingerprint);
    assert_eq!(cache["addresses"], json!(fixture.addresses));
    fixture.assert_addresses(&run(fixture.command(), None));
    assert_eq!(fs::read(fixture.config()).unwrap(), bytes);
}

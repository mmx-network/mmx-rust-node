use mmxwallet::service::Service;
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn desktop_storage_ignores_node_home() {
    if std::env::var_os("MMX_DESKTOP_STORAGE_CHILD").is_some() {
        let service = Service::open_desktop().unwrap();
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
        assert_eq!(
            service.directory(),
            std::path::PathBuf::from(home).join(".mmx")
        );
        assert!(
            !service.directory().exists(),
            "opening must not create storage"
        );
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let node = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "desktop_storage_ignores_node_home"])
        .env("MMX_DESKTOP_STORAGE_CHILD", "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("MMX_HOME", node.path())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!home.path().join(".mmx").exists());
    assert_eq!(std::fs::read_dir(node.path()).unwrap().count(), 0);
}

#[test]
fn installed_cli_creates_wallets_the_desktop_service_can_open() {
    let dir = tempfile::tempdir().unwrap();
    let payload: serde_json::Value = serde_json::from_str(include_str!("payloads.json")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mmxwallet"))
        .args(["import", "--input-stdin", "--json", "--num-addresses", "3"])
        .env("MMX_HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    serde_json::to_writer(
        child.stdin.as_mut().unwrap(),
        &serde_json::json!({"mnemonic": payload["import_wallet"]["mnemonic"]}),
    )
    .unwrap();
    child.stdin.take().unwrap().flush().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "CLI import failed");
    let service = Service::open(Some(dir.path().into())).unwrap();
    let wallets = service.wallets().unwrap();
    assert_eq!(wallets.len(), 1);
    assert!(!wallets[0].locked);
    assert_eq!(service.addresses(&wallets[0].id).unwrap().len(), 3);
}

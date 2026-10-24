// The window may call only the app's own commands: each gets a generated
// permission (`allow-<command>`), granted in capabilities/default.json.
include!("src/commands.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/commands.rs");
    #[cfg(feature = "app")]
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to prepare the Tauri application");
    #[cfg(not(feature = "app"))]
    let _ = COMMANDS;
}

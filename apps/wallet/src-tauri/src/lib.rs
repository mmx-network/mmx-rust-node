//! MMX Wallet desktop application: the shared wallet UI in a native window,
//! backed by [`mmxwallet::service::Service`] (the same wallet directory, key
//! files and `mmxwallet.json` as the command line wallet and the node).
//!
//! The UI calls one command per backend operation. Each command takes one
//! JSON object, decoded strictly (see [`args`]), and runs on a blocking
//! thread. Errors reach the UI as `{code, message}` with the wallet UI's
//! error codes. The window may only call these commands; it has no direct
//! access to the clipboard, files or other system services.
#![cfg_attr(not(feature = "app"), allow(dead_code))]

mod args;
mod clipboard;
mod commands;
mod files;
mod links;

#[cfg(feature = "app")]
pub use desktop::run;

pub use commands::COMMANDS;

#[cfg(feature = "app")]
mod desktop {
    use crate::{args, clipboard, files, links};
    use mmxwallet::service::{AppState, Error, PreparedSend, Result, Service, WalletInfo};
    use serde::{de::DeserializeOwned, Serialize};
    use serde_json::Value;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };
    use tauri::{
        ipc::{InvokeBody, Request},
        plugin::TauriPlugin,
        webview::PageLoadEvent,
        AppHandle, Manager, RunEvent, State, Url, Window, Wry,
    };
    use tauri_plugin_clipboard_manager::ClipboardExt;
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_window_state::StateFlags;
    use zeroize::Zeroizing;

    const MAIN_WINDOW: &str = "main";
    /// The window is shown once the page has loaded (no blank flash), or
    /// after this delay at the latest.
    const SHOW_AT_THE_LATEST: Duration = Duration::from_secs(5);

    struct Session {
        /// The wallet service, or why the wallet directory is unavailable.
        service: std::result::Result<Arc<Service>, Error>,
        clipboard: Arc<clipboard::Guard>,
        /// A file dialog is open (only one at a time).
        dialog_open: Arc<AtomicBool>,
    }

    impl Session {
        fn service(&self) -> Result<Arc<Service>> {
            self.service.clone()
        }
    }

    fn internal() -> Error {
        Error::new("internal", "request failed")
    }

    /// Runs blocking work (disk, network, dialogs) off the async runtime.
    async fn blocking<T, F>(work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        tauri::async_runtime::spawn_blocking(work)
            .await
            .unwrap_or_else(|_| Err(internal()))
    }

    fn decode<T: DeserializeOwned>(request: &Request<'_>) -> Result<T> {
        match request.body() {
            InvokeBody::Json(body) => args::parse(body),
            InvokeBody::Raw(_) => Err(args::invalid_request()),
        }
    }

    // -- wallets and settings

    #[tauri::command]
    async fn load_app_state(session: State<'_, Session>) -> Result<AppState> {
        let service = session.service()?;
        blocking(move || service.app_state()).await
    }

    #[tauri::command]
    async fn save_app_state(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::SaveState { state } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.set_app_state(state)).await
    }

    #[tauri::command]
    async fn list_wallets(session: State<'_, Session>) -> Result<Vec<WalletInfo>> {
        let service = session.service()?;
        blocking(move || service.wallets()).await
    }

    #[derive(Serialize)]
    struct Created {
        wallet: WalletInfo,
        mnemonic: Zeroizing<String>,
    }

    #[tauri::command]
    async fn create_wallet(session: State<'_, Session>, request: Request<'_>) -> Result<Created> {
        let args::Create {
            name,
            passphrase,
            address_count,
        } = decode(&request)?;
        let service = session.service()?;
        blocking(move || {
            let (wallet, mnemonic) = service.create(&name, passphrase, address_count)?;
            Ok(Created { wallet, mnemonic })
        })
        .await
    }

    #[tauri::command]
    async fn import_wallet(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<WalletInfo> {
        let args::Import {
            name,
            mnemonic,
            passphrase,
            address_count,
        } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.import(&name, mnemonic, passphrase, address_count)).await
    }

    #[tauri::command]
    async fn unlock_wallet(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<WalletInfo> {
        let args::Unlock { id, passphrase } = decode(&request)?;
        let passphrase = passphrase.unwrap_or_else(|| Zeroizing::new(String::new()));
        let service = session.service()?;
        blocking(move || service.unlock(&id, passphrase)).await
    }

    #[tauri::command]
    async fn lock_wallet(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Wallet { id } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.lock(&id)).await
    }

    #[tauri::command]
    async fn lock_all(session: State<'_, Session>) -> Result<()> {
        let service = session.service()?;
        blocking(move || {
            service.lock_all();
            Ok(())
        })
        .await
    }

    #[tauri::command]
    async fn rename_wallet(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Rename { id, name } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.rename(&id, &name)).await
    }

    #[tauri::command]
    async fn set_address_count(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<Vec<String>> {
        let args::AddressCount { id, count } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.set_address_count(&id, count)).await
    }

    #[tauri::command]
    async fn remove_wallet(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Wallet { id } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.remove(&id)).await
    }

    #[tauri::command]
    async fn reveal_mnemonic(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<Zeroizing<String>> {
        let args::Wallet { id } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.mnemonic(&id)).await
    }

    #[tauri::command]
    async fn get_addresses(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<Vec<String>> {
        let args::Wallet { id } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.addresses(&id)).await
    }

    #[tauri::command]
    async fn set_active_wallet(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Wallet { id } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.set_active(&id)).await
    }

    // -- chain data and transfers

    #[tauri::command]
    async fn rpc_get(session: State<'_, Session>, request: Request<'_>) -> Result<Value> {
        let args::RpcGet { path } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.rpc_get(&path)).await
    }

    #[tauri::command]
    async fn max_sendable(session: State<'_, Session>, request: Request<'_>) -> Result<String> {
        let args::MaxSend {
            wallet_id,
            contract,
            memo,
            fee_level,
            ..
        } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.max_send(&wallet_id, &contract, memo.as_deref(), fee_level)).await
    }

    #[tauri::command]
    async fn prepare_send(
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<PreparedSend> {
        let send: args::Send = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.prepare_send(&send.into())).await
    }

    #[tauri::command]
    async fn broadcast(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Broadcast { wallet_id, txid } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.broadcast(&wallet_id, &txid)).await
    }

    #[tauri::command]
    async fn discard_prepared(session: State<'_, Session>, request: Request<'_>) -> Result<()> {
        let args::Txid { txid } = decode(&request)?;
        let service = session.service()?;
        blocking(move || service.discard(&txid)).await
    }

    // -- system services

    struct SystemClipboard<'a>(&'a AppHandle);

    impl clipboard::Clipboard for SystemClipboard<'_> {
        fn write(&self, text: &str, sensitive: bool) -> Result<()> {
            let clipboard = self.0.clipboard();
            if sensitive {
                clipboard.write_secret(text)
            } else {
                clipboard.write_text(text)
            }
            .map_err(|_| internal())
        }

        fn read(&self) -> Option<Zeroizing<String>> {
            self.0.clipboard().read_text().ok().map(Zeroizing::new)
        }

        fn clear(&self) {
            let _ = self.0.clipboard().clear();
        }
    }

    #[tauri::command]
    async fn copy_text(
        app: AppHandle,
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<()> {
        let args::CopyText { text, sensitive } = decode(&request)?;
        let guard = Arc::clone(&session.clipboard);
        blocking(move || {
            let Some(ticket) = guard.copy(&SystemClipboard(&app), &text, sensitive)? else {
                return Ok(());
            };
            drop(text);
            let (timer_app, timer_guard) = (app.clone(), Arc::clone(&guard));
            let timer = thread::Builder::new()
                .name("clipboard-clear".into())
                .spawn(move || {
                    thread::sleep(clipboard::CLEAR_AFTER);
                    timer_guard.expire(&SystemClipboard(&timer_app), ticket);
                });
            if timer.is_err() {
                // Without a timer the words would stay on the clipboard.
                guard.expire(&SystemClipboard(&app), ticket);
                return Err(internal());
            }
            Ok(())
        })
        .await
    }

    #[tauri::command]
    async fn open_external(request: Request<'_>) -> Result<()> {
        let args::OpenExternal { url } = decode(&request)?;
        links::explorer_url(&url)?;
        blocking(move || tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|_| internal()))
            .await
    }

    /// Allows one file dialog at a time; released when dropped.
    struct DialogSlot(Arc<AtomicBool>);

    impl DialogSlot {
        fn take(flag: &Arc<AtomicBool>) -> Option<Self> {
            flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()
                .map(|_| Self(Arc::clone(flag)))
        }
    }

    impl Drop for DialogSlot {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    #[tauri::command]
    async fn export_file(
        window: Window,
        session: State<'_, Session>,
        request: Request<'_>,
    ) -> Result<bool> {
        let args::ExportFile {
            suggested_name,
            contents,
        } = decode(&request)?;
        files::check_export(&contents)?;
        let name = files::export_name(&suggested_name);
        let Some(slot) = DialogSlot::take(&session.dialog_open) else {
            return Ok(false);
        };
        blocking(move || {
            let _slot = slot;
            let chosen = window
                .dialog()
                .file()
                .set_file_name(name)
                .add_filter("JSON", &["json"])
                .set_parent(&window)
                .blocking_save_file();
            let Some(chosen) = chosen else {
                return Ok(false);
            };
            let path = chosen
                .into_path()
                .map_err(|_| Error::new("io_error", "the file cannot be saved there"))?;
            files::write_export(&path, &contents)?;
            Ok(true)
        })
        .await
    }

    #[tauri::command]
    async fn import_file(window: Window, session: State<'_, Session>) -> Result<Option<String>> {
        let Some(slot) = DialogSlot::take(&session.dialog_open) else {
            return Ok(None);
        };
        blocking(move || {
            let _slot = slot;
            let dialog = window.dialog().file().add_filter("JSON", &["json"]);
            // Like the dialog plugin's own open command: GTK file choosers
            // are not attached to the window.
            #[cfg(any(windows, target_os = "macos"))]
            let dialog = dialog.set_parent(&window);
            let Some(chosen) = dialog.blocking_pick_file() else {
                return Ok(None);
            };
            let path = chosen
                .into_path()
                .map_err(|_| Error::new("io_error", "the file cannot be read"))?;
            files::read_import(&path).map(Some)
        })
        .await
    }

    // -- application

    /// The page may only navigate within the app. A click on a block
    /// explorer link opens it in the browser instead.
    fn allow_navigation(app: &AppHandle, url: &Url) -> bool {
        let host = url.host_str();
        let local = match url.scheme() {
            "tauri" => host == Some("localhost") && url.port().is_none(),
            "http" | "https" => {
                cfg!(windows) && host == Some("tauri.localhost") && url.port().is_none()
            }
            _ => false,
        };
        let dev = tauri::is_dev()
            && app
                .config()
                .build
                .dev_url
                .as_ref()
                .is_some_and(|dev| dev.origin() == url.origin());
        if local || dev {
            return true;
        }
        if let Ok(link) = links::explorer_url(url.as_str()) {
            let _ = tauri_plugin_opener::open_url(link, None::<&str>);
        }
        false
    }

    /// Navigation rules and cleanup when the app quits. Registered before
    /// the clipboard plugin so the clipboard is still usable on exit.
    fn session_plugin() -> TauriPlugin<Wry> {
        tauri::plugin::Builder::new("wallet-session")
            .on_navigation(|webview, url| allow_navigation(webview.app_handle(), url))
            .on_event(|app, event| {
                if let RunEvent::Exit = event {
                    if let Some(session) = app.try_state::<Session>() {
                        // On Linux reading could wait on this (closing) app
                        // if it owns the clipboard, so a pending copy is
                        // cleared without checking it is still there.
                        let check = !cfg!(target_os = "linux");
                        session
                            .clipboard
                            .clear_on_exit(&SystemClipboard(app), check);
                        if let Ok(service) = &session.service {
                            service.lock_all();
                        }
                    }
                }
            })
            .build()
    }

    fn show_main(app: &AppHandle) {
        if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    /// WebKitGTK's DMA-BUF renderer shows a blank window with the
    /// proprietary NVIDIA driver; use its other renderer there.
    #[cfg(target_os = "linux")]
    fn prefer_compatible_rendering() {
        const VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
        if std::env::var_os(VARIABLE).is_none()
            && std::path::Path::new("/proc/driver/nvidia/version").exists()
        {
            // Runs first in main, before any other thread exists.
            std::env::set_var(VARIABLE, "1");
        }
    }

    pub fn run() {
        #[cfg(target_os = "linux")]
        prefer_compatible_rendering();

        let session = Session {
            service: Service::open(None).map(Arc::new),
            clipboard: Arc::default(),
            dialog_open: Arc::default(),
        };
        let result = tauri::Builder::default()
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                show_main(app)
            }))
            .plugin(session_plugin())
            .plugin(tauri_plugin_clipboard_manager::init())
            .plugin(tauri_plugin_dialog::init())
            .plugin(
                tauri_plugin_window_state::Builder::new()
                    .with_state_flags(
                        StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED,
                    )
                    .build(),
            )
            .manage(session)
            .on_page_load(|webview, payload| {
                if payload.event() == PageLoadEvent::Finished && webview.label() == MAIN_WINDOW {
                    let _ = webview.window().show();
                }
            })
            .setup(|app| {
                // Created here rather than from the configuration: clipboard
                // access lets the Paste button read the clipboard without a
                // browser permission prompt (Linux, Windows).
                let config = app
                    .config()
                    .app
                    .windows
                    .iter()
                    .find(|w| w.label == MAIN_WINDOW)
                    .cloned()
                    .ok_or("the main window is not configured")?;
                tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?
                    .enable_clipboard_access()
                    .build()?;
                let handle = app.handle().clone();
                thread::spawn(move || {
                    thread::sleep(SHOW_AT_THE_LATEST);
                    if let Some(window) = handle.get_webview_window(MAIN_WINDOW) {
                        if !window.is_visible().unwrap_or(true) {
                            let _ = window.show();
                        }
                    }
                });
                Ok(())
            })
            .invoke_handler(tauri::generate_handler![
                load_app_state,
                save_app_state,
                list_wallets,
                create_wallet,
                import_wallet,
                unlock_wallet,
                lock_wallet,
                lock_all,
                rename_wallet,
                set_address_count,
                remove_wallet,
                reveal_mnemonic,
                get_addresses,
                set_active_wallet,
                rpc_get,
                max_sendable,
                prepare_send,
                broadcast,
                discard_prepared,
                copy_text,
                open_external,
                export_file,
                import_file,
            ])
            .run(tauri::generate_context!());
        if let Err(e) = result {
            eprintln!("MMX Wallet could not start: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::COMMANDS;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;

    fn json_file(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn the_window_may_call_only_the_wallet_commands() {
        let capability = json_file(include_str!("../capabilities/default.json"));
        assert_eq!(capability["windows"], json!(["main"]));
        assert!(capability.get("remote").is_none());
        let granted: BTreeSet<&str> = capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        let expected: BTreeSet<String> = COMMANDS
            .iter()
            .map(|c| format!("allow-{}", c.replace('_', "-")))
            .collect();
        assert_eq!(
            granted,
            expected.iter().map(String::as_str).collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn the_ui_calls_the_same_commands() {
        let source = include_str!("../../src/backend/desktop.ts");
        let mut called = BTreeSet::new();
        for (i, _) in source
            .match_indices("call(")
            .chain(source.match_indices("call<"))
        {
            let rest = &source[i..];
            let Some(start) = rest.find("('").map(|s| s + 2) else {
                continue;
            };
            let name: String = rest[start..]
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            if !name.is_empty() {
                called.insert(name);
            }
        }
        let union: BTreeSet<String> = source
            .lines()
            .filter_map(|l| l.trim().strip_prefix("| '"))
            .map(|l| l.trim_end_matches(';').trim_end_matches('\'').to_owned())
            .collect();
        let expected: BTreeSet<String> = COMMANDS.iter().map(|c| c.to_string()).collect();
        assert_eq!(called, expected);
        assert_eq!(union, expected);
    }

    /// The UI's requests (also checked against desktop.ts by
    /// tests/unit/desktop-backend.test.ts) decode with the strict types.
    #[test]
    fn ui_requests_decode() {
        use crate::args::{self, parse};
        let payloads = json_file(include_str!("../tests/payloads.json"));
        let payloads = payloads.as_object().unwrap();
        assert_eq!(
            payloads.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            COMMANDS.iter().copied().collect::<BTreeSet<_>>()
        );
        for (command, body) in payloads {
            let decoded = match command.as_str() {
                "load_app_state" | "list_wallets" | "lock_all" | "import_file" => {
                    *body == json!({})
                }
                "save_app_state" => parse::<args::SaveState>(body).is_ok_and(|s| {
                    s.state.contacts.len() == 2
                        && s.state.labels.values().next().is_some_and(|l| l.len() == 2)
                        && s.state.settings.rpc_url.is_some()
                        && s.state.active_wallet.is_some()
                }),
                "create_wallet" => {
                    parse::<args::Create>(body).is_ok_and(|c| c.passphrase.is_some())
                }
                "import_wallet" => {
                    parse::<args::Import>(body).is_ok_and(|i| i.passphrase.is_some())
                }
                "unlock_wallet" => {
                    parse::<args::Unlock>(body).is_ok_and(|u| u.passphrase.is_some())
                }
                "lock_wallet" | "remove_wallet" | "reveal_mnemonic" | "get_addresses"
                | "set_active_wallet" => parse::<args::Wallet>(body).is_ok(),
                "rename_wallet" => parse::<args::Rename>(body).is_ok(),
                "set_address_count" => parse::<args::AddressCount>(body).is_ok(),
                "rpc_get" => parse::<args::RpcGet>(body).is_ok(),
                "max_sendable" => parse::<args::MaxSend>(body).is_ok_and(|m| m.memo.is_some()),
                "prepare_send" => parse::<args::Send>(body).is_ok_and(|s| s.memo.is_some()),
                "broadcast" => parse::<args::Broadcast>(body).is_ok(),
                "discard_prepared" => parse::<args::Txid>(body).is_ok(),
                "copy_text" => parse::<args::CopyText>(body).is_ok_and(|c| c.sensitive),
                "open_external" => parse::<args::OpenExternal>(body)
                    .is_ok_and(|o| crate::links::explorer_url(&o.url).is_ok()),
                "export_file" => parse::<args::ExportFile>(body).is_ok(),
                _ => false,
            };
            assert!(decoded, "{command}");
        }
    }

    #[test]
    fn window_and_content_policy() {
        let config = json_file(include_str!("../tauri.conf.json"));
        assert_eq!(config["productName"], "MMX Wallet");
        assert_eq!(config["identifier"], "network.mmx.wallet");
        assert_eq!(config["version"], "../package.json");
        assert_eq!(config["build"]["frontendDist"], "../dist/desktop");
        let app = &config["app"];
        assert_eq!(app["withGlobalTauri"], false);
        assert_eq!(
            app["security"]["csp"],
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
             font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; \
             base-uri 'none'; form-action 'none'; frame-src 'none'"
        );
        assert_eq!(app["security"]["capabilities"], json!(["default"]));
        assert!(app["security"].get("assetProtocol").is_none());
        assert!(app["security"]
            .get("dangerousDisableAssetCspModification")
            .is_none());
        let windows = app["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["label"], "main");
        // Created in setup (with clipboard access), hidden until loaded.
        assert_eq!(
            (&windows[0]["create"], &windows[0]["visible"]),
            (&json!(false), &json!(false))
        );
        assert_eq!(
            (
                windows[0]["minWidth"].as_u64(),
                windows[0]["minHeight"].as_u64()
            ),
            (Some(900), Some(620))
        );
        // The bundles take their version from package.json.
        let package = json_file(include_str!("../../package.json"));
        let version = package["version"].as_str().unwrap();
        assert!(
            version.split('.').count() == 3
                && version
                    .split('.')
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
            "{version}"
        );
    }
}

// Shared with build.rs (included there), which generates a permission for
// each command; capabilities/default.json grants exactly these.

/// The commands the wallet window may call.
pub const COMMANDS: &[&str] = &[
    "load_app_state",
    "save_app_state",
    "list_wallets",
    "new_recovery_phrase",
    "create_wallet",
    "import_wallet",
    "import_seed_file",
    "unlock_wallet",
    "lock_wallet",
    "lock_all",
    "rename_wallet",
    "set_address_count",
    "remove_wallet",
    "reveal_mnemonic",
    "get_addresses",
    "set_active_wallet",
    "rpc_get",
    "max_sendable",
    "prepare_send",
    "broadcast",
    "discard_prepared",
    "copy_text",
    "open_external",
    "export_file",
    "import_file",
];

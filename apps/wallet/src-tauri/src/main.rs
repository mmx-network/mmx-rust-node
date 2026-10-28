// No console window for release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // AppImages can run the CLI without opening a window or being extracted.
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--cli") {
        args.remove(1);
        args[0] = "mmxwallet".into();
        std::process::exit(mmxwallet::main_from(args));
    }
    mmx_wallet_desktop::run();
}

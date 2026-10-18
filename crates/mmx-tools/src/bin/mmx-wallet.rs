fn main() {
    if let Err(e) = mmx_tools::wallet_main() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

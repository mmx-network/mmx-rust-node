// The installed CLI uses exactly the same implementation as the standalone tool.
fn main() {
    std::process::exit(mmxwallet::main_from(std::env::args().collect()));
}

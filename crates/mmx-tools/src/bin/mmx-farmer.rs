fn main() {
    if let Err(e) = mmx_tools::farmer_main() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn main() {
    if let Err(e) = mmx_tools::harvester_main() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

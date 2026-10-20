fn main() {
    if let Err(e) = mmx_tools::timelord_main() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

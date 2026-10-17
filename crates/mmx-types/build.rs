use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../schema");
    let inputs = [
        "vnx-base/interface",
        "vnx-base/modules",
        "vnx-addons/interface",
        "vnx-addons/modules",
        "mmx/interface",
        "mmx/modules",
    ]
    .map(|path| root.join(path));
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("generated.rs");
    vnx_codegen::generate_to(inputs, output)?;
    Ok(())
}

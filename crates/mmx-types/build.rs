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
    let compiled = vnx_codegen::generate_to(inputs, &output)?;
    // Wire byte code deliberately erases aliases such as addr_t and hash_t.
    // Keep their declared names for the public WebAPI JSON presentation.
    fn text(ty: &vnx_codegen::ast::TypeRef) -> String {
        let mut out = ty.name.clone();
        if !ty.args.is_empty() {
            out.push('<');
            out.push_str(&ty.args.iter().map(text).collect::<Vec<_>>().join(","));
            out.push('>');
        }
        if let Some(size) = ty.array {
            out = format!("array<{out},{size}>");
        }
        out
    }
    let mut presentation = String::from("pub fn presentation_field(record: &str, field: &str) -> Option<&'static str> { match (record, field) {\n");
    for (name, definition) in &compiled.definitions {
        for field in &definition.fields {
            presentation.push_str(&format!(
                "({name:?}, {:?}) => Some({:?}),\n",
                field.name,
                text(&field.ty)
            ));
        }
    }
    presentation.push_str("_ => presentation_parent(record).and_then(|parent| presentation_field(parent, field)), } }\n");
    presentation.push_str(
        "pub fn presentation_parent(record: &str) -> Option<&'static str> { match record {\n",
    );
    for (name, definition) in &compiled.definitions {
        if let Some(parent) = &definition.parent {
            presentation.push_str(&format!("{name:?} => Some({:?}),\n", parent.name));
        }
    }
    presentation.push_str("_ => None, } }\n");
    std::fs::write(output.with_file_name("presentation.rs"), presentation)?;
    Ok(())
}

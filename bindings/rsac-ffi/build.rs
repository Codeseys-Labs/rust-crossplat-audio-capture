fn main() {
    // Generate the C header using cbindgen
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let config =
        cbindgen::Config::from_file(format!("{}/cbindgen.toml", crate_dir)).unwrap_or_default();

    if let Ok(bindings) = cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
    {
        // Write to include/ for hand-crafted header coexistence, and also
        // to the crate root for backward compatibility.
        let include_dir = format!("{}/include", crate_dir);
        std::fs::create_dir_all(&include_dir).ok();
        bindings.write_to_file(format!("{}/rsac_generated.h", include_dir));
    } else {
        eprintln!("cargo:warning=cbindgen failed to generate header — this is expected if cbindgen cannot parse the crate");
    }
}

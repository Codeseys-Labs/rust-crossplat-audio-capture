fn main() {
    // Forward the pkg-config requirement from rsac when on Linux
    #[cfg(target_os = "linux")]
    {
        // PipeWire requires pkg-config discovery
        println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    }
}

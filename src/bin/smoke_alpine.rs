//! Alpine musl + PipeWire runtime linkage smoke test.
//!
//! Minimal on purpose: exercise rsac's first real PipeWire IPC round-trip
//! (socket connect + core object query) from a musl-compiled binary on
//! Alpine. The `pipewire` Rust crate links libpipewire-0.3 at link time
//! via pkg-config, so the failure modes we care about here are all at
//! RUNTIME: Alpine's different library sonames, missing SPA plugin
//! search paths, musl dynamic-loader behavior differing from glibc, or
//! the PipeWire daemon refusing a musl-built client's connection.
//!
//! Success criteria: the binary prints the enumerated device list (even if
//! empty) and exits 0. Any panic / library-resolution failure / pipewire
//! connection refusal bubbles up as a non-zero exit.
//!
//! Scoped tightly so it only compiles on Linux with the feat_linux backend:
//! the whole test is a no-op on other targets (the binary just prints a skip
//! line and exits 0, which makes accidental host `cargo build` cheap).
//!
//! Invoked by `ci/alpine-musl-validation/smoke-test.sh` inside the Alpine
//! Docker image — not wired into normal CI.

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rsac::get_device_enumerator;

    eprintln!("[smoke_alpine] resolving device enumerator...");
    let enumerator =
        get_device_enumerator().map_err(|e| format!("get_device_enumerator() failed: {e}"))?;

    eprintln!("[smoke_alpine] enumerating devices (first pipewire round-trip)...");
    let devices = enumerator
        .enumerate_devices()
        .map_err(|e| format!("enumerate_devices() failed: {e}"))?;

    println!("[smoke_alpine] OK: enumerated {} device(s)", devices.len());
    for (idx, dev) in devices.iter().enumerate() {
        println!("  [{idx}] {:?}", dev);
    }

    // Also exercise the default-device path, which is a separate PipeWire
    // roundtrip and often the first thing a real consumer calls.
    match enumerator.get_default_device() {
        Ok(def) => println!("[smoke_alpine] OK: default device = {:?}", def),
        Err(e) => {
            // Not fatal on a headless box with no real sinks beyond the
            // null-sink we create, but log loudly so the operator sees it.
            eprintln!("[smoke_alpine] WARN: get_default_device() returned: {e}");
        }
    }

    Ok(())
}

#[cfg(not(all(target_os = "linux", feature = "feat_linux")))]
fn main() {
    eprintln!(
        "[smoke_alpine] skipped: this binary is only meaningful on \
         target_os = \"linux\" with feature \"feat_linux\" enabled."
    );
}

#[cfg(target_os = "linux")]
fn main() {
    use std::{
        env,
        process::{self, Command},
    };

    // Use PipeWire as primary backend, PulseAudio as optional fallback
    let required_libs = ["alsa", "libpipewire-0.3"];
    let optional_libs = ["libpulse"];
    let missing: Vec<&str> = required_libs
        .iter()
        .copied()
        .filter(|lib| pkg_config::Config::new().probe(lib).is_err())
        .collect();
    
    // Check optional libs but don't fail if missing
    let missing_optional: Vec<&str> = optional_libs
        .iter()
        .copied()
        .filter(|lib| pkg_config::Config::new().probe(lib).is_err())
        .collect();
    
    if !missing_optional.is_empty() {
        eprintln!("Optional libraries not found (PipeWire will be used instead): {}", missing_optional.join(", "));
    }

    if missing.is_empty() {
        return;
    }

    eprintln!("Missing system libraries: {}", missing.join(", "));

    if env::var("RSAC_AUTO_INSTALL").as_deref() == Ok("1") {
        if Command::new("which")
            .arg("apt-get")
            .output()
            .map_or(false, |o| o.status.success())
        {
            let packages: Vec<&str> = missing
                .iter()
                .map(|lib| match *lib {
                    "alsa" => "libasound2-dev",
                    "libpipewire-0.3" => "libpipewire-0.3-dev",
                    _ => *lib,
                })
                .collect();

            eprintln!("Attempting to install: {}", packages.join(" "));
            let _ = Command::new("sudo").arg("apt-get").arg("update").status();
            let status = Command::new("sudo")
                .arg("apt-get")
                .arg("install")
                .arg("-y")
                .args(&packages)
                .status();

            if status.map_or(false, |s| s.success()) {
                let still_missing: Vec<&str> = required_libs
                    .iter()
                    .copied()
                    .filter(|lib| pkg_config::Config::new().probe(lib).is_err())
                    .collect();
                if still_missing.is_empty() {
                    return;
                }
                eprintln!(
                    "Automatic install failed or incomplete. Missing: {}",
                    still_missing.join(", ")
                );
            } else {
                eprintln!("Automatic install failed. Proceeding with error message.");
            }
        } else {
            eprintln!("apt-get not found. Cannot auto-install dependencies.");
        }
    } else {
        eprintln!("Set RSAC_AUTO_INSTALL=1 to attempt automatic installation via apt-get.");
    }

    eprintln!("Install missing libraries with your package manager or set PKG_CONFIG_PATH.");
    process::exit(1);
}

#[cfg(not(target_os = "linux"))]
fn main() {}

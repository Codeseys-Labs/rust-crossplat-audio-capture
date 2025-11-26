fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Platform-specific build configuration for application capture
    // Only configure if the corresponding feature is enabled

    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    configure_windows_build();

    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    configure_linux_build();

    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    configure_macos_build();
}

#[cfg(all(target_os = "windows", feature = "feat_windows"))]
fn configure_windows_build() {
    // Windows-specific build configuration for WASAPI Process Loopback
    println!("cargo:rustc-link-lib=ole32"); // For COM operations
    println!("cargo:rustc-link-lib=oleaut32"); // For VARIANT operations
    println!("cargo:rustc-link-lib=user32"); // For user interface operations
    println!("cargo:rustc-link-lib=advapi32"); // For advanced API operations
    println!("cargo:rustc-link-lib=shell32"); // For shell operations

    // Note: WASAPI libraries are typically linked automatically by the windows crate
    // but we ensure they're available for Process Loopback functionality
    println!("cargo:rustc-link-lib=winmm"); // For multimedia operations
}

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
fn configure_linux_build() {
    use std::{
        env,
        process::{self, Command},
    };

    // Enhanced Linux build configuration for PipeWire application capture

    // Required libraries for PipeWire application capture
    let required_libs = ["libpipewire-0.3"]; // Removed ALSA as we're PipeWire-only for app capture
    let missing: Vec<&str> = required_libs
        .iter()
        .copied()
        .filter(|lib| {
            // For PipeWire, check for minimum version 0.3.44 for monitor stream features
            if *lib == "libpipewire-0.3" {
                pkg_config::Config::new()
                    .atleast_version("0.3.44")
                    .probe(lib)
                    .is_err()
            } else {
                pkg_config::Config::new().probe(lib).is_err()
            }
        })
        .collect();

    if missing.is_empty() {
        // Configure PipeWire linking
        if let Ok(library) = pkg_config::Config::new()
            .atleast_version("0.3.44")
            .probe("libpipewire-0.3")
        {
            for lib in &library.libs {
                println!("cargo:rustc-link-lib={}", lib);
            }
        }
        return;
    }

    eprintln!(
        "Missing system libraries for application capture: {}",
        missing.join(", ")
    );
    eprintln!("Application capture requires PipeWire 0.3.44+ for monitor stream functionality");

    if env::var("RSAC_AUTO_INSTALL").as_deref() == Ok("1") {
        if Command::new("which")
            .arg("apt-get")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let packages: Vec<&str> = missing
                .iter()
                .map(|lib| match *lib {
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

            if status.is_ok_and(|s| s.success()) {
                let still_missing: Vec<&str> = required_libs
                    .iter()
                    .copied()
                    .filter(|lib| {
                        if *lib == "libpipewire-0.3" {
                            pkg_config::Config::new()
                                .atleast_version("0.3.44")
                                .probe(lib)
                                .is_err()
                        } else {
                            pkg_config::Config::new().probe(lib).is_err()
                        }
                    })
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
    eprintln!(
        "For Ubuntu/Debian: sudo apt-get install libpipewire-0.3-dev pkg-config build-essential"
    );
    process::exit(1);
}

#[cfg(all(target_os = "macos", feature = "feat_macos"))]
fn configure_macos_build() {
    // macOS-specific build configuration for CoreAudio Process Tap

    // Link against CoreAudio framework
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=AudioToolbox");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");

    // For Process Tap APIs (macOS 14.4+), we may need additional frameworks
    println!("cargo:rustc-link-lib=framework=AVFoundation"); // For AVAudioFormat, AVAudioFile

    // Check macOS version and warn if Process Tap APIs may not be available
    if let Ok(version) = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
    {
        if let Ok(version_str) = String::from_utf8(version.stdout) {
            let version_str = version_str.trim();
            println!("cargo:warning=Building for macOS {}", version_str);

            // Parse version to check if it's >= 14.4
            if let Some((major, minor)) = parse_macos_version(&version_str) {
                if major < 14 || (major == 14 && minor < 4) {
                    println!(
                        "cargo:warning=Process Tap APIs require macOS 14.4+, current: {}",
                        version_str
                    );
                    println!("cargo:warning=Application capture on macOS will not be available");
                }
            }
        }
    }
}

#[cfg(all(target_os = "macos", feature = "feat_macos"))]
fn parse_macos_version(version_str: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = version_str.split('.').collect();
    if parts.len() >= 2 {
        if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return Some((major, minor));
        }
    }
    None
}

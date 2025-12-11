//! Virtual Audio Device Setup Tool
//!
//! This tool creates virtual audio devices for CI testing across platforms:
//! - Linux: Creates a PipeWire null sink (no external dependencies)
//! - Windows: Installs/verifies the bundled virtual audio driver
//! - macOS: Installs/verifies the bundled BlackHole-based driver
//!
//! Usage:
//!   vad-setup create    # Create/install virtual audio device
//!   vad-setup remove    # Remove virtual audio device
//!   vad-setup status    # Check if virtual audio device exists
//!   vad-setup test      # Create device, play test tone, verify capture

use std::process::ExitCode;

mod platform;

fn main() -> ExitCode {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str()).unwrap_or("status");

    println!("Virtual Audio Device Setup Tool v0.1.0");
    println!("======================================");
    println!();

    let result = match command {
        "create" | "install" => {
            println!("Creating virtual audio device...");
            platform::create_virtual_device()
        }
        "remove" | "uninstall" => {
            println!("Removing virtual audio device...");
            platform::remove_virtual_device()
        }
        "status" | "check" => {
            println!("Checking virtual audio device status...");
            platform::check_device_status()
        }
        "test" => {
            println!("Testing virtual audio device...");
            platform::test_virtual_device()
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_help();
            Err("Unknown command".into())
        }
    };

    match result {
        Ok(()) => {
            println!();
            println!("Operation completed successfully.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!();
            eprintln!("Operation failed: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("Usage: vad-setup <command>");
    println!();
    println!("Commands:");
    println!("  create, install    Create/install virtual audio device");
    println!("  remove, uninstall  Remove virtual audio device");
    println!("  status, check      Check if virtual audio device exists");
    println!("  test               Create device, play test tone, verify");
    println!("  help               Show this help message");
    println!();
    println!("Platform-specific behavior:");
    println!("  Linux:   Uses PipeWire module-null-sink (built-in, no deps)");
    println!("  Windows: Installs bundled virtual audio driver");
    println!("  macOS:   Installs bundled BlackHole-based driver");
}

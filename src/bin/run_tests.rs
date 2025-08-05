use clap::{arg, Parser};
use std::path::PathBuf;
use std::process::exit;

#[derive(Parser, Debug)]
#[command(author, version, about = "Audio capture test runner")]
struct Args {
    /// Test type (application, system, all)
    #[arg(short, long, default_value = "all")]
    test_type: String,

    /// Duration in seconds to capture
    #[arg(short, long, default_value = "5")]
    duration: u32,

    /// Output directory for test results
    #[arg(short, long, default_value = "./test-results")]
    output_dir: PathBuf,
}

fn main() {
    let args = Args::parse();

    // Create output directory if it doesn't exist
    if !args.output_dir.exists() {
        std::fs::create_dir_all(&args.output_dir).unwrap_or_else(|e| {
            eprintln!("Failed to create output directory: {}", e);
            exit(1);
        });
    }

    // Print test information
    println!("Running audio backend tests");
    println!("Test type: {}", args.test_type);
    println!("Duration: {} seconds", args.duration);
    println!("Output directory: {}", args.output_dir.display());

    // Initialize the library
    if let Err(e) = rsac::init() {
        eprintln!("Failed to initialize library: {}", e);
        exit(1);
    }

    // Get the appropriate audio backend
    let backend = match rsac::get_audio_backend() {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("Failed to get audio backend: {}", e);
            exit(1);
        }
    };

    println!("Using audio backend: {}", backend.name());

    // Run the requested test
    match args.test_type.as_str() {
        "application" => {
            println!("Listing available applications...");
            match backend.list_applications() {
                Ok(apps) => {
                    if apps.is_empty() {
                        println!("No applications found.");
                    } else {
                        println!("Found {} applications:", apps.len());
                        for (i, app) in apps.iter().enumerate() {
                            println!("{}: {} ({})", i + 1, app.name, app.id);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to list applications: {}", e);
                }
            }
        }
        "system" => {
            println!("Testing system audio capture...");
            // Implementation would depend on the specific backend
            println!("System audio capture test not implemented yet.");
        }
        "all" => {
            println!("Running all tests...");
            // Run application test
            println!("Listing available applications...");
            match backend.list_applications() {
                Ok(apps) => {
                    if apps.is_empty() {
                        println!("No applications found.");
                    } else {
                        println!("Found {} applications:", apps.len());
                        for (i, app) in apps.iter().enumerate() {
                            println!("{}: {} ({})", i + 1, app.name, app.id);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to list applications: {}", e);
                }
            }

            // Run system test
            println!("Testing system audio capture...");
            println!("System audio capture test not implemented yet.");
        }
        _ => {
            eprintln!("Unknown test type: {}", args.test_type);
            exit(1);
        }
    }

    println!("Tests completed!");
}

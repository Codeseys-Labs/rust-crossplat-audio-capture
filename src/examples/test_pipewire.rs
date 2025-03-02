use clap::{arg, Parser};
use std::path::PathBuf;
use std::process::exit;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Output file path (WAV)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Duration in seconds
    #[arg(short, long, default_value = "5")]
    duration: u32,
}

fn main() {
    let args = Args::parse();

    let output_path = args
        .output
        .unwrap_or_else(|| PathBuf::from("test_capture.wav"));
    println!("PipeWire Test");
    println!("Output file: {}", output_path.display());
    println!("Duration: {} seconds", args.duration);

    // Initialize PipeWire
    pipewire::init();

    // Create a main loop
    let main_loop = match pipewire::main_loop::MainLoop::new(None) {
        Some(ml) => ml,
        None => {
            eprintln!("Failed to create main loop");
            exit(1);
        }
    };

    // Create a context
    let context = match pipewire::context::Context::new(&main_loop) {
        Some(ctx) => ctx,
        None => {
            eprintln!("Failed to create context");
            exit(1);
        }
    };

    // Connect to PipeWire
    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect: {}", e);
            exit(1);
        }
    };

    println!("Successfully connected to PipeWire!");
    println!("PipeWire test completed successfully");
}

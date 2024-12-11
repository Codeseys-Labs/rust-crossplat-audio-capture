use color_eyre::Result;
use whisper_rs::{WhisperContext, WhisperContextParameters};

fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    println!("Testing whisper model loading...");
    let model_path = "models/whisper-base.bin";

    println!("Creating context parameters...");
    let params = WhisperContextParameters::new();

    println!("Loading model from {}...", model_path);
    let context = WhisperContext::new_with_params(model_path, params)?;

    println!("Creating state...");
    let _state = context.create_state()?;

    println!("Success! Model loaded and state created.");
    Ok(())
}

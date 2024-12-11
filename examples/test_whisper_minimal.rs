use color_eyre::Result;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    println!("Testing minimal whisper usage...");
    let model_path = "models/whisper-base.bin";

    // Load model
    println!("Creating context parameters...");
    let params = WhisperContextParameters::new();

    println!("Loading model from {}...", model_path);
    let context = WhisperContext::new_with_params(model_path, params)?;

    println!("Creating state...");
    let mut state = context.create_state()?;

    // Create a small test audio chunk (1 second of silence)
    println!("Creating test audio...");
    let samples = vec![0.0f32; 16000];

    // Set up parameters
    println!("Setting up parameters...");
    let mut params = FullParams::new(SamplingStrategy::default());
    params.set_language(Some("en"));
    params.set_print_realtime(false);
    params.set_print_progress(false);
    params.set_print_timestamps(true);
    params.set_print_special(false);

    // Process audio
    println!("Processing audio...");
    state.full(params, &samples)?;

    // Get results
    println!("Getting results...");
    let n_segments = state.full_n_segments()?;
    println!("Found {} segments", n_segments);

    for i in 0..n_segments {
        let text = state.full_get_segment_text(i)?;
        let start = state.full_get_segment_t0(i)? as f64 / 1000.0;
        let end = state.full_get_segment_t1(i)? as f64 / 1000.0;
        println!("[{:.2}s - {:.2}s]: {}", start, end, text.trim());
    }

    println!("Success!");
    Ok(())
}

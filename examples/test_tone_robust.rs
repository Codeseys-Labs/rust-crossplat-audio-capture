use rodio::{OutputStream, Sink, Source};
use std::time::Duration;
use std::thread;

fn create_test_signal() -> impl Source<Item = f32> + Send {
    // Create a more complex test signal combining multiple frequencies
    let freq1 = 440.0; // A4 note
    let freq2 = 880.0; // A5 note
    
    let signal1 = rodio::source::SineWave::new(freq1)
        .take_duration(Duration::from_secs(30))
        .amplify(0.3);
    
    let signal2 = rodio::source::SineWave::new(freq2)
        .take_duration(Duration::from_secs(30))
        .amplify(0.2);
    
    // Mix the signals
    signal1.mix(signal2)
}

fn main() {
    // Retry logic for getting output stream
    let max_retries = 5;
    let mut attempt = 0;
    let mut last_error = None;

    while attempt < max_retries {
        match OutputStream::try_default() {
            Ok((stream, stream_handle)) => {
                println!("Successfully obtained output stream on attempt {}", attempt + 1);
                
                // Create a sink
                let sink = match Sink::try_new(&stream_handle) {
                    Ok(sink) => sink,
                    Err(e) => {
                        eprintln!("Failed to create sink: {}", e);
                        std::process::exit(1);
                    }
                };

                println!("Starting test tone playback...");
                sink.append(create_test_signal());
                
                // Keep the program running but allow for clean shutdown
                while !sink.empty() {
                    thread::sleep(Duration::from_millis(100));
                }
                
                // Keep stream alive until we're done
                drop(stream);
                return;
            }
            Err(e) => {
                last_error = Some(e);
                eprintln!("Attempt {} failed to get output stream", attempt + 1);
                thread::sleep(Duration::from_secs(2));
                attempt += 1;
            }
        }
    }

    eprintln!("Failed to get output stream after {} attempts. Last error: {:?}", max_retries, last_error);
    std::process::exit(1);
}
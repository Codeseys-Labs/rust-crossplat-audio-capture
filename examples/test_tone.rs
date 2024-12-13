use rodio::{Decoder, OutputStream, Sink};
use std::fs::File;
use std::io::BufReader;
use std::time::Duration;

fn main() {
    // Get a handle to the default output stream
    let (_stream, stream_handle) =
        OutputStream::try_default().expect("Failed to get default output stream");

    // Create a sink to control the playback
    let sink = Sink::try_new(&stream_handle).expect("Failed to create sink");

    // Load the audio file
    let file =
        BufReader::new(File::open("test_audio/sample.mp3").expect("Failed to open audio file"));

    // Decode the audio file
    let source = Decoder::new(file).expect("Failed to decode audio file");

    println!("Playing audio file...");
    println!("Press Ctrl+C to stop");

    // Append the audio source to the sink for playback
    sink.append(source);

    // Set volume to 50%
    sink.set_volume(0.5);

    // Keep playing until interrupted
    sink.sleep_until_end();
}

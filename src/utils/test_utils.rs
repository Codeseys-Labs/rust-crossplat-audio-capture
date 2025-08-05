use crate::core::config::SampleFormat as CoreSampleFormat; // Alias to avoid conflict if crate::SampleFormat is different
use crate::{AudioCaptureStream, AudioError};
use std::sync::Arc;

/// Test utilities for audio generation, validation, and mock implementations
pub mod generation {
    /// Creates a test sine wave for audio validation
    pub fn create_sine_wave(frequency: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> {
        let num_samples = (duration_ms as f32 * sample_rate as f32 / 1000.0) as usize;
        let mut samples = Vec::with_capacity(num_samples);

        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let sample = (2.0 * std::f32::consts::PI * frequency * t).sin();
            samples.push(sample);
        }

        samples
    }

    /// Creates white noise for testing
    pub fn create_white_noise(duration_ms: u32, sample_rate: u32) -> Vec<f32> {
        let num_samples = (duration_ms as f32 * sample_rate as f32 / 1000.0) as usize;
        let mut samples = Vec::with_capacity(num_samples);

        for _ in 0..num_samples {
            let sample = (rand::random::<f32>() * 2.0 - 1.0) * 0.5; // Scale to -0.5..0.5 range
            samples.push(sample);
        }

        samples
    }

    /// Creates a simple click sound for testing
    pub fn create_click(duration_ms: u32, sample_rate: u32) -> Vec<f32> {
        let num_samples = (duration_ms as f32 * sample_rate as f32 / 1000.0) as usize;
        let mut samples = vec![0.0; num_samples];

        // Create a click at the beginning
        let click_duration = std::cmp::min(num_samples, (0.01 * sample_rate as f32) as usize);
        for i in 0..click_duration {
            let t = i as f32 / click_duration as f32;
            samples[i] = (1.0 - t) * 0.8; // Decaying amplitude
        }

        samples
    }
}

pub mod validation {
    use hound::Error as HoundError;
    use std::io::{Error as IoError, ErrorKind};

    /// Verifies that two audio signals are similar within a tolerance
    pub fn verify_audio_similarity(signal1: &[f32], signal2: &[f32], tolerance: f32) -> bool {
        if signal1.len() != signal2.len() {
            return false;
        }

        signal1
            .iter()
            .zip(signal2.iter())
            .all(|(s1, s2)| (s1 - s2).abs() <= tolerance)
    }

    /// Analyzes frequency content of an audio signal
    pub struct FrequencyAnalysis {
        pub dominant_frequency: f32,
        pub frequency_magnitudes: Vec<(f32, f32)>,
    }

    /// Simple frequency analysis using a basic DFT
    pub fn analyze_frequency_content(signal: &[f32], sample_rate: u32) -> FrequencyAnalysis {
        // Simple DFT implementation
        let n = signal.len();
        let _max_freq = sample_rate as usize / 2;
        let freq_step = sample_rate as f32 / n as f32;

        let mut magnitudes = Vec::new();
        let mut max_magnitude = 0.0;
        let mut dominant_freq = 0.0;

        // Only calculate up to Nyquist frequency
        for k in 0..std::cmp::min(n / 2, 1000) {
            let freq = k as f32 * freq_step;

            let mut re = 0.0;
            let mut im = 0.0;

            for i in 0..n {
                let angle = 2.0 * std::f32::consts::PI * (i as f32) * (k as f32) / (n as f32);
                re += signal[i] * angle.cos();
                im += signal[i] * angle.sin();
            }

            let magnitude = (re * re + im * im).sqrt() / n as f32;

            if magnitude > max_magnitude {
                max_magnitude = magnitude;
                dominant_freq = freq;
            }

            magnitudes.push((freq, magnitude));
        }

        FrequencyAnalysis {
            dominant_frequency: dominant_freq,
            frequency_magnitudes: magnitudes,
        }
    }

    /// Validates that a captured audio file is not silent
    pub fn validate_not_silent(signal: &[f32], threshold: f32) -> bool {
        let rms = (signal.iter().map(|s| s * s).sum::<f32>() / signal.len() as f32).sqrt();
        rms > threshold
    }

    pub fn hound_to_io_error(err: HoundError) -> IoError {
        IoError::new(ErrorKind::Other, err.to_string())
    }

    /// Helper function to create a temporary WAV file for testing
    pub fn create_test_wav_file(
        path: &str,
        samples: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> std::io::Result<()> {
        use hound::{WavSpec, WavWriter};

        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = WavWriter::create(path, spec).map_err(hound_to_io_error)?;
        for sample in samples {
            writer.write_sample(*sample).map_err(hound_to_io_error)?;
        }
        writer.finalize().map_err(hound_to_io_error)?;

        Ok(())
    }

    /// Helper function to read a WAV file for testing
    pub fn read_wav_file(path: &str) -> std::io::Result<(Vec<f32>, hound::WavSpec)> {
        use hound::WavReader;

        let mut reader = WavReader::open(path).map_err(hound_to_io_error)?;
        let spec = reader.spec();
        let samples: Vec<f32> = reader.samples::<f32>().filter_map(Result::ok).collect();

        Ok((samples, spec))
    }
}

pub mod environment {
    /// Sets up a test environment for audio capture
    pub fn setup_test_environment() -> Result<(), String> {
        // This would be platform-specific in a real implementation
        Ok(())
    }

    /// Check if audio is available on the current system
    pub fn is_audio_available() -> bool {
        // This would be platform-specific in a real implementation
        #[cfg(target_os = "linux")]
        {
            // Check for PulseAudio or PipeWire
            std::process::Command::new("pactl")
                .arg("info")
                .output()
                .is_ok()
        }

        #[cfg(target_os = "windows")]
        {
            // Windows typically always has audio, but could check for WASAPI
            true
        }

        #[cfg(target_os = "macos")]
        {
            // Check for CoreAudio
            std::process::Command::new("system_profiler")
                .arg("SPAudioDataType")
                .output()
                .is_ok()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }
}

pub mod reporting {
    use serde::Serialize;
    use std::fmt;

    /// Test result status
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    pub enum TestStatus {
        Passed,
        Failed,
        Skipped,
    }

    impl fmt::Display for TestStatus {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                TestStatus::Passed => write!(f, "PASSED"),
                TestStatus::Failed => write!(f, "FAILED"),
                TestStatus::Skipped => write!(f, "SKIPPED"),
            }
        }
    }

    /// Individual test result
    #[derive(Debug, Clone, Serialize)]
    pub struct TestResult {
        pub name: String,
        pub backend: String,
        pub status: TestStatus,
        pub message: Option<String>,
        pub duration_ms: u64,
        pub artifacts: Vec<String>,
    }

    impl TestResult {
        pub fn new(name: &str, backend: &str) -> Self {
            Self {
                name: name.to_string(),
                backend: backend.to_string(),
                status: TestStatus::Skipped,
                message: None,
                duration_ms: 0,
                artifacts: Vec::new(),
            }
        }

        pub fn passed(mut self, duration_ms: u64) -> Self {
            self.status = TestStatus::Passed;
            self.duration_ms = duration_ms;
            self
        }

        pub fn failed(mut self, message: &str, duration_ms: u64) -> Self {
            self.status = TestStatus::Failed;
            self.message = Some(message.to_string());
            self.duration_ms = duration_ms;
            self
        }

        pub fn with_artifact(mut self, path: &str) -> Self {
            self.artifacts.push(path.to_string());
            self
        }
    }

    /// Test report structure
    #[derive(Debug, Clone, Serialize)]
    pub struct TestReport {
        pub results: Vec<TestResult>,
        pub platform: String,
    }

    impl TestReport {
        pub fn new(platform: &str) -> Self {
            Self {
                results: Vec::new(),
                platform: platform.to_string(),
            }
        }

        pub fn add_result(&mut self, result: TestResult) {
            self.results.push(result);
        }

        pub fn summary(&self) -> (usize, usize, usize) {
            let passed = self
                .results
                .iter()
                .filter(|r| r.status == TestStatus::Passed)
                .count();
            let failed = self
                .results
                .iter()
                .filter(|r| r.status == TestStatus::Failed)
                .count();
            let skipped = self
                .results
                .iter()
                .filter(|r| r.status == TestStatus::Skipped)
                .count();
            (passed, failed, skipped)
        }

        pub fn save_to_file(&self, path: &str) -> std::io::Result<()> {
            let json = serde_json::to_string_pretty(self).unwrap();
            std::fs::write(path, json)
        }
    }
}

/// Mock audio device for testing
#[derive(Debug, Clone)]
pub struct MockAudioDevice {
    pub id: String,
    pub name: String,
    pub channels: u32,
    pub sample_rate: u32,
}

// Local trait that we can implement for mocking
pub trait MockCapture: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<(), AudioError>;
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError>;
    fn config(&self) -> &AudioConfig;
    fn is_capturing(&self) -> bool;
    fn get_captured_data(&self) -> Vec<f32>;
}

// Wrapper type that implements AudioCaptureStream
pub struct MockCaptureWrapper<T: MockCapture>(T);

impl<T: MockCapture + 'static> MockCaptureWrapper<T> {
    pub fn new(inner: T) -> Self {
        Self(inner)
    }

    pub fn is_capturing(&self) -> bool {
        self.0.is_capturing()
    }

    pub fn get_captured_data(&self) -> Vec<f32> {
        self.0.get_captured_data()
    }
}

// Implementation of AudioCaptureStream for the wrapper
impl<T: MockCapture + 'static> AudioCaptureStream for MockCaptureWrapper<T> {
    fn start(&mut self) -> Result<(), AudioError> {
        self.0.start()
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.0.stop()
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        self.0.read(buffer)
    }

    fn config(&self) -> &AudioConfig {
        self.0.config()
    }
}

struct MockAudioCaptureInner {
    is_capturing: bool,
    devices: Vec<MockAudioDevice>,
    current_device: Option<MockAudioDevice>,
    test_data: Arc<Vec<f32>>,
    read_position: usize,
}

impl MockAudioCaptureInner {
    fn new() -> Self {
        Self {
            is_capturing: false,
            devices: vec![
                MockAudioDevice {
                    id: "device1".to_string(),
                    name: "Test Device 1".to_string(),
                    channels: 2,
                    sample_rate: 48000,
                },
                MockAudioDevice {
                    id: "device2".to_string(),
                    name: "Test Device 2".to_string(),
                    channels: 1,
                    sample_rate: 44100,
                },
            ],
            current_device: None,
            test_data: Arc::new(generation::create_sine_wave(440.0, 1000, 48000)),
            read_position: 0,
        }
    }

    fn with_test_data(test_data: Vec<f32>) -> Self {
        Self {
            is_capturing: false,
            devices: vec![],
            current_device: None,
            test_data: Arc::new(test_data),
            read_position: 0,
        }
    }
}

/// Mock audio capture implementation for testing
pub struct MockAudioCapture(MockAudioCaptureInner);

impl MockAudioCapture {
    pub fn new() -> Self {
        Self(MockAudioCaptureInner::new())
    }

    pub fn with_test_data(test_data: Vec<f32>) -> Self {
        Self(MockAudioCaptureInner::with_test_data(test_data))
    }

    pub fn is_capturing(&self) -> bool {
        self.0.is_capturing
    }

    pub fn get_available_devices(&self) -> Vec<MockAudioDevice> {
        self.0.devices.clone()
    }

    pub fn set_device(&mut self, device: MockAudioDevice) {
        self.0.current_device = Some(device);
    }

    pub fn get_current_device(&self) -> Option<&MockAudioDevice> {
        self.0.current_device.as_ref()
    }

    pub fn get_captured_data(&self) -> Vec<f32> {
        self.0.test_data.to_vec()
    }
}

impl MockCapture for MockAudioCapture {
    fn start(&mut self) -> Result<(), AudioError> {
        if self.0.is_capturing {
            return Err(AudioError::CaptureError("Already capturing".into()));
        }
        self.0.is_capturing = true;
        self.0.read_position = 0; // Reset read position on start
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if !self.0.is_capturing {
            return Err(AudioError::CaptureError("Not capturing".into()));
        }
        self.0.is_capturing = false;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        if !self.0.is_capturing {
            return Ok(0);
        }

        let samples_remaining = self.0.test_data.len() - self.0.read_position;
        if samples_remaining == 0 {
            return Ok(0);
        }

        // Convert number of bytes to number of f32 samples
        let bytes_per_sample = std::mem::size_of::<f32>();
        let samples_requested = buffer.len() / bytes_per_sample;
        let samples_to_copy = std::cmp::min(samples_requested, samples_remaining);

        // Copy samples to buffer
        for i in 0..samples_to_copy {
            let sample = self.0.test_data[self.0.read_position + i];
            let bytes = sample.to_le_bytes();
            let start = i * bytes_per_sample;
            buffer[start..start + bytes_per_sample].copy_from_slice(&bytes);
        }

        self.0.read_position += samples_to_copy;
        Ok(samples_to_copy * bytes_per_sample)
    }

    fn config(&self) -> &AudioConfig {
        // Construct a full AudioFormat struct as required by AudioConfig
        static DEFAULT_AUDIO_FORMAT: crate::core::config::AudioFormat =
            crate::core::config::AudioFormat {
                sample_rate: 48000,
                channels: 2,
                bits_per_sample: 32,                    // Assuming f32 is 32 bits
                sample_format: CoreSampleFormat::F32LE, // Use aliased SampleFormat
            };
        static DEFAULT_CONFIG: AudioConfig = AudioConfig {
            format: DEFAULT_AUDIO_FORMAT, // Assign the constructed AudioFormat
                                          // Add other fields of AudioConfig if they exist, e.g., application_name
                                          // For now, assuming AudioConfig only has 'format'.
                                          // If AudioConfig has more fields, this mock needs to be updated.
                                          // Example if it had application_name:
                                          // application_name: Some("MockApp".to_string()),
        };
        &DEFAULT_CONFIG
    }

    fn is_capturing(&self) -> bool {
        self.0.is_capturing
    }

    fn get_captured_data(&self) -> Vec<f32> {
        self.0.test_data.to_vec()
    }
}

// Audio playback utilities
pub mod playback {
    use crate::utils::test_utils::generation;
    use rodio::{Decoder, OutputStream, Sink, Source};
    use std::fs::File;
    use std::io::{self, BufReader};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    pub enum PlaybackError {
        FileOpenError(io::Error),
        DecodingError(rodio::decoder::DecoderError),
        OutputStreamError(String),
        SinkError(String),
    }

    impl std::fmt::Display for PlaybackError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                PlaybackError::FileOpenError(e) => write!(f, "Failed to open audio file: {}", e),
                PlaybackError::DecodingError(e) => write!(f, "Failed to decode audio file: {}", e),
                PlaybackError::OutputStreamError(e) => {
                    write!(f, "Failed to get output stream: {}", e)
                }
                PlaybackError::SinkError(e) => write!(f, "Failed to create sink: {}", e),
            }
        }
    }

    impl std::error::Error for PlaybackError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                PlaybackError::FileOpenError(ref e) => Some(e),
                PlaybackError::DecodingError(ref e) => Some(e),
                _ => None,
            }
        }
    }

    impl From<io::Error> for PlaybackError {
        fn from(err: io::Error) -> Self {
            PlaybackError::FileOpenError(err)
        }
    }

    impl From<rodio::decoder::DecoderError> for PlaybackError {
        fn from(err: rodio::decoder::DecoderError) -> Self {
            PlaybackError::DecodingError(err)
        }
    }

    /// Simple audio player using rodio
    pub struct AudioPlayer {
        _stream: OutputStream,
        sink: Arc<Mutex<Sink>>,
    }

    impl AudioPlayer {
        /// Create a new player from an audio file path
        pub fn new(audio_file_path: &Path) -> Result<Self, PlaybackError> {
            let (stream, handle) = OutputStream::try_default()
                .map_err(|e| PlaybackError::OutputStreamError(e.to_string()))?;
            let sink =
                Sink::try_new(&handle).map_err(|e| PlaybackError::SinkError(e.to_string()))?;

            let file = File::open(audio_file_path)?;
            let source = Decoder::new(BufReader::new(file))?;

            sink.append(source);
            sink.play(); // Start playing immediately

            Ok(Self {
                _stream: stream,
                sink: Arc::new(Mutex::new(sink)),
            })
        }

        /// Create a new player that generates a test tone
        pub fn new_test_tone() -> Result<Self, PlaybackError> {
            let (stream, handle) = OutputStream::try_default()
                .map_err(|e| PlaybackError::OutputStreamError(e.to_string()))?;
            let sink =
                Sink::try_new(&handle).map_err(|e| PlaybackError::SinkError(e.to_string()))?;

            // Generate a 1-second 440Hz sine wave as a test tone
            let sample_rate = 44100;
            let duration_ms = 5000; // 5 seconds tone
            let sine_wave = generation::create_sine_wave(440.0, duration_ms, sample_rate);

            let source = rodio::buffer::SamplesBuffer::new(1, sample_rate, sine_wave)
                .convert_samples::<f32>()
                .repeat_infinite(); // Repeat the tone

            sink.append(source);
            sink.play(); // Start playing immediately

            Ok(Self {
                _stream: stream,
                sink: Arc::new(Mutex::new(sink)),
            })
        }

        /// Set the volume (0.0 to 1.0)
        pub fn set_volume(&self, volume: f32) {
            if let Ok(sink) = self.sink.lock() {
                sink.set_volume(volume.clamp(0.0, 1.0));
            }
        }

        /// Pause playback
        pub fn pause(&self) {
            if let Ok(sink) = self.sink.lock() {
                sink.pause();
            }
        }

        /// Resume playback
        pub fn play(&self) {
            if let Ok(sink) = self.sink.lock() {
                sink.play();
            }
        }

        /// Stop playback and detach the sink
        pub fn stop(&self) {
            if let Ok(sink) = self.sink.lock() {
                sink.stop();
            }
        }

        /// Wait until the current sound finishes playing
        pub fn wait_until_end(&self) {
            if let Ok(sink) = self.sink.lock() {
                sink.sleep_until_end();
            }
        }
    }
}

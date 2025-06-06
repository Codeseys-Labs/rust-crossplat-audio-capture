use rsac::{AudioConfig, AudioError, AudioFormat};
use std::sync::Arc;

// Simplified wrapper that directly implements AudioCaptureStream
pub struct MockCaptureWrapper {
    inner: MockAudioCapture,
}

impl MockCaptureWrapper {
    pub fn new(inner: MockAudioCapture) -> Self {
        Self { inner }
    }

    pub fn is_capturing(&self) -> bool {
        self.inner.is_capturing()
    }

    pub fn get_captured_data(&self) -> Vec<f32> {
        self.inner.get_captured_data()
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        self.inner.start()
    }

    pub fn stop(&mut self) -> Result<(), AudioError> {
        self.inner.stop()
    }
}

impl rsac::AudioCaptureStream for MockCaptureWrapper {
    fn start(&mut self) -> Result<(), AudioError> {
        self.inner.start()
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.inner.stop()
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        self.inner.read(buffer)
    }

    fn config(&self) -> &AudioConfig {
        self.inner.config()
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
            test_data: Arc::new(super::create_test_signal(440.0, 1000, 48000)),
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

impl MockAudioCapture {
    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.0.is_capturing {
            return Err(AudioError::CaptureError("Already capturing".into()));
        }
        self.0.is_capturing = true;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), AudioError> {
        if !self.0.is_capturing {
            return Err(AudioError::CaptureError("Not capturing".into()));
        }
        self.0.is_capturing = false;
        Ok(())
    }

    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
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

    pub fn config(&self) -> &AudioConfig {
        static DEFAULT_CONFIG: AudioConfig = AudioConfig {
            sample_rate: 48000,
            channels: 2,
            format: AudioFormat::F32LE,
        };
        &DEFAULT_CONFIG
    }
}

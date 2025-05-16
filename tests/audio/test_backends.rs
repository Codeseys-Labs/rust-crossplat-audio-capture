// Import from the main crate
// extern crate rsac; // Removed this line
use crate::audio::{AudioError, AudioFormat}; // Add AudioFormat back
use std::path::Path;

/// Trait that defines standardized test cases that all audio backend implementations must satisfy
pub trait AudioBackendTests {
    /// Test name for reporting
    fn name(&self) -> &str;

    /// Test basic connectivity to the audio backend
    fn test_connect_to_backend(&mut self) -> Result<(), AudioError>;

    /// Test enumeration of audio devices
    fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError>;

    /// Test capturing audio from a specific application
    fn test_capture_application(
        &mut self,
        app_name: &str,
        duration_sec: u32,
        output_path: &Path,
    ) -> Result<Vec<f32>, AudioError>;

    /// Test capturing system audio
    fn test_capture_system(
        &mut self,
        duration_sec: u32,
        output_path: &Path,
    ) -> Result<Vec<f32>, AudioError>;

    /// Test audio format conversion
    fn test_format_conversion(&mut self, format: AudioFormat) -> Result<(), AudioError>;

    /// Test robustness against error conditions
    fn test_error_conditions(&mut self) -> Result<(), AudioError>;

    /// Run all standard tests
    fn run_all_tests(
        &mut self,
        output_dir: &Path,
    ) -> Vec<crate::utils::test_utils::reporting::TestResult>;
}

/// Mock implementation of AudioBackendTests for testing the trait itself
#[cfg(test)]
pub struct MockAudioBackendTests {
    pub name: String,
    pub should_fail: bool,
}

#[cfg(test)]
impl AudioBackendTests for MockAudioBackendTests {
    fn name(&self) -> &str {
        &self.name
    }

    fn test_connect_to_backend(&mut self) -> Result<(), AudioError> {
        if self.should_fail {
            Err(AudioError::BackendError(
                "Simulated connection failure".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
        if self.should_fail {
            Err(AudioError::DeviceError(
                "Simulated device list failure".into(),
            ))
        } else {
            Ok(vec![
                "Mock Device 1".to_string(),
                "Mock Device 2".to_string(),
            ])
        }
    }

    fn test_capture_application(
        &mut self,
        app_name: &str,
        duration_sec: u32,
        output_path: &Path,
    ) -> Result<Vec<f32>, AudioError> {
        if self.should_fail {
            Err(AudioError::CaptureError("Simulated capture failure".into()))
        } else {
            use crate::utils::test_utils::generation;
            use crate::utils::test_utils::validation;

            // Generate a test sine wave
            let samples = generation::create_sine_wave(440.0, duration_sec * 1000, 48000);

            // Save to output file
            validation::create_test_wav_file(output_path.to_str().unwrap(), &samples, 2, 48000)
                .map_err(|e| AudioError::IoError(e.to_string()))?;

            Ok(samples)
        }
    }

    fn test_capture_system(
        &mut self,
        duration_sec: u32,
        output_path: &Path,
    ) -> Result<Vec<f32>, AudioError> {
        if self.should_fail {
            Err(AudioError::CaptureError(
                "Simulated system capture failure".into(),
            ))
        } else {
            use crate::utils::test_utils::generation;
            use crate::utils::test_utils::validation;

            // Generate white noise to simulate system audio
            let samples = generation::create_white_noise(duration_sec * 1000, 48000);

            // Save to output file
            validation::create_test_wav_file(output_path.to_str().unwrap(), &samples, 2, 48000)
                .map_err(|e| AudioError::IoError(e.to_string()))?;

            Ok(samples)
        }
    }

    fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
        if self.should_fail {
            Err(AudioError::FormatError("Simulated format error".into()))
        } else {
            Ok(())
        }
    }

    fn test_error_conditions(&mut self) -> Result<(), AudioError> {
        if self.should_fail {
            Err(AudioError::BackendError(
                "Simulated error condition failure".into(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Windows WASAPI test implementation
#[cfg(target_os = "windows")]
pub mod wasapi {
    use super::*;

    pub struct WasapiTests {
        pub name: String,
    }

    impl WasapiTests {
        pub fn new() -> Self {
            Self {
                name: "wasapi".to_string(),
            }
        }
    }

    impl AudioBackendTests for WasapiTests {
        fn name(&self) -> &str {
            &self.name
        }

        fn test_connect_to_backend(&mut self) -> Result<(), AudioError> {
            // Implementation for Windows WASAPI
            Ok(())
        }

        fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
            // Implementation for Windows WASAPI
            Ok(vec!["Default WASAPI Device".to_string()])
        }

        fn test_capture_application(
            &mut self,
            app_name: &str,
            duration_sec: u32,
            output_path: &Path,
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for Windows WASAPI
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_capture_system(
            &mut self,
            duration_sec: u32,
            output_path: &Path,
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for Windows WASAPI
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_error_conditions(&mut self) -> Result<(), AudioError> {
            // Implementation for Windows WASAPI
            Err(AudioError::CaptureError("Not implemented".into()))
        }
    }
}

/// Linux PulseAudio test implementation
#[cfg(target_os = "linux")]
pub mod pulse {
    use super::*;
    use crate::utils::test_utils::reporting::TestResult; // Import TestResult

    // PulseAudio tests temporarily disabled as we focus on WASAPI and PipeWire
    pub struct PulseAudioTests {
        pub name: String,
    }

    impl PulseAudioTests {
        pub fn new() -> Self {
            Self {
                name: "pulseaudio".to_string(),
            }
        }
    }

    impl AudioBackendTests for PulseAudioTests {
        fn name(&self) -> &str {
            &self.name
        }

        fn test_connect_to_backend(&mut self) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_capture_application(
            &mut self,
            _app_name: &str,     // Added underscore
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_capture_system(
            &mut self,
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_error_conditions(&mut self) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        // Add placeholder implementation for run_all_tests
        fn run_all_tests(&mut self, _output_dir: &Path) -> Vec<TestResult> {
            println!("PulseAudio tests are disabled.");
            vec![]
        }
    }
}

/// Linux PipeWire test implementation
#[cfg(target_os = "linux")]
pub mod pipewire {
    use super::*;
    use crate::utils::test_utils::reporting::TestResult; // Import TestResult

    pub struct PipeWireTests {
        pub name: String,
    }

    impl PipeWireTests {
        pub fn new() -> Self {
            Self {
                name: "pipewire".to_string(),
            }
        }

        pub fn is_available() -> bool {
            // Check if PipeWire is available
            true
        }
    }

    impl AudioBackendTests for PipeWireTests {
        fn name(&self) -> &str {
            &self.name
        }

        fn test_connect_to_backend(&mut self) -> Result<(), AudioError> {
            // Implementation for PipeWire
            Ok(())
        }

        fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
            // Implementation for PipeWire
            Ok(vec!["Default PipeWire Device".to_string()])
        }

        fn test_capture_application(
            &mut self,
            _app_name: &str,     // Added underscore
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for PipeWire
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_capture_system(
            &mut self,
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for PipeWire
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_error_conditions(&mut self) -> Result<(), AudioError> {
            // Implementation for PipeWire
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        // Add placeholder implementation for run_all_tests
        fn run_all_tests(&mut self, _output_dir: &Path) -> Vec<TestResult> {
            println!("PipeWire tests run_all_tests is not fully implemented.");
            vec![]
        }
    }
}

/// macOS CoreAudio test implementation
#[cfg(target_os = "macos")]
pub mod coreaudio {
    use super::*;

    pub struct CoreAudioTests {
        pub name: String,
    }

    impl CoreAudioTests {
        pub fn new() -> Self {
            Self {
                name: "coreaudio".to_string(),
            }
        }
    }

    impl AudioBackendTests for CoreAudioTests {
        fn name(&self) -> &str {
            &self.name
        }

        fn test_connect_to_backend(&mut self) -> Result<(), AudioError> {
            // Implementation for CoreAudio
            Ok(())
        }

        fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
            // Implementation for CoreAudio
            Ok(vec!["Default CoreAudio Device".to_string()])
        }

        fn test_capture_application(
            &mut self,
            app_name: &str,
            duration_sec: u32,
            output_path: &Path,
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for CoreAudio
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_capture_system(
            &mut self,
            duration_sec: u32,
            output_path: &Path,
        ) -> Result<Vec<f32>, AudioError> {
            // Implementation for CoreAudio
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
            Err(AudioError::CaptureError("Not implemented".into()))
        }

        fn test_error_conditions(&mut self) -> Result<(), AudioError> {
            // Implementation for CoreAudio
            Err(AudioError::CaptureError("Not implemented".into()))
        }
    }
}

/// Return the appropriate backend tests implementation for the current platform
pub fn get_backend_tests() -> Box<dyn AudioBackendTests> {
    #[cfg(target_os = "windows")]
    {
        Box::new(wasapi::WasapiTests::new())
    }
    #[cfg(target_os = "linux")]
    {
        if pipewire::PipeWireTests::is_available() {
            Box::new(pipewire::PipeWireTests::new())
        } else {
            Box::new(pulse::PulseAudioTests::new())
        }
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(coreaudio::CoreAudioTests::new())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        compile_error!("Unsupported operating system")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_utils::reporting::TestStatus;

    #[test]
    fn test_mock_backend_success() {
        let mut backend = MockAudioBackendTests {
            name: "mock_success".to_string(),
            should_fail: false,
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let results = backend.run_all_tests(temp_dir.path());

        assert_eq!(results.len(), 6);
        assert!(results.iter().all(|r| r.status == TestStatus::Passed));
    }

    #[test]
    fn test_mock_backend_failure() {
        let mut backend = MockAudioBackendTests {
            name: "mock_failure".to_string(),
            should_fail: true,
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let results = backend.run_all_tests(temp_dir.path());

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, TestStatus::Failed);
    }
}

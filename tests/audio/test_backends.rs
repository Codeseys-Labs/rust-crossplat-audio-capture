// Import from the main crate
// extern crate rsac; // Removed this line
use crate::{AudioError, AudioFormat}; // Use crate:: for re-exported types
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
            Err(AudioError::DeviceEnumerationError(
                // Corrected to a valid AudioError variant
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
            // Ensure app_name, duration_sec, and output_path are used or marked as unused.
            let _ = app_name; // Mark as used
            let _ = duration_sec;
            let _ = output_path;
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
            let _ = duration_sec; // Mark as used
            let _ = output_path;
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
            Err(AudioError::UnsupportedFormat(
                "Simulated format error".into(),
            )) // Corrected to a valid AudioError variant
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

    fn run_all_tests(
        &mut self,
        output_dir: &Path,
    ) -> Vec<crate::utils::test_utils::reporting::TestResult> {
        use crate::utils::test_utils::reporting::TestResult;
        let mut results = Vec::new();
        let start_time = std::time::Instant::now();

        // test_connect_to_backend
        let mut connect_result = TestResult::new("test_connect_to_backend", self.name());
        match self.test_connect_to_backend() {
            Ok(_) => {
                connect_result = connect_result.passed(start_time.elapsed().as_millis() as u64)
            }
            Err(e) => {
                connect_result =
                    connect_result.failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(connect_result);
        if self.should_fail {
            return results;
        } // Stop if critical test fails

        // test_list_devices
        let mut list_devices_result = TestResult::new("test_list_devices", self.name());
        match self.test_list_devices() {
            Ok(_) => {
                list_devices_result =
                    list_devices_result.passed(start_time.elapsed().as_millis() as u64)
            }
            Err(e) => {
                list_devices_result = list_devices_result
                    .failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(list_devices_result);

        // test_capture_application
        let mut capture_app_result = TestResult::new("test_capture_application", self.name());
        let app_output_path = output_dir.join("mock_app_capture.wav");
        match self.test_capture_application("mock_app", 1, &app_output_path) {
            Ok(_) => {
                capture_app_result = capture_app_result
                    .passed(start_time.elapsed().as_millis() as u64)
                    .with_artifact(app_output_path.to_str().unwrap_or(""))
            }
            Err(e) => {
                capture_app_result = capture_app_result
                    .failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(capture_app_result);

        // test_capture_system
        let mut capture_sys_result = TestResult::new("test_capture_system", self.name());
        let sys_output_path = output_dir.join("mock_sys_capture.wav");
        match self.test_capture_system(1, &sys_output_path) {
            Ok(_) => {
                capture_sys_result = capture_sys_result
                    .passed(start_time.elapsed().as_millis() as u64)
                    .with_artifact(sys_output_path.to_str().unwrap_or(""))
            }
            Err(e) => {
                capture_sys_result = capture_sys_result
                    .failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(capture_sys_result);

        // test_format_conversion
        let mut format_conv_result = TestResult::new("test_format_conversion", self.name());
        let dummy_format = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            bits_per_sample: 16,
            sample_format: crate::core::config::SampleFormat::S16LE,
        };
        match self.test_format_conversion(dummy_format) {
            Ok(_) => {
                format_conv_result =
                    format_conv_result.passed(start_time.elapsed().as_millis() as u64)
            }
            Err(e) => {
                format_conv_result = format_conv_result
                    .failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(format_conv_result);

        // test_error_conditions
        let mut error_cond_result = TestResult::new("test_error_conditions", self.name());
        match self.test_error_conditions() {
            Ok(_) => {
                error_cond_result =
                    error_cond_result.passed(start_time.elapsed().as_millis() as u64)
            }
            Err(e) => {
                error_cond_result = error_cond_result
                    .failed(&e.to_string(), start_time.elapsed().as_millis() as u64)
            }
        }
        results.push(error_cond_result);

        results
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

        fn run_all_tests(
            &mut self,
            _output_dir: &Path,
        ) -> Vec<crate::utils::test_utils::reporting::TestResult> {
            // Placeholder for WASAPI tests
            println!("WASAPI run_all_tests is not fully implemented.");
            vec![]
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
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError> {
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_capture_application(
            &mut self,
            _app_name: &str,     // Added underscore
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_capture_system(
            &mut self,
            _duration_sec: u32,  // Added underscore
            _output_path: &Path, // Added underscore
        ) -> Result<Vec<f32>, AudioError> {
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_format_conversion(&mut self, _format: AudioFormat) -> Result<(), AudioError> {
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
                "PulseAudio tests temporarily disabled".into(),
            ))
        }

        fn test_error_conditions(&mut self) -> Result<(), AudioError> {
            Err(AudioError::UnsupportedPlatform(
                // Corrected to a valid AudioError variant
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

        fn run_all_tests(
            &mut self,
            _output_dir: &Path,
        ) -> Vec<crate::utils::test_utils::reporting::TestResult> {
            // Placeholder for CoreAudio tests
            println!("CoreAudio run_all_tests is not fully implemented.");
            vec![]
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

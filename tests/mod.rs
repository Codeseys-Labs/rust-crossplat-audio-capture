pub mod audio;

#[cfg(test)]
mod tests {
    use super::audio::test_backends::AudioBackendTests;
    use std::path::Path;

    #[test]
    #[ignore] // Run manually with --ignored flag
    fn run_backend_tests() {
        // Set up output directory for test artifacts
        let output_dir = Path::new("target/test_output");
        std::fs::create_dir_all(output_dir).expect("Failed to create test output directory");

        // Run platform-specific tests
        #[cfg(target_os = "windows")]
        {
            let mut backend_tests = super::audio::test_backends::wasapi::WasapiTests::new();
            let results = backend_tests.run_all_tests(output_dir);
            assert!(results
                .iter()
                .all(|r| r.status == crate::utils::test_utils::reporting::TestStatus::Passed));
            // Corrected path
        }

        #[cfg(target_os = "linux")]
        {
            // Try PipeWire first
            // Assuming PipeWireTests::new() returns Self, not Result
            let mut backend_tests = super::audio::test_backends::pipewire::PipeWireTests::new();
            // A check like `is_available()` should be used if construction can fail or if it's conditional
            if super::audio::test_backends::pipewire::PipeWireTests::is_available() {
                let results = backend_tests.run_all_tests(output_dir);
                assert!(results
                    .iter()
                    .all(|r| r.status == crate::utils::test_utils::reporting::TestStatus::Passed));
            // Corrected path
            } else {
                // Fallback to PulseAudio
                let mut backend_tests_pulse =
                    super::audio::test_backends::pulse::PulseAudioTests::new();
                let results = backend_tests_pulse.run_all_tests(output_dir);
                assert!(results
                    .iter()
                    .all(|r| r.status == crate::utils::test_utils::reporting::TestStatus::Passed));
                // Corrected path
            }
        }

        #[cfg(target_os = "macos")]
        {
            let mut backend_tests = super::audio::test_backends::coreaudio::CoreAudioTests::new();
            let results = backend_tests.run_all_tests(output_dir);
            assert!(results
                .iter()
                .all(|r| r.status == crate::utils::test_utils::reporting::TestStatus::Passed));
            // Corrected path
        }
    }
}

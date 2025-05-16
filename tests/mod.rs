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
                .all(|r| r.status == super::audio::test_utils::reporting::TestStatus::Passed));
        }

        #[cfg(target_os = "linux")]
        {
            // Try PipeWire first
            if let Ok(mut backend_tests) =
                super::audio::test_backends::pipewire::PipeWireTests::new()
            {
                let results = backend_tests.run_all_tests(output_dir);
                assert!(results
                    .iter()
                    .all(|r| r.status == super::audio::test_utils::reporting::TestStatus::Passed));
            } else {
                // Fallback to PulseAudio
                let mut backend_tests = super::audio::test_backends::pulse::PulseAudioTests::new();
                let results = backend_tests.run_all_tests(output_dir);
                assert!(results
                    .iter()
                    .all(|r| r.status == super::audio::test_utils::reporting::TestStatus::Passed));
            }
        }

        #[cfg(target_os = "macos")]
        {
            let mut backend_tests = super::audio::test_backends::coreaudio::CoreAudioTests::new();
            let results = backend_tests.run_all_tests(output_dir);
            assert!(results
                .iter()
                .all(|r| r.status == super::audio::test_utils::reporting::TestStatus::Passed));
        }
    }
}

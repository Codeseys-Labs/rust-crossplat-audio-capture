# Testing Strategy

## Overview
This document outlines the testing strategy for the audio capture library.

## Test Categories

### 1. Unit Tests
Unit tests cover individual components and functions:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_config_builder() {
        let config = AudioCaptureConfig::new()
            .sample_rate(48000)
            .channels(2)
            .buffer_size(1024);
        
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
        assert_eq!(config.buffer_size, 1024);
    }

    #[test]
    fn test_audio_device_creation() {
        let device = AudioDevice {
            id: "test_device".to_string(),
            name: "Test Device".to_string(),
            device_type: DeviceType::Input,
            channels: 2,
            sample_rate: 44100,
            backend: AudioBackend::Wasapi,
        };

        assert_eq!(device.channels, 2);
        assert_eq!(device.sample_rate, 44100);
    }
}
```

### 2. Integration Tests
Tests that verify the interaction between components:

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_capture_pipeline() {
        let config = AudioCaptureConfig::new();
        let mut capture = create_platform_capture(config).unwrap();
        
        assert!(!capture.is_capturing());
        capture.start().unwrap();
        assert!(capture.is_capturing());
        capture.stop().unwrap();
        assert!(!capture.is_capturing());
    }
}
```

### 3. Platform-Specific Tests
Tests for platform-specific implementations:

```rust
#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    #[test]
    fn test_wasapi_device_enumeration() {
        // Test WASAPI device listing
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    #[test]
    fn test_coreaudio_device_enumeration() {
        // Test CoreAudio device listing
    }
}
```

### 4. Mock Tests
Tests using mock implementations:

```rust
#[cfg(test)]
mod mock_tests {
    use super::*;

    struct MockCapture {
        is_capturing: bool,
    }

    impl AudioCapture for MockCapture {
        // Mock implementations
    }

    #[test]
    fn test_with_mock_capture() {
        let mut capture = MockCapture { is_capturing: false };
        // Test using mock implementation
    }
}
```

## Test Infrastructure

### CI Test Matrix
- Windows: WASAPI tests
- macOS: CoreAudio tests
- Linux: PipeWire and PulseAudio tests

### Test Utilities
Common utilities for testing:

```rust
#[cfg(test)]
mod test_utils {
    pub fn create_test_audio() -> Vec<f32> {
        // Create test audio data
    }

    pub fn verify_audio_data(captured: &[f32], expected: &[f32]) -> bool {
        // Verify audio data matches expected
    }
}
```

### Performance Tests
Tests for performance characteristics:

```rust
#[cfg(test)]
mod perf_tests {
    use criterion::{criterion_group, criterion_main, Criterion};

    fn bench_audio_capture(c: &mut Criterion) {
        c.bench_function("capture_1s", |b| {
            // Benchmark audio capture
        });
    }
}
```

## Test Coverage Goals
- Unit test coverage: >90%
- Integration test coverage: >80%
- Platform-specific coverage: >85%

## Running Tests
```bash
# Run all tests
cargo test

# Run specific test categories
cargo test --test unit_tests
cargo test --test integration_tests

# Run platform-specific tests
cargo test --features wasapi  # Windows
cargo test --features coreaudio  # macOS
cargo test --features pipewire  # Linux
```
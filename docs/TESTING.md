# Audio Capture Testing

This document describes the testing infrastructure for the cross-platform audio capture library.

## Testing Structure

The testing infrastructure is organized into several components:

```
┌─────────────────────────────────────────────────────────────┐
│                      Test Runner                            │
├─────────────┬─────────────────────────────┬─────────────────┤
│ Windows     │ Linux                       │ macOS           │
│ Tests       │ Tests                       │ Tests           │
├─────────────┼─────────────────────────────┼─────────────────┤
│ WASAPI      │ PulseAudio    │ PipeWire    │ CoreAudio       │
│ Backend     │ Backend       │ Backend     │ Backend         │
├─────────────┴─────────────────────────────┴─────────────────┤
│                  Common Test Cases                          │
├─────────────────────────────────────────────────────────────┤
│                  Test Utilities Library                     │
└─────────────────────────────────────────────────────────────┘
```

### Test Utilities Library (src/audio/test_utils.rs)

A comprehensive test utilities library providing:

- **Generation**: Functions to generate test audio signals (sine waves, white noise, etc.)
- **Validation**: Tools to validate captured audio (similarity checking, frequency analysis)
- **Environment**: Functions to set up test environments
- **Reporting**: Tools to collect and report test results

### Backend Tests (src/audio/test_backends.rs)

This module defines a trait `AudioBackendTests` that standardizes tests across all backends:

```rust
pub trait AudioBackendTests {
    fn name(&self) -> &str;
    fn test_connect_to_backend(&mut self) -> Result<(), AudioError>;
    fn test_list_devices(&mut self) -> Result<Vec<String>, AudioError>;
    fn test_capture_application(&mut self, app_name: &str, duration_sec: u32, output_path: &Path) -> Result<Vec<f32>, AudioError>;
    fn test_capture_system(&mut self, duration_sec: u32, output_path: &Path) -> Result<Vec<f32>, AudioError>;
    fn test_format_conversion(&mut self, format: AudioFormat) -> Result<(), AudioError>;
    fn test_error_conditions(&mut self) -> Result<(), AudioError>;
    fn run_all_tests(&mut self, output_dir: &Path) -> Vec<TestResult>;
}
```

### Test Binaries

- **test_runner**: A standardized CLI to run tests (`src/bin/test_runner.rs`)
- **test_report_generator**: Generates HTML reports from test results (`src/bin/test_report_generator.rs`)

### Examples

- **standardized_test.rs**: A standardized example that works on all platforms

## Testing Environments

The tests can be run in several environments:

### Local Development

Run tests locally using cargo:

```bash
# Run all tests for the current platform
cargo run --example standardized_test

# Run a specific test type
cargo run --example standardized_test -- --test-type application

# Run with a specific backend
cargo run --example standardized_test -- --backend pulseaudio
```

### Using Docker (Linux)

Use Docker for isolated testing:

```bash
# Run all Linux tests
./scripts/run_linux_matrix_tests.sh

# Run a specific test
docker-compose up --build rsac-linux-pipewire
```

### Using the Test Runner Script

Use the unified test runner script:

```bash
./scripts/run_audio_tests.sh --backend auto --type all
```

## Test Types

### Unit Tests

Standard Rust unit tests in the `tests` directory.

### Integration Tests

- **Application Capture**: Tests capturing audio from a specific application
- **System Capture**: Tests capturing system-wide audio
- **Device Enumeration**: Tests listing audio devices
- **Format Conversion**: Tests audio format conversions

## Adding New Tests

To add a new test:

1. If it's a general test case, add it to the `AudioBackendTests` trait
2. Implement the test for each backend
3. Add it to the test runner if needed

## Test Reports

Tests generate reports in JSON format that can be combined into an HTML report using the test_report_generator tool.

- Individual JSON reports: `./test-results/{backend}_{test}_*.json`
- HTML report: `./test-results/test_report.html`

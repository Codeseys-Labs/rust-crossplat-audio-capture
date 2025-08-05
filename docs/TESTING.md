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

---

## Running Tests

This section details how to execute the various test suites for the `rust-crossplat-audio-capture` project.

### Local Testing

To run tests locally on your machine:

**Prerequisites:**

- Ensure Rust stable is installed.
- Platform-specific audio backends (e.g., PulseAudio/PipeWire on Linux, WASAPI on Windows, CoreAudio on macOS) should be available for full test coverage on respective OSes.

**Command:**

```bash
cargo test --all-features --all-targets
```

This command will compile and run all tests defined in the project, including unit and integration tests, across all available features and target configurations.

### Dockerized Linux Testing

For a consistent Linux testing environment, tests can be run within a Docker container.

**Command:**
To build the Docker image and run tests:

```bash
docker build --file docker/linux/Dockerfile --tag rust-audio-capture-linux-test:latest .
```

**Explanation:**
The [`docker/linux/Dockerfile`](docker/linux/Dockerfile:1) is configured to run `cargo test --all-features --all-targets` as part of the image build process. A successful build of this Docker image indicates that all tests passed within the defined Linux container environment.

### CI/CD Pipeline Overview

Our Continuous Integration and Continuous Delivery (CI/CD) pipeline is managed using GitHub Actions. The workflow is defined in [`.github/workflows/ci.yml`](.github/workflows/ci.yml:1).

**Triggers:**
The CI pipeline is automatically triggered on:

- Push events to the `main` or `master` branch.
- Pull request events targeting the `main` or `master` branch.

**Jobs:**
The pipeline consists of the following key jobs:

1.  **`build_and_test`**:

    - Runs on: Linux, Windows, and macOS native runners.
    - Tasks:
      - Checks code formatting using `cargo fmt --all -- --check`.
      - Lints the code using `cargo clippy --all-targets --all-features -- -D warnings`.
      - Builds the project.
      - Runs tests using `cargo test --all-features --all-targets`.

2.  **`docker_linux_test`**:

    - Runs on: Linux runner.
    - Tasks: Builds the Linux Docker image defined in [`docker/linux/Dockerfile`](docker/linux/Dockerfile:1). This process includes running `cargo test --all-features --all-targets` inside the container.

3.  **`coverage`**:
    - Runs on: Linux runner.
    - Tasks:
      - Generates a code coverage report using `cargo-tarpaulin`.
      - Uploads the coverage report to Codecov.io.

**Interpreting Results:**

- CI run statuses can be monitored on the "Actions" tab of the GitHub repository.
- For pull requests, these checks (formatting, linting, tests on all platforms, Dockerized Linux tests, coverage) will be reported directly on the pull request page.
- Code coverage reports are available on Codecov.io. A link to the report is usually posted as a comment in pull requests by the Codecov bot, or can be found by navigating to the project's page on Codecov.

### Troubleshooting

Here are a few common issues and their solutions:

- **Dockerized tests fail:**
  - Ensure Docker daemon is running on your system.
  - Check for sufficient disk space for Docker images.
- **Local build/test errors:**
  - Update your Rust toolchain: `rustup update stable`.
  - Ensure all necessary platform-specific development libraries for audio backends are installed.
- **Coverage report not appearing:**
  - Verify the Codecov token is correctly configured in GitHub Actions secrets (for maintainers).
  - Check the `coverage` job logs in GitHub Actions for any errors during report generation or upload.

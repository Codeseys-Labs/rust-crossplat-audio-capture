# Testing Infrastructure Refactoring Plan

## Overview

This document outlines the plan for refactoring the testing infrastructure of the cross-platform audio capture library. The goal is to create a more maintainable, consistent, and effective testing framework that works across all supported platforms (Windows, Linux, macOS) and audio backends (WASAPI, PulseAudio, PipeWire, CoreAudio).

## Current State Analysis

### Strengths of Current Testing Infrastructure

1. **Platform Coverage**: Tests exist for multiple platforms and audio backends
2. **Docker Integration**: Containerized testing provides isolation and reproducibility
3. **Mock Testing**: Good foundation for unit testing with mocks
4. **Test Utilities**: Basic utilities for audio generation and validation

### Weaknesses of Current Testing Infrastructure

1. **Inconsistency**: Different approaches for different backends
2. **Duplication**: Similar code repeated across test scripts
3. **Complexity**: Docker setup has many moving parts
4. **Maintainability**: Difficult to add or modify tests
5. **Coverage Gaps**: Windows and macOS testing less developed
6. **Validation Inconsistency**: Different validation approaches
7. **Poor Organization**: Unclear separation between test types

## Refactoring Goals

1. **Consistency**: Standardize testing approach across all platforms
2. **Modularity**: Create reusable components for testing
3. **Simplicity**: Reduce complexity in Docker setup
4. **Maintainability**: Make it easier to add and modify tests
5. **Coverage**: Improve testing for all platforms
6. **Validation**: Standardize result validation
7. **Organization**: Clear separation between test types

## Architecture

The refactored testing infrastructure will follow this architecture:

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

## Detailed Refactoring Plan

### 1. Test Utilities Library (`src/audio/test_utils.rs`)

Create a comprehensive test utilities library that provides:

```rust
// Example structure for test utilities
pub mod generation {
    // Generate test tones, patterns, etc.
    pub fn create_sine_wave(frequency: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> { ... }
    pub fn create_white_noise(duration_ms: u32, sample_rate: u32) -> Vec<f32> { ... }
}

pub mod validation {
    // Validate captured audio
    pub fn verify_audio_similarity(signal1: &[f32], signal2: &[f32], tolerance: f32) -> bool { ... }
    pub fn analyze_frequency_content(signal: &[f32], sample_rate: u32) -> FrequencyAnalysis { ... }
}

pub mod environment {
    // Set up test environment
    pub fn setup_virtual_audio_device() -> Result<VirtualDevice, Error> { ... }
    pub fn play_audio_file(path: &str) -> Result<AudioPlayer, Error> { ... }
}

pub mod reporting {
    // Report test results
    pub fn save_test_result(name: &str, result: &TestResult) -> Result<(), Error> { ... }
    pub fn generate_test_report(results: &[TestResult]) -> Report { ... }
}
```

### 2. Standardized Test Cases

Define a set of standard test cases in a trait that all backend implementations must satisfy:

```rust
// Example trait for standardized test cases
pub trait AudioBackendTests {
    // Basic connectivity
    fn test_connect_to_backend() -> Result<(), Error>;
    
    // Device enumeration
    fn test_list_devices() -> Result<(), Error>;
    
    // Application capture
    fn test_capture_application(app_name: &str, duration_sec: u32) -> Result<Vec<f32>, Error>;
    
    // System capture
    fn test_capture_system(duration_sec: u32) -> Result<Vec<f32>, Error>;
    
    // Format handling
    fn test_format_conversion(format: AudioFormat) -> Result<(), Error>;
    
    // Error handling
    fn test_error_conditions() -> Result<(), Error>;
}
```

### 3. Docker Infrastructure Refactoring

#### 3.1 Base Docker Images

Create base Docker images with common dependencies:

```dockerfile
# Base image for all tests
FROM ubuntu:22.04 as rsac-test-base

# Install common dependencies
RUN apt-get update && apt-get install -y \
    curl wget build-essential pkg-config \
    libclang-dev llvm-14 clang-14 \
    xvfb dbus ffmpeg \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# Set up common environment
ENV PATH="/root/.cargo/bin:${PATH}"
ENV DEBIAN_FRONTEND=noninteractive
ENV DISPLAY=:99
ENV RUST_BACKTRACE=1
ENV RUST_LOG=debug

# Create directories
RUN mkdir -p /app /test-results

WORKDIR /app

# Copy common test utilities
COPY scripts/test_utils.sh /app/scripts/
```

#### 3.2 Backend-Specific Images

Extend base images for specific backends:

```dockerfile
# PulseAudio image
FROM rsac-test-base as rsac-pulseaudio

# Install PulseAudio
RUN apt-get update && apt-get install -y \
    pulseaudio pulseaudio-utils libpulse-dev

# Copy PulseAudio configuration
COPY docker/linux/pulse-client.conf /etc/pulse/client.conf
COPY docker/linux/pulse-daemon.conf /etc/pulse/daemon.conf
```

#### 3.3 Shared Test Scripts

Create shared scripts for common functionality:

```bash
#!/bin/bash
# scripts/test_utils.sh

# Download test audio if needed
download_test_audio() {
    local TEST_AUDIO=$1
    local URL=$2
    
    if [ ! -f $TEST_AUDIO ]; then
        echo "Downloading test audio from $URL..."
        curl -L "$URL" -o "$TEST_AUDIO"
    fi
}

# Set up audio server
setup_audio_server() {
    local SERVER_TYPE=$1
    
    case $SERVER_TYPE in
        "pulseaudio")
            if ! pulseaudio --check; then
                echo "Starting PulseAudio..."
                pulseaudio --start
                sleep 2
            fi
            ;;
        "pipewire")
            if ! pgrep -x "pipewire" > /dev/null; then
                echo "Starting Pipewire..."
                pipewire &
                sleep 2
            fi
            if ! pgrep -x "pipewire-pulse" > /dev/null; then
                echo "Starting Pipewire-Pulse..."
                pipewire-pulse &
                sleep 2
            fi
            ;;
    esac
}

# Create virtual monitor for system audio
create_system_monitor() {
    local SERVER_TYPE=$1
    
    case $SERVER_TYPE in
        "pulseaudio")
            pacmd load-module module-null-sink sink_name=system_monitor sink_properties=device.description="System Monitor"
            pacmd load-module module-loopback source_dont_move=true sink_dont_move=true source=system_monitor.monitor sink=@DEFAULT_SINK@
            ;;
        "pipewire")
            pactl load-module module-null-sink sink_name=system_monitor sink_properties=device.description=system_monitor
            pactl load-module module-loopback source_dont_move=true sink_dont_move=true source=system_monitor.monitor sink=@DEFAULT_SINK@
            ;;
    esac
}

# Play test audio
play_test_audio() {
    local SERVER_TYPE=$1
    local TEST_AUDIO=$2
    local DEVICE=$3
    
    case $SERVER_TYPE in
        "pulseaudio"|"pipewire")
            paplay --device=$DEVICE $TEST_AUDIO &
            echo $!  # Return PID
            ;;
    esac
}

# Validate test result
validate_test_result() {
    local TEST_OUTPUT=$1
    
    if [ -f "$TEST_OUTPUT" ]; then
        echo "Capture successful, audio file saved to: $TEST_OUTPUT"
        
        # Get file info
        SIZE=$(stat -c%s "$TEST_OUTPUT")
        echo "Output file size: $SIZE bytes"
        
        # Validate it's not empty or too small
        if [ $SIZE -lt 1000 ]; then
            echo "WARNING: Output file is suspiciously small!"
            return 1
        fi
        return 0
    else
        echo "ERROR: Capture failed, no output file found!"
        return 1
    fi
}
```

### 4. Unified Test Runner

Create a unified test runner in Rust that can run all tests:

```rust
// Example structure for test runner
pub struct TestRunner {
    platform: Platform,
    backends: Vec<Box<dyn AudioBackendTests>>,
    test_cases: Vec<TestCase>,
    results: Vec<TestResult>,
}

impl TestRunner {
    pub fn new() -> Self {
        let platform = detect_platform();
        let backends = get_available_backends(platform);
        let test_cases = get_standard_test_cases();
        
        Self {
            platform,
            backends,
            test_cases,
            results: Vec::new(),
        }
    }
    
    pub fn run_all_tests(&mut self) -> Result<(), Error> {
        for backend in &self.backends {
            for test_case in &self.test_cases {
                let result = self.run_test(backend, test_case)?;
                self.results.push(result);
            }
        }
        
        self.generate_report()?;
        Ok(())
    }
    
    fn run_test(&self, backend: &Box<dyn AudioBackendTests>, test_case: &TestCase) -> Result<TestResult, Error> {
        // Run test and collect result
        // ...
    }
    
    fn generate_report(&self) -> Result<(), Error> {
        // Generate test report
        // ...
    }
}
```

### 5. Docker Compose Refactoring

Simplify the docker-compose.yml file:

```yaml
version: '3'

services:
  # Base service with common configuration
  rsac-test-base:
    image: rsac-test-base
    build:
      context: .
      dockerfile: docker/base/Dockerfile
    volumes:
      - .:/app
      - ./test-results:/test-results
    environment:
      - RUST_BACKTRACE=1
      - RUST_LOG=debug
    # This is an abstract service, not meant to be run directly

  # Linux PulseAudio tests
  rsac-linux-pulseaudio:
    extends: rsac-test-base
    build:
      context: .
      dockerfile: docker/linux/Dockerfile.pulseaudio
    privileged: true
    devices:
      - /dev/snd:/dev/snd
    command: bash -c "/app/scripts/run_tests.sh pulseaudio"

  # Linux PipeWire tests
  rsac-linux-pipewire:
    extends: rsac-test-base
    build:
      context: .
      dockerfile: docker/linux/Dockerfile.pipewire
    privileged: true
    devices:
      - /dev/snd:/dev/snd
    environment:
      - XDG_RUNTIME_DIR=/run/user/0
      - PIPEWIRE_RUNTIME_DIR=/run/user/0
    command: bash -c "/app/scripts/run_tests.sh pipewire"

  # Windows tests
  rsac-windows:
    extends: rsac-test-base
    build:
      context: .
      dockerfile: docker/windows/Dockerfile
    command: bash -c "/app/scripts/run_tests.sh windows"

  # macOS tests
  rsac-macos:
    extends: rsac-test-base
    build:
      context: .
      dockerfile: docker/macos/Dockerfile
    command: bash -c "/app/scripts/run_tests.sh macos"
```

### 6. Standardized Test Examples

Create standardized test examples for each backend:

```rust
// Example for standardized test example
use clap::Parser;
use rsac::{get_audio_backend, AudioConfig, AudioFormat};
use rsac::test_utils::{generation, validation, environment, reporting};

#[derive(Parser, Debug)]
#[command(about = "Standardized audio capture test")]
struct Args {
    /// Audio backend to test
    #[arg(long, default_value = "auto")]
    backend: String,

    /// Test type (application, system)
    #[arg(long, default_value = "application")]
    test_type: String,

    /// Duration in seconds to capture
    #[arg(long, default_value = "5")]
    duration: u64,

    /// Output WAV file path
    #[arg(long, default_value = "test_capture.wav")]
    output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    // Get the appropriate backend
    let backend = if args.backend == "auto" {
        get_audio_backend()?
    } else {
        get_specific_backend(&args.backend)?
    };
    
    println!("Using audio backend: {}", backend.name());
    
    // Run the appropriate test
    match args.test_type.as_str() {
        "application" => test_application_capture(backend, args.duration, &args.output)?,
        "system" => test_system_capture(backend, args.duration, &args.output)?,
        _ => return Err("Invalid test type".into()),
    }
    
    println!("Test completed successfully!");
    Ok(())
}

fn test_application_capture(backend: Box<dyn AudioBackend>, duration: u64, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Standardized application capture test
    // ...
}

fn test_system_capture(backend: Box<dyn AudioBackend>, duration: u64, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Standardized system capture test
    // ...
}
```

## Implementation Phases

### Phase 1: Test Utilities Library

1. Create or expand `src/audio/test_utils.rs` with comprehensive utilities
2. Update existing tests to use the new utilities
3. Add documentation for the test utilities

### Phase 2: Standardized Test Cases

1. Define the `AudioBackendTests` trait
2. Implement the trait for each backend
3. Create test runner infrastructure

### Phase 3: Docker Infrastructure Refactoring

1. Create base Docker image
2. Create backend-specific Docker images
3. Create shared test scripts
4. Update docker-compose.yml

### Phase 4: Standardized Test Examples

1. Create standardized test examples for each backend
2. Update test scripts to use the standardized examples

### Phase 5: Continuous Integration Integration

1. Update CI configuration to use the new testing infrastructure
2. Add test result reporting
3. Add test coverage reporting

## Expected Benefits

1. **Reduced Duplication**: Common code moved to shared libraries
2. **Improved Maintainability**: Standardized approach makes it easier to add and modify tests
3. **Better Coverage**: Comprehensive testing across all platforms and backends
4. **Easier Debugging**: Standardized test results make it easier to identify issues
5. **Faster Development**: Reusable components speed up test development

## Conclusion

This refactoring plan aims to create a more maintainable, consistent, and effective testing infrastructure for the cross-platform audio capture library. By standardizing the approach across all platforms and backends, we can reduce duplication, improve maintainability, and ensure comprehensive test coverage.

# CI/CD Setup for Application-Specific Audio Capture

This document describes the CI/CD setup for testing the application-specific audio capture functionality across Windows, Linux, and macOS platforms.

## Overview

The CI/CD system is designed to automatically test the application capture implementation on every push and pull request. It uses GitHub Actions to run comprehensive tests across all supported platforms.

## Test Binary (`src/bin/app_capture_test.rs`)

A dedicated test binary provides automated testing capabilities without requiring interactive input. This binary is specifically designed for CI/CD environments.

### Features

- **Platform Detection**: Automatically detects the current platform and tests appropriate features
- **Non-Interactive**: All tests run without user input, suitable for automated environments
- **Comprehensive Coverage**: Tests application listing, error handling, capture lifecycle, and platform-specific features
- **Clear Exit Codes**: Returns specific exit codes for different failure types
- **Detailed Logging**: Provides clear output for debugging CI failures

### Commands

```bash
# Quick functionality test (default for CI)
cargo run --bin app_capture_test -- --quick-test

# Test application listing
cargo run --bin app_capture_test -- --list

# Test error handling with invalid inputs
cargo run --bin app_capture_test -- --test-invalid

# Test capture start/stop lifecycle
cargo run --bin app_capture_test -- --test-lifecycle

# Test platform-specific features
cargo run --bin app_capture_test -- --test-platform

# Show version and platform information
cargo run --bin app_capture_test -- --version
```

### Exit Codes

- `0`: Success
- `1`: General error
- `2`: Platform not supported
- `3`: No applications found
- `4`: Capture failed

## GitHub Actions Workflow

The workflow file `.github/workflows/application_capture_ci.yml` defines comprehensive testing across platforms.

### Jobs

#### 1. Windows Testing (`test-windows`)
- **Runner**: `windows-latest`
- **Features**: Tests WASAPI Process Loopback functionality
- **Dependencies**: Automatic (Windows SDK via `windows` crate)
- **Tests**:
  - Code formatting and linting
  - Windows-specific compilation
  - Application listing
  - Error handling
  - Platform-specific features
  - Quick functionality test

#### 2. Linux Testing (`test-linux`)
- **Runner**: `ubuntu-latest`
- **Features**: Tests PipeWire monitor streams
- **Dependencies**: 
  - `libpipewire-0.3-dev`
  - `pkg-config`
  - `build-essential`
  - `clang` and `libclang-dev`
  - `llvm-dev`
- **Tests**:
  - PipeWire installation verification
  - Linux-specific compilation
  - Application listing
  - Error handling
  - PipeWire integration
  - Quick functionality test

#### 3. macOS Testing (`test-macos`)
- **Runner**: `macos-latest`
- **Features**: Tests CoreAudio Process Tap functionality
- **Dependencies**: Automatic (macOS frameworks)
- **Tests**:
  - macOS version checking (Process Tap requires 14.4+)
  - macOS-specific compilation
  - Application listing
  - Error handling
  - Process Tap availability
  - Quick functionality test

#### 4. Cross-Platform Build (`cross-platform-build`)
- **Purpose**: Verify compilation across different targets
- **Targets**:
  - `x86_64-pc-windows-gnu`
  - `x86_64-unknown-linux-gnu`
  - `x86_64-apple-darwin` (limited)

#### 5. Integration Tests (`integration-test`)
- **Purpose**: Run comprehensive integration test suite
- **Tests**: `tests/application_capture_tests.rs`

#### 6. Documentation (`documentation`)
- **Purpose**: Verify documentation builds and examples compile
- **Tests**: Documentation generation and example compilation

## Platform-Specific Considerations

### Windows
- **Permissions**: Tests run with standard user permissions
- **Audio System**: Tests WASAPI availability and COM initialization
- **Process Discovery**: Tests process enumeration and name lookup
- **Expected Behavior**: Should find running processes and test capture creation

### Linux
- **Dependencies**: Requires PipeWire development packages
- **Audio System**: Tests PipeWire connection and node discovery
- **Permissions**: No special permissions required
- **Expected Behavior**: May not find audio applications if PipeWire is not running

### macOS
- **Version Requirements**: Process Tap requires macOS 14.4+
- **Permissions**: May require additional permissions for audio access
- **Audio System**: Tests CoreAudio framework availability
- **Expected Behavior**: Version checking and basic functionality tests

## Test Strategy

### Graceful Degradation
Tests are designed to handle environments where:
- Audio systems are not running (headless CI)
- No applications are producing audio
- Permissions are restricted
- Platform features are unavailable

### Error Handling Validation
All tests include validation of error handling:
- Invalid process IDs
- Non-existent application names
- Platform feature unavailability
- Resource cleanup

### Performance Considerations
- Tests use minimal capture durations (10-100ms)
- Quick tests prioritize speed over comprehensive coverage
- Caching is used for Cargo dependencies

## Debugging CI Failures

### Common Issues

1. **Dependency Installation Failures**
   - Check package manager commands in workflow
   - Verify package names for different distributions
   - Ensure build tools are available

2. **Compilation Errors**
   - Check feature flags are correctly applied
   - Verify platform-specific dependencies
   - Review Rust toolchain compatibility

3. **Runtime Failures**
   - Check audio system availability
   - Verify permissions and access rights
   - Review platform version requirements

4. **Test Timeouts**
   - Audio capture tests may timeout in headless environments
   - Adjust test durations if needed
   - Check for infinite loops in capture code

### Debugging Commands

```bash
# Local testing with same commands as CI
cargo run --bin app_capture_test -- --version
cargo run --bin app_capture_test -- --quick-test

# Platform-specific testing
cargo test --features feat_windows  # Windows
cargo test --features feat_linux    # Linux  
cargo test --features feat_macos    # macOS

# Integration tests
cargo test --test application_capture_tests
```

## Future Enhancements

### Potential Improvements
- Add performance benchmarking to CI
- Include memory leak detection
- Add cross-compilation testing for ARM architectures
- Include audio quality validation tests
- Add stress testing with multiple concurrent captures

### Platform Expansion
- Add support for FreeBSD
- Test on additional Linux distributions
- Add Windows ARM64 support
- Test on older macOS versions with fallback behavior

## Maintenance

### Regular Updates
- Keep GitHub Actions runner versions updated
- Update Rust toolchain versions
- Refresh system dependencies
- Review and update test timeouts

### Monitoring
- Monitor CI success rates across platforms
- Track test execution times
- Review failure patterns and common issues
- Update documentation based on CI feedback

The CI/CD setup provides comprehensive validation of the application-specific audio capture functionality while being robust enough to handle the variability of different CI environments and platform configurations.

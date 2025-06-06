# CI/CD Setup for Cross-Platform Audio Capture

This document explains the CI/CD setup for testing the rust-crossplat-audio-capture project across multiple platforms.

## Overview

The CI/CD system tests audio capture functionality on three platforms:
- **Linux**: PipeWire and PulseAudio backends
- **macOS**: CoreAudio backend  
- **Windows**: WASAPI backend

## Architecture

### GitHub Actions Workflows

1. **`ci.yml`** - Main CI workflow for basic building and testing
2. **`audio-tests.yml`** - Comprehensive cross-platform audio testing
3. **`linux.yml`** - Linux-specific audio tests (PipeWire/PulseAudio)
4. **`macos.yml`** - macOS-specific audio tests (CoreAudio)
5. **`windows.yml`** - Windows-specific audio tests (WASAPI)

### Test Examples

The CI/CD system uses specialized test examples:

- **`test_tone.rs`** - Generates test audio tones for capture testing
- **`test_capture.rs`** - Linux audio capture testing
- **`test_coreaudio.rs`** - macOS audio capture testing  
- **`test_windows.rs`** - Windows audio capture testing
- **`verify_audio.rs`** - Verifies captured audio contains expected frequencies

## Platform-Specific Setup

### Linux (Ubuntu)

**Audio Systems Tested:**
- PipeWire (modern audio server)
- PulseAudio (traditional audio server)

**Dependencies Installed:**
```bash
sudo apt-get install -y \
  libpipewire-0.3-dev \
  libspa-0.2-dev \
  libpulse-dev \
  libasound2-dev \
  pkg-config \
  pipewire \
  pipewire-audio-client-libraries \
  pulseaudio \
  firefox
```

**Virtual Audio Setup:**
- Creates virtual audio sinks and sources for testing
- Configures PipeWire/PulseAudio for headless operation
- Uses Firefox for application-specific capture testing

### macOS

**Audio System Tested:**
- CoreAudio (native macOS audio)

**Dependencies Installed:**
```bash
brew install blackhole-2ch vlc
```

**Virtual Audio Setup:**
- BlackHole provides virtual audio routing
- VLC used for application-specific capture testing
- Audio permissions pre-configured for GitHub Actions runners

### Windows

**Audio System Tested:**
- WASAPI (Windows Audio Session API)

**Dependencies Installed:**
```powershell
choco install vlc -y
```

**Audio Setup:**
- Windows Audio service verification
- VLC used for application-specific capture testing
- Tests multiple audio formats (f32le, s16le, s32le)

## Test Process

### 1. Audio Environment Setup
Each platform sets up its audio environment:
- Install audio libraries and tools
- Configure virtual audio devices
- Start background audio processes

### 2. Test Execution
For each platform, tests include:
- **System-wide capture** - Capture all system audio
- **Application-specific capture** - Capture from specific applications
- **Concurrent capture** - Multiple simultaneous captures
- **Format testing** - Different audio formats (Windows)
- **Session management** - Audio session handling (macOS)

### 3. Audio Verification
Each captured audio file is verified:
- File existence and non-zero size
- Frequency analysis to detect expected test tones
- Amplitude threshold checking

### 4. Artifact Collection
Test results are uploaded as artifacts:
- Captured audio files (.wav)
- Test logs
- Debug information

## Running Tests

### GitHub Actions
Tests run automatically on:
- Push to main/master branches
- Pull requests
- Manual workflow dispatch

### Local Testing
Use the provided script:
```bash
./scripts/test_ci_locally.sh
```

This script:
- Detects your platform
- Checks prerequisites
- Builds the project
- Runs platform-specific tests
- Verifies audio output

## Docker Support

### Linux Docker
- **File**: `docker/linux/Dockerfile`
- **Purpose**: Local testing and development
- **Usage**: `docker build -f docker/linux/Dockerfile .`

### macOS/Windows Docker
- **Status**: Experimental/Limited
- **Issue**: Complex virtualization requirements
- **Recommendation**: Use native GitHub Actions runners

## Troubleshooting

### Common Issues

1. **Audio Libraries Missing**
   - Install platform-specific development libraries
   - Check `scripts/test_ci_locally.sh` for requirements

2. **Virtual Audio Devices**
   - Linux: Ensure PipeWire/PulseAudio is running
   - macOS: Install BlackHole virtual audio driver
   - Windows: Verify Windows Audio service

3. **Permissions**
   - macOS: Audio recording permissions may be required
   - Linux: User may need audio group membership

4. **Test Failures**
   - Check audio verification logs
   - Verify test tone generation is working
   - Ensure virtual audio routing is correct

### Debug Mode
Enable debug output in workflows:
```yaml
with:
  debug_enabled: true
```

## Audio Verification

The `verify_audio.rs` example performs frequency analysis:
- Uses autocorrelation to detect dominant frequencies
- Configurable frequency tolerance
- Amplitude threshold checking
- Provides diagnostic information on failures

**Example Usage:**
```bash
cargo run --example verify_audio -- \
  --input captured_audio.wav \
  --frequency 440.0 \
  --tolerance 10.0 \
  --verbose
```

## Future Improvements

1. **Real Audio APIs**: Replace simulated capture with actual audio API calls
2. **More Test Patterns**: Add complex audio patterns for better verification
3. **Performance Testing**: Add latency and throughput measurements
4. **Cross-Platform Consistency**: Ensure identical behavior across platforms
5. **Error Recovery**: Better handling of audio system failures

## Contributing

When modifying the CI/CD setup:
1. Test locally first using `scripts/test_ci_locally.sh`
2. Update this documentation
3. Test on all platforms if possible
4. Consider backward compatibility

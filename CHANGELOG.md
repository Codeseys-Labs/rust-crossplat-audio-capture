# Changelog

## [Unreleased]

### Added

- Implemented process-specific audio capture using wasapi-rs
- Added support for capturing audio from specific applications (tested with Spotify)
- Audio capture features:
  - Process listing and selection
  - Raw audio capture in standard format (2 channels, 48000 Hz, 32-bit float)
  - Packet-based streaming with detailed logging
  - Output to both raw and WAV formats
  - Silence detection
- Logging system for debugging and monitoring capture status

### Technical Details

- Using wasapi-rs for Windows Audio Session API integration
- Process monitoring via sysinfo crate
- Audio format: 32-bit float PCM, 2 channels, 48000 Hz
- Buffer size: 3840 bytes per packet (480 frames)
- WAV file output using hound crate
- Capture performance: ~494 packets in 5 seconds with ~10% silence detection
- Average throughput: 379 KB/s

### Fixed

- Audio format compatibility issues by using standard stereo format
- Sample format conversion for WAV output
- Proper handling of silent audio packets

### Removed

- Removed unused C++ wrapper files (wrapper.cpp, wrapper.hpp)
- Removed build script (build.rs) as native bindings are no longer needed
- Removed generated bindings.rs file
- Cleaned up dependencies in Cargo.toml

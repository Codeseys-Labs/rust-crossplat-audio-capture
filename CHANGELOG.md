# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Cross-platform audio capture system:
  - Trait-based API for platform-specific backends
  - Windows WASAPI implementation
  - Legacy ProcessAudioCapture API support
  - Process enumeration and selection
  - Audio format configuration
  - Real-time audio capture
  - Example implementations:
    - Windows API comparison (trait vs legacy)
    - WAV file processing
    - Basic audio capture demo
- Improved Windows test example:
  - Added automated process audio capture test
  - Added cross-platform audio playback using rodio
  - Added comprehensive audio validation
  - Added configurable test parameters via CLI
  - Added proper cleanup of test processes
  - Added support for MP3 playback testing

### Changed

- Restructured audio capture:
  - Added trait-based abstraction for cross-platform support
  - Maintained legacy ProcessAudioCapture API for compatibility
  - Improved error handling and type safety
  - Better buffer management and synchronization
  - Platform-specific optimizations
- Updated test infrastructure:
  - Switched to rodio for cross-platform audio playback
  - Improved test reliability and validation
  - Added support for CI/CD environments
- Improved CI workflow:
  - Added cross-platform audio testing
  - Replaced external audio players with rodio
  - Added audio capture validation
  - Added test artifacts for debugging
  - Added waveform visualization

### Technical Debt

- Linux and macOS audio capture backends

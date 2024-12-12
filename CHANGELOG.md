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

### Changed

- Restructured audio capture:
  - Added trait-based abstraction for cross-platform support
  - Maintained legacy ProcessAudioCapture API for compatibility
  - Improved error handling and type safety
  - Better buffer management and synchronization
  - Platform-specific optimizations

### Technical Debt

- Linux and macOS audio capture backends

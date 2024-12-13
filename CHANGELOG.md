# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

- Reorganized CI workflows:
  - Split into platform-specific files:
    - windows.yml for Windows audio tests
    - linux.yml for Linux audio tests (PipeWire/PulseAudio)
    - macos.yml for macOS audio tests
    - code-quality.yml for shared checks
  - Improved maintainability with modular structure
  - Added reusable workflow components
  - Fixed PipeWire setup in Linux workflow

### Technical Debt

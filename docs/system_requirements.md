# System Requirements for Application-Specific Audio Capture

This document outlines the system requirements and setup steps for building and running the application-specific audio capture functionality across Windows, Linux, and macOS platforms.

## Overview

The library supports application-specific audio capture using platform-native APIs:

- **Windows**: WASAPI Process Loopback (Windows 10 build 20348+)
- **Linux**: PipeWire monitor streams (PipeWire 0.3.44+)
- **macOS**: CoreAudio Process Tap (macOS 14.4+)

## Platform-Specific Requirements

### Windows (WASAPI Process Loopback)

#### System Requirements
- **OS Version**: Windows 10 or later (for Process Loopback virtual device)
- **Architecture**: x86_64 (primary), x86 (supported)

#### Build Dependencies
- **Rust**: 1.95+ (matches the pinned CI toolchain, `dtolnay/rust-toolchain@1.95.0`)
- **Windows SDK**: Automatically handled by `windows` crate
- **Visual Studio Build Tools**: Required for linking

#### Runtime Dependencies
- **COM**: Automatically available on Windows
- **WASAPI**: Built into Windows 10+

#### Automatic Linking
The build system automatically links the following Windows libraries:
- `ole32.lib` - COM operations
- `oleaut32.lib` - VARIANT operations  
- `user32.lib` - User interface operations
- `advapi32.lib` - Advanced API operations
- `shell32.lib` - Shell operations
- `winmm.lib` - Multimedia operations

#### Verification
```powershell
# Check Windows version (should be 10.0 or higher)
winver

# Verify Visual Studio Build Tools
where cl.exe
```

### Linux (PipeWire Monitor Streams)

#### System Requirements
- **OS**: Any Linux distribution with PipeWire support
- **PipeWire Version**: 0.3.44 or later (for monitor stream features)
- **Architecture**: x86_64 (primary), aarch64 (supported)

#### Build Dependencies
```bash
# Ubuntu/Debian
sudo apt-get update
sudo apt-get install \
    libpipewire-0.3-dev \
    pkg-config \
    build-essential \
    clang \
    libclang-dev \
    llvm-dev

# Fedora/RHEL
sudo dnf install \
    pipewire-devel \
    pkg-config \
    gcc \
    clang \
    clang-devel \
    llvm-devel

# Arch Linux
sudo pacman -S \
    pipewire \
    pkg-config \
    base-devel \
    clang \
    llvm
```

#### Runtime Dependencies
- **PipeWire**: Must be running as the audio server
- **Session Manager**: WirePlumber (recommended) or pipewire-media-session

#### Automatic Installation
Set `RSAC_AUTO_INSTALL=1` environment variable to attempt automatic dependency installation:
```bash
export RSAC_AUTO_INSTALL=1
cargo build
```

#### Verification
```bash
# Check PipeWire version
pipewire --version

# Verify PipeWire is running
systemctl --user status pipewire

# Check for development headers
pkg-config --modversion libpipewire-0.3

# List available PipeWire nodes (should show applications)
pw-cli list-objects Node
```

### macOS (CoreAudio Process Tap)

#### System Requirements
- **OS Version**: macOS 14.4 (Sonoma) or later (for Process Tap APIs)
- **Architecture**: Apple Silicon (aarch64) or Intel (x86_64)

#### Build Dependencies
- **Xcode Command Line Tools**: Required for framework linking
- **Rust**: 1.95+ with macOS target support

#### Runtime Dependencies
- **CoreAudio Framework**: Built into macOS
- **AudioToolbox Framework**: Built into macOS
- **AVFoundation Framework**: Built into macOS
- **CoreFoundation Framework**: Built into macOS

#### Application Requirements
Applications using Process Tap must include in `Info.plist`:
```xml
<key>NSAudioCaptureUsageDescription</key>
<string>This application needs to capture audio from other applications for recording purposes.</string>
```

#### Automatic Linking
The build system automatically links the following macOS frameworks:
- `CoreAudio.framework` - Core audio operations
- `AudioToolbox.framework` - Audio processing tools
- `CoreFoundation.framework` - Core Foundation types
- `AVFoundation.framework` - Audio format and file handling

#### Version Detection
The build script automatically detects macOS version and warns if Process Tap APIs are not available:
```bash
# Check macOS version
sw_vers -productVersion

# Should be 14.4.0 or higher for Process Tap support
```

#### Verification
```bash
# Install Xcode Command Line Tools
xcode-select --install

# Verify frameworks are available
ls /System/Library/Frameworks/CoreAudio.framework
ls /System/Library/Frameworks/AudioToolbox.framework
ls /System/Library/Frameworks/AVFoundation.framework

# Check if running on supported macOS version
if [[ $(sw_vers -productVersion | cut -d. -f1-2) > "14.3" ]]; then
    echo "Process Tap APIs available"
else
    echo "Process Tap APIs NOT available - requires macOS 14.4+"
fi
```

## Build Configuration

### Feature Flags
The library uses platform-specific feature flags:
```toml
# Build for specific platforms only
cargo build --no-default-features --features feat_windows
cargo build --no-default-features --features feat_linux  
cargo build --no-default-features --features feat_macos

# Build for all platforms (default)
cargo build
```

### Cross-Platform Development
For development across multiple platforms:
```bash
# Check all platforms compile
cargo check --target x86_64-pc-windows-msvc
cargo check --target x86_64-unknown-linux-gnu
cargo check --target x86_64-apple-darwin
cargo check --target aarch64-apple-darwin
```

## Troubleshooting

### Windows Issues
- **"Process Loopback not supported"**: Ensure Windows 10+ and target process is audio-capable
- **COM initialization errors**: Check for conflicting COM initialization in other libraries
- **Access denied**: Run as administrator or check process permissions

### Linux Issues  
- **"PipeWire not found"**: Install development packages and ensure PipeWire is running
- **"Node not found"**: Check if target application is producing audio and visible in PipeWire graph
- **Permission denied**: Check if application has access to PipeWire socket

### macOS Issues
- **"Process Tap not available"**: Ensure macOS 14.4+ and check system version
- **Permission denied**: Add NSAudioCaptureUsageDescription to Info.plist and grant permission
- **Framework not found**: Install Xcode Command Line Tools

## Testing Setup

### Minimal Test Applications
Each platform ships diagnostic binaries and cross-platform examples for
verification (see `Cargo.toml` for the full target list):

```bash
# Windows — WASAPI session diagnostic (feature-gated bin)
cargo run --bin wasapi_session_test --no-default-features --features feat_windows

# Linux — PipeWire diagnostic (feature-gated bin)
cargo run --bin pipewire_diagnostics --no-default-features --features feat_linux

# Cross-platform examples (use the public API only; pick your platform feature)
cargo run --example list_devices  --no-default-features --features feat_linux
cargo run --example basic_capture --no-default-features --features feat_linux
cargo run --example record_to_file -- out.wav --no-default-features --features feat_linux
```

The end-to-end capture behaviour (including per-application and process-tree
capture) is exercised by the `ci_audio` integration suite; see
[CI Audio Integration Testing](CI_AUDIO_TESTING.md).

### CI/CD Considerations
- **Windows**: audio integration tests run on GitHub-hosted `windows-latest`
  with VB-CABLE (the Blacksmith Windows image has no audio subsystem, so it is
  compile/unit-only). See [`ci-audio-tests.yml`](../.github/workflows/ci-audio-tests.yml).
- **Linux**: Ubuntu 24.04 (`blacksmith-4vcpu-ubuntu-2404`) with PipeWire packages.
- **macOS**: `blacksmith-6vcpu-macos-15`; Process Tap paths skip early on managed
  runners because `kTCCServiceAudioCapture` cannot be granted non-interactively.

For detailed implementation information, see the archived
[Application-Specific Audio Capture Research](history/app_specific_capture_research.md)
(historical — written against the pre-Phase-0 API).

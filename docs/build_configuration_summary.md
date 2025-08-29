# Build Configuration Summary for Application-Specific Audio Capture

This document summarizes the enhanced build configuration and dependencies added to support application-specific audio capture across Windows, Linux, and macOS platforms.

## Overview

We have successfully enhanced the build system with platform-specific dependencies and build scripts to support the application-specific audio capture implementations researched and documented in [app_specific_capture_research.md](app_specific_capture_research.md).

## Enhanced Dependencies (Cargo.toml)

### Windows Dependencies
```toml
[target.'cfg(target_os = "windows")'.dependencies]
wasapi = "0.19.0"  # WASAPI bindings for Process Loopback
windows = { version = "0.61.3", features = [
    "Foundation",                 # For basic Windows types like HRESULT, BOOL, HANDLE
    "Win32_Foundation",           # For basic Windows types like HRESULT, BOOL, HANDLE
    "Win32_System_Com",           # For COM initialization (CoInitializeEx, CoUninitialize)
    "Win32_Media_Audio",          # For WASAPI interfaces and Process Loopback specific APIs
    "Win32_Devices_FunctionDiscovery",  # For device enumeration
    "Win32_Devices_Properties",         # For device properties
    "Win32_UI_Shell_PropertiesSystem",  # For property system
    "Win32_System_Com_StructuredStorage", # For structured storage
    "Win32_Media_KernelStreaming",      # For kernel streaming
    "Win32_Media_Multimedia",           # For multimedia APIs
    "Win32_System_Threading",           # For process operations
    "Win32_System_ProcessStatus",       # For process status
    "Win32_System_Variant",             # For PROPVARIANT used in Process Loopback activation
    "Win32_Security",                   # For security APIs
] }
windows-core = "0.61"  # Core Windows types
widestring = "1.1.0"   # For wide string conversion
sysinfo = "0.35.2"     # For process discovery by name
```

**Key Features Enabled:**
- WASAPI Process Loopback APIs (AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS)
- COM operations for audio client activation
- Process discovery and management
- Wide string handling for Windows APIs

### Linux Dependencies
```toml
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = { version = "0.8.0", features = ["v0_3_44"] }  # PipeWire bindings with v0.3.44+ features
libspa = "0.8.0"      # SPA (Simple Plugin API) for format negotiation
libspa-sys = "0.8.0"  # Low-level SPA bindings for direct parameter access
```

**Key Features Enabled:**
- PipeWire 0.3.44+ monitor stream functionality
- SPA parameter handling for format negotiation
- Node discovery and targeting capabilities
- Stream property configuration (TARGET_OBJECT, STREAM_MONITOR)

### macOS Dependencies
```toml
[target.'cfg(target_os = "macos")'.dependencies]
coreaudio-rs = { version = "0.13.0", features = ["core_audio"] }  # High-level CoreAudio bindings
objc2-core-audio = { version = "0.3", features = [
    "std",
    "AudioHardware",              # For AudioHardwareCreateProcessTap, AudioHardwareCreateAggregateDevice
    "AudioHardwareDeprecated",    # For additional hardware APIs
] }
objc2-core-audio-types = { version = "0.3", features = [
    "std", 
    "bitflags",
    "CoreAudioBaseTypes",         # For AudioStreamBasicDescription, AudioObjectID
] }
objc2-core-foundation = { version = "0.3", features = [
    "std",
    "CFString",                   # For CFString handling in device UIDs
] }
core-foundation = "0.10"          # For CFDictionary creation (aggregate device description)
```

**Key Features Enabled:**
- CoreAudio hardware APIs for Process Tap creation
- Objective-C bindings for modern CoreAudio APIs
- Core Foundation types for aggregate device configuration
- AudioObject property access and manipulation

## Enhanced Build Script (build.rs)

### Platform-Specific Build Configuration

#### Windows Build Configuration
```rust
#[cfg(target_os = "windows")]
fn configure_windows_build() {
    // Windows-specific build configuration for WASAPI Process Loopback
    println!("cargo:rustc-link-lib=ole32");      // For COM operations
    println!("cargo:rustc-link-lib=oleaut32");   // For VARIANT operations
    println!("cargo:rustc-link-lib=user32");     // For user interface operations
    println!("cargo:rustc-link-lib=advapi32");   // For advanced API operations
    println!("cargo:rustc-link-lib=shell32");    // For shell operations
    println!("cargo:rustc-link-lib=winmm");      // For multimedia operations
}
```

#### Linux Build Configuration
```rust
#[cfg(target_os = "linux")]
fn configure_linux_build() {
    // Enhanced Linux build configuration for PipeWire application capture
    // Checks for PipeWire 0.3.44+ with automatic installation support
    // Provides clear error messages and installation instructions
}
```

#### macOS Build Configuration
```rust
#[cfg(target_os = "macos")]
fn configure_macos_build() {
    // macOS-specific build configuration for CoreAudio Process Tap
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=AudioToolbox");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    
    // Runtime macOS version checking for Process Tap API availability
}
```

## Code Stub Integration

### Windows Application Capture
- **File**: `src/audio/windows.rs`
- **Struct**: `WindowsApplicationCapture`
- **Dependencies Used**: `wasapi`, `windows`, `sysinfo`
- **Key APIs**: AUDIOCLIENT_ACTIVATION_PARAMS, Process Loopback activation

### Linux Application Capture
- **File**: `src/audio/linux/pipewire.rs`
- **Struct**: `PipeWireApplicationCapture`
- **Dependencies Used**: `pipewire`, `libspa`, `libspa-sys`
- **Key APIs**: Stream creation with TARGET_OBJECT, monitor streams

### macOS Application Capture
- **File**: `src/audio/macos/tap.rs`
- **Struct**: `MacOSApplicationCapture`
- **Dependencies Used**: `objc2-core-audio`, `core-foundation`
- **Key APIs**: AudioHardwareCreateProcessTap, Aggregate Device creation

## Verification Results

### Dependency Resolution
✅ **Successful**: `cargo tree --no-default-features` shows all dependencies resolve correctly:
- PipeWire dependencies: `pipewire v0.8.0`, `libspa v0.8.0`, `libspa-sys v0.8.0`
- Build system dependencies: All platform-specific dependencies downloading successfully
- No dependency conflicts or version issues

### Build Script Validation
✅ **Successful**: Enhanced build.rs with:
- Platform-specific library linking
- Runtime version checking (macOS)
- Automatic dependency installation support (Linux)
- Clear error messages and installation guidance

### Code Integration
✅ **Successful**: All platform-specific code stubs:
- Compile without syntax errors
- Have access to required dependencies
- Include comprehensive TODO markers for implementation
- Follow established patterns from research

## System Requirements Documentation

Created comprehensive system requirements documentation:
- **File**: `docs/system_requirements.md`
- **Content**: Platform-specific setup instructions, dependency installation, verification steps
- **Coverage**: Windows, Linux, and macOS requirements with troubleshooting guides

## Next Steps for Implementation

1. **Windows**: Implement WASAPI Process Loopback activation using the configured dependencies
2. **Linux**: Implement PipeWire node discovery and monitor stream creation
3. **macOS**: Implement Process Tap creation and aggregate device setup
4. **Testing**: Create platform-specific test suites using the configured build system
5. **CI/CD**: Set up automated testing with the enhanced build configuration

## Validation Commands

To verify the build configuration on each platform:

```bash
# Check dependency resolution
cargo tree --features feat_windows  # Windows-specific deps
cargo tree --features feat_linux    # Linux-specific deps  
cargo tree --features feat_macos    # macOS-specific deps

# Verify build script execution
cargo clean && cargo check --features feat_<platform>

# Test platform-specific compilation
cargo check --target x86_64-pc-windows-msvc     # Windows
cargo check --target x86_64-unknown-linux-gnu   # Linux
cargo check --target x86_64-apple-darwin        # macOS
```

## Summary

The build configuration has been successfully enhanced to support application-specific audio capture across all three platforms. The dependencies are correctly specified, the build scripts handle platform-specific requirements, and the code stubs are ready for implementation. The system provides a solid foundation for implementing the researched audio capture techniques while maintaining cross-platform compatibility and clear error handling.

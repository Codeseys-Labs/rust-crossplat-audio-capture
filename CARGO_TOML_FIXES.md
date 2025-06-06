# 🔧 Cargo.toml Quick Fixes

## Issue
The merged master branch is missing the feature definitions and new examples that were in our refactor branch.

## Required Changes

### 1. Add Features Section
Add this after the `[package]` section:

```toml
[features]
default = []
feat_linux = ["libpulse-binding", "libpulse-simple-binding"]
feat_macos = ["coreaudio-rs"]
feat_windows = ["wasapi", "windows"]
test-utils = ["rodio"]
```

### 2. Fix PipeWire Dependency (Temporary)
In the Linux dependencies section, comment out PipeWire:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
# libasound2-dev && libpipewire-0.3-dev && pkg-config && build-essential && clang && libclang-dev && llvm-dev
libpulse-binding = "2.28.2"
libpulse-simple-binding = "2.28.1"
# pipewire = "0.8.0"  # Temporarily disabled due to Ubuntu 22.04 compatibility
```

### 3. Add Missing Examples
Add these example definitions before the `[workspace]` section:

```toml
[[example]]
name = "test_capture"
path = "examples/test_capture.rs"
required-features = ["feat_linux"]

[[example]]
name = "test_coreaudio"
path = "examples/test_coreaudio.rs"
required-features = ["feat_macos"]

[[example]]
name = "test_windows"
path = "examples/test_windows.rs"
required-features = ["feat_windows"]

[[example]]
name = "verify_audio"
path = "examples/verify_audio.rs"

[[example]]
name = "demo_library"
path = "examples/demo_library.rs"

[[example]]
name = "test_tone"
path = "examples/test_tone.rs"
required-features = ["test-utils"]
```

## Complete Fixed Cargo.toml

Here's what the complete file should look like:

```toml
[package]
name = "rsac"
version = "0.1.0"
edition = "2021"

[features]
default = []
feat_linux = ["libpulse-binding", "libpulse-simple-binding"]
feat_macos = ["coreaudio-rs"]
feat_windows = ["wasapi", "windows"]
test-utils = ["rodio"]

[dependencies]
# Core dependencies
hound = "3.5.1"
sysinfo = "0.33.1"
clap = { version = "4.5.27", features = ["derive"] }
inquire = "0.7.5"
indicatif = "0.17.11"
color-eyre = "0.6.3"
ctrlc = "3.4.5"
rodio = { version = "0.20.1", optional = true }

# Platform-specific audio capture
[target.'cfg(target_os = "windows")'.dependencies]
wasapi = { version = "0.16.0", optional = true }
windows = { version = "0.60.0", features = [
    "Win32_System_Com",
    "Win32_Media_Audio",
    "Win32_Media_Audio_Endpoints",
    "Win32_Media_MediaFoundation",
    "Win32_Foundation",
    "Win32_System_Threading",
    "Win32_Devices_FunctionDiscovery",
    "Win32_UI_Shell_PropertiesSystem",
    "Win32_System_Com_StructuredStorage",
], optional = true }

[target.'cfg(target_os = "linux")'.dependencies]
# libasound2-dev && libpipewire-0.3-dev && pkg-config && build-essential && clang && libclang-dev && llvm-dev
libpulse-binding = { version = "2.28.2", optional = true }
libpulse-simple-binding = { version = "2.28.1", optional = true }
# pipewire = "0.8.0"  # Temporarily disabled due to Ubuntu 22.04 compatibility

[target.'cfg(target_os = "macos")'.dependencies]
coreaudio-rs = { version = "0.12.1", optional = true }

# Error handling
thiserror = "2.0.11"

[dev-dependencies]
criterion = "0.5.1" # Benchmarking
tempfile = "3.16.0" # Temporary files for tests

[[example]]
name = "wav_demo"
path = "examples/wav_demo.rs"

[[example]]
name = "test_windows"
path = "examples/test_windows.rs"
required-features = ["feat_windows"]

[[example]]
name = "test_pulseaudio"
path = "examples/test_pulseaudio.rs"
required-features = ["feat_linux"]

[[example]]
name = "test_coreaudio"
path = "examples/test_coreaudio.rs"
required-features = ["feat_macos"]

[[example]]
name = "test_coreaudio_app"
path = "examples/test_coreaudio_app.rs"
required-features = ["feat_macos"]

[[example]]
name = "test_tone"
path = "examples/test_tone.rs"
required-features = ["test-utils"]

[[example]]
name = "test_capture"
path = "examples/test_capture.rs"
required-features = ["feat_linux"]

[[example]]
name = "verify_audio"
path = "examples/verify_audio.rs"

[[example]]
name = "demo_library"
path = "examples/demo_library.rs"

[workspace]
members = []
```

## How to Apply

1. Go to your repository on GitHub
2. Navigate to `Cargo.toml`
3. Click "Edit this file"
4. Replace the entire content with the fixed version above
5. Commit the changes

## Expected Result

After applying these fixes:
- ✅ Features will be properly defined
- ✅ Examples will build with correct feature requirements
- ✅ PipeWire compatibility issues will be avoided
- ✅ CI/CD should build successfully (except for missing examples that need PipeWire)

## Testing

After applying the fix, test locally:
```bash
# Test basic build
cargo check --features feat_linux

# Test examples
cargo check --example demo_library
cargo check --example verify_audio
cargo check --example test_tone --features test-utils
```

This should resolve the immediate build issues and get the CI/CD working! 🚀

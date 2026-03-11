# Reference Repositories

This directory contains git submodules of reference implementations used to guide the development of `rsac` (Rust Cross-Platform Audio Capture).

## Quick Start

```bash
# Initialize all reference submodules
git submodule update --init --recursive

# Initialize a specific reference
git submodule update --init reference/wasapi-rs

# Update all references to latest
git submodule update --remote --merge
```

## Windows (WASAPI) References

### wasapi-rs
- **URL:** https://github.com/HEnquist/wasapi-rs
- **Language:** Rust
- **Description:** Safe Rust WASAPI bindings. Contains `examples/record_application.rs` showing per-application recording via WASAPI process loopback.
- **Key files:** `examples/record_application.rs`, `src/`
- **Relevance:** Direct reference for rsac's Windows backend — WASAPI initialization, COM threading, audio buffer handling, process loopback capture.

### camilladsp
- **URL:** https://github.com/HEnquist/camilladsp
- **Language:** Rust
- **Description:** Cross-platform audio DSP pipeline by the same author as wasapi-rs. Uses WASAPI, CoreAudio, ALSA backends.
- **Key files:** `src/wasapidevice.rs`, `src/coreaudiodevice.rs`, `src/audiodevice.rs`
- **Relevance:** Production-grade cross-platform audio pipeline architecture. Demonstrates trait-based backend abstraction, dual-tiered threading model, and real-time thread priority.

## Linux (PipeWire) References

### wiremix
- **URL:** https://github.com/tsowell/wiremix
- **Language:** Rust
- **Description:** PipeWire-based application audio capture tool.
- **Key files:** `src/`
- **Relevance:** Demonstrates PipeWire node discovery, stream capture setup, and the monitor/recording pattern for per-application audio on Linux.

### pipewire-rs
- **URL:** https://gitlab.freedesktop.org/pipewire/pipewire-rs
- **Language:** Rust
- **Description:** Official PipeWire Rust bindings. rsac's direct dependency for the Linux backend.
- **Key files:** `pipewire/examples/`, `pipewire/src/stream.rs`, `pipewire/src/main_loop.rs`, `pipewire/src/channel.rs`, `pipewire/src/registry.rs`
- **Relevance:** Understanding ownership model, listener callbacks, `!Send + !Sync` constraints, cross-thread communication via `pipewire::channel`.

### obs-pipewire-audio-capture
- **URL:** https://github.com/dimtpap/obs-pipewire-audio-capture
- **Language:** C
- **Description:** OBS Studio plugin for per-application and per-device audio capture on Linux via PipeWire.
- **Key files:** `src/pipewire-audio.c`, `src/pipewire-audio-capture-app.c`, `src/pipewire-audio-capture-device.c`
- **Relevance:** Battle-tested PipeWire per-application capture. Shows app tracking, dynamic reconnection, node property matching, and WirePlumber integration.

## macOS (CoreAudio Process Tap) References

### AudioCap
- **URL:** https://github.com/insidegui/AudioCap
- **Language:** Swift
- **Description:** macOS audio capture using CoreAudio Process Tap API (macOS 14.4+).
- **Key files:** Source files showing CATapDescription, aggregate device creation, tap lifecycle
- **Relevance:** High-level reference for the Process Tap workflow: tap creation → aggregate device wiring → AudioUnit capture.

### audio-rec
- **URL:** https://github.com/DSRCorporation/audio-rec
- **Language:** Objective-C++ / C++
- **Description:** CoreAudio Process Tap implementation in C++/ObjC++, showing the raw C API calls.
- **Key files:** Source files showing AudioHardwareCreateProcessTap(), CATapDescription, aggregate device setup
- **Relevance:** Shows the raw C/C++ API calls that rsac must invoke through Rust FFI. Demonstrates process enumeration, tap lifecycle, and error handling for CoreAudio.

### screencapturekit-rs
- **URL:** https://github.com/doom-fish/screencapturekit-rs
- **Language:** Rust
- **Description:** Rust bindings for Apple's ScreenCaptureKit, supporting application audio capture on macOS 12.3+.
- **Key files:** `src/stream/`, `src/output/`, `src/content_filter/`, `screencapturekit-sys/`
- **Relevance:** Demonstrates Objective-C to Rust FFI bridging patterns. Potential fallback backend for macOS (ScreenCaptureKit vs Process Tap).

## Cross-Platform / Infrastructure References

### cpal
- **URL:** https://github.com/RustAudio/cpal
- **Language:** Rust
- **Description:** The most widely-used cross-platform audio I/O library in Rust. Supports WASAPI, CoreAudio, ALSA, JACK.
- **Key files:** `src/host/wasapi/`, `src/host/coreaudio/`, `src/traits.rs`, `src/platform/mod.rs`
- **Relevance:** Definitive reference for cross-platform audio API design patterns — device enumeration, stream configuration, format negotiation, platform dispatch.

### rtrb
- **URL:** https://github.com/mgeier/rtrb
- **Language:** Rust
- **Description:** Wait-free SPSC ring buffer designed for real-time audio. rsac's dependency for BridgeStream.
- **Key files:** `src/lib.rs`, `src/chunks.rs`, `benches/`
- **Relevance:** Understanding bulk chunk read/write API, backpressure semantics, memory ordering — directly affects BridgeStream implementation.

# Audio Capture Library Architecture

## Overview
This document describes the architecture of the cross-platform audio capture library.

## Core Components

### AudioCapture Trait
The core abstraction for audio capture functionality across all platforms:
```rust
pub trait AudioCapture: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<(), AudioError>;
    fn is_capturing(&self) -> bool;
    fn get_available_devices(&self) -> Result<Vec<AudioDevice>, AudioError>;
    fn set_device(&mut self, device: AudioDevice) -> Result<(), AudioError>;
}
```

### Platform-Specific Implementations
- Windows: WASAPI-based implementation
- macOS: CoreAudio-based implementation
- Linux: PipeWire and PulseAudio implementations with automatic backend selection

## Audio Pipeline

```mermaid
graph LR
    A[Audio Source] --> B[Platform Backend]
    B --> C[Ring Buffer]
    C --> D[Audio Processing]
    D --> E[Output Format]
```

### Data Flow
1. Audio is captured from system or application
2. Platform-specific backend processes raw audio data
3. Data is stored in a lock-free ring buffer
4. Optional processing (format conversion, resampling)
5. Output in requested format (WAV, raw PCM)

## Error Handling
The library uses a custom `AudioError` type for comprehensive error handling:
- Device-related errors
- Initialization errors
- Runtime capture errors
- Backend-specific errors

## Configuration
Configuration is handled through the `AudioCaptureConfig` builder pattern:
- Sample rate
- Channel count
- Buffer size
- Device selection
- Application targeting (for app-specific capture)

## Performance Considerations
- Lock-free data structures for audio buffers
- SIMD optimizations for audio processing
- Parallel processing capabilities
- Minimal allocations in hot paths

## Platform-Specific Details

### Windows (WASAPI)
- Uses Windows Audio Session API
- Supports both shared and exclusive mode
- Application-specific capture via process ID

### macOS (CoreAudio)
- Uses Audio Unit API
- System-wide and application-specific capture
- Integration with macOS audio system

### Linux (PipeWire/PulseAudio)
- Automatic backend selection
- PipeWire preferred when available
- PulseAudio fallback support
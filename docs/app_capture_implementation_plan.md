# Application-Specific Audio Capture Implementation Plan

This document outlines the implementation plan for application-specific audio capture across Windows, Linux, and macOS platforms, based on our research and the code stubs created.

## Overview

We have enhanced our library with platform-specific application capture capabilities:

- **Windows**: WASAPI Process Loopback for PID-based capture
- **Linux**: PipeWire monitor streams for application node targeting  
- **macOS**: CoreAudio Process Tap with Aggregate Device (macOS 14.4+)

## Documentation Updates

### Enhanced Research Document
- **File**: `docs/app_specific_capture_research.md`
- **Content**: Detailed technical analysis of each platform's approach
- **Includes**: API references, code examples, implementation flows, constraints, and error handling patterns

### Architecture Integration
- **File**: `docs/ARCHITECTURE.md`
- **Addition**: Cross-reference to application capture research
- **Context**: Explains how app-specific capture fits into overall library architecture

## Code Stubs Created

### Windows (WASAPI Process Loopback)
- **File**: `src/audio/windows.rs`
- **Struct**: `WindowsApplicationCapture`
- **Key Features**:
  - Process ID targeting with optional process tree inclusion
  - Event-driven capture with resilient buffering
  - Helper for process discovery by name
  - Comprehensive TODO markers for WASAPI implementation

**Key APIs to Implement**:
```rust
// Core activation with AUDIOCLIENT_ACTIVATION_PARAMS
pub fn initialize(&mut self, format: &AudioFormat) -> AudioResult<()>

// Event-driven capture loop
pub fn start_capture<F>(&mut self, callback: F) -> AudioResult<()>

// Process discovery helper
pub fn find_process_by_name(process_name: &str) -> Option<u32>
```

### Linux (PipeWire Monitor Streams)
- **File**: `src/audio/linux/pipewire.rs`
- **Struct**: `PipeWireApplicationCapture`
- **Key Features**:
  - Flexible application targeting (PID, name, node ID)
  - Non-invasive monitor stream creation
  - Application enumeration and discovery
  - Integration with existing PipeWire infrastructure

**Key APIs to Implement**:
```rust
// Node discovery and targeting
pub fn discover_target_node(&mut self) -> Result<u32, AudioError>

// Monitor stream with TARGET_OBJECT and STREAM_MONITOR
pub fn create_monitor_stream(&mut self) -> Result<(), AudioError>

// Application listing for user selection
pub fn list_audio_applications() -> Result<Vec<LinuxApplicationInfo>, AudioError>
```

### macOS (CoreAudio Process Tap)
- **File**: `src/audio/macos/tap.rs`
- **Struct**: `MacOSApplicationCapture`
- **Key Features**:
  - Process Tap creation with mute behavior control
  - Aggregate Device setup with tap integration
  - I/O proc-based audio processing
  - Application enumeration for user selection

**Key APIs to Implement**:
```rust
// PID to AudioObjectID translation
pub fn translate_pid_to_process_object(&self) -> AudioResult<sys::AudioObjectID>

// Process tap and aggregate device creation
pub fn create_process_tap(&mut self) -> AudioResult<sys::AudioObjectID>
pub fn create_aggregate_device(&mut self) -> AudioResult<sys::AudioObjectID>

// I/O proc-based capture
pub fn start_capture<F>(&mut self, callback: F) -> AudioResult<()>
```

## Implementation Priority

### Phase 1: Core Functionality
1. **Windows**: Implement WASAPI Process Loopback activation and basic capture
2. **Linux**: Implement PipeWire node discovery and monitor stream creation
3. **macOS**: Implement Process Tap creation and basic aggregate device setup

### Phase 2: Enhanced Features
1. **Cross-platform**: Application discovery and enumeration
2. **Error handling**: Platform-specific error mapping and user-friendly messages
3. **Format handling**: Automatic format conversion and resampling

### Phase 3: Integration and Testing
1. **API unification**: Common interface across platforms
2. **Testing**: Platform-specific test suites and CI integration
3. **Documentation**: User guides and examples

## Key Implementation Notes

### Windows Considerations
- Many AudioClient methods are non-functional in Process Loopback mode
- Use resilient buffering strategies (VecDeque) due to unreliable buffer queries
- Handle process tree inclusion carefully (use parent PID)
- Implement proper COM initialization (MTA)

### Linux Considerations  
- Requires PipeWire (not just PulseAudio compatibility)
- Node visibility depends on session manager (WirePlumber) and portals
- Handle dynamic node appearance/disappearance
- Implement robust property matching for application discovery

### macOS Considerations
- Requires macOS 14.4+ (runtime version checks needed)
- NSAudioCaptureUsageDescription required in Info.plist
- No public permission preflight API (first use triggers prompt)
- Proper cleanup sequence critical to avoid resource leaks
- Consider exposing mute behavior and drift compensation options

## Testing Strategy

### Unit Tests
- Mock implementations for each platform's core APIs
- Format conversion and buffer handling tests
- Error condition simulation and handling

### Integration Tests
- Real application capture on each platform
- Cross-platform format compatibility
- Performance and latency measurements

### CI/CD Integration
- Platform-specific test runners
- Manual test procedures for permission-dependent features
- Documentation of test app requirements per platform

## Next Steps

1. **Choose implementation order** based on platform priority and complexity
2. **Set up development environment** for each target platform
3. **Implement core APIs** following the research-based patterns
4. **Create minimal examples** demonstrating each platform's capabilities
5. **Integrate with existing library architecture** and error handling
6. **Add comprehensive tests** and documentation

## References

- [Application-Specific Audio Capture Research](app_specific_capture_research.md)
- [Library Architecture](ARCHITECTURE.md)
- External repositories analyzed:
  - [HEnquist/wasapi-rs](https://github.com/HEnquist/wasapi-rs) (Windows WASAPI)
  - [tsowell/wiremix](https://github.com/tsowell/wiremix) (Linux PipeWire)
  - [insidegui/AudioCap](https://github.com/insidegui/AudioCap) (macOS CoreAudio)

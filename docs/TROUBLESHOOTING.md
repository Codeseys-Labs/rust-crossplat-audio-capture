# Troubleshooting Guide

## Common Issues

### No Audio Devices Found

#### Windows
```
Error: DeviceNotFound("No audio capture devices available")
```

Solutions:
1. Check Device Manager
2. Verify WASAPI service is running
3. Update audio drivers
4. Run with administrator privileges

#### Linux
```
Error: BackendError("Failed to connect to audio server")
```

Solutions:
1. Check PulseAudio/PipeWire status:
   ```bash
   systemctl --user status pipewire
   systemctl --user status pulseaudio
   ```
2. Install required packages:
   ```bash
   # Ubuntu/Debian
   sudo apt-get install libpulse-dev
   
   # Fedora
   sudo dnf install pipewire-devel
   ```

#### macOS
```
Error: DeviceInitError("Failed to initialize CoreAudio")
```

Solutions:
1. Check Privacy & Security settings
2. Reset CoreAudio:
   ```bash
   sudo killall coreaudiod
   ```
3. Verify permissions

### Capture Failures

#### Permission Errors
```
Error: CaptureError("Access denied")
```

Solutions:
1. Windows: Run as administrator
2. Linux: Add user to audio group
3. macOS: Grant privacy permissions

#### Buffer Overruns
```
Error: CaptureError("Buffer overrun detected")
```

Solutions:
1. Increase buffer size
2. Reduce processing overhead
3. Check system load

## Debugging

### Enable Debug Logging
```rust
use env_logger::{Builder, Env};

Builder::from_env(Env::default().default_filter_or("debug"))
    .init();
```

### Capture Debug Info
```rust
fn collect_debug_info() -> String {
    format!(
        "Platform: {}\n\
         Backend: {}\n\
         Sample Rate: {}\n\
         Channels: {}\n\
         Buffer Size: {}\n",
        std::env::consts::OS,
        audio_backend(),
        sample_rate(),
        channels(),
        buffer_size()
    )
}
```

### Performance Issues

#### High Latency
Symptoms:
- Delayed audio capture
- Buffer underruns

Solutions:
1. Reduce buffer size
2. Optimize processing
3. Check system performance

#### High CPU Usage
Symptoms:
- System slowdown
- Capture glitches

Solutions:
1. Use SIMD optimizations
2. Implement buffer pooling
3. Profile hot paths

## Platform-Specific Notes

### Windows
- WASAPI requires Windows 7+
- Exclusive mode needs admin rights
- Check application permissions

### Linux
- PipeWire preferred over PulseAudio
- Check audio server logs
- Verify ALSA configuration

### macOS
- CoreAudio permissions required
- Check Security & Privacy
- Verify audio device settings

## Error Messages

### Format
```
[Component] Error Message
Details: Additional information
Platform: Windows/Linux/macOS
```

### Common Errors

1. Device Initialization
```
[Init] Failed to initialize audio device
Details: Device busy or unavailable
```

2. Capture Start
```
[Capture] Failed to start capture
Details: Invalid configuration
```

3. Buffer Management
```
[Buffer] Buffer overflow
Details: Processing too slow
```

## Logging

### Log Levels
```rust
error!("Critical error: {}", err);
warn!("Performance warning: {}", msg);
info!("Capture started: {}", config);
debug!("Buffer status: {}", status);
trace!("Processing sample: {}", sample);
```

### Log Output
```
2024-01-20T10:15:30Z ERROR [audio_capture] Failed to open device: Device busy
2024-01-20T10:15:31Z WARN  [audio_capture] Buffer utilization high: 85%
```

## Reporting Issues

Include:
1. Error message and stack trace
2. System information
3. Audio device details
4. Steps to reproduce
5. Debug logs

Example:
```
Platform: Windows 10
Backend: WASAPI
Error: DeviceNotFound
Log:
[2024-01-20 10:15:30] ERROR: No devices available
[2024-01-20 10:15:30] DEBUG: Enumerated 0 devices
```
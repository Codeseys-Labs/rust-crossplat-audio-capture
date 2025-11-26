# Platform-Specific Audio Testing

This document describes how to test the Rust audio capture library and `dynamic_vlc` example on Linux (PipeWire), Windows, and macOS using Docker containers.

## Overview

The testing setup provides:

- **Linux PipeWire**: Native PipeWire audio capture with VLC integration
- **Windows**: Full Windows environment with WASAPI/DirectSound and VLC
- **macOS**: macOS container with Core Audio and VLC support

Each platform tests:
1. Library compilation with platform-specific features
2. `dynamic_vlc` example compilation and execution
3. VLC audio playback and capture integration
4. Platform-specific audio system functionality

## Quick Start

### Prerequisites

- Docker and Docker Compose
- At least 16GB RAM (for running multiple containers)
- KVM support (for Windows and macOS containers)

### Run All Platform Tests

```bash
# Start all platform testing
make test-all-platforms

# Or use Docker Compose directly
docker-compose -f docker-compose.testing.yml up --build
```

### Run Individual Platform Tests

```bash
# Linux PipeWire testing
make test-linux-pipewire

# Windows testing (manual setup required)
make test-windows-manual

# macOS Core Audio testing
make test-macos-coreaudio

# Run test orchestrator
make test-orchestrate
```

## Platform Details

### 1. Linux PipeWire Testing

**Container**: `linux-pipewire-test`
**Base Image**: Ubuntu 22.04 with PipeWire
**Features Tested**: `feat_linux`

**What it tests**:
- PipeWire audio system setup
- Library compilation with Linux features
- `dynamic_vlc` example compilation
- VLC audio playback
- Audio capture functionality
- PipeWire integration

**Automated**: ✅ Fully automated testing

```bash
# Run Linux testing
docker-compose -f docker-compose.testing.yml up linux-pipewire-test
```

### 2. Windows Testing

**Container**: `windows-test`
**Base Image**: `dockurr/windows` (Windows 11)
**Features Tested**: `feat_windows`

**What it tests**:
- Windows audio system (WASAPI, DirectSound)
- Rust toolchain installation
- Library compilation with Windows features
- `dynamic_vlc` example compilation
- VLC installation and audio playback
- Windows-specific audio capture

**Manual Setup Required**: ⚠️ Requires manual interaction

**Steps**:
1. Start container: `make test-windows-manual`
2. Access Windows at http://localhost:8006
3. Wait for Windows to boot (5-10 minutes)
4. Run setup script: `C:\scripts\setup-windows-test.ps1`
5. Run tests: `C:\test-windows.ps1`

### 3. macOS Core Audio Testing

**Container**: `macos-test`
**Base Image**: `sickcodes/docker-osx:monterey`
**Features Tested**: `feat_macos`

**What it tests**:
- Core Audio system
- Library compilation with macOS features
- `dynamic_vlc` example compilation
- VLC installation and audio playback
- BlackHole audio driver setup
- macOS-specific audio capture

**Automated**: ✅ Fully automated testing (takes longer to start)

```bash
# Run macOS testing
docker-compose -f docker-compose.testing.yml up macos-test
```

## Test Results

### Result Structure

```
test-results/
├── linux-pipewire/
│   ├── audio_system_test_TIMESTAMP.log
│   ├── library_build_TIMESTAMP.log
│   ├── dynamic_vlc_build_TIMESTAMP.log
│   ├── vlc_test_TIMESTAMP.log
│   ├── audio_capture_test_TIMESTAMP.log
│   ├── dynamic_vlc_test_TIMESTAMP.log
│   ├── test_summary_TIMESTAMP.json
│   └── test_report_TIMESTAMP.html
├── windows/
│   ├── manual_test_ready_TIMESTAMP.txt
│   └── windows_test_TIMESTAMP.log (after manual testing)
├── macos-coreaudio/
│   ├── audio_system_test_TIMESTAMP.log
│   ├── library_build_TIMESTAMP.log
│   ├── dynamic_vlc_build_TIMESTAMP.log
│   ├── vlc_test_TIMESTAMP.log
│   ├── blackhole_test_TIMESTAMP.log
│   ├── audio_capture_test_TIMESTAMP.log
│   ├── dynamic_vlc_test_TIMESTAMP.log
│   ├── test_summary_TIMESTAMP.json
│   └── test_report_TIMESTAMP.html
└── reports/
    ├── comprehensive_test_report_TIMESTAMP.html
    └── test_summary_TIMESTAMP.json
```

### Viewing Results

- **HTML Reports**: Open `test_report_*.html` files in browser
- **JSON Summaries**: Machine-readable test results
- **Log Files**: Detailed output from each test phase

## Container Configuration

### Linux PipeWire Container

```yaml
linux-pipewire-test:
  build:
    dockerfile: docker/testing/Dockerfile.linux-pipewire
  volumes:
    - /dev/snd:/dev/snd  # Audio device access
    - /run/user/1000/pipewire-0:/run/user/1000/pipewire-0:ro
  environment:
    - XDG_RUNTIME_DIR=/run/user/1000
    - PIPEWIRE_RUNTIME_DIR=/run/user/1000
  privileged: true
```

### Windows Container

```yaml
windows-test:
  image: dockurr/windows
  environment:
    VERSION: "11"
    RAM_SIZE: "6G"
    CPU_CORES: "4"
  devices:
    - /dev/kvm
  ports:
    - "8006:8006"  # Web interface
```

### macOS Container

```yaml
macos-test:
  build:
    dockerfile: docker/testing/Dockerfile.macos
  devices:
    - /dev/kvm
  environment:
    - SHORTNAME=monterey
  ports:
    - "50922:10022"  # SSH
```

## Testing Workflow

### Automated Testing (Linux, macOS)

1. **Container Startup**: Platform-specific container starts
2. **Environment Setup**: Audio system and dependencies
3. **Library Build**: Compile with platform features
4. **Example Build**: Compile `dynamic_vlc` example
5. **VLC Testing**: Test VLC installation and playback
6. **Audio Capture**: Test audio capture functionality
7. **Report Generation**: Create HTML and JSON reports

### Manual Testing (Windows)

1. **Container Startup**: Windows container starts
2. **Windows Boot**: Wait for Windows to fully boot
3. **Manual Setup**: Run setup script via web interface
4. **Manual Testing**: Execute test script
5. **Result Collection**: Collect results manually

## Troubleshooting

### Common Issues

1. **Audio Device Access (Linux)**
   ```bash
   # Ensure user is in audio group
   sudo usermod -a -G audio $USER
   
   # Check PipeWire is running
   systemctl --user status pipewire
   ```

2. **KVM Access (Windows/macOS)**
   ```bash
   # Check KVM availability
   ls -la /dev/kvm
   
   # Add user to kvm group
   sudo usermod -a -G kvm $USER
   ```

3. **Container Resource Issues**
   ```bash
   # Increase Docker resources
   # Docker Desktop: Settings > Resources
   # Recommended: 16GB RAM, 4+ CPU cores
   ```

### Debug Mode

Enable debug logging:

```bash
# Set debug environment
export RUST_LOG=debug
export RUST_BACKTRACE=full

# Run with debug output
make test-all-platforms
```

### Manual Container Access

```bash
# Access Linux container
docker exec -it rsac-linux-pipewire-test bash

# Access macOS container (after startup)
docker exec -it rsac-macos-test bash

# Windows access via web browser
# http://localhost:8006
```

## Performance Considerations

### Resource Requirements

- **Linux Container**: 2GB RAM, 2 CPU cores
- **Windows Container**: 6GB RAM, 4 CPU cores
- **macOS Container**: 4GB RAM, 4 CPU cores
- **Total Recommended**: 16GB RAM, 8 CPU cores

### Optimization Tips

1. **Run Platforms Separately**: Test one platform at a time to reduce resource usage
2. **Use SSD Storage**: Improves container startup times
3. **Close Unnecessary Applications**: Free up system resources

## Integration with CI/CD

### GitHub Actions Example

```yaml
name: Platform Testing
on: [push, pull_request]

jobs:
  test-linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - name: Test Linux PipeWire
        run: make test-linux-pipewire
      
  test-macos:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - name: Test macOS Core Audio
        run: make test-macos-coreaudio
```

## Contributing

When adding new tests:

1. Update platform-specific test scripts in `docker/testing/scripts/`
2. Modify Dockerfiles in `docker/testing/`
3. Update `docker-compose.testing.yml`
4. Add new Make targets
5. Update this documentation

## References

- [dockurr/windows](https://github.com/dockur/windows) - Windows in Docker
- [sickcodes/Docker-OSX](https://github.com/sickcodes/Docker-OSX) - macOS in Docker
- [PipeWire Documentation](https://pipewire.org/) - Linux audio system
- [VLC Documentation](https://www.videolan.org/vlc/) - Media player integration

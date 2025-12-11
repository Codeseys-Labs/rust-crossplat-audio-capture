# Virtual Audio Device (VAD) Setup Tool

Cross-platform virtual audio device manager for CI testing of audio capture libraries.

## Overview

This tool creates virtual audio devices that enable audio capture testing in headless CI environments where no physical audio hardware exists.

| Platform | Technology | External Dependencies |
|----------|-----------|----------------------|
| **Linux** | PipeWire null sink | None (built-in) |
| **Windows** | WDM audio driver | Virtual Audio Driver (auto-downloaded) |
| **macOS** | CoreAudio HAL plugin | BlackHole (via Homebrew or built from source) |

## Usage

```bash
# Build the tool
cargo build -p virtual-audio-device --release

# Create/install virtual audio device
./target/release/vad-setup create

# Check device status
./target/release/vad-setup status

# Test the device
./target/release/vad-setup test

# Remove the device
./target/release/vad-setup remove
```

## Platform Details

### Linux (PipeWire)

Uses PipeWire's built-in `module-null-sink` - **no external dependencies required**.

The null sink creates:
- A virtual speaker (`rsac_ci_test_sink`) for applications to output to
- A monitor source (`rsac_ci_test_sink.monitor`) for capturing audio

```bash
# What happens under the hood:
pactl load-module module-null-sink sink_name=rsac_ci_test_sink

# Applications output to the sink, capture from the monitor:
# Output: rsac_ci_test_sink
# Capture: rsac_ci_test_sink.monitor
```

### Windows (WDM Driver)

Windows requires a kernel-mode audio driver. The tool automatically downloads and installs the [Virtual Audio Driver](https://github.com/VirtualDrivers/Virtual-Audio-Driver).

**Requirements:**
- Administrator privileges
- Windows 10/11

**Note:** For truly self-contained deployment, we plan to include a minimal pre-built driver in future releases.

### macOS (CoreAudio HAL)

Uses [BlackHole](https://github.com/ExistentialAudio/BlackHole), an open-source virtual audio driver.

Installation methods (in order of preference):
1. **Homebrew** (recommended): `brew install --cask blackhole-2ch`
2. **Build from source**: Automatically clones and builds if Homebrew unavailable
3. **Bundled plugin** (future): Pre-built HAL plugin included in distribution

**Requirements:**
- Xcode Command Line Tools (for building from source)
- sudo access (for installing HAL plugins)

## CI Integration

### GitHub Actions

```yaml
# Linux - just use pactl (built into PipeWire)
- name: Create Virtual Audio Device
  run: |
    pactl load-module module-null-sink sink_name=rsac_ci_test_sink
    pactl set-default-sink rsac_ci_test_sink

# Windows - use the vad-setup tool
- name: Create Virtual Audio Device
  run: |
    cargo build -p virtual-audio-device --release
    ./target/release/vad-setup.exe create

# macOS - use Homebrew
- name: Create Virtual Audio Device
  run: |
    brew install --cask blackhole-2ch
```

### Using vad-setup in CI

```yaml
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4

      - name: Build vad-setup
        run: cargo build -p virtual-audio-device --release

      - name: Create Virtual Audio Device
        run: ./target/release/vad-setup create

      - name: Run Audio Tests
        run: cargo test --features audio-tests

      - name: Cleanup
        if: always()
        run: ./target/release/vad-setup remove
```

## Building Native Drivers (Advanced)

### Windows: Building SYSVAD-based Driver

For a truly self-contained Windows solution, you can build a minimal driver:

```powershell
# Install Windows Driver Kit (WDK)
# https://docs.microsoft.com/en-us/windows-hardware/drivers/download-the-wdk

# Clone Microsoft driver samples
git clone https://github.com/microsoft/Windows-driver-samples

# Build the SYSVAD sample
cd Windows-driver-samples/audio/sysvad
msbuild sysvad.sln /p:Configuration=Release /p:Platform=x64

# Note: Driver signing required for distribution
```

### macOS: Building BlackHole

```bash
# Clone BlackHole
git clone https://github.com/ExistentialAudio/BlackHole
cd BlackHole

# Build with custom settings
xcodebuild -project BlackHole.xcodeproj \
  -configuration Release \
  -target BlackHole2ch \
  GCC_PREPROCESSOR_DEFINITIONS='kNumber_Of_Channels=2 kDevice_Name="RSAC Virtual Audio"'

# Install (requires sudo)
sudo cp -R build/Release/BlackHole2ch.driver /Library/Audio/Plug-Ins/HAL/
sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod
```

## How It Works

### Audio Flow in CI

```
┌──────────────────────────────────────────────────────────────┐
│                    CI AUDIO TESTING                          │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  1. vad-setup creates virtual audio device                   │
│                                                              │
│  2. Test application (VLC, etc.) outputs audio               │
│     ┌─────────┐                                              │
│     │   VLC   │ ──► Virtual Speaker                          │
│     └─────────┘                                              │
│                                                              │
│  3. RSAC captures from virtual device                        │
│                        ┌─────────┐                           │
│     Virtual Monitor ──►│  RSAC   │ ──► captured.wav          │
│                        └─────────┘                           │
│                                                              │
│  4. Validate captured audio matches expected                 │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

## Troubleshooting

### Linux

```bash
# Check if PipeWire is running
systemctl --user status pipewire

# List available sinks
pactl list short sinks

# Check if our sink exists
pactl list short sinks | grep rsac_ci_test_sink
```

### Windows

```powershell
# Check audio devices
Get-PnpDevice | Where-Object { $_.FriendlyName -like '*Virtual*Audio*' }

# Check audio services
Get-Service AudioSrv, AudioEndpointBuilder
```

### macOS

```bash
# List audio devices
system_profiler SPAudioDataType

# Check HAL plugins
ls /Library/Audio/Plug-Ins/HAL/

# Restart CoreAudio
sudo launchctl kickstart -kp system/com.apple.audio.coreaudiod
```

## License

MIT License - Same as the main RSAC project.

The tool may download/use:
- [Virtual Audio Driver](https://github.com/VirtualDrivers/Virtual-Audio-Driver) - MIT License
- [BlackHole](https://github.com/ExistentialAudio/BlackHole) - MIT License

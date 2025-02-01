# Cross-Platform Audio Capture Library

A robust, high-performance audio capture library for Rust, supporting Windows, macOS, and Linux platforms. This library provides a unified interface for capturing audio across different operating systems, with support for both system-wide and application-specific capture.

## Features

### Core Features
- **Cross-Platform Support**
  - Windows: WASAPI-based capture with both system-wide and application-specific support
  - Linux: Modern PipeWire backend (preferred) with PulseAudio fallback, supporting both system-wide and application-specific capture
  - macOS: CoreAudio implementation with system-wide capture

### Audio Capabilities
- **System-wide Audio Capture**
  - Supported on all platforms (Windows, Linux, macOS)
  - High-quality audio capture from system output
  - Zero-configuration setup on most systems

- **Application-specific Capture**
  - Windows: WASAPI-based per-application capture
  - Linux: PipeWire/PulseAudio process-specific capture
  - Capture from multiple applications simultaneously

- **Flexible Output Formats**
  - Multiple formats: WAV, RAW PCM
  - Configurable parameters:
    - Sample rates: 44.1kHz, 48kHz, 96kHz
    - Bit depths: 16-bit, 24-bit, 32-bit
    - Channel configurations: Mono, Stereo
    - Format types: F32LE, S16LE, S32LE

### Advanced Features
- Async support with Tokio
- Lock-free audio buffers
- SIMD optimizations
- Zero-copy audio processing
- Comprehensive error handling
- Detailed logging and diagnostics

### Development Features
- Trait-based API design
- Extensive test coverage
- Mock implementations for testing
- Comprehensive documentation
- Performance benchmarks

## Installation

1. Ensure you have Rust installed on your system
2. Clone this repository
3. Build the project:

```bash
cargo build --release
```

4. The executable will be available at `target/release/rsac` (or `rsac.exe` on Windows)

## Usage

RSAC supports both interactive and command-line modes, with bounded or unbounded recording duration. The functionality varies slightly between platforms.

### Platform-Specific Notes

#### Windows
- Supports both system-wide and application-specific audio capture
- Uses WASAPI for high-quality audio capture
- Process names should include the `.exe` extension for app-specific capture
- Use "System" as the process name for system-wide capture
- Administrative privileges may be required for some applications

#### Linux
- Primary support through PipeWire (modern audio system)
- Fallback to PulseAudio when PipeWire is not available
- Supports both system-wide and application-specific capture
- Process names should match the application name in PipeWire/PulseAudio
- No special privileges required for most applications

#### macOS
- System-wide audio capture only (application-specific capture not supported)
- Requires Screen Recording permission to be granted
- Process selection is limited to system audio

### Interactive Mode

Simply run the application without any process specification:

```bash
rsac
```

This will:

1. List available audio sources (processes on Windows/Linux, system audio on macOS)
2. Allow you to select a source interactively
3. Start capturing audio from the selected source

You can filter the source list in interactive mode (Windows/Linux):

```bash
rsac -i spotify
```

### Command Line Mode

#### Bounded Recording

Specify a duration to capture for a fixed time:

```bash
# Windows
rsac -p System -d 30

# Linux
rsac -p System -d 30

# macOS
rsac -p "System Audio" -d 30
```

#### Unbounded Recording

Omit the duration to record until Ctrl+C is pressed:

```bash
# Windows
rsac -p Spotify.exe

# Linux
rsac -p spotify

# macOS
rsac -p "System Audio"
```

### Available Options

- `-p, --process <n>`: Process/source name to capture audio from
- `-d, --duration <SECONDS>`: Duration to capture (omit for unbounded recording)
- `-o, --output-dir <PATH>`: Output directory (default: current directory)
- `-f, --format <FORMAT>`: Output format: raw, wav, or both (default: both)
- `-i, --filter <FILTER>`: Filter source list in interactive mode
- `-v, --verbose`: Enable verbose output for debugging
- `-r, --rate <RATE>`: Sample rate in Hz (default: 48000)
- `-c, --channels <COUNT>`: Number of channels (default: 2)
- `--format-type <TYPE>`: Audio format type: f32le, s16le, s32le (default: f32le)

### Examples

1. Record 30 seconds of system-wide audio:

```bash
# Windows
rsac -p System -d 30

# Linux
rsac -p System -d 30

# macOS
rsac -p "System Audio" -d 30
```

2. Record 30 seconds of application-specific audio:

```bash
# Windows
rsac -p Spotify.exe -d 30

# Linux
rsac -p firefox -d 30

# macOS (system audio only)
rsac -p "System Audio" -d 30
```

3. Record with custom audio format:

```bash
rsac -p firefox --format-type s16le -r 44100 -c 1
```

4. Record in WAV format only with high quality:

```bash
rsac -p chrome -f wav --format-type f32le -r 96000
```

5. Save to custom directory with verbose output:

```bash
rsac -p vlc -o ./captures -v
```

6. Record system audio on macOS with custom format:

```bash
rsac -p "System Audio" --format-type f32le -r 48000 -c 2
```

## Output Files

For a process named "example", the following files will be created:

- `example_audio.raw`: Raw audio data (if raw or both format selected)
- `example_audio.wav`: WAV format audio (if wav or both format selected)
- `example_capture.log`: Capture statistics and debug information

## Audio Format

The captured audio supports the following configurations:

- Channels: 1 (Mono) or 2 (Stereo)
- Sample Rate: Any standard rate (44100, 48000, 96000 Hz, etc.)
- Bit Depth/Format:
  - 32-bit float (F32LE)
  - 16-bit signed integer (S16LE)
  - 32-bit signed integer (S32LE)

## Progress Display

- Bounded recording: Shows a progress bar with elapsed time and completion percentage
- Unbounded recording: Shows a spinner with elapsed time
- Both modes display a capture summary at completion with:
  - Total packets captured
  - Silent packet detection
  - Total data size
  - Average packet size
  - Recording duration

## Requirements

### Windows
- Windows 7 or later
- Rust toolchain
- Administrative privileges may be required for some applications

### Linux
- PipeWire (recommended) or PulseAudio sound server
- Development libraries:
  - For PipeWire: `libpipewire-dev`
  - For PulseAudio: `libpulse-dev`
- Rust toolchain

### macOS
- macOS 10.13 or later
- Rust toolchain
- Xcode Command Line Tools
- Screen Recording permission (required for audio capture)

## Contributing

Contributions are welcome! Here's how you can help:

### Development Workflow
1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run the test suite locally
5. Submit a Pull Request

### Running Tests
The project includes comprehensive test suites for each platform:

```bash
# Run basic tests
cargo test

# Run platform-specific audio tests (requires audio setup)
cargo test --features audio-tests
```

### CI/CD
- GitHub Actions workflows are configured for each platform
- Tests can be triggered manually through GitHub Actions UI
- Available workflows:
  - Windows Audio Tests
  - Linux Audio Tests (PipeWire and PulseAudio)
  - macOS Audio Tests
  - Code Quality Checks

### Best Practices
- Write tests for new features
- Update documentation for API changes
- Follow Rust coding guidelines
- Include both system-wide and app-specific tests where applicable

## License

This project is licensed under the MIT License - see the LICENSE file for details.
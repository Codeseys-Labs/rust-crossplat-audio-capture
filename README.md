# RSAC (Rust System Audio Capture)

A cross-platform command-line tool for capturing audio from specific applications, supporting Windows, Linux, and macOS.

## Features

- Cross-platform support:
  - Windows: Application-specific capture using WASAPI
  - Linux: Application-specific capture using PulseAudio (PipeWire support coming soon)
  - macOS: System audio capture using CoreAudio
- Process-specific audio capture (where supported)
- Interactive process/source selection with filtering
- Multiple output formats (RAW/WAV)
- Real-time progress visualization
- Detailed capture logs
- Configurable capture duration
- Unbounded recording mode
- Custom output paths
- Multiple audio formats (F32LE, S16LE, S32LE)
- Configurable sample rate and channel count

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
- Full application-specific audio capture support
- Process names should include the `.exe` extension
- Administrative privileges may be required for some applications

#### Linux
- Application-specific capture through PulseAudio
- Process names should match the application name in PulseAudio
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
rsac -p Spotify.exe -d 30

# Linux
rsac -p spotify -d 30

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

1. Record 30 seconds of application audio:

```bash
# Windows
rsac -p Spotify.exe -d 30

# Linux
rsac -p firefox -d 30

# macOS
rsac -p "System Audio" -d 30
```

2. Record with custom audio format:

```bash
rsac -p firefox --format-type s16le -r 44100 -c 1
```

3. Record in WAV format only with high quality:

```bash
rsac -p chrome -f wav --format-type f32le -r 96000
```

4. Save to custom directory with verbose output:

```bash
rsac -p vlc -o ./captures -v
```

5. Interactive mode with filtering (Windows/Linux):

```bash
rsac -i firefox
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
- PulseAudio sound server (default on most distributions)
- PulseAudio development libraries (`libpulse-dev` on Ubuntu/Debian)
- Rust toolchain

### macOS
- macOS 10.13 or later
- Rust toolchain
- Xcode Command Line Tools
- Screen Recording permission (required for audio capture)

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the LICENSE file for details.
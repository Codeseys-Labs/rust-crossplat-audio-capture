# RSAC (Rust System Audio Capture)

A command-line tool for capturing audio from specific applications on Windows using the Windows Audio Session API (WASAPI).

## Features

- Process-specific audio capture
- Interactive process selection with filtering
- Multiple output formats (RAW/WAV)
- Real-time progress visualization
- Detailed capture logs
- Configurable capture duration
- Unbounded recording mode
- Custom output paths

## Installation

1. Ensure you have Rust installed on your system
2. Clone this repository
3. Build the project:

```bash
cargo build --release
```

4. The executable will be available at `target/release/rsac.exe`

## Usage

RSAC supports both interactive and command-line modes, with bounded or unbounded recording duration.

### Interactive Mode

Simply run the application without any process specification:

```bash
rsac
```

This will:

1. List all running processes
2. Allow you to select a process interactively
3. Start capturing audio from the selected process

You can filter the process list in interactive mode:

```bash
rsac -i spotify
```

This will show only processes containing "spotify" in their name.

### Command Line Mode

#### Bounded Recording

Specify a duration to capture for a fixed time:

```bash
rsac -p Spotify.exe -d 30
```

#### Unbounded Recording

Omit the duration to record until Ctrl+C is pressed:

```bash
rsac -p Spotify.exe
```

### Available Options

- `-p, --process <NAME>`: Process name to capture audio from
- `-d, --duration <SECONDS>`: Duration to capture (omit for unbounded recording)
- `-o, --output-dir <PATH>`: Output directory (default: current directory)
- `-f, --format <FORMAT>`: Output format: raw, wav, or both (default: both)
- `-i, --filter <FILTER>`: Filter process list in interactive mode
- `-v, --verbose`: Enable verbose output for debugging

### Examples

1. Record 30 seconds of audio from Spotify:

```bash
rsac -p Spotify.exe -d 30
```

2. Record until Ctrl+C is pressed:

```bash
rsac -p Spotify.exe
```

3. Record in WAV format only:

```bash
rsac -p Spotify.exe -f wav
```

4. Save to custom directory:

```bash
rsac -p Spotify.exe -o ./captures
```

5. Interactive mode with filtering:

```bash
rsac -i spot
```

6. Debug mode with verbose output:

```bash
rsac -p Spotify.exe -v
```

## Output Files

For a process named "example.exe", the following files will be created:

- `example.exe_audio.raw`: Raw audio data (if raw or both format selected)
- `example.exe_audio.wav`: WAV format audio (if wav or both format selected)
- `example.exe_capture.log`: Capture statistics and debug information

## Audio Format

The captured audio uses the following format:

- Channels: 2 (Stereo)
- Sample Rate: 48000 Hz
- Bit Depth: 32-bit float

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

- Windows operating system
- Rust toolchain
- Administrative privileges may be required for some applications

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the LICENSE file for details.

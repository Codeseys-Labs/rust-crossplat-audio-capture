# Local Testing Guide

> **How to test rsac on your physical machines.**
>
> This guide covers setup, quick validation, and end-to-end testing of all three
> capture levels (system, application, process tree) on macOS, Windows, and Linux.

---

## Prerequisites per Platform

### macOS (laptop)

| Requirement | Details |
|---|---|
| **macOS version** | 14.4+ (Sonoma) required for Process Tap |
| **Rust toolchain** | Install via [rustup](https://rustup.rs) |
| **Xcode CLI tools** | `xcode-select --install` |
| **Clone the repo** | `git clone --recurse-submodules <repo-url>` |

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Install Xcode Command Line Tools
xcode-select --install

# Clone
git clone --recurse-submodules <repo-url>
cd rust-crossplat-audio-capture
```

### Windows (desktop)

| Requirement | Details |
|---|---|
| **Windows version** | Windows 10 build 20348+ or Windows 11 (process loopback) |
| **Rust toolchain** | Download from [rustup.rs](https://rustup.rs) |
| **Build tools** | Visual Studio Build Tools with "C++ build tools" workload |
| **Clone the repo** | `git clone --recurse-submodules <repo-url>` |

1. Download and run the [rustup installer](https://rustup.rs).
2. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) — select the **"Desktop development with C++"** workload.
3. Open a new terminal (so PATH is updated) and clone:
   ```powershell
   git clone --recurse-submodules <repo-url>
   cd rust-crossplat-audio-capture
   ```

### Linux (current dev machine)

| Requirement | Details |
|---|---|
| **PipeWire** | 0.3.44+ with development headers |
| **Rust toolchain** | Install via [rustup](https://rustup.rs) |

```bash
# Debian/Ubuntu
sudo apt install pipewire pipewire-pulse libpipewire-0.3-dev

# Fedora
sudo dnf install pipewire pipewire-devel

# Arch
sudo pacman -S pipewire pipewire-pulse

# Verify PipeWire is running
systemctl --user status pipewire
```

---

## Quick Validation (All Platforms)

These commands work on every platform and do **not** require audio hardware:

```bash
# Compile check — catches type errors, missing imports, etc.
cargo check

# Run unit tests (no audio hardware needed)
cargo test --lib

# Run with async feature enabled
cargo test --lib --features async-stream

# Run all tests including integration tests (requires platform feature)
# Linux:
cargo test --features feat_linux
# macOS:
cargo test --features feat_macos
# Windows:
cargo test --features feat_windows
```

---

## System Capture Testing

System capture records all audio output from the system's default device.

### Steps

```bash
# 1. Play some audio on your system (music, video, etc.)

# 2. List available devices — verify your audio stack is working
cargo run --features cli -- list

# 3. Capture system audio (shows a live ASCII level meter)
#    Press Ctrl+C to stop
cargo run --features cli -- capture

# 4. Record system audio to a WAV file (5 seconds)
cargo run --features cli -- record --duration 5 output.wav

# 5. Verify the recorded file
#    - Check file size is > 44 bytes (WAV header)
#    - Play it back with your system audio player
ls -la output.wav
```

### Expected Output

The `capture` command displays a live level meter:

```
🎙  Capture target: system default
    Sample rate: 48000 Hz, Channels: 2
    Press Ctrl+C to stop.

  [████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░]  -12.3 dB  | frames:    48000 | buffers:     50
```

---

## Application Capture Testing

Application capture isolates audio from a single application.

### Steps

```bash
# 1. Start an audio-playing application:
#    macOS:   Spotify, Safari with YouTube, VLC
#    Windows: Spotify, Firefox with YouTube, VLC
#    Linux:   Spotify, Firefox with YouTube, mpv

# 2. Find the application's PID:

# macOS:
pgrep -x Spotify
# or
ps aux | grep -i spotify

# Windows (PowerShell):
Get-Process spotify | Select-Object Id
# or (cmd):
tasklist | findstr /i spotify

# Linux:
pgrep -x spotify
# or
pidof spotify

# 3. Capture by PID (uses ProcessTree target)
cargo run --features cli -- capture --pid <PID>

# 4. Capture by application name (uses ApplicationByName target)
cargo run --features cli -- capture --app spotify

# 5. Record application audio to a WAV file
cargo run --features cli -- record --app spotify --duration 10 app_audio.wav
```

### How It Works per Platform

| Platform | `--app <name>` | `--pid <PID>` |
|---|---|---|
| **macOS** | Resolves via Process Tap. Creates an aggregate device targeting the named process. | Uses `CaptureTarget::ProcessTree(ProcessId)` via CoreAudio Process Tap. |
| **Windows** | Uses `sysinfo` to resolve the process name to a PID, then captures via WASAPI process loopback. | Direct WASAPI process loopback capture. |
| **Linux** | Uses `pw-dump` to resolve the app name to a PipeWire node serial, then creates a targeted PipeWire stream. | Resolves PID to PipeWire node via `pw-dump`, then captures that node. |

---

## Process Tree Capture Testing

Process tree capture targets a parent process and all its child processes.
This is useful for browsers where multiple tabs/processes produce audio.

### Steps

```bash
# 1. Start a browser and open multiple tabs playing audio

# 2. Find the parent (main) browser PID:

# macOS:
pgrep -x "Google Chrome"
# or
pgrep -x Firefox

# Windows (PowerShell):
Get-Process chrome | Select-Object Id, ProcessName | Select-Object -First 1
# or (cmd):
tasklist | findstr chrome

# Linux:
pgrep -x chrome
# or
pgrep -x firefox

# 3. Capture the entire process tree
cargo run --features cli -- capture --pid <PARENT_PID>

# 4. Record process tree audio
cargo run --features cli -- record --pid <PARENT_PID> --duration 10 tree_audio.wav
```

> **Note:** When `--pid` is specified, `rsac` uses `CaptureTarget::ProcessTree(ProcessId)`.
> On platforms that support it, this captures audio from the target PID and all its
> descendant processes. On platforms where process tree capture is not distinct from
> single-process capture, the behavior is equivalent to application capture.

---

## Composed Capture Testing (`compose` feature)

Composed capture (ADR-0011) runs several sources simultaneously and delivers
one interleaved multi-channel stream — e.g. an application mixed to a mono
channel plus the system default's native channels appended after it.

### Steps

```bash
# 1. Start an application playing audio (e.g. Spotify, a browser tab).

# 2. Run the shipped example: app group (mono) + system group (native channels).
#    Falls back to a system-only composition when no app name is given.
cargo run --example composed_capture --features compose -- spotify

# 3. Verify in the output:
#    - the printed channel map (e.g. "composed 3 channels": app, sysL, sysR)
#    - per-group RMS levels move when the app / system audio plays
#    - the final stats block: per-source buffers_received > 0; padded_frames
#      stays near zero while both sources play; `resampling=true` appears for
#      any source whose endpoint rate differs from 48 kHz (expected for
#      Windows process loopback on 44.1 kHz endpoints)
```

### What to check per platform

- **Windows** — compose an `ApplicationByName` group with a `SystemDefault`
  keep-channels group. Process loopback cannot autoconvert, so if the endpoint
  runs at 44.1 kHz the app source must report `resampling=true` and the
  composed output must still be 48 kHz.
- **macOS** — same recipe; the app group exercises Process Tap (14.4+, TCC
  audio-capture permission required — grant it to your terminal).
- **Linux** — with the PipeWire null-sink setup from the sections above,
  route the player to the sink and compose `SystemDefault` twice (mono mix +
  keep-channels) as a self-test without a second app.

### Deterministic unit-level verification (no devices)

The compose engine (mixdown math, resampling, master-clock pacing, padding /
trimming / re-election) is fully covered by scripted-source tests that run on
any machine:

```bash
cargo test --lib --features compose compose::
```

---

## Platform-Specific Notes

### macOS

- **Permissions:** Per-app / process-tree / system capture on macOS 14.4+ use
  the **Process Tap** API, gated by the **Audio Capture** TCC service
  (`kTCCServiceAudioCapture`). This is a *distinct* service from Microphone and
  from Screen Recording — granting either of those is **not** sufficient. Declare
  `NSAudioCaptureUsageDescription` in your `Info.plist`; the OS prompts on the
  first capture attempt. If you get permission errors, approve the prompt under
  **System Settings → Privacy & Security → Audio Capture** and relaunch the
  process (grants are read only at process start).
- **Process Tap:** Requires macOS 14.4+ (Sonoma). On older versions, only system capture works.
- **Aggregate device:** For app capture, rsac creates a temporary CoreAudio aggregate device automatically.
- **Xcode requirement:** The build uses CoreAudio system frameworks; Xcode CLI tools must be installed.

### Windows

- **Process loopback:** Requires Windows 10 build 20348+ or Windows 11.
  On older builds, system capture works but per-process capture will return
  `PlatformNotSupported`.
- **COM initialization:** rsac initializes COM in MTA mode automatically. If you get
  COM errors, ensure no other library is initializing COM in STA mode in the same thread.
- **ApplicationByName:** Uses the `sysinfo` crate to resolve process names to PIDs.
  Ensure the process name matches exactly (case-insensitive on Windows).
- **Audio must be playing:** If capture returns silence, make sure the target application
  is actively producing audio — WASAPI process loopback only captures active streams.

### Linux

- **PipeWire required:** rsac's Linux backend uses PipeWire exclusively.
  Verify it's running:
  ```bash
  systemctl --user status pipewire
  ```
- **ApplicationByName resolution:** Uses `pw-dump` to find PipeWire nodes matching
  the application name. Check available nodes with:
  ```bash
  pw-dump | jq '.[] | select(.type == "PipeWire:Interface:Node") | {id, name: .info.props["node.name"], app: .info.props["application.name"]}'
  ```
- **ProcessTree resolution:** PIDs are mapped to PipeWire nodes via `pw-dump`.
  Use `pw-top` to see active audio streams in real time:
  ```bash
  pw-top
  ```
- **No audio captured:** If the level meter shows silence:
  1. Check `pw-top` to confirm the target app has an active stream
  2. Verify the PipeWire node serial matches: `pw-dump | grep -A5 <app-name>`
  3. Ensure `pipewire-pulse` is installed if the app uses PulseAudio

---

## Running Integration Tests

Integration tests require a running audio stack and may need actual audio playback.

```bash
# Linux (with PipeWire running):
cargo test --test ci_audio --features feat_linux

# macOS:
cargo test --test ci_audio --features feat_macos

# Windows:
cargo test --test ci_audio --features feat_windows
```

### What the Integration Tests Cover

The `ci_audio` suite spans one module per capture surface. A representative
sample:

| Module | Description |
|---|---|
| `system_capture` | `CaptureTarget::SystemDefault` end-to-end (tone → capture → verify) |
| `device_capture` / `device_enumeration` | Device-targeted capture + enumerator contract |
| `app_capture` | Per-application capture by PID / PipeWire node + nonexistent-target handling |
| `application_by_name` / `application_by_pid` | macOS `ApplicationByName` / `Application(PID)` resolution |
| `process_tree` / `process_tree_capture` | `ProcessTree` public-API + end-to-end tree capture |
| `subscribe` | `subscribe()` mpsc fan-out, disconnect-after-stop, multi-subscriber |
| `overrun` | `overrun_count()` increments when the consumer stalls (G8) |
| `stream_lifecycle` | start → read → stop, idempotent stop, drop-while-running |
| `lifecycle_terminal` | `request_stop()` + terminal read (`StreamEnded`) semantics |
| `multi_source` | two `AudioCapture` instances at once |
| `platform_caps` | `PlatformCapabilities::query()` sanity |

See [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) for the full module list,
the platform × tier truth table, and the gate macros.

---

## CLI Reference

```
rsac — Cross-platform audio capture demo

USAGE:
    rsac <COMMAND>

COMMANDS:
    info       Show platform capabilities
    list       List available audio devices
    capture    Capture audio and show a live level meter
    record     Record audio to a WAV file

CAPTURE OPTIONS:
    --app <NAME>          Capture by application name
    --pid <PID>           Capture by process ID (uses ProcessTree target)
    --sample-rate <HZ>    Sample rate [default: 48000]
    --channels <N>        Number of channels [default: 2]

RECORD OPTIONS:
    <OUTPUT>              Output WAV file path
    --app <NAME>          Capture by application name
    --pid <PID>           Capture by process ID
    --duration <SECS>     Recording duration (omit for unbounded, Ctrl+C to stop)
    --sample-rate <HZ>    Sample rate [default: 48000]
    --channels <N>        Number of channels [default: 2]
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| **"No audio devices found"** | Audio server not running | **Linux:** `systemctl --user start pipewire` · **macOS:** Reboot (CoreAudio is always on) · **Windows:** Check Windows Audio service |
| **"Application not found"** | App not running or name mismatch | Use `--pid` instead, or check exact process name with `ps`/`tasklist`/`pw-dump` |
| **Silence captured** | Target app not producing audio | Ensure the app is actively playing audio. On Linux, verify with `pw-top` |
| **Permission denied (macOS)** | Missing Audio Capture (`kTCCServiceAudioCapture`) permission | Grant in **System Settings → Privacy & Security → Audio Capture** (not Microphone / Screen Recording) and relaunch |
| **COM error (Windows)** | COM initialization conflict | Ensure no STA COM init in the same thread. rsac uses MTA mode. |
| **"PlatformNotSupported" for app capture** | OS version too old | **macOS:** Need 14.4+ · **Windows:** Need build 20348+ · **Linux:** Need PipeWire 0.3.44+ |
| **Build fails with missing headers** | Development libraries not installed | **Linux:** `sudo apt install libpipewire-0.3-dev` · **macOS:** `xcode-select --install` · **Windows:** Install VS Build Tools C++ workload |
| **"StreamCreationFailed"** | Device busy or misconfigured | Try a different sample rate (`--sample-rate 44100`) or check other apps aren't locking the device |

---

## Example Workflows

### Quick Smoke Test (any platform)

```bash
cargo check                     # Compiles?
cargo test --lib                # Unit tests pass?
cargo run --features cli -- info               # Platform caps look right?
cargo run --features cli -- list               # Devices enumerated?
cargo run --features cli -- capture            # Ctrl+C after seeing level meter
cargo run --features cli -- record --duration 3 test.wav  # Record and play back
```

### Application Capture End-to-End (macOS)

```bash
# Start Spotify, play a song
SPOTIFY_PID=$(pgrep -x Spotify)
echo "Spotify PID: $SPOTIFY_PID"

# Capture by name
cargo run --features cli -- capture --app Spotify
# Ctrl+C after confirming audio levels

# Record 10 seconds by PID
cargo run --features cli -- record --pid $SPOTIFY_PID --duration 10 spotify_capture.wav

# Play back
afplay spotify_capture.wav
```

### Application Capture End-to-End (Windows)

```powershell
# Start Spotify, play a song
$pid = (Get-Process spotify).Id
Write-Host "Spotify PID: $pid"

# Capture by name
cargo run --features cli -- capture --app spotify
# Ctrl+C after confirming audio levels

# Record 10 seconds
cargo run --features cli -- record --pid $pid --duration 10 spotify_capture.wav

# Play back
Start-Process spotify_capture.wav
```

### Application Capture End-to-End (Linux)

```bash
# Start Spotify or Firefox with YouTube
SPOTIFY_PID=$(pgrep -x spotify)
echo "Spotify PID: $SPOTIFY_PID"

# Verify PipeWire sees it
pw-top  # Look for the spotify stream

# Capture by name
cargo run --features cli -- capture --app spotify
# Ctrl+C after confirming audio levels

# Record 10 seconds by PID
cargo run --features cli -- record --pid $SPOTIFY_PID --duration 10 spotify_capture.wav

# Play back
pw-play spotify_capture.wav
# or
mpv spotify_capture.wav
```

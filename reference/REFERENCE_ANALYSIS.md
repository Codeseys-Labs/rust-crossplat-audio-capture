# Reference Repository Analysis

> Comprehensive analysis of 10 reference repositories mapped to rsac's architecture.
> This document provides actionable implementation guidance for each platform backend.

## Executive Summary

This document compiles the analysis of 10 reference repositories spanning three platforms (Windows/WASAPI, Linux/PipeWire, macOS/CoreAudio) plus two cross-platform libraries (cpal, rtrb). The analysis validates rsac's architectural decisions and provides concrete implementation guidance.

**Key findings across all platforms:**

1. **Threading model is universal**: Every reference project uses a dedicated OS audio thread that bridges to consumer threads via lock-free primitives. rsac's `BridgeStream<S>` design is validated by all three platform analyses.

2. **Ring buffer bridge is the correct pattern**: CamillaDSP (Windows), wiremix/OBS (Linux), and AudioCap (macOS) all use ring-buffer-like bridges between OS callback threads and consumer threads. rtrb's SPSC design is ideal for this.

3. **Per-application capture is platform-specific but achievable**: Windows uses process loopback (Win10+), Linux uses PipeWire virtual sinks + link management, macOS uses CoreAudio Process Tap (macOS 14+). All three patterns map cleanly to rsac's `CaptureTarget` enum.

4. **cpal validates the Host → Device → Stream hierarchy** but doesn't support per-app capture — this is exactly where rsac adds value beyond cpal.

5. **Error taxonomy is convergent**: All platforms surface similar error categories (device not found, format unsupported, permission denied, timeout) that map cleanly to rsac's 21-variant `AudioError`.

---

## Table of Contents

- [1. Windows Platform (WASAPI)](#1-windows-platform-wasapi)
  - [1.1 wasapi-rs — Rust WASAPI Bindings](#11-wasapi-rs--rust-wasapi-bindings)
  - [1.2 CamillaDSP — WASAPI Backend](#12-camilladsp--wasapi-backend)
  - [1.3 Key Patterns for rsac Windows Backend](#13-key-patterns-for-rsac-windows-backend)
- [2. Linux Platform (PipeWire)](#2-linux-platform-pipewire)
  - [2.1 wiremix — PipeWire App Capture](#21-wiremix--pipewire-app-capture)
  - [2.2 pipewire-rs — Official Rust Bindings](#22-pipewire-rs--official-rust-bindings)
  - [2.3 OBS PipeWire Audio Capture](#23-obs-pipewire-audio-capture)
  - [2.4 Key Patterns for rsac Linux Backend](#24-key-patterns-for-rsac-linux-backend)
- [3. macOS Platform (CoreAudio Process Tap)](#3-macos-platform-coreaudio-process-tap)
  - [3.1 AudioCap — Swift Process Tap](#31-audiocap--swift-process-tap)
  - [3.2 audio-rec — C++/ObjC Process Tap](#32-audio-rec--cobjc-process-tap)
  - [3.3 screencapturekit-rs — ScreenCaptureKit Bindings](#33-screencapturekit-rs--screencapturekit-bindings)
  - [3.4 Key Patterns for rsac macOS Backend](#34-key-patterns-for-rsac-macos-backend)
- [4. Cross-Platform Infrastructure](#4-cross-platform-infrastructure)
  - [4.1 cpal — Cross-Platform Audio I/O](#41-cpal--cross-platform-audio-io)
  - [4.2 rtrb — Lock-Free Ring Buffer](#42-rtrb--lock-free-ring-buffer)
- [5. Cross-Platform Patterns & Synthesis](#5-cross-platform-patterns--synthesis)
  - [5.1 Universal Threading Model](#51-universal-threading-model)
  - [5.2 BridgeStream Validation](#52-bridgestream-validation)
  - [5.3 CaptureTarget Implementation Matrix](#53-capturetarget-implementation-matrix)
  - [5.4 Error Taxonomy Cross-Reference](#54-error-taxonomy-cross-reference)
  - [5.5 Capability Matrix](#55-capability-matrix)
- [6. Key Takeaways for rsac Implementation](#6-key-takeaways-for-rsac-implementation)

---

## 1. Windows Platform (WASAPI)

### 1.1 wasapi-rs — Rust WASAPI Bindings

#### Architecture Overview

The `wasapi-rs` crate (v0.22.0) is a thin, idiomatic Rust wrapper around the Windows WASAPI COM APIs. It uses the `windows` crate (v0.62) for FFI bindings and exposes a module structure of:

- [`src/lib.rs`](reference/wasapi-rs/src/lib.rs:1) — Re-exports all public types from submodules
- [`src/api.rs`](reference/wasapi-rs/src/api.rs:1) — Core API: `DeviceEnumerator`, `Device`, `AudioClient`, `AudioCaptureClient`, `AudioRenderClient`, `Handle`, plus stream mode/share mode enums
- [`src/errors.rs`](reference/wasapi-rs/src/errors.rs:1) — `WasapiError` enum (16 variants) using `thiserror`
- [`src/events.rs`](reference/wasapi-rs/src/events.rs:1) — `EventCallbacks` and `AudioSessionEvents` for session notifications (disconnect, volume, state changes)
- [`src/waveformat.rs`](reference/wasapi-rs/src/waveformat.rs:1) — `WaveFormat` wrapping `WAVEFORMATEXTENSIBLE`, format construction and parsing

The API follows a **sequential object construction** pattern:
```
DeviceEnumerator → Device → AudioClient → initialize_client() → AudioCaptureClient/AudioRenderClient
```

Key dependency: `windows = "0.62"` with features for `Win32_Media_Audio`, `Win32_System_Com`, `Win32_System_Threading`, etc. Also uses `thiserror` for error derive and `num-integer` for LCM alignment calculations.

#### COM Threading & Safety

**MTA is the primary model.** The crate provides two public functions at [`src/api.rs:67-79`](reference/wasapi-rs/src/api.rs:67):

- [`initialize_mta()`](reference/wasapi-rs/src/api.rs:67) — calls `CoInitializeEx(None, COINIT_MULTITHREADED)` — **recommended for most use**
- [`initialize_sta()`](reference/wasapi-rs/src/api.rs:73) — calls `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` — needed for older Windows versions with process loopback
- [`deinitialize()`](reference/wasapi-rs/src/api.rs:77) — calls `CoUninitialize()`

**Critical pattern**: COM must be initialized **per-thread**. All examples call `initialize_mta()` at the beginning of each thread that touches WASAPI. The capture example [`record_application.rs:23`](reference/wasapi-rs/examples/record_application.rs:23) initializes MTA in the capture thread.

None of the wasapi-rs types implement `Send` or `Sync` — they are thread-local COM objects. This means audio client/device objects must be created **on the same thread** that uses them.

#### Device Enumeration

Enumeration follows the WASAPI `IMMDeviceEnumerator` pattern at [`src/api.rs:318-411`](reference/wasapi-rs/src/api.rs:318):

1. **`DeviceEnumerator::new()`** — creates via `CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)`
2. **`get_device_collection(direction)`** — returns `DeviceCollection` wrapping `IMMDeviceCollection`, filtered to `DEVICE_STATE_ACTIVE` only
3. **`get_default_device(direction)`** / **`get_default_device_for_role(direction, role)`** — get system default for Console/Multimedia/Communications roles
4. **`get_device(device_id)`** — retrieve by string ID

`DeviceCollection` supports iteration via `IntoIterator`, index access, and name-based lookup.

Each `Device` exposes: `get_friendlyname()`, `get_id()`, `get_state()`, `get_device_format()`, `get_iaudioclient()`, `get_iaudiosessionmanager()`.

The session manager path (`Device → AudioSessionManager → AudioSessionEnumerator → AudioSessionControl → get_process_id()`) enables discovery of which processes are using which audio devices.

#### Process Loopback / Application Capture

**`AudioClient::new_application_loopback_client(process_id, include_tree)`** creates a per-process capture client using the Windows 10+ `ActivateAudioInterfaceAsync` API with `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`.

The implementation:
1. Constructs `AUDIOCLIENT_ACTIVATION_PARAMS` with `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` containing the target `process_id` and loopback mode (`INCLUDE_TARGET_PROCESS_TREE` or `EXCLUDE_TARGET_PROCESS_TREE`)
2. Wraps params in a `PROPVARIANT` with `VT_BLOB` type
3. Creates a completion handler using `Arc<(Mutex<bool>, Condvar)>` for synchronization
4. Calls `ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, activation_params, &callback)`
5. Waits on `Condvar` for completion
6. Extracts `IAudioClient` from the async result via `GetActivateResult()`

**Key limitations**:
- Must use `Direction::Capture` and `ShareMode::Shared` only
- `get_mixformat()`, `is_supported()`, `get_device_period()` return "Not implemented"
- `get_buffer_size()` returns huge nonsensical values
- Format should be requested as f32/48kHz/2ch with autoconvert since format negotiation APIs don't work

**Usage in example** [`record_application.rs:18-74`](reference/wasapi-rs/examples/record_application.rs:18):
1. Find process by name using `sysinfo` crate, request **parent PID** (important: for tree capture, must use the root process)
2. Create application loopback client with `include_tree = true`
3. Use `EventsShared` mode with `autoconvert = true`
4. Set event handle, get capture client
5. Capture loop: wait for event → `get_next_packet_size()` → `read_from_device_to_deque()` → chunk data → send via `mpsc::SyncSender`

#### Audio Buffer Flow

Two data flow patterns in wasapi-rs:

**Pattern A: VecDeque accumulator** (used in all examples):
```
WASAPI Buffer → read_from_device_to_deque(VecDeque<u8>) → accumulate → chunk by blockalign*chunksize → send via channel
```

**Pattern B: Slice-based**: `read_from_device(&mut [u8])` copies directly to a caller-provided slice.

**BufferFlags**: `data_discontinuity`, `silent`, `timestamp_error` — metadata from WASAPI about buffer quality.

**Event-driven timing**: `audio_client.set_get_eventhandle()` → loop: `handle.wait_for_event(timeout_ms)` → read data.

#### Stream Lifecycle

```
1. initialize_mta()                          // COM init per thread
2. DeviceEnumerator::new()                   // Create enumerator
3. enumerator.get_default_device()           // Get device
   -- OR --
   AudioClient::new_application_loopback_client(pid, include_tree)  // Per-app capture
4. device.get_iaudioclient()                 // Get AudioClient
5. audio_client.get_mixformat()              // Query device format
6. audio_client.is_supported(&format, &mode) // Check format
7. audio_client.initialize_client(...)       // Initialize
8. audio_client.set_get_eventhandle()        // Get event
9. audio_client.get_audiocaptureclient()     // Get capture client
10. audio_client.start_stream()              // Begin capture
11. loop { handle.wait_for_event() → capture_client.read_from_device() }
12. audio_client.stop_stream()               // Stop
```

#### Error Handling

`WasapiError` has 16 variants: `DeviceNotFound`, `IllegalDeviceState`, `UnsupportedFormat`, `ClientNotInit`, `EventTimeout`, `DataLengthMismatch`, `LoopbackWithExclusiveMode`, `Windows(windows_core::Error)`, etc.

#### Format Negotiation

- `WaveFormat::new(storebits, validbits, sample_type, samplerate, channels, channel_mask)` — constructs `WAVEFORMATEXTENSIBLE`
- Format check: `get_mixformat()` → `is_supported()` → returns `None` if direct match, `Some(WaveFormat)` if closest match
- `autoconvert` flag essential for process loopback

---

### 1.2 CamillaDSP — WASAPI Backend

#### Architecture Overview

CamillaDSP WASAPI backend ([`src/wasapidevice.rs`](reference/camilladsp/src/wasapidevice.rs:1), 1254 lines) uses a **dual-thread model**:
- **Outer thread** — lifecycle, format conversion, resampling, status reporting
- **Inner thread** — real-time WASAPI interaction (event-driven read/write)

#### Device Abstraction Layer

Two traits in [`src/audiodevice.rs:222-243`](reference/camilladsp/src/audiodevice.rs:222): `PlaybackDevice` and `CaptureDevice`, with factory dispatch via `#[cfg(target_os = "windows")]`.

Data flows through `AudioChunk` — a multi-channel container with `waveforms: Vec<Vec<PrcFmt>>`, frame count, and timestamp.

#### WASAPI Buffer Management

**Pre-allocated buffer pool**: Creates `channel_capacity + 2` buffers up front, recycles via channels.
**Zero-copy approach**: Reuses `Vec<u8>` buffers, resizes only when needed.
**Saved buffer**: If send channel full, saves buffer locally instead of dropping.

#### Threading Model

**Four-thread architecture** with real-time thread priority via `audio_thread_priority` crate. Inner↔Outer communication via `crossbeam_channel::bounded()`. Stop signal via `Arc<AtomicBool>`.

#### Error Handling & Recovery

- Device format change → `DisconnectReason::FormatChange`
- Missed event recovery → stop/reset/start
- Buffer underrun → fill with zeros
- Sample rate change detection via `ValueWatcher`

---

### 1.3 Key Patterns for rsac Windows Backend

#### COM Threading Recommendations
- MTA per audio thread, `ComGuard` RAII struct
- COM objects thread-local, cannot cross threads

#### Device Enumeration Strategy

| wasapi-rs Pattern | rsac Mapping |
|---|---|
| `DeviceEnumerator::new()` | `DeviceEnumerator::new()` |
| `get_device_collection(Direction::Capture)` | `get_input_devices()` |
| `get_default_device(Direction::Capture)` | `get_default_device(DeviceKind::Input)` |
| `device.get_friendlyname()` | `AudioDevice::get_name()` |
| `device.get_id()` | `AudioDevice::get_id()` |

#### Application Capture Implementation

| rsac `CaptureTarget` | WASAPI Implementation |
|---|---|
| `SystemDefault` | Default render device + loopback capture |
| `Device(DeviceId)` | Direct device capture |
| `Application(ApplicationId)` | `new_application_loopback_client(pid, false)` |
| `ApplicationByName(String)` | `sysinfo` → PID → `new_application_loopback_client()` |
| `ProcessTree(ProcessId)` | `new_application_loopback_client(pid, true)` |

Critical: process loopback = shared mode only, autoconvert required, use parent PID for tree.

#### Buffer Bridge Design (→ BridgeStream)
Inner thread: COM init → create client → event loop → read → convert to f32 → push to rtrb.
Consumer: pull from rtrb → AudioBuffer.
Buffer sizing: ~4-8× WASAPI buffer period.

#### Error Mapping (→ AudioError)

| WASAPI Error | rsac AudioError | Recoverability |
|---|---|---|
| `DeviceNotFound` | `DeviceNotFoundError` | Fatal |
| `UnsupportedFormat` | `UnsupportedFormat` | Fatal |
| `EventTimeout` | `Timeout` | TransientRetry |
| `DataLengthMismatch` | `BufferError` | Recoverable |
| Session disconnect (FormatChanged) | `ConfigurationError` | TransientRetry |

#### Capability Reporting (→ PlatformCapabilities)
System capture: true, device capture: true, application capture: true (Win10+), process tree: true (Win10+), format negotiation: true (NOT for process loopback), event-driven: true, autoconvert: true (shared mode).

---

## 2. Linux Platform (PipeWire)

### 2.1 wiremix — PipeWire App Capture

#### Architecture Overview
TUI mixer for PipeWire with clean separation: `src/wirehose/` (PipeWire interaction) and `src/view.rs` (UI). Dependencies: `pipewire = "0.9.2"`, `libspa = "0.9.2"`, `bytemuck`, `nix`.

#### PipeWire Threading & !Send+!Sync
**Dedicated PipeWire thread** pattern: `Session::spawn()` creates thread, runs `MainLoopRc`. Cross-thread via `pipewire::channel::channel::<Command>()`. Events go from PipeWire→UI via `EventHandler` trait (`Send + 'static`). Shutdown via `EventFd`.

All PipeWire objects live exclusively on PipeWire thread. Only serializable enums cross boundaries.

#### Node Discovery & Tracking
Registry `global` callback filters by `media.class`: `Audio/Sink`, `Audio/Source`, `Stream/Output/Audio`, `Stream/Input/Audio`. State maintained in `HashMap<ObjectId, Node/Device/Client/Link/Metadata>`.

#### Application Audio Capture
`capture_node()` targets nodes via `TARGET_OBJECT` + `STREAM_CAPTURE_SINK` + `STREAM_MONITOR`. Format: `AudioFormat::F32P`. Flags: `AUTOCONNECT | MAP_BUFFERS`.

#### Stream & Buffer Handling
`stream.dequeue_buffer()` → `buffer.datas_mut()` → per-channel data as `&[f32]` via `bytemuck::cast_slice()`. Peaks stored in `Arc<[AtomicF32]>`. First buffer after connection skipped (contains zeros).

#### Dynamic Reconnection
`CaptureEligibility`: `Eligible`, `Ineligible`, `NeedsRestart`. `StreamRegistry` manages deferred deletion — objects moved to "garbage" lists, cleaned up outside callbacks via `EventFd`.

---

### 2.2 pipewire-rs — Official Rust Bindings

#### Threading Model & MainLoop
PipeWire objects are `!Send + !Sync`. `MainLoop` + `MainLoopRc`, or `ThreadLoop` with lock/unlock. Two patterns: MainLoop (wiremix) or ThreadLoop (OBS).

#### Channel Communication
`pipewire::channel`: Uses Unix pipe + `Mutex<VecDeque<T>>`. `Sender` is `Clone + Send`. `Receiver::attach()` registers on PipeWire loop.

#### Stream API
`Stream` wraps `pw_stream`. States: `Error`, `Unconnected`, `Connecting`, `Paused`, `Streaming`. Callbacks: `state_changed`, `param_changed`, `process`, `io_changed`, `drained`. `StreamFlags`: `AUTOCONNECT`, `MAP_BUFFERS`, `RT_PROCESS`, `DONT_RECONNECT`.

#### Registry & Object Discovery
`Registry::add_listener_local()` → `global(callback)` / `global_remove(callback)`. `bind<T: ProxyT>(object)` creates typed proxies.

---

### 2.3 OBS PipeWire Audio Capture

#### Architecture Overview
C plugin using **virtual sink + link management** strategy. Uses `pw_thread_loop` with explicit lock/unlock.

#### Node & Port Tracking
Registry categorizes: `Stream/Output/Audio` → target nodes, `Audio/Sink` → system sinks, ports by direction, clients by APP_NAME/APP_PROCESS_BINARY, metadata for default sink.

#### Application Targeting Strategy
Name-matching: `binary`, `app_name`, `name` compared case-insensitive. "except" mode inverts match.

#### Link Management (dynamic reconnection)
1. Create virtual null-audio-sink via `pw_core_create_object("adapter", ...)`
2. Connect capture stream to virtual sink by serial
3. Link app ports to virtual sink ports by channel name via `pw_core_create_object("link-factory", ...)`
4. Auto-link on new nodes/ports. Default sink change → recreate virtual sink.

#### Audio Data Flow
```
App Node Output Ports → [PW Links] → Virtual Sink Input Ports → [mixing] → Capture Stream → OBS
```
`pw_stream_dequeue_buffer()` → read `buf->datas[i].data` → pass to OBS. No intermediate copying.

---

### 2.4 Key Patterns for rsac Linux Backend

#### PipeWire Threading Recommendations (→ BridgeStream)
Dedicated PipeWire thread with `MainLoopRc`. Process callback pushes to `rtrb`. Commands via `pipewire::channel`. Shutdown via `EventFd`.

```
Consumer thread                    PipeWire thread (MainLoop::run())
CapturingStream::read_chunk()      process callback:
  ← rtrb::Consumer ←──────────      → rtrb::Producer.push(audio_data)
AudioCapture::start()              registry listener:
  → pw::channel::Sender ─────→      ← pw::channel::Receiver
Drop / stop
  → EventFd.arm() ───────────→      main_loop.quit()
```

#### Node Discovery Strategy (→ DeviceEnumerator)
Registry-based: `Audio/Sink` → outputs, `Audio/Source` → inputs, `Stream/Output/Audio` → app streams. Properties: `NODE_NAME`, `NODE_DESCRIPTION`, `APP_NAME`, `APP_PROCESS_BINARY`, `OBJECT_SERIAL`, `MEDIA_CLASS`. Default via `"default"` metadata object.

#### Application Capture Implementation (→ CaptureTarget)
**Strategy A (simple)**: `TARGET_OBJECT = <serial>` — one node at a time.
**Strategy B (OBS pattern)**: Virtual sink + link management — multiple apps, name matching.

Recommendations:
- `SystemDefault` / `Device(DeviceId)` → Strategy A
- `Application(ApplicationId)` → Strategy A (target by serial)
- `ApplicationByName(String)` → Strategy B (virtual sink + name matching)
- `ProcessTree(ProcessId)` → Strategy B (virtual sink + client binary/PID matching)

#### Buffer Bridge Design
In `process` callback: `dequeue_buffer()` → convert to f32 → push to `rtrb::Producer`. Request `F32LE` (interleaved). Skip first buffer. Validate `chunk->stride != 0`.

#### Error Mapping

| PipeWire Error | rsac AudioError | Recoverability |
|---|---|---|
| Core connection failure | `ConnectionFailed` | Fatal |
| Stream state Error | `StreamError` | TransientRetry |
| Node not found | `DeviceNotFound` | Recoverable |
| Virtual sink creation fails | `StreamCreationFailed` | Recoverable |
| Format negotiation fails | `UnsupportedFormat` | Fatal |
| `pipewire::init()` fails | `BackendInitFailed` | Fatal |

#### Capability Reporting
System capture: true, device selection: true, application capture: true, application by name: true, process tree: true (userspace), exclusive mode: true, loopback: true, dynamic device changes: true.

---

## 3. macOS Platform (CoreAudio Process Tap)

### 3.1 AudioCap — Swift Process Tap

#### Architecture Overview
SwiftUI app: `ProcessTap` (tap lifecycle), `ProcessTapRecorder` (recording), `AudioProcessController` (process discovery), `CoreAudioUtils` (property helpers), `AudioRecordingPermission` (TCC).

#### Process Tap (CATapDescription) Setup
```swift
let tapDescription = CATapDescription(stereoMixdownOfProcesses: [objectID])
tapDescription.uuid = UUID()
tapDescription.muteBehavior = muteWhenRunning ? .mutedWhenTapped : .unmuted
AudioHardwareCreateProcessTap(tapDescription, &tapID)
```
Takes AudioObjectID (not PID), UUID for aggregate device reference, muteBehavior controls whether tapped audio is muted.

#### Aggregate Device Creation & Wiring
Dictionary with: `kAudioAggregateDeviceNameKey`, `kAudioAggregateDeviceUIDKey`, `kAudioAggregateDeviceMainSubDeviceKey` (system output for clock), `kAudioAggregateDeviceIsPrivateKey`, `kAudioAggregateDeviceTapAutoStartKey`, `kAudioAggregateDeviceSubDeviceListKey`, `kAudioAggregateDeviceTapListKey`.

Created via `AudioHardwareCreateAggregateDevice(description, &aggregateDeviceID)`.

#### Audio Data Flow (AUHAL Callback)
```swift
AudioDeviceCreateIOProcIDWithBlock(&deviceProcID, aggregateDeviceID, queue, ioBlock)
AudioDeviceStart(aggregateDeviceID, deviceProcID)
```
`ioBlock` receives `AudioBufferList*` — captured audio in PCM format. Runs on CoreAudio real-time thread.

#### Process Discovery
1. `kAudioHardwarePropertyProcessObjectList` → list of audio process object IDs
2. Each resolved: PID via `kAudioProcessPropertyPID`, bundle ID via `kAudioProcessPropertyBundleID`, activity via `kAudioProcessPropertyIsRunning`
3. `kAudioHardwarePropertyTranslatePIDToProcessObject` for PID→AudioObjectID translation

#### Permissions & Entitlements
- `com.apple.security.device.audio-input` — required
- TCC `kTCCServiceAudioCapture` — process tap specific
- Does NOT require screen recording permission

#### Tap Lifecycle
Create → `prepare()` (CATapDescription + ProcessTap + Aggregate) → `run()` (IOProc + Start) → `invalidate()` (Stop → DestroyIOProc → DestroyAggregate → DestroyTap)

---

### 3.2 audio-rec — C++/ObjC Process Tap

#### Public API Design
```c
typedef struct {
    AudioDeviceIOProcID ioproc;
    AudioObjectID pidObj, tapID, aggregatedID;
    pid_t pid;
    aur_callback_t callback;
    void* userData;
} aur_rec_t;
```
Functions: `aur_init(pid, callback, &handle)`, `aur_start(handle, userData)`, `aur_stop(handle)`, `aur_deinit(handle)`.

#### Process Tap Implementation
PID → AudioObjectID via `kAudioHardwarePropertyTranslatePIDToProcessObject`. `CATapDescription` with `privateTap = true`, `exclusive = false`, `mixdown = true`.

**Minimal aggregate device** — no sub-device list, no main sub-device. Just name, UID, private flag, tap list.

---

### 3.3 screencapturekit-rs — ScreenCaptureKit Bindings

#### ObjC FFI Patterns from Rust
1. **Swift Bridge with C ABI**: Compiles Swift Package, links static library
2. **Opaque Pointer + Ref Counting**: `*const c_void` with explicit retain/release
3. **Context Pointer for Callbacks**: Heap-allocated context passed through FFI as `*mut c_void`
4. **Packed FFI Structs**: Offset+length for batch string data

#### Audio Capture via ScreenCaptureKit
`SCStream` provides audio alongside screen capture. Config: `set_captures_audio(true)`, `set_sample_rate(48000)`, `set_channel_count(2)`. Different from Process Tap — requires screen recording permission.

#### Error Handling
21 error variants in `SCError`, 21 Apple-defined `SCStreamErrorCode` values (-3801 to -3821).

---

### 3.4 Key Patterns for rsac macOS Backend

#### Process Tap Implementation Strategy
Use CoreAudio Process Tap (not ScreenCaptureKit): no screen recording permission, lower latency, per-process targeting, macOS 14.0+.

Sequence: translate PID → AudioObjectID → CATapDescription → AudioHardwareCreateProcessTap → aggregate device → AudioDeviceCreateIOProcID → AudioDeviceStart.

#### ObjC FFI Strategy for Rust
Option B recommended: Direct CoreAudio C API + minimal ObjC for `CATapDescription` only. Most CoreAudio functions are plain C.

#### CaptureTarget Mapping

| rsac CaptureTarget | macOS Mapping |
|---|---|
| `SystemDefault` | Standard AUHAL capture (no tap) |
| `Device(DeviceId)` | Standard CoreAudio device capture |
| `Application(ApplicationId)` | PID → AudioObjectID → CATapDescription |
| `ApplicationByName(String)` | Enumerate → find → same |
| `ProcessTree(ProcessId)` | PID → AudioObjectID → CATapDescription (accepts array) |

#### Error Mapping

| CoreAudio Error | rsac AudioError | Recoverability |
|---|---|---|
| `kAudioHardwareBadObjectError` | `DeviceNotFound` / `BackendError` | Fatal |
| `kAudioDevicePermissionsError` | `PermissionDenied` | Fatal |
| Tap creation failure | `TapCreationFailed` | Fatal |
| PID not in audio system | `TargetNotAvailable` | Recoverable |

#### Capability Reporting
Application capture: true (macOS 14.0+), process tree: true, mute on capture: true, requires entitlement: true, requires TCC permission: true.

---

## 4. Cross-Platform Infrastructure

### 4.1 cpal — Cross-Platform Audio I/O

#### Overview

cpal (v0.18.0) is the de facto standard Rust crate for cross-platform audio I/O. It provides a unified `Host → Device → Stream` abstraction spanning 10+ backends: WASAPI, ALSA, PipeWire, PulseAudio, CoreAudio, AAudio, JACK, ASIO, WebAudio, and AudioWorklet.

**Key dependency**: `dasp_sample = "0.11"` for sample type conversions. Platform-specific dependencies include `windows` (WASAPI), `alsa` (Linux), `coreaudio-rs` + `objc2-core-audio` (macOS), `pipewire` (Linux, optional), `ndk` (Android).

#### Core Trait Hierarchy

cpal defines three core traits in [`src/traits.rs`](reference/cpal/src/traits.rs:1):

**[`HostTrait`](reference/cpal/src/traits.rs:38)**:
```rust
pub trait HostTrait {
    type Devices: Iterator<Item = Self::Device>;
    type Device: DeviceTrait;

    fn is_available() -> bool;
    fn devices(&self) -> Result<Self::Devices, DevicesError>;
    fn default_input_device(&self) -> Option<Self::Device>;
    fn default_output_device(&self) -> Option<Self::Device>;
    fn input_devices(&self) -> Result<InputDevices<Self::Devices>, DevicesError>;   // provided
    fn output_devices(&self) -> Result<OutputDevices<Self::Devices>, DevicesError>; // provided
}
```
- Platform-specific host structs (e.g., `wasapi::Host`) implement this trait
- Associated type system allows each backend to define its own `Device` and `Devices` types
- `input_devices()` / `output_devices()` are provided methods that filter using `DeviceTrait::supports_input/output()`

**[`DeviceTrait`](reference/cpal/src/traits.rs:92)**:
```rust
pub trait DeviceTrait {
    type SupportedInputConfigs: Iterator<Item = SupportedStreamConfigRange>;
    type SupportedOutputConfigs: Iterator<Item = SupportedStreamConfigRange>;
    type Stream: StreamTrait;

    fn description(&self) -> Result<DeviceDescription, DeviceNameError>;
    fn id(&self) -> Result<DeviceId, DeviceIdError>;
    fn supported_input_configs(&self) -> Result<Self::SupportedInputConfigs, SupportedStreamConfigsError>;
    fn supported_output_configs(&self) -> Result<Self::SupportedOutputConfigs, SupportedStreamConfigsError>;
    fn default_input_config(&self) -> Result<SupportedStreamConfig, DefaultStreamConfigError>;
    fn default_output_config(&self) -> Result<SupportedStreamConfig, DefaultStreamConfigError>;
    fn build_input_stream<T, D, E>(&self, config, data_callback, error_callback, timeout) -> Result<Self::Stream, BuildStreamError>;
    fn build_output_stream<T, D, E>(&self, config, data_callback, error_callback, timeout) -> Result<Self::Stream, BuildStreamError>;
    fn build_input_stream_raw<D, E>(&self, config, sample_format, data_callback, error_callback, timeout) -> Result<Self::Stream, BuildStreamError>;
    fn build_output_stream_raw<D, E>(&self, config, sample_format, data_callback, error_callback, timeout) -> Result<Self::Stream, BuildStreamError>;
}
```
- Callbacks are `FnMut + Send + 'static` — they run on a backend thread, must be movable
- `build_input_stream<T>` is a provided method that delegates to `build_input_stream_raw` with type erasure via `Data::as_slice()`
- `timeout` parameter: `None` = blocking, `Some(Duration)` = max wait; not all backends honor it

**[`StreamTrait`](reference/cpal/src/traits.rs:292)**:
```rust
pub trait StreamTrait {
    fn play(&self) -> Result<(), PlayStreamError>;
    fn pause(&self) -> Result<(), PauseStreamError>;
}
```
- Minimal interface — streams are controlled externally
- cpal asserts `Stream: Send + Sync` at compile time via macros ([`assert_stream_send!()`](reference/cpal/src/traits.rs:320), [`assert_stream_sync!()`](reference/cpal/src/traits.rs:340))

#### Platform Backend Dispatch

Platform backends are conditionally compiled via [`src/host/mod.rs`](reference/cpal/src/host/mod.rs:1):
```rust
#[cfg(windows)]                          pub(crate) mod wasapi;
#[cfg(any(target_os = "linux", ...))]    pub(crate) mod alsa;
#[cfg(any(target_os = "macos", ...))]    pub(crate) mod coreaudio;
#[cfg(feature = "pipewire")]             pub(crate) mod pipewire;
// ... etc.
```

Each backend module implements the three traits for its own types. For example, [`src/host/wasapi/mod.rs`](reference/cpal/src/host/wasapi/mod.rs:27) defines `pub struct Host;` implementing `HostTrait` with `type Device = Device` and `type Devices = Devices`.

#### WASAPI Backend Insights

The cpal WASAPI backend ([`src/host/wasapi/device.rs`](reference/cpal/src/host/wasapi/device.rs:1)) reveals several patterns relevant to rsac:

1. **COM initialization**: Uses a global `OnceLock<Enumerator>` with `com::com_initialized()` — COM is initialized lazily per-thread
2. **Device wrapping**: `Device` contains `IMMDevice` + `Arc<Mutex<Option<IAudioClientWrapper>>>` — caches an uninitialized `IAudioClient` for format queries
3. **`Send + Sync`**: Explicitly implemented via `unsafe impl Send for Device {}` / `unsafe impl Sync for Device {}` despite raw COM pointers — safe because access is coordinated through the `Mutex`
4. **Auto-conversion**: `DEFAULT_FLAGS` includes `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY` — always uses the Windows audio engine's format conversion, never exclusive mode
5. **Loopback capture**: If data flow is `eRender` (output device) but building an input stream, adds `AUDCLNT_STREAMFLAGS_LOOPBACK` flag — transparently captures output device audio
6. **Format enumeration**: Since shared mode with auto-convert always succeeds, `is_format_supported()` returns `true` unconditionally — format enumeration trials common sample rates × sample formats against the default channel count

#### CoreAudio Backend Insights

The cpal CoreAudio backend ([`src/host/coreaudio/mod.rs`](reference/cpal/src/host/coreaudio/mod.rs:1)):
1. **Helper functions**: [`asbd_from_config()`](reference/cpal/src/host/coreaudio/mod.rs:43) converts cpal config to `AudioStreamBasicDescription`
2. **Format flags**: Maps `F32`/`F64` → `kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked`, integers → `kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked`
3. **Error mapping**: CoreAudio errors → cpal errors via `From` impls — format errors → `StreamConfigNotSupported`, others → `DeviceNotAvailable`
4. **Stream safety**: `assert_stream_send!(Stream)` and `assert_stream_sync!(Stream)` — enforced at compile time

#### Error Model

cpal's error types in [`src/error.rs`](reference/cpal/src/error.rs:1) follow a hierarchy:
- **`BackendSpecificError`** — catch-all with string description
- **Operation-specific enums**: `DevicesError`, `DeviceNameError`, `DeviceIdError`, `SupportedStreamConfigsError`, `DefaultStreamConfigError`, `BuildStreamError`, `PlayStreamError`, `PauseStreamError`, `StreamError`
- Each enum has `DeviceNotAvailable` and `BackendSpecific` variants; some add `StreamConfigNotSupported`, `DeviceBusy`, `InvalidArgument`, etc.
- All error types implement `From<BackendSpecificError>` for easy conversion

**Key difference from rsac**: cpal uses per-operation error enums (many small enums) vs. rsac's unified `AudioError` with 21 categorized variants. rsac's approach is better for a library that must report errors across abstraction layers.

#### What rsac Learns from cpal

| cpal Pattern | rsac Application | Notes |
|---|---|---|
| `HostTrait → DeviceTrait → StreamTrait` | Validates `AudioCaptureBuilder → AudioCapture → CapturingStream` | rsac adds builder pattern for configuration |
| Callback-based stream (`FnMut + Send + 'static`) | rsac uses pull-based `read_chunk()` | cpal pushes data; rsac lets consumer pull — better for streaming-first |
| `Stream: Send + Sync` enforced at compile time | rsac's `CapturingStream` should be `Send` | Compile-time assertions via macros are good practice |
| `BackendSpecificError` catch-all | rsac avoids this — every error is categorized | rsac's 21-variant `AudioError` is more actionable |
| `SupportedStreamConfigRange` for format enumeration | rsac's `StreamConfig` | rsac should support format enumeration for device backends |
| No per-app capture at all | rsac's `CaptureTarget` enum | **This is the gap rsac fills** — cpal only supports device-level capture |
| Platform dispatch via `#[cfg()]` modules | rsac's `src/audio/{platform}/` modules | Same pattern, validated |
| Feature gating: `feat(pipewire)`, `feat(jack)`, etc. | rsac's `feat_windows`, `feat_linux`, `feat_macos` | Same pattern, validated |

#### Why rsac Goes Beyond cpal

1. **No per-application capture**: cpal captures from devices, not applications. rsac's `CaptureTarget::Application`, `ApplicationByName`, and `ProcessTree` are entirely novel.
2. **Callback-push vs. pull-stream**: cpal's callback model pushes data to the user. rsac's `CapturingStream::read_chunk()` lets the consumer pull data at their own pace — essential for streaming-first architecture.
3. **No capability reporting**: cpal doesn't report what features a platform supports. rsac's `PlatformCapabilities` is explicit.
4. **No error categorization**: cpal's `BackendSpecificError` is a string. rsac's `ErrorKind` + recoverability classification enables programmatic error handling.

---

### 4.2 rtrb — Lock-Free Ring Buffer

#### Overview

rtrb (v0.3.3) is a realtime-safe single-producer single-consumer (SPSC) ring buffer. It is the lock-free bridge that rsac's `BridgeStream` uses to transfer audio data from OS callback threads to consumer threads.

**Key properties**:
- **Zero dependencies** (beyond `alloc` and `core`) — `no_std` compatible
- **Lock-free and wait-free**: All read/write operations return immediately
- **Single allocation**: Memory allocated once at construction, never again
- **No overwriting**: Full buffer returns error; consumer must read before producer can write more
- **Rust edition 2018**, MSRV 1.38 — extremely mature and stable

#### Core API

[`RingBuffer::new(capacity)`](reference/rtrb/src/lib.rs:132) returns `(Producer<T>, Consumer<T>)`:

```rust
let (mut producer, mut consumer) = RingBuffer::<f32>::new(capacity);
```

**Producer** ([`src/lib.rs:289`](reference/rtrb/src/lib.rs:289)):
- [`push(value)`](reference/rtrb/src/lib.rs:330) → `Result<(), PushError<T>>` — moves one element in
- [`slots()`](reference/rtrb/src/lib.rs:361) — number of available write slots (reads atomic head)
- [`cached_slots()`](reference/rtrb/src/lib.rs:372) — fast path using cached head (no atomic read)
- [`is_full()`](reference/rtrb/src/lib.rs:414) — quick check
- [`is_abandoned()`](reference/rtrb/src/lib.rs:462) — returns `true` if `Consumer` was dropped (via `Arc::strong_count`)
- `Send` but not `Sync` — can move between threads but only one thread can write

**Consumer** ([`src/lib.rs:514`](reference/rtrb/src/lib.rs:514)):
- [`pop()`](reference/rtrb/src/lib.rs:566) → `Result<T, PopError>` — moves one element out
- [`peek()`](reference/rtrb/src/lib.rs:613) → `Result<&T, PeekError>` — look without consuming
- [`slots()`](reference/rtrb/src/lib.rs:640) — number of available read slots (reads atomic tail)
- [`cached_slots()`](reference/rtrb/src/lib.rs:651) — fast path using cached tail
- [`is_empty()`](reference/rtrb/src/lib.rs:693) — quick check
- [`is_abandoned()`](reference/rtrb/src/lib.rs:740) — returns `true` if `Producer` was dropped
- `Send` but not `Sync`

#### Chunk-Based I/O

The [`chunks`](reference/rtrb/src/chunks.rs:1) module provides bulk read/write operations — **critical for audio where you process frames in blocks**:

**Writing chunks** (Producer):
- [`write_chunk_uninit(n)`](reference/rtrb/src/chunks.rs:217) → `Result<WriteChunkUninit, ChunkError>` — returns `n` uninitialized slots
  - [`WriteChunkUninit::as_mut_slices()`](reference/rtrb/src/chunks.rs:702) → `(&mut [MaybeUninit<T>], &mut [MaybeUninit<T>])` — two slices (ring buffer wraps)
  - [`WriteChunkUninit::fill_from_iter(iter)`](reference/rtrb/src/chunks.rs:793) → `usize` — fill from iterator, auto-commits
  - [`WriteChunkUninit::commit(n)`](reference/rtrb/src/chunks.rs:721) / [`commit_all()`](reference/rtrb/src/chunks.rs:732) — make slots readable
- [`write_chunk(n)`](reference/rtrb/src/chunks.rs:179) → `WriteChunk` — same but `Default`-initialized (safe but slower)
- [`push_partial_slice(slice)`](reference/rtrb/src/chunks.rs:280) → `(&[T], &[T])` — copies as many as possible, returns (pushed, remainder); requires `T: Copy`
- [`push_entire_slice(slice)`](reference/rtrb/src/chunks.rs:306) → `Result<(), ChunkError>` — copies all or fails

**Reading chunks** (Consumer):
- [`read_chunk(n)`](reference/rtrb/src/chunks.rs:341) → `Result<ReadChunk, ChunkError>` — returns `n` readable slots
  - [`ReadChunk::as_slices()`](reference/rtrb/src/chunks.rs:877) → `(&[T], &[T])` — two immutable slices
  - [`ReadChunk::commit(n)`](reference/rtrb/src/chunks.rs:961) / [`commit_all()`](reference/rtrb/src/chunks.rs:968) — free slots for reuse
  - Implements `IntoIterator` — moves items out one by one
- [`pop_partial_slice(slice)`](reference/rtrb/src/chunks.rs:406) → `(&mut [T], &mut [T])` — copies as many as possible; `T: Copy`
- [`pop_entire_slice(slice)`](reference/rtrb/src/chunks.rs:519) → `Result<(), ChunkError>` — copies exactly N or fails; `T: Copy`

**Why two slices?** The ring buffer is circular — data may wrap around the end. `as_slices()` returns (end-of-buffer, start-of-buffer) when data spans the boundary, or (all-data, empty) when it doesn't.

#### Internal Architecture

**Position tracking** ([`src/lib.rs:89-108`](reference/rtrb/src/lib.rs:89)):
- `head` and `tail` are `CachePadded<AtomicUsize>` — cache-line padded to prevent false sharing
- Positions range `0 .. 2 * capacity` (not `0 .. capacity`) — this avoids the ambiguity between "full" and "empty" when head == tail
- [`collapse_position(pos)`](reference/rtrb/src/lib.rs:171) maps `0..2*capacity` → `0..capacity` for actual indexing
- [`distance(a, b)`](reference/rtrb/src/lib.rs:216) computes the logical distance between two positions

**Memory ordering**:
- Producer stores tail with `Ordering::Release` after writing
- Consumer loads tail with `Ordering::Acquire` before reading
- Consumer stores head with `Ordering::Release` after reading
- Producer loads head with `Ordering::Acquire` before writing
- Cached positions use `Cell<usize>` (thread-local, no atomics) for fast-path checks

**Caching optimization**: Both Producer and Consumer cache the other side's position. They only read the atomic variable when the cached value suggests the buffer might be full/empty. This dramatically reduces atomic operations in the common case.

#### Error Handling

Three simple error types:
- `PushError::Full(T)` — buffer full, returns the value back
- `PopError::Empty` — buffer empty
- `ChunkError::TooFewSlots(usize)` — not enough slots, returns available count

All implement `std::error::Error` (behind `std` feature) and `Display`.

#### std::io Integration

When `T = u8` and the `std` feature is enabled:
- [`Producer<u8>`](reference/rtrb/src/chunks.rs:1081) implements `std::io::Write` — returns `WouldBlock` when full
- [`Consumer<u8>`](reference/rtrb/src/chunks.rs:1101) implements `std::io::Read` — returns `WouldBlock` when empty

#### How rsac Should Use rtrb in BridgeStream

**Recommended usage pattern for audio capture**:

```rust
// Setup
let frames_per_period = 480; // ~10ms at 48kHz
let channels = 2;
let capacity = frames_per_period * channels * 8; // 8 periods of headroom
let (mut producer, mut consumer) = RingBuffer::<f32>::new(capacity);

// OS callback thread (Producer side):
fn audio_callback(data: &[f32], producer: &mut Producer<f32>) {
    match producer.push_partial_slice(data) {
        (pushed, []) => { /* all data written */ }
        (pushed, remainder) => {
            // Buffer overflow — log but don't block
            eprintln!("ring buffer overflow: {} samples dropped", remainder.len());
        }
    }
}

// Consumer thread (CapturingStream::read_chunk):
fn read_chunk(consumer: &mut Consumer<f32>, frames: usize, channels: usize) -> Option<Vec<f32>> {
    let samples = frames * channels;
    match consumer.read_chunk(samples) {
        Ok(chunk) => {
            let (first, second) = chunk.as_slices();
            let mut buf = Vec::with_capacity(samples);
            buf.extend_from_slice(first);
            buf.extend_from_slice(second);
            chunk.commit_all();
            Some(buf)
        }
        Err(ChunkError::TooFewSlots(available)) => {
            // Not enough data yet — try smaller read or return None
            None
        }
    }
}
```

**Capacity sizing recommendations**:
| Scenario | Recommended Capacity | Reasoning |
|---|---|---|
| Low-latency monitoring | 4× buffer period | Minimum viable; drops possible under load |
| General streaming | 8× buffer period | Good balance of latency and reliability |
| Recording/analysis | 16-32× buffer period | Tolerates scheduling jitter, GC pauses |
| With format conversion | +2× overhead | Conversion may produce slightly different frame counts |

For 48kHz stereo with 10ms periods (480 frames):
- Minimum: `480 * 2 * 4 = 3840` samples (~80ms)
- Recommended: `480 * 2 * 8 = 7680` samples (~160ms)
- Conservative: `480 * 2 * 16 = 15360` samples (~320ms)

**Key patterns for BridgeStream**:
1. **Use `push_partial_slice()`** on the producer side — never block in an OS audio callback
2. **Use `read_chunk()` or `pop_partial_slice()`** on the consumer side — handle partial reads gracefully
3. **Check `is_abandoned()`** to detect when the other end has been dropped — signals stream shutdown
4. **Size for headroom**: Audio callbacks are periodic but scheduling is imprecise; overprovision capacity
5. **Element type should be `f32`**: rsac standardizes on f32 internally; convert in the OS callback before pushing

---

## 5. Cross-Platform Patterns & Synthesis

### 5.1 Universal Threading Model

Every reference project follows the same fundamental threading pattern:

```
┌──────────────────┐         ┌──────────────────┐
│   OS Audio Thread │         │  Consumer Thread  │
│  (callback-based) │  rtrb   │  (pull-based)     │
│                   │ ──────► │                   │
│  WASAPI events    │         │  read_chunk()     │
│  PW process cb    │ lock-   │  → AudioBuffer    │
│  CA IOProc        │ free    │                   │
└──────────────────┘         └──────────────────┘
        ▲                            │
        │ commands                   │ commands
        │ (channel)                  │ (channel)
        ▼                            ▼
┌──────────────────────────────────────────────┐
│              Control Thread                   │
│  AudioCapture: start() / stop() / drop()     │
└──────────────────────────────────────────────┘
```

| Platform | OS Thread Pattern | Command Channel | Data Bridge |
|---|---|---|---|
| **Windows** | Event-driven loop (`WaitForSingleObject`) | `Arc<AtomicBool>` or `mpsc` | `VecDeque` (wasapi-rs) / `crossbeam_channel` (CamillaDSP) |
| **Linux** | PipeWire `MainLoop::run()` + `process` callback | `pipewire::channel` | Direct `dequeue_buffer` access |
| **macOS** | CoreAudio IOProc callback on real-time thread | Implicit via `AudioDeviceStart/Stop` | Callback writes to user buffer |
| **rsac** | Platform-specific (above) | `pipewire::channel` or `mpsc` | **rtrb SPSC ring buffer** |

**rsac's `BridgeStream<S>` unifies all three** by providing the rtrb-based bridge + platform-generic control flow.

### 5.2 BridgeStream Validation

The `BridgeStream` design is validated by all reference projects:

| BridgeStream Component | Windows Validation | Linux Validation | macOS Validation |
|---|---|---|---|
| rtrb Producer in callback | CamillaDSP inner thread pushes to `crossbeam_channel` | wiremix pushes peaks in `process` cb | AudioCap writes in IOProc |
| rtrb Consumer for `read_chunk()` | CamillaDSP outer thread reads from channel | OBS pulls from `pw_stream_dequeue_buffer` | audio-rec reads in callback |
| Overflow handling (non-blocking) | CamillaDSP saves buffer locally | wiremix skips first buffer | AudioCap bounded by buffer list size |
| Shutdown signal | `Arc<AtomicBool>` | `EventFd` → `main_loop.quit()` | `AudioDeviceStop` |
| Capacity sizing | ~4-8× WASAPI period | PipeWire negotiates buffer size | CoreAudio sets buffer frame size |

**rtrb is the right choice** because:
1. Zero dependencies (matches rsac's minimal dependency philosophy)
2. Lock-free and wait-free (safe for real-time audio callback threads)
3. Chunk-based I/O (`read_chunk()` / `push_partial_slice()`) maps directly to audio frame blocks
4. `is_abandoned()` provides natural shutdown detection
5. `Send` but not `Sync` — correct ownership model for SPSC

### 5.3 CaptureTarget Implementation Matrix

| rsac CaptureTarget | Windows (WASAPI) | Linux (PipeWire) | macOS (CoreAudio) |
|---|---|---|---|
| `SystemDefault` | Default render device + loopback flag | Default sink monitor (metadata lookup) | Standard AUHAL capture |
| `Device(DeviceId)` | Direct device capture via IMMDevice ID | Node by serial/name | CoreAudio device by AudioObjectID |
| `Application(ApplicationId)` | `new_application_loopback_client(pid, false)` | Strategy A: `TARGET_OBJECT = <serial>` | PID → AudioObjectID → CATapDescription |
| `ApplicationByName(String)` | sysinfo → PID → process loopback | Strategy B: Virtual sink + name matching | Enumerate processes → find → tap |
| `ProcessTree(ProcessId)` | `new_application_loopback_client(pid, true)` | Strategy B: Virtual sink + PID/binary matching | CATapDescription with process array |

**Platform-specific constraints**:
- **Windows**: Process loopback requires Win10+, shared mode only, autoconvert required, use parent PID for tree capture
- **Linux**: Virtual sink strategy requires creating/managing PipeWire objects dynamically; Strategy A is simpler for single-app capture
- **macOS**: Requires macOS 14.0+, `com.apple.security.device.audio-input` entitlement, TCC `kTCCServiceAudioCapture` permission

### 5.4 Error Taxonomy Cross-Reference

| rsac AudioError Category | Windows Source | Linux Source | macOS Source | cpal Equivalent |
|---|---|---|---|---|
| `DeviceNotFound` | `WasapiError::DeviceNotFound` | Node not in registry | `kAudioHardwareBadObjectError` | `DeviceNotAvailable` |
| `UnsupportedFormat` | `WasapiError::UnsupportedFormat` | Format negotiation failure | `kAudioFormatUnsupportedDataFormatError` | `StreamConfigNotSupported` |
| `PermissionDenied` | N/A (WASAPI doesn't gate) | N/A (PipeWire session-based) | `kAudioDevicePermissionsError` / TCC | N/A |
| `Timeout` | `WasapiError::EventTimeout` | Loop iteration timeout | IOProc stall detection | N/A |
| `BufferError` | `WasapiError::DataLengthMismatch` | Buffer dequeue fails | Buffer list underrun | `BufferUnderrun` |
| `StreamError` | Session disconnect | Stream state → Error | IOProc failure | `StreamInvalidated` |
| `BackendInitFailed` | COM init failure | `pipewire::init()` fails | CoreAudio unavailable | `BackendSpecific` |
| `ConnectionFailed` | Device activation fails | Core connection failure | Aggregate device creation fails | `BackendSpecific` |
| `ConfigurationError` | Format change disconnect | Param renegotiation | Format change notification | `StreamConfigNotSupported` |
| `TargetNotAvailable` | Process not playing audio | Node not found | PID not in audio system | N/A |

**rsac's error model is superior to cpal's** because:
1. Every variant has an `ErrorKind` and recoverability classification
2. No opaque `BackendSpecificError` catch-all
3. Three-state recoverability (`Recoverable`, `TransientRetry`, `Fatal`) enables programmatic retry logic

### 5.5 Capability Matrix

| Capability | Windows | Linux | macOS |
|---|---|---|---|
| System audio capture | ✅ (loopback) | ✅ (sink monitor) | ✅ (AUHAL) |
| Device selection | ✅ | ✅ | ✅ |
| Application capture | ✅ (Win10+) | ✅ (PipeWire) | ✅ (macOS 14+) |
| Application by name | ✅ (via sysinfo) | ✅ (node properties) | ✅ (process enumeration) |
| Process tree capture | ✅ (INCLUDE_TARGET_PROCESS_TREE) | ✅ (userspace PID/binary match) | ✅ (CATapDescription array) |
| Mute on capture | ❌ | ❌ | ✅ (muteBehavior) |
| Format negotiation | ✅ (not for process loopback) | ✅ (PW negotiation) | ✅ (ASBD) |
| Event-driven callbacks | ✅ (WASAPI events) | ✅ (PW process callback) | ✅ (IOProc) |
| Auto-format conversion | ✅ (AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM) | ✅ (PW SPA format negotiation) | ✅ (AudioConverter) |
| Dynamic reconnection | ⚠️ (session events) | ✅ (registry callbacks) | ⚠️ (property listeners) |
| Exclusive mode | ✅ (not for process loopback) | ✅ (PW stream flags) | ❌ |
| Requires special permission | ❌ | ❌ | ✅ (entitlement + TCC) |
| Min OS version for app capture | Win10 2004+ | Any with PipeWire | macOS 14.0+ |

This maps directly to rsac's `PlatformCapabilities` struct — each cell becomes a boolean field.

---

## 6. Key Takeaways for rsac Implementation

### Priority 1: Core Infrastructure (Phase 1-2)

1. **`BridgeStream<S>` using rtrb is validated**. Implement it as:
   - `rtrb::Producer<f32>` held by the OS callback thread
   - `rtrb::Consumer<f32>` held by `CapturingStream`
   - Use `push_partial_slice()` on the producer side (never block in callbacks)
   - Use `read_chunk()` on the consumer side
   - Default capacity: 8× the platform's buffer period × channels
   - Check `is_abandoned()` for shutdown detection

2. **Error taxonomy is sufficient**. The 21-variant `AudioError` maps cleanly to errors from all three platforms. No new variants needed. Ensure every variant has `ErrorKind` + recoverability.

3. **`PlatformCapabilities` should reflect the capability matrix** (Section 5.5). Report honestly — never pretend a platform supports something it doesn't.

### Priority 2: Platform Backends (Phase 3)

4. **macOS first** (validates the architecture end-to-end):
   - Use CoreAudio Process Tap, not ScreenCaptureKit
   - Direct C API for most of CoreAudio, minimal ObjC only for `CATapDescription`
   - IOProc callback → convert to f32 → push to rtrb
   - PID → AudioObjectID translation for `CaptureTarget`

5. **Windows second**:
   - COM MTA initialization per audio thread (`ComGuard` RAII)
   - Event-driven loop for WASAPI capture
   - Process loopback for `Application` / `ProcessTree` targets
   - Autoconvert flag mandatory for process loopback
   - Parent PID for tree capture

6. **Linux third**:
   - Dedicated PipeWire thread with `MainLoopRc`
   - `pipewire::channel` for commands, `EventFd` for shutdown
   - Strategy A (TARGET_OBJECT) for simple capture targets
   - Strategy B (virtual sink + link management) for ApplicationByName / ProcessTree
   - Skip first buffer after connection (contains zeros)

### Priority 3: API Refinements (Phase 4-5)

7. **Pull-based streaming is correct** (validated by contrast with cpal's push model). `CapturingStream::read_chunk()` → `AudioBuffer` gives consumers control over timing.

8. **cpal's trait hierarchy validates rsac's API structure** but rsac intentionally differs:
   - Builder pattern instead of chained method calls
   - `CaptureTarget` enum instead of device-only capture
   - Unified `AudioError` instead of per-operation error enums
   - `PlatformCapabilities` for explicit feature reporting

9. **`Stream: Send` should be enforced at compile time** (like cpal's `assert_stream_send!()` macro). `CapturingStream` must be movable between threads.

10. **Format handling**: Use f32 internally everywhere. Convert in the OS callback thread before pushing to rtrb. The autoconvert flags on Windows and format negotiation on PipeWire/CoreAudio handle OS-side conversion.


---

## Reference Delta & Unadopted Techniques (2026-05-29)

This section appends per-repo NEW unadopted techniques and upstream-vs-pinned deltas mined from the vendored submodules. rsac is **capture-only** (no mixing/resampling/encoding/playback/effects/VAD/AEC). Techniques crossing that line are tagged INFORMATIONAL ONLY.

### wasapi-rs (pinned `wasapi = 0.22.0` → submodule now `0.23.0`)

**Upstream delta (0.22.0 → 0.23.0):**
- Memory safety: `WaveFormat::parse_from_blob_bytes()` reads `WAVEFORMATEXTENSIBLE` from byte-aligned device property blobs via `read_unaligned()`, fixing UB vs. the old aligned-reference cast (`waveformat.rs:92-118`).
- Pointer safety: `std::ptr::from_mut`/`std::ptr::from_ref` replace manual `as` casts for const-correctness (`api.rs:706-717`).
- COM cleanup: `AudioEffectsManager::get_audio_effects()` now `CoTaskMemFree`s a non-null buffer even when `num_effects==0` (`api.rs:1865-1877`).
- New APIs (Win11+): `AudioEffectsManager` + `AcousticEchoCancellationControl` (`api.rs:1849-1913`); `AudioClientProperties` builder + `StreamOption` enum (Raw/MatchFormat/Ambisonics) (`api.rs:1290-1465`); generic `GetService<T>()` interface resolution (`api.rs:1215-1225`).
- **MSRV bump 1.74 → 1.76** (commit `75d017a`). rsac is `edition = "2021"` with no `rust-version` pin — adopting 0.23 forces an MSRV declaration.
- Transitive bumps: `log 0.4`, `thiserror 2.0`, `sysinfo 0.38`, `rand 0.10`. No public-API breaks.

**NEW unadopted techniques relevant to rsac (capture-only):**
- `parse_from_blob_bytes()` (`waveformat.rs:92-118`) — adopt the safe-parse pattern in any rsac path that reads `WAVEFORMATEX` from a device blob; setup-time only, RT-irrelevant. *Relevance:* `WindowsDeviceEnumerator` format discovery.
- `std::ptr::from_mut`/`from_ref` (`api.rs:706-717`) — audit `WindowsCaptureThread` + device enumeration unsafe casts. *Relevance:* RT-callback pointer hygiene; preferred where any unsafe touches audio buffers.
- `StreamOption`/`AudioClientProperties` builder (`api.rs:1290-1465`) — rsac hardcodes shared-mode format/flags; `Raw` (AEC-free capture) and `MatchFormat` are legitimately *capture-relevant* config knobs (NOT effects processing). *Relevance:* `create_audio_client()` format negotiation; medium effort.
- `AudioEffectsManager` / AEC (`api.rs:1849-1913`) — **INFORMATIONAL ONLY**, out of scope (effects/AEC). Could at most inform a future read-only "OS effects present" capability hint; do not wire control.
- `GetService<T>()` generic pattern (`api.rs:1215-1225`) — INFORMATIONAL; only if rsac ever exposes additional optional WASAPI interfaces.

### camilladsp (`v4.1.3`; rsac references v3)

**Upstream delta:** `wasapi-rs` still 0.22.0 (matches rsac); `windows 0.62.0` (rsac 0.62.2, compatible); uses `ringbuf 0.4.7` (rsac uses `rtrb 0.3.3` — different SPSC lib). v4 adds Bluez/DBus device-change + PipeWire 0.3.44+ features; Windows WASAPI backend stable, no breaking changes.

**NEW unadopted techniques (capture-relevant):**
- Multi-format fallback chain S32→S24→S16→F32 (`wasapi_backend/device.rs:278-301`) — probe on rejection instead of hard-failing on hardcoded f32. *Relevance:* `src/audio/windows/wasapi.rs create_audio_client()`; small.
- Exclusive-mode period alignment `calculate_aligned_period_near()` (`device.rs:333-334,408-409`) — rsac's exclusive stub doesn't align to the hardware quantum. *Relevance:* `wasapi.rs`; small.
- Session notification callbacks for disconnect/format-change (`device.rs:481-492,615-626`) — `DisconnectReason::FormatChange` vs `Error` to trigger clean restart vs. retry; rsac's `wasapi_capture_thread_main()` has none and can hang on disconnect. *Relevance:* `src/audio/windows/thread.rs`; medium. **Callback must only send an enum to a bounded channel — no alloc/lock on the WASAPI thread.**
- Sample-rate drift detection via `ValueWatcher` (`device.rs:1266,1408-1425`) — surface drift as diagnostics/`StreamStats`, NOT resample (capture-only). *Relevance:* metadata/log; medium.
- Silence/stall state machine `SilenceCounter` (`device.rs:1272,1443-1461`) — Running/Paused/Stalled/Inactive distinguishes intentional silence from a broken stream. *Relevance:* `StreamStats`; medium.
- Polling fallback with `poll_delay = device_period_hns/10` (`device.rs:682-708`) — robustness when `set_get_eventhandle()` fails on buggy drivers; rsac is event-driven only. *Relevance:* `src/audio/windows/thread.rs`; medium.
- Channel-capacity scaling from device period (`device.rs:837-840,1190-1195`) — rsac's `calculate_capacity()` uses a flat multiplier. *Relevance:* `src/bridge/mod.rs`; small.
- RT-priority boost via `audio_thread_priority` on capture thread with graceful fallback (`device.rs:643-652,1285-1296`) — rsac's Windows thread does no RT promotion. *Relevance:* new dep + `thread.rs`; small.
- INFORMATIONAL ONLY: PI rate-controller for playback feedback (`device.rs:984-994`) — playback, out of scope.

### wiremix (TUI PipeWire mixer)

**Upstream delta:** `pipewire 0.9.2` + `libspa 0.9.2` (`v0_3_44`) — exact match to rsac. `nix 0.29.0` (eventfd), `pulp 0.22.2` (SIMD). `bytemuck::cast_slice` for zero-copy conversion (rsac already uses). No version pressure; wiremix is a production reference for the APIs rsac depends on but doesn't fully exploit.

**NEW unadopted techniques (capture-relevant):**
- Native registry listeners + deferred GC via `EventFd` (`session.rs:312-451`, `proxy_registry.rs`, `stream_registry.rs`) — replaces rsac's `pw-dump` subprocess enumeration with in-process live tracking; deferred deletion avoids callback re-entrancy. *Relevance:* `src/audio/linux/mod.rs`, `thread.rs`; large. (See backlog PR-1.)
- `media.class` predicates `is_sink/is_source/is_sink_input/is_source_output` (`media_class.rs:1-18`, `node.rs:20-35`) — cleaner than substring-parsing pw-dump JSON. *Relevance:* `parse_pw_dump_nodes`; small.
- `PropertyStore` metadata extraction (`property_store.rs`, `node.rs:89-94`) — populate richer `DeviceInfo`/app metadata without parsing. *Relevance:* `src/core/interface.rs DeviceInfo`; small.
- `AtomicF32` peak metering in `process()` + `Arc<[AtomicF32]>` share-to-UI (`atomic_f32.rs:1-48`, `stream.rs:209-234`) — RT-safe level metrics; skip first (zero) buffer. *Relevance:* metering; metering is read-only metadata (in scope), but rsac's RMS/peak belongs on `AudioBuffer` consumer-side (UX-1), not necessarily in the callback.
- `pulp::Arch` SIMD peak (`stream.rs:59-86`) — portable vectorized abs/max, no NEON/SSE branches. *Relevance:* only if metering moves into the callback.
- `default`-metadata monitoring for default sink/source changes (`metadata.rs:1-58`) — live default-device-change events without `pw-metadata`. *Relevance:* `get_pipewire_default_device`; medium.
- `pipewire::channel` for loop-integrated commands (`session.rs:53-66`) vs rsac's `std::mpsc` + manual pump. *Relevance:* `thread.rs PipeWireCommand`; small.
- `EventFd`-based shutdown signaling (`session.rs:50-103`). *Relevance:* `thread.rs` shutdown; small.

### pipewire-rs (`0.9.2`, matches rsac)

**Upstream delta:** rsac pin `0.9.2` is current; upstream exposes feature flags `v0_3_44..v0_3_57` (+ `v1_1` in dev). `v0.3.50` adds `Time::buffered/queued_buffers/avail_buffers`. API stable, no breaking changes across 0.9.x. rsac uses only `v0_3_44`.

**NEW unadopted techniques (capture-relevant):**
- In-process `Registry::add_listener_local` discovery (`registry/mod.rs:67-72`) — replaces `pw-dump`/`pw-cli` subprocess enumeration. *Relevance:* `LinuxDeviceEnumerator`; large. (PR-1.)
- `Metadata::add_listener_local` default sink/source proxy (`metadata.rs:46-95`) — in-process default discovery + change notifications vs `pw-metadata`. *Relevance:* `get_pipewire_default_device`; medium.
- `Node::subscribe_params`/`enum_params` format-capability enumeration (`node.rs:41-90`) — build a real Linux `supported_formats()` list. *Relevance:* `src/audio/linux`; medium (note: PR-5 — proxy machinery is currently unused; `_registry` discarded at `thread.rs:887`).
- `state_changed` callback (`stream/mod.rs:25-52,410`) — Error/Unconnected/Connecting/Paused/Streaming transitions for disconnect detection. *Relevance:* `thread.rs CaptureStreamData`; small.
- `format_utils::parse_format()` + `AudioInfoRaw::parse()` (`format_utils.rs:13-34`) — safe POD format parsing vs manual extraction. *Relevance:* `param_changed`; small.
- `drained` callback (`stream/mod.rs:418`) — graceful EOF for record-to-EOF. *Relevance:* `CapturingStream` flush semantics; small.
- `MainLoopRc::downgrade()`/`upgrade()` weak refs in callbacks (`pw-mon.rs:62-75`). *Relevance:* `thread.rs` listener user_data; small.
- `AttachedReceiver::deattach()` (`channel.rs:119-138`) — pause/resume listening without recreation. *Relevance:* start/stop caching; medium.
- INFORMATIONAL: `io_changed`/`control_info`/`Stream::time()` (`stream/mod.rs:54-140,382-413`) — buffer/latency/position metadata; useful for timestamping but currently no rsac consumer.

### obs-pipewire-audio-capture (C; vendored at HEAD, no canonical upstream)

**Upstream delta:** No version churn — uses stable PipeWire 0.3.x registry/metadata/`spa_io_position` APIs. No breaking changes vs rsac's `pipewire-rs 0.9.2`; techniques adoptable without a version bump.

**NEW unadopted techniques (capture-relevant):**
- Registry-driven node discovery + virtual null-sink + dynamic link management for multi-app capture (`pipewire-audio-capture-app.c:705-810,504-535`) — unblocks `ApplicationByName`/`ProcessTree` multi-app. *Relevance:* large; foundational for future multi-source.
- Metadata listener for default-sink changes + virtual-sink recreation (`:659-701`). *Relevance:* device hot-swap for `SystemDefault`; medium.
- Case-insensitive multi-field app matching (binary, app_name, node_name, client metadata) with invert/"except" mode (`:285-310,161-180`) — reduces "app not found" when binary lives only on the client. *Relevance:* `ApplicationByName`; small.
- Explicit `PW_KEY_AUDIO_CHANNEL` port→channel mapping (`:326-375`) — correct FL/FR/LFE alignment for multi-app linking. *Relevance:* future multi-app fidelity; small.
- `spa_io_position.clock.rate_diff` timestamp calc (`pipewire-audio.c:204-215`) + `on_io_changed` to obtain `spa_io_position` (`:249-258`) — rsac currently leaves Linux `AudioBuffer.timestamp = None`. *Relevance:* timestamping foundation; small. **NOTE: rsac's `param_changed` exists but `on_io_changed` is missing — prerequisite for any timestamping.**
- Deferred per-port registration + auto-link on new port (`:228-276,756-760`) — handles apps that add/remove ports (Zoom call start/end). *Relevance:* multi-app reliability; medium.
- Pre-disconnect stream-state validation (`pipewire-audio-capture-device.c:67-90`) — idempotent teardown. *Relevance:* `LinuxPlatformStream::stop_capture`; small.
- Format preference list (F32P, U8, S16, S32, ...) (`pipewire-audio.c:293-310`) — fallback ordering vs rsac's single F32LE request. *Relevance:* small; lower value since rsac converts internally.
- INFORMATIONAL: proxy-list RAII bookkeeping (`pipewire-audio.h:128-170`) — Rust ownership already covers this.

### AudioCap (Swift; CoreAudio Process Tap reference)

**Upstream delta:** No breaking changes — stable CoreAudio Process Tap APIs (`CATapDescription`, `AudioHardware{Create,Destroy}ProcessTap`, `AudioDeviceCreateIOProcIDWithBlock`) since macOS 14.4. `respondsToSelector` guards (`setPrivateTap:`) align with rsac's macOS 26+ forward-compat approach.

**NEW unadopted techniques (capture-relevant):**
- Private TCC SPI preflight/request via `dlopen`+`dlsym` (`TCCAccessPreflight`/`TCCAccessRequest`) (`AudioRecordingPermission.swift:77-124`) — proactive permission check/request; rsac hardcodes `NotDetermined` (`introspection.rs:312`). *Relevance:* macOS first-run UX; medium. (PR-4.)
- `kAudioProcessPropertyIsRunning` audio-activity per process (`AudioProcessController.swift:125`, `CoreAudioUtils.swift:86-88`) — filter to apps actually producing audio. *Relevance:* `enumerate_audio_applications`; small.
- `libproc` `proc_name`/`proc_pidpath` (`:198-217`) — PID→path for daemon/background processes invisible to `NSWorkspace`/sysinfo. *Relevance:* enumeration reliability; small.
- `NSWorkspace.publisher(for:\.runningApplications)` reactive process tracking (`:69-76`) — detect target exit/restart. *Relevance:* `ProcessTree` re-enumeration; medium.
- Explicit tap lifecycle state tracking prepare→run→invalidate with guards (`ProcessTap.swift:38-159`) — rsac's tap `Drop` assumes valid state. *Relevance:* macOS robustness; small.
- Bundle vs CLI classification via `URL.contentType` + bounded `parentBundleURL` walk (`:226-243`). *Relevance:* enrich `ApplicationInfo` (`kind`/`bundle_url`); small.
- OSStatus codes preserved in user-facing strings (`ProcessTap.swift:99,138,155`) — rsac maps to `AudioError` but drops the raw code from user messages. *Relevance:* `BackendContext`/error UX; small.

### audio-rec (C++/ObjC; CoreAudio Process Tap recorder)

**Upstream delta:** Vendored at fixed commit, no active upstream; stable macOS 14.4+ CoreAudio APIs (`kAudioSubTapDriftCompensationKey` etc.).

**NEW unadopted techniques (capture-relevant):**
- `kAudioSubTapDriftCompensationKey: true` in the aggregate-device tap dict (`audiorec.mm:231-236`) — prevents sample-rate drift on long/synchronized recordings; rsac builds the dict but doesn't set drift compensation. *Relevance:* `tap.rs build_aggregate_device_dict`; small. (Drift *compensation at the OS tap layer* is capture-config, not rsac-side resampling — in scope.)
- Per-stream format enumeration via `kAudioDevicePropertyStreams` + `kAudioStreamPropertyVirtualFormat` by direction (`main.mm:35-79`) — rsac returns `vec![default]`. *Relevance:* `coreaudio.rs supported_formats`; medium.
- `kAudioHardwarePropertyProcessObjectList` + `proc_pidinfo` + `NSRunningApplication` enumeration, dual `name→pid`/`pid→name` maps (`audiorec.mm:38-147`) — audio-active filtering without subprocesses. *Relevance:* `enumerate_audio_applications`; medium.
- INFORMATIONAL / no-change-recommended (input concurs): push IOProc callback model (`audiorec.h:8-15`), upfront child-PID aggregation (`:257`), format caching (`main.mm:157-165`) — rsac's pull-based rtrb bridge + sysinfo child discovery is the safer, idiomatic choice.

### screencapturekit-rs (`v0.2` → submodule now `v6.1.0`) — MAJOR jump

**Upstream delta (v0.2 → v6.1.0):** Pure Swift FFI (no ObjC bridge), per-stream heap `StreamContext` callback routing. New: microphone device selection (macOS 15.0+), HDR modes (15.0+), executor-agnostic async API, per-output `DispatchQueue` QoS config, stream naming, `excludes_current_process_audio`, batched `frame_info()` FFI. Dependency bumps `apple-cf 0.6→0.10`, `apple-metal 0.6→0.9` (now macOS 13+). **rsac does NOT depend on this crate** — it's a reference for a potential macOS 13+ fallback backend.

**NEW unadopted techniques (capture-relevant, mostly INFORMATIONAL given rsac's Process-Tap scope):**
- Version-gated Cargo features `macos_13_0..macos_26_0` (`Cargo.toml`) — pattern for honest sub-14.4 capability gating. *Relevance:* `PlatformCapabilities` macOS reporting; medium. (PR-2.)
- `catch_unwind` panic-safety at the FFI/C ABI boundary + RwLock poison recovery (`sc_stream.rs:145-150`, `panic_safe.rs`) — prevents one callback panic from breaking the stream. *Relevance:* rsac's ring producer has no panic guard at the OS callback edge; small. **Genuinely applicable RT-safety hardening.**
- Arc-style FFI refcount with explicit Acquire/Release fences (`sc_stream.rs:51-102`) — for Rust objects exposed to long-lived C/Swift callbacks. *Relevance:* small; only if rsac adds trampolines.
- Enum-validated `AudioSampleRate`/`AudioChannelCount` constructors (`audio.rs:42-148`) — config-time validation. *Relevance:* `StreamConfig`; small (cf. EF-4).
- `data()` vs `data_mut()` aliasing documentation (`cm/audio.rs:55-92`) — memory-safety doc pattern. *Relevance:* `AudioBuffer` docs; small.
- INFORMATIONAL: per-output QoS dispatch (`sc_stream.rs:549-555`), microphone/system separation (`audio.rs:309-450`), async completion handlers (`async_api.rs`), batched `frame_info()` — out of proportion for capture-only Process Tap; **SCK handlers run on arbitrary DispatchQueue threads, NOT RT** — any SCK backend would need a dedicated thread + rtrb bridge and must document no-RT-guarantee.

### rtrb (`v0.3.4` upstream; rsac pins `rtrb = "0.3.3"`)

**Upstream delta (0.3.3 → 0.3.4):** Purely additive/UX, **no breaking changes, no new functions of substance**: added `#[must_use]` to `push_partial_slice`/`pop_partial_slice`/`pop_partial_slice_uninit` (`chunks.rs:280,404,491`); added `ReadChunkIntoIter::iterated()` to track partial consumption (`chunks.rs:1037-1062`); doc improvements. Low-risk bump.

**NEW unadopted techniques (capture-relevant):**
- Zero-copy `write_chunk_uninit()` + `CopyToUninit` (`lib.rs:788-813`, `chunks.rs:702-721`) — write f32 directly into `MaybeUninit` slots, eliminating the `Vec` intermediate in `BridgeProducer::push_samples_or_drop` (`ring_buffer.rs:288-344`). **Strengthens ADR-0001's RT-allocation guarantee** by removing the Vec entirely. *Relevance:* high-value, small.
- `push_partial_slice()` returning `(pushed, remainder)` (`chunks.rs:306-338`) — synchronous overflow detection in the callback vs post-hoc `buffers_dropped`. *Relevance:* RT overflow metrics; small.
- `ReadChunkIntoIter::iterated()` (`chunks.rs:1037-1062`, new in 0.3.4) — batch-consume N buffers, commit only those actually consumed. *Relevance:* future adaptive-batch consumer; medium.
- `CachePadded<AtomicUsize>` head/tail (`cache_padded.rs`) — rsac's `BridgeShared` atomics (`ring_buffer.rs:90-134`) are NOT cache-padded; false-sharing tail-latency risk on 32+ core systems. *Relevance:* p99 jitter on Xeon/EPYC; medium.
- INFORMATIONAL: position-collapse arithmetic (`lib.rs:170-215`) — only if rsac ever forks a custom raw-f32 ring.

### cpal (`v0.18.0`; rsac does not depend on cpal — architectural reference)

**Upstream delta:** Unified `Error{kind, message}` model with `ErrorKind::{RealtimeDenied, HostUnavailable, Xrun, DeviceChanged}`; structured `DeviceDescription`/`DeviceType`/`InterfaceType`/`DeviceDirection`; `SupportedStreamConfigRange::cmp_default_heuristics()`; serializable `DeviceId` (`Display`/`FromStr`) + `device_by_id()`; `COMMON_SAMPLE_RATES` (29 rates). rsac should track these for parity in all three backends.

**NEW unadopted techniques (capture-relevant):**
- `DefaultDeviceMonitor` / `IMMNotificationClient` (`wasapi/stream.rs:44-190`) — fires `DeviceChanged`; callbacks set `Arc<AtomicBool>`, never call user callbacks directly (deadlock-safe). *Relevance:* `DeviceEnumerator::watch()` Windows arm (DL-1); medium.
- `AudioObjectPropertyListener` RAII wrapper with closure callbacks + auto-deregister on drop (`coreaudio/macos/property_listener.rs:1-90`). *Relevance:* DL-1 macOS arm; medium.
- `cmp_default_heuristics()` (stereo>mono>channel-count>format-rank) (`lib.rs:700+`) — formal, testable default selection vs rsac's private `pick_supported_format`. *Relevance:* `EF-4 closest_format`; small (note: rsac's `AudioFormat` lacks `Ord` — reimplement, do not "reuse").
- `COMMON_SAMPLE_RATES` (`lib.rs:~4980`) — rsac hides a private 6-rate const in `build()` (`api.rs:157`). *Relevance:* EF-4 public list; small.
- `DeviceDescription` structured metadata (`device_description.rs:16-379`) — rsac's `AudioDevice` exposes only `id`/`name`. *Relevance:* EF-2 `kind()`/`DeviceInfo`; medium.
- Compile-time `assert_stream_send!`/`assert_stream_sync!` (`wasapi/stream.rs:243-244`) — rsac claims `CapturingStream: Send` but doesn't statically assert it. *Relevance:* small, high-confidence safety net.
- `ResultExt::context()` error-context chaining (`error.rs:209-222`) and `RealtimeDenied`/`PermissionDenied`/`HostUnavailable` taxonomy (`error.rs:10-86`). *Relevance:* OBS-4 user-message mapping; small.
- `RateListener` async sample-rate confirmation (`coreaudio/macos/device.rs:161-200`). *Relevance:* macOS format-change sync; medium.

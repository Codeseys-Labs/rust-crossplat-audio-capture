## Application-Specific Audio Capture: Cross-Platform Techniques (Research Notes)

This document summarizes concrete approaches and APIs for capturing audio from a specific application on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio, macOS 14.4+). It is based on code dives into:

- Windows: HEnquist/wasapi-rs (examples/record_application.rs)
- Linux: tsowell/wiremix (TUI mixer demonstrating PipeWire targeting/monitor)
- macOS: insidegui/AudioCap (CoreAudio Process Tap with Aggregate Device)

The goal is to inform our design and implementation for per-app capture in this library.

---

### Windows (WASAPI — Process Loopback per PID)

Source references (external repo):
- Repo: https://github.com/HEnquist/wasapi-rs
- Example: examples/record_application.rs (lines 1-120)
- Core API: src/api.rs (AudioClient::new_application_loopback_client, lines 596-680)

Approach summary:
- Use the Process Loopback virtual audio interface that binds an IAudioClient to a target process (or its process tree) by PID.
- Capture in shared/event mode via IAudioCaptureClient.
- This is a Windows 10+ feature that creates a virtual audio endpoint specifically for capturing a target process's audio output.

#### Key APIs and Data Structures:

**Core Windows APIs:**
```cpp
// From Windows SDK
HRESULT ActivateAudioInterfaceAsync(
    LPCWSTR deviceInterfacePath,        // VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK
    REFIID riid,                        // IAudioClient::IID
    PROPVARIANT *activationParams,     // Contains AUDIOCLIENT_ACTIVATION_PARAMS
    IActivateAudioInterfaceCompletionHandler *completionHandler,
    IActivateAudioInterfaceAsyncOperation **activationOperation
);
```

**Critical Data Structures:**
```cpp
typedef struct AUDIOCLIENT_ACTIVATION_PARAMS {
    AUDIOCLIENT_ACTIVATION_TYPE ActivationType;  // AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
    union {
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS ProcessLoopbackParams;
    } Anonymous;
} AUDIOCLIENT_ACTIVATION_PARAMS;

typedef struct AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
    DWORD TargetProcessId;              // PID of target process
    PROCESS_LOOPBACK_MODE ProcessLoopbackMode;  // INCLUDE_TARGET_PROCESS_TREE or EXCLUDE_TARGET_PROCESS_TREE
} AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS;
```

**Stream Flags:**
- `AUDCLNT_STREAMFLAGS_EVENTCALLBACK`: Use event-driven timing
- `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`: Allow automatic format conversion
- `AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY`: Use default sample rate conversion quality

#### Detailed Implementation Flow:

1. **Process Discovery and PID Selection:**
   ```rust
   // From wasapi-rs example (record_application.rs:78-85)
   let system = System::new_with_specifics(refreshes);
   let process_ids = system.processes_by_name(OsStr::new("firefox.exe"));
   let mut process_id = 0;
   for process in process_ids {
       // Use parent PID to capture entire process tree
       process_id = process.parent().unwrap_or(process.pid()).as_u32();
   }
   ```

2. **COM Initialization:**
   ```rust
   initialize_mta().ok().unwrap(); // CoInitializeEx(None, COINIT_MULTITHREADED)
   ```

3. **Process Loopback Client Creation:**
   ```rust
   // From wasapi-rs (api.rs:596-680)
   let mut audio_client_activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
       ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
       Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
           ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
               TargetProcessId: process_id,
               ProcessLoopbackMode: if include_tree {
                   PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
               } else {
                   PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
               },
           },
       },
   };
   ```

4. **Async Activation with Completion Handler:**
   ```rust
   // Wrap activation params in PROPVARIANT with VT_BLOB
   let raw_prop = PROPVARIANT {
       Anonymous: PROPVARIANT_0 {
           Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
               vt: VT_BLOB,
               // ... blob data pointing to activation_params
           }),
       },
   };

   let operation = ActivateAudioInterfaceAsync(
       VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
       &IAudioClient::IID,
       activation_params,
       &callback,
   )?;
   ```

5. **Stream Initialization and Format:**
   ```rust
   let desired_format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
   let mode = StreamMode::EventsShared {
       autoconvert: true,
       buffer_duration_hns: 0,  // Let system decide
   };
   audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;
   ```

6. **Event-Driven Capture Loop:**
   ```rust
   let h_event = audio_client.set_get_eventhandle().unwrap();
   let capture_client = audio_client.get_audiocaptureclient().unwrap();

   audio_client.start_stream().unwrap();
   loop {
       let new_frames = capture_client.get_next_packet_size()?.unwrap_or(0);
       if new_frames > 0 {
           capture_client.read_from_device_to_deque(&mut sample_queue).unwrap();
       }
       if h_event.wait_for_event(3000).is_err() {
           break; // Timeout or error
       }
   }
   ```

#### Caveats and Limitations:

**Non-Functional Methods in Process Loopback Mode:**
- `get_mixformat()`: Returns "Not implemented"
- `is_supported()`: Returns "Not implemented" even for working formats
- `get_buffer_size()`: Returns huge values like 3131961357
- `get_current_padding()`: Returns "Not implemented"
- `get_audiorenderclient()`: Returns "No such interface supported"
- `get_audiosessioncontrol()`: Returns "No such interface supported"
- `get_audioclock()`: Returns "No such interface supported"

**Buffering Strategy:**
```rust
// From record_application.rs: Use VecDeque for resilient buffering
let mut sample_queue: VecDeque<u8> = VecDeque::new();
// Don't rely on get_buffer_size(); use dynamic allocation
let additional = (new_frames as usize * blockalign as usize)
    .saturating_sub(sample_queue.capacity() - sample_queue.len());
sample_queue.reserve(additional);
```

**Error Handling Patterns:**
- Always check HRESULT return values
- Use timeout on event waits (3000ms typical)
- Handle device disconnection gracefully
- Validate process existence before creating loopback

#### Implementation notes for our library:
- Provide an API like `capture_application_by_pid(pid: u32, include_tree: bool)` on Windows.
- Internally, activate with AUDIOCLIENT_ACTIVATION_PARAMS and run event-driven capture.
- Offer a safe default format (Float32/48kHz/stereo) with automatic conversion.
- Implement robust buffering that doesn't depend on unreliable buffer size queries.
- Provide clear error messages for common failure modes (process not found, permission denied, etc.).

---

### Linux (PipeWire — Monitor a specific node/stream)

Source references (external repo):
- Repo: https://github.com/tsowell/wiremix
- Capture/monitor stream: src/wirehose/stream.rs (capture_node, lines 25-150)
- Targeting/metadata and routing: src/view.rs (lines 384-403, 636-652), src/wirehose/execute.rs
- Property definitions: src/wirehose/property_store.rs (lines 318-343)

Approach summary:
- Each application typically exposes one or more PipeWire nodes (e.g., sink_input for playback, source_output for capture). To capture an app, create a PipeWire Stream that targets the node and runs in monitor mode.
- The stream receives raw PCM buffers that can be analyzed or recorded.
- PipeWire's graph-based architecture allows non-invasive monitoring without affecting the original audio routing.

#### Key PipeWire Properties and APIs:

**Stream Properties (from pipewire::keys):**
```rust
// Core targeting properties
*pipewire::keys::TARGET_OBJECT => String::from(serial),  // Target node's object serial
*pipewire::keys::STREAM_MONITOR => "true",               // Non-invasive monitor mode
*pipewire::keys::STREAM_CAPTURE_SINK => "true",          // Monitor sink output (optional)
*pipewire::keys::NODE_NAME => "app-capture-monitor",     // Descriptive name for our stream
```

**Stream Creation and Connection:**
```rust
// From wiremix stream.rs (lines 25-40)
let stream = Stream::new(core, "app-capture-monitor", props).ok()?;
let stream = Rc::new(stream);

// Connect with specific flags
stream.connect(
    libspa::utils::Direction::Input,
    None,
    pipewire::stream::StreamFlags::AUTOCONNECT |
    pipewire::stream::StreamFlags::MAP_BUFFERS,
    &mut params,
).ok()?;
```

**Format Negotiation (SPA Parameters):**
```rust
// From wiremix stream.rs (lines 130-150)
let mut audio_info = AudioInfoRaw::new();
audio_info.set_format(AudioFormat::F32LE);  // Prefer Float32 Little Endian

let pod_object = Object {
    type_: pipewire::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
    id: ParamType::EnumFormat.as_raw(),
    properties: audio_info.into(),
};

// Serialize to Pod for PipeWire
let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
    std::io::Cursor::new(Vec::new()),
    &pipewire::spa::pod::Value::Object(pod_object),
).ok()?.0.into_inner();

let mut params = [Pod::from_bytes(&values)?];
```

#### Detailed Implementation Flow:

1. **Node Discovery and Application Mapping:**
   ```rust
   // From wiremix property_store.rs - key properties for app identification
   application_name: String = "application.name",
   application_process_id: i32 = "application.process.id",
   application_process_binary: String = "application.process.binary",
   client_name: String = "client.name",
   media_class: String = "media.class",  // "Stream/Output/Audio" for playback
   node_name: String = "node.name",
   ```

2. **Stream Listener Setup with User Data:**
   ```rust
   // From wiremix stream.rs (lines 35-50)
   #[derive(Default)]
   pub struct StreamData {
       format: AudioInfoRaw,
       cursor_move: bool,
   }

   let listener = stream
       .add_local_listener_with_user_data(data)
       .param_changed({
           let sender_weak = Rc::downgrade(sender);
           move |_stream, user_data, id, param| {
               // Handle format negotiation
               if id != ParamType::Format.as_raw() { return; }
               let _ = user_data.format.parse(param);
           }
       })
   ```

3. **Process Callback for Audio Data:**
   ```rust
   // From wiremix stream.rs (lines 80-120)
   .process({
       let sender_weak = Rc::downgrade(sender);
       move |stream, user_data| {
           let Some(mut buffer) = stream.dequeue_buffer() else { return; };
           let datas = buffer.datas_mut();
           if datas.is_empty() { return; }

           let data = &mut datas[0];
           let n_channels = user_data.format.channels();
           let n_samples = data.chunk().size() / (mem::size_of::<f32>() as u32);

           if let Some(samples) = data.data() {
               // Process interleaved f32 samples
               for c in 0..n_channels {
                   for n in (c..n_samples).step_by(n_channels as usize) {
                       let start = n as usize * mem::size_of::<f32>();
                       let end = start + mem::size_of::<f32>();
                       let chan = &samples[start..end];
                       let f = f32::from_le_bytes(chan.try_into().unwrap_or([0; 4]));
                       // Process sample 'f' here
                   }
               }
           }
       }
   })
   ```

4. **Metadata-Based Targeting (Advanced):**
   ```rust
   // From wiremix view.rs (lines 636-652) - setting target via metadata
   fn set_target(&self, node_id: ObjectId, target: Target) {
       match target {
           Target::Node(target_id) => {
               self.wirehose.metadata_set_property(
                   metadata_id,
                   node_id.into(),
                   String::from("target.node"),
                   Some(String::from("Spa:Id")),
                   Some(target_id.to_string()),
               );
           }
           // ... other target types
       }
   }
   ```

#### Node Discovery Strategies:

**By Application Properties:**
```rust
// Look for nodes with specific application properties
fn find_nodes_by_app_name(state: &State, app_name: &str) -> Vec<ObjectId> {
    state.nodes.values()
        .filter(|node| {
            node.props.application_name()
                .map_or(false, |name| name.contains(app_name))
        })
        .map(|node| node.object_id)
        .collect()
}

// By process ID
fn find_nodes_by_pid(state: &State, pid: i32) -> Vec<ObjectId> {
    state.nodes.values()
        .filter(|node| {
            node.props.application_process_id() == Some(&pid)
        })
        .map(|node| node.object_id)
        .collect()
}
```

**Media Class Filtering:**
```rust
// From wiremix media_class.rs - identify stream types
pub fn is_sink_input(media_class: &str) -> bool {
    media_class == "Stream/Output/Audio"  // App playback streams
}

pub fn is_source_output(media_class: &str) -> bool {
    media_class == "Stream/Input/Audio"   // App recording streams
}
```

#### Caveats and Constraints:

**PipeWire vs PulseAudio:**
- Requires PipeWire daemon running (not just PulseAudio compatibility layer)
- Session manager (WirePlumber) controls node visibility and permissions
- Flatpak/Snap apps may have restricted node visibility through portals

**Node Lifecycle Management:**
- Nodes appear/disappear as apps start/stop audio
- Monitor for node state changes via registry listeners
- Handle graceful disconnection when target node disappears

**Format Negotiation:**
- Apps may use different sample rates (44.1kHz, 48kHz, 96kHz)
- Channel layouts vary (mono, stereo, 5.1, etc.)
- Bit depths: typically 16-bit int or 32-bit float

#### Implementation notes for our library:
- Add a PipeWire backend function `capture_application_by_node(node_id/serial)` and a helper to resolve node by app info (name, PID).
- Use monitor streams to tap audio (non-invasive). Provide PCM frames via our unified callback/streaming API.
- Prefer Float32 interleaved; expose negotiated format to callers.
- Implement robust node discovery with fallback strategies (by name, PID, binary path).
- Handle dynamic node appearance/disappearance gracefully.
- Provide clear error messages for missing PipeWire, permission issues, or node not found.

---

### macOS (CoreAudio, macOS 14.4+ — Process Tap + Aggregate Device)

Source references (external repo):
- Repo: https://github.com/insidegui/AudioCap
- Core files: AudioCap/AudioCap/ProcessTap/ProcessTap.swift (lines 89-140), CoreAudioUtils.swift (lines 27-178)
- README: detailed step-by-step with links to Apple docs (lines 25-40)

Approach summary:
- New CoreAudio API allows tapping a process’ audio via a Process Tap and exposing it through an Aggregate Device that includes the tap as a sub-tap. An I/O proc pulls frames which can be written to an audio file.
- This approach creates a private aggregate device that combines the system output with a tap on the target process, allowing non-invasive capture.

#### Key CoreAudio APIs and Constants:

**Process Discovery and Translation:**
```swift
// From CoreAudioUtils.swift (lines 32-36, 62-67)
static func translatePIDToProcessObjectID(pid: pid_t) throws -> AudioObjectID {
    try AudioObjectID.system.translatePIDToProcessObjectID(pid: pid)
}

func translatePIDToProcessObjectID(pid: pid_t) throws -> AudioObjectID {
    try read(
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        defaultValue: AudioObjectID.unknown,
        qualifier: pid
    )
}
```

**Process Tap Creation:**
```swift
// From ProcessTap.swift (lines 95-105)
let tapDescription = CATapDescription(stereoMixdownOfProcesses: [objectID])
tapDescription.uuid = UUID()
tapDescription.muteBehavior = muteWhenRunning ? .mutedWhenTapped : .unmuted

var tapID: AUAudioObjectID = .unknown
var err = AudioHardwareCreateProcessTap(tapDescription, &tapID)
guard err == noErr else {
    throw "Process tap creation failed with error \(err)"
}
```

**Aggregate Device Dictionary Structure:**
```swift
// From ProcessTap.swift (lines 114-132)
let description: [String: Any] = [
    kAudioAggregateDeviceNameKey: "Tap-\(process.id)",
    kAudioAggregateDeviceUIDKey: aggregateUID,
    kAudioAggregateDeviceMainSubDeviceKey: outputUID,
    kAudioAggregateDeviceIsPrivateKey: true,
    kAudioAggregateDeviceIsStackedKey: false,
    kAudioAggregateDeviceTapAutoStartKey: true,
    kAudioAggregateDeviceSubDeviceListKey: [
        [
            kAudioSubDeviceUIDKey: outputUID
        ]
    ],
    kAudioAggregateDeviceTapListKey: [
        [
            kAudioSubTapDriftCompensationKey: true,
            kAudioSubTapUIDKey: tapDescription.uuid.uuidString
        ]
    ]
]
```

Key APIs/keys (legacy summary):
- Permission: NSAudioCaptureUsageDescription in Info.plist (prompt on first use; no public preflight API). Project optionally uses private TCC APIs for preflight.
- kAudioHardwarePropertyTranslatePIDToProcessObject (PID -> AudioObjectID)
- CATapDescription(stereoMixdownOfProcesses: [AudioObjectID]) with a uuid
- AudioHardwareCreateProcessTap(tapDescription, &tapID)
- Aggregate Device CFDictionary keys:
  - kAudioAggregateDeviceIsPrivateKey: true
  - kAudioAggregateDeviceMainSubDeviceKey: (UID of default system output)
  - kAudioAggregateDeviceTapListKey: [ { kAudioSubTapUIDKey: tapDescription.uuid, kAudioSubTapDriftCompensationKey: true } ]
  - (Optionally) kAudioAggregateDeviceTapAutoStartKey: true
- kAudioTapPropertyFormat to read AudioStreamBasicDescription from the tap
- AudioDeviceCreateIOProcIDWithBlock(aggregateDeviceID, queue, ioBlock)
- AudioDeviceStart/Stop; AudioHardwareDestroyAggregateDevice; AudioHardwareDestroyProcessTap

#### Detailed Implementation Flow:

1. **Permission and OS Version Check:**
   ```swift
   // Info.plist requirement
   <key>NSAudioCaptureUsageDescription</key>
   <string>This app needs to capture audio from other applications for recording purposes.</string>

   // Runtime version check
   if #available(macOS 14.4, *) {
       // Process tap APIs available
   } else {
       throw "Process tap requires macOS 14.4 or later"
   }
   ```

2. **Process Object ID Resolution:**
   ```swift
   // From CoreAudioUtils.swift (lines 83-87)
   func readProcessList() throws -> [AudioObjectID] {
       let objectIdentifiers = try AudioObjectID.readProcessList()
       return objectIdentifiers.filter { objectID in
           (try? objectID.isRunning) == true
       }
   }

   // Get specific process
   let objectID = try AudioObjectID.translatePIDToProcessObjectID(pid: targetPID)
   ```

3. **System Output Device Discovery:**
   ```swift
   // From CoreAudioUtils.swift (lines 104-108)
   func readDefaultSystemOutputDevice() throws -> AudioDeviceID {
       return try read(
           kAudioHardwarePropertyDefaultSystemOutputDevice,
           defaultValue: AudioDeviceID.unknown
       )
   }

   let systemOutputID = try AudioDeviceID.readDefaultSystemOutputDevice()
   let outputUID = try systemOutputID.readDeviceUID()
   ```

4. **Tap Stream Format Reading:**
   ```swift
   // From CoreAudioUtils.swift (lines 114-116)
   func readAudioTapStreamBasicDescription() throws -> AudioStreamBasicDescription {
       try read(kAudioTapPropertyFormat, defaultValue: AudioStreamBasicDescription())
   }

   self.tapStreamDescription = try tapID.readAudioTapStreamBasicDescription()
   ```

5. **Aggregate Device Creation:**
   ```swift
   // From ProcessTap.swift (lines 135-140)
   aggregateDeviceID = AudioObjectID.unknown
   err = AudioHardwareCreateAggregateDevice(description as CFDictionary, &aggregateDeviceID)
   guard err == noErr else {
       throw "Failed to create aggregate device: \(err)"
   }
   ```

6. **I/O Proc Setup and Audio Processing:**
   ```swift
   // From ProcessTap.swift - run method
   func run(on queue: DispatchQueue, ioBlock: @escaping AudioDeviceIOBlock, invalidationHandler: @escaping InvalidationHandler) throws {
       var err = AudioDeviceCreateIOProcIDWithBlock(&deviceProcID, aggregateDeviceID, queue, ioBlock)
       guard err == noErr else { throw "Failed to create device I/O proc: \(err)" }

       err = AudioDeviceStart(aggregateDeviceID, deviceProcID)
       guard err == noErr else { throw "Failed to start audio device: \(err)" }
   }

   // Typical I/O block implementation
   let ioBlock: AudioDeviceIOBlock = { (inNow, inInputData, inInputTime, outOutputData, inOutputTime) in
       // Process AudioBufferList from inInputData
       // Convert to AVAudioPCMBuffer and write to file or forward to callback
       return noErr
   }
   ```

Minimal flow (conceptual - legacy):
1) Ensure macOS 14.4+; ensure NSAudioCaptureUsageDescription in Info.plist.
2) Get PID and translate to process AudioObjectID via kAudioHardwarePropertyTranslatePIDToProcessObject.
3) Create CATapDescription (set uuid); create Process Tap (tapID) with AudioHardwareCreateProcessTap.
4) Build Aggregate Device dictionary including the tap (kAudioAggregateDeviceTapListKey) and the system output subdevice.
5) Create Aggregate Device; read kAudioTapPropertyFormat from tap; construct AVAudioFormat.
6) Create AVAudioFile for writing (or provide frames upstream via our callbacks).
7) Create IOProc with block, start device; in callback, wrap buffers (AudioBufferList -> AVAudioPCMBuffer) and write/forward.
8) Stop and destroy IOProc, destroy aggregate device, destroy tap.

#### Advanced Features and Configuration:

**Mute Behavior Control:**
```swift
// From CATapDescription
enum MuteBehavior {
    case unmuted           // Don't affect original audio
    case mutedWhenTapped   // Mute original when tap is active
}
tapDescription.muteBehavior = .mutedWhenTapped
```

**Drift Compensation:**
```swift
// In aggregate device tap list
kAudioSubTapDriftCompensationKey: true  // Compensate for timing differences
```

**Auto-Start Configuration:**
```swift
kAudioAggregateDeviceTapAutoStartKey: true  // Start tap automatically with device
```

#### Error Handling and Cleanup Patterns:

**Proper Cleanup Sequence:**
```swift
// From ProcessTap.swift invalidate() method
func invalidate() {
    // 1. Stop device
    if aggregateDeviceID.isValid {
        var err = AudioDeviceStop(aggregateDeviceID, deviceProcID)

        // 2. Destroy I/O proc
        if let deviceProcID {
            err = AudioDeviceDestroyIOProcID(aggregateDeviceID, deviceProcID)
            self.deviceProcID = nil
        }

        // 3. Destroy aggregate device
        err = AudioHardwareDestroyAggregateDevice(aggregateDeviceID)
        aggregateDeviceID = .unknown
    }

    // 4. Destroy process tap
    if processTapID.isValid {
        let err = AudioHardwareDestroyProcessTap(processTapID)
        self.processTapID = .unknown
    }
}
```

**Common Error Codes:**
- `kAudioHardwareNotRunningError`: CoreAudio not available
- `kAudioHardwareBadObjectError`: Invalid AudioObjectID
- `kAudioHardwareIllegalOperationError`: Operation not permitted
- `kAudioDeviceUnsupportedFormatError`: Format not supported

Caveats/constraints:
- Requires macOS 14.4+, poorly documented; some behaviors are intricate (mute while tapped, drift compensation, etc.).
- Permission prompt cannot be programmatically preflighted using public APIs.
- Aggregate device approach means your app effectively “owns” a private device to pull audio from the tap.

Implementation notes for our library:
- Provide a macOS backend function capture_application_by_pid(pid: pid_t, mute_when_running: bool).
- Gate with runtime and compile-time checks (>= macOS 14.4). Provide graceful errors on older OS versions.
- Prefer a callback-based API; optionally provide convenience writer to file via AVFoundation.
- Link to docs/macos_application_capture.md for user-facing guidance.

---

### Cross-Platform Integration Plan (Library)

Abstractions:
- Extend our AudioCaptureBackend trait (or equivalent) with an optional app-capture capability (feature-gated by OS):
  - Windows: capture_application_by_pid(pid: u32, include_tree: bool, format_hint: Option<Format>) -> Stream/Handle
  - Linux: capture_application_by_selector(selector: AppSelector) -> Stream/Handle
    - AppSelector could include: pid, app_name, node_id/serial
  - macOS (14.4+): capture_application_by_pid(pid: i32, mute_when_running: bool) -> Stream/Handle

Configuration & format negotiation:
- Default to Float32 interleaved; expose actual negotiated format to caller (sample rate, channels, layout) and resample/convert downstream if required.
- Provide non-blocking callback or async stream of PCM frames with timestamps.

Discovery utilities:
- Windows: find PID by process name (optional convenience).
- Linux: enumerate PipeWire nodes and map to app by client properties (application.*). Return node IDs/serials for selection.
- macOS: enumerate running NSRunningApplication to expose user-friendly selector, then translate to PID.

Error handling and permissions:
- Windows: surface activation errors clearly; document non-functional methods in loopback mode.
- Linux: detect missing PipeWire or insufficient permissions (e.g., portals/sandbox). Provide actionable messages.
- macOS: detect OS version; if permission denied or missing Info.plist key, fail loudly with guidance.

Testing strategy (per platform):
- Windows: CLI example capturing a known app (e.g., Notepad or browser), verify non-silent output; add CI lint/build; runtime E2E is manual or nightly.
- Linux: example to list nodes, pick one, start monitor capture; verify non-silent audio when app plays sound (can run in PipeWire-enabled container/VM).
- macOS: example app for 14.4+; manual test steps due to permission prompts; unit tests around utility functions and version checks.

Telemetry and safety:
- Clearly indicate when capturing an app vs system output; ensure user consent where applicable.
- Provide visual indicator or logging during capture (especially on macOS per platform conventions).

---

### Risks and Mitigations

- API churn / OS version dependencies:
  - macOS 14.4+ requirement; document fallback behavior (feature unavailable).
  - Windows loopback quirks; rely on known-good patterns (event callbacks, deque buffers).
  - Linux node discovery variance; implement robust filtering of PipeWire properties.

- Permissions and user expectations:
  - macOS permission prompts; document Info.plist and UX implications.
  - Linux desktop portals may affect app visibility; document requirements.

- Format/latency differences:
  - Expose negotiated formats; provide conversion options; buffer appropriately.

---

### Quick Links

- Windows (WASAPI): HEnquist/wasapi-rs — examples/record_application.rs, src/api.rs
- Linux (PipeWire): tsowell/wiremix — src/wirehose/stream.rs, src/view.rs, src/wirehose/execute.rs
- macOS (CoreAudio): insidegui/AudioCap — ProcessTap.swift, CoreAudioUtils.swift, README (step-by-step)

See also: docs/macos_application_capture.md for more platform-specific user guidance on macOS.


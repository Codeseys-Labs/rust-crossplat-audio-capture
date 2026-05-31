# rsac UX / Helper Enhancement Backlog (2026-05-29)

Prioritized, deduplicated, verified. All items respect rsac's **capture-only** scope (no mixing/resampling/encoding/playback/effects/VAD/AEC). Each item is grounded in a reference and verdict-tagged.

The input proposed several items multiple times across categories. They are merged here: `FromStr` (DL-5/EF-1/UX-4 → **H1**); `stream_stats()` (DL-4/EF-5/UX-2/OBS-1 → **H3**); RMS/peak + prelude (UX-1 → **H2**); Linux native enumeration (PR-1 → **H4**); Linux `supported_formats()` (EF-3 rejected vs PR-5 deferred → see Deferred). The richest variant of each merged item is kept.

---

## High priority

### H1 — `CaptureTarget::FromStr` + `TryFrom<&str>` + `AudioCaptureBuilder::target_str()`
- **Verdict:** adopt-with-changes
- **Gap:** `CaptureTarget` (`core/config.rs:53-66`) has typed ctors (`app`/`pid`/`device`, `introspection.rs:81-102`) but no string parser. Every CLI/config/env consumer hand-rolls the match; rsac's own `main.rs build_target` (`:458-464`) duplicates it. VISION.md:128 lists it as roadmap.
- **api_sketch:**

  ```rust
  // core/config.rs
  impl std::str::FromStr for CaptureTarget {
      type Err = crate::core::error::AudioError; // -> AudioError::InvalidParameter{param,reason} (error.rs:111)
      fn from_str(s: &str) -> Result<Self, Self::Err>;
  }
  impl TryFrom<&str> for CaptureTarget { type Error = AudioError; fn try_from(s:&str)->Result<Self,_>{ s.parse() } }
  impl std::fmt::Display for CaptureTarget { /* inverse; round-trips for persistence */ }
  // api.rs
  impl AudioCaptureBuilder { pub fn target_str(self, s:&str) -> AudioResult<Self>; } // with_target(s.parse()?)
  ```

- **Grounded in:** cpal `DeviceId` `Display`/`FromStr` round-trip (`cpal/src/lib.rs:284`, `device_description.rs`); wiremix `kind:value` mini-grammar (`config/property_key.rs:33`); obs case-insensitive multi-field matching (`pipewire-audio-capture-app.c:285-310`).
- **Effort:** small.
- **REQUIRED CHANGES (grammar reconciliation — the one real wrinkle):** rsac's existing convention is `app:<pid>` — `AudioSource.id` is formatted `app:{pid}` (`introspection.rs:179,197,250`) and `to_capture_target()` maps a discovered app to `Application(ApplicationId(pid_string))` (`introspection.rs:66-76`). The naive `app:<name>→ApplicationByName` mapping would break round-tripping of `AudioSource.id`. **Pick `app:<pid> → Application` (matches the codebase) and use a distinct prefix `name:<n> → ApplicationByName`**, so `source.id.parse()` round-trips. Map non-numeric/overflow PID → `InvalidParameter` (never panic). Split `device:` IDs on the FIRST colon only (e.g. `device:hw:0,0`). Write + test the `Display` inverse. Parsing runs only at config time — RT-irrelevant.

### H2 — `rsac::prelude` module + `AudioBuffer` level metering (RMS / peak / dBFS)
- **Verdict:** adopt-with-changes
- **Gap:** (1) No `pub mod prelude` (`lib.rs:92-138` re-exports ~15 names flat); VISION.md:129 roadmap. (2) RMS/peak helpers that power `rsac capture`'s meter live only in the binary (`main.rs:480-493`), not the library — every VU/meter consumer re-derives RMS from `buffer.data()`. `AudioBuffer` (`core/buffer.rs:150-278`) exposes `data()/channels()/duration()` but no `rms()/peak()/dbfs()`.
- **api_sketch:**

  ```rust
  // lib.rs
  pub mod prelude {
      pub use crate::{AudioCapture, AudioCaptureBuilder, CaptureTarget, AudioBuffer,
          AudioError, AudioResult, PlatformCapabilities, AudioSink, NullSink, ChannelSink};
      #[cfg(feature = "sink-wav")] pub use crate::WavFileSink;
  }
  // core/buffer.rs — impl AudioBuffer (all &self, read-only metrics over interleaved f32)
  pub fn rms(&self) -> f32;  pub fn peak(&self) -> f32;
  pub fn rms_dbfs(&self) -> f32;  pub fn peak_dbfs(&self) -> f32;     // NEG_INFINITY for silence
  pub fn channel_rms(&self, ch:u16) -> Option<f32>;  pub fn channel_peak(&self, ch:u16) -> Option<f32>;
  ```

- **Grounded in:** wiremix `find_peak` SIMD metering + `AtomicF32` update (`wirehose/stream.rs:59-86,209-234`); rsac's own CLI `rms_level` (`main.rs:480-486`).
- **Effort:** small.
- **REQUIRED CHANGES:** (a) implement `channel_rms/channel_peak` as a **strided** reduction (`skip(ch).step_by(channels)`) — do NOT reuse `channel_data()` (`buffer.rs:248-262`) which allocates via `collect()`; keep allocation-free. (b) guard `channels()==0` (div-by-zero). (c) `rms_dbfs/peak_dbfs` return `f32::NEG_INFINITY` at level 0.0; clamp/handle NaN/Inf inputs (buffers can legitimately carry them — see `buffer_with_nan_and_infinity` test, `buffer.rs:651`). (d) refactor CLI `rms_level` to call `AudioBuffer::rms()` to kill duplication. RMS/peak are read-only metrics (metadata), NOT DSP — in scope. Run on consumer threads on already-delivered buffers, never the OS callback.

### H3 — `AudioCapture::stream_stats()` + `AudioCapture::format()` (fix the dangling contract; enrich the snapshot)
- **Verdict:** adopt-with-changes
- **Gap:** `StreamStats {overruns, is_running, format_description}` is defined and publicly re-exported (`lib.rs:115`) and its doc says "Obtained via `AudioCapture::stream_stats()`" (`introspection.rs:326`) — **but that method does not exist** (`api.rs` has only piecemeal `overrun_count()/is_running()/is_under_backpressure()`). A broken-promise API. Separately there is no `AudioCapture::format()` to read the *negotiated* (not requested) format. The bridge already tracks `buffers_pushed`/`buffers_dropped`/`buffers_popped`/`consecutive_drops` as Relaxed atomics (`bridge/ring_buffer.rs:93-101`) but none of the totals/uptime/drop-ratio are reachable.
- **api_sketch:**

  ```rust
  // core/introspection.rs — enrich + make non_exhaustive (keep #[derive(Default)])
  #[non_exhaustive]
  pub struct StreamStats {
      pub overruns: u64, pub is_running: bool, pub format_description: String,
      pub buffers_captured: u64, pub buffers_dropped: u64,   // BUFFER counts (not frames)
      pub uptime: std::time::Duration, pub under_backpressure: bool,
  }
  impl StreamStats { pub fn dropped_ratio(&self) -> f64; } // dropped/(captured+dropped); pushed excludes drops
  // core/interface.rs — additive default-0 trait methods (mirror overrun_count default {0})
  trait CapturingStream { fn buffers_captured(&self)->u64{0} fn buffers_delivered(&self)->u64{0} }
  // api.rs — the missing producer
  impl AudioCapture {
      pub fn format(&self) -> AudioFormat;          // stream.format() else config requested
      pub fn stream_stats(&self) -> StreamStats;    // snapshot atomics + uptime; defaults when stream None
  }
  ```

- **Grounded in:** camilladsp `ProcessingState`/buffer-fill diagnostics surfaced in status messages (`wasapi_backend/device.rs:1272,1443-1461`); rsac-baseline dangling-API gap; pipewire-rs negotiated-format reporting (`CapturingStream::format()`, `interface.rs:144`).
- **Effort:** small.
- **REQUIRED CHANGES:** (a) name fields `buffers_*`, NOT `frames_*` — the bridge counts buffers, not frames. (b) `#[non_exhaustive]` MUST land WITH the field additions (the struct is not currently marked, so adding fields is source-breaking for external literals); keep `Default`. (c) `format()` reports the negotiated format (`interface.rs:140-144` contract). (d) `format_description` allocates a `String` only on the caller thread — never the OS callback. (e) update the `introspection.rs` doc once shipped so the contract is honored end-to-end. (f) `start_instant: Option<Instant>` set in `start()` (`api.rs:469`) drives `uptime`.

### H4 — Native in-process PipeWire registry/metadata enumeration (drop `pw-dump`/`pw-cli`/`pw-metadata`)
- **Verdict:** adopt-with-changes
- **Gap:** Linux enumeration + target resolution shell out to `pw-dump` (`introspection.rs:211`), `pw-cli`/`pw-dump`/`pw-metadata` (`audio/linux/mod.rs:65/77/94`), `find_pipewire_node_serial` (`thread.rs:538`). Fragile (regex/JSON parsing of human output), adds a runtime binary dependency (**`list_audio_applications()` silently returns empty if `pw-dump` is missing — common on headless/Flatpak**), and incurs spawn latency. The crate already depends on `pipewire = 0.9.2` and already builds a `MainLoop+Context+Core+Registry` — but the registry is bound `let _registry` (`thread.rs:887`) and never listened on.
- **api_sketch:**

  ```rust
  // audio/linux/thread.rs — additive commands, no public API change
  enum PipeWireCommand { /*…*/
      SnapshotDevices { reply: mpsc::Sender<Vec<PwDeviceSnapshot>> },
      SnapshotApplications { reply: mpsc::Sender<Vec<PwAppSnapshot>> },
      SnapshotDefault { reply: mpsc::Sender<Option<String>> },
  }
  impl PipeWireThread {
      pub(crate) fn snapshot_devices(&self) -> AudioResult<Vec<PwDeviceSnapshot>>;
      pub(crate) fn snapshot_applications(&self) -> AudioResult<Vec<PwAppSnapshot>>;
      pub(crate) fn snapshot_default(&self) -> AudioResult<Option<String>>;
  }
  // LinuxDeviceEnumerator + introspection route through these instead of std::process::Command.
  ```

- **Grounded in:** pipewire-rs `Registry::add_listener_local` (`registry/mod.rs:40,119,128`) + `Metadata::add_listener_local` (`metadata.rs:46,130`); wiremix native registry session (`wirehose/session.rs:312-451`) + `media_class` predicates; obs `on_global_cb` (`:705-810`) + `default_node_cb` (`:659-701`).
- **Effort:** large.
- **REQUIRED CHANGES:** (1) Define the discovery-thread lifecycle — enumeration spawns NO PW thread today; choose either per-call spawn (init latency) or a persistent discovery thread with explicit teardown. (2) `global` callbacks are `Fn + 'static` (not `FnMut`) — share the in-memory snapshot via `Rc<RefCell<…>>` captured into the closure (wiremix idiom). (3) A `SnapshotDevices` reply MUST wait for a `core.sync()`/`done` roundtrip so the initial registry dump is complete before replying (else it races an empty registry). (4) Keep the subprocess path behind `cfg`/fallback until native is proven on headless/Flatpak, then remove. RT-safe: callbacks fire on the PW thread during `main_loop.iterate()` (`thread.rs:918`), never the audio callback; only owned `Vec`s cross the mpsc reply.

---

## Medium priority

### M1 — `AudioDevice::kind()` + `DeviceInfo` snapshot (richer enumeration metadata)
- **Verdict:** adopt-with-changes · **api_fit: awkward**
- **Gap:** `list_audio_sources()`/`AudioSource` expose only id/name/is_default. Callers can't tell input from output/loopback before `build()`. `DeviceKind{Input,Output}` already exists (`interface.rs:17-24`).
- **api_sketch:**

  ```rust
  // core/interface.rs
  trait AudioDevice { fn kind(&self) -> AudioResult<DeviceKind>; fn describe(&self) -> DeviceInfo; }
  pub struct DeviceInfo { pub id:DeviceId, pub name:String, pub kind:DeviceKind,
                          pub is_default:bool, pub default_format:Option<AudioFormat> }
  // introspection.rs additive field: AudioSourceKind::Device { device_id, is_default, kind:Option<DeviceKind> }
  ```

- **Grounded in:** cpal `DeviceDescription`/`DeviceDirection` (`device_description.rs:16-379`); wiremix `media_class` predicates + `PropertyStore`.
- **Effort:** medium.
- **REQUIRED CHANGES:** Make `kind()` **fallible** (`-> AudioResult<DeviceKind>`) or rename — WASAPI already has an inherent fallible `kind()` doing COM `GetDataFlow` (`wasapi.rs:744-786`); the infallible sketch clashes. macOS `MacosAudioDevice` holds only `device_id` (`coreaudio.rs:127-129`) — needs real CoreAudio scope probing, `kind()` is NOT free there. `default_format` is frequently `None` (Linux `supported_formats()` returns `vec![]`). Keep `AudioSourceKind::Device{kind:Option<DeviceKind>}` additive.

### M2 — `RunningCapture` RAII guard + `AudioCaptureBuilder::start()` (one-call build-and-run)
- **Verdict:** adopt-with-changes
- **Gap:** Lifecycle is `build()→start()→…→stop()`; forgetting `start()` yields a confusing `StreamReadError` (`api.rs:620`). `Drop` stops (`api.rs:910`) but no scoped guard makes the started region explicit, and no one-call helper.
- **api_sketch:**

  ```rust
  impl AudioCaptureBuilder { pub fn start(self) -> AudioResult<RunningCapture>; } // build()? + start()?
  pub struct RunningCapture { inner: std::mem::ManuallyDrop<AudioCapture> }
  impl RunningCapture {
      pub fn stop(self) -> AudioResult<AudioCapture>;
      pub fn into_inner(self) -> AudioCapture;
  }
  impl Deref/DerefMut for RunningCapture { type Target = AudioCapture; }
  impl Drop for RunningCapture { /* ManuallyDrop::take once; inner Drop stops */ }
  ```

- **Grounded in:** AudioCap explicit tap lifecycle prepare→run→invalidate (`ProcessTap.swift:38-159`); rsac-baseline RAII-guard gap.
- **Effort:** small.
- **REQUIRED CHANGES:** Use `ManuallyDrop<AudioCapture>` so `into_inner`/`stop` can move out without double-stop (use-after-move otherwise). `AudioCapture::stop()` is idempotent and `AudioCapture::Drop` already best-effort stops (`api.rs:567,910`) — the explicit `RunningCapture::Drop` stop is therefore redundant; either drop it or keep only to log errors (don't imply two distinct cleanup layers). Document that `stop()`'s returned handle has `stream=None` and a later `start()` creates a fresh OS stream (`api.rs:469`).

### M3 — `combine_sources()` → tagged multi-source fan-in `Receiver<(SourceId, AudioBuffer)>`
- **Verdict:** adopt-with-changes
- **Gap:** Monitoring several targets requires one `AudioCapture` per source and hand-merging `subscribe()` receivers, losing origin. No helper fans multiple captures into ONE receiver with each buffer tagged. **This is fan-in, NOT mixing** — buffers stay separate.
- **api_sketch:**

  ```rust
  pub struct SourceId(pub String);
  pub struct TaggedBuffer { pub source: SourceId, pub buffer: AudioBuffer }
  pub struct CombinedReceiver { /* rx + guard threads */ }
  impl CombinedReceiver { pub fn recv(&self)->Result<TaggedBuffer,RecvError>; pub fn try_recv(&self)->…; pub fn iter(&self)->impl Iterator<Item=TaggedBuffer>+'_; }
  pub fn combine_sources<I>(sources: I) -> AudioResult<CombinedReceiver>
      where I: IntoIterator<Item=(SourceId, mpsc::Receiver<AudioBuffer>)>;
  ```

- **Grounded in:** VISION.md:24-26,71-78 (simultaneous multi-source is a core goal; mixing 2+→1 is the out-of-scope line, VISION.md:96); cpal/SCK per-stream independent handler model; rsac's proven `subscribe()` pump (`api.rs:790-834`).
- **Effort:** medium.
- **REQUIRED CHANGES:** Reconcile prose vs signature — take caller-produced receivers (cleaner; caller owns `subscribe()`). `AudioBuffer` is already `Send` (`subscribe()` sends it over mpsc, `api.rs:815`). The `CombinedReceiver` guard must drop forwarding senders to terminate per-source threads (mirror `subscribe()` teardown). Inherits `subscribe()`'s ~1ms poll latency floor and single-consumer-per-ring caveat (non-issue with one capture per source).

### M4 — `PlatformCapabilities::recommended_config()` + `closest_format()` + public `COMMON_SAMPLE_RATES`
- **Verdict:** adopt-with-changes · **api_fit: awkward**
- **Gap:** `build()` couples validation+device-resolution+negotiation (`api.rs:135-291`); callers can't ask "known-good config for this platform" or read the rate whitelist (`SUPPORTED_SAMPLE_RATES` is private at `api.rs:157`). `pick_supported_format` is private + Linux-gated (`api.rs:305-334`).
- **api_sketch:**

  ```rust
  impl PlatformCapabilities {
      pub fn recommended_config(&self) -> StreamConfig;
      pub fn closest_format(&self, supported: &[AudioFormat], wanted: &AudioFormat) -> Option<AudioFormat>;
  }
  pub const COMMON_SAMPLE_RATES: &[u32] = &[22050,32000,44100,48000,88200,96000]; // = existing 6, single source of truth
  ```

- **Grounded in:** cpal `cmp_default_heuristics()` (`lib.rs:771`) + `COMMON_SAMPLE_RATES` (`lib.rs:865`).
- **Effort:** small.
- **REQUIRED CHANGES:** Keep the existing **6-rate** whitelist (do NOT widen to 9 — that silently changes the public validation contract + error string at `api.rs:162`); widening is a separate deliberate decision. Do NOT promise cpal reuse — rsac's `AudioFormat` derives only `PartialEq/Eq/Hash` (no `Ord`); reimplement the heuristic natively. Define `recommended_config()` output on the unsupported stub (range `(0,0)`, empty formats, `max_channels 0`, `capabilities.rs:160-171`).

### M5 — `AudioError::user_message()` → `UserFacingError` with remediation hint
- **Verdict:** adopt-with-changes
- **Gap:** `AudioError` has a great developer taxonomy (`kind()/recoverability()/Display`, `error.rs:292-419`) but no user-facing layer turning an error into a short actionable sentence + "what to DO" hint. Every downstream tool re-matches 22 variants.
- **api_sketch:**

  ```rust
  pub struct UserFacingError {
      pub summary: String, pub remedy: Option<String>,
      pub recoverability: Recoverability, pub backend_code: Option<i64>, // BackendContext.os_error_code
  }
  impl AudioError { pub fn user_message(&self) -> UserFacingError; } // EXHAUSTIVE match, no `_`
  ```

- **Grounded in:** cpal `RealtimeDenied`-with-recovery messages + `ResultExt::context()` (`error.rs:10-86,209-222`); AudioCap OSStatus-in-UI strings (`ProcessTap.swift:99,138,155`).
- **Effort:** small.
- **REQUIRED CHANGES:** Use an EXHAUSTIVE match (no catch-all) mirroring `recoverability()` (`error.rs:242`) so a new variant forces a compile error. Only 4 variants carry `BackendContext` (`UnsupportedFormat/DeviceEnumerationError/StreamCreationFailed/BackendError`) → `backend_code` is `None` otherwise (consistent with `Option`). Add a test asserting every variant yields a non-empty summary (mirror `all_variants_display_is_nonempty`, `error.rs:1191`).

### M6 — Windowed drop-rate backpressure signal alongside the consecutive-drop flag
- **Verdict:** adopt-with-changes · **api_fit: awkward**
- **Gap:** Backpressure is one bool from `consecutive_drops >= threshold` (`ring_buffer.rs:140-142`). A consumer dropping 1-of-3 buffers (33% loss) never trips it because a successful push resets the streak to 0 (`ring_buffer.rs:236`). No severity, no rate.
- **api_sketch:**

  ```rust
  pub struct BackpressureReport {
      pub window: Duration, pub buffers_captured_in_window: u64, pub buffers_dropped_in_window: u64,
      pub drop_rate: f64, pub level: BackpressureLevel, pub consecutive_streak_tripped: bool,
  }
  pub enum BackpressureLevel { Healthy, Elevated, Critical }
  impl AudioCapture { pub fn backpressure_report(&self) -> BackpressureReport; } // delta since last call, interior Mutex
  ```

- **Grounded in:** cpal `ErrorKind::Xrun`; camilladsp graded stream health + periodic re-measurement over an interval.
- **Effort:** small.
- **REQUIRED CHANGES:** Name fields `buffers_*` (no frame counter exists). Computing `dropped/(pushed+dropped)` REQUIRES an additive trait accessor for the success/push count — `CapturingStream` (`interface.rs:120-201`) exposes only `overrun_count()`/`is_under_backpressure()`; `BridgeStream::buffers_read/buffers_dropped` are inherent, not on the trait, so unreachable from the handle. Route through the existing `StreamStats`/introspection surface (H3) rather than a parallel type.

### M7 — `request_audio_capture_permission()` (macOS TCC preflight/request)
- **Verdict:** adopt-with-changes
- **Gap:** `check_audio_capture_permission()` hardcodes `NotDetermined` on macOS (`introspection.rs:312`); no request fn to drive the `kTCCServiceAudioCapture` prompt before a Process Tap. First `Application`/`ProcessTree` capture either silently yields no audio or fails with an opaque OSStatus deep in `tap.rs`. The code itself names `AudioRecordingPermission.swift` as the future port (`:311`).
- **api_sketch:**

  ```rust
  pub fn request_audio_capture_permission() -> PermissionStatus; // macOS: TCC preflight/request; else NotRequired
  // check_* macOS arm returns real Granted/Denied/NotDetermined via SPI guard
  // api.rs build(): Application/ApplicationByName/ProcessTree on macOS, Denied -> PermissionDenied early
  ```

- **Grounded in:** AudioCap TCC SPI `dlopen`+`dlsym` `TCCAccessPreflight`/`TCCAccessRequest` (`AudioRecordingPermission.swift:77-124`); rsac TCC notes + selector guards (`introspection.rs:296-313`, `tap.rs:407,424`).
- **Effort:** medium.
- **REQUIRED CHANGES:** OSStatus goes into `PermissionDenied.details` (the variant has NO `BackendContext` field, `error.rs:181-184`) or use `BackendError`. `TCCAccessRequest` is async with a completion callback — the sync `-> PermissionStatus` must bridge async→sync (block caller on a channel while the dialog is up). `dlopen`/`dlsym` null → `NotDetermined`. Reuse the macOS 14.4+ gate (`capabilities.rs:125-141`). Placement of an `audio/macos` shim called from `core/introspection.rs` matches existing precedent (`introspection.rs:137,176`).

### M8 — Honest macOS sub-14.4 capability reporting + ScreenCaptureKit feasibility note
- **Verdict:** adopt-with-changes
- **Gap:** Below macOS 14.4 the Process Tap doesn't exist; `supports_application_capture` is correctly `false` (`capabilities.rs:129`) but a macOS-13 caller gets a generic `PlatformNotSupported` with no hint that SCK (13.0+) is the route, nor a "what min OS unlocks this". A full SCK *backend* is disproportionate (display-scoped, app-exclusion not inclusion pre-15.0, screen-recording TCC ≠ audio TCC). The high-value slice is granular self-documenting capabilities.
- **api_sketch:**

  ```rust
  pub struct PlatformCapabilities { /*…*/
      pub min_os_for_application_capture: Option<&'static str>,
      pub system_capture_available_via: &'static str,
  }
  impl PlatformCapabilities { pub fn explain_unsupported(&self, target:&CaptureTarget) -> Option<String>; }
  // build() enriches its PlatformNotSupported error via explain_unsupported()
  ```

- **Grounded in:** screencapturekit-rs version-gated features `macos_13_0..macos_26_0`; cpal `ErrorKind` fallback-UI variants (`error.rs:10-86`); rsac `get_macos_version()` gate (`capabilities.rs:125-141`).
- **Effort:** medium.
- **REQUIRED CHANGES:** Update the `supports_format_missing` test literal (`capabilities.rs:383`) with the two new fields. Phrase SCK as an **assessment** ("ScreenCaptureKit (assessed, backend not yet implemented)"), never as a usable route — otherwise it re-introduces the "claim a feature you don't back" violation it's meant to prevent. Keep the SCK backend deferred.

### M9 — Capability + permission preflight: `AudioCaptureBuilder::preflight()` → `PreflightReport`
- **Verdict:** adopt-with-changes
- **Gap:** No way to ask "will this (target, config) work right now, and if not why + how to fix?" before committing to the slow/permission-prompting device resolution. `PlatformCapabilities` predicates + permission state are scattered.
- **api_sketch:**

  ```rust
  pub struct PreflightReport { pub findings: Vec<PreflightFinding>, pub permission: PermissionStatus }
  impl PreflightReport { pub fn is_ok(&self)->bool; pub fn blockers(&self)->impl Iterator<Item=&PreflightFinding>; }
  pub struct PreflightFinding { pub severity: PreflightSeverity, pub message:String, pub remedy:Option<String> }
  pub enum PreflightSeverity { Blocker, Warning }
  pub fn preflight(target:&CaptureTarget, config:&StreamConfig) -> PreflightReport; // caps+permission, NO device open
  impl AudioCaptureBuilder { pub fn preflight(&self) -> PreflightReport; }
  ```

- **Grounded in:** cpal contextual error taxonomy (`HostUnavailable`/`DeviceNotAvailable`/`PermissionDenied`); AudioCap TCC preflight; rsac private `SUPPORTED_SAMPLE_RATES` + `NotDetermined` TODO.
- **Effort:** medium.
- **REQUIRED CHANGES:** Validate against the builder's ACTUAL list (the hardcoded 6 rates at `api.rs:157` + channels 1..=32), NOT `sample_rate_range` — promote the const to one `pub` source consumed by both `build()` and `preflight()` (M4) to avoid drift. Surface the macOS `NotDetermined` limitation in the remedy text. Runs entirely before any stream exists — RT-irrelevant.

### M10 — `DeviceEnumerator::watch()` — hotplug / default-device-changed events
- **Verdict:** adopt-with-changes · **api_fit: clean**
- **Gap:** `DeviceEnumerator` (`interface.rs:221-243`) offers only one-shot snapshots. No notification on device add/remove/state-flip or default-endpoint change; consumers must poll. All three OS backends expose native change notifications.
- **api_sketch:**

  ```rust
  pub enum DeviceEvent { DeviceAdded{id,name,kind}, DeviceRemoved{id}, DefaultChanged{id,kind}, StateChanged{id,available} }
  trait DeviceEnumerator { fn watch(&self, on_event: Box<dyn FnMut(DeviceEvent)+Send+'static>) -> AudioResult<DeviceWatcher> { Err(PlatformNotSupported{..}) } }
  pub struct DeviceWatcher; // RAII; Drop unregisters OS listener + joins notify thread
  impl CrossPlatformDeviceEnumerator { pub fn watch(&self, …) -> AudioResult<DeviceWatcher>; } // inherent = real entry
  ```

- **Grounded in:** cpal `DefaultDeviceMonitor`/`IMMNotificationClient` (`wasapi/stream.rs:44-190`) + CoreAudio `AudioObjectPropertyListener` RAII (`coreaudio/macos/property_listener.rs:1-90`); wiremix `metadata.rs:1-58` default-sink listener + pipewire-rs `Registry::add_listener_local`.
- **Effort:** large.
- **REQUIRED CHANGES:** (1) Do NOT claim VISION mandates it — it's a net-new capture-UX feature (the "dynamic device changes" line is `REFERENCE_ANALYSIS.md:868`, a per-backend reference survey, not an rsac scope commitment). (2) Add a `PlatformCapabilities` flag (`supports_device_change_notifications`) so callers gate before calling. (3) The inherent enum `watch()` is the public entry (`CrossPlatformDeviceEnumerator` doesn't impl the trait); the trait default is for external impls. (4) Linux is genuinely large: `LinuxDeviceEnumerator` is a unit struct shelling out subprocesses with NO persistent loop — `watch()` needs a NEW persistent pw main-loop + registry/metadata listener thread (best sequenced after H4). RT-safe: handler runs on the OS notification thread, never the audio callback.

---

## Low priority

### L1 — `tracing`/`log` spans on the non-RT capture lifecycle + pump paths
- **Verdict:** adopt-with-changes
- **Gap:** `log::` used ad hoc (`api.rs:217/258/583/923`, `introspection.rs:153`), no correlation. Concurrent `AudioCapture` instances interleave log lines with no per-capture id; no spans around build→resolve→negotiate→start→pump→stop; no events for transitions.
- **api_sketch:**

  ```rust
  // Cargo.toml: [features] tracing = ["dep:tracing"]; tracing optional
  macro_rules! rsac_event { /* tracing event! when feature, else log::* */ }
  struct AudioCapture { /*…*/ capture_id: u64 } // process-local AtomicU64
  // pump/subscribe gain an instrumented span; optional:
  pub fn install_default_tracing();  // behind feature = "tracing"
  ```

- **Grounded in:** camilladsp outer-thread health logging (rate drift, silence, disconnect reason) with RT inner thread clean; cpal `RealtimeDenied` setup-path messages.
- **Effort:** medium.
- **REQUIRED CHANGES:** Sketch bug — `spawn_callback_pump` is an associated fn with no `self` (`api.rs:525`); pass `capture_id` + a Debug-cloned target as params (or create the span in `start()`). The "final stats from OBS-1" event must degrade to `overrun_count()` alone (no aggregate stats type exists in the library — those counters live only in `main.rs`); couple to H3's `StreamStats` once it lands. Add `tracing = {optional=true}` + the feature; with it off, the macro compiles to today's `log::` calls (behavior-identical). Strict review invariant: NO `tracing`/`log`/alloc/format inside `ring_buffer.rs` producer.

---

## Deferred / out-of-scope

- **PR-5 — Wire Linux `supported_formats()` via PipeWire `enum_params` (DEFER, not reject).** The goal (cross-platform parity; VISION.md:132 confirms it's unwired) is legitimate and in scope (format DISCOVERY, not conversion). But it's mis-scoped as "medium": the Node-proxy `enum_params`/`subscribe_params` machinery is entirely UNUSED in rsac (registry discarded as `_registry`, `thread.rs:887`), `LinuxAudioDevice` holds no live PW thread (built from subprocess parsing, `mod.rs:335-341`), and `id` may be a node NAME not a numeric id. It also depends on H4's not-yet-built command-channel/proxy infrastructure. **No caller is blocked** — PipeWire negotiates the authoritative format at connect-time and reports it via `param_changed`. Defer until H4 lands the proxy machinery + device-lifetime plumbing, then implement on top.

- **EF-3 — Linux `supported_formats()` via a `pw-dump` probe (REJECTED).** False premise: `supported_formats() == vec![]` is a DELIBERATE recorded decision (`audio/linux/mod.rs:357-367`, audit finding L2; module doc `mod.rs:30-36`), not an unfinished gap. The negotiation already happens authoritatively in the connect path: rsac requests F32LE (`thread.rs:1145-1154`), PipeWire picks the closest, and `param_changed` (`thread.rs:1042-1055`) reports it back and stamps each buffer. A static `pw-dump` probe would inject a fragile spa→`SampleFormat` guess that can DISAGREE with the live negotiated result — a regression that overrides authoritative negotiation, exactly what L2 warns against. `PlatformCapabilities::linux()` (`capabilities.rs:143`) already advertises formats/ranges honestly. (PR-5 is the correct, in-process variant of this idea, deferred above.)

- **DL-2 — Opt-in auto-reconnect policy + supervisor thread (DEFER behind prerequisites).** Scope and RT-safety are fine, but the WAKE TRIGGER is infeasible today: on a mid-stream device-invalidated error the WASAPI thread merely logs, breaks, and calls `signal_done()` (`thread.rs:483-489,537`), which does the IDENTICAL `Running→Stopping` transition as a clean user stop (`bridge/stream.rs:220-225`). `StreamState::Error` exists but neither backend ever sets it on disconnect, and no reason is propagated — so a yanked device is indistinguishable from a user stop. Also the swap-the-`Arc` design fails: `subscribe()`/`spawn_callback_pump` capture an `Arc::clone` of the CURRENT stream at spawn (`api.rs:806,526`), so they'd keep reading the OLD stream after a swap and exit on `Err`, not follow it. Prerequisites: (a) emit a distinct disconnect signal (`StreamState::Error` + reason) on backend mid-stream failure; (b) an intentional-stop flag in `AudioCapture::stop()`; (c) ideally M10's `DefaultChanged` as the trigger; (d) a swap-aware stream indirection (`ArcSwap`/forwarding stream). Defer until those land.

- **PR-3 — Device-change notifications via a separate `DeviceEnumerator` watch subscription (SUBSUMED by M10, DEFER).** Same capability as M10; lower-priority framing. The grounding overstates support (`REFERENCE_ANALYSIS.md:868` flags 2 of 3 backends ⚠️) and it depends on H4's unbuilt PipeWire registry listener. No current consumer is blocked (poll `enumerate_devices()`). Build M10 once H4's registry listener lands; needs the same new `PlatformCapabilities` flag.

- **INFORMATIONAL-ONLY references that crossed into DSP/effects/playback (do NOT adopt):** wasapi-rs `AudioEffectsManager`/AEC (`api.rs:1849-1913`); camilladsp PI rate-controller for playback (`device.rs:984-994`); SCK per-output QoS dispatch / microphone-vs-system separation / async completion handlers. Out of rsac's capture-only scope; recorded in the Reference Delta as informational pattern context only.

---

## Suggested implementation waves (High items)

- **Wave A — Zero-risk pure-additive DX (independent, parallelizable):** H1 (`FromStr`/`target_str`) + H2 (`prelude` + `AudioBuffer` metering). No trait changes, no platform code, no RT path. Land first to remove the day-one footguns and unblock CLI/config consumers.
- **Wave B — Fix the broken contract (depends on nothing, but feeds M6/L1):** H3 (`stream_stats()` + `format()` + `#[non_exhaustive] StreamStats` + the two additive `CapturingStream` default-0 methods). Establishes the canonical stats surface that M6 (windowed backpressure) and L1 (tracing final-stats event) build on.
- **Wave C — Linux robustness (largest, sequence after A/B):** H4 (native in-process PipeWire enumeration). Removes the silent-empty-on-headless/Flatpak failure mode. Its registry/metadata listener + command-channel infrastructure is the prerequisite that later unblocks M10 Linux arm, PR-5, and the metadata default-change work — so it must land before those even though it's the heaviest single item.

Sequencing rationale: A and B are independent and can run concurrently; C is the long pole and should start in parallel with A/B since it gates the most downstream Medium/deferred work.

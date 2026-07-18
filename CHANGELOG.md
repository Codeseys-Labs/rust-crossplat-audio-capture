# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

In addition to the standard Keep-a-Changelog subsections, each release that
touches the C ABI carries a dedicated **`### C ABI changes`** subsection.
This records every change to the `rsac-ffi` exported `extern "C"` symbols or
the generated `rsac.h` header that affects binary compatibility — symbol
removals/renames, signature or `#[repr(C)]` layout changes, and changed
return/error-code semantics. Per the
[versioning & ABI contract](docs/RELEASE_PROCESS.md#versioning--abi-contract),
any such change is a **MAJOR** bump for the FFI surface; the subsection tells
consumers who pin the `.so`/`.dll`/`.dylib` exactly what to recompile against.
Releases with no ABI change omit the subsection (or state "No C ABI changes").

## [Unreleased]

### Added

- **Bindings (live gain/mute):** exposed `Composition::set_gain` / `set_muted`
  (+ `gain` / `is_muted` getters, rsac-5a2d) across all four binding layers —
  C FFI (`rsac_composition_set_gain` / `_set_muted` / `_gain` / `_is_muted`),
  Node/napi (`setGain`/`setMuted`/`gain`/`isMuted`), Python
  (`set_gain`/`set_muted`/`gain`/`is_muted`), and Go
  (`SetGain`/`SetMuted`/`Gain`/`IsMuted`). Sources addressed by group name +
  within-group index. Setters refuse on a not-started/stopped/ended composition
  (STREAM_READ / StreamError / ErrStreamRead); getters keep reading a stopped
  composition. Gain is validated by the core after the f64→f32 narrowing in the
  dynamically-typed bindings, so a finite-in-f64 value that narrows to inf is
  rejected. napi/python wrappers take the shared read guard (rsac-8082 topology,
  &self methods); Go guards the handle mutex + KeepAlive. (rsac-9dec)
- **Compose (group master gain):** `Composition::set_group_gain` / `group_gain`
  apply a live per-**group** master gain on a running composition — a linear
  multiplier applied **on top of** every member source's own gain
  (`set_gain`), effective on the next compositor tick (~1 quantum). Addressed by
  group name; seeded to `1.0` (identity) at start. Orthogonal to `set_muted`
  (a group gain of `0.0` silences the group without touching any source's mute
  or gain). Backed by a lock-free per-group atomic read on the (non-RT)
  compositor thread — no RT-path change. Same lifecycle/validation contract as
  `set_gain`: the setter refuses (`StreamReadError`, `NotInitialized` before
  start / `NotRunning` after stop/end) and `ConfigurationError` for an unknown
  group or a non-finite/negative gain; the getter keeps reading the last-applied
  value on a stopped composition. `CompositionStats` gains a `groups: Vec<GroupStats>`
  field (new `#[non_exhaustive]` struct) exposing every group's current gain in
  one snapshot. Additive-only; `cargo-semver-checks` reports `minor`. No C ABI
  change this release; language bindings handled separately. (rsac-1ce7)
- **Compose (live mixing):** `Composition::set_gain` / `set_muted` (+ `gain` /
  `is_muted` getters) apply per-source level and mute changes on a **running**
  composition, effective on the next compositor tick (~1 quantum latency).
  Sources are addressed by group name + within-group index (declaration order).
  Gain **replaces** the build-time `Group::source_with_gain` value (which now
  seeds the initial level); mute is a separate flag, so unmute restores the
  prior gain. Backed by lock-free per-source atomics read on the (non-RT)
  compositor thread — no RT-path change. The setters refuse (`StreamReadError`,
  `LifecycleStage::NotRunning`) once the composition has stopped or ended — no
  tick would ever apply the change (CodeRabbit PR #62); the getters keep
  reading the last-applied values. `SourceStats` gains `gain` / `muted`
  fields (the `#[non_exhaustive]` struct makes this non-breaking). Group-level
  master gain is provided by `set_group_gain` (rsac-1ce7). No C ABI / bindings change this
  release. Additive-only; `cargo-semver-checks` reports `minor`. (rsac-5a2d)
- **Bindings (Node/napi):** `Composition`, `CompositionBuilder`, `Group` classes
  exposing multi-source channel composition (ADR-0011) — the same
  `Arc<RwLock<>>` + pump topology and stop-vs-parked-read fix as `AudioCapture`
  (rsac-8082), `onData`/`onEnd` push delivery, sync + async terminal-observable
  reads, hand-maintained `index.d.ts` types (`CompositionStats` / `SourceStats`,
  the full Rust set including `gapPaddedFrames` / `innerDropped`). The `compose`
  cargo feature is now enabled unconditionally in the compiled addon (rsac-fba7).
- **Bindings (Python):** `Composition`, `CompositionBuilder`, `Group` classes
  exposing multi-source channel composition (ADR-0011) — synchronous and
  asynchronous context-manager + iterator protocols, terminal-observable reads,
  and per-source stats (`CompositionStats` / `SourceStats`, the full Rust set
  including `gap_padded_frames` / `inner_dropped`). Teardown reuses
  `AudioCapture`'s GIL-released two-phase lock discipline (rsac-8082). The
  `compose` cargo feature is now enabled unconditionally in the Python wheel
  (rsac-fba7).
- **Bindings (Go):** `Composition` / `CompositionBuilder` / `Group` cgo wrappers
  over the `rsac_composition_*` / `rsac_group_*` C FFI, exposing multi-source
  channel composition (ADR-0011) — mutex-guarded lifecycle with the same
  in-flight-read drain barrier and `request_stop`-unblock teardown as
  `AudioCapture`, channel-based `Stream`/`StreamWithErrors`, and per-source
  stats. `librsac_ffi.a` for the Go bindings is now built with
  `--features compose` (`FFI_FEATURES += compose`) so the compose symbols are
  exported (rsac-fba7).
- `list_audio_applications_scoped()` + `ApplicationScope` / `ApplicationEnumeration`:
  application enumeration now reports whether the list is the *exact*
  audio-producer set or an *unfiltered fallback superset*. On macOS the
  audio-process filter can be unavailable (macOS < 14.4, or the CoreAudio
  process-object query is unavailable / reports no active PIDs), in which case
  the list is the full running-app set (`ApplicationScope::AllRunningFallback`);
  Windows (`AudioSessionStateActive`) and Linux (native PipeWire audio nodes)
  report `ApplicationScope::ExactAudioProducers` on success. A failed backend
  query (PipeWire unreachable, a WASAPI/CoreAudio error) reports
  `ApplicationScope::EnumerationFailed` — the empty list is *incomplete*, not
  evidence that nothing is playing. `rsac list-apps` surfaces
  the fallback and failure modes with banner lines. The existing
  `list_audio_applications()` is unchanged (it delegates to the scoped variant
  and discards the scope). Additive-only; `cargo-semver-checks` reports `minor`.
  No C ABI change. (rsac-f547)
- `rsac list-apps` CLI subcommand — prints PID / name / bundle-id for
  applications currently producing audio via `list_audio_applications()`
  (`cli` feature; demo binary only, no library API change; rsac-86ee).
- `AudioCapture::drain_to(sink)` — the built-in background-thread sink driver,
  previously only on `RunningCapture`/`Composition`, is now available on a
  plain started `AudioCapture` too. Same `rsac-drain` thread, presence gate,
  recoverable-vs-fatal policy, and `flush()`+`close()` finalization; the
  `RunningCapture` method now delegates to it so the driver lives in one place.
  Additive — `cargo-semver-checks` reports `minor`. The earlier `pipe_to` name
  for this driver is retired: `drain_to` is the single shipped name across all
  three surfaces (rsac-2135).
- **Bindings (Node/napi):** the runtime smoke suite (`bindings/rsac-napi/tests/smoke.test.mjs`)
  now also exercises `getDefaultDevice()`, awaited with the same
  headless-tolerant catch discipline already used for `listDevices()`. It was
  an async, exported function that had never been smoke-tested — the same
  class of dormant unhandled-rejection landmine that caused a prior CI
  failure on the `listDevices()` leg (rsac-f9c1). Test-only; no library
  behavior change.
- `AudioError::lifecycle_stage()` — a structured accessor that classifies a
  lifecycle-cause `StreamReadError` into a new `#[non_exhaustive]`
  `LifecycleStage` enum (`NotInitialized`, `NotRunning`, `Unknown`), so
  callers no longer need to grep `reason` prose to distinguish "never
  started"/"stopped" from "not yet running" (rsac-feb4). Additive-only;
  `StreamReadError`'s shape is unchanged and `cargo-semver-checks 0.48.0`
  confirms "no semver update required" against v0.4.2.

### Changed

- **core (Android) — BREAKING:** `AndroidProjectionToken` is no longer `Copy`/
  `Hash`; it now enforces single-owner deletion of the `MediaProjection`
  `GlobalRef` via a shared consume-latch, fixing a safe-Rust JNI double-delete
  (UB) when a builder/`StreamConfig` carrying a token was cloned and built
  twice. The first capture stream built from a token (or any `Clone`/
  `StreamConfig` copy) claims sole deletion ownership; a second `build()` from a
  clone is now refused with `StreamCreationFailed` rather than double-releasing
  the ref. The shared claim is per token *instance* (and its clones), not per
  raw `i64`: wrapping the same handle in two separate `from_raw` calls yields
  independent latches and is an unguarded double-delete — mint a token once and
  clone it. `from_raw` is accordingly now `unsafe fn`, encoding the
  validity + once-per-handle contract in the API (CodeRabbit PR #62).
  `Clone`, `PartialEq`, `Eq`, and the `as_raw`/
  `with_android_projection` signatures are retained. This surface is
  `#[cfg(target_os = "android")]`, so the host `cargo-semver-checks` gate does
  not observe it; the removal of `Copy`/`Hash` is nonetheless a breaking change
  for Android consumers and gates the next release at **0.5.0** (rsac-3407).
- Mobile backend deps (`jni-sys`; `objc2-avf-audio`/`block2`/`objc2`/
  `objc2-foundation` on iOS) are now `optional` and tied to `feat_android`/
  `feat_ios`, completing the `cfg(all(target_os, feature))` double-gate the
  `src/` cfgs already use — a `--no-default-features` build for the mobile
  triples no longer pulls backend deps unless the matching feature is enabled
  (rsac-a96b, CodeRabbit PR #36). Default build is unchanged (both features are
  in `default`).
- The compose "Composition is not started" `StreamReadError` now routes through
  a canonical `REASON_*` constant (`REASON_COMPOSITION_NOT_STARTED`), so
  `AudioError::lifecycle_stage()` classifies it as `NotInitialized` instead of
  `Unknown` (rsac-90b1). Message text is byte-identical; no error-shape change.
- CoreAudio tap-creation/introspection errors (`CoreAudioProcessTap::new`,
  `new_tree`, `new_system`, `get_stream_format`) now report their real
  `operation` label (`"process_tap"`, `"process_tap_tree"`, `"system_tap"`,
  `"get_stream_format"`) instead of the generic `"Unknown"` category that
  `map_ca_error` previously derived from a synthesized `CAError::Unknown`
  status — e.g. `rsac record --pid 999999` now reports `operation:
  "process_tap"` instead of `operation: "Unknown"` (rsac-931e). `pub(crate)`
  change only; no public API impact. Side effect: a permission-denied
  OSStatus arriving through `map_ca_error` without a caller-supplied label
  now derives its `operation` from the `CAError` variant (e.g. `"AudioUnit"`)
  instead of the former generic `"audio_capture"` — diagnostic text only.
- `BridgeStream`'s pre-start (`Created`-state) read error now reuses the
  shared `REASON_NOT_RUNNING` message instead of formatting the live
  `StreamState` into the text — a diagnostic-text change only, made to keep
  `AudioError::lifecycle_stage()` a pure string-equality match with no
  heuristics (rsac-feb4).

### Deprecated

### Removed

### Fixed

- **macOS device-watch teardown no longer leaks the `WatchListenerContext` or
  races an in-flight callback (GH #32 / ADR-0005 §5).**
  `DeviceEnumerator::watch` now registers block-based CoreAudio listeners
  (`AudioObjectAddPropertyListenerBlock`) on a **self-owned serial dispatch
  queue** instead of the C-function-pointer PROC API. At teardown it removes each
  block on that same queue, then dispatches a synchronous no-op **barrier** which
  — by serial-queue FIFO ordering — cannot return until every in-flight
  notification block has finished; only then is the `Arc<WatchListenerContext>`
  dropped, so the context is **freed, not leaked**, with no use-after-free window.
  This eliminates the previously-*intentional* per-`watch()` context leak (the
  stopgap that made a late PROC deref sound) and the underlying in-flight-callback
  race. Adds direct `dispatch2` / `block2` macOS dependencies. No public-API
  change (the `DeviceWatcher` contract is unchanged) and no C ABI change.
  (rsac-e8aa)
- **Bindings (Node/napi + Python):** fixed a deadlock where `stop()` (and, in
  Python, `close()`/`__del__`) could hang forever against a thread parked in a
  blocking read of a silent stream. Both bindings previously held their single
  wrapper mutex across the parked `read_chunk_blocking`, so the teardown — which
  needed the same lock to reach the stream — blocked on a lock the reader would
  only release once the stream went terminal. The wrapper now guards the capture
  with an `RwLock`: blocking readers park under a shared read guard, and the
  teardown first takes a read guard to call the core's `request_stop()` (which
  signals the stream terminal and wakes the parked reader without contending for
  the lock) before taking the write guard for the lifecycle teardown. In the
  Python binding the teardown additionally runs the whole read-guard →
  write-guard dance inside `Python::allow_threads` (GIL released): the parked
  reader releases the GIL across its blocking read but re-acquires it before
  dropping its read guard, so a teardown that held the GIL across the write-lock
  acquire would still deadlock (reader blocked on the GIL, teardown blocked on
  the write lock). `start()` likewise no longer requests exclusive access while
  a reader may be parked: a redundant `start()` on a running capture is resolved
  under a shared guard (core `start()` is idempotent on a running stream), so it
  cannot block behind — or deadlock with — a parked blocking read. Public API
  and terminal (`StreamEnded`) semantics are unchanged (rsac-8082).
- **Bindings (Node/napi):** `AudioCapture`'s push-model data pump (`onData`/
  `offData`/`stop`) now uses the same monotonic `pump_generation` cancellation
  guard `Composition` got in PR #59 — a rapid `offData()` → `onData()` flip
  could previously resurrect a pump thread that had already observed
  cancellation (or leave a *second* pump alive delivering concurrently with
  it), and a data-pump-thread spawn failure left `pump_active` permanently
  `true`, wedging all future `start()`/`onData()` pump attempts. Each spawned
  pump now owns the generation value current at spawn time and exits the
  moment cancellation bumps it, regardless of any later flag flip; a spawn
  failure now rolls `pump_active` back so a later `start()`/`onData()` can
  retry. `Composition` was fixed for this in PR #59 (CodeRabbit
  3605948884); `AudioCapture` had the identical pre-existing pattern but sat
  outside that PR's diff (rsac-1a34). No public API change.

### Security

### C ABI changes

- **New exported symbols (MAJOR for the FFI surface).** Added four
  `RSAC_FEATURE_COMPOSE`-gated functions to `rsac-ffi`:
  `rsac_composition_set_gain(const RsacComposition*, const char* group, size_t source_idx, float gain)`,
  `rsac_composition_set_muted(const RsacComposition*, const char* group, size_t source_idx, int32_t muted)`,
  `rsac_composition_gain(const RsacComposition*, const char* group, size_t source_idx, float* out_gain)`,
  `rsac_composition_is_muted(const RsacComposition*, const char* group, size_t source_idx, int32_t* out_muted)`.
  Existing symbols and layouts are unchanged; this is purely additive to the
  header, but new exported entry points are a MAJOR bump for consumers who pin
  the shared library. Regenerate `rsac_generated.h` (cbindgen) and mirror the
  prototypes in the curated `rsac.h`. (rsac-9dec)
- No change to existing symbols. `rsac_builder_set_android_projection` keeps its
  `int64_t` signature; the single-owner `MediaProjection` token contract
  (rsac-3407) is documented on the symbol. The library enforces it only for a
  *cloned* builder/config (which share one deletion latch → the second
  `rsac_builder_build()` fails with `RSAC_ERROR_STREAM_FAILED`); it does **not**
  deduplicate on the raw `int64_t`, so supplying the same handle to two
  independently constructed builders is an unguarded double-delete (UB) that
  C/Flutter callers must avoid by construction (one handle → one builder).

## [0.4.2] - 2026-07-17

### Added

### Changed

- Release version lockstep now includes `mobile/android-native/Cargo.toml`, so
  the Android `librsac.so` shim version stays aligned with the root crate and
  binding manifests.
- Python `AudioCapture.read()` and the Node `readBlocking()` /
  `readBlockingAsync()` readers now use the terminal-accurate
  `read_chunk_blocking`: after `stop()` or a fatal backend error they surface
  the stream's true terminal state promptly instead of downgrading it to a
  recoverable "not running" error — matching the C FFI, Go, and the Python
  iterator, which had already migrated.

### Deprecated

### Removed

- Retired the stale root Docker test matrix and its dead compose/scripts/images;
  the maintained container surface is now the Linux/PipeWire devcontainer plus
  the optional manual `dockur` native VM lab.
- Windows: removed a structurally unreachable malformed-packet branch and its
  dead counters from the WASAPI capture loop (no behavior change; the
  interleaved-f32 contract enforcement and its unit tests remain).

### Fixed

- Refreshed 0.4.1 docs, binding READMEs, and example commands to reflect source
  builds before registry publishing, mobile compile-only status, Go module tags,
  and the current example feature flags.
- **macOS: capturing from an input-only device (every real microphone) no
  longer fails instantly with OSStatus -10851.** The AUHAL setup now follows
  TN2091's ordering — enable input IO and disable output IO *before* binding
  `kAudioOutputUnitProperty_CurrentDevice` — so the bind no longer asks the
  still-enabled output element to attach to a device with no output streams.
  Combo input+output devices (USB headsets) masked the bug. (#53)
- compose: renegotiating a resampled source to exactly the session rate no
  longer strands the old resampler's tail (up to ~25–45 ms of captured audio
  was silently dropped); the exact-rate path now flushes it like the
  rate-change and end-of-stream paths already did.
- macOS: the `CATapDescription` used to create a Process Tap is now released
  after `AudioHardwareCreateProcessTap`, fixing a per-tap-creation object leak
  in all three constructors (single-process, process-tree, system-wide).
- Recoverable capture errors forwarded to `subscribe()` receivers now arm
  their coalescing cooldown only when actually delivered; previously a full
  channel dropped the error *and* suppressed the next report for the full
  coalescing interval.
- Linux: `ApplicationByName` matching strips a trailing `.exe`
  case-insensitively (Wine apps report names like `VLC.EXE`), and the native
  resolver now matches `application.name` and `application.process.binary`
  independently — the same fields the `pw-dump` fallback checks — so a
  non-matching display name no longer shadows a matching binary.

### Security

- `build.rs` no longer executes package managers: the opt-in
  `RSAC_AUTO_INSTALL=1` path that ran `sudo apt-get` from inside the cargo
  build script is removed. The build now prints the exact install commands;
  dependency installation stays in the human-invoked setup scripts.

## [0.4.1] - 2026-07-08

### Added

- **`macos-tcc-spi` feature — real audio-capture permission preflight
  (ADR-0015).** `check_audio_capture_permission()` on macOS can now return a
  real answer: there is no public API for the `kTCCServiceAudioCapture`
  (Process Tap) status, so the opt-in feature `dlopen`s the private
  `TCCAccessPreflight` SPI at runtime (the AudioCap mechanism). Off by
  default: the published artifact carries no private-symbol usage and keeps
  the honest `NotDetermined` stub; any SPI resolution failure also degrades
  to `NotDetermined`.
- **macOS silent-zeros diagnostic (ADR-0016).** A denied/unkeyed TCC context
  makes a Process Tap stream perfectly silent zeros while looking healthy
  (verified on macOS 26). The RT callback now flips an atomic on the first
  non-zero sample (alloc-free) and a non-RT watchdog logs one warning at 2 s
  if a tap capture is still all-silent — warn-only, since silence is
  legitimate. Would have one-lined a multi-hour diagnosis.
- **Multi-source channel composition (`compose` feature, ADR-0011).** New
  `rsac::compose` module — `CompositionBuilder` → `Composition`: declare
  *groups* of `CaptureTarget`s that either mix down to Mono/Stereo output
  channels (gain-weighted plain summation, optional `clamp_output`) or pass a
  single source's native channels through (`keep_channels()`); groups append
  in declaration order into ONE interleaved-f32 multi-channel stream speaking
  the standard `CapturingStream` contract (terminal semantics, overrun and
  backpressure counters, `drain_to()` sinks, async waker all included).
  Sources delivering a different sample rate are resampled to the session
  rate (default 48 kHz) with `rubato` on a dedicated non-RT compositor
  thread. Alignment is master-clock paced (first system/device source) with
  per-source silence-padding / bounded trimming and a wall-clock fallback on
  master stall; `Composition::stats()` exposes per-source
  `padded_frames`/`trimmed_frames`/`resampling` counters and
  `channel_map()` reports which output channel belongs to which group. Off by
  default; enabling it pulls `rubato` + `audioadapter-buffers`. See
  `examples/composed_capture.rs` and docs/designs/0011-compose-feature.md.
- **CI: MSRV job** — builds with Rust 1.87 (`rust-version` floor) so the
  declared MSRV can no longer rot silently under the pinned toolchain; also
  the tripwire for optional-dependency MSRV bumps (rubato et al.).
- **CI: cargo-hack feature-powerset job** — pairwise (`--depth 2`) checks
  across `async-stream`/`sink-wav`/`test-utils`/`bridge-zerocopy`/`tracing`/
  `compose`/`cli`/`feat_linux`, replacing single-combo-only coverage.
- **Release gate: cargo-semver-checks** — the release verify stage now fails
  on API breakage not matched by the right semver bump (baseline = previous
  release tag; auto-skips on the first release).
- **`#![warn(missing_docs)]`** at the crate root, with the outstanding
  rustdoc gaps filled (`ErrorKind` variants, `AudioError` variant fields,
  platform enumerator items).
- **Buffer timestamps populated on every backend (stream position).** The
  platform RT push paths (WASAPI thread, PipeWire `.process`, CoreAudio
  IOProc) now use stamping push variants
  (`push_samples_or_drop_stamped`/`push_samples_guarded_stamped`): each
  delivered `AudioBuffer` carries `timestamp() = frames offered ÷ rate` —
  pure integer math, no clock syscall, ADR-0001 alloc-free proof extended to
  the stamped path (`tests/rt_alloc.rs`). Producer-side drops surface as
  *gaps* between consecutive timestamps instead of a contiguous lie. Composed
  buffers (the `compose` feature) are stamped with the same semantics. The
  refinement to per-backend device clocks stays tracked (rsac-ec25).
- **`Composition` consumption parity**: `subscribe()`,
  `subscribe_with_errors()`, and (with `async-stream`) `audio_data_stream()`
  now exist on `Composition` with the same delivery contracts as
  `AudioCapture` (the pumps are shared, not duplicated).
- **Compose exposed through the C FFI** (`rsac-ffi` `compose` feature, off by
  default like `sink-wav`): 24 new `rsac_*` functions —
  `RsacGroup`/`RsacCompositionBuilder`/`RsacComposition` opaque handles,
  layout enum, `repr(C)` stats structs, channel-map introspection — all
  null-safe and panic-caught; headers regenerated deterministically behind
  `RSAC_FEATURE_COMPOSE`. Python/Node/Go wrappers remain tracked
  (rsac-fba7).
- **Interop recipes** (`docs/INTEROP.md`): copy-paste bridges from rsac
  buffers into `dasp`, `cpal`/`rodio` playback, `hound` WAV, and encoder
  pipelines, plus timestamp-based drop/sync accounting.
- **CI: advisory coverage job** (cargo-llvm-cov on the Linux lib suite,
  lcov artifact + optional Codecov upload when `CODECOV_TOKEN` exists;
  `continue-on-error` — trend data, not a gate).
- **Release: automatic tag→publish fan-out.** `release-tag.yml` mints a
  GitHub-App installation token (org secrets `RSAC_RELEASE_APP_ID` /
  `RSAC_RELEASE_APP_PRIVATE_KEY`) so the version tag triggers the
  crates.io/npm/PyPI publish workflows automatically, and pushes the
  `bindings/rsac-go/vX.Y.Z` module tag in the same job. Without the secrets
  the previous manual-dispatch behavior is preserved and the job summary
  documents the setup.
- **Mobile backends — Android + iOS (compile-verified; ADR-0012/0013).**
  New target-gated backends riding the same `BridgeStream` data plane:
  - **Android (`feat_android`)**: AAudio microphone capture
    (`CaptureTarget::Device`), plus all playback-capture tiers
    (`SystemDefault`, `Application`/`ApplicationByName`, `ProcessTree` —
    tree ≡ app on Android) via `AudioPlaybackCaptureConfiguration`:
    the Java capture loop lives in the first-party AAR
    (`mobile/android`, Kotlin `CaptureBridge` + `RsacCaptureService` +
    `RsacProjection` consent helper) and feeds Rust through a JNI ingest
    layer (`jni-sys`, registry-id sessions, alloc-free scratch pushes).
    Playback capture requires API 29+, a foreground service, and the new
    user-consent token: `AudioCaptureBuilder::with_android_projection`
    (`AndroidProjectionToken`); building a playback target without one
    fails preflight with the new `AudioError::UserConsentRequired`.
    `librsac.so` packaging (cargo-ndk) ships in the AAR with its
    `JNI_OnLoad` export CI-asserted.
  - **iOS (`feat_ios`)**: AVAudioEngine microphone capture, plus
    `SystemDefault` as a ReplayKit broadcast-upload consumer
    (`with_ios_app_group`; the canonical broadcast-ring contract,
    RingLayout v1, ships in the `mobile/ios` SwiftPM package).
    `Application`/`ProcessTree` are **permanently unsupported** (no iOS
    API) and report so via `PlatformCapabilities`.
  - **Honest status**: both backends are compile- and CI-build-verified
    (cross-target check + clippy, real Gradle AAR + xcodebuild SwiftPM
    builds) but **not yet runtime-verified on any device** — capability
    reporting and docs say so explicitly.
- **C FFI capability parity accessors.** `rsac_capabilities_*` grew the
  fields the Rust struct already exposed: device-change-notification
  support, user-consent requirement (pairs with the new
  `rsac_builder_set_android_projection`), the supported sample-format
  list, and the sample-rate range + discrete-rate whitelist. The Go
  binding projects all of them (capability parity across C/Python/Node/Go).
- **CI: deterministic desktop audio integration tiers.** The Linux jobs
  gain a hard routing gate (`scripts/ci-linux-audio-route.sh`: pins the
  null-sink default, proves the tone→monitor route end-to-end with sox,
  then flips `RSAC_CI_AUDIO_DETERMINISTIC=1`) on a PipeWire stack brought
  up with a private session D-Bus — the missing session bus silently
  killed wireplumber and was the root cause of the historical
  "SystemDefault yields 0 buffers" softness. Windows device/process tiers
  gain the equivalent VB-CABLE endpoint gate
  (`scripts/ci-windows-audio-default.ps1`). All six desktop platform×tier
  jobs now hard-fail on silence instead of warning.
- **DevEx: mise task coverage for the operator scripts.** `mise run
  release:bump -- X.Y.Z [--dry-run]`, `mise run release:verify-docs`, and
  `mise run test:audio` (host-OS dispatch); new generic
  `scripts/run-bash.ps1` Git-bash wrapper backs the Windows legs.

### Changed

- **CLI-only dependencies are now feature-gated (`cli` feature).** `clap`,
  `color-eyre`, `ctrlc`, and `env_logger` were unconditional dependencies
  serving only the demo binaries; they are now optional behind the new `cli`
  feature (NOT in defaults). Library consumers' dependency trees shrink
  accordingly. **Action needed only for binary users:** build/install the
  demo CLI with `--features cli`; the `rsac`/`standardized_test` bins and the
  `verify_audio`/`basic_capture`/`record_to_file` examples declare
  `required-features = ["cli"]`.
- **VISION scope amendment (ADR-0011):** stream mixing moved from
  out-of-scope to in-scope *behind the opt-in `compose` feature*;
  general-purpose DSP/effects/encoding remain out of scope.
- `Cargo.toml` `repository` now points at the canonical
  `Codeseys-Labs/rust-crossplat-audio-capture` (was a stale personal fork
  URL); `/bindings` is excluded from the published crate tarball (rsac-go has
  no manifest of its own and would otherwise ship in it).
- CI ARM64 cross-compile gates (`cross-compile-linux-arm64`,
  `go-bindings-arm64-check`) are now exit-code-authoritative instead of
  grepping logs behind `|| true` (which `CARGO_TERM_COLOR=always` ANSI codes
  could false-green); only the known missing-aarch64-PipeWire diagnostic is
  tolerated.

### Deprecated

### Removed

- Stale `blacksmith-audio-probe.yml` one-shot diagnostic workflow (its
  findings are recorded in AGENTS.md §6).

### Fixed

Real-hardware macOS verification pass (macOS 26 / M4, full report in PR #35):

- `watch()` rustdoc linked a private item from public docs — invisible to
  CI's Linux-only docs job because `src/audio/macos/` is cfg-stripped there
  (systemic gap seeded as rsac-0fb1).
- A stale ci_audio round-trip test still asserted the pre-#27
  `Application→ProcessTree` mapping; retargeted to the shipped contract.
- `subscribe_delivers_buffers_from_live_capture` hard-asserted capture
  content behind `require_audio!()` while targeting the TCC-gated
  `SystemDefault` tap; now gated on `require_system_capture!()` like its
  siblings.

Adversarial-review batch (16 tracked seeds, all landed pre-merge):

- **Compose engine panic containment** — a panic on the compositor thread
  previously left the composed stream permanently non-terminal (blocking
  reads spun on the 1 ms backstop forever, `is_running()` lied, C callers
  blocked indefinitely). The tick loop now runs under `catch_unwind` with an
  infallible teardown that poisons the stream to a fatal terminal.
- **Resampler tail flush** — resampled compose sources no longer lose the
  final partial chunk + FFT delay residue (~25–45 ms) at natural end.
- **Intra-source gap compensation** — the compose engine now consumes the
  drop-gap timestamp semantics: inner-ring overflows re-insert the hole as
  silence (`gap_padded_frames` stat) instead of silently time-compressing
  that source against its peers; inner overruns surface as
  `SourceStats::inner_dropped`.
- **Timestamps survive rate renegotiation** — stream-position stamps now
  accumulate nanoseconds instead of dividing a frame counter by the
  *current* rate (a mid-stream PipeWire renegotiation retroactively rescaled
  the whole past timeline); the position advance is centralized so mixing
  push variants can no longer desync the timeline. The mock backend now
  stamps like every real backend.
- **WASAPI capture-thread panic containment** — the capture loop runs under
  `catch_unwind` routed into the fatal-error tail, upholding ADR-0010's
  "no exit path leaves the bridge non-terminal" on the panic path.
- **PipeWire RT callback logging** — the one-shot misalignment `log::warn!`
  (allocation + lock on the RT thread, outside the panic guard) is now two
  plain counter adds; the warning emits from the non-RT teardown path.
- **Bounded subscribe pumps** — `subscribe()`/`subscribe_with_errors()` on
  both handles switch from unbounded channels to `sync_channel(128)` +
  drop-and-count (new `subscriber_dropped_count()`), restoring the
  "drop, don't block, and count it" invariant; the fatal terminal is
  guaranteed as the final item; repeated recoverable errors coalesce.
- **Consumption preconditions** — subscribe/drain can now attach during the
  drainable `Stopping` window (a fast-ending composition no longer strands
  its buffered tail); `stop()` → `start()` restart-by-recreation is blessed
  and documented; `Composition`'s read methods renamed to
  `read_chunk_nonblocking`/`read_chunk_blocking` (the old `read_buffer`
  names collided with `AudioCapture`'s never-fatal family while carrying
  terminal-observable semantics).
- **FFI soundness** — `rsac_group_set_layout`/`rsac_default_device` no longer
  take Rust enums by value across the ABI (out-of-range ints from C were
  instant UB; now validated `int32_t`); `rsac_composition_builder_add_group`
  no longer has a panic window that double-freed the group;
  `rsac_capture_free`/`rsac_composition_free` teardown is panic-caught.
- **Go module path** — `bindings/rsac-go` is now repo-path-prefixed so the
  `bindings/rsac-go/vX.Y.Z` tag automation actually resolves via `go get`
  (the previous path pointed at a nonexistent repo; the fan-out achieved
  nothing).
- **Release-path truth** — RELEASE_PROCESS.md rewritten where it had drifted
  (PyPI is 4 abi3 wheels + Trusted Publishing, not 15 wheels + a token
  secret; `bump-version.sh` rewrites all six manifests); README no longer
  claims Windows audio tiers are soft; `release-npm.yml` prepublish gets its
  token and the final publish uses `--ignore-scripts` (double-publish);
  `release.yml` dispatch publishes now require a version guard;
  `ci-audio-tests.yml` actions SHA-pinned (including the kernel-driver
  installer); the compose FFI surface is now actually compiled in CI.

Post-integration wave (PRs #41–#48, reviewed on the release branch):

- **PipeWire `stop()` no longer wedges for ~8 s.** Destroying a live,
  linked PipeWire stream from the dedicated loop thread blocked inside
  `pw_stream_destroy` (observed 7.8 s); teardown now disconnects, pumps
  the loop, and only then drops the stream.
- **Compose resampler flush on mid-stream rate renegotiation.** The
  natural-end tail flush shipped in the review batch above, but a
  *mid-stream* source rate change rebuilt the resampler without flushing
  the old one — losing the same ~25–45 ms (partial chunk + FFT delay
  residue) at every renegotiation. The old resampler is now flushed into
  the FIFO before the rebuild.
- **FFI: missing-consent errors are configuration errors.**
  `AudioError::UserConsentRequired` (Android playback capture built
  without a projection token) fell through `map_rsac_error`'s
  forward-compat arm to `RSAC_ERROR_INTERNAL`; it now maps to
  `RSAC_ERROR_CONFIGURATION` as `rsac.h` documents.
- **Android JNI: bridge start no longer leaks on GlobalRef failure.** If
  `NewGlobalRef` failed after `CaptureBridge.start()` succeeded, the Java
  read thread + `AudioRecord` kept running service-anchored; the failure
  path now clears the pending exception (JNI calls are illegal with one
  pending) and rolls back — stop, service unregister, local-ref delete.
- **macOS: silence-watchdog window widened past TCC grant-propagation
  latency.** A freshly granted permission streams all-zero buffers for a
  short window exactly like a denied one; the ADR-0016 watchdog no longer
  false-warns during it (verified on real hardware).

### Security

### C ABI changes

**Additive only — no breaking changes.** The `rsac-ffi` `compose` feature
(off by default) adds 29 new `rsac_*` symbols (`RsacGroup` /
`RsacCompositionBuilder` / `RsacComposition` handles, layout constants,
`RsacCompositionStats`/`RsacSourceStats` structs, overrun/knob/preflight
accessors), emitted in the generated header behind
`#if defined(RSAC_FEATURE_COMPOSE)`. Two prototypes changed from a C enum
parameter to `int32_t` (`rsac_group_set_layout`, `rsac_default_device`) —
ABI-identical on all supported targets (C enums are `int`-sized) and
C-source-compatible via implicit enum→int conversion; out-of-range values
now return `RSAC_ERROR_INVALID_PARAMETER` instead of being undefined
behavior. Existing symbol layouts are otherwise unchanged.

Further additive symbols (always compiled, no feature gate):
`rsac_builder_set_android_projection` (the Android consent-token carry,
an opaque `int64_t`; present on every platform for ABI uniformity,
returning `RSAC_ERROR_PLATFORM_NOT_SUPPORTED` off-Android), and the
capability-parity accessors —
`rsac_capabilities_supports_device_change_notifications`,
`rsac_capabilities_requires_user_consent`,
`rsac_capabilities_supported_sample_format_count`/`_at`,
`rsac_capabilities_min_sample_rate`/`max_sample_rate`, and
`rsac_capabilities_supported_sample_rate_count`/`_at`.

One error-code mapping fix (behavioral, not an ABI shape change): a
playback-capture build missing its consent token now returns the
documented `RSAC_ERROR_CONFIGURATION` instead of falling through to
`RSAC_ERROR_INTERNAL`.

## [0.4.0] - 2026-05-31

### Added

- **Windowed backpressure report (`backpressure_report()`).** A bounded,
  recent-window view of producer drop activity, complementing the lifetime
  counters in `stream_stats()`. Unlike the consecutive-drop
  `is_under_backpressure` flag (which resets on any successful push), the
  windowed `drop_rate` surfaces a *sustained* 1-in-N loss pattern. Backed by an
  alloc-free, lock-free fixed ring of `(pushed, dropped)` slots the producer
  advances on every push path (rsac-cfe4). Exposed across the whole surface:
  - Rust: `AudioCapture::backpressure_report() -> BackpressureReport`
    (`window`, `pushed`, `dropped`, `drop_rate`, `is_under_backpressure`).
  - C FFI: `rsac_capture_backpressure_report()` filling `RsacBackpressureReport`.
  - Go: `capture.BackpressureReport()`; Python: `capture.backpressure_report()`;
    Node: `capture.backpressureReport()`.
- **macOS spontaneous device-death detection (ADR-0010).** A
  `kAudioDevicePropertyDeviceIsAlive` listener on the captured device drives the
  bridge to a terminal `Error` state when a device/tap dies *without* a
  `stop()`/`Drop` (e.g. the interface is unplugged), so a parked blocking reader
  observes a fatal `StreamEnded` instead of hanging indefinitely (rsac-ead3).

### Changed

- `AudioCapture::backpressure_report()` now reports a *windowed* view with a
  populated `window` span (estimated from the buffer size and negotiated sample
  rate), instead of lifetime counters with a zero `window`. The public
  `BackpressureReport` shape is unchanged.

### Fixed

- `cargo doc --all-features` is clean again: cross-module/cfg-divergent doc
  references now use plain backticks rather than intra-doc method-path links
  that require the target type to be in scope.
- CI: the `softprops/action-gh-release` SHA is now unified across `release.yml`
  and `release-tag.yml` (#33); `subscribe()` integration coverage hard-asserts
  the 440 Hz tone content under a deterministic source (#34).

### C ABI changes

- **Added** (backward compatible — new symbol, no removals or layout changes):
  `rsac_capture_backpressure_report(const RsacCapture*, RsacBackpressureReport*)`
  and the `RsacBackpressureReport` value type (`window_secs`, `pushed`,
  `dropped`, `drop_rate`, `is_under_backpressure`). Existing symbols are
  unchanged, so consumers pinning the `.so`/`.dll`/`.dylib` need not recompile;
  this is a **MINOR** bump for the FFI surface.

## [0.3.0] - 2026-05-30

Two threads of work landed since 0.2.0. First, correctness-focused fixes from the
2026-05-29 deep-dive audit (waves 1–2) closed the real-time-safety,
callback-delivery, and error-classification findings. Second, a six-wave feature
program built out the capture-API surface — buffer metering, stream stats,
device-change watching, native PipeWire/CoreAudio enumeration, the `capture!`
macro and `rsac::prelude`, Python `abi3` wheels, and cross-platform Go CI — while
holding the capture-only scope (no DSP/mixing/resampling/encoding/playback) and the
RT-safety guarantee. Nine Architecture Decision Records now back these decisions:
[ADR-0001 (RT-allocation guarantee)](docs/designs/0001-rt-allocation-guarantee.md),
[ADR-0002 (callback delivery)](docs/designs/0002-callback-delivery.md),
[ADR-0003 (terminal stream error)](docs/designs/0003-terminal-stream-error.md),
ADR-0004 through ADR-0008 (device-watch threading & RAII teardown, the
[bridge-zerocopy `SampleRing`](docs/designs/0006-bridge-zerocopy-samplering.md)
data plane, period-derived ring sizing & `buffer_size` semantics, and the
CachePadded false-sharing mitigation),
and [ADR-0009 (tracing/log instrumentation shim)](docs/designs/0009-tracing-log-shim.md).
(See `docs/designs/` for ADR-0004–0008; sequential numbers are coordinated across
the parallel ADR set.)

### Added

- `AudioError::StreamEnded { reason }` (ADR-0003) — a `Fatal`, `ErrorKind::Stream`
  variant emitted when a read is attempted on a stream that has reached a terminal
  state (`Stopped` / `Closed` / `Error`). Distinguishes a clean end-of-stream from
  the recoverable `StreamReadError`, so read loops that break on `is_fatal()`
  terminate instead of busy-waiting. `AudioError` now has **22** variants (was 21).
- Push-callback delivery is now wired (ADR-0002): a callback registered via
  `set_callback` is invoked from a dedicated non-RT pump thread spawned by
  `start()` (mirroring `subscribe()`). The FFI trampoline is wrapped in
  `catch_unwind` so a panicking C callback cannot unwind across the boundary.
- **`AudioBuffer` level metering** — zero-allocation, RT-safe, `#[inline]` read-only
  observability metadata (not signal processing): `rms()`, `peak()`, `rms_dbfs()`,
  `peak_dbfs()`, plus the channel-strided `channel_rms()` / `channel_peak()`
  variants. NaN-safe; computed on demand from the buffer's existing samples.
- **`StreamStats` and `BackpressureReport`** (`#[non_exhaustive]`) observability
  snapshots, surfaced via `AudioCapture::stream_stats()`. `StreamStats` carries
  buffers captured/dropped/pushed, uptime, and a guarded `dropped_ratio`;
  `BackpressureReport` is assembled from inline atomics with zero-division guards
  and honestly documents its current lifetime-window limitation. Bridge counters
  (`buffers_captured` / `buffers_dropped` / `is_producing`) are exposed as default
  methods on the `CapturingStream` trait so every backend reports through the same
  path. A criterion bench (`benches/observability.rs`) proves the read path is
  cheap, non-locking, and alloc-free.
- **`CaptureTarget` string round-tripping**: `FromStr`, `TryFrom<&str>`, and
  `Display`, round-tripping the canonical forms `system` / `device:<id>` /
  `app:<pid>` / `name:<n>` / `tree:<pid>` (case-insensitive schemes; colon-split
  preserves device ids like `hw:0,0`).
- **Builder ergonomics & RAII** on `AudioCaptureBuilder`: `target_str(&str)` (parse
  a canonical target string — the CLI/config counterpart to `with_target`);
  `preflight()` (validate capabilities, the supported-sample-rate whitelist, and the
  channel range before device resolution); and `start() -> RunningCapture`, an RAII
  guard that `Deref`/`DerefMut`s to `AudioCapture` and stops the stream on `Drop`
  (idempotent; `into_inner()` escapes the guard without stopping).
- **`capture!` declarative macro** for one-line builder construction
  (`capture!(system)`, `capture!(app: pid)`,
  `capture!(device: id, rate: 48000, channels: 2)`, `target_str: "…"`).
- **`rsac::prelude`** module re-exporting the common surface — including `capture!`,
  `RunningCapture`, and `DeviceInfo` — so `use rsac::prelude::*;` is a one-import
  setup.
- **`DeviceInfo` + `AudioDevice::describe()`**: a `#[non_exhaustive]` device
  descriptor and an infallible `describe()` default method composed from the
  existing accessors; additive `Option<DeviceKind>` on `AudioSourceKind::Device`.
- **Device hot-plug / default-change watching (M10)**: `DeviceEvent`
  (`#[non_exhaustive]`; `DeviceAdded` / `DeviceRemoved` / `DefaultChanged` /
  `StateChanged`), the `DeviceWatcher` RAII guard (runs its backend teardown exactly
  once on `Drop`), `DeviceEventHandler`, and `DeviceEnumerator::watch()` (a provided
  trait method defaulting to `PlatformNotSupported`). Per-OS arms now back it:
  Windows registers an `IMMNotificationClient`, macOS an
  `AudioObjectAddPropertyListener`, and Linux a persistent PipeWire registry/metadata
  listener. The handler runs on the OS notification thread, never the RT audio
  callback thread (per-platform delivery model recorded in the device-watch ADR;
  `supports_device_change_notifications` reports honestly per backend).
- **Native, subprocess-free platform enumeration**: Linux enumerates devices and
  audio-active applications via an in-process PipeWire registry (replacing
  `pw-cli`/`pw-dump`, with the subprocess fallback retained) and populates
  `supported_formats()` from the node's `EnumFormat` params; macOS enumerates output
  devices with multi-format probing and filters application enumeration to processes
  actually emitting audio (macOS 14.4+, graceful pre-14.4 fallback).
- **Optional `tracing` instrumentation** (default off; ADR-0009): the `rsac_event!`
  and `rsac_span!` macros emit `tracing` events/spans with `--features tracing` and
  fall back to the always-present `log::` facade when off (a span degrades to an
  event). The feature pulls in only the `tracing` facade (no `tracing-subscriber`);
  `install_default_tracing()` is a best-effort, idempotent convenience for
  binaries/examples. Control-plane only — these macros are prohibited on the RT
  audio callback / sample-push path.
- **Bridge data plane**: `calculate_capacity_for_period(period_frames, channels)`, a
  pure function deriving ring capacity from the negotiated device callback period
  (backends adopt it later — see the ring-sizing ADR); an opt-in `bridge-zerocopy`
  feature providing a sample-domain SPSC `SampleRing` written via `rtrb` 0.3.4
  `write_chunk_uninit` + `CopyToUninit` (default off; A/B'd in `benches/bridge.rs` —
  see the bridge-zerocopy ADR for status and promotion criteria); and a criterion
  bench harness (`benches/bridge.rs`) for producer throughput and push→pop latency.
- **Cross-language binding parity** with the Rust ground-truth surface — Python
  (PyO3), Node (napi-rs), and Go (cgo) all gained `stream_stats()`/`format()`, buffer
  metering (`rms`/`peak`/`rms_dbfs`/`peak_dbfs` and channel variants),
  target-from-string, and context-manager / RAII ergonomics. Python ships a single
  CPython `abi3` (`abi3-py39`) wheel per platform covering 3.9–3.13; napi carries u64
  counters as `BigInt` and f32 samples as `Float32Array`; Go copies borrowed C
  buffers into Go memory before dispatch.
- `tests/rt_alloc.rs` (`CountingAllocator` harness proving `push_samples_or_drop` is
  alloc-free in steady state, ADR-0001) and `tests/enumeration_matrix.rs`
  (cross-platform "honest failure" enumeration + `DeviceInfo` round-trip contract,
  device-free in headless CI).

### Changed

- **BREAKING (SemVer): four public enums are now `#[non_exhaustive]`** —
  `AudioError`, `CaptureTarget`, `AudioSourceKind`, and `PermissionStatus`. They
  are expected to grow, so downstream `match` expressions on them must now carry a
  trailing wildcard (`_ =>`) arm; adding a variant in a future minor release will
  no longer be a breaking change. The deliberately **closed** enums —
  `SampleFormat`, `DeviceKind`, `ErrorKind`, and `Recoverability` — are documented
  as stable, exhaustively-matchable sets that will not grow. In-crate exhaustive
  matches (e.g. `AudioError::recoverability()` / `user_message()`) are unaffected
  and intentionally remain exhaustive to keep forcing classification of every
  variant.
- **Terminal-read signaling now crosses the C FFI** (fixes a Wave-B/Wave-C
  interaction): `rsac_capture_read` / `rsac_capture_try_read` now read via the
  terminal-observable path so a stopped stream surfaces the fatal
  `RSAC_ERROR_STREAM_FAILED` (from `StreamEnded`) once drained, instead of a
  recoverable `RSAC_ERROR_STREAM_READ`. Binding pumps (Go/Node) that branch on
  recoverability now end cleanly on stop instead of spinning.
- **RT producer is now allocation-free in steady state** (ADR-0001). Fixed the
  `push_samples_or_drop` scratch-shrink bug (the scratch `Vec` could collapse to
  capacity 0 and then re-allocate on the audio callback thread) and sized the
  seed/scratch buffers from a named worst-case-period constant so recycled buffers
  converge to zero allocation. Documented the guarantee precisely: allocation-free
  in steady state, with bounded one-time growth during warm-up or when the period
  grows.
- `PlatformCapabilities::query()` is now gated on the `feat_*` features so it
  cannot claim support for a backend that was not compiled in.
- `recoverability()` now uses an exhaustive `match` (no `_` catch-all): adding a
  new `AudioError` variant forces a compile error until its recoverability is
  classified deliberately.
- **CachePadded diagnostic atomics** (ADR-0007): the producer- and consumer-written
  diagnostic counters are wrapped in a `#[repr(align(64))]` `CachePadded<T>` newtype
  to kill false sharing on the RT push path. Transparent `Deref` keeps every call
  site unchanged; the `rt_alloc` gate confirms no new allocation.
- `PlatformCapabilities::SUPPORTED_SAMPLE_RATES` is now a public const (the single
  source of truth the builder `preflight` whitelist references).
- Bumped `wasapi` 0.22 → 0.23 (safe `WAVEFORMATEXTENSIBLE` blob parse,
  `get_device`/`get_device_format`), establishing a `rust-version` floor of 1.87.

### Fixed

- `buffers_iter()` no longer terminates prematurely on a transient empty read,
  and no longer drops buffers still queued in the ring after `stop()` — the
  buffered tail is drained before the iterator ends.
- `ChannelSink` is now bounded (back-pressure instead of unbounded memory growth),
  and `WavFileSink` finalizes the WAV header on drop.
- Removed the unsound manual `Send`/`Sync` impls on `AudioBuffer` (it is auto-`Send`/
  `Sync` via its fields) and replaced ad-hoc `eprintln!` diagnostics with the `log`
  facade.
- Removed a reachable `panic!` in the `AudioCapture` `Debug` impl; calling `start()`
  on an already-stopped stream now returns an error instead of misbehaving.
- Eliminated the consumer-side double-copy in the bridge `pop` path.
- Windows device-watch teardown pins its own `Arc<ComInitializer>` clone so the MTA
  apartment outlives the watcher even when the borrowed enumerator is dropped right
  after `watch()` returns.

### Deprecated

### Removed

### Security

### C ABI changes

**Additive only — no removals, renames, or layout changes to existing symbols.**
The `rsac-ffi` surface gained, in support of the binding-parity work above:

- `rsac_capture_stream_stats(capture, out: *mut RsacStreamStats) -> rsac_error_t`
  and `rsac_capture_format(capture, out: *mut RsacAudioFormat) -> rsac_error_t` —
  out-param accessors filling the new `#[repr(C)]` `RsacStreamStats` /
  `RsacAudioFormat` structs (both null-checked and `catch_unwind`-wrapped).
- `AudioBuffer` metering accessors over `RsacAudioBuffer`:
  `rsac_audio_buffer_rms`, `rsac_audio_buffer_peak`, `rsac_audio_buffer_rms_dbfs`,
  and `rsac_audio_buffer_peak_dbfs` (each returns `f32`; null-safe — the linear
  `rms`/`peak` accessors return `0.0` on a null buffer, while the `*_dbfs`
  accessors return `f32::NEG_INFINITY` (silence) on null, matching their
  silence-floor semantics).
- `rsac_builder_set_target_str(builder, spec: *const c_char) -> rsac_error_t` —
  set the capture target from a canonical target string.

The curated `rsac.h` and the vendored Go header were synced with these additions.
Because the changes only add symbols and `#[repr(C)]` types (no existing symbol
removed, renamed, re-signed, or re-laid-out), pinned `.so`/`.dll`/`.dylib`
consumers remain binary-compatible; consumers that want the new accessors recompile
against the updated `rsac.h`.

## [0.2.0] - 2026-04-18

Trait-level backpressure signaling, unified Linux backend errors, broader
`ci_audio` coverage (ApplicationByName + ApplicationByPID), and docs that
explain macOS enumeration scope. One breaking change on the device
enumerator (see Removed).

### Added

- `ci_audio` integration coverage for application-scoped capture:
  `application_by_name.rs` exercises `CaptureTarget::ApplicationByName`
  across backends, and `application_by_pid.rs` exercises
  `CaptureTarget::ApplicationByPID`, including a CI existence-check path
  so heterogeneous runners don't fail when the target app isn't present.
- README section describing macOS enumeration scope — clarifies that
  `list_audio_sources()` / `list_audio_applications()` return a superset
  (every GUI app) on macOS vs. the "currently producing audio" set that
  WASAPI session enumeration and PipeWire `pw-dump` yield on Windows and
  Linux, and that Process Tap attachment is the only way to observe the
  live per-process audio graph.

### Changed

- Relocated `is_under_backpressure()` from an inherent method on
  `BridgeStream<S>` to the `CapturingStream` trait, so every backend
  (WASAPI, PipeWire, CoreAudio) exposes backpressure signaling through
  the same dynamic-dispatch path used by `AudioCapture`. Callers should
  invoke it via `AudioCapture::is_under_backpressure()` or through the
  trait. (See Removed for the inherent-method deletion.)
- Linux device enumeration failures (cannot reach PipeWire via `pw-cli`
  or `pw-dump`) now return `AudioError::BackendError` with a descriptive
  message, matching the variants used by the Windows (WASAPI) and macOS
  (CoreAudio) backends. Callers pattern-matching for platform-specific
  recovery can now distinguish "backend busted" from "device really not
  there" on Linux. Cases where the backend is healthy but no matching
  device exists continue to return `AudioError::DeviceNotFound`.
- Strengthened `ci_audio` integration test assertions alongside the
  existing no-panic backbone: `test_stream_start_read_stop` now checks
  that returned buffers match the requested sample rate and channels and
  that `num_frames() * channels() == data().len()`;
  `test_capture_format_correct` asserts `overrun_count()` is monotonically
  non-decreasing across successive reads. The graceful-fallthrough
  pattern for heterogeneous CI hardware is preserved.
- Reorganized CI workflows into platform-specific files (`windows.yml`,
  `linux.yml`, `macos.yml`) plus a shared `code-quality.yml`, with
  reusable workflow components and a fixed PipeWire setup on Linux.
- `cargo fmt` baseline applied repo-wide; `clippy` cleanups across
  `src/core/introspection.rs` and backend code (including the Rust 1.95
  `collapsible_match` / `manual_checked_ops` lints) so the workspace
  builds cleanly on the pinned toolchain.

### Deprecated

- `CapturingStream::close()` is now a no-op default method. New code
  should rely on `stop()` plus `Drop` for stream teardown. The trait
  method is retained for one minor-version cycle to ease migration and
  will be removed in a future release.

### Removed

- **Breaking:** `CrossPlatformDeviceEnumerator::get_default_device()` no
  longer takes a `DeviceKind` argument — the parameter was silently
  ignored by every backend and has been dropped. Migration: remove the
  argument at the call site (e.g.
  `enumerator.get_default_device(DeviceKind::Output)` becomes
  `enumerator.get_default_device()`).
- **Breaking:** `BridgeStream::is_under_backpressure()` is no longer
  available as an inherent method; the trait-backed dispatch path via
  `CapturingStream::is_under_backpressure()` is now the only entry
  point. Migration: call through `AudioCapture::is_under_backpressure()`
  or bring `CapturingStream` into scope and invoke
  `stream.is_under_backpressure()` via the trait.

## [0.1.0]

Initial public pre-release: cross-platform audio capture with WASAPI
(Windows), PipeWire (Linux), and CoreAudio Process Tap (macOS 14.4+)
backends, ring-buffer data plane, and trait-based `CapturingStream` API.

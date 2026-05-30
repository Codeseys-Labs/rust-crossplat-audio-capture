# rsac architecture critique — 2026-05-30 (deep-work workflow, 136 agents, adversarially verified)

## rsac architecture critique — consolidated verdict

**Overall health: STRONG. Ship-worthy core, with a small set of genuine safety bugs and a CI/doc honesty gap to close before they bite.**

rsac is an unusually disciplined capture-only audio library. Across 11 adversarially-verified review dimensions, the **vision is faithfully respected in code** (no DSP/mixing/resampling/encoding crept in), the **RT-safety guarantee (ADR-0001) is real and regression-tested**, the **error taxonomy is exhaustively classified with no catch-all**, and the **device-watch + callback-pump machinery is mostly correct and thoughtfully isolated from the audio callback thread**. This is a mature codebase whose own docs are precise enough to be checked against — which is how most of these findings surfaced.

The findings that survived adversarial verification cluster into **five things that actually matter**:

1. **Two real memory-safety bugs in the bindings/backend FFI layer** (HIGH). The macOS device-watch teardown frees the listener context immediately after removing listeners, with no barrier for an in-flight proc (`coreaudio.rs:1116-1145` vs the proc deref at `:841`) — a use-after-free window. The Go `ReadBuffer`/`TryReadBuffer` release the mutex and then use a handle a concurrent `Close()` can free (`rsac.go:840-848`) — a UAF under concurrent close, with a code comment that actively claims it is safe. These are the only true soundness defects and should be fixed first.

2. **The single empirical proof of the headline RT-safety guarantee never runs in CI** (HIGH-impact gap). `tests/rt_alloc.rs` — the only test that proves zero heap allocation on the producer hot path — is an integration test, and CI runs only `cargo test --lib` / `--test ci_audio` / `--doc`. ADR-0001's guarantee is asserted but unguarded against regression. The honest-failure `enumeration_matrix.rs` is dead in CI for the same reason.

3. **A genuine module-DAG violation**: `core/introspection.rs` reaches *up* into `crate::audio::*` (4 call sites), breaking the `core depends on nothing internal` invariant stated in lib.rs, ARCHITECTURE.md, and AGENTS.md — and the module's own doc-comment relabels the layers to mask it. Surgical to fix (one file), but it is a documented invariant the code breaks.

4. **A cluster of "implemented-but-unwired" features that make docs over-claim** (mostly MEDIUM/LOW once verified): `pipe_to()`/`AudioSink` is documented but does not exist; period-aware ring sizing (`calculate_capacity_for_period`) and the zero-copy `SampleRing` plane are fully built and tested but called by no backend; buffer `timestamp()` is always `None` because every backend uses the non-timestamping push; `set_negotiated_format` is never called in production; `buffer_size` is honored only on Windows. None are unsafe, but each is a place where VISION/README/design-doc claims outrun shipped behavior.

5. **ADR drift and gaps**: ADR-0003's §5 FFI-mapping decision contradicts the (better) shipped mapping; the abi3 ADR shipped early but is still marked "Recommendation"; ADR-0001/0002 line refs are stale; and several load-bearing decisions have **no ADR at all** — most importantly the **per-platform device-watch threading model**, where Linux invokes the user handler directly on the PipeWire loop thread while Windows/macOS use a bounded-channel + helper-thread hand-off. That divergence is a behavioral-parity claim that needs to be either unified or documented.

The recurring theme is **honesty drift, not rot**: the engineering is sound, but recent feature waves landed code ahead of docs/CI/ADRs. Closing the two UAFs, wiring `rt_alloc` into CI, fixing the DAG edge, and reconciling docs/ADRs with what actually ships would put this library on very solid footing.


## Vision alignment

VERDICT: FAITHFUL. rsac genuinely serves the capture-only vision — this is the codebase's strongest dimension. A full read of the public surface (lib.rs, api.rs, core/*, sink/*) finds NO mixing, resampling, encoding (beyond the feature-gated downstream-adapter WAV sink), playback, VAD, or AEC anywhere; the explicit non-goals in VISION.md:89-102 are respected in code, not just docs. The one place DSP could creep in — core/processing.rs — is a 33-line empty trait fenced off with a module-level disclaimer. AudioBuffer level metering is correctly framed and implemented as read-only, alloc-free observability metadata, not signal processing. The CaptureTarget model matches the vision's headline differentiator one-to-one (the five VISION.md variants with round-trip FromStr/Display and a capture! macro), the 7-ErrorKind taxonomy matches VISION.md:84 exactly, platform-honesty is real (preflight()/build() gate unsupported targets and return PlatformNotSupported rather than silently failing), and a real downstream (audio-graph) deleted ~120 lines of per-platform discovery by adopting list_audio_sources() — confirming the seam is the right granularity and the capture/DSP division of labor (downstream owns rubato resampling, mixdown, chunking) is intended, not a gap.

CAVEATS (honesty drift, not scope creep): (1) A few vestigial surfaces lean toward a 'recorder' framing the vision disowns — AudioFileFormat (unused, names 'recording') and LatencyMode (documented as a StreamConfig field that does not exist); both should be deleted or deprecated (VS-1, VS-2). (2) The 'zero-copy ring buffer -> consumer' promise (VISION.md:69) is delivered only by the default-off bridge-zerocopy path; the shipped default is alloc-free-in-steady-state, so the wording should be tightened (PERF-02, VS-3 was refuted as the contract being owned-AudioBuffer-per-chunk). (3) Buffer timestamp() is always None because no backend timestamps — capture-side timing IS in scope for a capture library, so this is an under-delivery to fix or explicitly reserve (DF-01). None of these breach the non-goals; they are places where docs/claims outrun shipped behavior, consistent with the overall finding that the engineering is sound but recent feature waves landed ahead of their docs.


## Strengths (preserve)

- Capture-only scope discipline is genuinely excellent and verified in code, not just docs: no DSP/mixing/resampling/encoding/playback/VAD/AEC anywhere; core/processing.rs is an empty fenced-off extension point; level metering is correctly framed as read-only, alloc-free observability metadata.
- ADR-0001 RT-allocation guarantee is real and faithfully implemented: the free-list return ring keeps the producer alloc-free in steady state, the unavoidable user allocation is moved to the non-RT consumer thread, the H3 scratch-shrink defect is fixed on BOTH the success and ring-full arms, and it is locked in by named regression tests (scratch_never_shrinks_to_zero_after_underrun).
- The error taxonomy is unusually disciplined: recoverability(), kind(), user_message(), and Display are all EXHAUSTIVE matches with NO catch-all over 22 variants, driven off a single make_all_variants() helper so a new variant forces a compile error to classify it — backed by ~59 tests. is_fatal()/is_recoverable() give consumers a clean retry decision.
- Capability reporting is HONEST and verified against real implementations: query() gates on BOTH target_os AND the matching feat_* feature (H4 regression test locks this in), supports_device_change_notifications is backed by a real OS listener on all three platforms (not a stub), and the WASAPI I24 claim actually negotiates 24-valid-bits-in-32-bit and is correctly asserted absent on Linux/macOS.
- The ADR-0002 callback-delivery pump is correct and defensive: the pump OWNS the callback so no lock is held while the user closure runs (eliminating the re-entrancy deadlock + poison-on-panic class), it self-join-guards, only fatal errors break the loop, and the FFI trampoline is wrapped in catch_unwind so a panicking C callback cannot unwind across extern C.
- Lock-free concurrency design is sound: all producer/consumer coordination is atomic (AtomicStreamState, Relaxed diagnostic counters), CachePadded separates producer-hot from consumer-hot counters, the single-consumer SPSC invariant is preserved by serializing all read paths behind one Mutex<BridgeConsumer>, and the Windows COM-apartment lifetime fix (Arc<ComInitializer> in the teardown) is subtle and handled correctly with a regression test.
- Bindings are disciplined about FFI safety: every fallible exported fn null-checks pointers and wraps its body in catch_unwind, no panic=abort anywhere makes that sound, the Go cgo layer copies borrowed C buffers into Go memory before invoking the user fn and pins the ABI with discriminant tests, Python uses provably-sound f32::to_le_bytes (not the old from_raw_parts reinterpret), and napi carries u64 as BigInt and f32 as Float32Array to avoid precision loss.
- The !Send PipeWire idiom is used soundly (all Rc/RefCell loop objects stay on their dedicated thread; only owned snapshots cross the mpsc boundary; compile-time Send assertions guard the thread types), and caller-thread target resolution (pw-dump / /proc walk) is moved OFF the loop thread with recv_timeout handshakes so a wedged daemon surfaces as Timeout rather than an unbounded hang.
- The CI 'honest-skip' design is principled rather than hole-hiding: helpers gate by capability AND macOS TCC, Windows VB-CABLE PROMOTES non-silence/tone checks into HARD asserts with a fail-fast endpoint gate, content verification is a real Goertzel single-bin DFT with an 8x dominance threshold, and warmup_and_guard_player hard-panics if the source dies so 'source failed' cannot masquerade as 'capture broke'.
- The architecture is documented precisely enough to be testable against its own contracts (module DAG in lib.rs/ARCHITECTURE.md/ARCHITECTURE_OVERVIEW.md, ADRs in docs/designs/), which is itself a maturity signal — it is why most findings in this critique are pinpointable to a line rather than vague.
- CaptureTarget is a strong, well-tested handoff seam (case-insensitive schemes, colon-split that preserves device ids like hw:0,0, property-tested round-trip across all variants incl. u32::MAX/empty), and StreamStats/BackpressureReport are well-designed #[non_exhaustive] diagnostics with zero-division guards that honestly document the current lifetime-window limitation rather than faking windowed data.

## Confirmed findings (adversarially verified)


### [HIGH] (concurrency-threading) macOS device-watch teardown has a use-after-free window on the listener context

**Evidence:** src/audio/macos/coreaudio.rs:1116-1145 removes the property listeners and then immediately `drop(context)` with no barrier; the listener proc dereferences `&*(client_data as *const WatchListenerContext)` (coreaudio.rs:841) on a CoreAudio-managed thread. An AudioObjectRemovePropertyListener does not guarantee an in-flight proc has finished, so the proc can touch the freed WatchListenerContext (allocated at coreaudio.rs:780-787). The proc doc (coreaudio.rs:826-828) asserts 'the context outlives every listener', which overstates the actual guarantee. Verified in-tree.

**Recommendation:** Do not free the context immediately after removing the listeners. Either keep the WatchListenerContext alive for the process (drop only the SyncSender to stop delivery and intentionally leak/Arc the context with a comment), add a barrier that drains any in-flight proc before drop, or reference-count the context (Arc) and have the proc upgrade a Weak before touching it. At minimum, replace the overstated safety comment with the precise Apple guarantee being relied on.


### [HIGH] (bindings-ffi) Go ReadBuffer/TryReadBuffer use a capture handle a concurrent Close() can free (UAF)

**Evidence:** bindings/rsac-go/rsac.go:840-848 (ReadBuffer) snapshots `handle := c.handle`, unlocks the mutex (line 841), then calls C.rsac_capture_read(handle, &cbuf) (line 848) with no lock held; TryReadBuffer repeats the pattern (rsac.go:868+). closeLocked calls C.rsac_capture_free(c.handle) (rsac.go:741) which Box::from_raw-drops the AudioCapture (rsac-ffi/src/lib.rs:755-758), freeing the bridge/consumer a blocking read is touching. The comment at rsac.go:843-846 ('safe to use without the lock because Close() only frees the handle') is misleading. Verified in-tree.

**Recommendation:** Do not free the underlying capture while a read may be in flight. Hold c.mu across the C call, or add an in-flight-read counter/WaitGroup that Close() drains before rsac_capture_free, or apply the same clear-then-free-after-readers-quiesce discipline already used for the callback handle. Remove the misleading 'safe without the lock' comment.


### [HIGH] (adr-review) Device-watch threading model is inconsistent across platforms and has no ADR

**Evidence:** Windows (wasapi.rs:1139-1261) and macOS (coreaudio.rs:1011-1142) use a bounded sync_channel(64) + dedicated helper thread, so the user FnMut never runs on the OS notify thread, with drop-on-full backpressure. Linux (linux/thread.rs:2126-2230, doc :2136-2137) invokes the handler DIRECTLY on the PipeWire loop thread — no channel, no helper thread, no bounded backpressure. The trait doc (interface.rs:498-502) promises only 'OS notification thread, never the RT audio thread', so Linux is technically conformant, but the threading guarantee, the channel-bound (64), and the drop-on-full event-loss policy differ silently across platforms. No ADR records this load-bearing, behavioral-parity decision.

**Recommendation:** Write an ADR recording the device-change-notification delivery model: considered options (direct-invoke vs helper-thread hand-off; bounded vs unbounded; drop-oldest vs drop-newest; channel capacity), the chosen per-platform approach, and WHY Linux differs (PipeWire's !Send Rc loop objects make same-thread invocation natural). Either unify Linux onto the helper-thread+bounded-channel model for true parity, or document the divergence in the watch() trait contract and PlatformCapabilities so consumers can branch honestly.


### [HIGH] (test-coverage) rt_alloc.rs (the sole ADR-0001 alloc-free proof) is never executed in any CI job

**Evidence:** tests/rt_alloc.rs:164-266 is the only test empirically proving ADR-0001's headline zero-heap-allocation producer guarantee (process-wide CountingAllocator, 2000 steady-state cycles, bounded-one-time-growth assertion). It is a standard harness integration test (Cargo.toml:228-231). CI runs only `cargo test --lib` (ci.yml:101,128,182), `--test ci_audio`, and `--doc` (ci.yml:431) — verified by grep across all workflows. No `--test rt_alloc`, no bare `cargo test`, no `--tests`/`--all-targets` anywhere. The guarantee is asserted but unguarded against regression.

**Recommendation:** Add `cargo test --test rt_alloc` to the per-OS unit jobs (alloc behavior can differ by target, so run it per-platform). Also wire `cargo test --test enumeration_matrix`. Alternatively switch the default matrix to a form that includes integration tests (e.g. `cargo test --no-default-features --features feat_<os>`).


### [MEDIUM] (module-dag) core/introspection.rs reaches UP into the audio layer (core -> audio reverse DAG edge)

**Evidence:** src/core/introspection.rs:149 `crate::audio::get_device_enumerator()`, :191 `crate::audio::macos::enumerate_audio_applications()`, :208 `crate::audio::windows::enumerate_application_audio_sessions()`, :236 `crate::audio::linux::enumerate_audio_applications()`. These are genuine audio-layer backend fns. This violates the `core/ depends on nothing internal` invariant in docs/architecture/ARCHITECTURE_OVERVIEW.md:373/382, docs/ARCHITECTURE.md:10-16, and src/lib.rs:25-29. Compounding it, the introspection.rs module doc (lines 9-15) relabels core/bridge/audio as one lump called 'rsac core' to make the edge look conformant — contradicting the four-distinct-layer model in ARCHITECTURE.md:18-24.

**Recommendation:** Move list_audio_sources / list_audio_applications(_into) out of core into the audio (or api) layer, leaving only platform-agnostic data types (AudioSource, AudioSourceKind, PermissionStatus, CaptureTarget ctors) in core; core may define a discovery trait that audio/api implements, then re-export at the same lib.rs paths so the public surface is unchanged. If the edge is an accepted exception, amend the DAG docs + AGENTS.md to say so and record an ADR, and fix/delete the relabeling 'Separation of Concerns' note. Add a CI grep/cargo-modules guard so core->audio/bridge/api/sink edges fail the build.


### [MEDIUM] (api-ergonomics) AudioSink trait and pipe_to() are documented but pipe_to() does not exist; sinks are unwired

**Evidence:** docs/architecture/API_DESIGN.md:672-691 specifies `pub fn pipe_to<S: AudioSink + 'static>(&self, sink: S) -> AudioResult<()>` on AudioCapture, but grep for pipe_to|AudioSink in src/api.rs returns no matches. AudioSink/ChannelSink/NullSink/WavFileSink are prominently exported (prelude.rs:60, lib.rs:307-312). The crate's own examples/record_to_file.rs:43-80 uses hound::WavWriter directly with a hand-rolled read loop instead of the bundled WavFileSink — the library bypasses its own sink.

**Recommendation:** Implement AudioCapture::pipe_to (or RunningCapture::drain_to) reusing the existing callback-pump/subscribe thread machinery, and add an end-to-end example that uses WavFileSink. Then fix or remove the stale pipe_to section in API_DESIGN.md so the design doc matches reality.


### [MEDIUM] (downstream-fit) Buffer timestamps are always None in production; downstreams must re-derive time

**Evidence:** All backends call the non-timestamping push: windows/thread.rs:517, macos/thread.rs:284, linux/thread.rs:1762 -> push_samples_or_drop -> push_samples_or_drop_inner(..., None) -> AudioBuffer::new with no timestamp (ring_buffer.rs:599). A timestamping push_samples_or_drop_at exists (ring_buffer.rs:529) but is called by no production backend. AudioBuffer::timestamp() (buffer.rs:178) thus always returns None; the real downstream re-derives wall time itself (audio-graph capture.rs:510 sets `Some(start_time.elapsed())`). Capture-side timing is in-scope for a capture library (not DSP).

**Recommendation:** Wire the timestamping path through at least one backend with a documented contract (monotonic, stream-relative, populated-or-always-None). If hardware timestamps aren't cheap, populate a producer-side monotonic stamp at enqueue. Otherwise explicitly document that timestamp() is reserved/unpopulated so downstreams don't depend on it.


### [MEDIUM] (downstream-fit) Ring-buffer capacity / buffer_size is honored only on Windows; macOS and Linux hardcode calculate_capacity(None, 4)

**Evidence:** wasapi.rs:560 `calculate_capacity(config.buffer_size, 4)` honors the request; coreaudio.rs:495, macos/thread.rs:824, and linux/mod.rs:630 all use `calculate_capacity(None, 4)` (=64), ignoring config.buffer_size. config.rs:263 documents buffer_size as 'desired buffer size in frames' but it is consumed as a ring SLOT count (number of AudioBuffers). The period-aware calculate_capacity_for_period (ring_buffer.rs:1354) exists but no backend calls it.

**Recommendation:** Thread config.buffer_size into the macOS/Linux create_bridge calls, or adopt calculate_capacity_for_period uniformly once the negotiated period is known. Fix the buffer_size doc to say 'ring-buffer depth in buffers/slots' (or convert frames->slots explicitly). At minimum document that buffer_size is honored only on Windows.


### [MEDIUM] (downstream-fit) subscribe() has no end-of-stream / terminating-error signal; consumers infer death from a bare disconnect

**Evidence:** src/api.rs:1136 subscribe()'s reader thread breaks on any Err from try_read_chunk and drops the Sender (Err(_) => break at api.rs:1146), so the consumer only sees a channel Disconnected and cannot distinguish a clean stop from a fatal backend error (device unplug / StreamEnded vs InternalError). The real downstream treats Disconnected as 'stream ended' (audio-graph capture.rs:551). The error taxonomy has is_fatal()/Recoverability and read_buffer()/iterator surface AudioError, but subscribe() discards the terminating error.

**Recommendation:** Make subscribe() deliver the terminating AudioError — change the channel item to Result<AudioBuffer, AudioError> (matching the async-stream Item type at api.rs:1012), or expose a paired error channel / final stream_stats carrying the last error, so downstreams can reuse is_fatal()/Recoverability for restart-vs-surface decisions.


### [MEDIUM] (bindings-ffi) Python iterator __next__ raises StreamError on natural end-of-capture instead of StopIteration

**Evidence:** bindings/rsac-python/src/lib.rs:1101-1110 __next__ maps only StreamReadError to PyStopIteration; everything else (including StreamEnded) goes to audio_error_to_pyerr, which maps StreamEnded into the StreamError group (python/src/lib.rs:144-150). But the bridge emits StreamEnded (NOT StreamReadError) on terminal state (stream.rs:181-192), and read_buffer_blocking -> pop_blocking returns StreamEnded when the stream goes terminal during the wait (api.rs:954-971). Per ADR-0003, StreamEnded is the Fatal terminal signal. So Python's natural end-of-capture raises an exception instead of cleanly stopping iteration.

**Recommendation:** In __next__, treat AudioError::StreamEnded (and arguably StreamReadError for pre-start/transient) as StopIteration. Cleaner: branch on e.is_fatal()/recoverability() or ErrorKind::Stream-with-terminal so the iterator ends on any terminal stream signal rather than one enumerated variant.


### [MEDIUM] (api-ergonomics) AudioError is not #[non_exhaustive], unlike every other recently-added public type

**Evidence:** src/core/error.rs:107-108 `#[derive(Debug)] pub enum AudioError {` — 22 variants, no #[non_exhaustive]. Contrast: UserFacingError (error.rs:526), StreamStats (introspection.rs:322), BackpressureReport (introspection.rs:382), DeviceInfo (interface.rs:49), DeviceEvent (interface.rs:363) ARE non_exhaustive. The doc (error.rs:105) and the deliberately-exhaustive recoverability()/user_message() matches signal the variant set is expected to grow — making the missing attribute a future SemVer hazard for downstream exhaustive matches.

**Recommendation:** Add #[non_exhaustive] to AudioError and document that external code must use a trailing `_ =>` arm and rely on kind()/recoverability()/is_fatal() for classification. Internal exhaustive matches keep compiling since #[non_exhaustive] does not force `_` within the defining crate.


### [MEDIUM] (bindings-ffi) cbindgen-generated rsac_generated.h emits incompatible (double-prefixed) names; the documented regen command would clobber the curated rsac.h and break the Go/C ABI

**Evidence:** bindings/rsac-ffi/include/rsac_generated.h:114 `RsacRsacBuilder`, :22 `Rsacrsac_error_t`, :217 `Rsacrsac_audio_callback_t` vs the curated bindings/rsac-ffi/include/rsac.h:100 `RsacBuilder`, :45 `rsac_error_t`, :125 `rsac_audio_callback_t`. cbindgen.toml:17 `prefix = "Rsac"` double-applies because only some types are in [export.rename]. README.md:161-163 documents regenerating to `--output .../include/rsac.h` (NOT rsac_generated.h). Go links against the curated names (rsac-go/rsac.h:107, rsac.go:247 `*C.RsacBuilder`), so running the documented command would overwrite rsac.h with incompatible names and break the ABI.

**Recommendation:** Either make cbindgen produce the curated names (drop the global prefix / extend [export.rename] for the opaque structs and callback/enum typedefs) so a regen is a no-op, or delete rsac_generated.h, declare rsac.h the single hand-maintained source of truth, and have build.rs emit the generated header to OUT_DIR for diffing only. Fix the README regen command so it can never target rsac.h.


### [LOW] (api-ergonomics) Builder read methods take &mut self, contradicting the 'AudioCapture is Send+Sync, shareable behind Arc' claim

**Evidence:** src/api.rs:9-13 doc says AudioCapture is Send+Sync and shareable behind an Arc, but read_buffer (api.rs:930), read_buffer_blocking (:954), buffers_iter (:974) take &mut self, while subscribe (:1113), is_under_backpressure (:1176), stream_stats (:1227) take &self. The underlying CapturingStream::try_read_chunk is already &self (interface.rs:233), and API_DESIGN.md:704 states reads should be &self via interior mutability. The &mut read path forces a Mutex on Arc-sharing consumers.

**Recommendation:** Change read_buffer/read_buffer_blocking/buffers_iter to &self (the stream methods are already &self; only start/stop truly need &mut). If &mut is only needed for callback-pump fields, gate those behind interior mutability as start()/stop() already do.


### [LOW] (bindings-ffi) napi data-pump treats every read error (including Recoverable StreamReadError) as terminal

**Evidence:** bindings/rsac-napi/src/lib.rs:716-719 pump loop does `Err(_) => { break; }` on any error from read_buffer(). read_buffer returns Err(StreamReadError{reason:'Stream is not running'}) in transient/pre-start conditions (api.rs:930-948), which is Recoverable per ADR-0003 (only StreamEnded is Fatal). So a transient hiccup silently kills push delivery to JS.

**Recommendation:** Branch on the error's recoverability: continue (optionally after a short backoff) on Recoverable errors and only break on Fatal/StreamEnded. Surface terminal-stream errors to JS (the TSFN is ErrorStrategy::Fatal) rather than silently dropping the pump.


### [LOW] (error-capability-model) Two expect() panics in the lock-free stream-state read path violate the no-panics-in-library invariant

**Evidence:** src/bridge/state.rs:106 (get()) and :127-128 (transition()) call StreamState::from_u8(raw).expect('AtomicStreamState contains invalid discriminant'). The only writers store `state as u8` from a valid #[repr(u8)] enum (state.rs:96-99, 29-43) so from_u8 cannot return None — but AGENTS.md/CLAUDE.md mandate 'no panics in library code'. This is the only non-test, non-doc, non-FFI-boundary panic path found in core/bridge.

**Recommendation:** Replace the two expect() with a debug_assert!-plus-fallback (e.g. unwrap_or(StreamState::Error)), or record them in an explicit no-panics exceptions list with the soundness argument.


### [LOW] (test-coverage) Three high-value tests are dead in CI: enumeration_matrix, AsyncAudioStream behavior, and binding-tier marshaling

**Evidence:** (1) tests/enumeration_matrix.rs (honest-failure contract, self-skipping) is declared (Cargo.toml:238-241) but named in no workflow (default matrix is --lib). (2) Binding Rust tests re-implement mappings instead of calling binding fns (rsac-ffi/src/lib.rs:1496-1524 stream_stats_struct_round_trip 'mirrors' the mapping; napi tests call rsac core directly, lib.rs:859-992), and no workflow runs `cargo test -p rsac-ffi/-napi/-python` (ci.yml:289-298 is cargo check only); Go runs only make test-pure. (3) The real PlatformStream backends are tested only via MockPlatformStream (stream.rs:346-367); the trait<->backend seam is unverified off-device.

**Recommendation:** Run `cargo test --test enumeration_matrix` in per-OS unit jobs; run `cargo test -p rsac-ffi -p rsac-napi -p rsac-python` in check-bindings (the FFI null-pointer tests are device-free); add a minimal language-tier smoke test per binding; and add a backend-level fake at the OS-callback boundary so the trait<->backend seam is covered without a device.


### [LOW] (test-coverage) Windows unit-test job is continue-on-error, so watch/WASAPI unit-test regressions don't gate CI

**Evidence:** .github/workflows/ci.yml:123-128 runs the Windows unit step with continue-on-error: true (justified by absent AudioSrv on the runner). But the four device-free DeviceWatcher lifecycle tests (wasapi.rs:2203-2294) and ~40 inline wasapi.rs tests + 21 thread.rs tests run in exactly this advisory step, so their pass/fail does not gate CI.

**Recommendation:** Split device-touching Windows tests from pure-logic ones; run the device-free subset (the watch lifecycle tests) WITHOUT continue-on-error via a --skip/filter, keeping only true device-open tests advisory. Consider an injectable event source so watch delivery can be positively asserted.


### [LOW] (test-coverage) Non-ignored macOS CoreAudio FFI tests run unguarded in the unit job — latent 10+ minute hang/flake

**Evidence:** src/audio/macos/tap.rs:1352 test_new_system_creates_tap is NOT #[ignore]d and calls CoreAudioProcessTap::new_system -> AudioHardwareCreateProcessTap; tap.rs:1214/:1236/:1248 are likewise non-ignored. The project's own docs (ci_audio/helpers.rs:571-581) note AudioHardwareCreateProcessTap can block 10+ minutes without kTCCServiceAudioCapture. ci.yml:181-182 runs `cargo test --lib --features feat_macos` with no gtimeout and no continue-on-error (unlike ci-audio-tests.yml:870-877 which wraps the family in gtimeout 120).

**Recommendation:** Mark device/permission-touching macOS tests #[ignore] (matching the 21 already in coreaudio.rs) and add an existence-wiring check; or gate them on RSAC_CI_MACOS_TCC_GRANTED and wrap the macos unit step in gtimeout.


### [LOW] (bindings-ffi) rsac_version() hardcodes "0.1.0" while the crate and sibling bindings are 0.2.0

**Evidence:** bindings/rsac-ffi/src/lib.rs:1411-1414 returns static b"0.1.0\0". rsac-ffi/Cargo.toml:3 is version 0.1.0 but depends on rsac 0.2.0 (:37); workspace crate, rsac-python, rsac-napi are all 0.2.0; Python __version__ uses env!(CARGO_PKG_VERSION). So the C/Go ABI reports a wrong version string.

**Recommendation:** Return env!(CARGO_PKG_VERSION) from rsac_version() (NUL-terminate via concat!/CStr or a once-init CString) and bump rsac-ffi's package version to 0.2.0 to align with the workspace.


### [LOW] (performance) Period-derived ring sizing and the zero-copy SampleRing plane are implemented and tested but called by no backend

**Evidence:** calculate_capacity_for_period (ring_buffer.rs:1354-1382, tests :2426-2497) has zero call sites outside its own tests — backends use the static calculate_capacity (wasapi.rs:560, linux/mod.rs:630, macos/thread.rs:824, coreaudio.rs:495). The bridge-zerocopy SampleRing (ring_buffer.rs:979-1065, one memcpy into MaybeUninit, no alloc) is gated #[cfg(feature=bridge-zerocopy)] and wired only to the benchmark; all backends use the AudioBuffer ring. VISION.md:69-70 promises 'zero-copy (ring buffer -> consumer without intermediate Vec)', which only the unwired path delivers; the default path is alloc-free-in-steady-state, not zero-copy.

**Recommendation:** Wire calculate_capacity_for_period into backends once the OS period is known, and either wire SampleRing into the interleaved-f32 backends (PipeWire/CoreAudio) behind its feature and measure the win, or soften VISION wording to 'alloc-free producer in steady state' so docs match the default path.


### [LOW] (performance) WASAPI hot path performs three copies and an O(n) make_contiguous rotation per packet

**Evidence:** src/audio/windows/thread.rs:483 copies OS bytes into a reused VecDeque (copy 1); :504 `make_contiguous()` rotates the VecDeque (O(n)); :508-511 a scalar per-sample loop does f32::from_le_bytes + push (copy 2, element-at-a-time, no bulk reinterpret); ring_buffer.rs:585 then extend_from_slice (copy 3). Linux already does the right thing with a bulk align_to::<f32>() reinterpret (linux/thread.rs:1756).

**Recommendation:** Mirror Linux: yield a contiguous &[u8] and reinterpret via align_to::<f32>() (or bytemuck::cast_slice) into the producer in one bulk copy, eliminating the scalar loop, the second Vec, and ideally the VecDeque. Add the WASAPI conversion to the benchmark suite.


### [LOW] (performance) subscribe() push-delivery busy-polls with a 1ms sleep instead of using the existing waker

**Evidence:** src/api.rs:1135-1149 subscribe()'s thread loops try_read_chunk() and on Ok(None) does std::thread::sleep(1ms). An atomic_waker is already woken by the producer on every push (ring_buffer.rs:294-296, 451, 609) and pop_blocking exists (ring_buffer.rs:880); the async path already uses wake-on-push (async_stream.rs:60-110). subscribe() adds up to 1ms latency for no reason.

**Recommendation:** Drive the subscribe thread off pop_blocking or the waker so delivery is wake-on-push with no steady-state polling. At minimum document the up-to-1ms added latency.


### [LOW] (performance) set_negotiated_format is never called by a backend, so format()/StreamStats report the requested (not delivered) format

**Evidence:** All set_negotiated_format call sites are in bridge tests and the api.rs mock — no src/audio/* backend calls it (linux/thread.rs:1680-1693 explicitly defers it with a comment to trust AudioBuffer metadata meanwhile). negotiated_format() falls back to the requested format whenever the atomic is unset (ring_buffer.rs:389-397), i.e. always in production.

**Recommendation:** Call producer.set_negotiated_format(&delivered) from each backend's negotiation point (the Linux param_changed callback already has the negotiated rate/channels and the producer in scope). Until then, narrow any doc claim that format() returns the negotiated delivery format.


### [LOW] (adr-review) ADR-0003 §5 FFI-mapping decision contradicts the shipped (and better) mapping

**Evidence:** docs/designs/0003-terminal-stream-error.md:68-70 records 'chosen: reuse RSAC_ERROR_STREAM_READ group to avoid ABI churn'. The code instead maps StreamEnded -> RSAC_ERROR_STREAM_FAILED (the Fatal group), explicitly NOT the recoverable STREAM_READ group, with a justifying comment (rsac-ffi/src/lib.rs:172-176). The code is more correct, but the ADR text now contradicts shipped behavior.

**Recommendation:** Amend ADR-0003 §5 to record the actual decision (map to the Fatal STREAM_FAILED group) and the rationale already present in the code comment. One-line consequence edit.


### [LOW] (adr-review) abi3 ADR Status/sequencing no longer matches shipped code

**Evidence:** docs/designs/abi3-decision.md:8 Status is still 'Recommendation' and §5 sequences adoption as 'rsac-python v0.3.0 / keep per-version for v0.2.0'. But abi3-py39 is already enabled (rsac-python/Cargo.toml:20) and release-pypi.yml builds a single cp39-abi3 wheel (:105-118), while Cargo.toml:3 and pyproject.toml:7 are still 0.2.0 — i.e. abi3 was adopted DURING the 0.2.0 line, not post-v0.2.0 as planned.

**Recommendation:** Promote the ADR to Accepted/Implemented, update §5 to record that abi3-py39 shipped within 0.2.0 (and why the post-v0.2.0 gating was dropped), and check the §6.4 verification boxes the CI smoke test now covers (release-pypi.yml:120-129).


### [LOW] (adr-review) ADR-0001 and ADR-0002 line references are stale after the push-path refactor

**Evidence:** docs/designs/0001-rt-allocation-guarantee.md:5/:87 cite ring_buffer.rs:173-206 but the implemented logic is in push_samples_or_drop_inner at ring_buffer.rs:567-647. docs/designs/0002-callback-delivery.md:75-76 cite api.rs:577-610/:398-429/:631-675 and ffi:184-227, but set_callback is at api.rs:1029-1048, the start pump at :784-810, FFI invoke at rsac-ffi/src/lib.rs:294-317.

**Recommendation:** Refresh the Scope/References line ranges (or switch to symbol references that survive refactors) when the ADRs are next touched; bundle with the ADR-0003/abi3 edits.


### [LOW] (error-capability-model) FFI error map collapses semantically distinct variants into one C code (lossy at the boundary)

**Evidence:** bindings/rsac-ffi/src/lib.rs:169-176 maps StreamCreationFailed | StreamStartFailed | StreamStopFailed | StreamEnded all to RSAC_ERROR_STREAM_FAILED; :166-168 collapses DeviceNotFound | DeviceNotAvailable | DeviceEnumerationError to RSAC_ERROR_DEVICE_NOT_FOUND; :177-179 collapses StreamReadError | BufferOverrun | BufferUnderrun to RSAC_ERROR_STREAM_READ. The map is correctly exhaustive (no _ =>), so this is fidelity, not soundness. (Note: one verifier refuted this as an intentional ABI-stability tradeoff per ADR-0003; recorded as low for fidelity tracking.)

**Recommendation:** Either add distinct codes (RSAC_ERROR_STREAM_ENDED, RSAC_ERROR_DEVICE_UNAVAILABLE) or expose a companion rsac_error_recoverability(rsac_error_t) -> int accessor so FFI consumers can recover the Recoverability classification without parsing the collapsed code.


### [LOW] (vision-scope) Vestigial AudioFileFormat enum exposes a recording/encoding surface — an explicit non-goal

**Evidence:** src/core/config.rs:358-364 defines `pub enum AudioFileFormat { #[default] Wav }` with a doc 'Specifies the audio file format for recording.' and it is re-exported (core/mod.rs:26-29). Grep shows it is never constructed or consumed anywhere. VISION.md:89-102 lists Encoding (MP3/AAC/Opus) as out of scope (use hound/symphonia/opus).

**Recommendation:** Remove AudioFileFormat and its core::mod.rs re-export (encoding/format selection belongs to the downstream sink, which already has the feature-gated WavFileSink). If kept for compat, mark #[deprecated] like DeviceSelector and reword the doc to stop describing rsac as a recorder.


### [LOW] (vision-scope) LatencyMode is documented as a StreamConfig field that does not exist; the enum is unused dead surface

**Evidence:** src/core/config.rs:9-12 module docs claim StreamConfig 'carries ... latency mode', but the actual StreamConfig fields (config.rs:255-270) are sample_rate/channels/sample_format/buffer_size/capture_target — no latency_mode. LatencyMode (config.rs:316-325) is defined and re-exported (mod.rs:28) but has zero other references: never set, read, or wired into the builder.

**Recommendation:** Either wire LatencyMode into StreamConfig + a builder setter that backends honor as a hint, or (preferred for capture-only minimalism) delete the enum, its re-export, and the 'and latency mode' clause in the config.rs module docs so docs match code.


### [OBSERVATION] (api-ergonomics) Public enums are inconsistently #[non_exhaustive]: every public struct is, almost no public enum is

**Evidence:** NOT non_exhaustive: CaptureTarget (config.rs:54, explicitly a growing set per VISION.md:43-51), SampleFormat (config.rs:191), DeviceKind (interface.rs:19), ErrorKind (error.rs:26), Recoverability (error.rs:52), AudioSourceKind (introspection.rs:41), PermissionStatus (introspection.rs:266). Only DeviceEvent (interface.rs:363) is non_exhaustive among enums.

**Recommendation:** Decide a policy and apply it uniformly. At minimum mark CaptureTarget, AudioSourceKind, and PermissionStatus #[non_exhaustive] (most likely to gain variants); SampleFormat/DeviceKind/ErrorKind/Recoverability are defensible to leave closed if stability is intended — but state that intent in the doc.


### [LOW] (api-ergonomics) DeviceEnumerator::default_device() vs the called CrossPlatformDeviceEnumerator::get_default_device() — two names for one operation

**Evidence:** Trait method is default_device() (interface.rs:494) but the concrete facade exposes get_default_device() (audio/mod.rs:97), and everything user-facing uses the get_ form (README.md:107, examples/list_devices.rs:65, api.rs:351/382, napi:782, ffi:1163). enumerate_devices() is consistent across both, making default_device the lone divergence.

**Recommendation:** Pick one name. Preferred: rename the facade to default_device() to match the trait and enumerate_devices(), keeping get_default_device() as a #[deprecated] alias for one release so callers/bindings don't break.


### [LOW] (concurrency-threading) subscribe() reader thread is detached with no JoinHandle — not tracked or joined by stop()/Drop, unlike the callback pump

**Evidence:** src/api.rs:1133-1154 subscribe spawns 'rsac-subscribe' and discards the JoinHandle; stop() (api.rs:881-907) and Drop (api.rs:1358-1380) join only the callback_pump. The thread terminates on detecting stream stop (api.rs:878-880) but is never reclaimed by the lifecycle, inconsistent with the ADR-0002 pump story.

**Recommendation:** Track subscriber threads like the pump (a shared stop flag + JoinHandle vec) so stop()/Drop can signal and best-effort join them, OR prominently document that stop()/Drop does not reclaim subscriber threads and that holding a Receiver pins the stream. Unify the lifecycle story with ADR-0002.


### [LOW] (api-ergonomics) README/lib.rs quick-start read loops mishandle Ok(None)/recoverable errors, setting a footgun template

**Evidence:** README.md:79-85 is `loop { if let Some(buffer) = capture.read_buffer()? { ... } }` — no break, no Ok(None) sleep, and `?` makes a transient (Recoverable) StreamReadError terminate the whole function. lib.rs:56 doc-test uses `while let Some(buffer) = capture.read_buffer()? {}` which exits on the first Ok(None). The correct pattern is in examples/basic_capture.rs:48-74 and the iterator (api.rs:1336).

**Recommendation:** Align the README and lib.rs doc loops with examples/basic_capture.rs: handle Ok(None) with a short sleep, break on Err(e) if e.is_fatal(), retry on recoverable errors.


### [OBSERVATION] (api-ergonomics) Crate's own examples/README reimplement RMS by hand instead of the new RT-safe metering API

**Evidence:** buffer.rs:298 rms()/:321 peak()/:339 rms_dbfs()/:349 peak_dbfs() exist and are RT-safe, but examples/basic_capture.rs:54 computes a hand-written rms_level(buffer.data()) and README.md:55 hand-rolls 20.0 * rms.log10() instead of buffer.rms_dbfs().

**Recommendation:** Update basic_capture.rs/README to call buffer.rms_dbfs()/buffer.peak_dbfs() — a docs/example fix that immediately demonstrates the new surface.


### [LOW] (downstream-fit) Format-negotiation logic is duplicated verbatim in the real downstream; its non-Linux-only nature is undocumented

**Evidence:** api.rs:587 pick_supported_format defines a preference order; api.rs:402 gates negotiation #[cfg(not(target_os=linux))] (Linux build() does not negotiate). audio-graph capture.rs:88 reimplements the identical preference order and capture.rs:128 re-enumerates the device before build(); the downstream comment (capture.rs:71-77) defends against now-false 'rsac hard-errors on a miss' behavior.

**Recommendation:** Expose negotiation as a public pre-build query (AudioCaptureBuilder::negotiated_format() or a PlatformCapabilities/DeviceInfo helper) so downstreams get the resolved format without re-enumerating; document that build() negotiates (rather than hard-fails) and that Linux negotiates at stream-open.


### [LOW] (downstream-fit) AudioCapture lacks a compile-time Send+Sync assertion; the real downstream architects around a wrong !Sync assumption

**Evidence:** api.rs:9 module doc claims AudioCapture is Send+Sync (and the fields support it), but there is no _assert_send_sync::<AudioCapture>() in api.rs, unlike AudioBuffer (buffer.rs:430) and BridgeStream (stream.rs:330). The real downstream believes it is !Sync (audio-graph capture.rs:147/:369 'rsac::AudioCapture is !Sync') and builds thread-per-source around that.

**Recommendation:** Add a const Send+Sync assertion in api.rs (mirroring buffer.rs:430) and state the Send+Sync guarantee at the type level (not just module level), so the contract is enforced and discoverable and downstreams stop guessing.


### [OBSERVATION] (vision-scope) AudioProcessor trait docs ('apply effects/transformations') sit at the edge of the no-DSP boundary

**Evidence:** src/core/processing.rs:1,17-19 doc the trait for 'in-flight audio transformations ... apply effects, analyze audio'. Mitigating: processing.rs:8-10 disclaims bundling DSP and points at VISION.md, the trait is empty with no implementors, and buffer.rs:419 as_mut_slice reinforces that transformation is a consumer concern.

**Recommendation:** Keep the trait but reword its docs to emphasize observation/forwarding/analysis over 'apply effects/transformations', and record an ADR that AudioProcessor is a non-bundled extension point rsac will never populate with DSP, so the boundary is decision-backed.


### [OBSERVATION] (error-capability-model) BackendError is classified TransientRetry but is used for non-transient listener-registration failures

**Evidence:** error.rs:251-253 classifies BackendError as TransientRetry and error.rs:410-411 user_message says 'may be transient — retry'. But coreaudio.rs:1057-1069 returns BackendError when AudioObjectAddPropertyListener fails and wasapi.rs:1161-1169 when RegisterEndpointNotificationCallback fails — registration/config failures that are not necessarily transient.

**Recommendation:** Acceptable as-is (recoverability is a hint). If tightened, route genuinely non-transient backend failures through BackendInitializationFailed (already Fatal) or carry a transient: bool in BackendContext. Low priority.


### [OBSERVATION] (concurrency-threading) macOS listener proc re-enters CoreAudio (device id/name/kind queries) on the notification thread

**Evidence:** coreaudio.rs:872-906 emit_device_list_diff calls current_device_id_set -> get_audio_device_ids plus device_kind_of(id)+device.name() per added device; :911-919 emit_default_changed calls get_default_device_id — all on the CoreAudio notification thread, while the proc doc (:816-823) warns it must not re-enter CoreAudio in a blocking way.

**Recommendation:** Either move id->name/kind resolution to the helper thread (notify thread only diffs ids) or relax/clarify the proc doc to state that bounded, non-waiting property queries are intentionally permitted. Pick one so comment and code agree.


### [LOW] (rt-safety) push_samples_guarded (the FFI-boundary panic guard) is dead code; all three audio callbacks call the unguarded push

**Evidence:** macos/thread.rs:284, linux/thread.rs:1762, windows/thread.rs:517 all call producer.push_samples_or_drop directly. Grep shows push_samples_guarded (ring_buffer.rs:666) has zero production call sites (only its def + tests). Its own doc (ring_buffer.rs:649-665) says it is 'for use directly at the OS audio-callback boundary' because an unwind across FFI is UB.

**Recommendation:** Use push_samples_guarded at the macOS and PipeWire FFI-callback sites where an unwind crosses into C, OR — if the inner push is provably panic-free — delete push_samples_guarded and document that decision in ADR-0002/0003 so the 'FFI boundary needs a guard' contract is not left half-implemented.


## ADR review


- **0001-rt-allocation-guarantee.md** — _sound (line-refs stale)_: No decision change. Refresh stale Scope/References line ranges (cites ring_buffer.rs:173-206; implemented logic now at push_samples_or_drop_inner :567-647) — prefer symbol references over line numbers. Bundle with the other ADR doc edits.

- **0002-callback-delivery.md** — _sound (line-refs stale)_: No decision change. §6 line refs (api.rs:577-610/:398-429/:631-675, ffi:184-227) are stale relative to current set_callback (api.rs:1029-1048), start pump (:784-810), FFI invoke (rsac-ffi/src/lib.rs:294-317). Refresh refs. Optionally extend to record the subscribe() vs pump lifecycle asymmetry (CT-2).

- **0003-terminal-stream-error.md** — _needs-update_: Decision (add StreamEnded, Fatal, ErrorKind::Stream, exhaustive recoverability) is fully implemented. Amend §5: the shipped FFI mapping is StreamEnded -> RSAC_ERROR_STREAM_FAILED (Fatal group), NOT the recoverable STREAM_READ group as the ADR text records — update to the actual (better) decision with the code-comment rationale.

- **abi3-decision.md** — _needs-update_: Recommendation already EXECUTED (abi3-py39 enabled in rsac-python/Cargo.toml:20; single cp39-abi3 wheel in release-pypi.yml) but within the 0.2.0 line, not post-v0.2.0 as §5 planned. Promote Status to Accepted/Implemented, correct §5 sequencing/version note, and check the §6.4 verification boxes the CI smoke test now covers.

- **GAP: device-watch threading model (per-OS watch() delivery)** — _gap-missing-adr_: Write a new ADR. Record the device-change-notification delivery model: direct-invoke (Linux, PipeWire loop thread) vs bounded-channel + helper-thread hand-off (Windows/macOS), bounded vs unbounded, drop-oldest vs drop-newest, channel capacity (64), and WHY Linux differs (!Send Rc loop objects). Either unify Linux for true parity or document the divergence in the watch() trait contract + PlatformCapabilities. This is the highest-priority ADR gap.

- **GAP: bridge-zerocopy SampleRing alternative data plane** — _gap-missing-adr_: Write an ADR (or design note). Record why a parallel SampleRingProducer/Consumer plane exists, its default-off A/B-only status, the two-ring all-or-nothing commit/metadata-desync invariant, the rtrb 0.3.4 pin for write_chunk_uninit/CopyToUninit, and the criteria for promoting it into backends or removing it — so it does not rot as undocumented near-dead surface.

- **GAP: calculate_capacity_for_period sizing model + buffer_size semantics** — _gap-missing-adr_: Write an ADR (or design note). Record the period-derived ring sizing model (12-period headroom, sub-reference scale-up, clamp 8..=1024, pow2 rounding, tuned constants), why the channels parameter is accepted-but-ignored (`let _ = channels`), and why backends currently use the static calculate_capacity instead (DF-04/PERF-01). Clarify that buffer_size is a ring slot count, not frames, and that it is honored only on Windows today.

- **GAP: CachePadded false-sharing mitigation** — _gap-missing-adr_: Write a short ADR/design note. Record the choice of a hand-rolled #[repr(align(64))] CachePadded newtype over crossbeam::CachePadded, the 64-vs-128-byte choice (x86-64/aarch64/Apple-silicon rationale), and that the padding is currently partial (the per-push drop_window atomics + cursor are unpadded). Lower priority than the watch gap.

- **GAP: tracing/log dual-backend fallback + RT-path prohibition** — _gap-missing-adr_: Write a short ADR/design note. Record the rsac_event!/rsac_span! dual-backend shim (tracing on, log tt-muncher off, span->event degradation), the deliberate dependency on `tracing` only (no tracing-subscriber, NoSubscriber default), and the RT-path prohibition contract on these macros. Low priority.

- **GAP: DeviceWatcher RAII teardown / lifecycle contract** — _gap-missing-adr_: Write a short ADR/design note (can fold into the device-watch threading ADR). Record the take-once Option<Box<dyn FnOnce>> teardown, Drop best-effort no-panic join-on-drop, the platform-specific self-join-from-inside-the-callback guard (CallbackPump api.rs:693-712), and the Windows COM-apartment Arc<ComInitializer> lifetime fix. Currently scattered across doc-comments.

- **GAP: AudioProcessor as a non-bundled, never-populated DSP extension point** — _gap-missing-adr_: Write a short ADR. Record that AudioProcessor is an intentional extension point rsac will NEVER ship DSP implementors for, so the capture-only no-DSP boundary (VS-4) is decision-backed rather than only doc-asserted. Optional/low priority.


## Top actions (prioritized)

1. FIX the two memory-safety bugs first: (1) close the macOS device-watch use-after-free by keeping WatchListenerContext alive past listener removal / adding a drain barrier / Arc-Weak-guarding the proc (CT-1, coreaudio.rs:1116-1145 vs :841); (2) close the Go ReadBuffer/TryReadBuffer UAF under concurrent Close() by holding the mutex across the C call or draining in-flight reads before rsac_capture_free (BFFI-02, rsac.go:840-848), and delete the misleading 'safe without the lock' comment.

2. WIRE rt_alloc and enumeration_matrix into CI: add `cargo test --test rt_alloc` (per-platform) and `cargo test --test enumeration_matrix` to the per-OS unit jobs so ADR-0001's headline RT-safety guarantee and the honest-failure enumeration contract are actually regression-gated (TC-01, TC-02).

3. FIX the module-DAG violation: move list_audio_sources/list_audio_applications(_into) out of core into audio/api (re-export at the same lib.rs paths so the public surface is unchanged), delete/rewrite the layer-relabeling 'Separation of Concerns' doc, and add a CI grep/cargo-modules guard that fails the build on any core->audio/bridge/api/sink edge (DAG-001/DAG-002, DAG-004).

4. WRITE the device-watch threading ADR (highest-priority ADR gap): record direct-invoke (Linux) vs helper-thread hand-off (Windows/macOS), bounded-channel capacity, drop policy, and the RAII teardown contract; then either unify Linux for parity or document the divergence in the watch() trait + PlatformCapabilities (ADR-R1, RT-002, CT-5).

5. RECONCILE ADRs with shipped code: amend ADR-0003 §5 (StreamEnded -> STREAM_FAILED Fatal group), promote abi3-decision.md to Accepted/Implemented with corrected 0.2.0 sequencing, and refresh stale ADR-0001/0002 line refs (prefer symbol refs) (ADR-R2/R3/R4).

6. CLOSE the docs-vs-reality drift on the public surface: implement AudioCapture::pipe_to (or RunningCapture::drain_to) and an end-to-end WavFileSink example, or remove the pipe_to section from API_DESIGN.md (API-ERG-03); fix README/lib.rs read-loop footguns and switch examples to buffer.rms_dbfs() (API-ERG-08, API-ERG-04); and add the AudioCapture Send+Sync compile-time assertion (DF-03).

7. DECIDE the surface-evolution policy: add #[non_exhaustive] to AudioError plus CaptureTarget/AudioSourceKind/PermissionStatus before any wider release, since several enums are explicitly expected to grow (API-ERG-01, API-ERG-02).

8. FIX the cross-binding inconsistencies and ABI hazards: make Python __next__ end iteration on StreamEnded (BFFI-03), make the napi pump branch on recoverability instead of break-on-any-error (BFFI-07), return env!(CARGO_PKG_VERSION) from rsac_version() (BFFI-04), and resolve the cbindgen header double-prefix so a regen cannot clobber the curated rsac.h / break the Go ABI (BFFI-01).

9. WIRE the implemented-but-dead capabilities or soften the claims: call calculate_capacity_for_period from backends and thread config.buffer_size on macOS/Linux (PERF-01, DF-04); call set_negotiated_format from the negotiation point (PERF-07); populate buffer timestamps in at least one backend or document timestamp() as reserved (DF-01); and either wire bridge-zerocopy SampleRing into an interleaved-f32 backend or soften the VISION 'zero-copy' wording to 'alloc-free producer in steady state' (PERF-02).

10. TIGHTEN remaining test/CI gaps: split device-free Windows watch tests out of the continue-on-error step (TC-07), #[ignore]/gtimeout the unguarded macOS process-tap tests (TC-03), run `cargo test -p rsac-ffi -p rsac-napi -p rsac-python` plus a language-tier smoke test per binding (TC-06), and add AsyncAudioStream poll_next behavioral tests despite the verifier split, since the missed-wake race is currently unexercised.

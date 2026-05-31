# rsac FINAL critique + gap analysis — 2026-05-30 (post docs+CI/CD waves, 121 agents, adversarially verified)

## rsac final critique synthesis — release-readiness verdict

**Verdict: NOT release-ready for a stable/1.0 tag. Suitable for a 0.x / pre-1.0 release only if the two safety UAFs are either fixed or loudly fenced as experimental, AND the release-automation no-op is fixed.** The docs + CI/CD waves did genuine, honest work — the codebase's *self-description* is now trustworthy, the RT-allocation proof (`rt_alloc`) finally runs in CI on all three OS, six new ADRs accurately document previously-undocumented machinery, and the release workflows are well-guarded against the dangerous mistakes (no auto-major, idempotent tagging, least-privilege, pre-release exclusion). But three classes of problem block a confident stable cut.

### The 3 things that matter most NOW

1. **The "automated" release path silently no-ops after the tag (REL-1, CRITICAL).** `release-tag.yml` pushes the tag with the default `GITHUB_TOKEN` (`persist-credentials: true`, line 185). GitHub Actions will not re-trigger `on: push: tags:` workflows from a GITHUB_TOKEN-pushed tag — and `release.yml`/`release-npm.yml`/`release-pypi.yml` trigger *only* on tag push (npm/pypi have no `workflow_dispatch` fallback at all). **Verified against code.** So the documented recommended path ends with a tag that nothing observes: crates.io / npm / PyPI publishes never run. This is the single highest-impact defect: the release machine looks done but stops dead at the tag.

2. **Two real use-after-free windows remain in code (H1 macOS teardown, H2 Go read/Close).** H2 is tracked correctly as OPEN issue #28 with sound deferral rationale. H1 is documented in ADR-0005 §5 as a KNOWN LIMITATION but — **verified** — has NO tracking issue (contrast #28), and the overstated `coreaudio.rs:826-828` safety comment the ADR itself flags as "overstating the guarantee" is still in the source. Both are reachable under ordinary concurrent use; both gate a stable tag.

3. **Cross-manifest version integrity is unenforced in the automated path, and `rsac-ffi` is already diverged (0.1.0 vs 0.2.0).** **Verified:** `bindings/rsac-ffi/Cargo.toml` is 0.1.0 while the workspace is 0.2.0, `rsac_version()` still hardcodes `"0.1.0"`, `bump-version.sh` doesn't touch rsac-ffi, and the six-manifest hard lockstep gate (`ci.yml:684`) is dead code because **ci.yml has no `tags:` trigger** (verified) so it never runs at the point-of-no-return.

Two more material items round out the must-look list: the **async/blocking end-of-stream signal is Windows-only** (FH-1 — Linux/macOS never call `signal_done()`, **verified**: zero matches in those backends), so `audio_data_stream()` can hang forever on a dead non-Windows producer; and `set_negotiated_format` is **still called by no backend** while the docs now affirmatively claim `format()` returns the authoritative *delivered* format (PU-1 — a docs-harder-than-before cross-surface lie).

The good news: none of these are deep redesigns. REL-1 is a token swap; H1 has a one-line `Box::leak` stopgap; the version skew is a list reconciliation; FH-1 is one `signal_done()` call per backend; PU-1 is one call per backend or a doc narrowing. A focused deep-work-loop can clear the stable-blockers in a single wave.


## Release readiness

NOT ready for a stable / 1.0 tag. Acceptable for a 0.x pre-1.0 release ONLY after the genuine blockers below are addressed and the experimental surfaces are loudly fenced.

GENUINE BLOCKERS (must fix before ANY release, including 0.x):
1. REL-1 (CRITICAL) — fix the tag-push credential (fine-grained PAT or GitHub App token) so the publish workflows actually fire. Until this lands, the 'automated' release is a no-op past the tag and a maintainer will believe a release shipped when nothing reached the registries. This is the #1 must-fix because it silently breaks the entire delivery mechanism.
2. H2/BP-1 (HIGH) — the Go ReadBuffer/Close UAF is reachable under ordinary concurrent use. Either fix it (WaitGroup-drain before free, per #28) OR ship rsac-go as explicitly experimental/pre-1.0 in the package doc AND delete the false 'safe without the lock' comment now. Tracked in #28, but tracking does not make it shippable as stable.
3. H1/PS-1 (HIGH) — before enabling macOS device-watch in any release: apply the one-line Box::leak stopgap (or Arc+Weak), correct the overstated coreaudio.rs:826-828 safety comment NOW, and open a deferred-review issue mirroring #28 (currently violates CLAUDE.md's disposition policy — verified no such issue exists).
4. The rsac-ffi version skew (REL-3/CI-2/BP-4) — bump rsac-ffi to 0.2.0, fix rsac_version() to use CARGO_PKG_VERSION, and reconcile the bump-version.sh vs ci.yml lockstep manifest sets so a tagged release is internally consistent.

V0.x-ACCEPTABLE (document, then defer): AEG-1 AudioError #[non_exhaustive] and AEG-2/AEG-5 enum/Send+Sync hygiene are high-value for a 1.0 trajectory but a 0.x cut can ship without them PROVIDED the breaking-change-on-new-variant risk is stated in the CHANGELOG. FH-1 (Windows-only end-of-stream) is acceptable for 0.x IF documented as a known platform limitation, but should be fixed before 1.0 because it can hang consumer apps on Linux/macOS. The unwired cluster (SampleRing, period sizing, sinks, timestamps), the WASAPI hot-path copies, the 1ms polls, and the CI test-gating gaps (CI-3/4/5/6) are all v0.x-acceptable and now mostly documented or guard-mitigated.

Recommended cut: fix the four blockers, label rsac-go experimental if not fully fixed, document FH-1 and AEG-1/2/5 as known limitations in the CHANGELOG, then ship v0.2.0. Defer the 1.0 tag until AudioError is non_exhaustive, both UAFs are fixed in code, and FH-1 is unified across backends.


## Prior-findings status

Of the first critique's 40 findings, the docs + CI/CD waves produced a clear three-way split. FIXED (in code, verifiable): H4/TC-01 — rt_alloc now runs per-OS on all three platforms as a hard-fail gate exercising the exact production push path (the single most important closure, since rt_alloc is the sole proof of ADR-0001's alloc-free guarantee); the AsyncAudioStream missed-wake race is now backed by five behavioral regression tests; the Go callback-handle UAF (#28's cousin) is genuinely closed via clear-C-callback-before-cgo.Handle.Delete plus a recover() in goAudioCallback; and the whole-suite Windows continue-on-error was replaced with a hard-fail pure-logic partition in lockstep with release.yml. NOW-DOCUMENTED-AS-KNOWN (honest, but the underlying code is unchanged): H1 macOS teardown UAF (ADR-0005 §5, but still untracked as an issue), H3 device-watch threading divergence (ADR-0004, the standout ADR), the entire unwired cluster — SampleRing (ADR-0006), calculate_capacity_for_period + buffer_size semantics (ADR-0007), CachePadded partial-padding (ADR-0008), and the tracing/log shim (ADR-0009), all six ADRs verified accurate to the line — plus AudioError-not-non_exhaustive, pipe_to/AudioSink unwired, the &mut-vs-Send+Sync split, subscribe()'s missing terminating signal, the detached subscriber thread, buffer_size mislabeled-as-frames, and the format-negotiation duplication, all now enumerated in API_DESIGN.md §16 'Not Yet Implemented / Tracked' tagged with their critique IDs. The docs work was genuinely thorough and converted the prior 'docs over-claim' theme into 'docs honestly under-promise' — the major exception being PERF-07, where the new docs now over-claim HARDER (format()/FFI assert authoritative delivered-format while no backend calls set_negotiated_format). STILL-OPEN (untouched in code, not yet documented as a known limitation): H2 Go read/Close UAF (but now correctly tracked as OPEN issue #28), the two state.rs expect() panics, push_samples_guarded dead code, the default_device/get_default_device naming divergence, rsac_version() hardcoded 0.1.0 (+ the rsac-ffi 0.1.0 manifest skew), the cbindgen README clobber footgun, Python __next__ raising on StreamEnded, the napi pump break-on-any-error, the WASAPI multi-copy hot path, subscribe()'s 1ms busy-poll, and buffer timestamps always None (the lone unwired item with neither wiring nor an ADR). Net: roughly 4 fixed, ~18 now-documented-as-known, ~18 still-open — and this critique surfaced 14 issues the first did not, the most consequential being the release-automation no-op (REL-1) and the Windows-only end-of-stream signal (FH-1).


## Remaining work (prioritized)


### [CRITICAL] (release-automation) [new] REL-1 / CI-1: Automated release path no-ops after the tag — GITHUB_TOKEN-pushed tag does not re-trigger publish workflows

**Evidence:** release-tag.yml:72 persist-credentials:true + :185 git push origin via default GITHUB_TOKEN (no PAT/App token). GitHub Actions does not re-trigger on:push:tags: from a GITHUB_TOKEN push. release.yml:27-34, release-npm.yml:25-31, release-pypi.yml:32-38 trigger ONLY on tag push; npm/pypi have no workflow_dispatch. Verified: release-npm.yml on: block is push:tags only. RELEASE_PROCESS.md affirmatively claims the tag 'fans out' with no caveat.

**Recommendation:** Push the tag from a credential that re-triggers workflows: a fine-grained PAT or actions/create-github-app-token installation token via actions/checkout token: or explicit git push https://x-access-token:${TOKEN}@... Keep repo guard + least-privilege. As an interim, emit a ::warning:: after the push telling the maintainer to verify publishes started, and document the limitation. Do NOT broaden GITHUB_TOKEN scope.


### [HIGH] (safety-macos) [still-open] H1 / PS-1: macOS device-watch teardown UAF — open in code, documented in ADR-0005, but UNTRACKED as an issue + overstated source comment persists

**Evidence:** coreaudio.rs:1116-1145 removes listeners then drop(context) at :1141 with no in-flight-proc barrier; watch_listener_proc derefs the freed Box at :841. coreaudio.rs:826-828 safety comment still claims 'the context outlives every listener' — the exact comment ADR-0005 §5 says overstates the guarantee. Verified via gh issue list: only #28 (Go UAF) exists; NO issue tracks the macOS teardown UAF, violating CLAUDE.md's valid-defer disposition policy.

**Recommendation:** Before enabling macOS device-watch in a stable release: (1) fix the code — simplest sound stopgap is Box::leak the WatchListenerContext (drop only the SyncSender to stop delivery) or Arc + Weak upgrade in the proc; a true in-flight drain is infeasible (CoreAudio gives no barrier). (2) Correct coreaudio.rs:777-779 and :826-828 comments to the actual weaker Apple guarantee NOW. (3) Open a deferred-review issue mirroring #28 so the deferral is tracked, not just narrated in an ADR.


### [HIGH] (binding-go) [still-open] H2 / PS-2 / BP-1: Go ReadBuffer/TryReadBuffer UAF under concurrent Close() — open in code, correctly tracked as OPEN #28

**Evidence:** rsac.go:840-848 (ReadBuffer) and :874-878 (TryReadBuffer) snapshot the handle, c.mu.Unlock(), then call the blocking C read with no lock; misleading 'safe without the lock' comment at :843-846 unchanged. closeLocked() calls rsac_capture_free at :741. read_chunk->pop_blocking borrows the freed consumer for up to a 1s default_timeout. The SEPARATE callback-handle UAF (#28's cousin) WAS correctly fixed (clear C callback before cgo.Handle.Delete + recover()). Tracked as OPEN issue #28 (verified present) with sound rationale for why naive lock-across-read deadlocks.

**Recommendation:** Release-block the Go binding's stable tag on a fix OR ship rsac-go as explicitly pre-1.0/experimental in the package doc. Per #28 acceptance criteria: add rsac_capture_request_stop to unblock pop_blocking, have Close() set a closing flag + WaitGroup that drains in-flight reads before free, and return StreamEnded on stop. Independently, delete the false 'safe without the lock' comment NOW.


### [HIGH] (ci-correctness) [new] CI-1: Tag-time six-manifest version-lockstep hard gate is dead code — ci.yml has no tags: trigger

**Evidence:** Verified: ci.yml on: block is push:[main,master] + pull_request + workflow_dispatch only — no tags:/workflow_call. The hard-fail branch at ci.yml:684 is gated `if GITHUB_REF == refs/tags/v*`, which never fires. Job comment at :611-615 claims 'the hard gate runs at tag time, which is the point of no return' — that gate does not exist. release.yml:154-164 checks ONLY root Cargo.toml vs tag; npm/pypi do no cross-manifest check.

**Recommendation:** Add push:tags:['v*.*.*','!v*-*'] to ci.yml so version-lockstep runs at tag time (also partially backstops REL-1's blind spot), OR move the all-six-manifest tag-equality assertion into release-tag.yml/release.yml which DO run on tags. Fix the misleading job comment until the gate is real.


### [HIGH] (api-ergonomics) [now-documented] AEG-1: AudioError still lacks #[non_exhaustive] — highest-value SemVer-stability fix; documentation does not remove the hazard

**Evidence:** error.rs:107-108 #[derive(Debug)] pub enum AudioError — 22 variants, no #[non_exhaustive] (only UserFacingError has it). Contrast: StreamStats, BackpressureReport, DeviceInfo, DeviceEvent, PushOutcome all ARE non_exhaustive. Classification accessors (kind/recoverability/is_fatal/is_recoverable/user_message) already exist so a _ => arm is viable. API_DESIGN.md:124-129 now documents it as tracked — but a doc cannot make a downstream's existing exhaustive match non-breaking.

**Recommendation:** Add #[non_exhaustive] to AudioError before any wider/1.0-track release (internal exhaustive matches still compile). This is the single most consequential API-stability fix and it is one line. Update the API_DESIGN.md §2 note and CHANGELOG to record it done. Severity is high for a 1.0 trajectory; deferrable for a 0.x cut.


### [MEDIUM] (data-flow-cross-platform) [new] FH-1: Async/blocking end-of-stream signal is Windows-only — Linux/macOS never call signal_done(), so audio_data_stream() can hang forever on producer death

**Evidence:** Verified: signal_done() production callers are ONLY in src/audio/windows/thread.rs (7 sites); zero matches in src/audio/linux/\* or src/audio/macos/\*. Both non-Windows backends only transition Created->Running, never to Stopping/Stopped. is_stream_producing() (stream.rs:317) stays true while Running; async_stream.rs:96-99 returns Poll::Pending whenever empty AND producing, so a dead/stalled Linux/macOS producer leaves state Running forever — poll_next never yields None or Err.

**Recommendation:** Have the Linux PipeWire loop and macOS CoreAudio IOProc call producer.signal_done() (or transition Running->Stopping/Error) when the capture loop terminates or hits a fatal error, mirroring Windows. Add an ADR documenting the producer-side terminal-signal contract so async/blocking terminal behavior is uniform. At minimum document that async end-of-stream is Windows-only today.


### [MEDIUM] (data-flow-correctness) [still-open] PU-1 / PERF-07: set_negotiated_format called by no backend, yet format()/FFI docs now assert it returns the authoritative DELIVERED format

**Evidence:** Verified: set_negotiated_format's only call sites are bridge tests + the api.rs mock; NO src/audio/* backend calls it. The bridge atomic (ring_buffer.rs:371-397) is correct; the gap is the missing backend call. Linux param_changed (thread.rs:1670-1693) has the negotiated rate/channels AND producer in scope but only writes per-buffer metadata. api.rs:1196-1206 + rsac-ffi:130/707 now CLAIM format() is the authoritative negotiated delivery format — so docs over-claim harder while behavior is unchanged. Per-buffer AudioBuffer metadata IS correct (the escape hatch).

**Recommendation:** Add producer.set_negotiated_format(&delivered) at each backend's negotiation point (Linux is one line in param_changed; WASAPI at mix-format open; CoreAudio at IOProc format). Until then, narrow the api.rs:1196-1206 and rsac-ffi docs to 'returns the requested format; trust per-buffer AudioBuffer metadata for true delivery'. Highest-value item in the perf/unwired dimension and small.


### [MEDIUM] (release-automation) [still-open] REL-3 / CI-2: rsac-ffi (0.1.0) excluded from bump-version.sh and release-tag.yml manifest defense; ci.yml lockstep checks it but disagrees with bump-version.sh

**Evidence:** Verified: Cargo.toml=0.2.0, rsac-napi=0.2.0, but bindings/rsac-ffi/Cargo.toml:3=0.1.0. bump-version.sh rewrites only 5 manifests (rsac-ffi explicitly excluded). ci.yml:654-659 version-lockstep set V[] DOES include rsac-ffi — so the two scripts disagree on the in-lockstep set. release-tag.yml manifest defense (:108-141) compares only against root Cargo.toml; will tag v0.3.0 with rsac-ffi still 0.1.0. rsac-ffi is publish=false so never reaches a registry, but the lockstep CONTRACT is silently violated.

**Recommendation:** Reconcile the two manifest lists: either add bindings/rsac-ffi/Cargo.toml to bump-version.sh's rewrite set + release-tag.yml's defense, OR drop it from ci.yml's version-lockstep set and document the intentional exclusion. Also bump rsac-ffi to 0.2.0 to clear the existing skew.


### [MEDIUM] (api-ergonomics) [now-documented] AEG-2: Growing public enums (CaptureTarget, AudioSourceKind, PermissionStatus) not #[non_exhaustive] despite being expected to grow

**Evidence:** config.rs:53-66 CaptureTarget (5 variants, has growth-friendly app()/pid()/device() constructors), introspection.rs:40 AudioSourceKind, :265 PermissionStatus — none non_exhaustive. API_DESIGN.md:151-153 documents CaptureTarget's missing attribute as an OPEN decision; AudioSourceKind/PermissionStatus not even mentioned in §16.

**Recommendation:** Mark CaptureTarget, AudioSourceKind, PermissionStatus #[non_exhaustive] before a wider release. For SampleFormat/DeviceKind/ErrorKind/Recoverability either mark them too or add a one-line doc stating the closed set is an intentional stability guarantee, so the policy is uniform and decision-backed. Bundle with AEG-1.


### [MEDIUM] (api-ergonomics) [now-documented] AEG-5: &mut self read methods contradict the type-level Send+Sync claim, with no compile-time assertion

**Evidence:** api.rs module doc claims AudioCapture is Send+Sync, Arc-shareable. read_buffer (:930), read_buffer_blocking (:954), buffers_iter (:974) take &mut self while subscribe/stream_stats/format take &self. Underlying try_read_chunk is &self, so &mut is not required. No _assert_send_sync::<AudioCapture>() exists (only AudioBuffer/BridgeStream have it). API_DESIGN.md:383-400 documents both halves including that downstreams guessed !Sync.

**Recommendation:** Add _assert_send_sync::<AudioCapture>() (mirroring buffer.rs:431) and assert Send+Sync at the type level. Change the three read methods to &self (gate any pump-field mutation behind interior mutability like start()/stop() already do) so Arc-shared read consumers need no external lock.


### [MEDIUM] (api-ergonomics-bindings) [still-open] AEG-6 / BP-2/BP-3/BP-6: Terminal-error / end-of-stream signaling is dropped across consumption modes and bindings (subscribe, Python __next__, napi pump, Go Stream)

**Evidence:** subscribe() returns mpsc::Receiver<AudioBuffer> (api.rs:1113) and breaks on any Err dropping the Sender (:1146) — the lone consumption mode discarding the terminating error (async/iterator carry AudioResult). Python __next__ (rsac-python lib.rs:1101-1110) maps only StreamReadError to StopIteration; StreamEnded falls to the StreamError path. napi pump (rsac-napi lib.rs:716-719) does Err(_)=>break on any error incl. recoverable StreamReadError. Go streamLoop (rsac.go:932-956) ends the channel on any error.

**Recommendation:** subscribe(): change channel item to Result<AudioBuffer, AudioError> matching the async/iterator Item, or add a paired stop/error signal. Python: treat StreamEnded (fatal terminal) as StopIteration, ideally branch on e.is_fatal(). napi/Go pumps: continue on Recoverable errors, terminate only on Fatal/StreamEnded, surface terminal errors. Steer error-aware consumers to StreamWithErrors()/async stream in rustdoc.


### [MEDIUM] (rt-safety) [still-open] PS-4 / PU-3-adjacent: push_samples_guarded is dead in production; all three backend callbacks call the unguarded push — FFI panic-guard contract half-implemented

**Evidence:** push_samples_guarded (ring_buffer.rs:666) has zero production call sites (def + tests at :2156-2196). macos/thread.rs:284 and linux/thread.rs:1762 call the unguarded push from genuine C/FFI callback threads (an unwind there is UB); windows/thread.rs:517 runs on an rsac-owned Rust thread (guard matters less). No ADR records a panic-free proof that would justify deleting the guard.

**Recommendation:** Pick one and record it: (A) call push_samples_guarded at macos/thread.rs:284 and linux/thread.rs:1762 (documented alloc-free on happy path, preserving ADR-0001); leave windows on the unguarded push; OR (B) audit push_samples_or_drop_inner for panic-freedom and delete the guard, recording the decision in ADR-0001/0002. Option A is lower-risk before a release.


### [MEDIUM] (binding-ffi) [partially-addressed] BP-5: cbindgen double-prefix persists AND README manual-regen command still clobbers the curated rsac.h

**Evidence:** rsac_generated.h shows RsacRsacBuilder / Rsacrsac_error_t double-prefix; curated rsac.h has correct RsacBuilder/rsac_error_t that Go links against. cbindgen.toml:17 prefix='Rsac' with only 5 export.rename entries so the prefix double-applies. build.rs:16 correctly writes to rsac_generated.h (safe). BUT README.md:160-162 manual regen still targets --output .../include/rsac.h — following the docs overwrites the curated header and breaks the ABI.

**Recommendation:** Fix the README command to target rsac_generated.h (never rsac.h), OR resolve the double-prefix at source (drop global prefix, extend export.rename for opaque structs + callback/enum typedefs so regen of rsac.h is a no-op). Add a CI diff check that fails if rsac.h drifts from the curated symbol set.


### [LOW] (ci-correctness) [new] CI-3: Windows DEVICE_SKIPS '--skip watch_' over-skips the four device-free DeviceWatcher lifecycle tests into the tolerated bucket

**Evidence:** ci.yml:164 and release.yml:78 DEVICE_SKIPS includes watch_, demoting test_watch_register_and_clean_drop / _repeated_register_unregister / _two_concurrent_watchers / _outlives_enumerator_drop (wasapi.rs:2204-2294) to continue-on-error. These create WindowsDeviceEnumerator::new() (only CoCreateInstance, no Audiosrv) + RegisterEndpointNotificationCallback — client-side, no audio service needed. First critique TC-07 explicitly recommended they run WITHOUT continue-on-error; this is the opposite.

**Recommendation:** Remove watch_ from DEVICE_SKIPS (and the tolerated loop) in both ci.yml and release.yml so the four device-free watch tests hard-fail. If a specific test truly can't register the COM callback on the runner, narrow the skip to that exact test name, not the whole watch_ family.


### [LOW] (ci-correctness) [new] CI-4: enumeration_matrix's silent-empty (audit L4) assertion never executes — wired only where audio_available() is always false

**Evidence:** enumeration_matrix runs only in ci.yml's three headless unit jobs (none set RSAC_CI_AUDIO_AVAILABLE, set only in ci-audio-tests.yml). ci-audio-tests.yml exports the var but does NOT run enumeration_matrix. tests/enumeration_matrix.rs:213-223 audio_available() falls to a runtime probe (enumerate_devices() non-empty) when unset -> false on headless. The non-empty assertions (:143-157, :368-373) only run if audio_available(), and the same enumerate_devices() both gates and is asserted, so an Ok(empty) regression is accepted as 'honest headless'.

**Recommendation:** Run cargo test --test enumeration_matrix in the ci-audio-tests.yml hardware jobs that already export RSAC_CI_AUDIO_AVAILABLE=1 so the non-empty assertion runs against real devices, or set the var from an independent probe (as ci-audio-tests.yml already does via pactl) before the step. Keep graceful-skip for headless ci.yml jobs.


### [LOW] (safety-bridge) [still-open] PS-3: Two expect() panics in src/bridge/state.rs violate the literal 'No panics in library code' rule (provably unreachable today, undocumented)

**Evidence:** state.rs:106 get() and :127-128 transition() Err arm both call StreamState::from_u8(raw).expect('AtomicStreamState contains invalid discriminant'). Writers store `state as u8` from #[repr(u8)] enum (variants 0..=5) so from_u8 cannot return None — unreachable, but a panic in source. CLAUDE.md:34 mandates 'No panics in library code'; no ADR (0004-0009) or AGENTS.md exception documents them.

**Recommendation:** Replace both with .unwrap_or(StreamState::Error) + debug_assert!(false, ...) so a stray discriminant degrades to terminal-Error instead of aborting, OR add a one-line no-panics exceptions entry in AGENTS.md/short ADR recording the soundness argument. Cheap; land with the next bridge touch.


### [LOW] (binding-ffi) [still-open] BP-4 / BFFI-04: rsac_version() hardcodes "0.1.0" while workspace + sibling bindings are 0.2.0

**Evidence:** Verified: bindings/rsac-ffi/src/lib.rs:1412 static VERSION: &[u8] = b"0.1.0\0". Python __version__ correctly uses env!(CARGO_PKG_VERSION). Compounds with the rsac-ffi 0.1.0 manifest skew.

**Recommendation:** Return concat!(env!("CARGO_PKG_VERSION"), "\0") so the &[u8] stays a valid C string with no runtime alloc, and bump rsac-ffi to 0.2.0.


### [LOW] (api-ergonomics) [still-open] AEG-3: default_device() (trait/backends) vs get_default_device() (public facade) naming divergence

**Evidence:** Trait/backends use default_device() (interface.rs:494, wasapi.rs:1103, etc.); facade exposes get_default_device() (audio/mod.rs:97). enumerate_devices() is consistent across both — default_device is the lone get_-prefixed divergence. Not in §16 tracked items.

**Recommendation:** Rename the facade to default_device() to match the trait; keep get_default_device() as a #[deprecated] alias for one release so bindings + examples migrate. Cheap; bundle with the next facade change.


### [LOW] (api-ergonomics) [now-documented] AEG-7 / PU-3 / DF-04: buffer_size setter doc says 'in frames' but is a ring slot count honored only on Windows; field doc never corrected

**Evidence:** api.rs:174 and config.rs:263-264 say 'buffer size in frames'. Only WASAPI threads config.buffer_size into calculate_capacity (wasapi.rs:560); macOS/Linux hardcode calculate_capacity(None,4) so the setter no-ops there and is actually a slot count. ADR-0007 §4/§6 documents the truth and lists the doc fix as a follow-up, but config.rs:263 was not corrected. Redundant buffer_size_frames alias (api.rs:181) compounds the confusion.

**Recommendation:** Correct config.rs:263 to 'ring-buffer depth in buffers/slots, honored only on Windows today' matching ADR-0007. Consider deprecating the buffer_size_frames alias. Optionally thread buffer_size into macOS/Linux create_bridge (or adopt calculate_capacity_for_period) so the knob works everywhere.


### [LOW] (api-ergonomics) [now-documented] AEG-4 / FH-6: AudioSink family (NullSink/ChannelSink/WavFileSink) is exported + in the prelude but entirely unwired; no library path drives AudioSink::write

**Evidence:** No pipe_to/drain_to/.write reference in src/api.rs. Sinks re-exported at crate root + prelude; only callers are tests. docs/API.md:336-338 + API_DESIGN.md §16 now honestly state pipe_to is NOT IMPLEMENTED and show the manual drain pattern (over-claim fixed). The crate's own examples/record_to_file.rs writes WAV directly, bypassing WavFileSink.

**Recommendation:** Either add a thin RunningCapture::drain_to<S: AudioSink>(sink) reusing the callback-pump thread and update examples/record_to_file.rs to use WavFileSink, OR explicitly decide the sinks are consumer-convenience-only. Don't let a doc note be the permanent resting state of an unwired four-type abstraction. Acceptable for 0.x.


### [LOW] (api-ergonomics) [now-documented] AEG-8: Negotiated format not exposed as a pre-build query; downstreams re-enumerate the device to learn the resolved format

**Evidence:** build() negotiates internally (api.rs:402-433, non-Linux only) but the resolved format is reachable only after build()+start() via format() (api.rs:1204, None before start, and per PU-1 reflects requested not delivered). No builder negotiated_format() or pre-build resolver; downstream (audio-graph) reimplements pick_supported_format and re-enumerates. Linux build() doesn't negotiate at all (cfg-gated), so negotiation timing is platform-divergent.

**Recommendation:** Expose negotiation as a public pre-build query (AudioCaptureBuilder::negotiated_format() or a DeviceInfo/PlatformCapabilities helper) so consumers get the resolved format without re-enumerating. Document the per-platform timing (non-Linux negotiates at build, Linux at stream-open). Bundle with PU-1.


### [LOW] (build-config) [new] FH-2: cargo build --no-default-features breaks — app_capture_test bin gated by target_os only, not feat_windows, with no required-features

**Evidence:** Verified by report: cargo build --no-default-features fails E0432 unresolved import rsac::audio::windows at app_capture_test.rs:221. The bin guards with #[cfg(target_os='windows')] only while the lib gates pub mod windows behind cfg(all(target_os, feature='feat_windows')). Cargo.toml:191-193 [[bin]] app_capture_test has NO required-features (unlike sibling bins). --no-default-features --lib succeeds — only the default-built bin breaks. Cargo.toml:51 documents this exact build invocation.

**Recommendation:** Gate the in-bin platform blocks on cfg(all(target_os, feature=feat_windows/feat_linux)) to match the lib, OR add required-features to the [[bin]], OR route the bin through the public get_device_enumerator() API. Add a cargo build --no-default-features smoke step to CI to catch bin/lib cfg drift.


### [LOW] (sink) [new] FH-3: WavFileSink writes raw interleaved samples against a header fixed at construction with no channel-count validation — mid-stream channel change silently corrupts the file

**Evidence:** wav.rs:57-65 captures channels/sample_rate into WavSpec once at new(). write() (:69-89) writes whatever interleaving the buffer carries with no comparison against buffer.channels()/sample_rate(). frames_written += buffer.num_frames() (:87) divides by buffer.channels(), so a mismatched buffer also miscounts frames.

**Recommendation:** In write(), reject (or document the requirement) when buffer.channels()/sample_rate() differ from the spec — return ConfigurationError/UnsupportedFormat rather than producing a corrupt file. At minimum document that WavFileSink assumes a fixed format for the file's lifetime.


### [LOW] (perf) [still-open] PU-5: subscribe() 1ms busy-poll AND its sync fallback pop_blocking also 1ms-polls — the waker is async-only

**Evidence:** api.rs:1142-1144 subscribe reader sleeps 1ms on empty. pop_blocking (ring_buffer.rs:880-911) ALSO uses a 1ms sleep spin-loop, not the waker. The AtomicWaker is cfg(feature='async-stream')-gated (ring_buffer.rs:294); every wake() site is async-gated. So the prior 'route subscribe through pop_blocking' fix keeps the 1ms latency.

**Recommendation:** Make the AtomicWaker (or a std Condvar) unconditional so pop_blocking + subscribe can wake-on-push in the default build, then drive subscribe off it; OR document the up-to-1ms latency on subscribe()/pop_blocking. Do not just route subscribe through pop_blocking.


### [LOW] (perf) [still-open] PU-7: WASAPI hot path does VecDeque copy + O(n) make_contiguous + scalar per-sample from_le_bytes + extend_from_slice (3 copies); Linux's bulk align_to is the model; unbenchmarked

**Evidence:** windows/thread.rs:483 VecDeque<u8> copy, :504 make_contiguous O(n) memmove per packet, :508-511 scalar from_le_bytes loop, ring_buffer.rs:585 extend_from_slice. Contrast linux/thread.rs:1756 single align_to::<f32>() bulk reinterpret. No WASAPI byte-conversion benchmark exists. Off the RT callback thread, so perf-only.

**Recommendation:** Mirror Linux: yield contiguous &[u8] and reinterpret via align_to::<f32>() (or bytemuck::cast_slice) in one bulk copy, eliminating the scalar loop and ideally the VecDeque/make_contiguous. Add a WASAPI byte-conversion bench to benches/bridge.rs. Quantify before/after.


### [LOW] (perf-unwired) [now-documented] PU-2 / DF-01: calculate_capacity_for_period, bridge-zerocopy SampleRing, and buffer timestamps remain unwired (now ADR-documented with promote-or-remove gates, except timestamps)

**Evidence:** calculate_capacity_for_period (ring_buffer.rs:1354) zero production callers; all backends use static calculate_capacity. SampleRing default-off, no backend/bench uses it. ADR-0006/0007 honestly document both with promote-or-remove gates and self-correct the Cargo.toml 'A/B'd in benches' overstatement (Cargo.toml:67 comment still literally false). push_samples_or_drop_at/with_timestamp unwired and — unlike the others — NOT covered by an ADR; buffer timestamps always None.

**Recommendation:** Next wave: wire calculate_capacity_for_period into all three backends once the period is known (low-risk, satisfies ADR-0007 §6). Leave SampleRing until someone produces the ADR-0006 §6 benchmark; honor the 'remove' branch by next minor if none lands. Fix the stale Cargo.toml:67 comment. For timestamps: either populate via push_samples_or_drop_at in one backend with a documented monotonic contract, OR add a note declaring timestamp() reserved/always-None — the silent always-None state is the worst option.


### [LOW] (ci-correctness) [new] CI-5: Module-DAG guard false-negative surface — only matches crate::<layer>::, missing module-level and super-relative reverse edges

**Evidence:** scripts/check-module-dag.sh:125/159 patterns require trailing :: (crate::audio::). A reverse edge as `use crate::audio;` then `audio::foo()`, or `use super::super::audio::Foo;`, contains no crate::audio:: token and would not be flagged. Verified no such evasive edges exist today.

**Recommendation:** Broaden to also match crate::(audio|api|sink)\b without trailing :: and (super::)+(audio|api|sink):: relative forms. Add a self-test fixture with a deliberately-violating snippet to prove the guard fails on it.


### [LOW] (ci-correctness) [new] CI-6: Non-ignored macOS CoreAudio tap tests hard-fail in the macOS unit job and assert hardware presence

**Evidence:** ci.yml:252-254 macOS unit job runs cargo test --lib --features feat_macos (not continue-on-error, timeout-minutes:5, no --skip). Only test_new_system_creates_tap is #[ignore]d (tap.rs:1358). tap.rs:1214 test_enumerate_audio_process_objects asserts !ids.is_empty() (hardware presence); :1236/:1248/:1316 are non-ignored. None hang, but the non-empty assertion can fail on a device-less macOS VM.

**Recommendation:** Gate the hardware-presence assertions (tap.rs:1214 etc.) on require_audio!/RSAC_CI_AUDIO_AVAILABLE so they tolerate an empty list on headless CI, or #[ignore] them like test_new_system_creates_tap, so the macOS unit job's hard-fail only reflects real logic regressions.


### [OBSERVATION] (release-automation) [new] REL-4/REL-5/CI-7/REL-2: release-tag.yml auto-tags on any 'release: vX.Y.Z' commit subject (mitigated by guards); workflow header cites a non-existent release-please.yml

**Evidence:** release-tag.yml:41-43 triggers on every master push; gate is the commit subject regex (:89) only — any commit subject 'release: vX.Y.Z' auto-tags regardless of provenance (no merged-PR/label check). Meaningfully constrained by repo guard, manifest defense, major guard, and remote-tag idempotency. Separately, release-tag.yml:3-7,32 header references release-please.yml which does not exist (actual file is release-prepare.yml). REL-5: prepare's tag-existence guard can be defeated only across DIFFERENT NEXT values off divergent bases (low).

**Recommendation:** Subject-gate is acceptable given the guards and documented convention; if tightening, additionally require the merged-PR 'release' label (prepare already applies it). Replace the three release-please.yml references in release-tag.yml's header with release-prepare.yml. For REL-5, optionally refuse if an open release/* PR exists or recompute NEXT against origin/master at PR-open time.


### [OBSERVATION] (perf-unwired) [new] FH-4/PU-8: bridge-zerocopy meta-push-after-commit recovery path is logically unsound (orphaned samples permanently desync the stream) but unreachable while default-off

**Evidence:** ring_buffer.rs:1024 commits samples BEFORE meta.push at :1036; on push failure it drop_chunk()s but the committed samples are not removed. SampleRingConsumer::pop reads meta then pops exactly meta.len samples, so an orphaned meta-less run shifts every later read window — permanent desync, not single-chunk loss. The recovery comment (:1032-1035) misdescribes the consumer. Unreachable in practice (sole producer + meta.slots()==0 check). Default-off, unwired.

**Recommendation:** When/if SampleRing is promoted, restructure to reserve+commit metadata atomically with the samples (reserve both write_chunks, commit both, neither if either fails), eliminating the commit-then-orphan window. Fix the misleading comment now. Record as a promote-time prerequisite alongside ADR-0006 §6. No action needed while default-off.


### [OBSERVATION] (data-flow) [new] FH-5: audio_data_stream() never checks register_waker()'s return; the trait default returns false, so a non-waking stream would hang in Poll::Pending

**Evidence:** async_stream.rs:87 calls register_waker(cx.waker()) and discards the bool; interface.rs:333-337 default returns false (no-op). api.rs:994-1002 constructs AsyncAudioStream without checking support. Mitigant: the only production CapturingStream is BridgeStream whose register_waker always returns true (stream.rs:312-315), so the hang can't occur with the shipped backend.

**Recommendation:** Make audio_data_stream() return an AudioError when register_waker() reports false, or have poll_next fall back to polling / return None. Cheapest: debug_assert!(register_waker(...)) in new() and document the contract as enforced.


## New findings (not in first critique)

- REL-1/CI-1 (CRITICAL): the GITHUB_TOKEN-pushed tag does not re-trigger the publish workflows, so the documented automated release path silently no-ops after the tag — crates.io/npm/PyPI publishes never run. Verified against release-tag.yml (persist-credentials:true, default token) and the on:push:tags-only triggers of all three publish workflows.
- CI-1 (HIGH): ci.yml has no tags: trigger, so the six-manifest version-lockstep HARD gate at ci.yml:684 is dead code — the 'point of no return' gate the job comment promises does not exist.
- FH-1 (MEDIUM): signal_done() is called only by the Windows backend (verified zero matches in Linux/macOS); a dead/stalled non-Windows producer leaves the bridge Running forever, so audio_data_stream() can hang indefinitely with no None/Err.
- CI-3 (LOW): '--skip watch_' over-skips the four device-free DeviceWatcher lifecycle tests into the tolerated bucket — the opposite of the first critique's TC-07 recommendation.
- CI-4 (LOW): enumeration_matrix's silent-empty (audit L4) assertion only runs when RSAC_CI_AUDIO_AVAILABLE=1, which no enumeration_matrix-running job sets — the headline 'no silent empties' guard never executes.
- FH-2 (LOW): cargo build --no-default-features breaks because app_capture_test bin is gated on target_os only (not feat_windows) and declares no required-features — bin/lib cfg drift, no CI coverage of the bare-no-default build.
- FH-3 (LOW): WavFileSink fixes its WAV header at construction and never re-validates per-buffer channel/rate, so a mid-stream format change silently corrupts the file and miscounts frames.
- CI-5 (LOW): the module-DAG guard only matches crate::<layer>:: (trailing ::), missing module-level (use crate::audio;) and super-relative reverse edges — a false-negative surface with no self-test fixture.
- CI-6 (LOW): non-ignored macOS CoreAudio tap tests hard-fail in the unit job and assert hardware presence (!ids.is_empty()), a latent headless-VM flake.
- FH-4/PU-8 (OBS): the bridge-zerocopy meta-push-after-commit recovery arm is logically unsound (orphaned samples permanently desync) though unreachable while default-off; the recovery comment misdescribes the consumer.
- FH-5 (OBS): audio_data_stream() ignores register_waker()'s bool and the trait default returns false — only safe because the shipped BridgeStream always returns true.
- REL-2/CI-7 (OBS): release-tag.yml's header references a non-existent release-please.yml (the real step-1 file is release-prepare.yml).
- REL-4 (OBS): release-tag.yml auto-tags on any 'release: vX.Y.Z' commit subject regardless of provenance (mitigated by repo/manifest/major/idempotency guards).
- PS-5 (OBS): explicit release-blocking ranking of the prior safety findings plus the process gap that the macOS H1 deferral, unlike the Go H2, has no tracking issue despite CLAUDE.md's disposition policy.


## Strengths (now solid)

- The docs wave is honest, not aspirational: API_DESIGN.md §16 enumerates every unimplemented/tracked surface (pipe_to, LatencyMode, AudioFileFormat, buffer_size, set_negotiated_format, timestamp, missing Send+Sync assertion) tagged with critique IDs — the prior 'docs over-claim' theme is largely inverted to 'docs honestly under-promise'. Preserve this discipline.
- The six new ADRs (0004-0009) are accurate to the line — every quantitative claim (channel cap 64, drop-newest policy, SampleRing all-or-nothing commit, period-sizing worked examples, CachePadded field split, trace.rs macro arms) was verified against code with no fabricated or drifted numbers. ADR-0004 (threading divergence) is the standout; 0006/0007 even self-correct an overstated Cargo.toml comment and set promote-or-remove gates so features can't rot silently.
- rt_alloc — the sole proof of ADR-0001's alloc-free guarantee — now runs as a hard-fail gate per-OS on all three platforms, exercising the exact production push_samples_or_drop -> _inner path, with MEASURE_LOCK + --test-threads=1 making the process-wide allocator measurement deterministic. This closes the most important prior gap.
- Issue #28 is exemplary tracking discipline (bug + deferred-review labels, verified-against-code detail, documented reason the naive fix deadlocks, acceptance criteria including a -race gate) — exactly the CLAUDE.md disposition policy working as intended; use it as the template for the still-untracked H1 deferral.
- FFI safety discipline is real and uniform: every fallible extern "C" fn null-checks pointers and runs inside catch_unwind, the user-callback trampoline is itself catch_unwind-guarded, Go copies borrowed C buffers into Go memory before any callback, Python marshals via f32::to_le_bytes (no reinterpret), and napi carries f32 as Float32Array + u64 as BigInt with honest .d.ts typedefs. The exhaustive error maps (no _ => arm) force every new AudioError variant to be classified at each boundary.
- The release workflows are well-guarded against the dangerous mistakes: a major bump is impossible through three independent guards, tagging is idempotent against BOTH local and remote tags, permissions are least-privilege and repo-pinned, pre-release shapes are excluded at multiple layers, no bot ever pushes to master, and the squash-subject regex is precisely tuned (accepts GitHub's default 'release: vX.Y.Z (#N)', rejects rc/bare/chore/capitalized). The REL-1 token fix must not weaken any of these.
- AudioBuffer level metering and sample math are genuinely RT-safe and robust: every method is #[inline] and alloc-free, RMS accumulates in f64, non-finite samples are skipped so a single NaN can't poison the meter, and integer-overflow/divide-by-zero paths use checked_div/saturating_mul/clamp — backed by ~20 edge-case tests. The bridge has exactly one unsafe block, correctly justified.
- The error taxonomy gives downstreams a clean escape from exhaustive matching even before AudioError gains #[non_exhaustive]: kind(), recoverability(), is_fatal()/is_recoverable(), and user_message() all exist, so the SemVer hazard is latent rather than blocking and the fix is mechanical.


## Recommended next wave

1. Wave A — release-pipeline unblock (do first, gates everything): (1) REL-1 — swap the tag push to a fine-grained PAT or GitHub App installation token so release.yml/npm/pypi re-trigger; add a post-push ::warning:: as a belt-and-suspenders verification nudge. (2) CI-1 — add push:tags:['v*.*.*','!v*-*'] to ci.yml (also backstops REL-1) OR move the all-six-manifest tag-equality assertion into release-tag.yml. (3) REL-3/CI-2/BP-4 — bump rsac-ffi to 0.2.0, fix rsac_version() to CARGO_PKG_VERSION, reconcile bump-version.sh and ci.yml lockstep sets. (4) REL-2/CI-7 — fix the release-please.yml references in release-tag.yml's header.

2. Wave B — safety blockers (gates a stable tag): (1) H1/PS-1 — apply the Box::leak (or Arc+Weak) stopgap for the macOS teardown UAF, correct the overstated coreaudio.rs:777-779/826-828 comments, and open a deferred-review issue mirroring #28. (2) H2/BP-1 — fix the Go read/Close UAF via a WaitGroup that drains in-flight reads before rsac_capture_free (add rsac_capture_request_stop to unblock pop_blocking) OR label rsac-go experimental; either way delete the false 'safe without the lock' comment now. (3) FH-1 — call signal_done() in the Linux PipeWire loop and macOS IOProc on termination/fatal error, and add an ADR for the producer-side terminal-signal contract.

3. Wave C — API stability + cross-surface correctness (gates 1.0, document-and-defer for 0.x): (1) AEG-1 — add #[non_exhaustive] to AudioError; (2) AEG-2 — same for CaptureTarget/AudioSourceKind/PermissionStatus (and a uniform-policy doc for the closed enums); (3) AEG-5 — add _assert_send_sync::<AudioCapture>() and convert the three read methods to &self via interior mutability; (4) PU-1 — call set_negotiated_format at each backend's negotiation point (Linux is one line in param_changed) and, until then, narrow the format()/FFI docs; (5) AEG-6/BP-2/BP-3/BP-6 — deliver terminating errors across subscribe()/Python __next__/napi+Go pumps (Result channel item + recoverability branching).

4. Wave D — CI gate fidelity + safety hygiene: (1) CI-3 remove watch_ from DEVICE_SKIPS so the four device-free watch tests hard-fail; (2) CI-4 run enumeration_matrix in the RSAC_CI_AUDIO_AVAILABLE=1 hardware jobs so the no-silent-empties assertion executes; (3) CI-5 broaden the module-DAG guard to module-level/super-relative edges + add a violating self-test fixture; (4) CI-6 gate the macOS tap hardware-presence asserts behind require_audio!; (5) FH-2 fix the app_capture_test bin cfg + add a --no-default-features CI smoke step; (6) PS-3 swap the two state.rs expects for unwrap_or(Error)+debug_assert; (7) PS-4 wire push_samples_guarded at the macOS/Linux FFI sites or record a panic-free ADR; (8) BP-5 fix the README cbindgen regen target + add a header-drift CI diff.

5. Wave E — perf + unwired cleanup (post-stable, lowest priority): (1) PU-2 wire calculate_capacity_for_period into all three backends and honor the ADR-0006 §6 promote-or-remove decision for SampleRing (fix the stale Cargo.toml:67 comment); (2) DF-01 either populate buffer timestamps in one backend with a documented monotonic contract or declare timestamp() reserved/always-None; (3) PU-7 adopt Linux's bulk align_to in the WASAPI path and add a conversion benchmark; (4) PU-5 make the waker unconditional (or Condvar) to remove the 1ms polls; (5) AEG-4/FH-3/FH-4/FH-5 decide the sink story (drain_to or consumer-only), add WavFileSink format validation, harden the SampleRing commit ordering as a promote-time prerequisite, and enforce the register_waker contract.

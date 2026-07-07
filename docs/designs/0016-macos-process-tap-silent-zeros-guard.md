# ADR 0016 — macOS Process-Tap silent-zeros diagnostic (denied-permission guard)

**Status:** Accepted
**Date:** 2026-07-06
**Scope:** `src/audio/macos/thread.rs` (`create_macos_capture`, `MacosPlatformStream`)
**Verdict:** When a macOS **Process-Tap** capture (SystemDefault / Application /
ApplicationByName / ProcessTree) starts but delivers **only zeroed samples**
throughout a bounded grace window while the stream is otherwise healthy, rsac
emits a **single `log::warn!`** naming the most likely cause (missing/denied
`kTCCServiceAudioCapture` permission). It is a **diagnostic only** — never an
error, because pure silence is a legitimate stream state.

## 1. Context

macOS Process Taps are gated by the Audio Capture TCC service
(`kTCCServiceAudioCapture`). When that permission is missing or denied, the
CoreAudio setup path is **silently deceptive** (verified on macOS 26 Tahoe,
arm64, 2026-07-06, and corroborated by community write-ups): every call —
`AudioHardwareCreateProcessTap`, `AudioHardwareCreateAggregateDevice`,
`AudioDeviceCreateIOProcIDWithBlock`, `AudioDeviceStart` — returns `noErr`, and
then the IO proc delivers **all-zero buffers** (or never fires). Apple does not
document this. There is **no error code** to key on.

Consequently rsac's `create_macos_capture` returns a perfectly healthy-looking
`MacosPlatformStream`, the reader gets well-formed 48 kHz buffers, and the only
symptom is that every sample is `0.0`. A downstream consumer (e.g. the
audio-graph transcription pipeline) sees "working capture, no audio" with no
signal as to *why* — the single most confusing failure mode on the platform.

This is exactly the trap the branch's real-hardware verification hit: the host
terminal (cmux) lacked `NSAudioCaptureUsageDescription`, so `tccd` refused the
service, and every capture streamed silence with no diagnostic.

The proactive preflight ([ADR-0015](0015-macos-tcc-audiocapture-preflight.md)) is
*advisory* and requires a private SPI + an opt-in feature; a `Granted` preflight
still doesn't guarantee non-silent capture (a terminal-launched or unbundled
process is denied at runtime regardless of the TCC DB). So a **runtime** signal
is the authoritative, always-available backstop.

## 2. Decision drivers

- **Honest failure over silent success** (`src/core/capabilities.rs` §doc,
  `AGENTS.md`). The M8 precedent — rejecting output-only AUHAL devices rather
  than returning a "silently-dead capture" (`thread.rs` `resolve_capture_target`)
  — is the same principle: do not hand back a stream that looks alive but can
  never carry signal without saying so.
- **But silence is legitimate.** A paused video, a muted call, or a genuinely
  quiet source all produce all-zero buffers with permission fully granted.
  Escalating silence to an `AudioError` would break every one of those valid
  captures. So the guard must **warn, not fail**.
- **RT-safety is non-negotiable** (ADR-0001). The detection touching the audio
  IO-proc must be alloc-free and lock-free; anything that logs, sleeps, or
  allocates happens off the RT thread.
- **Scope to where the trap exists.** Only the Process-Tap tiers hit the
  silent-denial behavior. A plain `Device` (AUHAL input / mic) capture that is
  silent is far more likely to be genuinely quiet and is gated by a *different*
  service (microphone), so the guard does not run for it.

## 3. Decision

In `create_macos_capture`, for **Process-Tap captures only** (`process_tap.is_some()`):

1. **RT side** — a shared `Arc<AtomicBool> non_silence_seen`, cloned into the
   input callback. On each callback, if the flag is not yet set, a cheap
   alloc-free scan (`data.iter().any(|&s| s != 0.0)`) sets it the first time any
   non-zero sample arrives. Once set, subsequent callbacks do a single relaxed
   load and skip the scan — so the steady-state cost is one atomic load. ADR-0001
   preserved (no alloc, no lock, no blocking).

2. **Non-RT side** — a detached watchdog thread spawned right after
   `AudioOutputUnitStop`'s sibling `start()`. It sleeps a bounded grace window
   (`SILENCE_GRACE`, 2 s) in short increments, then, if and only if
   `non_silence_seen` is still `false` **and** the stream has not reached a
   terminal state (it's genuinely running, not stopped/errored) **and** teardown
   has not begun, emits one `log::warn!` pointing at the likely
   `kTCCServiceAudioCapture` / `NSAudioCaptureUsageDescription` cause and how to
   check it. It then exits. It never touches the RT thread and never mutates
   stream state.

3. **Teardown** — a shared `Arc<AtomicBool> watchdog_stop` (set in
   `stop_audio_unit` and `Drop`) makes the watchdog exit promptly and suppresses
   a spurious warning when the user stops within the grace window. The thread is
   detached (self-terminating within one sleep increment of the stop flag or the
   grace deadline), so it imposes no join on the struct's documented drop-order
   contract.

The warning fires **at most once** per capture. There is **no `AudioError`**,
no state change, no behavioral difference for a correctly-permissioned or
legitimately-silent capture beyond one atomic load per callback.

## 4. Consequences

- A denied-permission Process-Tap capture now says so in the log within ~2 s,
  instead of silently streaming zeros forever. This is the single highest-value
  diagnostic for the platform's most confusing failure mode.
- A legitimately-silent capture (paused/muted) also logs the warning once. This
  is an accepted false-positive: it is a `warn`, it is one line, and its text is
  explicit that silence *may* be legitimate — it does not fail or degrade the
  capture. Consumers that expect silence can filter the target or lower the log
  level.
- Always-on (no feature gate), matching the always-on device-alive listener
  (ADR-0010) — an honest diagnostic should not be something a consumer has to
  opt into. Cost when audio is flowing is one relaxed atomic load per callback.
- Pairs with ADR-0015: the opt-in preflight is the *proactive* UX signal; this
  runtime guard is the *always-available* authoritative backstop. Neither is
  required by the other.

## 5. Alternatives considered

- **Escalate to `AudioError::PermissionDenied` on sustained silence.** Rejected:
  indistinguishable from a legitimately silent source at the sample level, so it
  would break valid captures. The existing `PermissionDenied` variant is still
  produced on the *explicit* CoreAudio `'hog!'` permission OSStatus
  (`coreaudio.rs` `map_ca_error`) — that path is a real error and stays an error.
- **Compute RMS/dBFS instead of a non-zero scan.** Rejected as overkill: the
  denied case is *bit-exact* zeros, so `any(|&s| s != 0.0)` is both sufficient
  and cheaper than the `AudioBuffer::rms` sum-of-squares. (The level meters
  remain available for consumers who want graded silence detection.)
- **Detect on the RT thread and signal inline.** Rejected: logging/sleeping is
  not RT-safe; the split (RT sets a flag, non-RT decides + warns) is the only
  ADR-0001-compatible shape.
- **Feature-gate it.** Rejected: an honest diagnostic belongs on by default
  (device-alive listener precedent). The cost is negligible.

## 6. References

- [ADR-0001](0001-rt-allocation-guarantee.md) — RT-allocation guarantee (the
  constraint on the callback-side detection).
- [ADR-0010](0010-producer-terminal-signal.md) — the always-on device-alive
  listener (precedent for an always-on, non-RT macOS watchdog).
- [ADR-0015](0015-macos-tcc-audiocapture-preflight.md) — the opt-in proactive
  preflight this backstops.
- Seed `rsac-4c3b`.

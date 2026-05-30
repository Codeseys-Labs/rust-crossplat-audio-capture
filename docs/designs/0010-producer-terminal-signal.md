# ADR 0010 — Producer-side terminal-signal contract

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/bridge/ring_buffer.rs` (`BridgeProducer::signal_error`), the three
platform backends (`src/audio/windows/thread.rs`, `src/audio/linux/thread.rs`,
`src/audio/macos/thread.rs` + `src/audio/macos/coreaudio.rs`)
**Verdict:** Every backend MUST drive the shared bridge state to a terminal/ending
state when its producer stops — gracefully (`signal_done` → `Stopping`) or fatally
(`signal_error` → `Error`). Add `signal_error()` as the fatal sibling of the existing
`signal_done()`.

## 1. Context

The bridge between the OS audio callback (producer) and the user/reader thread is a
lock-free SPSC ring guarded by an [`AtomicStreamState`](../../src/bridge/state.rs).
Readers decide *when to stop* purely from that state:

- the **blocking** reader `BridgeConsumer::pop_blocking`
  ([`src/bridge/ring_buffer.rs`](../../src/bridge/ring_buffer.rs)) returns the Fatal
  `AudioError::StreamEnded` only when `is_terminal()` is true (`Stopped`/`Closed`/`Error`);
- the **async** reader `AsyncAudioStream::poll_next`
  ([`src/bridge/async_stream.rs`](../../src/bridge/async_stream.rs)) ends with
  `Poll::Ready(None)` when `is_stream_producing()` is false (anything past `Running`).

If the producer stops but **no one moves the state**, the readers never observe an end:
`pop_blocking` busy-waits until an outer timeout and the async stream parks forever.

Only the **Windows** WASAPI backend was correct: `wasapi_capture_thread_main` calls
`producer.signal_done()` on every thread-exit path, including the clean exit
([`src/audio/windows/thread.rs`](../../src/audio/windows/thread.rs)). **Linux**
(PipeWire) and **macOS** (CoreAudio) did **not** signal the bridge when the producer
stopped or died, so a Linux/macOS reader hung indefinitely on:

- a device unplug / source-node removal (PipeWire `Unconnected`);
- a daemon/proxy death (PipeWire `Error`);
- a normal capture-loop teardown (PipeWire `StopCapture`/`Shutdown`/disconnect);
- a CoreAudio `stop_capture()` **or** merely dropping the stream handle.

This is finding **FH-1**. It is also the prerequisite for the Wave-C async-binding
terminal-error delivery work: terminal-error delivery is only end-to-end observable
once Linux/macOS actually reach a terminal state.

`signal_done()` alone is **insufficient** for a *dead* producer: it transitions
`Running → Stopping`, and `Stopping` is still *readable/drainable* — so
`read_buffer_blocking()` keeps draining (and then waiting) forever when no more data
can ever arrive. We need a distinct **fatal** signal that lands the state in the
terminal `Error`.

## 2. Decision drivers

- The state machine is the single source of truth both readers consult; a stopped
  producer must be reflected there or readers cannot terminate.
- "Stream ended gracefully (drain the tail)" and "producer died (nothing more is
  coming)" are semantically different and map to different states
  (`Stopping` vs `Error`) and different reader behavior (drain vs end-now). This is the
  same graceful-vs-fatal distinction ADR-0003 draws for `StreamReadError` vs
  `StreamEnded`.
- The signal can be invoked from a platform callback context (PipeWire
  `.state_changed`, a possible CoreAudio property listener), so it MUST stay
  lock-free and allocation-free (ADR-0001).
- Cross-backend uniformity: the reader contract must hold on all three backends, not
  just Windows.

## 3. Considered options

### Option A — Poll the platform `is_active()` inside `is_stream_producing()`
Have the async/blocking readers ask the platform stream whether the OS unit is still
active, instead of relying on the bridge state.
- ➖ `PlatformStream::is_active()` for macOS locks a `Mutex` (and Windows/Linux read a
  flag), but `is_stream_producing()` is on the **hot async-poll path** and must stay
  lock-free — this would put a lock on every poll.
- ➖ Inverts the layering (the lock-free data-plane state would depend on the
  platform layer) and does nothing for the blocking reader's `pop_blocking`, which only
  consults `BridgeShared`.
- ➖ Does not capture *fatal vs graceful* — `is_active() == false` cannot say whether
  the tail should still drain. **Rejected.**

### Option B — Push terminal into `BridgeShared` state from each backend (CHOSEN)
Add `BridgeProducer::signal_error()` (force `Running`/any → `Error`, terminal) as the
fatal sibling of `signal_done()` (CAS `Running → Stopping`, graceful). Each backend
calls the appropriate one on producer stop/death. Readers are unchanged — they already
branch on `is_terminal()` / `is_stream_producing()`.
- ➕ The readers already do the right thing once the state is set; no hot-path change.
- ➕ Lock-free + alloc-free (a single atomic state store + a waker wake), so it is safe
  from a callback context (ADR-0001).
- ➕ Captures the graceful-vs-fatal distinction precisely: `Stopping` keeps draining,
  `Error` ends both readers immediately.
- ➖ Each backend must wire its own stop/death hooks (per-backend code, below).

## 4. Decision

**Option B.** Add `BridgeProducer::signal_error(&self)` that force-sets the state to
the terminal `StreamState::Error` and wakes the async waker, factoring the existing
terminal-poison tail of `on_push_panic()` so there is a single poison path. Keep
`signal_done()` for the graceful `Running → Stopping` end.

Per-backend hooks:

- **Windows** (`src/audio/windows/thread.rs`): unchanged — `signal_done()` already
  fires on every `wasapi_capture_thread_main` exit (init-failure paths and the clean
  exit). WASAPI has no spontaneous-callback in-flight window; the dedicated capture
  thread's exit is the single choke point.

- **Linux** (`src/audio/linux/thread.rs`): two hooks.
  1. **Spontaneous death** — a `.state_changed` listener arm on the PipeWire stream
     calls `user_data.producer.signal_error()` when the new
     `pipewire::stream::StreamState` is `Error(_)` (daemon/proxy death) or
     `Unconnected` (node removal / disconnect). Benign transitions
     (`Connecting`/`Paused`/`Streaming`) and the normal connect handshake are no-ops, so
     a transient startup state cannot falsely poison the stream.
  2. **Graceful clean exit** — a clone of the active session's `Arc<BridgeShared>` is
     retained on the PipeWire thread (`active_shared`, set in `StartCapture`, cleared in
     `StopCapture`) and driven `Running → Stopping` (the helper
     `signal_session_graceful_end`) before tearing down the listener/stream in the
     `StopCapture`, `Shutdown`, and command-channel-disconnect arms, plus a final
     safety-net call after the loop. No signal is emitted on the init-failure early
     returns: the producer only enters `user_data` on the successful path, so a
     pre-producer failure has no reader to wake (matches Windows).

- **macOS** (`src/audio/macos/thread.rs` + `coreaudio.rs`): the platform stream holds a
  cloned `Arc<BridgeShared>` (`terminal`), plumbed from `create_stream` (where
  `consumer.shared()` is in scope) through `create_macos_capture`. In
  `stop_audio_unit()` — the single choke point reused by both `stop_capture()` and
  `Drop` — the bridge is driven `Running → Stopping` **after** `au.stop()` returns.
  `AudioOutputUnitStop` is synchronous (no further input callbacks fire after it
  returns), so signaling *after* it is race-free; signaling *before* could let an
  in-flight callback push past the declared end. Dropping the handle (not just calling
  `stop()`) therefore also lands the stream terminal.

  *Optional follow-up (not in this change):* registering an
  `AudioObjectAddPropertyListener` on `kAudioDevicePropertyDeviceIsAlive` for the
  device/aggregate id and calling `signal_error()` on the not-alive callback would also
  cover spontaneous device/tap death **without** a `stop()` call. Until then, that rare
  case (a device dies while the user neither stops nor drops the handle) remains a
  documented known limitation; the `stop_capture`/`Drop` hook already removes the common
  teardown hang.

`signal_error()`/`signal_done()`/the Linux teardown transition/the macOS transition are
all **idempotent** (`force_set` is last-writer-wins; the `transition` CAS no-ops if the
state already advanced past `Running`) and **sticky** (terminal `Error` cannot be
downgraded by a late graceful signal), so a spontaneous death racing an explicit stop is
harmless.

## 5. Consequences

- A Linux/macOS reader (blocking or async) now terminates on producer stop/death
  instead of hanging — the FH-1 hang is removed for the common teardown paths and for
  PipeWire spontaneous death.
- `signal_error()` is **lock-free + alloc-free** (one `Release` atomic store + a waker
  wake), preserving the ADR-0001 RT-allocation guarantee; it is safe to call from a
  PipeWire `.state_changed` callback. The Linux/macOS RT *data* callbacks
  (`.process` / the CoreAudio input callback) are **not** changed and continue to do
  only the lock-free `push_samples_or_drop`.
- No new `AudioError` variant — the terminal state maps to the existing Fatal
  `StreamEnded` per ADR-0003, so `recoverability()` is untouched and stays exhaustive.
- No public/FFI signature change: `signal_error()` is a new `pub` method on the internal
  `BridgeProducer`, the Linux `active_shared`/`session_shared` and macOS `terminal`
  field are internal plumbing, and `create_macos_capture` is `pub(crate)`.
- The remaining macOS spontaneous-death-without-stop case is a documented known
  limitation pending the optional `DeviceIsAlive` listener.

## 6. References

- Finding **FH-1** (2026-05-30 backlog blueprints,
  [`docs/reviews/rsac-backlog-blueprints-2026-05-30.md`](../reviews/rsac-backlog-blueprints-2026-05-30.md),
  *producer-terminal-signal* section).
- [ADR-0001](0001-rt-allocation-guarantee.md) — RT-allocation guarantee: the terminal
  signal must stay lock-free + alloc-free to be callable from a callback context.
- [ADR-0002](0002-callback-delivery.md) — callback delivery: which thread invokes the
  terminal signals (Windows capture thread; PipeWire loop thread for `.state_changed`
  and teardown; CoreAudio caller thread for `stop_audio_unit`).
- [ADR-0003](0003-terminal-stream-error.md) — terminal stream end vs recoverable read
  errors: terminal state ⇒ Fatal `StreamEnded` (blocking) / `Poll::Ready(None)` (async);
  this work relies on that mapping unchanged.
- `BridgeProducer::signal_error`/`signal_done` in
  [`src/bridge/ring_buffer.rs`](../../src/bridge/ring_buffer.rs); the per-backend hooks
  in [`src/audio/windows/thread.rs`](../../src/audio/windows/thread.rs),
  [`src/audio/linux/thread.rs`](../../src/audio/linux/thread.rs),
  [`src/audio/macos/thread.rs`](../../src/audio/macos/thread.rs), and
  [`src/audio/macos/coreaudio.rs`](../../src/audio/macos/coreaudio.rs).
- `pipewire::stream::StreamState` (variants `Error(String)`, `Unconnected`, `Connecting`,
  `Paused`, `Streaming`) and `ListenerLocalCallbacks::state_changed`
  (`FnMut(&Stream, &mut D, StreamState, StreamState)`) — pipewire-rs 0.9 API.

# ADR 0004 — Device-change-notification delivery model (per-platform)

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/core/interface.rs` (`DeviceEvent`, `DeviceEventHandler`,
`DeviceEnumerator::watch`), `src/audio/windows/wasapi.rs`,
`src/audio/macos/coreaudio.rs`, `src/audio/linux/thread.rs`,
`src/core/capabilities.rs` (`supports_device_change_notifications`)
**Verdict:** `DeviceEnumerator::watch` delivers `DeviceEvent`s on a backend OS
notification thread — **never** the RT audio callback thread. Windows and macOS
hand off through a bounded `sync_channel(64)` to a dedicated helper thread;
Linux invokes the handler **directly on the PipeWire loop thread**. Full-channel
events are **dropped (drop-newest)**. The Linux divergence is intentional and
documented, not unified — see §5.

## 1. Context

`M10` added live device hot-plug / default-change notifications via
`DeviceEnumerator::watch(on_event) -> AudioResult<DeviceWatcher>`
(`interface.rs`). Each platform wires a real OS listener (so
`PlatformCapabilities::supports_device_change_notifications` is `true` on all
three backends — `capabilities.rs`, not a stub), translates native events into
the `#[non_exhaustive]` `DeviceEvent` enum (`DeviceAdded` / `DeviceRemoved` /
`DefaultChanged` / `StateChanged`), and feeds them to the user
`DeviceEventHandler` (`Box<dyn FnMut(DeviceEvent) + Send + 'static>`).

The trait contract (`DeviceEnumerator::watch` doc, `interface.rs`) promises only
that the handler runs on the backend's **OS notification thread**, never the
real-time audio callback thread — so the handler **may allocate and lock**. That
is the load-bearing guarantee for consumers: device-watch is explicitly *not* on
the RT path, unlike the audio producer governed by ADR-0001.

Beyond that one guarantee, the three backends differ in *how* the handler is
reached, and the divergence was shipped without a recorded decision. This ADR
records it (the 2026-05-30 architecture critique flagged the missing ADR as the
highest-priority ADR gap):

- **Windows** (`wasapi.rs`, `WatcherNotificationClient` + `watch`): an
  `IMMNotificationClient` registered with
  `RegisterEndpointNotificationCallback`. COM invokes the client on the MMDevice
  *system notification thread*. The client only translates + enqueues onto a
  bounded `mpsc::sync_channel::<DeviceEvent>(DEVICE_EVENT_CHANNEL_BOUND)`
  (`DEVICE_EVENT_CHANNEL_BOUND = 64`); a dedicated `rsac-wasapi-device-watch`
  helper thread owns the receiver **and the user `FnMut`** and runs it off the
  COM thread.
- **macOS** (`coreaudio.rs`, `watch_listener_proc` + `watch`): three
  `AudioObjectAddPropertyListener` registrations (device list, default output,
  default input). CoreAudio invokes the proc on one of its own threads; the proc
  diffs/translates and `try_send`s onto a bounded
  `sync_channel(WATCH_CHANNEL_CAP)` (`WATCH_CHANNEL_CAP = 64`); a dedicated
  `rsac-macos-device-watch` helper thread owns the receiver and the user `FnMut`.
- **Linux** (`thread.rs`, `spawn_device_watcher` / `watch_thread_main`): a
  persistent PipeWire `MainLoop` + `Context` + `Core` + `Registry` plus a bound
  `default` metadata proxy, all living on a dedicated `rsac-pw-watch` thread. The
  registry `global` / `global_remove` and metadata `property` callbacks invoke
  the user closure **directly, inline, on that loop thread** (the handler is
  held in `Rc<RefCell<DeviceEventHandler>>` and called as
  `(on_event.borrow_mut())(event)`). There is **no channel and no separate
  helper thread**: the persistent loop thread *is* the delivery thread.

So the channel hand-off, the channel bound, and the full-channel event-loss
policy all exist on Windows/macOS but not on Linux — a real per-platform
behavioral difference that consumers can observe under handler stalls.

## 2. Decision drivers

- **Hard contract first.** The handler must never run on the RT audio callback
  thread (that is the ADR-0001 invariant). Whatever the delivery shape, this
  must hold on every platform.
- **Never block / wedge the OS notification mechanism.** A COM
  `IMMNotificationClient` method or a CoreAudio property proc that blocks (e.g.
  on a slow user closure) can stall the *system* notification thread —
  unacceptable. The user closure must therefore not run inline on those threads.
- **Honesty over forced symmetry.** PipeWire's loop objects
  (`MainLoop`/`Context`/`Core`/`Registry`, all `Rc`/`!Send`) cannot leave their
  owning thread, which makes a same-thread invocation the *natural* shape on
  Linux. Forcing a channel hand-off there would add machinery for no safety win
  (the loop thread is already a non-RT notification thread). Per ADR-0001/0002,
  rsac documents real per-platform behavior rather than papering over it.
- **Device events are rare and idempotent.** Hot-plug / default-switch happen at
  human timescales; a consumer that misses one can re-enumerate. Losing a
  redundant notification is far better than deadlocking the OS notify mechanism.
- **Bounded memory.** An unbounded queue would let a stalled handler grow memory
  without limit; the buffer must be bounded.

## 3. Considered options

### Option A — Unify all platforms on bounded-channel + helper-thread hand-off

Run a `sync_channel(N)` + dedicated helper thread on Linux too, marshaling
events off the PipeWire loop thread before invoking the user closure.
- ➕ Identical observable threading model and identical drop policy on all three
  platforms; one mental model for consumers.
- ➖ Pure overhead on Linux: the PipeWire loop thread is *already* a dedicated,
  non-RT notification thread that nothing else blocks, so invoking the handler
  inline there is already safe. A channel + extra thread would add a hop and a
  second thread to join at teardown for no correctness benefit. The `Rc`/`!Send`
  loop objects already pin everything to one thread; the only thing that crosses
  out is each owned `DeviceEvent`. Rejected as machinery without a payoff.

### Option B — Invoke the user handler inline on the OS notify thread everywhere

Call the user `FnMut` directly from the COM `IMMNotificationClient` method and
the CoreAudio proc, like Linux does.
- ➖ A slow/blocking/allocating user closure would run on the COM system
  notification thread or inside a CoreAudio property proc. Blocking COM's notify
  thread can wedge device notifications process-wide; a CoreAudio proc that
  blocks or re-enters CoreAudio in a waiting way risks deadlock (the proc doc in
  `coreaudio.rs` explicitly warns against it). The handler contract allows the
  user to allocate and lock, which is incompatible with those threads. Rejected
  on Windows/macOS.

### Option C — Per-platform: helper-thread on Windows/macOS, direct-invoke on Linux (CHOSEN)

Use the bounded-channel + dedicated-helper-thread hand-off where the OS notify
thread is hostile to user code (Windows COM, macOS CoreAudio proc), and
direct-invoke on the PipeWire loop thread where same-thread invocation is the
natural, already-safe shape. Document the divergence in the `watch()` trait
contract so consumers branch on real behavior.
- ➕ Honours the hard contract on every platform (handler never on the RT
  thread; never blocks the COM/CoreAudio notify thread). Zero needless machinery
  on Linux. Matches each backend's native ownership model.
- ➖ The observable threading model, the channel bound, and the full-buffer
  drop behavior are **not** identical across platforms — a documented divergence,
  not parity. Recorded here so it is decision-backed.

### Channel-policy sub-decisions (Windows/macOS hand-off)

- **Bounded vs unbounded:** bounded. `sync_channel(64)` on both. An unbounded
  channel lets a stalled handler grow memory without limit; a small bound is
  ample because device events are rare.
- **Capacity = 64** (`DEVICE_EVENT_CHANNEL_BOUND` / `WATCH_CHANNEL_CAP`): far
  more headroom than the burst from a single hot-plug or default switch, while
  staying trivially small.
- **Drop-newest on full vs drop-oldest vs block:** **drop-newest.** On a full
  channel the producer (`WatcherNotificationClient::emit` on Windows;
  `try_push_event` on macOS) `try_send`s and, on `TrySendError::Full`, logs and
  **drops the just-arrived event** rather than blocking. Blocking is rejected
  (it would stall the COM / CoreAudio notify thread — Option B's failure mode).
  Drop-oldest was not chosen: `std`'s `SyncSender` has no atomic
  replace-oldest, and since a device change is idempotent (the consumer can
  re-enumerate) dropping the newest is equally recoverable and needs no extra
  structure.

## 4. Decision

**Option C**, with the channel sub-decisions above.

- **Trait contract** (`DeviceEnumerator::watch`): the handler runs on the
  backend's OS notification thread, **never** the RT audio callback thread, so it
  may allocate and lock. Dropping the returned `DeviceWatcher` unregisters the OS
  listener and joins the notify thread, after which the handler will not run
  again (the RAII teardown ordering is ADR-0005). `watch` is a *provided* trait
  method defaulting to `AudioError::PlatformNotSupported`; backends whose
  `supports_device_change_notifications` is `false` inherit the default.
- **Windows / macOS:** OS listener → bounded `sync_channel(64)` → dedicated
  helper thread (`rsac-wasapi-device-watch` / `rsac-macos-device-watch`) that
  owns the user `FnMut`. Full channel → drop-newest + log. The OS notify thread
  only translates + enqueues; it never runs user code.
- **Linux:** persistent `rsac-pw-watch` PipeWire loop thread invokes the user
  `FnMut` **directly inline** from the registry / metadata callbacks
  (`Rc<RefCell<…>>`, single-thread, no channel). The loop thread is the delivery
  thread.
- **Capabilities honesty:** all three backends set
  `supports_device_change_notifications: true` (`capabilities.rs`), gated on both
  `target_os` and the matching `feat_*` feature; the no-backend build reports
  `false` and `watch` returns `PlatformNotSupported`.

## 5. Consequences

- **Parity is partial, by design.** Consumers get one guarantee everywhere (not
  the RT thread; may allocate/lock) but **must not** assume an identical
  threading model: on Windows/macOS the handler runs on a private helper thread
  with a 64-deep bounded buffer that drops the newest event under stall; on Linux
  it runs synchronously on the PipeWire loop thread with no buffer. A handler
  that blocks on Linux stalls the loop thread (and thus further PipeWire event
  processing for that watcher) directly; on Windows/macOS a blocking handler only
  backs up the bounded channel until events start dropping. Handlers should
  return promptly regardless.
- **Event loss under stall is allowed (Windows/macOS) and silent (logged at
  `warn`).** Because device changes are idempotent, a consumer that needs the
  authoritative current state after any event should re-enumerate via
  `enumerate_devices()` rather than reconstructing state purely from the event
  stream. On Linux there is no buffer to overflow, but a slow handler delays
  subsequent callbacks instead.
- **Linux reports the existing device set on subscribe.** The initial PipeWire
  registry dump fires `DeviceAdded` for every device already present when
  `watch()` is called, then live changes thereafter (`watch_thread_main` note).
  Windows/macOS deliver only changes after registration. Consumers wanting a
  consistent starting snapshot should `enumerate_devices()` once at subscribe
  time on all platforms.
- **`DeviceEvent` is `#[non_exhaustive]`** so new variants can be added in a
  minor release; consumer matches need a trailing `_ =>` arm.
- **Future-parity is an open option, not a commitment.** If a consumer need for
  identical cross-platform threading emerges, the path is Option A (wrap the
  Linux loop thread in the same bounded-channel + helper-thread shape). Until
  then the divergence is the documented contract, surfaced through the `watch()`
  doc and `PlatformCapabilities`.

## 6. References

- 2026-05-30 architecture critique, finding *"Device-watch threading model is
  inconsistent across platforms and has no ADR"* (HIGH, adr-review) and the
  *"GAP: device-watch threading model"* ADR-gap entry — recorded as the
  highest-priority ADR gap.
- `src/core/interface.rs` — `DeviceEvent`, `DeviceEventHandler`,
  `DeviceEnumerator::watch` contract + provided default.
- `src/audio/windows/wasapi.rs` — `WatcherNotificationClient::emit`
  (`try_send` drop-newest), `DEVICE_EVENT_CHANNEL_BOUND = 64`,
  `DeviceEnumerator::watch` helper-thread hand-off.
- `src/audio/macos/coreaudio.rs` — `watch_listener_proc`, `try_push_event`
  (`try_send` drop-newest), `WATCH_CHANNEL_CAP = 64`, `DeviceEnumerator::watch`.
- `src/audio/linux/thread.rs` — `spawn_device_watcher`, `watch_thread_main`
  (direct inline invocation on the PipeWire loop thread; `Rc<RefCell<…>>`
  handler).
- `src/core/capabilities.rs` — `supports_device_change_notifications`
  per-platform.
- Companion: ADR-0005 (DeviceWatcher RAII teardown / lifecycle). RT-thread
  prohibition shared with ADR-0001 (RT-allocation guarantee).

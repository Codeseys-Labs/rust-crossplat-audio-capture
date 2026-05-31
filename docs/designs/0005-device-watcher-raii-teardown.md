# ADR 0005 — `DeviceWatcher` RAII teardown / lifecycle contract

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/core/interface.rs` (`DeviceWatcher`), `src/audio/windows/wasapi.rs`
(`watch` teardown + `SendWatcherTeardown`), `src/audio/macos/coreaudio.rs`
(`watch` teardown + `SendContextPtr`), `src/audio/linux/thread.rs`
(`spawn_device_watcher` teardown)
**Verdict:** `DeviceWatcher` is an RAII guard whose `Drop` runs a **take-once**
`Option<Box<dyn FnOnce() + Send>>` teardown closure: it **unregisters the OS
listener first, then joins the notify thread**, and is **best-effort and never
panics**. On Windows the teardown also pins the COM apartment via
`Arc<ComInitializer>` so `CoUninitialize()` cannot race a live callback. On macOS
the in-flight-proc teardown race (CoreAudio gives no barrier that an executing
`watch_listener_proc` has finished when `AudioObjectRemovePropertyListener`
returns) is closed with an **intentional bounded leak** of the listener context
so a late proc always derefs valid memory (§5); the fully race-free
dispatch-queue rewrite is **deferred** (§5/§6). Pairs with ADR-0004.

## 1. Context

`DeviceEnumerator::watch` (ADR-0004) returns a `DeviceWatcher` that keeps an OS
device-change listener alive for as long as the caller holds it; dropping it
stops notifications. The public type (`interface.rs`) is deliberately
backend-agnostic:

```rust
pub struct DeviceWatcher {
    teardown: Option<Box<dyn FnOnce() + Send>>,
}
```

- The teardown is a **boxed `FnOnce` + `Send`** so the watcher type carries no
  platform detail and can be moved across threads.
- It is `Option<…>` so `Drop` can `take()` it **exactly once**
  (`from_teardown` sets `Some`; `Drop` does `if let Some(teardown) =
  self.teardown.take()`), making double-teardown impossible even though `Drop`
  may run during unwinding.

Each backend constructs the teardown closure inside its `watch` arm. Because the
closure runs in `Drop` — possibly while the thread is already unwinding from a
panic — it **must not panic** (a panic during unwind aborts the process). The
correct unregister/join *ordering* and the COM-apartment lifetime are subtle and
were scattered across doc-comments; the 2026-05-30 architecture critique called
for recording them in an ADR.

## 2. Decision drivers

- **No-panics-in-library** (`AGENTS.md` / `CLAUDE.md`): `Drop` is a panic
  hazard; every teardown step must be best-effort (`let _ = …`).
- **Run-once:** the OS unregister and the thread join must each happen at most
  once regardless of how `Drop` is reached.
- **Correct ordering:** after `drop` returns the handler must be guaranteed not
  to run again — which requires stopping the *source* of events before reclaiming
  the *consumer* of events, then waiting for the consumer to finish.
- **Send teardown across `!Send` backends:** the public `FnOnce` is `Send`, but
  the COM (`IMM*`) and CoreAudio (`*mut WatchListenerContext`) objects are not
  `Send` in their crates, so each backend needs a scoped, justified
  `unsafe impl Send` holder.
- **COM apartment lifetime (Windows):** the enumerator that `watch(&self)` was
  dispatched on may be dropped the instant `watch` returns (the legal
  `get_enumerator()?.watch(handler)?` one-liner), which would run
  `CoUninitialize()` while a callback is still registered. The teardown must pin
  the apartment independently.

## 3. Considered options

### Option A — `Drop`-with-take-once `Option<Box<dyn FnOnce() + Send>>` (CHOSEN)
The watcher holds an `Option<Box<dyn FnOnce() + Send>>`; `Drop` takes it once and
calls it; each backend builds a closure that (1) unregisters the OS listener,
(2) signals + joins the notify thread, best-effort and panic-free.
- ➕ Backend-agnostic public type; impossible to run twice; natural RAII; each
  backend encapsulates its own platform unsafe behind a tiny `Send` holder.
- ➖ The closure is opaque (`Debug` can only report `active: bool`); each backend
  must independently get ordering + panic-safety right (mitigated by this ADR +
  per-backend lifecycle tests).

### Option B — A `Drop`-implementing per-platform enum/struct inside `DeviceWatcher`
Make `DeviceWatcher` an enum over `{ Windows(..), Macos(..), Linux(..), Inactive }`
each with its own `Drop`.
- ➖ Leaks platform types into the public surface (or needs `cfg` soup), couples
  `core` to backend types (a module-DAG hazard), and still needs the same
  run-once + ordering + no-panic discipline. More surface for no benefit.
  Rejected.

### Option C — Explicit `stop()`/`close()` method, no RAII
- ➖ A forgotten `stop()` leaks the OS listener + the notify thread; a dropped
  watcher would silently keep delivering. RAII is the safer default for a
  resource whose whole purpose is bounded lifetime. Rejected (an explicit method
  could still be added later as a convenience that defers to the same closure).

## 4. Decision

**Option A.** `DeviceWatcher { teardown: Option<Box<dyn FnOnce() + Send>> }`,
`Drop` takes once and invokes; the closure is best-effort and never panics. The
per-backend teardown ordering is **unregister the OS listener first, then stop +
join the notify thread**:

- **Windows** (`wasapi.rs`): (1) `UnregisterEndpointNotificationCallback` (no
  further `OnDevice*` after it returns), best-effort (`let _ = …`); (2) set the
  `shutdown` `AtomicBool` and `join()` the `rsac-wasapi-device-watch` helper
  (bounded by `NOTIFY_THREAD_POLL_INTERVAL`), ignoring a poisoned-join error;
  (3) drop the `SendWatcherTeardown` holder **last** — it owns the
  `IMMNotificationClient` (and thus the `SyncSender`) **and** an
  `Arc<ComInitializer>` clone that **pins the COM apartment** for the watcher's
  whole life. Because `watch(&self)` runs on a *borrowed* enumerator, the
  `get_enumerator()?.watch(handler)?` pattern drops the `WindowsDeviceEnumerator`
  (and its apartment initializer) as soon as `watch` returns; without this
  independent `Arc` clone, `CoUninitialize()` could run while the
  `IMMNotificationClient` is still registered. Dropping the holder only after
  Unregister + join guarantees the ordering: **stop callbacks → reclaim consumer
  → release apartment**, never the reverse. The `Send` of the holder is asserted
  via `unsafe impl Send for SendWatcherTeardown {}`, scoped to the
  free-threaded MTA objects (`COINIT_MULTITHREADED`). The `com_initializer`
  field is load-bearing for its `Drop`, not its value: it is moved into the
  teardown closure inside `teardown_state` and released by the closure's final
  `drop(teardown_state)`, so the apartment outlives Unregister + join even
  though the field is never read.

- **macOS** (`coreaudio.rs`): (1) `AudioObjectRemovePropertyListener` for all
  three `WATCH_ADDRESSES` (best-effort — stops *new* notifications); (2) **take
  the `SyncSender` out of the context** — `event_tx` is a
  `Mutex<Option<SyncSender<DeviceEvent>>>`, and teardown sets it to `None`
  (recovering a poisoned lock, never `unwrap`ping), dropping the last live sender
  so the `rsac-macos-device-watch` helper's `recv()` returns `Err` and its loop
  ends; (3) `join()` the helper, ignoring a panicked-handler join error. The
  `WatchListenerContext` itself is **intentionally leaked** (`Box::into_raw`,
  reclaimed only on the pre-add construction-error path — see §5), so step (2)
  disconnects delivery *without* freeing the allocation a late proc may still
  deref. The raw `*mut WatchListenerContext` crosses into the closure inside
  `SendContextPtr` (`unsafe impl Send`), captured *whole* so Rust 2021
  disjoint-capture does not strip the wrapper's `Send`; it now identifies the
  listener registration and addresses the leaked context for step (2), but no
  longer owns/keeps-alive a `Box` (there is none).

- **Linux** (`thread.rs`, `spawn_device_watcher`): the closure owns the
  `JoinHandle` in an `Option` (single owner → cannot double-join) and a shared
  `shutdown` `AtomicBool`. Teardown sets the flag; the `rsac-pw-watch` loop
  notices on its next `iterate(50 ms)` tick and exits, dropping its `Rc`-owned
  PipeWire objects (which unregisters the registry + metadata listeners via
  RAII); then `handle.take().join()`, best-effort. There is no separate
  channel/helper-thread to reclaim because (ADR-0004) the loop thread *is* the
  delivery thread.

In all three, every fallible step is `let _ = …` so `Drop` never panics, and the
take-once `Option` makes the whole teardown idempotent.

## 5. Consequences

- **`drop` returns ⇒ handler will not run again** holds on **Windows** and
  **Linux**: Windows unregisters before joining the only thread that calls the
  user `FnMut`; Linux stops + joins the loop thread that calls it. Both reclaim
  the consumer thread, so no handler invocation can outlive `drop`. On **macOS**
  the user handler runs only on the helper thread, which teardown joins after
  disconnecting the channel, so the *handler* likewise cannot run after `drop`
  returns; the residual concurrency is the CoreAudio *proc* (which never calls
  the user handler — it only pushes onto the now-disconnected channel and
  no-ops). See the next bullet.

- **macOS in-flight-proc race — FIXED via an intentional bounded leak (H1 /
  PS-1).** CoreAudio's PROC-based listener gives **no** guarantee that an
  already-executing `watch_listener_proc` has finished when
  `AudioObjectRemovePropertyListener` returns (Apple's docs promise only that no
  *new* notifications fire), and the proc dereferences its `client_data`
  context. The previous design `drop(context)` immediately after removing the
  listeners, leaving a **use-after-free window** for a proc that began before the
  drop. There is no app-side barrier that closes this window safely — locking
  across the destructor/remove gap can deadlock on the HAL's own internal
  recursive mutex.

  The race is now closed by making the deref **always valid**: the
  `WatchListenerContext` is **intentionally leaked** (`Box::into_raw`; never
  reclaimed on the success or spawn-failure paths — reclaimed only on the pre-add
  construction-error path, where no listener was ever registered so no proc can
  fire). A late or in-flight proc therefore always dereferences valid, `'static`
  memory. Event **delivery** is stopped not by freeing the context but by
  disconnecting the channel: `event_tx` is a `Mutex<Option<SyncSender>>` and
  teardown takes the sender out (`None`); a proc that fires afterward finds the
  sender taken (or the channel `Disconnected`) and is a no-op
  (`try_push_event`). The proc's old safety comment ("the context outlives every
  listener; teardown removes the listeners before freeing it") **overstated** the
  guarantee and has been rewritten to state the actual leak-based invariant.

  **Residual tradeoff:** a bounded one-time leak per `watch()`/drop cycle — one
  `WatchListenerContext` (a sender slot plus a `HashSet` of device ids, tens of
  bytes). Watchers are long-lived and few, so this is acceptable as a stopgap.

  **Deferred proper fix (tracked, `deferred-review`):** migrate to
  `AudioObjectAddPropertyListenerBlock` on a **self-owned serial dispatch
  queue**, removing the listener on that same queue (the Chromium/Itsuki
  pattern). Because removal and dispatch are serialised on one queue, this
  eliminates **both** the race and the leak. It is deferred because it is a
  larger change (a different CoreAudio API surface + a dispatch-queue lifecycle)
  than the one-wave safety stopgap. Tracked as a GitHub issue with the
  `deferred-review` label, mirroring the rigor of the Go UAF issue (#28); see §6.

- **No self-join hazard in the watcher teardowns.** The teardown runs on the
  thread that drops the `DeviceWatcher`, which is never the notify thread it
  joins (Windows/macOS helper, Linux loop thread) under normal use, so the join
  cannot deadlock on itself. (This is unlike the `CallbackPump`, which guards an
  explicit `thread().id() == current().id()` self-join case in `api.rs` because a
  pump can stop itself from inside its own callback; `DeviceWatcher` has no such
  reentrant path.)

- **Drop is panic-free by contract.** Every teardown step ignores errors
  (`let _ = …`); a panicking *user handler* is contained on the notify thread and
  surfaces only as a join error that teardown swallows — it never propagates
  through `Drop`. This upholds the no-panics-in-library rule even when `Drop`
  runs during unwinding.

- **Bounded teardown latency.** Windows join is bounded by
  `NOTIFY_THREAD_POLL_INTERVAL`; Linux join is bounded by the 50 ms iterate tick;
  macOS join completes once teardown takes the sender out of the context and the
  resulting channel disconnect ends the helper's `recv()`. None block
  unboundedly.

## 6. References

- 2026-05-30 architecture critique: *"macOS device-watch teardown has a
  use-after-free window on the listener context"* (HIGH, concurrency-threading —
  the §5 race, now **fixed via the intentional bounded leak**), and *"GAP:
  DeviceWatcher RAII teardown / lifecycle contract"* (folded into the
  device-watch ADR family).
- **Deferred proper fix (H1 / PS-1):** GitHub issue (`deferred-review` label) to
  migrate the macOS watcher to `AudioObjectAddPropertyListenerBlock` on a
  self-owned serial dispatch queue with remove-on-same-queue (Chromium/Itsuki
  pattern), eliminating both the leak and any residual race. Linked from the
  leak's `WatchListenerContext` doc comment in `coreaudio.rs`.
- Prior art for the stopgap: Hush #826 (*"deliberately leak … callback data that
  may still be referenced by the HAL"*); TypeWhisper #209 (app-side locking
  cannot bridge the destructor/remove gap and can deadlock on the HAL's recursive
  mutex); cpal's own admission that its trust-the-OS approach *"could lead to a
  use-after-free."*
- `src/core/interface.rs` — `DeviceWatcher` (take-once `Option<Box<dyn FnOnce()
  + Send>>`, `Drop`, `from_teardown`).
- `src/audio/windows/wasapi.rs` — `DeviceEnumerator::watch` teardown,
  `SendWatcherTeardown` + its `Arc<ComInitializer>` apartment pin.
- `src/audio/macos/coreaudio.rs` — `DeviceEnumerator::watch` teardown
  (unregister → take sender / disconnect → join), the intentionally leaked
  `WatchListenerContext` (`event_tx: Mutex<Option<SyncSender>>`),
  `SendContextPtr`, and `watch_listener_proc`'s leak-based deref invariant.
- `src/audio/linux/thread.rs` — `spawn_device_watcher` teardown (flag + join;
  RAII listener drop on the loop thread).
- Companion: ADR-0004 (device-change-notification delivery model — the threading
  model these teardowns reclaim; the helper-thread delivery model is unchanged by
  this fix). No-panics rule: `AGENTS.md` / `CLAUDE.md`.

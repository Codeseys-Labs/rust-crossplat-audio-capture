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
`Arc<ComInitializer>` so `CoUninitialize()` cannot race a live callback. The
macOS context-free ordering has a **known in-flight-proc teardown race**,
tracked as a limitation (§5), not claimed safe. Pairs with ADR-0004.

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
  free-threaded MTA objects (`COINIT_MULTITHREADED`), with the apartment pin
  held purely for its `Drop` (`#[allow(dead_code)]` on `com_initializer`).

- **macOS** (`coreaudio.rs`): (1) `AudioObjectRemovePropertyListener` for all
  three `WATCH_ADDRESSES`; (2) `drop(context)` — dropping the
  `WatchListenerContext` `Box` frees the `SyncSender`, disconnecting the channel
  so the `rsac-macos-device-watch` helper's `recv()` returns `Err` and its loop
  ends; (3) `join()` the helper, ignoring a panicked-handler join error. The raw
  `*mut WatchListenerContext` crosses into the closure inside `SendContextPtr`
  (`unsafe impl Send`), captured *whole* so Rust 2021 disjoint-capture does not
  strip the wrapper's `Send`.

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
  the consumer thread, so no handler invocation can outlive `drop`.

- **KNOWN LIMITATION (macOS in-flight-proc teardown race — tracked).** On macOS
  the teardown removes the listeners and then **immediately `drop(context)`**
  with **no barrier** for a CoreAudio proc that is *already executing* on a
  CoreAudio-managed thread. `AudioObjectRemovePropertyListener` does **not**
  guarantee an in-flight `watch_listener_proc` has finished, and that proc
  dereferences the context (`&*(client_data as *const WatchListenerContext)`).
  So a proc that began before the drop can touch freed memory after it — a
  **use-after-free window**. The proc's safety comment ("the context outlives
  every listener") therefore **overstates** the guarantee CoreAudio actually
  provides. This ADR records the teardown *contract*; it does **not** claim the
  macOS path is fully race-free. The fix (keep the context alive past listener
  removal / drain in-flight procs / `Arc`-`Weak`-guard the proc, and correct the
  overstated comment) is a **code** change tracked separately
  (2026-05-30 critique, HIGH, concurrency-threading). Until it lands, the
  "handler will not run again after `drop`" guarantee is **best-effort on
  macOS**, with a narrow UAF window under teardown concurrent with an active OS
  notification.

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
  macOS join completes once the disconnected channel ends the helper's `recv()`.
  None block unboundedly.

## 6. References

- 2026-05-30 architecture critique: *"macOS device-watch teardown has a
  use-after-free window on the listener context"* (HIGH, concurrency-threading,
  the §5 known limitation), and *"GAP: DeviceWatcher RAII teardown / lifecycle
  contract"* (folded into the device-watch ADR family).
- `src/core/interface.rs` — `DeviceWatcher` (take-once `Option<Box<dyn FnOnce()
  + Send>>`, `Drop`, `from_teardown`).
- `src/audio/windows/wasapi.rs` — `DeviceEnumerator::watch` teardown,
  `SendWatcherTeardown` + its `Arc<ComInitializer>` apartment pin.
- `src/audio/macos/coreaudio.rs` — `DeviceEnumerator::watch` teardown
  (unregister → `drop(context)` → join), `SendContextPtr`, `watch_listener_proc`
  context deref (the UAF window).
- `src/audio/linux/thread.rs` — `spawn_device_watcher` teardown (flag + join;
  RAII listener drop on the loop thread).
- Companion: ADR-0004 (device-change-notification delivery model — the threading
  model these teardowns reclaim). No-panics rule: `AGENTS.md` / `CLAUDE.md`.

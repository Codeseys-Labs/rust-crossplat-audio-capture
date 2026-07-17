# ADR 0005 — `DeviceWatcher` RAII teardown / lifecycle contract

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/core/interface.rs` (`DeviceWatcher`), `src/audio/windows/wasapi.rs`
(`watch` teardown + `WatchTeardown`), `src/audio/linux/thread.rs`
(`spawn_device_watcher` teardown)
**Verdict:** `DeviceWatcher` is an RAII guard whose `Drop` runs a **take-once**
`Option<Box<dyn FnOnce() + Send>>` teardown closure: it **unregisters the OS
listener first, then joins the notify thread**, and is **best-effort and never
panics**. On Windows the teardown also pins the COM apartment via
`Arc<ComInitializer>` so `CoUninitialize()` cannot race a live callback. On macOS
the in-flight-callback teardown race is now **RESOLVED** (rsac-e8aa / GH #32): the
watcher uses **block-based** listeners (`AudioObjectAddPropertyListenerBlock`) on
a **self-owned serial dispatch queue**, and teardown removes the block on that
same queue then dispatches a **synchronous no-op barrier** which — by serial-queue
FIFO ordering — cannot return until every in-flight block has finished, after
which the `Arc<WatchListenerContext>` is dropped (**freed, not leaked**). This
eliminates both the previously-intentional context leak and the race (§5) on
the expected path. If `AudioObjectRemovePropertyListenerBlock` itself reports a
failure (never observed; no documented failure mode for a registered listener),
teardown falls back to the pre-fix bounded leak for that watcher — block,
context, and queue are intentionally leaked and the helper detached, so a late
dispatch can never touch freed memory. The leak is thus confined to an
OS-reported removal failure instead of being unconditional. The
device-*alive* listener (`DeviceAliveContext`) remains on the PROC API +
`tearing_down` guard as a follow-up. Pairs with ADR-0004.

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

- **macOS** (`coreaudio.rs`, `WatchTeardown::run`, rsac-e8aa): (1)
  `AudioObjectRemovePropertyListenerBlock` for all three `WATCH_ADDRESSES` on the
  self-owned serial queue (no *new* block scheduled after it returns); (2) a
  **serial-queue sync barrier** (`queue.exec_sync(|| {})`) which cannot start
  until every in-flight notification block has finished, so on return no block is
  running or pending; (3) drop the block's `Arc<WatchListenerContext>` clone and
  the teardown's clone → the context is **freed** and its `SyncSender` drops, so
  the `rsac-macos-device-watch` helper's `recv()` returns `Err` and its loop
  ends; (4) `join()` the helper, ignoring a panicked-handler join error; (5) drop
  the queue last. Everything the teardown owns — the `DispatchRetained<
  DispatchQueue>`, the `RcBlock`, the `Arc`, and the `JoinHandle` — is bundled in
  a `WatchTeardown` newtype (`unsafe impl Send`) captured *whole* by the closure
  so Rust 2021 disjoint-capture does not strip the wrapper's `Send` (the block's
  `RcBlock` is otherwise `!Send`; it only ever *executes* on the owned queue).

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
  freeing the context (which drops the sender and disconnects the channel), so
  the *handler* likewise cannot run after `drop` returns; the residual
  concurrency is the CoreAudio notification *block* (which never calls the user
  handler — it only pushes onto the channel), and the barrier below guarantees no
  block runs after teardown either. See the next bullet.

- **macOS in-flight-callback race — RESOLVED via block listeners on a serial
  dispatch queue (rsac-e8aa / GH #32).** CoreAudio's PROC-based listener gave
  **no** guarantee that an already-executing `watch_listener_proc` had finished
  when `AudioObjectRemovePropertyListener` returned (Apple's docs promise only
  that no *new* notifications fire), and the proc dereferenced its `client_data`
  context — a **use-after-free window** for a proc that began before the drop.
  The earlier wave closed this with an **intentional bounded leak** of the
  context (never freeing it made a late deref always valid); that stopgap traded
  a per-cycle leak for soundness.

  This is now replaced by the race-free design the stopgap deferred:

  1. All three listeners register via **`AudioObjectAddPropertyListenerBlock`** on
     **one self-owned serial dispatch queue** (`dispatch2::DispatchQueue` with
     `DispatchQueueAttr::SERIAL`). Every notification block therefore runs
     serialized on that queue.
  2. The block captures an `Arc<WatchListenerContext>` clone, keeping the context
     alive for exactly as long as a block can run. Context is held by `Arc`, not
     leaked, and `event_tx` is a plain `Mutex<SyncSender>` (the `Option`/take
     dance is gone; the `Mutex` remains only because a `SyncSender` is `Send` but
     not `Sync` and the block is a shared `Fn`).
  3. Teardown, on the drop thread, runs **remove → barrier → free → join**: remove
     each block on the same queue (no *new* block scheduled after it returns),
     then `queue.exec_sync(|| {})` — a synchronous no-op that, by serial-queue
     FIFO ordering, cannot start until every previously-enqueued block has
     finished. When it returns, no block is in flight or pending, so dropping the
     block's `Arc` clone and teardown's `Arc` clone **frees** the context with no
     use-after-free window.

  **Synchronization primitive is the serial queue, not atomics:** block N
  completes-before block N+1 begins; the `exec_sync` enqueued after the removals
  completes-before returning only once all prior blocks complete. **No
  HAL-recursive-mutex deadlock** (the reason app-side locking was rejected here):
  the barrier is a `dispatch_sync` on **our own** queue, never an app lock held
  across the HAL's `Remove`; and teardown never runs on the watch queue, so it
  cannot self-deadlock. This eliminates **both** the leak and the race.

  **Remaining follow-up:** the device-*alive* listener (`DeviceAliveContext`,
  used to detect spontaneous device death) still uses the PROC API + the
  `tearing_down` Release/Acquire guard; migrating it to the same block-queue
  pattern is a separate, out-of-scope task.

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
  macOS teardown removes each block, runs a serial-queue sync barrier that
  completes as soon as any in-flight block returns (bounded by one block's short
  push-onto-channel body), then frees the context — dropping the sender ends the
  helper's `recv()`. None block unboundedly.

## 6. References

- 2026-05-30 architecture critique: *"macOS device-watch teardown has a
  use-after-free window on the listener context"* (HIGH, concurrency-threading —
  the §5 race, now **RESOLVED via block listeners on a serial dispatch queue**,
  rsac-e8aa / GH #32), and *"GAP: DeviceWatcher RAII teardown / lifecycle
  contract"* (folded into the device-watch ADR family).
- **Shipped fix (rsac-e8aa / GH #32):** migrate the macOS watcher to
  `AudioObjectAddPropertyListenerBlock` on a self-owned serial dispatch queue
  with remove-on-same-queue + sync barrier (the Chromium/Itsuki pattern),
  eliminating both the leak and the race. Adds direct `dispatch2`/`block2` macOS
  dependencies.
- Prior art: Hush #826 (*"deliberately leak … callback data that may still be
  referenced by the HAL"* — the stopgap this fix replaces); TypeWhisper #209
  (app-side locking cannot bridge the destructor/remove gap and can deadlock on
  the HAL's recursive mutex — why the barrier is a `dispatch_sync` on our own
  queue, not an app lock); cpal's own admission that its trust-the-OS approach
  *"could lead to a use-after-free."*
- `src/core/interface.rs` — `DeviceWatcher` (take-once `Option<Box<dyn FnOnce()
  + Send>>`, `Drop`, `from_teardown`).
- `src/audio/windows/wasapi.rs` — `DeviceEnumerator::watch` teardown,
  `SendWatcherTeardown` + its `Arc<ComInitializer>` apartment pin.
- `src/audio/macos/coreaudio.rs` — `DeviceEnumerator::watch` teardown
  (remove → barrier → free → join via `WatchTeardown::run`), the block-based
  `WatchListenerContext` (`Arc`, `event_tx: Mutex<SyncSender>`), the locally
  hand-declared `AudioObject{Add,Remove}PropertyListenerBlock` prototypes, and
  the `WatchBlock` / `WatchTeardown` types. The device-*alive* listener
  (`DeviceAliveContext`) remains on the PROC API — a follow-up.
- `src/audio/linux/thread.rs` — `spawn_device_watcher` teardown (flag + join;
  RAII listener drop on the loop thread).
- Companion: ADR-0004 (device-change-notification delivery model — the threading
  model these teardowns reclaim; the helper-thread delivery model is unchanged by
  this fix). No-panics rule: `AGENTS.md` / `CLAUDE.md`.

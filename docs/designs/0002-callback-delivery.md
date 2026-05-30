# ADR 0002 — `set_callback` delivery: wire it, don't remove it

**Status:** Accepted
**Date:** 2026-05-29
**Scope:** `src/api.rs` (`AudioCapture::set_callback`/`start`), `bindings/rsac-ffi`
**Verdict:** **Wire the stored callback into a pump thread spawned by `start()`**,
mirroring `subscribe()`. Do not remove the API — the C FFI and docs already advertise it.

## 1. Context

`AudioCapture::set_callback(F: FnMut(&AudioBuffer))` stores a boxed closure in
`Arc<Mutex<Option<...>>>`. The audit (2026-05-29, finding H1) confirmed the closure is
**never invoked**: it is constructed, set, cleared, and read only by the `Debug` impl.
`start()` spawns no pump, so `capture.set_callback(cb); capture.start();` delivers zero
buffers with no error.

This is a documented, public consumption mode. It is also exposed through the C FFI
(`rsac_capture_set_callback`, with a real 5-arg `extern "C"` trampoline) and referenced
in `docs/architecture/API_DESIGN.md`. The Node/napi binding works only because it runs
its *own* pump thread and ignores the core callback path.

## 2. Decision drivers

- Correctness-first: a public method that silently does nothing is the worst outcome.
- Three consumption modes already exist: pull (`read_buffer`), channel (`subscribe`),
  and async (`audio_data_stream`). Push-callback is the documented fourth.
- The FFI/C ABI is built around callback delivery; removing it breaks the C surface and
  the (to-be-fixed) Go binding's intended design.

## 3. Considered options

### Option A — Remove `set_callback`/`clear_callback` + FFI callback path + docs
- ➕ Smallest code; eliminates dead surface.
- ➖ Breaks the C ABI's primary push model; contradicts API_DESIGN.md; forces FFI
   consumers onto polling. Throws away a legitimately-useful mode. Rejected.

### Option B — Wire the callback into a `start()`-spawned pump thread (CHOSEN)
On `start()`, if a callback is registered, spawn a named reader thread that loops
`try_read_chunk()` → on `Some(buf)` lock the callback mutex and invoke it; on `Ok(None)`
sleep ~1ms; on `Err` (terminal) exit. Reuse the exact lifecycle pattern already proven
in `subscribe()`.
- ➕ Makes the documented behavior real; consistent with `subscribe()`; the FFI
   trampoline finally fires.
- ➖ Callback runs on a library-owned thread (not the RT thread) — must be documented:
   the user closure must not block indefinitely. (It is off the RT path, so blocking
   only stalls delivery, never the audio callback — acceptable and safer than RT-thread
   invocation.)
- ➖ Competes with `read_buffer()`/`subscribe()` for the same ring (same caveat already
   documented for `subscribe()`).

### Option C — Invoke the callback directly on the OS RT callback thread
- ➖ A user closure on the RT thread can allocate/lock/block → the exact RT-safety
   violation the whole bridge exists to prevent. Rejected outright.

## 4. Decision

**Option B.** Spawn the pump in `start()` when a callback is present. Document that the
callback fires on a dedicated non-RT thread, must return promptly, and that callback +
`subscribe`/`read_buffer` compete for the same buffers. Wrap the FFI trampoline
invocation in `catch_unwind` (audit U3) so a panicking C callback cannot unwind across
the FFI boundary.

## 5. Consequences

- `start()` gains a "spawn callback pump if set" step; the pump exits on stream stop or
  callback-cleared.
- `set_callback` while running stays rejected (existing guard) — callback is bound at
  `start()`.
- Test via a mock `CapturingStream` asserting the closure observes pushed buffers.
- FFI `invoke()` gets `catch_unwind` (closes U3 before it becomes reachable).

## 6. References

- Audit findings H1, U3 (2026-05-29 deep-dive).
- `src/api.rs:577-610` (set/clear), `:398-429` (start), `:631-675` (subscribe pattern).
- `bindings/rsac-ffi/src/lib.rs:184-227` (callback trampoline).

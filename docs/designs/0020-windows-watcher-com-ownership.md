---
status: accepted
date: 2026-11-26
---

# ADR-0020: Windows device-watcher COM objects live on its helper thread; control-plane handles store no COM pointer

## Context and Problem Statement

`WindowsDeviceEnumerator`, `WindowsAudioDevice` and the device watcher's teardown state held COM interface pointers (`IMMDeviceEnumerator`, `IMMDevice`, `IMMNotificationClient`) behind `unsafe impl Send` and `Sync`. The public `DeviceEnumerator` and `AudioDevice` traits require `Send + Sync`, so a handle created on one thread could be used, unregistered or released on another, including a thread in a single-threaded apartment (STA). No primary source documents these interfaces as agile, and `UnregisterEndpointNotificationCallback` needs the pointer that was registered (#105). Refusing STA callers at construction did not cover later use, and it shut out GUI main threads.

## Decision Drivers

- No COM interface pointer in a value that can cross a thread.
- Keep the public signatures and the cross-platform traits unchanged.
- Work for callers in an STA, in the MTA, and with COM never initialised, without changing the caller's apartment.
- Keep ADR-0004's delivery model (bounded channel, drop-newest, one helper thread that owns the handler) and ADR-0005's RAII, run-once, no-panic teardown.

## Considered Options

- A stateless control plane, and one owner thread per watcher.
- One process-wide COM owner thread that serves every control-plane call.
- Agile references (`RoGetAgileReference`) to the existing pointers.

## Decision Outcome

Choose **a stateless control plane, and one owner thread per watcher**. The `MMDeviceEnumerator` class is registered `ThreadingModel=Both` (measured on a Windows 11 workstation), so COM creates it directly in the caller's apartment, STA or MTA, without a proxy.

### What this supersedes

- ADR-0004, Windows only: its account of the notification client's COM ownership and of what the client does on the MMDevice thread (the §1 and §4 Windows bullets, and the producer-side log in the §3 "Drop-newest" sub-decision). The helper thread, not the `watch()` caller, now creates and registers the client; the client no longer resolves a `DeviceAdded` name and kind or logs a dropped event on the MMDevice thread, the helper does both. The channel, its bound and the drop-newest policy are unchanged.
- ADR-0005, Windows only: the Windows teardown bullet of §4; the Windows sentence of its verdict and the Windows parts of the §2 drivers "Send teardown across `!Send` backends" and "COM apartment lifetime"; and in §5 the Windows handler-lifetime claim (first bullet), the "No self-join hazard" bullet and the Windows teardown latency. macOS and Linux are unchanged.

### Control plane

- `WindowsDeviceEnumerator` holds only an MTA usage pin: the cookie from `CoIncrementMTAUsage`, released with `CoDecrementMTAUsage`, which Microsoft documents as valid on a thread other than the one that incremented. `WindowsAudioDevice` holds plain data read at enumeration (endpoint id, friendly name, data-flow kind or the error reading it, default flag) and the shared pin. Both are `Send + Sync` through the auto traits.
- Each call creates, uses and releases its COM objects within the call, on the calling thread. `supported_formats()` looks the endpoint up by id; an endpoint that no longer exists yields the default-format fallback, never another endpoint's formats.
- No call changes the caller's apartment. `ComInitializer::new()` keeps its strict check (an STA caller is refused with `RPC_E_CHANGED_MODE`) for the capture worker thread and the deprecated `WindowsApplicationCapture`.
- The only `unsafe impl` items under `src/audio/windows/` are `Send` and `Sync` for the pin, and `Send` for the deprecated `WindowsApplicationCapture`, which goes at the 0.5.0 cutover. The `unsafe_impl_allowlist` unit test enforces this.

### Watcher ownership

- `watch()` makes no COM call. It spawns `rsac-wasapi-device-watch`, which initialises COM as MTA itself, creates its own `IMMDeviceEnumerator` and the `IMMNotificationClient`, registers the client, and reports the outcome over a zero-capacity channel.
- `watch()` waits at most `WATCH_REGISTRATION_TIMEOUT` (five seconds). A reported failure is returned at once. With no report in time, `watch()` returns `AudioError::Timeout`; because the channel holds nothing, a later report fails on the helper, which then unregisters if it registered, releases everything and exits without ever calling the handler. On every failure the helper owns its cleanup and `watch()` does not join it: the join would also wait for the handler's destructor, which may wait on the caller.
- The notification client holds no COM pointer, only the bounded sender and an overflow counter. Each callback copies the event data and `try_send`s it, nothing else: no user code, no logger, no COM call. On a full channel it drops the notice and increments the counter; the helper logs the count when it next wakes.
- The helper resolves a `DeviceAdded` name and kind with its own enumerator before it calls the handler.

### Teardown

The teardown closure captures only a shutdown flag and the helper's `JoinHandle`. Dropping the `DeviceWatcher`, on any thread:

1. Sets the flag.
2. Returns, if the dropping thread is the helper (the watcher was dropped from inside its own handler).
3. Otherwise joins the helper. A panic payload from the helper is dropped under `catch_unwind`; if that drop panics, the second payload is leaked, so nothing unwinds out of `DeviceWatcher::drop`.

The helper reads the flag after every wake, before any resolution or handler call; again after a resolution; and after every handler call. Once it sees the flag it leaves the delivery loop and, on its own thread, unregisters the client pointer it registered, releases the client (and with it the sender) and its enumerator, drops the receiver, calls `CoUninitialize` and exits. A panicking handler unwinds the helper through the same drops.

### Lifetime guarantee

- After `drop` returns on a thread other than the helper, no handler call is running and none starts.
- When the watcher is dropped from inside its own handler, `drop` returns while that call is still running; no new call starts after it, and the helper tears down once the handler returns.
- Teardown latency: up to one `NOTIFY_THREAD_POLL_INTERVAL` (100 ms) after the flag is set, plus any handler call or `DeviceAdded` resolution already running, plus the unregister and release.

### Restriction

`drop` joins the helper, so it deadlocks when the helper is waiting on the dropping thread: a handler that synchronously waits on that thread, such as `SendMessage` to a window owned by the STA thread that is dropping the watcher, or two watchers whose handlers each drop the other. A handler posts instead of sending: `PostMessage`, or a channel the UI thread drains, without waiting for the result. Dropping the watcher from inside its own handler is the one supported re-entrant case.

### Consequences

- Threads in an STA, such as GUI main threads, can enumerate devices, query them, list audio sessions and watch.
- Each control-plane call creates its own in-process `IMMDeviceEnumerator`.
- `is_default()` reports whether the endpoint was the default render endpoint at enumeration; `kind()` returns the kind read at enumeration.
- `enumerate_devices()` and `default_device()` can fail with `create_device_enumerator`; `watch()` can return `AudioError::Timeout`.
- ADR-0005's run-once, best-effort, no-panic teardown still holds on Windows.

## Pros and Cons of the Options

### Stateless control plane and one owner thread per watcher

- Good, because no COM pointer crosses a thread and the handles need no `unsafe impl`.
- Good, because callers keep their apartment, and STA callers work.
- Bad, because every call recreates the enumerator, and device data is a snapshot taken at enumeration.

### One process-wide COM owner thread

- Good, because one set of COM objects serves every call.
- Bad, because every call becomes a cross-thread request with its own timeout and failure modes, and the owner thread's lifetime must be managed for the whole process.

### Agile references

- Good, because handles could keep their pointers.
- Bad, because every use resolves a reference, and the watcher still needs one thread that unregisters the original pointer.

## More Information

Issue #105. Implementation: `src/audio/windows/wasapi.rs` (`MtaUsagePin`, `WindowsDeviceEnumerator`, `WindowsAudioDevice`, `watch`, `device_watch_thread_main`, `deliver_notices`); apartment tests written independently from the contract in `src/audio/windows/apartment_tests.rs`. Companion ADRs: [ADR-0004](0004-device-change-notifications.md) and [ADR-0005](0005-device-watcher-raii-teardown.md).

Microsoft documentation: [CoIncrementMTAUsage](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coincrementmtausage), [CoDecrementMTAUsage](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-codecrementmtausage), [UnregisterEndpointNotificationCallback](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-unregisterendpointnotificationcallback).

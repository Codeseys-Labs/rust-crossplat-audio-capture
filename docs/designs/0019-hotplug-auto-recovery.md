---
status: accepted
date: 2026-11-25
---

# ADR-0019: Recover desktop default-device capture across hotplug

## Context and Problem Statement

A default output device can change or disappear while a capture is running. Device-change notifications already exist, but a watcher alone does not replace an invalidated stream. Applications need an opt-in session that survives supported changes without making the real-time callback allocate or lock ([ADR-0001](0001-rt-allocation-guarantee.md)).

A bridge producer and consumer own complementary ring and free-list halves. Reusing a producer from a stopped backend would require a new teardown and ownership protocol. Device identifiers also cannot generally identify a replugged device: Linux uses PipeWire object serials, which change with object creation; macOS object IDs may also change.

## Decision Drivers

- Preserve a stable consumer facade, bounded observability, and explicit terminal errors.
- Follow only targets whose identity can be honored without silently capturing another source.
- Keep all recovery coordination, allocation, and backend opens off real-time callbacks.
- Preserve timing gaps and prevent unannounced delivery-format changes.
- Make stop effective during backoff and in-flight opens.

## Considered Options

- A supervised facade with a fresh bridge per generation.
- Reattach a reopened backend to the existing bridge producer.
- Leave recovery to each application using device watchers and stream errors.

## Decision Outcome

Choose **a supervised facade with a fresh bridge per generation**, behind the dependency-free, default-off `hotplug-recovery` feature. This is a design and release target, not a claim that every platform scenario has been verified. The scope is desktop `SystemDefault` only; reject device, application and process-tree targets with `ConfigurationError`, and mobile with `PlatformNotSupported`. `DeviceWatcher` remains independently available without the feature.

### Policy and observability

- `AudioCaptureBuilder::with_auto_recovery(AutoRecoveryConfig)` enables recovery. Public `RecoveryState`, `RecoverySnapshot` and `RecoveryEvent` types describe state and generation; `AudioBuffer::stream_generation()` identifies each generation. The snapshot includes attempt, successful-recovery and dropped-event counts.
- Defaults: five failed attempts; exponential backoff beginning at 100 ms, capped at 2 s; 60 s healthy time to reset the failure budget; 200 ms event settle delay; and 30 s maximum wait for an absent device. A failed open or a reopened stream dying before the stable period spends the budget; a successful proactive default switch does not. Missing devices wait for watcher events without consuming attempts, up to the device wait timeout. No jitter or end-to-end wall-clock deadline is promised.
- `subscribe_recovery_events()` allows one subscriber on a bounded 32-event channel. Drop newest when full and count drops in the snapshot. The watcher callback only marks the supervisor dirty; the supervisor debounces changes and handles opens outside the callback.
- Windows and macOS proactively reopen on a default change. Linux relies on native default routing and reopens after an audited invalidation. Dropped notifications must not prevent recovery from a terminal backend error.

### Lifecycle and data contract

Break before make: stop the old backend, then create a new backend and bridge. Drain any readable tail before reporting an empty read during recovery. Terminal-aware polling returns `Ok(None)` while awaiting a replacement; blocking and async readers remain live and wake on publication, exhaustion, or stop. `is_running()` and `is_stream_producing()` remain true during recovery. Existing legacy read contracts are not strengthened implicitly.

The consumer maintains cumulative capture counters and a monotonic stream-position timeline. The next generation starts at the last offered position plus the measured monotonic outage; no PCM silence is inserted. This exposes a gap that composition can pad. A changed negotiated delivery format is fatal `StreamEnded` before any new-format samples are delivered. Exhaustion and non-retryable reopen failures also end with the existing fatal `StreamEnded` rather than a new public error variant.

Stop is sticky: wake readers promptly, preserve readable-tail behavior, never publish a late open, and dispose of any late result off the read path. Do not hold a control lock across backend open or stop, a blocking read, or user code.

### Consequences

- **Positive:** applications retain a stable session and can observe generation changes and missed events.
- **Positive:** fresh bridge ownership avoids transferring a callback-owned producer across backends.
- **Negative:** reopens can create audible gaps; a changed format terminates capture rather than adapting consumers.
- **Negative:** bounded events can be dropped, and explicit-device and application recovery are unavailable until stable identities and routing are established.
- **Negative:** flapping devices, delayed opens and reader wakeups introduce concurrency and platform-test obligations.

## Pros and Cons of the Options

### Supervised facade with fresh bridges

- Good, because ownership and callback responsibilities remain separated.
- Good, because the facade can maintain timing and counters across replacement.
- Bad, because generation swaps, reader wakeups and stop races need careful coordination.

### Reattach to the existing bridge

- Good, because consumers could retain the same ring.
- Bad, because the callback owns the producer and free-list halves; reattachment needs a new teardown protocol.

### Application-managed recovery

- Good, because the core has no recovery supervisor.
- Bad, because every application must reproduce the lifecycle, timing and race handling.

## More Information

Prerequisites include a bounded Windows initialization handshake and preventing Linux explicitly targeted streams from silently reconnecting to an unrelated default. Validate the Linux routing assumptions with source-identifying virtual-audio tests; the WirePlumber behavior is not established by documentation alone. Required recovery evidence includes stop versus open/backoff races, async waker handoff, dropped watcher events, format rejection, real-time allocation checks, Windows/macOS default switches, and a two-null-sink Linux default switch. A skipped audio test is not evidence of recovery.

See [desktop refinement](../DESKTOP_REFINEMENT.md) for sequencing and release gates, and [ADR-0007](0007-capacity-period-sizing.md) for ring-slot sizing.

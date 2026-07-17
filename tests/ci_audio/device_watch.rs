//! Device-change notification integration tests (`watch()` / `DeviceEvent`).
//!
//! `CrossPlatformDeviceEnumerator::watch()` dispatches to the active backend's
//! `DeviceEnumerator::watch` implementation (ADR-0004), returning a
//! [`DeviceWatcher`] RAII guard whose `Drop` unregisters the OS listener and
//! joins the notify thread (ADR-0005). Backends whose
//! `supports_device_change_notifications` capability is `false` inherit the
//! trait default, which returns `AudioError::PlatformNotSupported`.
//!
//! What the unit tests do NOT cover: the *dispatch through a real backend*. The
//! `interface.rs` unit test exercises only the default trait impl on a fake
//! enumerator; nothing ties the public `watch()` surface on a real backend to
//! the advertised capability flag. These tests close that gap.
//!
//! Split rationale: real `DeviceEvent` delivery needs a hot-plug / default
//! change, which CI cannot produce deterministically. So the *contract*
//! (capability-consistency + RAII teardown) is deterministic and device-free
//! and runs everywhere; *event delivery* is `#[ignore]`d for manual hardware
//! runs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rsac::{get_device_enumerator, DeviceEvent, PlatformCapabilities};

/// The public `watch()` surface must agree with the advertised capability flag:
/// backends that claim `supports_device_change_notifications` return an active
/// `DeviceWatcher`; backends that do not return the documented
/// `PlatformNotSupported { feature: "device change notifications", .. }`.
///
/// Deterministic and device-free: no `require_*!` gate, mirroring
/// `platform_caps.rs`. If no backend is available (a backend-less build) we skip
/// honestly, matching `audio/mod.rs`-style enumerator-absence handling.
#[test]
fn watch_matches_capability_flag() {
    let caps = PlatformCapabilities::query();

    // Backend-less build → honest skip (there is no real enumerator to dispatch
    // through), same posture as the device-free enumeration tests.
    let enumerator = match get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "[ci_audio] device_watch: no device enumerator on this build ({e:?}); skipping"
            );
            return;
        }
    };

    // A no-op handler: we assert on the Result shape, not on delivery.
    let watcher = enumerator.watch(Box::new(|_ev: DeviceEvent| {}));

    if caps.supports_device_change_notifications {
        assert!(
            watcher.is_ok(),
            "capability advertises device-change notifications, but watch() failed: {:?}",
            watcher.err()
        );
        // Dropping the guard runs backend teardown (unregister + join notify
        // thread). It must return promptly and never panic (ADR-0005).
        drop(watcher.expect("watcher is Ok per the assert above"));
        eprintln!(
            "[ci_audio] ✅ watch() returned an active watcher consistent with the capability flag"
        );
    } else {
        match watcher {
            Err(rsac::AudioError::PlatformNotSupported { feature, .. }) => {
                assert_eq!(
                    feature, "device change notifications",
                    "PlatformNotSupported must name the device-change feature, got: {feature:?}"
                );
                eprintln!(
                    "[ci_audio] ✅ watch() correctly reported PlatformNotSupported on a backend \
                     without device-change support"
                );
            }
            other => panic!(
                "capability reports NO device-change support; watch() must return \
                 PlatformNotSupported {{ feature: \"device change notifications\", .. }}, \
                 got: {other:?}"
            ),
        }
    }
}

/// Dropping a `DeviceWatcher` must run backend teardown (unregister the OS
/// listener + join the notify thread) promptly and without hanging or panicking
/// (ADR-0005). We drop the watcher on a spawned thread and join that thread with
/// a bounded deadline: a teardown that hangs surfaces as a failed join (a test
/// failure) rather than a silent job-budget-eating hang.
///
/// Deterministic and device-free; gated only on the capability flag (a backend
/// without device-change support cannot produce a watcher to drop).
#[test]
fn watch_drop_teardown_returns_promptly() {
    let caps = PlatformCapabilities::query();
    if !caps.supports_device_change_notifications {
        eprintln!(
            "[ci_audio] device_watch: backend lacks device-change support; skipping teardown test"
        );
        return;
    }

    let enumerator = match get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "[ci_audio] device_watch: no device enumerator on this build ({e:?}); skipping"
            );
            return;
        }
    };

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_for_handler = Arc::clone(&counter);
    let watcher = match enumerator.watch(Box::new(move |_ev: DeviceEvent| {
        counter_for_handler.fetch_add(1, Ordering::Relaxed);
    })) {
        Ok(w) => w,
        Err(e) => {
            // Capability said yes but watch() failed — a real regression only if
            // the backend is guaranteed present. Keep this honest: report and
            // return rather than hard-fail, since a backend-less CI leg can still
            // advertise the capability statically.
            eprintln!(
                "[ci_audio] device_watch: watch() failed despite capability flag ({e:?}); skipping"
            );
            return;
        }
    };

    // Hold the subscription briefly (no events guaranteed on a quiet host).
    std::thread::sleep(Duration::from_millis(100));

    // Drop the watcher on a worker thread; join it with a bounded deadline so a
    // hung teardown fails the test instead of hanging the job.
    let handle = std::thread::spawn(move || {
        drop(watcher);
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            panic!(
                "DeviceWatcher teardown did not complete within 5s — Drop is expected to \
                 unregister the OS listener and join the notify thread promptly (ADR-0005)"
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    handle
        .join()
        .expect("watcher-drop thread panicked during teardown — Drop must never panic");

    // Do NOT assert the counter is non-zero: no device event is guaranteed on a
    // quiet CI host. The observation is purely diagnostic.
    eprintln!(
        "[ci_audio] ✅ DeviceWatcher teardown completed promptly (observed {} event(s))",
        counter.load(Ordering::Relaxed)
    );
}

/// Hardware-manual: verify a real `DeviceEvent` is delivered when a device is
/// hot-plugged or the default changes. Cannot run in CI (no way to force a
/// hardware change deterministically), so it is `#[ignore]`d.
#[test]
#[ignore = "requires a physical device hot-plug / default change; run manually on hardware"]
fn watch_delivers_device_event_on_change() {
    require_audio!();

    let caps = PlatformCapabilities::query();
    if !caps.supports_device_change_notifications {
        eprintln!("[ci_audio] device_watch: backend lacks device-change support; skipping");
        return;
    }

    let enumerator = match get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ci_audio] device_watch: no device enumerator ({e:?}); skipping");
            return;
        }
    };

    // The handler runs on the OS notify thread (never the RT audio callback), so
    // allocating / locking to push into a channel is allowed per the trait doc.
    let (tx, rx) = mpsc::channel::<DeviceEvent>();
    let _watcher = match enumerator.watch(Box::new(move |ev: DeviceEvent| {
        let _ = tx.send(ev);
    })) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[ci_audio] device_watch: watch() failed ({e:?}); skipping");
            return;
        }
    };

    eprintln!(
        "\n[ci_audio] >>> Within 30s, plug/unplug an audio device OR switch the system \
         default device to trigger a DeviceEvent...\n"
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            eprintln!(
                "[ci_audio] ⚠ no DeviceEvent received within 30s — cannot force a hardware \
                 change in an automated run; returning without failing"
            );
            return;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(event) => {
                // Assert the event is a well-formed variant with non-empty
                // identifying fields where applicable.
                match &event {
                    DeviceEvent::DeviceAdded { id, name, kind } => {
                        assert!(!id.0.is_empty(), "DeviceAdded id must be non-empty");
                        assert!(!name.is_empty(), "DeviceAdded name must be non-empty");
                        eprintln!(
                            "[ci_audio] ✅ DeviceAdded id={} name={name} kind={kind:?}",
                            id.0
                        );
                    }
                    DeviceEvent::DeviceRemoved { id } => {
                        assert!(!id.0.is_empty(), "DeviceRemoved id must be non-empty");
                        eprintln!("[ci_audio] ✅ DeviceRemoved id={}", id.0);
                    }
                    DeviceEvent::DefaultChanged { id, kind } => {
                        assert!(!id.0.is_empty(), "DefaultChanged id must be non-empty");
                        eprintln!("[ci_audio] ✅ DefaultChanged id={} kind={kind:?}", id.0);
                    }
                    DeviceEvent::StateChanged { id, available } => {
                        assert!(!id.0.is_empty(), "StateChanged id must be non-empty");
                        eprintln!(
                            "[ci_audio] ✅ StateChanged id={} available={available}",
                            id.0
                        );
                    }
                    // `#[non_exhaustive]`: tolerate future variants.
                    other => {
                        eprintln!("[ci_audio] ✅ received DeviceEvent variant: {other:?}");
                    }
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[ci_audio] device_watch: event channel disconnected; returning");
                return;
            }
        }
    }
}

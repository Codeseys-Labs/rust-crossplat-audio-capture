//! Unit tests for the [`crate::api`] facade.

use super::builder::SUPPORTED_SAMPLE_RATES;
use super::capture::{format_description_string, AudioCallback};
use super::delivery::SUBSCRIBE_CHANNEL_CAPACITY;
use super::{AudioCapture, AudioCaptureBuilder, AudioCaptureConfig, RunningCapture};

use crate::core::buffer::AudioBuffer;
use crate::core::capabilities::PlatformCapabilities;
use crate::core::config::{AudioFormat, CaptureTarget, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult, LifecycleStage, REASON_NO_ACTIVE_STREAM};
use crate::core::interface::CapturingStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn builder_defaults_to_system_default() {
    let builder = AudioCaptureBuilder::new();
    assert_eq!(builder.target, CaptureTarget::SystemDefault);
    assert_eq!(builder.config.sample_rate, 48000);
    assert_eq!(builder.config.channels, 2);
    assert_eq!(builder.config.sample_format, SampleFormat::F32);
    assert_eq!(builder.config.buffer_size, None);
}

#[test]
fn builder_fails_if_channels_is_zero() {
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(44100)
        .channels(0)
        .sample_format(SampleFormat::F32)
        .build();
    assert!(result.is_err());
    match result.err().unwrap() {
        AudioError::ConfigurationError { message: msg } => {
            assert_eq!(msg, "Channels must be greater than 0.");
        }
        other_error => panic!("Expected ConfigurationError, got {:?}", other_error),
    }
}

#[test]
fn builder_fails_on_unsupported_sample_rate() {
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(11025) // Not supported
        .channels(1)
        .sample_format(SampleFormat::F32)
        .build();
    assert!(result.is_err());
    match result.err().unwrap() {
        AudioError::InvalidParameter { param, reason } => {
            assert_eq!(param, "sample_rate");
            assert!(reason.contains("11025"));
        }
        other_error => panic!("Expected InvalidParameter, got {:?}", other_error),
    }
}

#[test]
fn builder_with_target_overrides_default() {
    let device_id = crate::core::config::DeviceId("test-device".to_string());
    let builder = AudioCaptureBuilder::new().with_target(CaptureTarget::Device(device_id.clone()));
    assert_eq!(builder.target, CaptureTarget::Device(device_id));
}

#[test]
fn builder_with_config_sets_all_fields() {
    let config = StreamConfig {
        sample_rate: 44100,
        channels: 1,
        sample_format: SampleFormat::I16,
        buffer_size: Some(1024),
        capture_target: CaptureTarget::SystemDefault,
        #[cfg(target_os = "ios")]
        ios_app_group: None,
        #[cfg(target_os = "android")]
        android_projection: None,
    };
    let builder = AudioCaptureBuilder::new().with_config(config.clone());
    assert_eq!(builder.config, config);
}

// ── Builder method chainability & defaults ────────────────────────

#[test]
fn builder_is_chainable() {
    // Verify all builder methods return Self and can be chained
    let builder = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(44100)
        .channels(2)
        .sample_format(SampleFormat::F32)
        .buffer_size(Some(1024))
        .buffer_size_frames(Some(512));
    // Just verifying compilation and chainability — no panic
    assert_eq!(builder.config.sample_rate, 44100);
    assert_eq!(builder.config.channels, 2);
}

#[test]
fn builder_default_trait_matches_new() {
    let from_new = AudioCaptureBuilder::new();
    let from_default = AudioCaptureBuilder::default();
    // Both should produce identical builders
    assert_eq!(from_new.config.sample_rate, from_default.config.sample_rate);
    assert_eq!(from_new.config.channels, from_default.config.channels);
    assert_eq!(
        from_new.config.sample_format,
        from_default.config.sample_format
    );
}

// ── Invalid sample rate tests ────────────────────────────────────

#[test]
fn builder_rejects_sample_rate_zero() {
    let result = AudioCaptureBuilder::new().sample_rate(0).build();
    assert!(result.is_err());
    match result.unwrap_err() {
        AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
        e => panic!("Expected InvalidParameter, got: {e:?}"),
    }
}

#[test]
fn builder_rejects_very_high_sample_rate() {
    let result = AudioCaptureBuilder::new().sample_rate(999999).build();
    assert!(result.is_err());
    match result.unwrap_err() {
        AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
        e => panic!("Expected InvalidParameter, got: {e:?}"),
    }
}

#[test]
fn builder_rejects_nonstandard_sample_rate() {
    // 11025 is a valid audio rate but not in the supported list
    let result = AudioCaptureBuilder::new().sample_rate(11025).build();
    assert!(result.is_err());
}

/// rsac-c957: the builder's config-time whitelist is the *same* array as the
/// public [`PlatformCapabilities::SUPPORTED_SAMPLE_RATES`] — a single source
/// of truth, so a caller pre-validating against the public const sees exactly
/// what `build()`/`preflight()` enforce.
#[test]
fn builder_whitelist_is_platform_capabilities_const() {
    assert_eq!(
        SUPPORTED_SAMPLE_RATES,
        PlatformCapabilities::SUPPORTED_SAMPLE_RATES
    );
    // And the slice accessor agrees with what preflight checks against.
    assert_eq!(
        PlatformCapabilities::supported_sample_rates(),
        &SUPPORTED_SAMPLE_RATES
    );
}

#[test]
fn builder_accepts_all_supported_sample_rates() {
    // These should NOT fail at the sample_rate validation step
    // They may fail later at device enumeration, which is fine
    for rate in [22050u32, 32000, 44100, 48000, 88200, 96000] {
        let result = AudioCaptureBuilder::new().sample_rate(rate).build();
        // Should NOT be InvalidParameter for sample_rate
        if let Err(AudioError::InvalidParameter { param, .. }) = &result {
            panic!("Rate {rate} should be valid, but got InvalidParameter {{ param: {param} }}");
        }
        // Other errors (DeviceEnumeration, etc.) are expected without hardware
    }
}

// ── Invalid channel count tests ──────────────────────────────────

#[test]
fn builder_rejects_channels_above_max() {
    let result = AudioCaptureBuilder::new()
        .channels(33) // MAX_CHANNELS = 32
        .build();
    assert!(result.is_err());
    match result.unwrap_err() {
        AudioError::ConfigurationError { .. } => {} // expected
        e => panic!("Expected ConfigurationError, got: {e:?}"),
    }
}

#[test]
fn builder_rejects_channels_way_above_max() {
    let result = AudioCaptureBuilder::new().channels(u16::MAX).build();
    assert!(result.is_err());
}

#[test]
fn builder_accepts_max_channels() {
    // 32 channels should be accepted (it's the max, not above it)
    let result = AudioCaptureBuilder::new().channels(32).build();
    // Should NOT be ConfigurationError
    if let Err(AudioError::ConfigurationError { .. }) = &result {
        panic!("32 channels (MAX_CHANNELS) should be accepted");
    }
    // Other errors (DeviceEnumeration, etc.) are fine
}

#[test]
fn builder_accepts_mono() {
    let result = AudioCaptureBuilder::new().channels(1).build();
    // Should NOT be ConfigurationError for channels
    if let Err(AudioError::ConfigurationError { message }) = &result {
        if message.contains("hannels") {
            panic!("Mono (1 channel) should be accepted, got ConfigurationError: {message}");
        }
    }
}

// ── Sample format tests ──────────────────────────────────────────

#[test]
fn builder_with_all_sample_formats() {
    // Verify all sample formats can be set without panic
    for format in [
        SampleFormat::I16,
        SampleFormat::I24,
        SampleFormat::I32,
        SampleFormat::F32,
    ] {
        let builder = AudioCaptureBuilder::new().sample_format(format);
        assert_eq!(builder.config.sample_format, format);
    }
}

// ── Buffer size tests ────────────────────────────────────────────

#[test]
fn builder_buffer_size_can_be_set_and_cleared() {
    let b1 = AudioCaptureBuilder::new().buffer_size(Some(1024));
    assert_eq!(b1.config.buffer_size, Some(1024));

    let b2 = AudioCaptureBuilder::new().buffer_size(None);
    assert_eq!(b2.config.buffer_size, None);
}

#[test]
fn builder_buffer_size_frames_sets_buffer_size() {
    let builder = AudioCaptureBuilder::new().buffer_size_frames(Some(256));
    assert_eq!(builder.config.buffer_size, Some(256));
}

// ── With_config override test ────────────────────────────────────

#[test]
fn builder_with_config_overrides_individual_settings() {
    let config = StreamConfig {
        sample_rate: 96000,
        channels: 8,
        sample_format: SampleFormat::I32,
        buffer_size: Some(2048),
        capture_target: CaptureTarget::SystemDefault,
        #[cfg(target_os = "ios")]
        ios_app_group: None,
        #[cfg(target_os = "android")]
        android_projection: None,
    };
    let builder = AudioCaptureBuilder::new()
        .sample_rate(44100) // This should be overridden
        .with_config(config.clone());
    assert_eq!(builder.config.sample_rate, 96000);
    assert_eq!(builder.config.channels, 8);
    assert_eq!(builder.config.sample_format, SampleFormat::I32);
}

// ── negotiated_format() tests (AEG-8, rsac-0113) ──────────────────

/// On Linux, negotiated_format() is intentionally unsupported pre-build
/// (PipeWire negotiates at stream-open). It must return PlatformNotSupported
/// rather than guessing a value — and must still run preflight first, so an
/// invalid config surfaces its config error before the platform gate.
#[cfg(target_os = "linux")]
#[test]
fn negotiated_format_unsupported_on_linux() {
    let builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
    match builder.negotiated_format() {
        Err(AudioError::PlatformNotSupported { platform, .. }) => {
            assert_eq!(platform, "linux");
        }
        other => panic!("expected PlatformNotSupported on Linux, got: {other:?}"),
    }
}

/// negotiated_format() runs preflight() first on every platform: an invalid
/// config (unsupported sample rate) is rejected with the same
/// InvalidParameter error build() would raise, before any device work.
#[test]
fn negotiated_format_runs_preflight_first() {
    let builder = AudioCaptureBuilder::new().sample_rate(11025); // unsupported
    match builder.negotiated_format() {
        Err(AudioError::InvalidParameter { param, .. }) => {
            assert_eq!(param, "sample_rate");
        }
        other => panic!("expected InvalidParameter from preflight, got: {other:?}"),
    }
}

/// On non-Linux platforms negotiated_format() resolves a real device. With
/// no hardware it surfaces an honest enumeration/format error; with hardware
/// it returns a format. Either way it must NOT panic — this asserts the
/// pre-build query is callable and well-behaved on a device-free CI box.
#[cfg(not(target_os = "linux"))]
#[test]
fn negotiated_format_is_callable_and_does_not_panic() {
    let builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
    // Valid config → preflight passes; device resolution may succeed (real
    // hardware) or fail (device-free CI). Both are acceptable; a panic is
    // not.
    let _ = builder.negotiated_format();
}

// ── Mock CapturingStream for subscribe/overrun_count tests ────────

/// A mock CapturingStream that serves buffers from an internal Mutex<VecDeque>
/// and tracks an overrun counter via an AtomicU64.
struct MockCapturingStream {
    buffers: Mutex<std::collections::VecDeque<AudioBuffer>>,
    running: AtomicBool,
    overruns: AtomicU64,
    /// Bridge counters mirrored for stream_stats()/backpressure_report():
    /// buffers the producer enqueued, buffers delivered to the consumer.
    pushed: AtomicU64,
    captured: AtomicU64,
    /// Legacy consecutive-drop backpressure flag the windowed report carries.
    backpressure: AtomicBool,
    /// The format `format()` reports. Defaults to `AudioFormat::default()`;
    /// `set_negotiated_format()` overwrites it to mirror a backend that
    /// negotiated a different delivery format on the bridge producer.
    format: Mutex<AudioFormat>,
    /// Count of `stop()` calls — lets RunningCapture tests assert that the
    /// guard's Drop stops exactly once and that an explicit stop + drop does
    /// not double-stop the *underlying* stream (stop() is a no-op once the
    /// AudioCapture has already dropped its Arc).
    stop_calls: AtomicU64,
    /// One-shot RECOVERABLE read errors to inject ahead of buffered data.
    /// Each `try_read_chunk`/`read_chunk` pops one off the front and returns
    /// it as an `Err`, modeling a transient hiccup (StreamReadError) that
    /// terminal-delivery consumers must RETRY rather than treat as the end.
    recoverable_errors: Mutex<std::collections::VecDeque<AudioError>>,
}

impl MockCapturingStream {
    fn new() -> Self {
        Self {
            buffers: Mutex::new(std::collections::VecDeque::new()),
            running: AtomicBool::new(true),
            overruns: AtomicU64::new(0),
            pushed: AtomicU64::new(0),
            captured: AtomicU64::new(0),
            backpressure: AtomicBool::new(false),
            format: Mutex::new(AudioFormat::default()),
            stop_calls: AtomicU64::new(0),
            recoverable_errors: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Queue one RECOVERABLE error (e.g. a transient StreamReadError) to be
    /// returned by the next read before any buffered data. Used by the
    /// terminal-delivery tests to prove a recoverable error is retried, not
    /// treated as end-of-stream.
    fn inject_recoverable_error(&self, e: AudioError) {
        debug_assert!(
            e.is_recoverable(),
            "inject_recoverable_error expects a recoverable variant"
        );
        self.recoverable_errors.lock().unwrap().push_back(e);
    }

    /// Number of times `stop()` was invoked on this stream.
    fn stop_calls(&self) -> u64 {
        self.stop_calls.load(Ordering::SeqCst)
    }

    /// Mirror the real bridge's `BridgeProducer::set_negotiated_format`:
    /// record the authoritative delivery format the backend produces.
    fn set_negotiated_format(&self, sample_rate: u32, channels: u16) {
        *self.format.lock().unwrap() = AudioFormat {
            sample_rate,
            channels,
            sample_format: SampleFormat::F32,
        };
    }

    /// Push a buffer for the mock to serve on the next try_read_chunk call.
    fn push_buffer(&self, buf: AudioBuffer) {
        self.buffers.lock().unwrap().push_back(buf);
    }

    /// Simulate overruns by incrementing the counter.
    fn add_overruns(&self, count: u64) {
        self.overruns.fetch_add(count, Ordering::Relaxed);
    }

    /// Mirror a producer that enqueued `count` buffers (bumps buffers_pushed).
    fn add_pushed(&self, count: u64) {
        self.pushed.fetch_add(count, Ordering::Relaxed);
    }

    /// Mirror a consumer that popped `count` buffers (bumps buffers_captured).
    fn add_captured(&self, count: u64) {
        self.captured.fetch_add(count, Ordering::Relaxed);
    }

    /// Mirror the producer dropping `count` buffers to overflow: bumps the
    /// drop/overrun counter (buffers_dropped is an alias of overrun_count).
    fn add_dropped(&self, count: u64) {
        self.overruns.fetch_add(count, Ordering::Relaxed);
    }

    /// Set the legacy consecutive-drop backpressure flag.
    fn set_backpressure(&self, on: bool) {
        self.backpressure.store(on, Ordering::SeqCst);
    }

    /// Signal the mock stream is stopped.
    fn signal_stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl CapturingStream for MockCapturingStream {
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        // Blocking: spin-wait until data or stopped. Mirrors the real bridge,
        // which returns the terminal StreamEnded (Fatal) once stopped.
        loop {
            if let Some(e) = self.recoverable_errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            if let Some(buf) = self.buffers.lock().unwrap().pop_front() {
                return Ok(buf);
            }
            if !self.running.load(Ordering::SeqCst) {
                return Err(AudioError::StreamEnded {
                    reason: "Mock stream stopped".into(),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        // Surface any injected RECOVERABLE error first (a transient hiccup
        // ahead of real data), so terminal-delivery consumers must retry it
        // rather than end iteration.
        if let Some(e) = self.recoverable_errors.lock().unwrap().pop_front() {
            return Err(e);
        }
        // Drain any buffered data first, even after stop, so the iterator's
        // drain-on-stop path (R2-2) is exercised; only report StreamEnded
        // once the buffer is empty AND the stream has stopped.
        if let Some(buf) = self.buffers.lock().unwrap().pop_front() {
            return Ok(Some(buf));
        }
        if !self.running.load(Ordering::SeqCst) {
            return Err(AudioError::StreamEnded {
                reason: "Mock stream stopped".into(),
            });
        }
        Ok(None)
    }

    fn stop(&self) -> AudioResult<()> {
        self.stop_calls.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn format(&self) -> AudioFormat {
        self.format.lock().unwrap().clone()
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn overrun_count(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    fn buffers_pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    fn buffers_captured(&self) -> u64 {
        self.captured.load(Ordering::Relaxed)
    }

    fn buffers_dropped(&self) -> u64 {
        // Alias of overrun_count(), matching the BridgeStream contract.
        self.overruns.load(Ordering::Relaxed)
    }

    fn is_under_backpressure(&self) -> bool {
        self.backpressure.load(Ordering::SeqCst)
    }

    fn drop_window_snapshot(&self) -> (u64, u64) {
        // The mock has no real sliding ring; its lifetime pushed/dropped
        // counters stand in as the windowed view so backpressure_report()
        // tests can drive a known (pushed, dropped) pair through the same
        // read path BridgeStream uses.
        (
            self.pushed.load(Ordering::Relaxed),
            self.overruns.load(Ordering::Relaxed),
        )
    }
}

/// Creates an AudioCapture with a mock stream, bypassing the builder (no hardware needed).
fn make_mock_capture(mock: Arc<MockCapturingStream>) -> AudioCapture {
    AudioCapture {
        config: AudioCaptureConfig {
            target: CaptureTarget::SystemDefault,
            stream_config: StreamConfig::default(),
        },
        device: None,
        stream: Some(mock),
        callback: Mutex::new(None),
        callback_pump: None,
        start_instant: None,
        subscriber_dropped: Arc::new(AtomicU64::new(0)),
        #[cfg(feature = "hotplug-recovery")]
        recovery: None,
    }
}

/// Creates an AudioCapture with NO stream — the pre-start / post-stop
/// state — for gate and default-snapshot tests.
fn make_capture_without_stream() -> AudioCapture {
    AudioCapture {
        config: AudioCaptureConfig {
            target: CaptureTarget::SystemDefault,
            stream_config: StreamConfig::default(),
        },
        device: None,
        stream: None,
        callback: Mutex::new(None),
        callback_pump: None,
        start_instant: None,
        subscriber_dropped: Arc::new(AtomicU64::new(0)),
        #[cfg(feature = "hotplug-recovery")]
        recovery: None,
    }
}

// ── Send + Sync / Arc-shareable tests (AEG-5, rsac-6f1f) ──────────

/// AEG-5: `AudioCapture` is `Send + Sync` at the type level (the compile-time
/// `_assert_send_sync` const proves it; this test makes the guarantee visible
/// in the test surface so a regression that flips it shows up as a failing
/// test, not just a build break).
#[test]
fn audio_capture_is_send_and_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<AudioCapture>();
    assert_sync::<AudioCapture>();
    // RunningCapture wraps it, so it inherits both.
    assert_send::<RunningCapture>();
    assert_sync::<RunningCapture>();
}

/// AEG-5: an `Arc<AudioCapture>` can be shared across threads and read
/// concurrently through `&self` with no external lock — the contract the
/// type-level `Send + Sync` claim makes good on. Two threads read from the
/// same shared handle via the `&self` read path; both succeed without a
/// `Mutex<AudioCapture>` wrapper.
#[test]
fn audio_capture_arc_shared_reads_need_no_external_lock() {
    let mock = Arc::new(MockCapturingStream::new());
    for _ in 0..8 {
        mock.push_buffer(AudioBuffer::new(vec![0.25; 4], 2, 48000));
    }
    let capture = Arc::new(make_mock_capture(Arc::clone(&mock)));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let capture = Arc::clone(&capture);
        handles.push(std::thread::spawn(move || {
            // &self read path through a shared Arc — no Mutex<AudioCapture>.
            let _ = capture.read_chunk_nonblocking();
            // Other &self queries are reachable on the shared handle too.
            let _ = capture.is_running();
            let _ = capture.stream_stats();
        }));
    }
    for h in handles {
        h.join().expect("reader thread joins");
    }
    mock.signal_stop();
}

// ── subscribe() tests ─────────────────────────────────────────────

/// rsac-7aa2(1): subscribe is gated on stream PRESENCE only. A stream that
/// already reached its fatal terminal (stopped, nothing buffered) is
/// accepted and yields a channel that ends immediately — racing a natural
/// end is indistinguishable from subscribing just before it, so an error
/// here would be arbitrary. A capture with NO stream at all still rejects,
/// with a message that is accurate for both never-started and post-stop.
#[test]
fn subscribe_on_terminal_stream_yields_immediately_ending_channel() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop(); // terminal, no buffered tail
    let capture = make_mock_capture(mock);

    let rx = capture.subscribe().expect("terminal stream is accepted");
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
        "channel must end immediately (no data, prompt disconnect)"
    );

    // No stream at all → rejected via the presence check.
    let empty = make_capture_without_stream();
    match empty.subscribe() {
        Err(e @ AudioError::StreamReadError { .. }) => {
            assert_eq!(
                e.lifecycle_stage(),
                Some(LifecycleStage::NotInitialized),
                "expected a NotInitialized lifecycle stage, got: {e:?}"
            );
        }
        other => panic!("expected StreamReadError, got: {other:?}"),
    }
}

/// rsac-7aa2(1): the drainable Stopping window is accepted — a stream that
/// gracefully ended with a buffered tail can still be subscribed to, and
/// the tail is delivered before the channel ends. (The old `is_running()`
/// gate rejected this call and stranded the tail.)
#[test]
fn subscribe_accepts_stopping_window_and_drains_tail() {
    let mock = Arc::new(MockCapturingStream::new());
    for i in 0..3 {
        mock.push_buffer(AudioBuffer::new(vec![i as f32 + 1.0; 4], 2, 48000));
    }
    // Not running, but the tail is still drainable — the mock models the
    // bridge's Stopping window (drain first, terminal only when empty).
    mock.signal_stop();
    let capture = make_mock_capture(Arc::clone(&mock));
    assert!(!capture.is_running());

    let rx = capture
        .subscribe()
        .expect("the drainable Stopping window must be accepted");
    let mut got = Vec::new();
    while let Ok(buf) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
        got.push(buf.data()[0]);
    }
    assert_eq!(
        got,
        vec![1.0, 2.0, 3.0],
        "the buffered tail is delivered, then the channel ends"
    );
}

/// rsac-7aa2(3): the post-stop subscribe error is accurate — stop()
/// releases the stream, so subscribe() must not claim the stream "is not
/// initialized" (the old message) when it was in fact stopped.
#[test]
fn post_stop_subscribe_error_is_accurate() {
    let mock = Arc::new(MockCapturingStream::new());
    let mut capture = make_mock_capture(mock);
    capture.stop().unwrap();
    match capture.subscribe() {
        Err(AudioError::StreamReadError { reason }) => {
            // Intentionally a prose assertion, not a lifecycle_stage() check
            // (rsac-feb4): this test's whole point is locking in *which
            // words* appear — the historical regression (rsac-7aa2(3)) was
            // the message misleadingly saying "not initialized" after a
            // stop(). Both never-started and post-stop map to the same
            // `LifecycleStage::NotInitialized`, so a stage-only assert would
            // silently lose this coverage.
            assert_eq!(
                reason, REASON_NO_ACTIVE_STREAM,
                "post-stop message must be the canonical no-active-stream reason: {reason}"
            );
        }
        other => panic!("expected StreamReadError, got: {other:?}"),
    }
}

#[test]
fn subscribe_receives_buffers() {
    let mock = Arc::new(MockCapturingStream::new());
    // Push some test buffers before subscribing
    mock.push_buffer(AudioBuffer::new(vec![0.1; 960], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.2; 960], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.3; 960], 2, 48000));

    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");

    // Receive the three buffers
    let buf1 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(buf1.data()[0], 0.1);

    let buf2 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(buf2.data()[0], 0.2);

    let buf3 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(buf3.data()[0], 0.3);

    // Stop the mock so the subscribe thread exits
    mock.signal_stop();
}

#[test]
fn subscribe_thread_stops_when_stream_stops() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");

    // Signal stop — the subscribe thread should exit
    mock.signal_stop();

    // After a short delay, recv should fail (channel disconnected)
    std::thread::sleep(std::time::Duration::from_millis(50));
    let result = rx.recv_timeout(std::time::Duration::from_millis(100));
    assert!(result.is_err());
}

#[test]
fn subscribe_thread_stops_when_receiver_dropped() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");

    // Drop the receiver — the subscribe thread should eventually exit
    drop(rx);

    // Push a buffer to trigger the send error in the thread
    mock.push_buffer(AudioBuffer::new(vec![1.0; 960], 2, 48000));

    // Give the thread time to realize the receiver is gone and exit
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Clean up
    mock.signal_stop();
}

// ── terminal-error delivery tests (FH-1 / BP-6) ───────────────────

/// subscribe() must NOT end on a recoverable read error: a transient
/// StreamReadError ahead of real data is retried, and the queued buffer is
/// still delivered. Previously ANY Err broke the loop, silently ending the
/// subscription on a transient hiccup.
#[test]
fn subscribe_continues_past_recoverable_error() {
    let mock = Arc::new(MockCapturingStream::new());
    // One transient error, then a real buffer behind it.
    mock.inject_recoverable_error(AudioError::StreamReadError {
        reason: "transient hiccup".into(),
    });
    mock.push_buffer(AudioBuffer::new(vec![0.42; 4], 2, 48000));

    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");

    // The recoverable error is swallowed+retried, so the buffer behind it
    // still arrives (subscribe() yields buffers only, not errors).
    let buf = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("buffer must arrive past the recoverable error");
    assert_eq!(buf.data()[0], 0.42);

    mock.signal_stop();
}

/// subscribe() ends (channel disconnects) only on a FATAL terminal. After
/// the mock stops and drains, the reader observes StreamEnded (fatal) and
/// exits, disconnecting the channel.
#[test]
fn subscribe_disconnects_on_fatal_terminal() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");

    // Stop → next try_read_chunk returns StreamEnded (fatal) → thread breaks.
    mock.signal_stop();

    // The channel disconnects; recv must eventually fail.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "channel must disconnect once the stream reaches a fatal terminal"
    );
}

/// subscribe_with_errors() delivers buffers as Ok(_) and forwards a
/// recoverable error as an Err item WITHOUT ending the stream (the buffer
/// behind it still arrives).
#[test]
fn subscribe_with_errors_forwards_recoverable_then_continues() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.inject_recoverable_error(AudioError::StreamReadError {
        reason: "transient hiccup".into(),
    });
    mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));

    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture
        .subscribe_with_errors()
        .expect("subscribe_with_errors should succeed");

    // First item: the recoverable error, surfaced but non-terminal.
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Err(e)) => {
            assert!(e.is_recoverable(), "first item is the recoverable error");
        }
        other => panic!("expected a recoverable Err item first, got: {other:?}"),
    }
    // Second item: the buffer behind it — delivery continued past the error.
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(buf)) => assert_eq!(buf.data()[0], 0.7),
        other => panic!("expected the buffered Ok item next, got: {other:?}"),
    }

    mock.signal_stop();
}

/// subscribe_with_errors() forwards the FATAL terminal AudioError as the
/// FINAL item before the channel disconnects — the consumer learns *why* the
/// stream ended rather than racing a bare RecvError.
#[test]
fn subscribe_with_errors_delivers_terminal_before_disconnect() {
    let mock = Arc::new(MockCapturingStream::new());
    // Queue one buffer, then stop so the next read is the fatal StreamEnded.
    mock.push_buffer(AudioBuffer::new(vec![1.0; 4], 2, 48000));
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture
        .subscribe_with_errors()
        .expect("subscribe_with_errors should succeed");

    // The buffered item arrives as Ok.
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(buf)) => assert_eq!(buf.data()[0], 1.0),
        other => panic!("expected the buffered Ok item, got: {other:?}"),
    }

    // Stop → the next read is the fatal terminal, delivered as a final Err.
    mock.signal_stop();
    let mut saw_terminal = false;
    // Drain until we either see the terminal Err or the channel disconnects.
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(_)) => continue, // any straggler buffer
            Ok(Err(e)) => {
                assert!(
                    e.is_fatal(),
                    "the final delivered Err must be the fatal terminal"
                );
                assert!(matches!(e, AudioError::StreamEnded { .. }));
                saw_terminal = true;
            }
            Err(_) => break, // disconnected
        }
    }
    assert!(
        saw_terminal,
        "subscribe_with_errors must deliver the terminal Err before disconnect"
    );
}

// ── bounded subscribe channel tests (rsac-d6a8) ───────────────────

/// rsac-d6a8: a stalled receiver must NOT grow memory without bound. The
/// channel depth is capped at SUBSCRIBE_CHANNEL_CAPACITY, the overflow is
/// dropped, and every drop is counted in subscriber_dropped_count().
#[test]
fn subscribe_bounds_channel_and_counts_drops() {
    const OVERFLOW: usize = 72;
    let total = SUBSCRIBE_CHANNEL_CAPACITY + OVERFLOW;

    let mock = Arc::new(MockCapturingStream::new());
    for _ in 0..total {
        mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
    }
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture.subscribe().expect("subscribe should succeed");
    // Stall the receiver: never recv while the pump floods the channel.

    // The pump drains all `total` buffers from the mock; the first
    // SUBSCRIBE_CHANNEL_CAPACITY fill the channel and the rest are dropped
    // and counted. Poll (bounded) until the counter reaches the overflow.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while capture.subscriber_dropped_count() < OVERFLOW as u64
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        capture.subscriber_dropped_count(),
        OVERFLOW as u64,
        "every overflowed buffer is dropped and counted, exactly once"
    );

    // End the stream so the pump exits, then drain: the channel holds
    // EXACTLY the cap — bounded depth, not the full flood.
    mock.signal_stop();
    let mut received = 0usize;
    while rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok() {
        received += 1;
    }
    assert_eq!(
        received, SUBSCRIBE_CHANNEL_CAPACITY,
        "channel depth is capped at the documented bound"
    );
}

/// rsac-d6a8: the fatal terminal is NEVER lost to a full channel. After
/// drops, subscribe_with_errors still delivers the terminal Err as the
/// final item (the pump uses a blocking send for that one item).
#[test]
fn subscribe_with_errors_terminal_survives_full_channel() {
    const OVERFLOW: usize = 10;
    let total = SUBSCRIBE_CHANNEL_CAPACITY + OVERFLOW;

    let mock = Arc::new(MockCapturingStream::new());
    for _ in 0..total {
        mock.push_buffer(AudioBuffer::new(vec![0.25; 4], 2, 48000));
    }
    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture
        .subscribe_with_errors()
        .expect("subscribe_with_errors should succeed");

    // Let the pump flood the (unread) channel and drop the overflow.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while capture.subscriber_dropped_count() < OVERFLOW as u64
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(capture.subscriber_dropped_count(), OVERFLOW as u64);

    // Now end the stream: the pump's terminal send blocks on the full
    // channel until we drain — the terminal must arrive as the FINAL item.
    mock.signal_stop();
    let mut oks = 0usize;
    let terminal = loop {
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(_)) => oks += 1,
            Ok(Err(e)) => break e,
            Err(e) => panic!("channel ended before the terminal Err arrived: {e:?}"),
        }
    };
    assert_eq!(
        oks, SUBSCRIBE_CHANNEL_CAPACITY,
        "all channel-buffered items are delivered before the terminal"
    );
    assert!(
        terminal.is_fatal() && matches!(terminal, AudioError::StreamEnded { .. }),
        "the final item is the fatal terminal, got: {terminal:?}"
    );
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(1)).is_err(),
        "channel disconnects after the terminal item"
    );
}

/// rsac-d6a8: repeated recoverable errors of the same variant are
/// coalesced (≈1/s), not forwarded once per ~1 ms poll — a persistently-
/// recoverable stream must not flood the channel with identical advisory
/// items. The audio behind the error burst is still delivered.
#[test]
fn subscribe_with_errors_coalesces_repeated_recoverable_errors() {
    const BURST: usize = 50;
    let mock = Arc::new(MockCapturingStream::new());
    for _ in 0..BURST {
        mock.inject_recoverable_error(AudioError::StreamReadError {
            reason: "persistent transient".into(),
        });
    }
    mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));

    let capture = make_mock_capture(Arc::clone(&mock));
    let rx = capture
        .subscribe_with_errors()
        .expect("subscribe_with_errors should succeed");

    // Collect items until the buffer behind the burst arrives.
    let mut err_items = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the buffer behind the error burst never arrived"
        );
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Err(e)) => {
                assert!(e.is_recoverable(), "burst items are recoverable");
                err_items += 1;
            }
            Ok(Ok(buf)) => {
                assert_eq!(buf.data()[0], 0.7);
                break;
            }
            Err(e) => panic!("channel ended prematurely: {e:?}"),
        }
    }
    // 50 identical errors ~1 ms apart span well under the 1 s coalescing
    // interval, so ~1 forwarded item is expected. Allow a little slack for
    // pathological CI scheduling (a >1 s stall between two polls legally
    // forwards again) — the point is "far fewer than the burst".
    assert!(
        (1..=3).contains(&err_items),
        "repeated same-variant recoverable errors must be coalesced \
         (expected 1..=3 forwarded, got {err_items} of {BURST})"
    );
    assert_eq!(
        capture.subscriber_dropped_count(),
        0,
        "advisory error items are never counted as dropped buffers"
    );
    mock.signal_stop();
}

// ── read_chunk_nonblocking() tests (terminal-observable read) ─────

/// read_chunk_nonblocking() yields Ok(Some(buf)) when data is available and
/// Ok(None) when the (running) ring is momentarily empty.
#[test]
fn read_chunk_nonblocking_yields_data_then_none() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.3; 4], 2, 48000));
    let capture = make_mock_capture(Arc::clone(&mock));

    let first = capture
        .read_chunk_nonblocking()
        .expect("read ok")
        .expect("a buffer is available");
    assert_eq!(first.data()[0], 0.3);

    // Ring now empty but still running → Ok(None), NOT an error.
    assert!(
        capture.read_chunk_nonblocking().expect("read ok").is_none(),
        "empty-but-running ring must yield Ok(None), not an error"
    );

    mock.signal_stop();
}

/// Unlike read_buffer() (which short-circuits to a RECOVERABLE
/// StreamReadError once the stream leaves Running), read_chunk_nonblocking()
/// surfaces the FATAL terminal StreamEnded once the ring is empty AND the
/// stream has stopped — the property the napi/Go pumps depend on.
#[test]
fn read_chunk_nonblocking_surfaces_fatal_terminal() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    mock.signal_stop();

    match capture.read_chunk_nonblocking() {
        Err(e) => {
            assert!(e.is_fatal(), "terminal read must be fatal");
            assert!(matches!(e, AudioError::StreamEnded { .. }));
        }
        other => panic!("expected fatal StreamEnded, got: {other:?}"),
    }
}

/// read_chunk_nonblocking() drains the buffered tail AFTER stop (Stopping
/// drain semantics) before reporting the fatal terminal — proving it does
/// not discard data the way read_buffer()'s is_running() guard would.
#[test]
fn read_chunk_nonblocking_drains_tail_then_reports_terminal() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![5.0; 2], 1, 48000));
    mock.signal_stop(); // stopped, but buffered tail remains
    let capture = make_mock_capture(Arc::clone(&mock));

    // The tail is drained first.
    let buf = capture
        .read_chunk_nonblocking()
        .expect("tail read ok")
        .expect("buffered tail is drained after stop");
    assert_eq!(buf.data()[0], 5.0);

    // Then the fatal terminal.
    assert!(
        matches!(
            capture.read_chunk_nonblocking(),
            Err(AudioError::StreamEnded { .. })
        ),
        "terminal reported only after the tail is drained"
    );
}

// ── terminal-read propagation: read_buffer* (simple pull) vs
//    read_chunk_* (terminal-observable) — the documented divergence ─────

/// The load-bearing contract: on a stopped stream, `read_chunk_nonblocking`
/// surfaces the FATAL `StreamEnded` while `read_buffer` DOWNGRADES to a
/// RECOVERABLE `StreamReadError`. A read loop that ends on `is_fatal()` must
/// therefore terminate on `read_chunk_nonblocking` but would spin forever on
/// `read_buffer`. This pins the exact behavior their rustdoc promises so it
/// cannot silently drift.
#[test]
fn read_buffer_downgrades_terminal_but_read_chunk_nonblocking_preserves_it() {
    // read_chunk_nonblocking → fatal StreamEnded on a stopped, empty stream.
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop();
    let capture = make_mock_capture(Arc::clone(&mock));
    match capture.read_chunk_nonblocking() {
        Err(e) => {
            assert!(
                e.is_fatal(),
                "read_chunk_nonblocking terminal must be fatal"
            );
            assert!(matches!(e, AudioError::StreamEnded { .. }));
        }
        other => panic!("expected fatal StreamEnded, got: {other:?}"),
    }

    // read_buffer → recoverable StreamReadError on the same stopped stream
    // (never StreamEnded), because it short-circuits on !is_running().
    let mock2 = Arc::new(MockCapturingStream::new());
    mock2.signal_stop();
    let capture2 = make_mock_capture(Arc::clone(&mock2));
    match capture2.read_buffer() {
        Err(e) => {
            assert!(
                e.is_recoverable(),
                "read_buffer downgrades terminal to a recoverable error"
            );
            assert!(
                matches!(e, AudioError::StreamReadError { .. }),
                "read_buffer must report StreamReadError, not StreamEnded"
            );
            assert!(
                !matches!(e, AudioError::StreamEnded { .. }),
                "read_buffer must NEVER surface the fatal terminal"
            );
        }
        other => panic!("expected recoverable StreamReadError, got: {other:?}"),
    }
}

/// The blocking pair mirrors the non-blocking pair: `read_chunk_blocking`
/// surfaces the FATAL `StreamEnded` on a stopped stream, while
/// `read_buffer_blocking` short-circuits to a RECOVERABLE `StreamReadError`.
#[test]
fn read_buffer_blocking_downgrades_terminal_but_read_chunk_blocking_preserves_it() {
    // read_chunk_blocking → fatal StreamEnded (delegates to read_chunk,
    // whose MockCapturingStream loop returns StreamEnded once stopped+empty).
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop();
    let capture = make_mock_capture(Arc::clone(&mock));
    match capture.read_chunk_blocking() {
        Err(e) => {
            assert!(e.is_fatal(), "read_chunk_blocking terminal must be fatal");
            assert!(matches!(e, AudioError::StreamEnded { .. }));
        }
        other => panic!("expected fatal StreamEnded, got: {other:?}"),
    }

    // read_buffer_blocking → recoverable StreamReadError (is_running guard).
    let mock2 = Arc::new(MockCapturingStream::new());
    mock2.signal_stop();
    let capture2 = make_mock_capture(Arc::clone(&mock2));
    match capture2.read_buffer_blocking() {
        Err(e) => {
            assert!(
                e.is_recoverable(),
                "read_buffer_blocking downgrades terminal to recoverable"
            );
            assert!(matches!(e, AudioError::StreamReadError { .. }));
        }
        other => panic!("expected recoverable StreamReadError, got: {other:?}"),
    }
}

/// Both terminal-observable read paths (`read_chunk_nonblocking` and
/// `read_chunk_blocking`) must NOT end on a *recoverable* read error: a
/// transient `StreamReadError` injected ahead of data is surfaced as-is (and
/// is `is_recoverable()`), and the buffer behind it is still delivered on the
/// retry — so a correct consumer loop that retries recoverable errors keeps
/// going and only stops on `is_fatal()`. This guards the recoverable-vs-fatal
/// split end-to-end through the AudioCapture read surface.
#[test]
fn read_chunk_paths_treat_recoverable_error_as_retryable_not_terminal() {
    let mock = Arc::new(MockCapturingStream::new());
    // One transient hiccup, then a real buffer behind it.
    mock.inject_recoverable_error(AudioError::StreamReadError {
        reason: "transient hiccup".into(),
    });
    mock.push_buffer(AudioBuffer::new(vec![0.9; 4], 2, 48000));
    let capture = make_mock_capture(Arc::clone(&mock));

    // First read observes the recoverable error (NOT fatal, NOT terminal).
    match capture.read_chunk_nonblocking() {
        Err(e) => {
            assert!(e.is_recoverable(), "injected error must be recoverable");
            assert!(!e.is_fatal());
            assert!(!matches!(e, AudioError::StreamEnded { .. }));
        }
        other => panic!("expected a recoverable error first, got: {other:?}"),
    }
    // Retry delivers the buffer that was queued behind the hiccup.
    let buf = capture
        .read_chunk_nonblocking()
        .expect("read ok on retry")
        .expect("buffer behind the recoverable error is delivered");
    assert_eq!(buf.data()[0], 0.9);

    mock.signal_stop();
}

// ── overrun_count() tests ─────────────────────────────────────────

#[test]
fn overrun_count_returns_zero_when_no_stream() {
    let capture = make_capture_without_stream();
    assert_eq!(capture.overrun_count(), 0);
}

#[test]
fn overrun_count_returns_zero_initially() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(mock);
    assert_eq!(capture.overrun_count(), 0);
}

#[test]
fn overrun_count_reflects_mock_overruns() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));

    assert_eq!(capture.overrun_count(), 0);

    mock.add_overruns(5);
    assert_eq!(capture.overrun_count(), 5);

    mock.add_overruns(3);
    assert_eq!(capture.overrun_count(), 8);
}

#[test]
fn overrun_count_returns_zero_after_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.add_overruns(10);
    let mut capture = make_mock_capture(mock);

    assert_eq!(capture.overrun_count(), 10);

    // Stop drops the stream Arc
    capture.stop().unwrap();

    // After stop, stream is None, so overrun_count returns 0
    assert_eq!(capture.overrun_count(), 0);
}

// ── buffers_iter() tests (H2) ─────────────────────────────────────

/// Regression (audit H2): the iterator must NOT end on a transient empty
/// poll. With buffers queued, `next()` yields them in order; an interleaved
/// empty poll (Ok(None)) is retried, not treated as end-of-stream.
#[test]
fn buffers_iter_yields_queued_then_continues_past_empty() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.1; 8], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.2; 8], 2, 48000));
    let mut capture = make_mock_capture(Arc::clone(&mock));

    // First two next() calls must return the queued buffers, even though the
    // mock's try_read_chunk returns Ok(None) once the queue drains (the old
    // iterator would have stopped at the first None instead of these items).
    let mut it = capture.buffers_iter();
    let b1 = it.next().expect("first item").expect("ok");
    assert_eq!(b1.data()[0], 0.1);
    let b2 = it.next().expect("second item").expect("ok");
    assert_eq!(b2.data()[0], 0.2);
    // Queue now empty but stream still running → the iterator is retrying on
    // Ok(None). Stop the stream from another thread so next() observes
    // !is_running and terminates rather than spinning forever.
    let mock2 = Arc::clone(&mock);
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        mock2.signal_stop();
    });
    assert!(
        it.next().is_none(),
        "iterator must end once the stream stops"
    );
    stopper.join().unwrap();
}

/// The iterator ends (returns None) when the capture is not running and there
/// is no stream, rather than panicking or looping.
#[test]
fn buffers_iter_ends_when_not_running() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop();
    let mut capture = make_mock_capture(mock);
    let mut it = capture.buffers_iter();
    assert!(it.next().is_none());
}

/// Regression (review R2-2): the iterator must DRAIN buffered data after the
/// stream stops, not discard the tail. We queue 5 buffers, stop the stream,
/// then iterate — all 5 must be yielded before the iterator ends.
#[test]
fn buffers_iter_drains_buffered_tail_after_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    for i in 0..5 {
        mock.push_buffer(AudioBuffer::new(vec![i as f32], 1, 48000));
    }
    // Stop BEFORE iterating — the buffered data must still be drained.
    mock.signal_stop();
    let mut capture = make_mock_capture(mock);

    let collected: Vec<f32> = capture
        .buffers_iter()
        .map(|r| r.expect("buffered reads are Ok").data()[0])
        .collect();
    assert_eq!(
        collected,
        vec![0.0, 1.0, 2.0, 3.0, 4.0],
        "iterator must drain all buffered frames after stop, then end"
    );
}

// ── callback delivery tests (H1 / ADR-0002) ──────────────────────

/// Regression (audit H1): a registered callback must actually be invoked.
/// We drive the pump helper directly against a mock stream and assert the
/// closure observes the pushed buffers, then that clearing the callback
/// stops delivery.
#[test]
fn callback_pump_invokes_registered_callback() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));

    let seen = Arc::new(AtomicU64::new(0));
    let seen_cb = Arc::clone(&seen);
    // The pump now OWNS the callback (moved in), so no shared mutex.
    let callback: AudioCallback = Box::new(move |buf: &AudioBuffer| {
        // Encode the first sample (scaled) so we can assert we saw real data.
        seen_cb.fetch_add((buf.data()[0] * 10.0) as u64, Ordering::SeqCst);
    });

    let stream: Arc<dyn CapturingStream> = mock.clone();
    let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");

    // Wait until both buffers (0.5*10 + 0.6*10 = 5 + 6 = 11) are delivered.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while seen.load(Ordering::SeqCst) < 11 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        seen.load(Ordering::SeqCst),
        11,
        "callback should have been invoked with both buffers"
    );

    // Shut the pump down → it stops consuming; further pushes are not seen.
    pump.shutdown();
    mock.push_buffer(AudioBuffer::new(vec![9.9; 4], 2, 48000));
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(
        seen.load(Ordering::SeqCst),
        11,
        "no further delivery after pump shutdown"
    );
    mock.signal_stop();
}

// ── start() lifecycle tests (L8) ─────────────────────────────────

/// Regression (audit L8): start() on an existing-but-stopped stream must
/// return an error rather than silently succeeding (a stopped stream cannot
/// be restarted). We simulate a stopped stream by signalling the mock.
#[test]
fn start_on_stopped_stream_errors() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop(); // stream exists but is not running
    let mut capture = make_mock_capture(mock);

    let result = capture.start();
    assert!(result.is_err(), "start() on a stopped stream must error");
    match result.unwrap_err() {
        AudioError::StreamStartFailed { reason } => {
            assert!(
                reason.contains("restart") || reason.contains("no longer running"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("Expected StreamStartFailed, got: {other:?}"),
    }
}

/// start() on an already-running stream is a no-op (idempotent), returning Ok.
#[test]
fn start_on_running_stream_is_noop() {
    let mock = Arc::new(MockCapturingStream::new()); // starts running
    let mut capture = make_mock_capture(mock);
    assert!(capture.start().is_ok());
    assert!(
        capture.start().is_ok(),
        "second start() on running stream is Ok"
    );
}

/// rsac-7aa2(3): restart-by-recreation is the blessed lifecycle — stop()
/// releases the stream and a later start() creates a FRESH stream from the
/// same resolved device; reads work again and the uptime anchor re-anchors.
#[test]
fn stop_then_start_restarts_by_recreation() {
    /// A device whose create_stream() hands out a fresh running mock with
    /// one buffered chunk, so the restarted capture demonstrably serves
    /// data end-to-end.
    struct RestartDevice;
    impl crate::core::interface::AudioDevice for RestartDevice {
        fn id(&self) -> crate::core::config::DeviceId {
            crate::core::config::DeviceId("restart-mock".to_string())
        }
        fn name(&self) -> String {
            "RestartMockDevice".to_string()
        }
        fn is_default(&self) -> bool {
            true
        }
        fn supported_formats(&self) -> Vec<AudioFormat> {
            vec![]
        }
        fn create_stream(&self, _config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
            let mock = MockCapturingStream::new();
            mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));
            Ok(Box::new(mock))
        }
    }

    let first = Arc::new(MockCapturingStream::new());
    let mut capture = AudioCapture {
        config: AudioCaptureConfig {
            target: CaptureTarget::SystemDefault,
            stream_config: StreamConfig::default(),
        },
        device: Some(Box::new(RestartDevice)),
        stream: Some(first),
        callback: Mutex::new(None),
        callback_pump: None,
        start_instant: Some(Instant::now()),
        subscriber_dropped: Arc::new(AtomicU64::new(0)),
        #[cfg(feature = "hotplug-recovery")]
        recovery: None,
    };
    assert!(capture.is_running());

    capture.stop().expect("stop ok");
    assert!(!capture.is_running());
    assert!(capture.uptime().is_none(), "stop clears the uptime anchor");

    // Restart: start() re-creates a stream from the resolved device.
    capture.start().expect("restart-by-recreation must succeed");
    assert!(capture.is_running(), "fresh stream is running");
    assert!(
        capture.uptime().is_some(),
        "the uptime anchor re-anchors on restart"
    );

    // The recreated stream serves data end-to-end.
    let buf = capture
        .read_chunk_nonblocking()
        .expect("read ok")
        .expect("data from the recreated stream");
    assert_eq!(buf.data()[0], 0.6);

    capture.stop().expect("second stop ok");
}

/// The pump thread exits when the stream stops (try_read_chunk → Err), and
/// shutdown() is safe to call afterwards (idempotent join).
#[test]
fn callback_pump_exits_when_stream_stops() {
    let mock = Arc::new(MockCapturingStream::new());
    let callback: AudioCallback = Box::new(|_: &AudioBuffer| {});
    let stream: Arc<dyn CapturingStream> = mock.clone();
    let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");
    // Stopping the mock makes try_read_chunk return Err → pump breaks.
    mock.signal_stop();
    std::thread::sleep(std::time::Duration::from_millis(20));
    // Joining a pump whose thread already exited must not hang or panic.
    pump.shutdown();
}

/// Regression (wave-1 review R1-#3): a callback that re-enters the capture
/// handle must not deadlock. Here the callback increments a counter and, on
/// the first invocation, flips a flag — proving the pump holds no lock across
/// the user closure (the closure could otherwise not run arbitrary code).
#[test]
fn callback_pump_holds_no_lock_during_invocation() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let mock = Arc::new(MockCapturingStream::new());
    for _ in 0..3 {
        mock.push_buffer(AudioBuffer::new(vec![1.0; 2], 2, 48000));
    }
    let count = Arc::new(AtomicU64::new(0));
    let count_cb = Arc::clone(&count);
    // The closure does real work (sleep) to widen any lock-held window; if
    // the pump held a lock across this, a concurrent shutdown join would
    // stall. We assert delivery proceeds and shutdown completes promptly.
    let callback: AudioCallback = Box::new(move |_buf: &AudioBuffer| {
        count_cb.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(1));
    });
    let stream: Arc<dyn CapturingStream> = mock.clone();
    let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while count.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "all three buffers delivered"
    );
    pump.shutdown();
    mock.signal_stop();
}

// ── uptime() tests (rsac-76dc) ────────────────────────────────────

/// Before any start(), there is no stream and therefore no uptime.
#[test]
fn uptime_is_none_before_start() {
    let capture = make_capture_without_stream();
    assert!(capture.uptime().is_none());
}

/// After a real start (mock has a stream and we set the anchor as start()
/// would), uptime() is Some and monotonically non-decreasing across two
/// reads. We construct the capture with start_instant already set to mirror
/// the post-start() state (start() itself needs a device to create a
/// stream, which the mock path bypasses).
#[test]
fn uptime_is_some_and_nondecreasing_after_start() {
    let mock = Arc::new(MockCapturingStream::new());
    let mut capture = make_mock_capture(mock);
    // Mirror what start() does on first real stream creation.
    capture.start_instant = Some(Instant::now());

    let first = capture.uptime().expect("uptime is Some after start");
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = capture.uptime().expect("uptime is Some after start");
    assert!(
        second >= first,
        "uptime must be monotonically non-decreasing: {second:?} < {first:?}"
    );
}

/// stop() tears down the stream and clears the uptime anchor, so uptime()
/// returns None afterwards.
#[test]
fn uptime_is_none_after_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    let mut capture = make_mock_capture(mock);
    capture.start_instant = Some(Instant::now());
    assert!(capture.uptime().is_some());

    capture.stop().unwrap();
    assert!(
        capture.uptime().is_none(),
        "uptime must be None after stop() drops the stream"
    );
}

/// A second start() on an already-running stream must NOT reset the uptime
/// anchor (idempotent restart). We seed an anchor, call start() (the mock
/// stream is running, so start() returns Ok without touching the anchor),
/// and assert the original anchor is preserved.
#[test]
fn uptime_anchor_not_reset_on_idempotent_restart() {
    let mock = Arc::new(MockCapturingStream::new()); // running
    let mut capture = make_mock_capture(mock);
    let anchor = Instant::now();
    capture.start_instant = Some(anchor);

    // start() on a running stream is a no-op and returns early, before the
    // is_none() branch that would re-anchor start_instant.
    capture
        .start()
        .expect("idempotent start on running stream is Ok");
    assert_eq!(
        capture.start_instant,
        Some(anchor),
        "idempotent restart must not reset the uptime anchor"
    );
}

// ── format() tests (rsac-574d) ────────────────────────────────────

/// Before start() there is no stream, so format() reports None.
#[test]
fn format_is_none_before_start() {
    let capture = make_capture_without_stream();
    assert!(capture.format().is_none());
}

/// Once started, format() returns Some(negotiated AudioFormat). A backend
/// that called set_negotiated_format(44100, 1) makes AudioCapture::format()
/// report sample_rate 44100, channels 1, normalized F32 — mirroring
/// test_format_reflects_negotiated_delivery_format in stream.rs.
#[test]
fn format_reflects_negotiated_delivery_format() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.set_negotiated_format(44100, 1);
    let capture = make_mock_capture(mock);

    let fmt = capture.format().expect("format is Some once started");
    assert_eq!(fmt.sample_rate, 44100);
    assert_eq!(fmt.channels, 1);
    assert_eq!(fmt.sample_format, SampleFormat::F32);
}

/// PU-1/PERF-07 (rsac-2c56): the consumer-observable contract that each
/// backend now wires via `producer.set_negotiated_format(...)` at its
/// negotiation point (PipeWire `param_changed`, WASAPI mix-format open,
/// CoreAudio AUHAL stream-format set). `format()` and
/// `stream_stats().format_description` must report the *delivered* format the
/// backend recorded, NOT the *requested* config — the exact regression where
/// they previously reported the requested format because no backend called
/// `set_negotiated_format`. Here the builder requested the 48k/2ch default
/// but the backend negotiated (delivered) 44100/1, so both reads track the
/// delivered values.
#[test]
fn format_reports_delivered_not_requested() {
    let requested = StreamConfig::default(); // 48 kHz, 2ch
    assert_eq!(requested.sample_rate, 48000);
    assert_eq!(requested.channels, 2);

    let mock = Arc::new(MockCapturingStream::new());
    // Backend negotiates a different *delivery* rate/channels than requested.
    mock.set_negotiated_format(44100, 1);
    let mut capture = make_mock_capture(Arc::clone(&mock));
    // The handle still carries the *requested* config unchanged...
    assert_eq!(capture.config().stream_config.sample_rate, 48000);
    assert_eq!(capture.config().stream_config.channels, 2);

    // ...but format() reports the DELIVERED format, divergent from requested.
    let delivered = capture.format().expect("format is Some once started");
    assert_eq!(delivered.sample_rate, 44100);
    assert_eq!(delivered.channels, 1);
    assert_eq!(delivered.sample_format, SampleFormat::F32);
    assert_ne!(
        delivered.sample_rate, requested.sample_rate,
        "format() must track delivered, not requested"
    );

    // And the stats description string is built from the delivered format.
    capture.start_instant = Some(Instant::now());
    assert_eq!(capture.stream_stats().format_description, "1ch 44100Hz F32");
}

/// format_description_string yields a non-empty 'Nch RHz FMT' string for
/// use by stream_stats() on the query path.
#[test]
fn format_description_string_is_well_formed() {
    let fmt = AudioFormat {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32,
    };
    let desc = format_description_string(&fmt);
    assert_eq!(desc, "2ch 48000Hz F32");
    assert!(!desc.is_empty());

    // Sanity-check the other sample formats render their tags.
    let i16_desc = format_description_string(&AudioFormat {
        sample_rate: 44100,
        channels: 1,
        sample_format: SampleFormat::I16,
    });
    assert_eq!(i16_desc, "1ch 44100Hz I16");
}

// ── stream_stats() tests (rsac-4c07) ──────────────────────────────

/// stream_stats() on a never-started capture (no stream) returns the default
/// snapshot: not running, ZERO uptime, zeroed counters, empty format, and
/// does not panic.
#[test]
fn stream_stats_default_when_no_stream() {
    let capture = make_capture_without_stream();
    let s = capture.stream_stats();
    assert!(!s.is_running);
    assert_eq!(s.uptime, Duration::ZERO);
    assert_eq!(s.buffers_captured, 0);
    assert_eq!(s.buffers_dropped, 0);
    assert_eq!(s.buffers_pushed, 0);
    assert_eq!(s.overruns, 0);
    assert!(s.format_description.is_empty());
    assert_eq!(s.dropped_ratio(), 0.0);
}

/// After pushing N, dropping M, popping K, stream_stats() reports
/// buffers_pushed==N, buffers_dropped==M, buffers_captured==K, is_running==true,
/// a non-empty format description, and a non-decreasing uptime across two reads.
#[test]
fn stream_stats_reflects_counters_and_running() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.set_negotiated_format(44100, 1);
    // N pushed, M dropped, K captured.
    mock.add_pushed(10);
    mock.add_dropped(4);
    mock.add_captured(7);
    let mut capture = make_mock_capture(Arc::clone(&mock));
    // Mirror start()'s anchor so uptime() reports Some.
    capture.start_instant = Some(Instant::now());

    let s1 = capture.stream_stats();
    assert!(s1.is_running, "mock stream is running");
    assert_eq!(s1.buffers_pushed, 10);
    assert_eq!(s1.buffers_dropped, 4);
    assert_eq!(s1.buffers_captured, 7);
    // overruns aliases buffers_dropped.
    assert_eq!(s1.overruns, 4);
    // dropped_ratio = 4 / (7 + 4) = 4/11.
    assert!((s1.dropped_ratio() - 4.0 / 11.0).abs() < f64::EPSILON);
    assert_eq!(s1.format_description, "1ch 44100Hz F32");
    assert!(!s1.format_description.is_empty());

    std::thread::sleep(std::time::Duration::from_millis(2));
    let s2 = capture.stream_stats();
    assert!(
        s2.uptime >= s1.uptime,
        "uptime must be non-decreasing across reads: {:?} < {:?}",
        s2.uptime,
        s1.uptime
    );
}

/// After stop(), the stream is dropped, so stream_stats() falls back to the
/// default snapshot (not running, ZERO uptime).
#[test]
fn stream_stats_default_after_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.add_pushed(5);
    let mut capture = make_mock_capture(mock);
    capture.start_instant = Some(Instant::now());
    assert!(capture.stream_stats().is_running);

    capture.stop().unwrap();
    let s = capture.stream_stats();
    assert!(!s.is_running);
    assert_eq!(s.uptime, Duration::ZERO);
    assert_eq!(s.buffers_pushed, 0, "no stream → zeroed counters");
}

// ── backpressure_report() tests (rsac-cfe4) ───────────────────────

/// backpressure_report() on a capture with no stream is the all-zero default.
#[test]
fn backpressure_report_default_when_no_stream() {
    let capture = make_capture_without_stream();
    let r = capture.backpressure_report();
    assert_eq!(r.pushed, 0);
    assert_eq!(r.dropped, 0);
    assert_eq!(r.drop_rate, 0.0);
    assert!(!r.is_under_backpressure);
}

/// A drop,push,drop,push pattern that never trips the consecutive-threshold
/// bool (is_under_backpressure stays false) still reports drop_rate ~0.5 in
/// the windowed report — the core motivation of rsac-cfe4.
#[test]
fn backpressure_report_surfaces_partial_loss_bool_misses() {
    let mock = Arc::new(MockCapturingStream::new());
    // Interleaved drop,push,drop,push → 2 pushed, 2 dropped, and the legacy
    // consecutive-drop bool never trips (each push resets the run).
    mock.add_pushed(2);
    mock.add_dropped(2);
    mock.set_backpressure(false);
    let capture = make_mock_capture(mock);

    let r = capture.backpressure_report();
    assert_eq!(r.pushed, 2);
    assert_eq!(r.dropped, 2);
    assert!(
        (r.drop_rate - 0.5).abs() < f64::EPSILON,
        "drop_rate should be ~0.5, got {}",
        r.drop_rate
    );
    assert!(
        !r.is_under_backpressure,
        "consecutive-drop bool stays false while windowed drop_rate shows loss"
    );
}

/// rsac-cfe4: `estimate_window_span` derives the report's `window` from the
/// buffer size and negotiated rate, falling back to ZERO when unattributable.
#[test]
fn estimate_window_span_derives_and_falls_back() {
    // The mock's format() reports the negotiated rate, which takes precedence
    // over config.sample_rate; set it so the two agree on 48000.
    let mock = Arc::new(MockCapturingStream::new());
    mock.format.lock().unwrap().sample_rate = 48000;
    let mut cap = make_mock_capture(mock);
    cap.config.stream_config.buffer_size = Some(1024);
    cap.config.stream_config.sample_rate = 48000;

    // Known buffer_size + rate → exact span: 100 buffers × 1024 frames @ 48000.
    let span = cap.estimate_window_span(100);
    let expected = 100.0 * 1024.0 / 48000.0; // ≈ 2.133s
    assert!(
        (span.as_secs_f64() - expected).abs() < 1e-6,
        "span {} should equal {}",
        span.as_secs_f64(),
        expected
    );

    // Zero buffers → zero span (no fabricated duration).
    assert_eq!(cap.estimate_window_span(0), Duration::ZERO);

    // Unknown buffer_size → ZERO (span unattributable).
    cap.config.stream_config.buffer_size = None;
    assert_eq!(cap.estimate_window_span(100), Duration::ZERO);
}

/// rsac-cfe4: when neither the negotiated format nor the config carries a
/// usable rate, `estimate_window_span` falls back to ZERO (div-by-zero guard)
/// rather than fabricating a span.
#[test]
fn estimate_window_span_zero_rate_falls_back() {
    let mock = Arc::new(MockCapturingStream::new());
    // Negotiated format rate 0 AND config rate 0 → no usable rate anywhere.
    mock.format.lock().unwrap().sample_rate = 0;
    let mut cap = make_mock_capture(mock);
    cap.config.stream_config.buffer_size = Some(512);
    cap.config.stream_config.sample_rate = 0;
    assert_eq!(cap.estimate_window_span(100), Duration::ZERO);
}

/// rsac-cfe4: `backpressure_report()` reports a non-zero `window` when the
/// buffer size and rate are known, matching `estimate_window_span`.
#[test]
fn backpressure_report_populates_window_span() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.add_pushed(10);
    mock.add_dropped(2);
    mock.format.lock().unwrap().sample_rate = 48000;
    let mut cap = make_mock_capture(mock);
    cap.config.stream_config.buffer_size = Some(960);
    cap.config.stream_config.sample_rate = 48000;

    let r = cap.backpressure_report();
    assert_eq!(r.pushed, 10);
    assert_eq!(r.dropped, 2);
    // window == (pushed + dropped=12) × 960 / 48000 = 0.24s, NOT Duration::ZERO.
    let expected = 12.0 * 960.0 / 48000.0;
    assert!(
        (r.window.as_secs_f64() - expected).abs() < 1e-6,
        "window {} should equal {}",
        r.window.as_secs_f64(),
        expected
    );
    assert_ne!(
        r.window,
        Duration::ZERO,
        "window must be populated, not lifetime-ZERO"
    );
}

/// The legacy bool is carried through unchanged when it is set.
#[test]
fn backpressure_report_carries_legacy_bool() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.add_pushed(1);
    mock.add_dropped(9);
    mock.set_backpressure(true);
    let capture = make_mock_capture(mock);

    let r = capture.backpressure_report();
    assert!(r.is_under_backpressure, "legacy bool carried through");
    assert!((r.drop_rate - 0.9).abs() < f64::EPSILON);
}

// ── target_str() tests (rsac-0f75) ────────────────────────────────

/// target_str("system") parses to SystemDefault and the builder carries it.
#[test]
fn target_str_system_sets_system_default() {
    let builder = AudioCaptureBuilder::new()
        .target_str("system")
        .expect("'system' parses");
    assert_eq!(builder.target, CaptureTarget::SystemDefault);
}

/// target_str("app:55") → Application(ApplicationId("55")).
#[test]
fn target_str_app_sets_application() {
    let builder = AudioCaptureBuilder::new()
        .target_str("app:55")
        .expect("'app:55' parses");
    assert_eq!(
        builder.target,
        CaptureTarget::Application(crate::core::config::ApplicationId("55".to_string()))
    );
}

/// target_str("name:VLC") → ApplicationByName("VLC").
#[test]
fn target_str_name_sets_application_by_name() {
    let builder = AudioCaptureBuilder::new()
        .target_str("name:VLC")
        .expect("'name:VLC' parses");
    assert_eq!(
        builder.target,
        CaptureTarget::ApplicationByName("VLC".to_string())
    );
}

/// A device string with embedded colons round-trips (first-colon split).
#[test]
fn target_str_device_preserves_colons() {
    let builder = AudioCaptureBuilder::new()
        .target_str("device:hw:0,0")
        .expect("'device:hw:0,0' parses");
    assert_eq!(
        builder.target,
        CaptureTarget::Device(crate::core::config::DeviceId("hw:0,0".to_string()))
    );
}

/// target_str("garbage") returns InvalidParameter and (because the method
/// consumes self and only returns the builder on success) the caller's
/// builder is untouched — we verify the error shape and that a fresh
/// builder still defaults to SystemDefault.
#[test]
fn target_str_garbage_errors_and_does_not_mutate_target() {
    let builder = AudioCaptureBuilder::new();
    // Sanity: starts at the default target.
    assert_eq!(builder.target, CaptureTarget::SystemDefault);

    let result = builder.target_str("garbage");
    assert!(result.is_err(), "unknown scheme must error");
    match result.unwrap_err() {
        AudioError::InvalidParameter { param, .. } => {
            assert_eq!(param, "capture_target");
        }
        e => panic!("Expected InvalidParameter, got: {e:?}"),
    }

    // The builder was consumed by the failed call; a freshly created one
    // still carries the default target (no global mutation occurred).
    assert_eq!(
        AudioCaptureBuilder::new().target,
        CaptureTarget::SystemDefault
    );
}

/// target_str is chainable (returns AudioResult<Self>) and composes with the
/// other setters.
#[test]
fn target_str_is_chainable() {
    let builder = AudioCaptureBuilder::new()
        .sample_rate(44100)
        .target_str("app:7")
        .expect("parses")
        .channels(1);
    assert_eq!(
        builder.target,
        CaptureTarget::Application(crate::core::config::ApplicationId("7".to_string()))
    );
    assert_eq!(builder.config.sample_rate, 44100);
    assert_eq!(builder.config.channels, 1);
}

/// The infallible try_target_str applies a valid string and silently keeps
/// the prior target for an invalid one.
#[test]
fn try_target_str_applies_valid_keeps_prior_on_invalid() {
    // Valid → applied.
    let ok = AudioCaptureBuilder::new().try_target_str("name:Spotify");
    assert_eq!(
        ok.target,
        CaptureTarget::ApplicationByName("Spotify".to_string())
    );

    // Invalid → unchanged. Pre-set a non-default target, feed garbage, and
    // assert the prior target survives.
    let kept = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::pid(99))
        .try_target_str("garbage");
    assert_eq!(
        kept.target,
        CaptureTarget::ProcessTree(crate::core::config::ProcessId(99))
    );
}

// ── preflight() tests (rsac-b65a) ─────────────────────────────────

/// preflight() passes for a valid SystemDefault/48000/2ch config, without
/// enumerating any device.
#[test]
fn preflight_ok_for_valid_default_config() {
    // Device target: valid on every platform WITHOUT a consent artifact
    // (SystemDefault honestly requires consent on mobile, ADR-0013 —
    // first surfaced when the emulator/simulator legs ran these dormant
    // tests, rsac-e6d3/97c8). This test is about parameter validity.
    let builder = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(crate::core::config::DeviceId(
            "default".to_string(),
        )))
        .sample_rate(48000)
        .channels(2);
    assert!(
        builder.preflight().is_ok(),
        "valid config must pass preflight"
    );
}

/// preflight() rejects an unsupported sample rate with
/// InvalidParameter{param:"sample_rate"} and does NOT touch a device.
#[test]
fn preflight_rejects_unsupported_sample_rate() {
    let builder = AudioCaptureBuilder::new().sample_rate(11025);
    match builder.preflight().unwrap_err() {
        AudioError::InvalidParameter { param, reason } => {
            assert_eq!(param, "sample_rate");
            assert!(reason.contains("11025"));
        }
        e => panic!("Expected InvalidParameter, got: {e:?}"),
    }
}

/// preflight() rejects channels == 0 with ConfigurationError.
#[test]
fn preflight_rejects_zero_channels() {
    let builder = AudioCaptureBuilder::new().channels(0);
    match builder.preflight().unwrap_err() {
        AudioError::ConfigurationError { message } => {
            assert_eq!(message, "Channels must be greater than 0.");
        }
        e => panic!("Expected ConfigurationError, got: {e:?}"),
    }
}

/// preflight() rejects channels > 32 (MAX_CHANNELS) with ConfigurationError.
#[test]
fn preflight_rejects_channels_above_max() {
    let builder = AudioCaptureBuilder::new().channels(33);
    match builder.preflight().unwrap_err() {
        AudioError::ConfigurationError { message } => {
            assert!(
                message.contains("33") && message.contains("32"),
                "unexpected message: {message}"
            );
        }
        e => panic!("Expected ConfigurationError, got: {e:?}"),
    }
}

/// A consent-free target valid on every platform: parameter-validation
/// tests must not couple to SystemDefault's mobile consent requirement
/// (ADR-0013; surfaced by the runtime legs, rsac-e6d3/97c8).
fn consent_free_builder() -> AudioCaptureBuilder {
    AudioCaptureBuilder::new().with_target(CaptureTarget::Device(crate::core::config::DeviceId(
        "default".to_string(),
    )))
}

/// preflight() accepts the channel-count boundaries 1 and 32.
#[test]
fn preflight_accepts_channel_boundaries() {
    assert!(consent_free_builder().channels(1).preflight().is_ok());
    assert!(consent_free_builder().channels(32).preflight().is_ok());
}

/// preflight() accepts every rate in the supported whitelist.
#[test]
fn preflight_accepts_all_supported_sample_rates() {
    for rate in SUPPORTED_SAMPLE_RATES {
        assert!(
            consent_free_builder().sample_rate(rate).preflight().is_ok(),
            "rate {rate} should pass preflight"
        );
    }
}

/// On a platform whose backend does not support application capture,
/// preflight() with an Application target returns PlatformNotSupported —
/// the same error build() raises. We assert the capability-gated behavior
/// matches PlatformCapabilities::query() on whatever platform runs the test.
#[test]
fn preflight_application_matches_capability_gate() {
    let caps = PlatformCapabilities::query();
    let builder = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(
            crate::core::config::ApplicationId("1234".to_string()),
        ))
        .sample_rate(48000)
        .channels(2);

    let result = builder.preflight();
    if caps.supports_application_capture {
        // The capability check must pass. On mobile, a supported
        // Application tier ALSO requires the consent artifact (ADR-0013),
        // so the honest outcome there is UserConsentRequired, not Ok
        // (surfaced by the emulator leg, rsac-e6d3).
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            if caps.requires_user_consent {
                assert!(
                    matches!(result, Err(AudioError::UserConsentRequired { .. })),
                    "consent-gated app capture must ask for the artifact, got {result:?}"
                );
                return;
            }
        }
        assert!(
            result.is_ok(),
            "preflight must pass app capability when supported"
        );
    } else {
        match result.unwrap_err() {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(feature, "application capture");
                assert_eq!(platform, caps.backend_name);
            }
            e => panic!("Expected PlatformNotSupported, got: {e:?}"),
        }
    }
}

/// The refactor is behavior-preserving: build() still rejects the same
/// configs preflight() does, with the same error variants (proving build()
/// routes through preflight()). Mirrors the existing builder_fails_* tests.
#[test]
fn build_routes_through_preflight_same_errors() {
    // Zero channels → ConfigurationError, before any device work.
    match AudioCaptureBuilder::new().channels(0).build().unwrap_err() {
        AudioError::ConfigurationError { .. } => {}
        e => panic!("Expected ConfigurationError from build(), got: {e:?}"),
    }
    // Unsupported rate → InvalidParameter{sample_rate}, before any device work.
    match AudioCaptureBuilder::new()
        .sample_rate(11025)
        .build()
        .unwrap_err()
    {
        AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
        e => panic!("Expected InvalidParameter from build(), got: {e:?}"),
    }
}

// ── RunningCapture / builder.start() tests (rsac-9175) ─────────────

/// Build a RunningCapture directly from a mock-backed AudioCapture (the
/// builder.start() path needs a device the mock layer bypasses, so we wrap
/// the capture the same way builder.start() would).
fn make_running_capture(mock: Arc<MockCapturingStream>) -> RunningCapture {
    RunningCapture(make_mock_capture(mock))
}

/// RunningCapture derefs to AudioCapture: read/stats/state methods are all
/// reachable through Deref/DerefMut on the guard.
#[test]
fn running_capture_derefs_to_audio_capture() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));
    let mut guard = make_running_capture(Arc::clone(&mock));

    // Through Deref: is_running() and stream_stats().
    assert!(guard.is_running());
    let stats = guard.stream_stats();
    assert!(stats.is_running);

    // read_buffer() now takes &self; it is reachable through Deref. A
    // DerefMut caller (a &mut guard) can still invoke a &self method, so the
    // narrowing is source-compatible.
    let buf = guard
        .read_buffer()
        .expect("read ok")
        .expect("a buffer is available");
    assert_eq!(buf.data()[0], 0.7);

    // Exercise a &mut-self method through DerefMut so `mut guard` is genuinely
    // required — proving the guard forwards both &self and &mut self methods.
    let _ = guard.stop();

    mock.signal_stop();
}

/// Dropping a RunningCapture stops the capture exactly once (the guard's
/// Drop calls AudioCapture::stop, which stops the underlying stream once;
/// the AudioCapture's own Drop then finds no stream and does not re-stop).
#[test]
fn running_capture_drop_stops_once() {
    let mock = Arc::new(MockCapturingStream::new());
    let guard = make_running_capture(Arc::clone(&mock));
    assert_eq!(mock.stop_calls(), 0, "not stopped before drop");

    drop(guard);
    assert_eq!(
        mock.stop_calls(),
        1,
        "guard Drop must stop the underlying stream exactly once"
    );
}

/// into_inner() returns the wrapped AudioCapture WITHOUT triggering the
/// guard's stop. The returned capture is still running; stopping it later
/// is the caller's responsibility.
#[test]
fn running_capture_into_inner_does_not_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    let guard = make_running_capture(Arc::clone(&mock));

    let mut capture = guard.into_inner();
    assert_eq!(mock.stop_calls(), 0, "into_inner must not stop the capture");
    assert!(
        capture.is_running(),
        "capture still running after into_inner"
    );

    // The caller can now stop it explicitly.
    capture.stop().expect("explicit stop ok");
    assert_eq!(mock.stop_calls(), 1);
}

/// No double-stop: an explicit stop() followed by dropping the guard does
/// not error and does not stop the underlying stream a second time (after
/// the explicit stop, the AudioCapture has dropped its Arc, so the guard's
/// Drop stop() is a no-op on the stream).
#[test]
fn running_capture_explicit_stop_then_drop_no_double_stop() {
    let mock = Arc::new(MockCapturingStream::new());
    let mut guard = make_running_capture(Arc::clone(&mock));

    guard.stop().expect("explicit stop ok");
    assert_eq!(mock.stop_calls(), 1, "explicit stop hit the stream once");

    // Dropping after an explicit stop must not panic, error, or re-stop the
    // underlying stream (the AudioCapture already released its Arc).
    drop(guard);
    assert_eq!(
        mock.stop_calls(),
        1,
        "drop after explicit stop must not double-stop the stream"
    );
}

/// The RAII guard ties teardown to scope: leaving a block drops the guard
/// and stops the capture.
#[test]
fn running_capture_stops_at_scope_end() {
    let mock = Arc::new(MockCapturingStream::new());
    {
        let _guard = make_running_capture(Arc::clone(&mock));
        assert_eq!(mock.stop_calls(), 0);
    }
    assert_eq!(
        mock.stop_calls(),
        1,
        "leaving scope must stop the capture exactly once"
    );
}

// ── request_stop() tests (H2 / #28) ───────────────────────────────

/// request_stop() signals the underlying stream (via its idempotent stop)
/// so a parked read_buffer_blocking observes a terminal state. We assert it
/// stops the mock stream exactly once.
#[test]
fn request_stop_signals_stream_once() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    assert!(mock.is_running());
    assert_eq!(mock.stop_calls(), 0);

    // Takes &self — no &mut alias to the handle.
    capture.request_stop();
    assert_eq!(mock.stop_calls(), 1, "request_stop must signal stop once");
    assert!(
        !mock.is_running(),
        "stream is no longer running after request_stop"
    );
}

/// request_stop() is idempotent: a second call after the stream is already
/// stopped still succeeds (no panic) — it just re-signals the idempotent
/// stream stop.
#[test]
fn request_stop_is_idempotent() {
    let mock = Arc::new(MockCapturingStream::new());
    let capture = make_mock_capture(Arc::clone(&mock));
    capture.request_stop();
    capture.request_stop();
    // The mock counts each stop() call; both are accepted without panic.
    assert_eq!(mock.stop_calls(), 2);
    assert!(!mock.is_running());
}

/// request_stop() on a capture with no stream is a no-op (does not panic).
#[test]
fn request_stop_no_stream_is_noop() {
    let capture = make_capture_without_stream();
    // Must not panic when there is no stream to signal.
    capture.request_stop();
}

/// request_stop() unblocks a parked read_buffer_blocking(): a reader blocked
/// on an empty-but-running mock returns the terminal StreamEnded once
/// request_stop transitions the stream. Drives the real read path through
/// &self (proving the narrowing lets a concurrent request_stop run while a
/// read is in flight, with no &mut alias).
#[test]
fn request_stop_unblocks_parked_blocking_read() {
    use std::sync::atomic::AtomicBool;
    let mock = Arc::new(MockCapturingStream::new());
    // No buffers queued, but the mock reports running, so read_chunk() spins.
    let capture = Arc::new(make_mock_capture(Arc::clone(&mock)));

    // Barrier instead of a fixed sleep: the reader flips `entered` right
    // before it enters the blocking read, so the stop is signalled only once
    // the read is genuinely in flight — deterministic under parallel load.
    let entered = Arc::new(AtomicBool::new(false));
    let reader = {
        let capture = Arc::clone(&capture);
        let entered = Arc::clone(&entered);
        std::thread::spawn(move || {
            entered.store(true, Ordering::SeqCst);
            capture.read_buffer_blocking()
        })
    };

    // Wait (bounded) until the reader has entered the blocking read, so a
    // genuine hang still fails the test rather than spinning forever.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !entered.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    // Brief settle so the reader reaches read_chunk's spin-wait, then signal
    // a stop through &self — no &mut alias to the handle.
    std::thread::sleep(std::time::Duration::from_millis(5));
    capture.request_stop();

    let result = reader.join().expect("reader thread joins");
    match result {
        Err(AudioError::StreamEnded { .. }) => {}
        other => panic!("expected StreamEnded after request_stop, got: {other:?}"),
    }
}

// ── drain_to() tests (AEG-4 / FH-6) ───────────────────────────────

/// A test sink that records, behind shared atomics, how many buffers it
/// wrote and how many frames, and how many times flush()/close() were called.
/// The atomics let the test inspect counts after the sink has been moved into
/// (and dropped by) the drain thread. Optionally fails write() once with a
/// FATAL error to exercise the early-exit-still-finalizes path.
struct CountingSink {
    writes: Arc<AtomicU64>,
    frames: Arc<AtomicU64>,
    flushes: Arc<AtomicU64>,
    closes: Arc<AtomicU64>,
    /// When true, the FIRST write() returns a fatal error (and is counted as
    /// an attempt). Subsequent writes would succeed, but the drain loop should
    /// have already broken out.
    fail_first_write_fatal: bool,
    first_write_seen: bool,
}

impl CountingSink {
    fn shared() -> (
        Arc<AtomicU64>,
        Arc<AtomicU64>,
        Arc<AtomicU64>,
        Arc<AtomicU64>,
    ) {
        (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
    }
}

impl crate::sink::AudioSink for CountingSink {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
        if self.fail_first_write_fatal && !self.first_write_seen {
            self.first_write_seen = true;
            // A fatal write error: the drain loop must break but still
            // flush()/close() afterwards.
            return Err(AudioError::ConfigurationError {
                message: "simulated fatal sink write failure".into(),
            });
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.frames
            .fetch_add(buffer.num_frames() as u64, Ordering::SeqCst);
        Ok(())
    }

    fn flush(&mut self) -> AudioResult<()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn close(&mut self) -> AudioResult<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// drain_to() drives the sink: every buffered AudioBuffer is delivered via
/// sink.write(), and on the terminal stream the thread flushes + closes the
/// sink exactly once. This is the first path that drives AudioSink::write.
#[test]
fn drain_to_writes_all_buffers_then_flushes_and_closes() {
    let mock = Arc::new(MockCapturingStream::new());
    // Queue three buffers (2 frames each), then stop so the drain reaches a
    // fatal terminal after draining the tail.
    mock.push_buffer(AudioBuffer::new(vec![0.1; 4], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.2; 4], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.3; 4], 2, 48000));

    let (writes, frames, flushes, closes) = CountingSink::shared();
    let sink = CountingSink {
        writes: Arc::clone(&writes),
        frames: Arc::clone(&frames),
        flushes: Arc::clone(&flushes),
        closes: Arc::clone(&closes),
        fail_first_write_fatal: false,
        first_write_seen: false,
    };

    let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
    let drain = capture.drain_to(sink).expect("drain_to should succeed");

    // POLL until the pump has drained all three buffered buffers, instead of
    // a fixed sleep that raced the pump-thread start under a loaded parallel
    // run. Bounded so a genuine hang still fails the test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while writes.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // Now stop so the drain loop hits the fatal terminal and finalizes the
    // sink (flush + close exactly once).
    mock.signal_stop();

    // shutdown() joins the thread, so all writes + the single flush/close
    // have completed by the time it returns.
    drain.shutdown();

    assert_eq!(
        writes.load(Ordering::SeqCst),
        3,
        "all three buffers written"
    );
    assert_eq!(frames.load(Ordering::SeqCst), 6, "2 frames per buffer × 3");
    assert_eq!(
        flushes.load(Ordering::SeqCst),
        1,
        "flush called exactly once"
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "close called exactly once"
    );
}

/// rsac-7aa2(2): drain_to on a stream already at its fatal terminal is
/// ACCEPTED (presence-only gate, drain-the-tail policy): the drain thread
/// exits on its first read, writes nothing, and still finalizes the sink
/// (flush + close) so e.g. a WAV header is written for an empty capture.
#[test]
fn drain_to_on_terminal_stream_finalizes_immediately() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.signal_stop(); // already terminal, no tail
    let capture = RunningCapture(make_mock_capture(mock));

    let (writes, frames, flushes, closes) = CountingSink::shared();
    let sink = CountingSink {
        writes: Arc::clone(&writes),
        frames,
        flushes: Arc::clone(&flushes),
        closes: Arc::clone(&closes),
        fail_first_write_fatal: false,
        first_write_seen: false,
    };
    let drain = capture
        .drain_to(sink)
        .expect("a terminal stream is accepted (drain-the-tail policy)");
    drain.shutdown(); // joins → the finalize has completed

    assert_eq!(writes.load(Ordering::SeqCst), 0, "nothing to drain");
    assert_eq!(flushes.load(Ordering::SeqCst), 1, "sink still flushed");
    assert_eq!(closes.load(Ordering::SeqCst), 1, "sink still closed");
}

/// rsac-7aa2(2): drain_to accepts the drainable Stopping window and drains
/// the buffered tail into the sink before finalizing — the old is_running()
/// gate rejected this call and stranded the tail.
#[test]
fn drain_to_accepts_stopping_window_and_drains_tail() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.1; 4], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.2; 4], 2, 48000));
    mock.signal_stop(); // not running, tail still drainable
    let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));

    let (writes, frames, flushes, closes) = CountingSink::shared();
    let sink = CountingSink {
        writes: Arc::clone(&writes),
        frames: Arc::clone(&frames),
        flushes: Arc::clone(&flushes),
        closes: Arc::clone(&closes),
        fail_first_write_fatal: false,
        first_write_seen: false,
    };
    let drain = capture
        .drain_to(sink)
        .expect("the Stopping window must be accepted");

    // Poll until the tail is drained (bounded), then join.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while writes.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drain.shutdown();

    assert_eq!(writes.load(Ordering::SeqCst), 2, "the tail is drained");
    assert_eq!(frames.load(Ordering::SeqCst), 4, "2 frames per buffer × 2");
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

/// A FATAL write() error ends the drain loop early, but flush() and close()
/// still run so the sink is finalized (e.g. a WAV header is written).
#[test]
fn drain_to_finalizes_sink_after_fatal_write() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));

    let (writes, frames, flushes, closes) = CountingSink::shared();
    let sink = CountingSink {
        writes: Arc::clone(&writes),
        frames: Arc::clone(&frames),
        flushes: Arc::clone(&flushes),
        closes: Arc::clone(&closes),
        fail_first_write_fatal: true,
        first_write_seen: false,
    };

    let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
    let drain = capture.drain_to(sink).expect("drain_to should succeed");

    std::thread::sleep(std::time::Duration::from_millis(20));
    mock.signal_stop();
    drain.shutdown();

    // The first write failed fatally → loop broke immediately, so no
    // successful writes recorded — but flush + close still ran.
    assert_eq!(
        writes.load(Ordering::SeqCst),
        0,
        "no successful writes after the fatal first write"
    );
    assert_eq!(
        flushes.load(Ordering::SeqCst),
        1,
        "flush still runs on fatal exit"
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "close still runs on fatal exit"
    );
}

/// A recoverable read error ahead of buffered data must NOT end draining: the
/// buffer behind the transient hiccup is still written.
#[test]
fn drain_to_continues_past_recoverable_read_error() {
    let mock = Arc::new(MockCapturingStream::new());
    mock.inject_recoverable_error(AudioError::StreamReadError {
        reason: "transient hiccup".into(),
    });
    mock.push_buffer(AudioBuffer::new(vec![0.9; 4], 2, 48000));

    let (writes, frames, flushes, closes) = CountingSink::shared();
    let sink = CountingSink {
        writes: Arc::clone(&writes),
        frames: Arc::clone(&frames),
        flushes: Arc::clone(&flushes),
        closes: Arc::clone(&closes),
        fail_first_write_fatal: false,
        first_write_seen: false,
    };

    let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
    let drain = capture.drain_to(sink).expect("drain_to should succeed");

    // Poll until the pump has retried past the recoverable error and drained
    // the one buffer behind it, instead of a fixed sleep that raced the
    // pump-thread start under parallel load. Bounded so a hang still fails.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while writes.load(Ordering::SeqCst) < 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    mock.signal_stop();
    drain.shutdown();

    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "the buffer behind the recoverable error is still drained"
    );
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

/// drain_to() drains into the real bundled WavFileSink and produces a valid,
/// readable WAV file (the end-to-end round-trip the example relies on).
#[cfg(feature = "sink-wav")]
#[test]
fn drain_to_wav_round_trip_writes_valid_file() {
    use crate::core::config::AudioFormat;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drain_round_trip.wav");

    let mock = Arc::new(MockCapturingStream::new());
    // Two stereo buffers, 2 frames each at 48k.
    mock.push_buffer(AudioBuffer::new(vec![0.25, -0.25, 0.5, -0.5], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.1, -0.1, 0.2, -0.2], 2, 48000));

    let format = AudioFormat {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32,
    };
    let sink = crate::sink::WavFileSink::new(&path, &format).expect("sink");

    let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
    let drain = capture.drain_to(sink).expect("drain_to should succeed");

    std::thread::sleep(std::time::Duration::from_millis(20));
    mock.signal_stop();
    drain.shutdown(); // joins the thread → file flushed + finalized

    // The WAV must be valid and contain all the samples we pushed.
    let reader = hound::WavReader::open(&path).expect("valid WAV");
    let spec = reader.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, 48000);
    let samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 8, "two 4-sample buffers");
    assert!((samples[0] - 0.25).abs() < 1e-6);
    assert!((samples[7] - (-0.2)).abs() < 1e-6);
}

/// rsac-2135: the new inherent `AudioCapture::drain_to` surface (not via
/// `RunningCapture`) round-trips a WAV. Drives the driver from a plain
/// started `AudioCapture` — the `&mut start()` path, modelled here by a mock
/// stream already present on the handle (as `start()` leaves it).
#[cfg(feature = "sink-wav")]
#[test]
fn audio_capture_drain_to_wav_round_trip_writes_valid_file() {
    use crate::core::config::AudioFormat;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio_capture_drain_round_trip.wav");

    let mock = Arc::new(MockCapturingStream::new());
    mock.push_buffer(AudioBuffer::new(vec![0.25, -0.25, 0.5, -0.5], 2, 48000));
    mock.push_buffer(AudioBuffer::new(vec![0.1, -0.1, 0.2, -0.2], 2, 48000));

    let format = AudioFormat {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32,
    };
    let sink = crate::sink::WavFileSink::new(&path, &format).expect("sink");

    // A plain AudioCapture (NOT wrapped in RunningCapture) — the surface
    // rsac-2135 adds.
    let capture: AudioCapture = make_mock_capture(Arc::clone(&mock));
    let drain = capture.drain_to(sink).expect("drain_to should succeed");

    std::thread::sleep(std::time::Duration::from_millis(20));
    mock.signal_stop();
    drain.shutdown(); // joins the thread → file flushed + finalized

    let reader = hound::WavReader::open(&path).expect("valid WAV");
    let spec = reader.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, 48000);
    let samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 8, "two 4-sample buffers");
    assert!((samples[0] - 0.25).abs() < 1e-6);
    assert!((samples[7] - (-0.2)).abs() < 1e-6);
}

/// rsac-2135: an `AudioCapture` that was never started has no stream, so
/// `drain_to` fails the presence gate with a `StreamReadError` that
/// classifies as `NotInitialized` via `lifecycle_stage()`.
#[test]
fn audio_capture_drain_to_without_stream_reports_not_initialized() {
    let capture = make_capture_without_stream();
    let sink = crate::sink::NullSink::new();
    let err = capture
        .drain_to(sink)
        .expect_err("drain_to on an unstarted capture must fail the presence gate");
    assert!(
        matches!(err, AudioError::StreamReadError { .. }),
        "expected StreamReadError, got {err:?}"
    );
    assert_eq!(
        err.lifecycle_stage(),
        Some(LifecycleStage::NotInitialized),
        "an unstarted capture classifies as NotInitialized"
    );
}

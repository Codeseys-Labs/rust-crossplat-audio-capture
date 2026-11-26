//! `WavFileSink` built from the first DELIVERED buffer's format (issue #104).
//!
//! `capture.format()` read right after `start()` can still be the REQUESTED
//! format (when the negotiated one is published is backend timing; on Linux it
//! happens asynchronously after `start()` returns). This test requests a format
//! the CI virtual devices do not offer (44.1 kHz mono against their 48 kHz
//! stereo), so the delivered format may differ from the request, and records
//! through a `WavFileSink` whose header comes from the first delivered buffer.
//! Every write must succeed, and the finished file's header and length must
//! match what was delivered and written.
//!
//! Skip policy (mirrors system_capture.rs): `require_system_capture!()` skips
//! when no audio infrastructure exists; setup failures and a missing audio
//! stream hard-fail under `RSAC_CI_AUDIO_DETERMINISTIC=1` and soft-skip
//! elsewhere. Format and header assertions on whatever was delivered are HARD
//! everywhere.
//!
//! Needs the `sink-wav` feature (`cargo test --test ci_audio --features sink-wav`
//! plus the platform's `feat_*` feature).

#![cfg(feature = "sink-wav")]

use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use rsac::core::config::AudioFormat;
use rsac::sink::{AudioSink, WavFileSink};
use rsac::{AudioBuffer, AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

/// Buffers written after the first one.
const EXTRA_BUFFERS: usize = 20;

/// Requested format: deliberately not the CI devices' native 48 kHz stereo.
const REQUESTED_RATE: u32 = 44100;
const REQUESTED_CHANNELS: u16 = 1;

/// Tone length. Long enough that the one-shot Linux/macOS players outlive the
/// whole collection (Windows loops the file instead of playing it once).
const TONE_SECS: f32 = 30.0;

/// Budget for collecting the `EXTRA_BUFFERS` after the first one, measured from
/// the moment the first buffer arrived (its own clock, not the first-buffer
/// wait's).
const COLLECT_SECS: u64 = 10;

/// Stops the tone player and removes the whole fixture directory: on Windows
/// the helper drops a `<stem>_pcm16.wav` sibling next to the float tone file.
fn cleanup(player: Option<Child>, wav_path: &Path) {
    if let Some(p) = player {
        helpers::stop_player(p);
    }
    if let Some(parent) = wav_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::remove_dir_all(parent);
    }
}

/// Setup/availability policy: hard-fail under the deterministic env, soft-skip
/// elsewhere.
fn fail_or_skip(label: &str, detail: &str) {
    if helpers::deterministic_audio_env() {
        panic!("deterministic source: {label}: {detail}");
    }
    eprintln!("[ci_audio] {label}: {detail}");
    eprintln!("[ci_audio] SKIPPING: not a test logic error");
}

/// Running totals of what was written through the sink.
#[derive(Default)]
struct Tally {
    buffers: usize,
    frames: u64,
    samples: u64,
}

/// Writes one buffer through the sink, checking first that its format still
/// equals the one the WAV header was built from. Returns the failure message
/// (both formats on a mismatch) instead of panicking so the caller can tear the
/// capture down first.
fn write_one(
    buffer: &AudioBuffer,
    sink: &mut WavFileSink,
    delivered: &AudioFormat,
    tally: &mut Tally,
) -> Result<(), String> {
    // A format change mid-stream would invalidate the single WAV header.
    if buffer.format() != delivered {
        return Err(format!(
            "delivered format changed: first buffer was {delivered:?}, buffer #{} is {:?}",
            tally.buffers,
            buffer.format()
        ));
    }
    sink.write(buffer)
        .map_err(|e| format!("write of buffer #{} failed: {e:?}", tally.buffers))?;
    tally.buffers += 1;
    tally.frames += buffer.num_frames() as u64;
    tally.samples += buffer.data().len() as u64;
    Ok(())
}

#[test]
fn test_wav_sink_from_first_delivered_buffer_matches_header() {
    require_system_capture!();

    // A tone on the default output so SystemDefault loopback has audio to capture.
    let wav_path = helpers::generate_test_wav(TONE_SECS, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    std::thread::sleep(Duration::from_millis(500));

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(REQUESTED_RATE)
        .channels(REQUESTED_CHANNELS)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            cleanup(player, &wav_path);
            fail_or_skip("capture build failed", &format!("{e:?}"));
            return;
        }
    };
    if let Err(e) = capture.start() {
        cleanup(player, &wav_path);
        fail_or_skip("capture start failed", &format!("{e:?}"));
        return;
    }
    // Diagnostic only — read right after `start()`, before any delivery, so the
    // log shows the requested-vs-delivered difference honestly.
    eprintln!(
        "[ci_audio] capture.format() right after start(): {:?}",
        capture.format()
    );

    // Phase 1: wait for the first delivered buffer. Its format is authoritative;
    // `capture.format()` is deliberately NOT used to build the sink.
    // `read_chunk_nonblocking` (not `read_buffer`) so a dead stream surfaces its
    // terminal error here instead of spinning until the deadline.
    let first_deadline = Instant::now() + helpers::capture_timeout();
    let mut first: Option<AudioBuffer> = None;
    while first.is_none() && Instant::now() < first_deadline {
        match capture.read_chunk_nonblocking() {
            Ok(Some(buffer)) => first = Some(buffer),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Read error: {e:?}");
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    let first = match first {
        Some(b) => b,
        None => {
            let _ = capture.stop();
            cleanup(player, &wav_path);
            fail_or_skip(
                "no buffer delivered",
                &format!("nothing arrived within {:?}", helpers::capture_timeout()),
            );
            return;
        }
    };

    let delivered = first.format().clone();
    eprintln!(
        "[ci_audio] requested {REQUESTED_RATE} Hz/{REQUESTED_CHANNELS} ch, first delivered \
         buffer is {} Hz/{} ch (capture.format() now: {:?})",
        delivered.sample_rate,
        delivered.channels,
        capture.format()
    );

    // Phase 2: sink from the delivered format; write the first buffer and then
    // at least EXTRA_BUFFERS more, on the collection's OWN bounded budget
    // (measured from here, not from the test start). Failures are recorded and
    // asserted after the capture and player are torn down, so a failing run
    // does not leak them.
    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path().join("wav_sink_format.wav");
    let mut sink = WavFileSink::new(&out_path, &delivered)
        .expect("WavFileSink::new must accept the delivered format");

    let mut failure: Option<String> = None;
    let mut tally = Tally::default();

    if let Err(msg) = write_one(&first, &mut sink, &delivered, &mut tally) {
        failure = Some(msg);
    }
    let collect_deadline = Instant::now() + Duration::from_secs(COLLECT_SECS);
    while failure.is_none()
        && tally.buffers < 1 + EXTRA_BUFFERS
        && Instant::now() < collect_deadline
    {
        match capture.read_chunk_nonblocking() {
            Ok(Some(buffer)) => {
                if let Err(msg) = write_one(&buffer, &mut sink, &delivered, &mut tally) {
                    failure = Some(msg);
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Read error: {e:?}");
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    let _ = capture.stop();
    cleanup(player, &wav_path);

    if let Some(msg) = failure {
        panic!("{msg}");
    }

    // Finalise, then check the file against what was delivered and written.
    sink.flush().expect("flush must succeed");
    sink.close().expect("close must succeed");
    assert_eq!(
        sink.frames_written(),
        tally.frames,
        "sink frame count must equal the frames written through it"
    );
    drop(sink);

    let mut reader =
        hound::WavReader::open(&out_path).expect("finalised WAV must reopen with hound");
    let spec = reader.spec();
    assert_eq!(
        spec.sample_rate, delivered.sample_rate,
        "WAV header sample rate must equal the delivered format"
    );
    assert_eq!(
        spec.channels, delivered.channels,
        "WAV header channel count must equal the delivered format"
    );
    assert_eq!(
        u64::from(reader.len()),
        tally.samples,
        "WAV sample count must equal the samples written"
    );
    assert_eq!(
        u64::from(reader.duration()),
        tally.frames,
        "WAV frame count must equal the frames written ({} buffers)",
        tally.buffers
    );

    // The declared length is not enough: decode the file sample by sample and
    // require every read to succeed, then compare with the tally written.
    let mut decoded: u64 = 0;
    for sample in reader.samples::<f32>() {
        let _ = sample.expect("every decoded WAV sample must read successfully");
        decoded += 1;
    }
    assert!(decoded > 0, "the finalised WAV must decode to samples");
    assert_eq!(
        decoded, tally.samples,
        "decoded sample count must equal the samples written through the sink"
    );

    // The recording itself is correct for what arrived; a short stream is an
    // environment problem, not a header mismatch.
    if tally.buffers < 1 + EXTRA_BUFFERS {
        fail_or_skip(
            "stream too short",
            &format!(
                "only {} buffers arrived within {COLLECT_SECS:?}, wanted {}",
                tally.buffers,
                1 + EXTRA_BUFFERS
            ),
        );
    }
}

//! AVAudioEngine interop: input-node tap → `BridgeProducer` (rsac-9e02).
//!
//! This is the ObjC boundary of the iOS microphone backend. It uses the
//! **typed** `objc2-avf-audio` bindings (same objc2 0.6 generation as the
//! macOS backend's `objc2-app-kit` usage) rather than raw `msg_send!` — every
//! class the mic slice needs is covered by the framework crate, so the
//! tap.rs-style raw-selector fallback is unnecessary here.
//!
//! # Capture shape
//!
//! ```text
//! AVAudioEngine → inputNode → installTapOnBus:0 bufferSize:N format:nil
//!                               │ (AVFAudio's tap thread, non-RT)
//!                               ▼
//!             tap block: interleave into pre-allocated scratch
//!                               ▼
//!             BridgeProducer::push_samples_guarded_stamped()
//! ```
//!
//! Passing `format: nil` to the tap means AVFAudio delivers the bus's native
//! format — typically **deinterleaved** float32 at the `AVAudioSession`'s
//! input rate. The tap block interleaves the planes into rsac's canonical
//! interleaved-f32 layout before pushing.
//!
//! # Real-time / allocation discipline (ADR-0001, adapted)
//!
//! The tap block runs on AVFAudio's internal tap-delivery thread — a
//! background (non-realtime) thread, *not* the hard-RT audio IO thread
//! (AVFAudio copies each period into an `AVAudioPCMBuffer` and dispatches it
//! off the IO thread). The adapted ADR-0001 rules still apply:
//!
//! - the interleave scratch `Vec<f32>` is **allocated once** at install time
//!   and never grown in the callback — a buffer that would exceed its
//!   capacity is dropped and counted (`overrun_count`), never allocated for;
//! - the push is `push_samples_guarded_stamped` — lock-free, alloc-free in
//!   steady state (free-list return ring), and panic-guarded because the
//!   block is invoked from a **foreign ObjC callback frame** where an unwind
//!   would be undefined behavior (same rule as the CoreAudio IOProc);
//! - the single `Mutex` around the tap state is uncontended by design (only
//!   the tap thread locks it after install) and is acceptable precisely
//!   because this thread is not the RT IO thread.

#![cfg(all(target_os = "ios", feature = "feat_ios"))]

use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2_avf_audio::{AVAudioEngine, AVAudioInputNode, AVAudioPCMBuffer, AVAudioTime};

use crate::bridge::ring_buffer::BridgeProducer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};

// ── Tuning constants ─────────────────────────────────────────────────────

/// Requested tap period, in sample frames, passed to
/// `installTapOnBus:bufferSize:format:block:`.
///
/// AVFAudio documents the honoured range as roughly \[100 ms, 400 ms\] and
/// clamps requests into it, so the *delivered* period is OS-chosen (commonly
/// ~4800 frames at 48 kHz). This value is only the request; the scratch
/// buffer is sized independently from the session's native rate (see
/// [`scratch_capacity_samples`]) so an upward clamp never forces a
/// callback-time allocation.
const TAP_BUFFER_FRAMES: u32 = 4096;

/// Sizes the interleave scratch buffer, in `f32` samples.
///
/// Half a second of audio at the native rate (with [`TAP_BUFFER_FRAMES`] as
/// the floor), times the channel count. That comfortably covers AVFAudio's
/// documented 400 ms maximum tap period plus clamp slack; a hypothetical
/// larger delivery is dropped + counted rather than allocated for
/// (ADR-0001 adapted). One-time cost at 48 kHz stereo: 48 000 × 4 B ≈ 192 KiB.
fn scratch_capacity_samples(sample_rate: u32, channels: u16) -> usize {
    ((sample_rate as usize) / 2).max(TAP_BUFFER_FRAMES as usize) * usize::from(channels.max(1))
}

// ── AvAudioEngineCapture ─────────────────────────────────────────────────

/// Owns the live AVAudioEngine objects for one microphone capture.
///
/// Keeps the engine and its input node retained (`Retained<_>` = ObjC strong
/// references) for the stream's whole lifetime — the tap block itself is
/// retained *by AVFAudio* (blocks passed to `installTapOnBus:` are copied),
/// so it is deliberately **not** stored here.
///
/// Not `Send`/`Sync` by itself; `IosPlatformStream` (thread.rs) wraps it in a
/// `Mutex` and documents the cross-thread safety argument there.
pub(crate) struct AvAudioEngineCapture {
    /// The engine driving the input hardware. Strong reference held for the
    /// stream's lifetime; released (ObjC refcount) on drop.
    engine: Retained<AVAudioEngine>,
    /// The engine's input node — kept so [`stop`](Self::stop) can remove the
    /// tap from the exact node it was installed on.
    input_node: Retained<AVAudioInputNode>,
}

impl AvAudioEngineCapture {
    /// Removes the tap and stops the engine (idempotent at the AVFAudio
    /// level).
    ///
    /// Order matters: the tap is removed **before** the engine stops so no
    /// further tap invocations are queued once stop returns. Removing a tap
    /// is documented safe while the engine is running, and `stop` on an
    /// already-stopped engine is a no-op.
    pub(crate) fn stop(&self) {
        autoreleasepool(|_| {
            // SAFETY: `installTapOnBus:` was called with bus 0 on this exact
            // node at creation; removing a tap (even one already removed) is
            // a documented-safe void call.
            unsafe { self.input_node.removeTapOnBus(0) };
            // SAFETY: stopping an AVAudioEngine has no preconditions and is
            // idempotent; it halts the audio hardware and releases prepared
            // resources.
            unsafe { self.engine.stop() };
        });
    }
}

// ── Tap-state (captured by the tap block) ────────────────────────────────

/// Mutable state owned by the tap block (behind a `Mutex` so the block can be
/// an ObjC-callable `Fn`).
struct TapState {
    /// Producer half of the lock-free bridge; pushes are alloc-free in steady
    /// state (free-list return ring, ADR-0001).
    producer: BridgeProducer,
    /// Pre-allocated interleave buffer — never grown in the callback.
    scratch: Vec<f32>,
    /// One-shot log guard: non-float32 delivery (should be impossible with
    /// `format: nil` on an input-node tap, but never silently spin-drop).
    warned_non_float: bool,
    /// One-shot log guard: delivered period exceeded the scratch capacity.
    warned_overflow: bool,
}

/// Counts one producer-side dropped buffer **without pushing** (the buffer
/// never reached the ring — e.g. non-float delivery or a period larger than
/// the scratch capacity).
///
/// Mirrors the accounting of `push_or_drop`'s drop arm on the shared bridge
/// counters, so the loss is visible through `overrun_count()` /
/// `buffers_dropped()` exactly like an ordinary ring-overflow drop. Lock-free
/// `Relaxed` adds — safe from the tap thread.
fn count_external_drop(producer: &BridgeProducer) {
    let shared = producer.shared();
    shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
    shared.consecutive_drops.fetch_add(1, Ordering::Relaxed);
}

// ── Interleave gather (pure logic, unit-tested) ──────────────────────────

/// Gathers one tap period into `dst` as interleaved f32, without allocating.
///
/// `channel_ptrs` follows the `AVAudioPCMBuffer.floatChannelData` contract:
/// one pointer per channel, each pointing at `frames` valid samples spaced
/// `stride` apart —
///
/// - **deinterleaved** (the common input-tap case): separate planes,
///   `stride == 1`;
/// - **interleaved**: pointers into one chunk, each offset by one frame,
///   `stride == channel count`.
///
/// The generic gather loop handles both via the stride contract; a memcpy
/// fast path covers the canonical interleaved layout.
///
/// Returns `false` — leaving `dst`'s capacity untouched and performing **no
/// allocation** — when `frames × channels` exceeds `dst`'s capacity
/// (ADR-0001 adapted: the caller drops + counts instead). Returns `true`
/// after clearing and filling `dst` with exactly `frames × channels`
/// interleaved samples.
///
/// # Safety
///
/// Every pointer in `channel_ptrs` must be valid for reads of
/// `(frames - 1) * stride + 1` `f32` values (the floatChannelData layout
/// guarantees this for `frames = frameLength` and `stride = stride()`), and
/// the pointed-to memory must not be mutated for the duration of the call.
unsafe fn gather_into_scratch(
    dst: &mut Vec<f32>,
    channel_ptrs: &[NonNull<f32>],
    frames: usize,
    stride: usize,
    interleaved: bool,
) -> bool {
    let channels = channel_ptrs.len();
    let needed = match frames.checked_mul(channels) {
        Some(n) => n,
        None => return false,
    };
    if needed > dst.capacity() {
        // Would force a reallocation on the callback thread — refuse.
        return false;
    }
    dst.clear();

    if interleaved && stride == channels && channels > 0 {
        // Canonical interleaved layout: channel 0's pointer is the base of
        // one contiguous chunk of `frames * channels` samples.
        // SAFETY: per the function contract, the base pointer is valid for
        // `(frames - 1) * stride + 1` reads per channel pointer; with the
        // channel pointers each offset by one sample, the union is exactly
        // `frames * channels` contiguous samples from the base.
        let samples = unsafe { std::slice::from_raw_parts(channel_ptrs[0].as_ptr(), needed) };
        dst.extend_from_slice(samples);
    } else {
        // Generic gather (deinterleaved planes, or any exotic stride):
        // frame-major, channel-minor — rsac's canonical interleaved order.
        // Capacity was checked above, so `push` never reallocates.
        for frame in 0..frames {
            for ch in channel_ptrs {
                // SAFETY: `frame * stride` indexes one of the `frames` valid
                // samples of this channel per the floatChannelData contract.
                dst.push(unsafe { *ch.as_ptr().add(frame * stride) });
            }
        }
    }
    true
}

// ── Engine construction + tap install ────────────────────────────────────

/// Creates an `AVAudioEngine`, installs an input-node tap that pushes
/// interleaved f32 into `producer`, and starts the engine.
///
/// Returns the live [`AvAudioEngineCapture`] (keep it alive for the stream's
/// lifetime; see thread.rs) plus the **delivered** [`AudioFormat`] — built
/// from the input node's actual sample rate / channel count, never from
/// assumptions. The same format is published on the bridge via
/// `set_negotiated_format` *before* the tap starts pushing, so
/// `CapturingStream::format()` is authoritative from the first buffer.
///
/// # Errors
///
/// [`AudioError::StreamCreationFailed`] when:
///
/// - the input node reports a 0 Hz / 0-channel format (no active input
///   route — the host app has not configured/activated a record-capable
///   `AVAudioSession`, or microphone permission is missing; both are
///   host-app responsibilities, see the module docs of `super`), or
/// - `startAndReturnError:` fails (the `NSError`'s localized description is
///   included verbatim).
pub(crate) fn start_input_capture(
    producer: BridgeProducer,
) -> AudioResult<(AvAudioEngineCapture, AudioFormat)> {
    autoreleasepool(|_| {
        // SAFETY: plain `[[AVAudioEngine alloc] init]`; no preconditions. The
        // engine is created in its default realtime-device-rendering mode.
        let engine = unsafe { AVAudioEngine::new() };

        // SAFETY: accessing `inputNode` lazily creates and attaches the
        // engine's singleton input node; no preconditions beyond a live
        // engine (held above).
        let input_node: Retained<AVAudioInputNode> = unsafe { engine.inputNode() };

        // The REAL format: what the hardware/session feeds bus 0. With
        // `format: nil` below, the tap delivers the bus's native format, so
        // this is the honest rate/channel source (the per-buffer format in
        // the tap block remains authoritative per invocation).
        //
        // SAFETY: bus 0 always exists on an input node.
        let native = unsafe { input_node.inputFormatForBus(0) };
        // SAFETY: trivial property reads on a retained, immutable
        // AVAudioFormat.
        let native_rate = unsafe { native.sampleRate() };
        let native_channels = unsafe { native.channelCount() };

        if native_rate <= 0.0 || native_channels == 0 || native_channels > u32::from(u16::MAX) {
            return Err(AudioError::StreamCreationFailed {
                reason: format!(
                    "AVAudioEngine input node reports an unusable native format \
                     ({native_rate} Hz, {native_channels} ch): no active audio \
                     input route. The HOST APP must declare \
                     NSMicrophoneUsageDescription, obtain microphone permission, \
                     and configure + activate an AVAudioSession with a \
                     record-capable category (.record / .playAndRecord) before \
                     building the capture — rsac's mobile/ios Swift helpers wrap \
                     this flow; the library deliberately does not touch the \
                     shared session"
                ),
                context: None,
            });
        }

        let delivered = AudioFormat {
            sample_rate: native_rate.round() as u32,
            channels: native_channels as u16,
            sample_format: SampleFormat::F32,
        };

        // Publish the delivery format BEFORE any push so readers never see
        // the requested-format fallback once data flows (M1 pattern; the
        // bridge normalizes sample_format to F32, which is also what the tap
        // delivers).
        producer.set_negotiated_format(&delivered);

        // Pre-allocate the interleave scratch (ADR-0001 adapted: the ONLY
        // buffer allocation, done here on the setup thread, never in the tap).
        let scratch = Vec::with_capacity(scratch_capacity_samples(
            delivered.sample_rate,
            delivered.channels,
        ));

        let state = Mutex::new(TapState {
            producer,
            scratch,
            warned_non_float: false,
            warned_overflow: false,
        });

        // The tap block. `'static`: it owns everything it touches (the Mutex
        // is moved in). Send-safety of the captured state: `TapState` is
        // `Send` (`BridgeProducer` is Send by design — it is *made* to be
        // moved to a callback thread — and `Vec<f32>`/`bool` are Send), so
        // `Mutex<TapState>` is Send + Sync; the closure only accesses the
        // state through that Mutex.
        //
        // SOUNDNESS (cross-thread invocation): AVFAudio invokes the block on
        // its internal tap thread and serializes invocations per tap; the
        // Mutex makes the state safe even against a hypothetical overlap.
        let tap_block = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
                // Autorelease pool: `pcm.format()` returns an (auto)released
                // ObjC object each invocation; pooling here keeps the foreign
                // thread from accumulating autoreleased references. Push/pop
                // of a pool is cheap and allocation-free on the Rust side.
                autoreleasepool(|_| {
                    let mut guard = match state.lock() {
                        Ok(g) => g,
                        // Poisoned == a previous invocation panicked. The push
                        // is panic-guarded so this is theoretical; skipping the
                        // period beats unwinding into the ObjC frame (UB).
                        Err(_) => return,
                    };
                    let st = &mut *guard;

                    // SAFETY: AVFAudio passes a valid AVAudioPCMBuffer that
                    // outlives this block invocation.
                    let pcm: &AVAudioPCMBuffer = unsafe { buffer.as_ref() };

                    // SAFETY: trivial property reads on the live buffer.
                    let frames = unsafe { pcm.frameLength() } as usize;
                    if frames == 0 {
                        return;
                    }

                    // Per-buffer format is authoritative (a session route
                    // change can alter rate/channels mid-stream).
                    // SAFETY: property reads on the live buffer / its format.
                    let fmt = unsafe { pcm.format() };
                    let channels = unsafe { fmt.channelCount() } as usize;
                    let rate = unsafe { fmt.sampleRate() };
                    let interleaved = unsafe { fmt.isInterleaved() };
                    let stride = unsafe { pcm.stride() };

                    // SAFETY: property read; null iff the buffer is not
                    // 32-bit float.
                    let data = unsafe { pcm.floatChannelData() };

                    if data.is_null()
                        || channels == 0
                        || channels > usize::from(u16::MAX)
                        || !rate.is_finite()
                        || rate <= 0.0
                    {
                        // Not float32 (or degenerate format): count the loss
                        // and log once — never silently spin.
                        if !st.warned_non_float {
                            st.warned_non_float = true;
                            log::warn!(
                                "AVAudioEngine tap delivered a non-float32/degenerate \
                                 buffer (channels={channels}, rate={rate}); dropping \
                                 and counting (further drops logged silently)"
                            );
                        }
                        count_external_drop(&st.producer);
                        return;
                    }

                    // SAFETY: floatChannelData is non-null (checked) and per
                    // its contract points at `channels` channel pointers,
                    // valid for this invocation.
                    let channel_ptrs: &[NonNull<f32>] =
                        unsafe { std::slice::from_raw_parts(data.cast_const(), channels) };

                    // SAFETY: `frames`/`stride` come from the same buffer the
                    // pointers describe, satisfying gather_into_scratch's
                    // validity contract.
                    let gathered = unsafe {
                        gather_into_scratch(
                            &mut st.scratch,
                            channel_ptrs,
                            frames,
                            stride,
                            interleaved,
                        )
                    };
                    if !gathered {
                        // Period larger than the pre-sized scratch: drop +
                        // count instead of allocating (ADR-0001 adapted).
                        if !st.warned_overflow {
                            st.warned_overflow = true;
                            log::warn!(
                                "AVAudioEngine tap period ({frames} frames x {channels} ch) \
                                 exceeds the pre-allocated scratch capacity \
                                 ({}); dropping and counting",
                                st.scratch.capacity()
                            );
                        }
                        count_external_drop(&st.producer);
                        return;
                    }

                    // Guarded push (foreign ObjC callback frame — an unwind
                    // out of here would be UB, same rule as the CoreAudio
                    // IOProc) with stream-position timestamps. Ring-full ⇒
                    // drop + count inside the producer; never blocks.
                    st.producer.push_samples_guarded_stamped(
                        &st.scratch,
                        channels as u16,
                        rate.round() as u32,
                    );
                });
            },
        );

        // SAFETY: bus 0 is valid; `format: nil` applies the bus's native
        // format; the block pointer is valid for the duration of the call and
        // AVFAudio *copies* (retains) the block on install, so dropping our
        // `tap_block` handle when this scope ends leaves AVFAudio's copy — and
        // the captured TapState — alive until `removeTapOnBus:` releases it.
        unsafe {
            input_node.installTapOnBus_bufferSize_format_block(
                0,
                TAP_BUFFER_FRAMES,
                None,
                RcBlock::as_ptr(&tap_block),
            );
        }

        // SAFETY: preallocates render resources; documented to have no
        // preconditions (may implicitly activate the audio session — which
        // the host app should already have done; see module docs).
        unsafe { engine.prepare() };

        // SAFETY: standard engine start; the Result maps the ObjC
        // `startAndReturnError:` out-error.
        if let Err(err) = unsafe { engine.startAndReturnError() } {
            // Roll back the tap so AVFAudio releases its copy of the block
            // (and with it the BridgeProducer) promptly.
            // SAFETY: removing the tap we just installed on bus 0.
            unsafe { input_node.removeTapOnBus(0) };
            return Err(AudioError::StreamCreationFailed {
                reason: format!(
                    "AVAudioEngine failed to start: {}. Common causes on iOS: \
                     the AVAudioSession is not configured/active with a \
                     record-capable category, or microphone permission \
                     (NSMicrophoneUsageDescription) was denied — both are host-app \
                     responsibilities (see rsac's mobile/ios helpers)",
                    err.localizedDescription()
                ),
                context: None,
            });
        }

        log::debug!(
            "AVAudioEngine: input capture started ({} Hz, {} ch, tap request {} frames)",
            delivered.sample_rate,
            delivered.channels,
            TAP_BUFFER_FRAMES
        );

        Ok((AvAudioEngineCapture { engine, input_node }, delivered))
    })
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — pure logic only (no ObjC). They compile for the iOS target under
// `--tests` and run on-device; they never touch AVAudioEngine.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `NonNull<f32>` channel pointers over borrowed slices.
    fn ptrs_of(planes: &[&[f32]]) -> Vec<NonNull<f32>> {
        planes
            .iter()
            .map(|p| NonNull::new(p.as_ptr() as *mut f32).unwrap())
            .collect()
    }

    #[test]
    fn gather_deinterleaved_stereo_interleaves_frame_major() {
        // L/R planes, stride 1 (the canonical input-tap layout).
        let left = [1.0f32, 2.0, 3.0];
        let right = [10.0f32, 20.0, 30.0];
        let ptrs = ptrs_of(&[&left, &right]);
        let mut dst = Vec::with_capacity(6);

        // SAFETY: each plane holds `frames` samples at stride 1.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 3, 1, false) };
        assert!(ok);
        assert_eq!(dst, vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
    }

    #[test]
    fn gather_interleaved_fast_path_is_a_straight_copy() {
        // One interleaved chunk; channel pointers offset by one sample,
        // stride == channels == 2.
        let interleaved = [1.0f32, 10.0, 2.0, 20.0, 3.0, 30.0];
        let base = NonNull::new(interleaved.as_ptr() as *mut f32).unwrap();
        // SAFETY: one-past offsets within the same allocation.
        let ch1 = unsafe { NonNull::new_unchecked(interleaved.as_ptr().add(1) as *mut f32) };
        let ptrs = [base, ch1];
        let mut dst = Vec::with_capacity(6);

        // SAFETY: 3 frames * stride 2 spans exactly the 6-sample chunk.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 3, 2, true) };
        assert!(ok);
        assert_eq!(dst, interleaved.to_vec());
    }

    #[test]
    fn gather_interleaved_generic_path_matches_fast_path() {
        // Force the generic loop (interleaved=false) over the same interleaved
        // layout: with stride == channels and offset pointers it must produce
        // the identical frame-major output.
        let interleaved = [1.0f32, 10.0, 2.0, 20.0, 3.0, 30.0];
        let base = NonNull::new(interleaved.as_ptr() as *mut f32).unwrap();
        // SAFETY: in-bounds offset.
        let ch1 = unsafe { NonNull::new_unchecked(interleaved.as_ptr().add(1) as *mut f32) };
        let ptrs = [base, ch1];
        let mut dst = Vec::with_capacity(6);

        // SAFETY: same layout contract as the fast-path test.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 3, 2, false) };
        assert!(ok);
        assert_eq!(dst, interleaved.to_vec());
    }

    #[test]
    fn gather_mono_passthrough() {
        let mono = [0.5f32, -0.5, 0.25, -0.25];
        let ptrs = ptrs_of(&[&mono]);
        let mut dst = Vec::with_capacity(4);

        // SAFETY: one plane of 4 samples at stride 1.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 4, 1, false) };
        assert!(ok);
        assert_eq!(dst, mono.to_vec());
    }

    #[test]
    fn gather_refuses_to_grow_scratch() {
        // Capacity 4 but 3 frames * 2 ch = 6 needed → must return false and
        // must NOT reallocate (ADR-0001 adapted: drop, don't allocate).
        let left = [1.0f32, 2.0, 3.0];
        let right = [10.0f32, 20.0, 30.0];
        let ptrs = ptrs_of(&[&left, &right]);
        let mut dst: Vec<f32> = Vec::with_capacity(4);
        let cap_before = dst.capacity();

        // SAFETY: valid planes; the function must bail before reading.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 3, 1, false) };
        assert!(!ok);
        assert_eq!(dst.capacity(), cap_before, "capacity must be untouched");
    }

    #[test]
    fn gather_zero_channels_yields_empty_ok() {
        // Degenerate but must not panic or read: 0 channels → needed == 0.
        let ptrs: [NonNull<f32>; 0] = [];
        let mut dst: Vec<f32> = Vec::with_capacity(8);
        // SAFETY: no pointers are dereferenced when the channel list is empty.
        let ok = unsafe { gather_into_scratch(&mut dst, &ptrs, 128, 1, false) };
        assert!(ok);
        assert!(dst.is_empty());
    }

    #[test]
    fn scratch_capacity_covers_the_documented_tap_clamp() {
        // AVFAudio can clamp the tap period up to ~400 ms; the scratch must
        // cover at least that at the native rate (we size to 500 ms).
        for (rate, ch) in [(48_000u32, 2u16), (44_100, 1), (96_000, 2), (8_000, 1)] {
            let cap = scratch_capacity_samples(rate, ch);
            let worst_case_400ms = (rate as usize * 2 / 5) * usize::from(ch);
            assert!(
                cap >= worst_case_400ms,
                "scratch for {rate} Hz x {ch} ch ({cap}) must cover a 400 ms period \
                 ({worst_case_400ms})"
            );
            // And never below the requested tap size either.
            assert!(cap >= TAP_BUFFER_FRAMES as usize * usize::from(ch));
        }
    }
}

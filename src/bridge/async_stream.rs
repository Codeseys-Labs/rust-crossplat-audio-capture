//! Async stream adapter for audio capture.
//!
//! Provides [`AsyncAudioStream`], which implements [`futures_core::Stream`] for
//! consuming audio data asynchronously. The stream is notified via [`atomic_waker::AtomicWaker`]
//! when new data is pushed into the ring buffer by the producer.
//!
//! # Waker contract (FH-5)
//!
//! [`CapturingStream::register_waker`]
//! returns `true` only if the backend **will** wake the registered waker when
//! new data or a terminal state arrives. The shipped `BridgeStream` always
//! returns `true`. A backend that returns `false` (including the trait default)
//! has *not* taken ownership of the waker and will never wake it.
//!
//! `poll_next` honours that contract: it must never return a bare
//! [`Poll::Pending`] after a `register_waker()` that returned `false`, because
//! nothing would ever wake the task — the future would park forever. Instead it
//! **self-wakes** (`cx.waker().wake_by_ref()`) so the executor re-polls, and it
//! bounds that self-wake budget so a backend that never produces *and* never
//! wakes fails fast with an error rather than busy-spinning indefinitely.
//! (`BridgeStream`, the shipped backend, is `pub(crate)`, so it is referenced by
//! name rather than as an intra-doc link to keep the public-docs build clean.)

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::core::buffer::AudioBuffer;
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::CapturingStream;

/// Maximum number of consecutive self-wake re-polls a non-waking backend is
/// allowed before [`AsyncAudioStream`]'s `poll_next` fails fast.
///
/// A backend whose [`register_waker`](CapturingStream::register_waker) returns
/// `false` never wakes the task, so the stream self-wakes to guarantee forward
/// progress (it never parks forever). To avoid an unbounded busy-spin when such
/// a backend *also* never produces data, the self-wake budget is capped: after
/// this many consecutive empty self-wakes the stream returns a fatal
/// [`AudioError::InternalError`] instead of yielding control again.
///
/// The budget is reset every time progress is made (a buffer is yielded), so a
/// non-waking backend that *is* steadily producing data is never penalised.
const NON_WAKING_SELF_WAKE_BUDGET: u32 = 1024;

/// An asynchronous stream of audio buffers.
///
/// Wraps a [`CapturingStream`] and implements [`futures_core::Stream`], yielding
/// [`AudioBuffer`]s as they become available from the audio capture backend.
///
/// The stream uses [`atomic_waker::AtomicWaker`] internally for efficient async
/// notification: when the producer pushes new data into the ring buffer, it wakes
/// this stream so the next `poll_next()` call can retrieve it.
///
/// # Waker contract (FH-5)
///
/// The wrapped stream is expected to support async notification — i.e.
/// [`register_waker`](CapturingStream::register_waker) returns `true`. The shipped
/// `BridgeStream` (`pub(crate)`) always does. If a backend
/// returns `false`, `poll_next` does **not** park on a waker that will never fire;
/// it self-wakes to keep the task scheduled (so it can never hang) and fails fast
/// after a bounded number of fruitless self-wakes (`NON_WAKING_SELF_WAKE_BUDGET`).
/// See the module-level docs for the full rationale.
///
/// # Usage
///
/// This type is obtained via [`AudioCapture::audio_data_stream()`](crate::AudioCapture::audio_data_stream).
/// It requires the `async-stream` feature to be enabled.
///
/// ```rust,no_run
/// # #[cfg(feature = "async-stream")]
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use rsac::AudioCaptureBuilder;
/// // use futures_core::StreamExt; // for .next()
///
/// let mut capture = AudioCaptureBuilder::new().build()?;
/// capture.start()?;
///
/// let mut stream = capture.audio_data_stream()?;
/// // Use with any async runtime's StreamExt::next()
/// # Ok(())
/// # }
/// ```
pub struct AsyncAudioStream<'a> {
    stream: &'a (dyn CapturingStream + 'a),
    /// Remaining self-wake re-polls allowed for a non-waking backend before
    /// `poll_next` fails fast. Reset to [`NON_WAKING_SELF_WAKE_BUDGET`] on every
    /// yielded buffer; only consumed when `register_waker()` returned `false`
    /// and the ring is empty. For a well-behaved (waking) backend this counter
    /// is never touched, because such a poll returns `Poll::Pending` and trusts
    /// the registered waker.
    self_wake_budget: u32,
}

impl<'a> AsyncAudioStream<'a> {
    /// Create a new `AsyncAudioStream` wrapping the given `CapturingStream`.
    ///
    /// The stream is expected to support async notification (i.e.
    /// [`register_waker`](CapturingStream::register_waker) returns `true`). When
    /// it does not, `poll_next` falls back to a bounded self-wake loop so the
    /// task can never park forever on a waker that will never fire — see the
    /// type- and module-level docs (FH-5).
    pub(crate) fn new(stream: &'a dyn CapturingStream) -> Self {
        Self {
            stream,
            self_wake_budget: NON_WAKING_SELF_WAKE_BUDGET,
        }
    }
}

impl Stream for AsyncAudioStream<'_> {
    type Item = AudioResult<AudioBuffer>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // Fast path: try non-blocking read.
        match this.stream.try_read_chunk() {
            Ok(Some(buffer)) => {
                // Progress made — refill the self-wake budget so a steadily
                // producing non-waking backend is never starved.
                this.self_wake_budget = NON_WAKING_SELF_WAKE_BUDGET;
                return Poll::Ready(Some(Ok(buffer)));
            }
            Ok(None) => {} // Empty, fall through to register waker.
            Err(e) => {
                // StreamReadError when state is not readable = stream ended.
                // Return None to signal end of stream.
                if !this.stream.is_stream_producing() {
                    return Poll::Ready(None);
                }
                return Poll::Ready(Some(Err(e)));
            }
        }

        // Check if producer is done and buffer is drained.
        if !this.stream.is_stream_producing() {
            // Producer done. Do one final drain attempt.
            match this.stream.try_read_chunk() {
                Ok(Some(buffer)) => {
                    this.self_wake_budget = NON_WAKING_SELF_WAKE_BUDGET;
                    return Poll::Ready(Some(Ok(buffer)));
                }
                _ => return Poll::Ready(None),
            }
        }

        // Register the waker for notification from the producer. The boolean
        // result is the waker contract (FH-5): `true` means the backend took
        // ownership of the waker and WILL wake it on new data / terminal state;
        // `false` means it did not, so the consumer must NOT park on it.
        let waker_registered = this.stream.register_waker(cx.waker());

        // Double-check after waker registration to avoid a missed-wake race:
        // the producer may have pushed data between our first read and the
        // waker registration.
        match this.stream.try_read_chunk() {
            Ok(Some(buffer)) => {
                this.self_wake_budget = NON_WAKING_SELF_WAKE_BUDGET;
                Poll::Ready(Some(Ok(buffer)))
            }
            Ok(None) => {
                // Truly empty. Check if the producer is still active.
                if !this.stream.is_stream_producing() {
                    return Poll::Ready(None); // Stream done, no more data coming.
                }

                if waker_registered {
                    // Well-behaved backend: it owns the waker and will wake us
                    // when data or a terminal state arrives. Park.
                    Poll::Pending
                } else {
                    // FH-5 safety net: the backend did NOT register the waker,
                    // so nothing will ever wake this task. Parking here would
                    // hang forever. Self-wake to force a re-poll instead — but
                    // bound the budget so a backend that never produces AND
                    // never wakes fails fast rather than busy-spinning forever.
                    if this.self_wake_budget == 0 {
                        // Fail fast with a FATAL error: a backend that neither
                        // wakes nor produces is a contract violation, not a
                        // transient hiccup. Fatal (not StreamReadError) so a
                        // pump-style consumer stops cleanly instead of retrying
                        // the unproductive poll forever.
                        return Poll::Ready(Some(Err(AudioError::InternalError {
                            message: "async stream stalled: backend does not support async \
                                      wakeups (register_waker returned false) and produced no \
                                      data within the self-wake budget; the stream cannot make \
                                      progress without a waking backend"
                                .to_string(),
                            source: None,
                        })));
                    }
                    this.self_wake_budget -= 1;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Err(e) => {
                if !this.stream.is_stream_producing() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(e)))
                }
            }
        }
    }
}

//! Async stream adapter for audio capture.
//!
//! Provides [`AsyncAudioStream`], which implements [`futures_core::Stream`] for
//! consuming audio data asynchronously. The stream is notified via [`atomic_waker::AtomicWaker`]
//! when new data is pushed into the ring buffer by the producer.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::core::buffer::AudioBuffer;
use crate::core::error::AudioResult;
use crate::core::interface::CapturingStream;

/// An asynchronous stream of audio buffers.
///
/// Wraps a [`CapturingStream`] and implements [`futures_core::Stream`], yielding
/// [`AudioBuffer`]s as they become available from the audio capture backend.
///
/// The stream uses [`atomic_waker::AtomicWaker`] internally for efficient async
/// notification: when the producer pushes new data into the ring buffer, it wakes
/// this stream so the next `poll_next()` call can retrieve it.
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
}

impl<'a> AsyncAudioStream<'a> {
    /// Create a new `AsyncAudioStream` wrapping the given `CapturingStream`.
    ///
    /// The stream must support async notification (i.e., `register_waker()` returns true).
    pub(crate) fn new(stream: &'a dyn CapturingStream) -> Self {
        Self { stream }
    }
}

impl Stream for AsyncAudioStream<'_> {
    type Item = AudioResult<AudioBuffer>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // Fast path: try non-blocking read
        match this.stream.try_read_chunk() {
            Ok(Some(buffer)) => return Poll::Ready(Some(Ok(buffer))),
            Ok(None) => {} // Empty, fall through to register waker
            Err(e) => {
                // StreamReadError when state is not readable = stream ended
                // Return None to signal end of stream
                if !this.stream.is_stream_producing() {
                    return Poll::Ready(None);
                }
                return Poll::Ready(Some(Err(e)));
            }
        }

        // Check if producer is done and buffer is drained
        if !this.stream.is_stream_producing() {
            // Producer done. Do one final drain attempt.
            match this.stream.try_read_chunk() {
                Ok(Some(buffer)) => return Poll::Ready(Some(Ok(buffer))),
                _ => return Poll::Ready(None),
            }
        }

        // Register waker for notification from the producer
        this.stream.register_waker(cx.waker());

        // Double-check after waker registration to avoid missed-wake race:
        // The producer may have pushed data between our first read and the
        // waker registration.
        match this.stream.try_read_chunk() {
            Ok(Some(buffer)) => Poll::Ready(Some(Ok(buffer))),
            Ok(None) => {
                // Truly empty. Check if producer is still active.
                if !this.stream.is_stream_producing() {
                    Poll::Ready(None) // Stream done, no more data coming
                } else {
                    Poll::Pending // Will be woken when producer pushes
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

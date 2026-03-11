//! AudioSink trait — the core sink abstraction for consuming audio data.

use crate::core::buffer::AudioBuffer;
use crate::core::error::AudioResult;

/// Trait for consuming audio buffers from a capture stream.
///
/// Sinks receive audio data and can write it to files, send it over
/// channels, process it, or simply discard it. All sinks must be `Send`
/// so they can be used across threads.
///
/// # Lifecycle
/// 1. Create the sink
/// 2. Call `write()` for each audio buffer
/// 3. Call `flush()` to ensure all data is written
/// 4. Call `close()` to release resources
///
/// # Example
/// ```rust,no_run
/// use rsac::sink::AudioSink;
/// // sink.write(&buffer)?;
/// // sink.flush()?;
/// // sink.close()?;
/// ```
pub trait AudioSink: Send {
    /// Write an audio buffer to the sink.
    ///
    /// This method may buffer data internally. Call `flush()` to ensure
    /// all data has been fully processed/written.
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()>;

    /// Flush any internally buffered data.
    ///
    /// Default implementation is a no-op (for sinks that don't buffer).
    fn flush(&mut self) -> AudioResult<()> {
        Ok(())
    }

    /// Close the sink and release any resources.
    ///
    /// Default implementation calls `flush()`. After `close()`, the sink
    /// should not be used for further writes.
    fn close(&mut self) -> AudioResult<()> {
        self.flush()
    }
}

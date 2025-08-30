// src/core/error.rs

/// Represents common errors that can occur during audio operations.
///
/// This enum is designed to be comprehensive, covering a wide range of potential
/// issues from device handling to stream management and configuration problems.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AudioError {
    /// The requested device is not available or could not be found.
    DeviceNotFound,
    /// The device is currently in use by another application or process.
    DeviceBusy,
    /// The specified audio format is not supported by the device or stream.
    UnsupportedFormat(String),
    /// An invalid parameter was provided to a method.
    /// Contains a string describing the parameter and the issue.
    InvalidParameter(String),
    /// The stream is not in a valid state for the requested operation.
    /// Contains a string describing the current state and expected state.
    InvalidStreamState(String),
    /// A configuration-related error occurred.
    /// Contains a string describing the configuration issue.
    ConfigurationError(String),
    /// The buffer operation failed (e.g., overflow, underflow).
    /// Contains a string describing the buffer error.
    BufferError(String),
    /// A platform-specific or backend error occurred.
    /// Contains a string detailing the backend-specific error.
    BackendError(String),
    /// Failed to open the audio stream.
    /// Contains a string with more details about the failure.
    StreamOpenFailed(String),
    /// Failed to start the audio stream.
    /// Contains a string with more details about the failure.
    StreamStartFailed(String),
    /// Failed to stop the audio stream.
    /// Contains a string with more details about the failure.
    StreamStopFailed(String),
    /// Failed to pause the audio stream.
    /// Contains a string with more details about the failure.
    StreamPauseFailed(String),
    /// Failed to resume the audio stream.
    /// Contains a string with more details about the failure.
    StreamResumeFailed(String),
    /// Failed to close the audio stream.
    /// Contains a string with more details about the failure.
    StreamCloseFailed(String),
    /// An error occurred within a user-provided callback.
    /// Contains a string describing the callback error.
    CallbackError(String),
    /// The audio system or a component was not initialized.
    /// Contains a string identifying the uninitialized component.
    NotInitialized(String),
    /// An attempt was made to initialize an already initialized component.
    /// Contains a string identifying the component.
    AlreadyInitialized(String),
    /// An operation was aborted, potentially by user request or system event.
    /// Contains a string with more details.
    OperationAborted(String),
    /// An operation timed out.
    /// Contains a string describing the timed-out operation.
    Timeout(String),
    /// An underlying Input/Output error occurred.
    /// Contains a string describing the I/O error.
    IOError(String),
    /// An unspecified or unknown error occurred.
    /// Contains a string with any available details.
    Unknown(String),
    /// The current operating system or platform is not supported.
    /// Contains a string with more details.
    UnsupportedPlatform(String),
    /// An error occurred during device enumeration.
    DeviceEnumerationError(String),
    /// A specific device was not found by its identifier (ID, name, etc.).
    DeviceNotFoundError(String),
    /// An operation was attempted that is not valid in the current context or state.
    InvalidOperation(String),
    /// A generic error related to stream operations not covered by more specific variants.
    StreamError(String),
    /// An error specific to capture operations.
    CaptureError(String),
    /// The provided sample rate is not supported.
    UnsupportedSampleRate(u32),
    /// The target application for capture could not be found or monitored.
    ApplicationNotFound(String),
    /// An error occurred during recording operations.
    RecordingError(String),
    /// An operation timed out.
    TimeoutError,
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::DeviceNotFound => write!(f, "Audio device not found"),
            AudioError::DeviceBusy => write!(f, "Audio device is busy"),
            AudioError::UnsupportedFormat(details) => {
                write!(f, "Unsupported audio format: {}", details)
            }
            AudioError::InvalidParameter(param) => write!(f, "Invalid parameter: {}", param),
            AudioError::InvalidStreamState(state) => write!(f, "Invalid stream state: {}", state),
            AudioError::ConfigurationError(details) => {
                write!(f, "Configuration error: {}", details)
            }
            AudioError::BufferError(err) => write!(f, "Buffer error: {}", err),
            AudioError::BackendError(err) => write!(f, "Audio backend error: {}", err),
            AudioError::StreamOpenFailed(details) => {
                write!(f, "Failed to open stream: {}", details)
            }
            AudioError::StreamStartFailed(details) => {
                write!(f, "Failed to start stream: {}", details)
            }
            AudioError::StreamStopFailed(details) => {
                write!(f, "Failed to stop stream: {}", details)
            }
            AudioError::StreamPauseFailed(details) => {
                write!(f, "Failed to pause stream: {}", details)
            }
            AudioError::StreamResumeFailed(details) => {
                write!(f, "Failed to resume stream: {}", details)
            }
            AudioError::StreamCloseFailed(details) => {
                write!(f, "Failed to close stream: {}", details)
            }
            AudioError::CallbackError(details) => write!(f, "Callback error: {}", details),
            AudioError::NotInitialized(component) => {
                write!(f, "Component not initialized: {}", component)
            }
            AudioError::AlreadyInitialized(component) => {
                write!(f, "Component already initialized: {}", component)
            }
            AudioError::OperationAborted(details) => write!(f, "Operation aborted: {}", details),
            AudioError::Timeout(operation) => write!(f, "Operation timed out: {}", operation),
            AudioError::IOError(details) => write!(f, "I/O error: {}", details),
            AudioError::Unknown(err) => write!(f, "Unknown audio error: {}", err),
            AudioError::UnsupportedPlatform(details) => {
                write!(f, "Unsupported platform: {}", details)
            }
            AudioError::DeviceEnumerationError(details) => {
                write!(f, "Device enumeration error: {}", details)
            }
            AudioError::DeviceNotFoundError(details) => {
                write!(f, "Device not found: {}", details)
            }
            AudioError::InvalidOperation(details) => {
                write!(f, "Invalid operation: {}", details)
            }
            AudioError::StreamError(details) => {
                write!(f, "Stream error: {}", details)
            }
            AudioError::CaptureError(details) => {
                write!(f, "Capture error: {}", details)
            }
            AudioError::UnsupportedSampleRate(rate) => {
                write!(f, "Unsupported sample rate: {} Hz", rate)
            }
            AudioError::ApplicationNotFound(details) => {
                write!(
                    f,
                    "Target application not found or could not be monitored: {}",
                    details
                )
            }
            AudioError::RecordingError(details) => {
                write!(f, "Recording error: {}", details)
            }
            AudioError::TimeoutError => {
                write!(f, "Operation timed out")
            }
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // For now, none of these variants wrap another error directly.
        // This could be extended if, for example, BackendError or IOError
        // were to store the underlying error.
        None
    }
}

/// Represents errors specific to audio processing operations.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProcessError {
    /// An internal error occurred within the processor.
    #[error("Internal processing error: {0}")]
    Internal(String),
    /// A configuration error prevented processing.
    #[error("Processing configuration error: {0}")]
    Configuration(String),
    /// Required audio data was unavailable for processing.
    #[error("Audio data unavailable for processing")]
    DataUnavailable,
    /// The processing operation failed for an unspecified reason.
    #[error("Audio processing failed")]
    Failed,
}
/// A convenient Result type alias for operations within this crate.
pub type Result<T> = std::result::Result<T, AudioError>;

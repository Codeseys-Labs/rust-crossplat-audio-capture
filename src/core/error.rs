// src/core/error.rs
//
// Canonical error taxonomy for the rsac library.
// 21 categorized variants with ErrorKind, Recoverability, and BackendContext.

use std::fmt;

// ── Supporting Types ─────────────────────────────────────────────────────

/// Categorizes an [`AudioError`] into a high-level domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Configuration,
    Device,
    Stream,
    Backend,
    Application,
    Platform,
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::Configuration => write!(f, "Configuration"),
            ErrorKind::Device => write!(f, "Device"),
            ErrorKind::Stream => write!(f, "Stream"),
            ErrorKind::Backend => write!(f, "Backend"),
            ErrorKind::Application => write!(f, "Application"),
            ErrorKind::Platform => write!(f, "Platform"),
            ErrorKind::Internal => write!(f, "Internal"),
        }
    }
}

/// Three-state recoverability classification for [`AudioError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Recoverability {
    /// The error is recoverable — the caller can continue normally.
    Recoverable,
    /// The error is transient — retrying the operation may succeed.
    TransientRetry,
    /// The error is fatal — the caller should abandon the operation.
    Fatal,
}

impl fmt::Display for Recoverability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Recoverability::Recoverable => write!(f, "Recoverable"),
            Recoverability::TransientRetry => write!(f, "TransientRetry"),
            Recoverability::Fatal => write!(f, "Fatal"),
        }
    }
}

/// Platform-specific backend context attached to certain error variants.
#[derive(Debug, Clone)]
pub struct BackendContext {
    pub backend_name: String,
    pub os_error_code: Option<i64>,
    pub os_error_message: Option<String>,
}

impl fmt::Display for BackendContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.backend_name)?;
        if let Some(code) = self.os_error_code {
            write!(f, " os_error={}", code)?;
        }
        if let Some(ref msg) = self.os_error_message {
            write!(f, " ({})", msg)?;
        }
        Ok(())
    }
}

// ── AudioError ───────────────────────────────────────────────────────────

/// Represents all errors that can occur during audio operations.
///
/// Organized into 7 categories with 21 total variants.
/// Each variant carries structured context for diagnostics.
#[derive(Debug)]
pub enum AudioError {
    // ── Configuration errors ─────────────────────────────────────────
    /// A parameter value is invalid.
    InvalidParameter { param: String, reason: String },
    /// The requested audio format is not supported.
    UnsupportedFormat {
        format: String,
        context: Option<BackendContext>,
    },
    /// A general configuration error.
    ConfigurationError { message: String },

    // ── Device errors ────────────────────────────────────────────────
    /// The requested device was not found.
    DeviceNotFound { device_id: String },
    /// The device exists but is not currently available.
    DeviceNotAvailable { device_id: String, reason: String },
    /// Failed to enumerate audio devices.
    DeviceEnumerationError {
        reason: String,
        context: Option<BackendContext>,
    },

    // ── Stream errors ────────────────────────────────────────────────
    /// Failed to create an audio stream.
    StreamCreationFailed {
        reason: String,
        context: Option<BackendContext>,
    },
    /// Failed to start an audio stream.
    StreamStartFailed { reason: String },
    /// Failed to stop an audio stream.
    StreamStopFailed { reason: String },
    /// An error occurred while reading audio data from a stream.
    StreamReadError { reason: String },
    /// The ring buffer overflowed — audio frames were dropped.
    BufferOverrun { dropped_frames: usize },
    /// The ring buffer underran — not enough data was available.
    BufferUnderrun { requested: usize, available: usize },

    // ── Backend errors ───────────────────────────────────────────────
    /// A platform-specific backend operation failed.
    BackendError {
        backend: String,
        operation: String,
        message: String,
        context: Option<BackendContext>,
    },
    /// The requested backend is not available on this system.
    BackendNotAvailable { backend: String },
    /// The backend failed to initialize.
    BackendInitializationFailed { backend: String, reason: String },

    // ── Application capture errors ───────────────────────────────────
    /// The target application for capture was not found.
    ApplicationNotFound { identifier: String },
    /// Capturing audio from the target application failed.
    ApplicationCaptureFailed { app_id: String, reason: String },

    // ── Platform errors ──────────────────────────────────────────────
    /// The requested feature is not supported on this platform.
    PlatformNotSupported { feature: String, platform: String },
    /// The operation was denied due to insufficient permissions.
    PermissionDenied {
        operation: String,
        details: Option<String>,
    },

    // ── Internal errors ──────────────────────────────────────────────
    /// An internal or unexpected error.
    InternalError {
        message: String,
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// An operation timed out.
    Timeout {
        operation: String,
        duration: std::time::Duration,
    },
}

// ── Methods ──────────────────────────────────────────────────────────────

impl AudioError {
    /// Returns the [`ErrorKind`] category for this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            AudioError::InvalidParameter { .. }
            | AudioError::UnsupportedFormat { .. }
            | AudioError::ConfigurationError { .. } => ErrorKind::Configuration,

            AudioError::DeviceNotFound { .. }
            | AudioError::DeviceNotAvailable { .. }
            | AudioError::DeviceEnumerationError { .. } => ErrorKind::Device,

            AudioError::StreamCreationFailed { .. }
            | AudioError::StreamStartFailed { .. }
            | AudioError::StreamStopFailed { .. }
            | AudioError::StreamReadError { .. }
            | AudioError::BufferOverrun { .. }
            | AudioError::BufferUnderrun { .. } => ErrorKind::Stream,

            AudioError::BackendError { .. }
            | AudioError::BackendNotAvailable { .. }
            | AudioError::BackendInitializationFailed { .. } => ErrorKind::Backend,

            AudioError::ApplicationNotFound { .. }
            | AudioError::ApplicationCaptureFailed { .. } => ErrorKind::Application,

            AudioError::PlatformNotSupported { .. } | AudioError::PermissionDenied { .. } => {
                ErrorKind::Platform
            }

            AudioError::InternalError { .. } | AudioError::Timeout { .. } => ErrorKind::Internal,
        }
    }

    /// Returns the [`Recoverability`] classification for this error.
    ///
    /// - `Recoverable`: `BufferOverrun`, `BufferUnderrun`, `StreamReadError`
    /// - `TransientRetry`: `DeviceNotAvailable`, `Timeout`, `BackendError`
    /// - `Fatal`: everything else
    pub fn recoverability(&self) -> Recoverability {
        match self {
            AudioError::BufferOverrun { .. }
            | AudioError::BufferUnderrun { .. }
            | AudioError::StreamReadError { .. } => Recoverability::Recoverable,

            AudioError::DeviceNotAvailable { .. }
            | AudioError::Timeout { .. }
            | AudioError::BackendError { .. } => Recoverability::TransientRetry,

            _ => Recoverability::Fatal,
        }
    }

    /// Returns `true` if the error is `Recoverable` or `TransientRetry`.
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self.recoverability(),
            Recoverability::Recoverable | Recoverability::TransientRetry
        )
    }

    /// Returns `true` if the error is `Fatal`.
    pub fn is_fatal(&self) -> bool {
        self.recoverability() == Recoverability::Fatal
    }
}

// ── Display ──────────────────────────────────────────────────────────────

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Configuration
            AudioError::InvalidParameter { param, reason } => {
                write!(f, "Invalid parameter '{}': {}", param, reason)
            }
            AudioError::UnsupportedFormat { format, context } => {
                write!(f, "Unsupported audio format: {}", format)?;
                if let Some(ctx) = context {
                    write!(f, " {}", ctx)?;
                }
                Ok(())
            }
            AudioError::ConfigurationError { message } => {
                write!(f, "Configuration error: {}", message)
            }

            // Device
            AudioError::DeviceNotFound { device_id } => {
                write!(f, "Audio device not found: {}", device_id)
            }
            AudioError::DeviceNotAvailable { device_id, reason } => {
                write!(f, "Device '{}' not available: {}", device_id, reason)
            }
            AudioError::DeviceEnumerationError { reason, context } => {
                write!(f, "Device enumeration failed: {}", reason)?;
                if let Some(ctx) = context {
                    write!(f, " {}", ctx)?;
                }
                Ok(())
            }

            // Stream
            AudioError::StreamCreationFailed { reason, context } => {
                write!(f, "Stream creation failed: {}", reason)?;
                if let Some(ctx) = context {
                    write!(f, " {}", ctx)?;
                }
                Ok(())
            }
            AudioError::StreamStartFailed { reason } => {
                write!(f, "Failed to start stream: {}", reason)
            }
            AudioError::StreamStopFailed { reason } => {
                write!(f, "Failed to stop stream: {}", reason)
            }
            AudioError::StreamReadError { reason } => {
                write!(f, "Stream read error: {}", reason)
            }
            AudioError::BufferOverrun { dropped_frames } => {
                write!(f, "Buffer overrun: {} frames dropped", dropped_frames)
            }
            AudioError::BufferUnderrun {
                requested,
                available,
            } => {
                write!(
                    f,
                    "Buffer underrun: requested {} frames, only {} available",
                    requested, available
                )
            }

            // Backend
            AudioError::BackendError {
                backend,
                operation,
                message,
                context,
            } => {
                write!(
                    f,
                    "Backend '{}' error in {}: {}",
                    backend, operation, message
                )?;
                if let Some(ctx) = context {
                    write!(f, " {}", ctx)?;
                }
                Ok(())
            }
            AudioError::BackendNotAvailable { backend } => {
                write!(f, "Backend '{}' is not available", backend)
            }
            AudioError::BackendInitializationFailed { backend, reason } => {
                write!(f, "Backend '{}' initialization failed: {}", backend, reason)
            }

            // Application
            AudioError::ApplicationNotFound { identifier } => {
                write!(f, "Application not found: {}", identifier)
            }
            AudioError::ApplicationCaptureFailed { app_id, reason } => {
                write!(f, "Application capture failed for '{}': {}", app_id, reason)
            }

            // Platform
            AudioError::PlatformNotSupported { feature, platform } => {
                write!(f, "Feature '{}' not supported on {}", feature, platform)
            }
            AudioError::PermissionDenied { operation, details } => {
                write!(f, "Permission denied for operation '{}'", operation)?;
                if let Some(d) = details {
                    write!(f, ": {}", d)?;
                }
                Ok(())
            }

            // Internal
            AudioError::InternalError { message, .. } => {
                write!(f, "Internal error: {}", message)
            }
            AudioError::Timeout {
                operation,
                duration,
            } => {
                write!(
                    f,
                    "Operation '{}' timed out after {:?}",
                    operation, duration
                )
            }
        }
    }
}

// ── std::error::Error ────────────────────────────────────────────────────

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AudioError::InternalError {
                source: Some(ref e),
                ..
            } => Some(e.as_ref()),
            _ => None,
        }
    }
}

// ── From impls ───────────────────────────────────────────────────────────

impl From<std::io::Error> for AudioError {
    fn from(err: std::io::Error) -> Self {
        AudioError::InternalError {
            message: format!("I/O error: {}", err),
            source: Some(Box::new(err)),
        }
    }
}

impl From<String> for AudioError {
    fn from(msg: String) -> Self {
        AudioError::InternalError {
            message: msg,
            source: None,
        }
    }
}

impl From<&str> for AudioError {
    fn from(msg: &str) -> Self {
        AudioError::InternalError {
            message: msg.to_string(),
            source: None,
        }
    }
}

// ── Result alias ─────────────────────────────────────────────────────────

/// A convenient `Result` type alias for audio operations.
pub type AudioResult<T> = std::result::Result<T, AudioError>;

/// Legacy alias — prefer [`AudioResult`].
pub type Result<T> = AudioResult<T>;

// ── ProcessError (kept for backward compat) ──────────────────────────────

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

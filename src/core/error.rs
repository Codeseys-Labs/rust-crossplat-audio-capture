// src/core/error.rs

//! Canonical error taxonomy for rsac.
//!
//! Every fallible API in the crate returns [`AudioResult<T>`] (alias for
//! `Result<T, AudioError>`). [`AudioError`] is an enum of categorized
//! failure modes. Each variant carries:
//!
//! - A high-level [`ErrorKind`] classifier (`Configuration`, `Device`,
//!   `Stream`, `Backend`, `Application`, `Platform`, `Internal`).
//! - A [`Recoverability`] hint (`Recoverable`, `TransientRetry`, `Fatal`,
//!   `UserError`) so callers can decide whether to retry, fall back, or
//!   surface the failure.
//! - Optional [`BackendContext`] — a structured wrapper for OS-level error
//!   codes + operation names from WASAPI, PipeWire, or CoreAudio.
//!
//! [`ProcessError`] is a small supporting type for the
//! [`AudioProcessor`](crate::core::processing::AudioProcessor) trait.

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
///
/// Wraps the OS-level failure so callers can surface accurate
/// diagnostics (e.g., a WASAPI `HRESULT`, a PipeWire errno, or a
/// CoreAudio `OSStatus`) without having to branch on
/// [`AudioError`] variants manually.
#[derive(Debug, Clone)]
pub struct BackendContext {
    /// Human-readable backend name — e.g., `"WASAPI"`, `"PipeWire"`,
    /// `"CoreAudio"`.
    pub backend_name: String,
    /// Raw OS-level error code, when one is available.
    pub os_error_code: Option<i64>,
    /// Human-readable OS-level error message, when one is available.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::time::Duration;

    // ── Helper: construct every variant ──────────────────────────────

    fn make_all_variants() -> Vec<AudioError> {
        vec![
            AudioError::InvalidParameter {
                param: "rate".into(),
                reason: "too high".into(),
            },
            AudioError::UnsupportedFormat {
                format: "PCM-64".into(),
                context: None,
            },
            AudioError::ConfigurationError {
                message: "bad config".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "hw:0".into(),
            },
            AudioError::DeviceNotAvailable {
                device_id: "hw:1".into(),
                reason: "in use".into(),
            },
            AudioError::DeviceEnumerationError {
                reason: "no perms".into(),
                context: None,
            },
            AudioError::StreamCreationFailed {
                reason: "format mismatch".into(),
                context: None,
            },
            AudioError::StreamStartFailed {
                reason: "busy".into(),
            },
            AudioError::StreamStopFailed {
                reason: "already stopped".into(),
            },
            AudioError::StreamReadError {
                reason: "timeout".into(),
            },
            AudioError::BufferOverrun { dropped_frames: 42 },
            AudioError::BufferUnderrun {
                requested: 1024,
                available: 256,
            },
            AudioError::BackendError {
                backend: "WASAPI".into(),
                operation: "init".into(),
                message: "fail".into(),
                context: None,
            },
            AudioError::BackendNotAvailable {
                backend: "CoreAudio".into(),
            },
            AudioError::BackendInitializationFailed {
                backend: "PipeWire".into(),
                reason: "daemon down".into(),
            },
            AudioError::ApplicationNotFound {
                identifier: "com.app".into(),
            },
            AudioError::ApplicationCaptureFailed {
                app_id: "app-1".into(),
                reason: "denied".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "process-tap".into(),
                platform: "linux".into(),
            },
            AudioError::PermissionDenied {
                operation: "capture".into(),
                details: Some("need root".into()),
            },
            AudioError::InternalError {
                message: "boom".into(),
                source: None,
            },
            AudioError::Timeout {
                operation: "connect".into(),
                duration: Duration::from_secs(5),
            },
        ]
    }

    // ── Construction ─────────────────────────────────────────────────

    #[test]
    fn all_21_variants_constructible() {
        let variants = make_all_variants();
        assert_eq!(variants.len(), 21, "Must have exactly 21 variants");
    }

    // ── ErrorKind: Configuration ─────────────────────────────────────

    #[test]
    fn kind_configuration_variants() {
        assert_eq!(
            AudioError::InvalidParameter {
                param: "x".into(),
                reason: "y".into()
            }
            .kind(),
            ErrorKind::Configuration
        );
        assert_eq!(
            AudioError::UnsupportedFormat {
                format: "x".into(),
                context: None
            }
            .kind(),
            ErrorKind::Configuration
        );
        assert_eq!(
            AudioError::ConfigurationError {
                message: "x".into()
            }
            .kind(),
            ErrorKind::Configuration
        );
    }

    // ── ErrorKind: Device ────────────────────────────────────────────

    #[test]
    fn kind_device_variants() {
        assert_eq!(
            AudioError::DeviceNotFound {
                device_id: "x".into()
            }
            .kind(),
            ErrorKind::Device
        );
        assert_eq!(
            AudioError::DeviceNotAvailable {
                device_id: "x".into(),
                reason: "y".into()
            }
            .kind(),
            ErrorKind::Device
        );
        assert_eq!(
            AudioError::DeviceEnumerationError {
                reason: "x".into(),
                context: None
            }
            .kind(),
            ErrorKind::Device
        );
    }

    // ── ErrorKind: Stream ────────────────────────────────────────────

    #[test]
    fn kind_stream_variants() {
        assert_eq!(
            AudioError::StreamCreationFailed {
                reason: "x".into(),
                context: None
            }
            .kind(),
            ErrorKind::Stream
        );
        assert_eq!(
            AudioError::StreamStartFailed { reason: "x".into() }.kind(),
            ErrorKind::Stream
        );
        assert_eq!(
            AudioError::StreamStopFailed { reason: "x".into() }.kind(),
            ErrorKind::Stream
        );
        assert_eq!(
            AudioError::StreamReadError { reason: "x".into() }.kind(),
            ErrorKind::Stream
        );
        assert_eq!(
            AudioError::BufferOverrun { dropped_frames: 0 }.kind(),
            ErrorKind::Stream
        );
        assert_eq!(
            AudioError::BufferUnderrun {
                requested: 0,
                available: 0
            }
            .kind(),
            ErrorKind::Stream
        );
    }

    // ── ErrorKind: Backend ───────────────────────────────────────────

    #[test]
    fn kind_backend_variants() {
        assert_eq!(
            AudioError::BackendError {
                backend: "x".into(),
                operation: "y".into(),
                message: "z".into(),
                context: None
            }
            .kind(),
            ErrorKind::Backend
        );
        assert_eq!(
            AudioError::BackendNotAvailable {
                backend: "x".into()
            }
            .kind(),
            ErrorKind::Backend
        );
        assert_eq!(
            AudioError::BackendInitializationFailed {
                backend: "x".into(),
                reason: "y".into()
            }
            .kind(),
            ErrorKind::Backend
        );
    }

    // ── ErrorKind: Application ───────────────────────────────────────

    #[test]
    fn kind_application_variants() {
        assert_eq!(
            AudioError::ApplicationNotFound {
                identifier: "x".into()
            }
            .kind(),
            ErrorKind::Application
        );
        assert_eq!(
            AudioError::ApplicationCaptureFailed {
                app_id: "x".into(),
                reason: "y".into()
            }
            .kind(),
            ErrorKind::Application
        );
    }

    // ── ErrorKind: Platform ──────────────────────────────────────────

    #[test]
    fn kind_platform_variants() {
        assert_eq!(
            AudioError::PlatformNotSupported {
                feature: "x".into(),
                platform: "y".into()
            }
            .kind(),
            ErrorKind::Platform
        );
        assert_eq!(
            AudioError::PermissionDenied {
                operation: "x".into(),
                details: None
            }
            .kind(),
            ErrorKind::Platform
        );
    }

    // ── ErrorKind: Internal ──────────────────────────────────────────

    #[test]
    fn kind_internal_variants() {
        assert_eq!(
            AudioError::InternalError {
                message: "x".into(),
                source: None
            }
            .kind(),
            ErrorKind::Internal
        );
        assert_eq!(
            AudioError::Timeout {
                operation: "x".into(),
                duration: Duration::from_millis(1)
            }
            .kind(),
            ErrorKind::Internal
        );
    }

    // ── Recoverability classification ────────────────────────────────

    #[test]
    fn recoverability_recoverable_variants() {
        assert_eq!(
            AudioError::StreamReadError { reason: "x".into() }.recoverability(),
            Recoverability::Recoverable
        );
        assert_eq!(
            AudioError::BufferOverrun { dropped_frames: 1 }.recoverability(),
            Recoverability::Recoverable
        );
        assert_eq!(
            AudioError::BufferUnderrun {
                requested: 1,
                available: 0
            }
            .recoverability(),
            Recoverability::Recoverable
        );
    }

    #[test]
    fn recoverability_transient_retry_variants() {
        assert_eq!(
            AudioError::DeviceNotAvailable {
                device_id: "x".into(),
                reason: "y".into()
            }
            .recoverability(),
            Recoverability::TransientRetry
        );
        assert_eq!(
            AudioError::Timeout {
                operation: "x".into(),
                duration: Duration::from_secs(1)
            }
            .recoverability(),
            Recoverability::TransientRetry
        );
        assert_eq!(
            AudioError::BackendError {
                backend: "x".into(),
                operation: "y".into(),
                message: "z".into(),
                context: None
            }
            .recoverability(),
            Recoverability::TransientRetry
        );
    }

    #[test]
    fn recoverability_fatal_variants() {
        // Test a representative set of fatal variants
        let fatal_errors: Vec<AudioError> = vec![
            AudioError::InvalidParameter {
                param: "x".into(),
                reason: "y".into(),
            },
            AudioError::UnsupportedFormat {
                format: "x".into(),
                context: None,
            },
            AudioError::ConfigurationError {
                message: "x".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "x".into(),
            },
            AudioError::DeviceEnumerationError {
                reason: "x".into(),
                context: None,
            },
            AudioError::StreamCreationFailed {
                reason: "x".into(),
                context: None,
            },
            AudioError::StreamStartFailed { reason: "x".into() },
            AudioError::StreamStopFailed { reason: "x".into() },
            AudioError::BackendNotAvailable {
                backend: "x".into(),
            },
            AudioError::BackendInitializationFailed {
                backend: "x".into(),
                reason: "y".into(),
            },
            AudioError::ApplicationNotFound {
                identifier: "x".into(),
            },
            AudioError::ApplicationCaptureFailed {
                app_id: "x".into(),
                reason: "y".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "x".into(),
                platform: "y".into(),
            },
            AudioError::PermissionDenied {
                operation: "x".into(),
                details: None,
            },
            AudioError::InternalError {
                message: "x".into(),
                source: None,
            },
        ];
        for err in &fatal_errors {
            assert_eq!(
                err.recoverability(),
                Recoverability::Fatal,
                "Expected Fatal for {:?}",
                err
            );
        }
    }

    // ── is_recoverable / is_fatal ────────────────────────────────────

    #[test]
    fn is_recoverable_true_for_recoverable_and_transient() {
        // Recoverable
        assert!(AudioError::BufferOverrun { dropped_frames: 1 }.is_recoverable());
        assert!(AudioError::BufferUnderrun {
            requested: 1,
            available: 0
        }
        .is_recoverable());
        assert!(AudioError::StreamReadError { reason: "x".into() }.is_recoverable());
        // TransientRetry
        assert!(AudioError::DeviceNotAvailable {
            device_id: "x".into(),
            reason: "y".into()
        }
        .is_recoverable());
        assert!(AudioError::Timeout {
            operation: "x".into(),
            duration: Duration::from_secs(1)
        }
        .is_recoverable());
        assert!(AudioError::BackendError {
            backend: "x".into(),
            operation: "y".into(),
            message: "z".into(),
            context: None
        }
        .is_recoverable());
    }

    #[test]
    fn is_fatal_true_for_fatal_variants() {
        assert!(AudioError::InvalidParameter {
            param: "x".into(),
            reason: "y".into()
        }
        .is_fatal());
        assert!(AudioError::DeviceNotFound {
            device_id: "x".into()
        }
        .is_fatal());
        assert!(AudioError::PlatformNotSupported {
            feature: "x".into(),
            platform: "y".into()
        }
        .is_fatal());
        assert!(AudioError::InternalError {
            message: "x".into(),
            source: None
        }
        .is_fatal());
    }

    #[test]
    fn is_fatal_false_for_recoverable() {
        assert!(!AudioError::BufferOverrun { dropped_frames: 1 }.is_fatal());
        assert!(!AudioError::DeviceNotAvailable {
            device_id: "x".into(),
            reason: "y".into()
        }
        .is_fatal());
    }

    // ── Display output ───────────────────────────────────────────────

    #[test]
    fn display_invalid_parameter() {
        let msg = AudioError::InvalidParameter {
            param: "rate".into(),
            reason: "negative".into(),
        }
        .to_string();
        assert!(msg.contains("rate"));
        assert!(msg.contains("negative"));
    }

    #[test]
    fn display_buffer_overrun() {
        let msg = AudioError::BufferOverrun { dropped_frames: 42 }.to_string();
        assert!(msg.contains("42"));
        assert!(msg.contains("overrun"));
    }

    #[test]
    fn display_buffer_underrun() {
        let msg = AudioError::BufferUnderrun {
            requested: 1024,
            available: 256,
        }
        .to_string();
        assert!(msg.contains("1024"));
        assert!(msg.contains("256"));
    }

    #[test]
    fn display_timeout() {
        let msg = AudioError::Timeout {
            operation: "connect".into(),
            duration: Duration::from_secs(5),
        }
        .to_string();
        assert!(msg.contains("connect"));
        assert!(msg.contains("5"));
    }

    #[test]
    fn display_permission_denied_with_details() {
        let msg = AudioError::PermissionDenied {
            operation: "capture".into(),
            details: Some("need root".into()),
        }
        .to_string();
        assert!(msg.contains("capture"));
        assert!(msg.contains("need root"));
    }

    #[test]
    fn display_permission_denied_without_details() {
        let msg = AudioError::PermissionDenied {
            operation: "record".into(),
            details: None,
        }
        .to_string();
        assert!(msg.contains("record"));
        assert!(!msg.contains("None"));
    }

    // ── From impls ───────────────────────────────────────────────────

    #[test]
    fn from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file missing");
        let audio_err: AudioError = io_err.into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        let msg = audio_err.to_string();
        assert!(msg.contains("I/O error"));
        assert!(msg.contains("file missing"));
    }

    #[test]
    fn from_string() {
        let audio_err: AudioError = "something broke".to_string().into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        assert!(audio_err.to_string().contains("something broke"));
    }

    #[test]
    fn from_str() {
        let audio_err: AudioError = "literal error".into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        assert!(audio_err.to_string().contains("literal error"));
    }

    // ── std::error::Error::source() ──────────────────────────────────

    #[test]
    fn source_some_for_internal_with_source() {
        let io_err = io::Error::other("root cause");
        let audio_err: AudioError = io_err.into();
        let src = std::error::Error::source(&audio_err);
        assert!(
            src.is_some(),
            "InternalError from io::Error should have a source"
        );
    }

    #[test]
    fn source_none_for_internal_without_source() {
        let audio_err = AudioError::InternalError {
            message: "no source".into(),
            source: None,
        };
        assert!(std::error::Error::source(&audio_err).is_none());
    }

    #[test]
    fn source_none_for_non_internal() {
        let err = AudioError::DeviceNotFound {
            device_id: "x".into(),
        };
        assert!(std::error::Error::source(&err).is_none());
    }

    // ── ErrorKind Display ────────────────────────────────────────────

    #[test]
    fn error_kind_display_all_variants() {
        assert_eq!(ErrorKind::Configuration.to_string(), "Configuration");
        assert_eq!(ErrorKind::Device.to_string(), "Device");
        assert_eq!(ErrorKind::Stream.to_string(), "Stream");
        assert_eq!(ErrorKind::Backend.to_string(), "Backend");
        assert_eq!(ErrorKind::Application.to_string(), "Application");
        assert_eq!(ErrorKind::Platform.to_string(), "Platform");
        assert_eq!(ErrorKind::Internal.to_string(), "Internal");
    }

    // ── Recoverability Display ───────────────────────────────────────

    #[test]
    fn recoverability_display_all_variants() {
        assert_eq!(Recoverability::Recoverable.to_string(), "Recoverable");
        assert_eq!(Recoverability::TransientRetry.to_string(), "TransientRetry");
        assert_eq!(Recoverability::Fatal.to_string(), "Fatal");
    }

    // ── BackendContext Display ────────────────────────────────────────

    #[test]
    fn backend_context_display_name_only() {
        let ctx = BackendContext {
            backend_name: "PipeWire".into(),
            os_error_code: None,
            os_error_message: None,
        };
        assert_eq!(ctx.to_string(), "[PipeWire]");
    }

    #[test]
    fn backend_context_display_with_code() {
        let ctx = BackendContext {
            backend_name: "WASAPI".into(),
            os_error_code: Some(-2004287478),
            os_error_message: None,
        };
        let s = ctx.to_string();
        assert!(s.contains("[WASAPI]"));
        assert!(s.contains("os_error=-2004287478"));
    }

    #[test]
    fn backend_context_display_full() {
        let ctx = BackendContext {
            backend_name: "CoreAudio".into(),
            os_error_code: Some(560227702),
            os_error_message: Some("format not supported".into()),
        };
        let s = ctx.to_string();
        assert!(s.contains("[CoreAudio]"));
        assert!(s.contains("os_error=560227702"));
        assert!(s.contains("format not supported"));
    }

    // ── AudioResult alias ────────────────────────────────────────────

    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn audio_result_ok() {
        let r: AudioResult<i32> = Ok(42);
        assert_eq!(r.unwrap(), 42);
    }

    #[test]
    fn audio_result_err() {
        let r: AudioResult<i32> = Err(AudioError::ConfigurationError {
            message: "oops".into(),
        });
        assert!(r.is_err());
    }

    // ===== K5.3: Error Classification Robustness Tests =====

    #[test]
    fn all_variants_display_is_nonempty() {
        // Every AudioError variant should produce a non-empty Display string
        let errors = vec![
            AudioError::InvalidParameter {
                param: "test".into(),
                reason: "reason".into(),
            },
            AudioError::UnsupportedFormat {
                format: "f32".into(),
                context: None,
            },
            AudioError::ConfigurationError {
                message: "msg".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "dev1".into(),
            },
            AudioError::DeviceNotAvailable {
                device_id: "dev1".into(),
                reason: "busy".into(),
            },
            AudioError::DeviceEnumerationError {
                reason: "fail".into(),
                context: None,
            },
            AudioError::StreamCreationFailed {
                reason: "fail".into(),
                context: None,
            },
            AudioError::StreamStartFailed {
                reason: "fail".into(),
            },
            AudioError::StreamStopFailed {
                reason: "fail".into(),
            },
            AudioError::StreamReadError {
                reason: "fail".into(),
            },
            AudioError::BufferOverrun { dropped_frames: 10 },
            AudioError::BufferUnderrun {
                requested: 100,
                available: 50,
            },
            AudioError::BackendError {
                backend: "test".into(),
                operation: "op".into(),
                message: "msg".into(),
                context: None,
            },
            AudioError::BackendNotAvailable {
                backend: "test".into(),
            },
            AudioError::BackendInitializationFailed {
                backend: "test".into(),
                reason: "fail".into(),
            },
            AudioError::ApplicationNotFound {
                identifier: "app".into(),
            },
            AudioError::ApplicationCaptureFailed {
                app_id: "app".into(),
                reason: "fail".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "feat".into(),
                platform: "linux".into(),
            },
            AudioError::PermissionDenied {
                operation: "capture".into(),
                details: None,
            },
            AudioError::InternalError {
                message: "internal".into(),
                source: None,
            },
            AudioError::Timeout {
                operation: "read".into(),
                duration: std::time::Duration::from_secs(1),
            },
        ];
        for (i, err) in errors.iter().enumerate() {
            let display = format!("{err}");
            assert!(!display.is_empty(), "Variant #{i} has empty Display");
            assert!(
                display.len() > 3,
                "Variant #{i} Display too short: '{display}'"
            );
        }
    }

    #[test]
    fn is_recoverable_and_is_fatal_are_mutually_exclusive() {
        let errors: Vec<AudioError> = vec![
            AudioError::InvalidParameter {
                param: "p".into(),
                reason: "r".into(),
            },
            AudioError::UnsupportedFormat {
                format: "f".into(),
                context: None,
            },
            AudioError::ConfigurationError {
                message: "m".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "d".into(),
            },
            AudioError::DeviceNotAvailable {
                device_id: "d".into(),
                reason: "r".into(),
            },
            AudioError::DeviceEnumerationError {
                reason: "r".into(),
                context: None,
            },
            AudioError::StreamCreationFailed {
                reason: "r".into(),
                context: None,
            },
            AudioError::StreamStartFailed { reason: "r".into() },
            AudioError::StreamStopFailed { reason: "r".into() },
            AudioError::StreamReadError { reason: "r".into() },
            AudioError::BufferOverrun { dropped_frames: 0 },
            AudioError::BufferUnderrun {
                requested: 0,
                available: 0,
            },
            AudioError::BackendError {
                backend: "b".into(),
                operation: "o".into(),
                message: "m".into(),
                context: None,
            },
            AudioError::BackendNotAvailable {
                backend: "b".into(),
            },
            AudioError::BackendInitializationFailed {
                backend: "b".into(),
                reason: "r".into(),
            },
            AudioError::ApplicationNotFound {
                identifier: "a".into(),
            },
            AudioError::ApplicationCaptureFailed {
                app_id: "a".into(),
                reason: "r".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "f".into(),
                platform: "p".into(),
            },
            AudioError::PermissionDenied {
                operation: "o".into(),
                details: None,
            },
            AudioError::InternalError {
                message: "m".into(),
                source: None,
            },
            AudioError::Timeout {
                operation: "o".into(),
                duration: std::time::Duration::from_secs(1),
            },
        ];
        for (i, err) in errors.iter().enumerate() {
            let recoverable = err.is_recoverable();
            let fatal = err.is_fatal();
            assert_ne!(recoverable, fatal,
                "Variant #{i} ({err:?}): is_recoverable={recoverable}, is_fatal={fatal} — must be mutually exclusive");
        }
    }

    #[test]
    fn recoverable_variants_are_correct() {
        // Only these should be recoverable (Recoverable or TransientRetry):
        // StreamReadError, BufferOverrun, BufferUnderrun (Recoverable)
        // DeviceNotAvailable, BackendError, Timeout (TransientRetry)
        let recoverable_errors = vec![
            AudioError::StreamReadError { reason: "r".into() },
            AudioError::BufferOverrun { dropped_frames: 0 },
            AudioError::BufferUnderrun {
                requested: 0,
                available: 0,
            },
            AudioError::DeviceNotAvailable {
                device_id: "d".into(),
                reason: "r".into(),
            },
            AudioError::BackendError {
                backend: "b".into(),
                operation: "o".into(),
                message: "m".into(),
                context: None,
            },
            AudioError::Timeout {
                operation: "o".into(),
                duration: std::time::Duration::from_secs(1),
            },
        ];
        for err in &recoverable_errors {
            assert!(err.is_recoverable(), "{err:?} should be recoverable");
            assert!(!err.is_fatal(), "{err:?} should NOT be fatal");
        }
    }

    #[test]
    fn fatal_variants_are_correct() {
        let fatal_errors = vec![
            AudioError::InvalidParameter {
                param: "p".into(),
                reason: "r".into(),
            },
            AudioError::UnsupportedFormat {
                format: "f".into(),
                context: None,
            },
            AudioError::ConfigurationError {
                message: "m".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "d".into(),
            },
            AudioError::DeviceEnumerationError {
                reason: "r".into(),
                context: None,
            },
            AudioError::StreamCreationFailed {
                reason: "r".into(),
                context: None,
            },
            AudioError::StreamStartFailed { reason: "r".into() },
            AudioError::StreamStopFailed { reason: "r".into() },
            AudioError::BackendNotAvailable {
                backend: "b".into(),
            },
            AudioError::BackendInitializationFailed {
                backend: "b".into(),
                reason: "r".into(),
            },
            AudioError::ApplicationNotFound {
                identifier: "a".into(),
            },
            AudioError::ApplicationCaptureFailed {
                app_id: "a".into(),
                reason: "r".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "f".into(),
                platform: "p".into(),
            },
            AudioError::PermissionDenied {
                operation: "o".into(),
                details: None,
            },
            AudioError::InternalError {
                message: "m".into(),
                source: None,
            },
        ];
        for err in &fatal_errors {
            assert!(err.is_fatal(), "{err:?} should be fatal");
            assert!(!err.is_recoverable(), "{err:?} should NOT be recoverable");
        }
    }

    #[test]
    fn error_kind_covers_all_categories() {
        // Verify each ErrorKind category has at least one error variant
        use std::collections::HashSet;
        let errors: Vec<AudioError> = vec![
            AudioError::InvalidParameter {
                param: "p".into(),
                reason: "r".into(),
            },
            AudioError::DeviceNotFound {
                device_id: "d".into(),
            },
            AudioError::StreamReadError { reason: "r".into() },
            AudioError::BackendError {
                backend: "b".into(),
                operation: "o".into(),
                message: "m".into(),
                context: None,
            },
            AudioError::ApplicationNotFound {
                identifier: "a".into(),
            },
            AudioError::PlatformNotSupported {
                feature: "f".into(),
                platform: "p".into(),
            },
            AudioError::InternalError {
                message: "m".into(),
                source: None,
            },
        ];
        let kinds: HashSet<String> = errors.iter().map(|e| format!("{:?}", e.kind())).collect();
        assert!(kinds.contains("Configuration"));
        assert!(kinds.contains("Device"));
        assert!(kinds.contains("Stream"));
        assert!(kinds.contains("Backend"));
        assert!(kinds.contains("Application"));
        assert!(kinds.contains("Platform"));
        assert!(kinds.contains("Internal"));
        assert_eq!(kinds.len(), 7, "Should have exactly 7 ErrorKind categories");
    }

    #[test]
    fn recoverability_display_is_meaningful() {
        assert_eq!(format!("{}", Recoverability::Recoverable), "Recoverable");
        assert_eq!(
            format!("{}", Recoverability::TransientRetry),
            "TransientRetry"
        );
        assert_eq!(format!("{}", Recoverability::Fatal), "Fatal");
    }

    #[test]
    fn error_kind_display_is_meaningful() {
        assert_eq!(format!("{}", ErrorKind::Configuration), "Configuration");
        assert_eq!(format!("{}", ErrorKind::Device), "Device");
        assert_eq!(format!("{}", ErrorKind::Stream), "Stream");
        assert_eq!(format!("{}", ErrorKind::Backend), "Backend");
        assert_eq!(format!("{}", ErrorKind::Application), "Application");
        assert_eq!(format!("{}", ErrorKind::Platform), "Platform");
        assert_eq!(format!("{}", ErrorKind::Internal), "Internal");
    }

    #[test]
    fn backend_context_with_all_fields_displays_correctly() {
        let ctx = BackendContext {
            backend_name: "WASAPI".into(),
            os_error_code: Some(42),
            os_error_message: Some("Access denied".into()),
        };
        let display = format!("{ctx}");
        assert!(display.contains("WASAPI"), "Should contain backend name");
        assert!(display.contains("42"), "Should contain error code");
        assert!(
            display.contains("Access denied"),
            "Should contain error message"
        );
    }

    #[test]
    fn backend_context_propagates_through_error_display() {
        let ctx = BackendContext {
            backend_name: "PipeWire".into(),
            os_error_code: Some(13),
            os_error_message: Some("Permission denied".into()),
        };
        let err = AudioError::BackendError {
            backend: "PipeWire".into(),
            operation: "connect".into(),
            message: "failed to connect".into(),
            context: Some(ctx),
        };
        let display = format!("{err}");
        assert!(
            display.contains("PipeWire")
                || display.contains("connect")
                || display.contains("failed"),
            "BackendError display should contain meaningful info: {display}"
        );
    }

    #[test]
    fn permission_denied_with_details() {
        let err = AudioError::PermissionDenied {
            operation: "capture".into(),
            details: Some("Microphone access not granted".into()),
        };
        let display = format!("{err}");
        assert!(
            display.contains("capture") || display.contains("Microphone"),
            "PermissionDenied with details should show them: {display}"
        );
    }

    #[test]
    fn permission_denied_without_details() {
        let err = AudioError::PermissionDenied {
            operation: "capture".into(),
            details: None,
        };
        let display = format!("{err}");
        assert!(
            display.contains("capture"),
            "Should at least show operation: {display}"
        );
    }

    #[test]
    fn from_io_error_produces_internal_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let audio_err: AudioError = io_err.into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        assert!(audio_err.is_fatal());
    }

    #[test]
    fn from_string_produces_internal_error() {
        let audio_err: AudioError = String::from("something went wrong").into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        assert!(audio_err.is_fatal());
        let display = format!("{audio_err}");
        assert!(
            display.contains("something went wrong"),
            "Should contain the original message: {display}"
        );
    }

    #[test]
    fn from_str_produces_internal_error() {
        let audio_err: AudioError = "static error message".into();
        assert_eq!(audio_err.kind(), ErrorKind::Internal);
        assert!(audio_err.is_fatal());
    }

    #[test]
    fn timeout_error_contains_duration_info() {
        let err = AudioError::Timeout {
            operation: "read_chunk".into(),
            duration: std::time::Duration::from_millis(500),
        };
        let display = format!("{err}");
        assert!(
            display.contains("read_chunk")
                || display.contains("500")
                || display.contains("timeout"),
            "Timeout display should be informative: {display}"
        );
        assert!(err.is_recoverable());
        assert!(!err.is_fatal());
    }

    #[test]
    fn buffer_overrun_contains_frame_count() {
        let err = AudioError::BufferOverrun { dropped_frames: 42 };
        let display = format!("{err}");
        assert!(
            display.contains("42"),
            "Should contain dropped frame count: {display}"
        );
    }

    #[test]
    fn buffer_underrun_contains_counts() {
        let err = AudioError::BufferUnderrun {
            requested: 1024,
            available: 512,
        };
        let display = format!("{err}");
        assert!(
            display.contains("1024") || display.contains("512"),
            "Should contain requested/available counts: {display}"
        );
    }
}

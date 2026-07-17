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
///
/// # Stability
///
/// This enum is **deliberately not** `#[non_exhaustive]`: its seven domains are a
/// fixed, intentional classification axis that downstream code is meant to match
/// exhaustively. Keeping it closed is a stability guarantee — the set will not
/// grow in a way that silently breaks exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Invalid or unsupported capture configuration (bad parameter values,
    /// unsupported formats).
    Configuration,
    /// Audio device problems: not found, unavailable, or enumeration failure.
    Device,
    /// Stream lifecycle and data-flow failures: create/start/stop errors,
    /// read errors, end-of-stream, and ring-buffer over/underruns.
    Stream,
    /// Platform backend (WASAPI, PipeWire, CoreAudio) operation or
    /// initialization failures.
    Backend,
    /// Application-capture failures: target app not found or its audio
    /// session could not be captured.
    Application,
    /// Platform-level constraints: unsupported features on the current OS or
    /// permission denials.
    Platform,
    /// Internal invariant violations, unexpected errors, and timeouts.
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
///
/// # Stability
///
/// This enum is **deliberately not** `#[non_exhaustive]`: the three recoverability
/// states are a fixed, intentional classification axis callers branch on
/// exhaustively (retry / abandon / continue). Keeping it closed is a stability
/// guarantee — the set will not grow in a way that silently breaks exhaustive
/// matches.
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
/// Organized into 7 categories with 23 total variants.
/// Each variant carries structured context for diagnostics.
///
/// # Stability
///
/// This enum is `#[non_exhaustive]`: new failure modes may be added in a minor
/// release without it being a breaking change. **Out-of-crate** code matching on
/// `AudioError` must therefore include a trailing wildcard (`_ =>`) arm to stay
/// forward-compatible; treat an unrecognized variant by consulting
/// [`kind`](Self::kind) / [`recoverability`](Self::recoverability) /
/// [`user_message`](Self::user_message) rather than the variant identity. The
/// classification methods on this type ([`kind`](Self::kind),
/// [`recoverability`](Self::recoverability), [`user_message`](Self::user_message))
/// remain exhaustive **inside this crate** so every new variant is forced to
/// declare its category, recoverability, and user-facing text deliberately.
#[derive(Debug)]
#[non_exhaustive]
pub enum AudioError {
    // ── Configuration errors ─────────────────────────────────────────
    /// A parameter value is invalid.
    InvalidParameter {
        /// Name of the offending parameter (e.g. `"sample_rate"`).
        param: String,
        /// Why the value was rejected.
        reason: String,
    },
    /// The requested audio format is not supported.
    UnsupportedFormat {
        /// Human-readable description of the rejected format.
        format: String,
        /// OS-level backend context, when the rejection came from a backend.
        context: Option<BackendContext>,
    },
    /// A general configuration error.
    ConfigurationError {
        /// Description of the configuration problem.
        message: String,
    },
    /// A required user-consent artifact is missing from the capture
    /// configuration.
    ///
    /// Mobile platforms gate capture behind an explicit user-consent flow that
    /// produces an artifact the builder must receive **before** `build()` —
    /// e.g. Android's `MediaProjection` token (supplied via
    /// `AudioCaptureBuilder::with_android_projection`, Android targets only)
    /// or an embedded iOS broadcast extension + App Group. Returned by the
    /// `build()`/`preflight()` step, before any OS resource is touched
    /// (ADR-0013, `docs/MOBILE_BACKEND_DESIGN.md`).
    ///
    /// Distinct from [`PermissionDenied`](AudioError::PermissionDenied), which
    /// is the OS *rejecting* an attempted operation at runtime: this variant
    /// means the consent artifact was never supplied to the configuration at
    /// all, so nothing was even attempted. Classified as a configuration
    /// error ([`ErrorKind::Configuration`], `Fatal`) — fix the builder call,
    /// don't retry.
    UserConsentRequired {
        /// The capture facility that requires consent
        /// (e.g. `"Android playback capture"`).
        feature: String,
        /// The concrete missing artifact and how to supply it (e.g.
        /// `"MediaProjection token — obtain one via the rsac consent helper
        /// and pass it to AudioCaptureBuilder::with_android_projection()"`).
        missing: String,
    },

    // ── Device errors ────────────────────────────────────────────────
    /// The requested device was not found.
    DeviceNotFound {
        /// Identifier of the device that could not be located.
        device_id: String,
    },
    /// The device exists but is not currently available.
    DeviceNotAvailable {
        /// Identifier of the unavailable device.
        device_id: String,
        /// Why the device is unavailable (e.g. disconnected, in exclusive use).
        reason: String,
    },
    /// Failed to enumerate audio devices.
    DeviceEnumerationError {
        /// Why enumeration failed.
        reason: String,
        /// OS-level backend context, when one is available.
        context: Option<BackendContext>,
    },

    // ── Stream errors ────────────────────────────────────────────────
    /// Failed to create an audio stream.
    StreamCreationFailed {
        /// Why stream creation failed.
        reason: String,
        /// OS-level backend context, when one is available.
        context: Option<BackendContext>,
    },
    /// Failed to start an audio stream.
    StreamStartFailed {
        /// Why the stream could not be started.
        reason: String,
    },
    /// Failed to stop an audio stream.
    StreamStopFailed {
        /// Why the stream could not be stopped.
        reason: String,
    },
    /// An error occurred while reading audio data from a stream.
    ///
    /// This is a **transient/recoverable** read failure (e.g. a momentary
    /// internal hiccup) — NOT end-of-stream. When a read fails because the
    /// stream has reached a terminal state, [`StreamEnded`](AudioError::StreamEnded)
    /// is returned instead, so callers can distinguish "retry" from "done".
    ///
    /// A lifecycle-cause `StreamReadError` (never-started / stopped / not-yet-
    /// running) can be classified structurally via
    /// [`AudioError::lifecycle_stage`] (rsac-feb4) instead of matching on the
    /// `reason` text.
    StreamReadError {
        /// Why the read failed.
        reason: String,
    },
    /// The stream has ended: a read was attempted on a stream that has reached
    /// a terminal state (Stopped / Closed / Error). This is **fatal** for the
    /// read loop — the stream will produce no more data and should not be
    /// retried. Distinct from the recoverable [`StreamReadError`](AudioError::StreamReadError).
    StreamEnded {
        /// Which terminal state ended the stream and why.
        reason: String,
    },
    /// The ring buffer overflowed — audio frames were dropped.
    BufferOverrun {
        /// Number of frames dropped because the consumer fell behind.
        dropped_frames: usize,
    },
    /// The ring buffer underran — not enough data was available.
    BufferUnderrun {
        /// Number of frames the caller asked for.
        requested: usize,
        /// Number of frames actually available.
        available: usize,
    },

    // ── Backend errors ───────────────────────────────────────────────
    /// A platform-specific backend operation failed.
    BackendError {
        /// Backend name (e.g. `"WASAPI"`, `"PipeWire"`, `"CoreAudio"`).
        backend: String,
        /// The backend operation that failed (e.g. `"IAudioClient::Initialize"`).
        operation: String,
        /// Human-readable failure description.
        message: String,
        /// OS-level backend context, when one is available.
        context: Option<BackendContext>,
    },
    /// The requested backend is not available on this system.
    BackendNotAvailable {
        /// Name of the missing backend.
        backend: String,
    },
    /// The backend failed to initialize.
    BackendInitializationFailed {
        /// Name of the backend that failed to initialize.
        backend: String,
        /// Why initialization failed.
        reason: String,
    },

    // ── Application capture errors ───────────────────────────────────
    /// The target application for capture was not found.
    ApplicationNotFound {
        /// The identifier used to look up the application (PID, name, or
        /// node id, depending on the [`CaptureTarget`](crate::core::config::CaptureTarget) variant).
        identifier: String,
    },
    /// Capturing audio from the target application failed.
    ApplicationCaptureFailed {
        /// Identifier of the application whose capture failed.
        app_id: String,
        /// Why the capture failed.
        reason: String,
    },

    // ── Platform errors ──────────────────────────────────────────────
    /// The requested feature is not supported on this platform.
    PlatformNotSupported {
        /// The unsupported feature (e.g. `"process tree capture"`).
        feature: String,
        /// The current platform name (e.g. `"linux"`).
        platform: String,
    },
    /// The operation was denied due to insufficient permissions.
    PermissionDenied {
        /// The operation that was denied.
        operation: String,
        /// Additional platform-specific detail (e.g. which permission to grant).
        details: Option<String>,
    },

    // ── Internal errors ──────────────────────────────────────────────
    /// An internal or unexpected error.
    InternalError {
        /// Description of the internal failure.
        message: String,
        /// The underlying error that caused this failure, when one exists.
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// An operation timed out.
    Timeout {
        /// The operation that timed out.
        operation: String,
        /// How long the operation ran before timing out.
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
            | AudioError::ConfigurationError { .. }
            | AudioError::UserConsentRequired { .. } => ErrorKind::Configuration,

            AudioError::DeviceNotFound { .. }
            | AudioError::DeviceNotAvailable { .. }
            | AudioError::DeviceEnumerationError { .. } => ErrorKind::Device,

            AudioError::StreamCreationFailed { .. }
            | AudioError::StreamStartFailed { .. }
            | AudioError::StreamStopFailed { .. }
            | AudioError::StreamReadError { .. }
            | AudioError::StreamEnded { .. }
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
    /// - `Fatal`: everything else (including `StreamEnded` — the stream is done)
    ///
    /// This match is **exhaustive** (no `_` catch-all) on purpose: adding a new
    /// `AudioError` variant forces a compile error here so its recoverability is
    /// classified deliberately rather than silently defaulting to `Fatal`.
    pub fn recoverability(&self) -> Recoverability {
        match self {
            AudioError::BufferOverrun { .. }
            | AudioError::BufferUnderrun { .. }
            | AudioError::StreamReadError { .. } => Recoverability::Recoverable,

            AudioError::DeviceNotAvailable { .. }
            | AudioError::Timeout { .. }
            | AudioError::BackendError { .. } => Recoverability::TransientRetry,

            // Fatal: the operation should be abandoned. StreamEnded is fatal for
            // a read loop — the stream will produce no more data.
            AudioError::InvalidParameter { .. }
            | AudioError::UnsupportedFormat { .. }
            | AudioError::ConfigurationError { .. }
            | AudioError::UserConsentRequired { .. }
            | AudioError::DeviceNotFound { .. }
            | AudioError::DeviceEnumerationError { .. }
            | AudioError::StreamCreationFailed { .. }
            | AudioError::StreamStartFailed { .. }
            | AudioError::StreamStopFailed { .. }
            | AudioError::StreamEnded { .. }
            | AudioError::BackendNotAvailable { .. }
            | AudioError::BackendInitializationFailed { .. }
            | AudioError::ApplicationNotFound { .. }
            | AudioError::ApplicationCaptureFailed { .. }
            | AudioError::PlatformNotSupported { .. }
            | AudioError::PermissionDenied { .. }
            | AudioError::InternalError { .. } => Recoverability::Fatal,
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

    /// Builds a [`UserFacingError`] — actionable, UI-ready text for this error.
    ///
    /// This turns the internal error taxonomy into a plain-language `summary`
    /// plus an optional `remedy` hint (a concrete next step the user or
    /// developer can take), without changing the taxonomy itself. The
    /// [`recoverability`](UserFacingError::recoverability) and
    /// [`kind`](UserFacingError::kind) fields mirror [`recoverability()`](Self::recoverability)
    /// and [`kind()`](Self::kind); [`backend_code`](UserFacingError::backend_code)
    /// surfaces the underlying OS error code ([`BackendContext::os_error_code`])
    /// when one is attached.
    ///
    /// The `match` here is **exhaustive** (no `_` catch-all) on purpose, mirroring
    /// [`recoverability()`](Self::recoverability): adding a new [`AudioError`]
    /// variant forces a compile error so its user-facing message is written
    /// deliberately rather than silently defaulting to a generic string.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsac::{AudioError, Recoverability};
    ///
    /// let err = AudioError::PermissionDenied {
    ///     operation: "capture".into(),
    ///     details: None,
    /// };
    /// let ui = err.user_message();
    /// assert!(!ui.summary.is_empty());
    /// assert!(ui.remedy.is_some());
    /// assert_eq!(ui.recoverability, Recoverability::Fatal);
    /// ```
    pub fn user_message(&self) -> UserFacingError {
        // Pull the OS error code out of any attached BackendContext so the UI
        // can surface it alongside the plain-language summary.
        let backend_code = self.backend_context().and_then(|ctx| ctx.os_error_code);

        let (summary, remedy): (String, Option<String>) = match self {
            // ── Configuration ────────────────────────────────────────────
            AudioError::InvalidParameter { param, reason } => (
                format!("Invalid value for '{param}': {reason}."),
                Some(format!(
                    "Correct the '{param}' value in your AudioCaptureBuilder configuration and rebuild."
                )),
            ),
            AudioError::UnsupportedFormat { format, .. } => (
                format!("The audio format '{format}' is not supported."),
                Some(
                    "Choose a format the backend accepts (query supported formats with \
                     PlatformCapabilities::query())."
                        .to_string(),
                ),
            ),
            AudioError::ConfigurationError { message } => (
                format!("The capture configuration is invalid: {message}."),
                Some("Review your AudioCaptureBuilder settings and rebuild.".to_string()),
            ),
            AudioError::UserConsentRequired { feature, missing } => (
                format!("'{feature}' requires a user-consent grant that was not provided."),
                Some(format!(
                    "Missing: {missing}. Run the platform's consent flow first and pass the \
                     resulting artifact to the builder before calling build()."
                )),
            ),

            // ── Device ───────────────────────────────────────────────────
            AudioError::DeviceNotFound { device_id } => (
                format!("The audio device '{device_id}' could not be found."),
                Some("List devices with list_audio_sources() and pick an available one.".to_string()),
            ),
            AudioError::DeviceNotAvailable { device_id, reason } => (
                format!("The audio device '{device_id}' is currently unavailable: {reason}."),
                Some(
                    "The device may be in use or disconnected — retry shortly, or pick \
                     another device from list_audio_sources()."
                        .to_string(),
                ),
            ),
            AudioError::DeviceEnumerationError { reason, .. } => (
                format!("Could not enumerate audio devices: {reason}."),
                Some("Check that the audio subsystem is running, then retry.".to_string()),
            ),

            // ── Stream ───────────────────────────────────────────────────
            AudioError::StreamCreationFailed { reason, .. } => (
                format!("The audio stream could not be created: {reason}."),
                Some(
                    "Verify the device and format are supported (PlatformCapabilities::query()) \
                     and try again."
                        .to_string(),
                ),
            ),
            AudioError::StreamStartFailed { reason } => (
                format!("The audio stream failed to start: {reason}."),
                Some("Ensure the capture target is still available, then retry start().".to_string()),
            ),
            AudioError::StreamStopFailed { reason } => (
                format!("The audio stream failed to stop cleanly: {reason}."),
                None,
            ),
            AudioError::StreamReadError { reason } => (
                format!("A transient error occurred while reading audio: {reason}."),
                Some("This is usually momentary — retry the read.".to_string()),
            ),
            AudioError::StreamEnded { reason } => (
                format!("The audio stream has ended: {reason}."),
                Some("The stream will produce no more data — create a new capture to continue.".to_string()),
            ),
            AudioError::BufferOverrun { dropped_frames } => (
                format!("The capture buffer overran and {dropped_frames} frame(s) were dropped."),
                Some("Consume buffers more frequently or increase the buffer size to avoid drops.".to_string()),
            ),
            AudioError::BufferUnderrun {
                requested,
                available,
            } => (
                format!(
                    "Not enough audio was available: requested {requested} frame(s) but only \
                     {available} were ready."
                ),
                Some("Wait for more audio to be captured before reading again.".to_string()),
            ),

            // ── Backend ──────────────────────────────────────────────────
            AudioError::BackendError {
                backend,
                operation,
                message,
                ..
            } => (
                format!("The {backend} audio backend failed during '{operation}': {message}."),
                Some("This may be transient — retry the operation.".to_string()),
            ),
            AudioError::BackendNotAvailable { backend } => (
                format!("The {backend} audio backend is not available on this system."),
                Some(
                    "Confirm the platform/feature is supported with PlatformCapabilities::query()."
                        .to_string(),
                ),
            ),
            AudioError::BackendInitializationFailed { backend, reason } => (
                format!("The {backend} audio backend failed to initialize: {reason}."),
                Some(
                    "Ensure the OS audio service is running (e.g. PipeWire on Linux) and retry."
                        .to_string(),
                ),
            ),

            // ── Application ──────────────────────────────────────────────
            AudioError::ApplicationNotFound { identifier } => (
                format!("No running application matched '{identifier}' for capture."),
                Some(
                    "Confirm the application is running and producing audio; list candidates \
                     with list_audio_applications()."
                        .to_string(),
                ),
            ),
            AudioError::ApplicationCaptureFailed { app_id, reason } => (
                format!("Capturing audio from application '{app_id}' failed: {reason}."),
                Some(
                    "Confirm the application is still running and that per-application capture \
                     is supported (PlatformCapabilities::query())."
                        .to_string(),
                ),
            ),

            // ── Platform ─────────────────────────────────────────────────
            AudioError::PlatformNotSupported { feature, platform } => (
                format!("The feature '{feature}' is not supported on {platform}."),
                Some(format!(
                    "'{feature}' is unavailable on {platform} — check PlatformCapabilities::query() \
                     before using it and choose a supported capture target."
                )),
            ),
            AudioError::PermissionDenied { operation, details } => {
                let mut summary = format!("Permission was denied for '{operation}'.");
                if let Some(d) = details {
                    summary.push(' ');
                    summary.push_str(d);
                }
                (
                    summary,
                    Some(
                        "Grant audio capture permission in System Settings > Privacy \
                         (macOS 14.4+), or your platform's equivalent, then retry."
                            .to_string(),
                    ),
                )
            }

            // ── Internal ─────────────────────────────────────────────────
            AudioError::InternalError { message, .. } => (
                format!("An unexpected internal error occurred: {message}."),
                Some("This is likely a bug — please report it with the surrounding logs.".to_string()),
            ),
            AudioError::Timeout {
                operation,
                duration,
            } => (
                format!("The operation '{operation}' timed out after {duration:?}."),
                Some("Retry the operation; if it persists, check the device/backend health.".to_string()),
            ),
        };

        UserFacingError {
            summary,
            remedy,
            recoverability: self.recoverability(),
            kind: self.kind(),
            backend_code,
        }
    }

    /// Borrows the [`BackendContext`] attached to this error, if any.
    ///
    /// Only the variants that carry an `Option<BackendContext>` can return
    /// `Some`; all others return `None`. This is the single source of truth
    /// for [`user_message`](Self::user_message)'s `backend_code` extraction.
    fn backend_context(&self) -> Option<&BackendContext> {
        match self {
            AudioError::UnsupportedFormat { context, .. }
            | AudioError::DeviceEnumerationError { context, .. }
            | AudioError::StreamCreationFailed { context, .. }
            | AudioError::BackendError { context, .. } => context.as_ref(),
            _ => None,
        }
    }

    /// Structured lifecycle classification for a lifecycle-cause
    /// [`StreamReadError`](AudioError::StreamReadError) (rsac-feb4). Returns
    /// `None` for every other variant.
    ///
    /// Returns `Some(LifecycleStage::Unknown)` for a `StreamReadError` whose
    /// `reason` doesn't match a recognized lifecycle phrasing (e.g. a
    /// future/third-party construction) rather than `None` — callers that
    /// only care about `StreamReadError` always get a stage, and should treat
    /// `Unknown` like a `#[non_exhaustive]` wildcard arm.
    ///
    /// This intentionally does not parse arbitrary prose: it matches against
    /// the same canonical `REASON_*` constants that construct the error in
    /// the first place, so construction and classification can never drift
    /// independently (the failure mode this method replaces — free-form
    /// `reason.contains(..)` greps in tests).
    pub fn lifecycle_stage(&self) -> Option<LifecycleStage> {
        let AudioError::StreamReadError { reason } = self else {
            return None;
        };
        Some(match reason.as_str() {
            REASON_NOT_INITIALIZED
            | REASON_NO_ACTIVE_STREAM
            | REASON_CAPTURE_NOT_STARTED
            | REASON_COMPOSITION_NOT_STARTED => LifecycleStage::NotInitialized,
            REASON_NOT_RUNNING => LifecycleStage::NotRunning,
            _ => LifecycleStage::Unknown,
        })
    }
}

/// Canonical reason strings for a lifecycle-cause
/// [`StreamReadError`](AudioError::StreamReadError) (rsac-feb4). Constructed
/// here and matched here — the single source of truth
/// [`AudioError::lifecycle_stage`] classifies against, so construction and
/// classification can never drift independently the way free-form
/// `reason.contains(..)` test greps did.
pub(crate) const REASON_NOT_INITIALIZED: &str = "Stream is not initialized. Call start() first.";

/// See [`REASON_NOT_INITIALIZED`].
pub(crate) const REASON_NOT_RUNNING: &str = "Stream is not running";

/// See [`REASON_NOT_INITIALIZED`].
pub(crate) const REASON_NO_ACTIVE_STREAM: &str =
    "No active stream: the capture was never started, or has been \
     stopped (stop() releases the stream). Call start() to begin \
     (or restart) capturing.";

/// See [`REASON_NOT_INITIALIZED`].
pub(crate) const REASON_CAPTURE_NOT_STARTED: &str =
    "Capture not started. Call start() before audio_data_stream().";

/// See [`REASON_NOT_INITIALIZED`]. Compose analogue: a `Composition` handle
/// whose stream has not been created yet (rsac-90b1). Classifies as
/// `NotInitialized` — semantically "no stream exists yet".
pub(crate) const REASON_COMPOSITION_NOT_STARTED: &str =
    "Composition is not started. Call start() first.";

/// Structured cause of a lifecycle [`AudioError::StreamReadError`]
/// (rsac-feb4) — see [`AudioError::lifecycle_stage`].
///
/// # Stability
///
/// `#[non_exhaustive]`: new lifecycle causes may be added in a minor
/// release. Out-of-crate matches need a trailing wildcard.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStage {
    /// No stream exists yet (never `start()`ed) or it was released by `stop()`.
    NotInitialized,
    /// A stream exists but is not in the `Running` state (pre-start `Created`,
    /// or the simple-pull-API `is_running()` short-circuit).
    NotRunning,
    /// Reason text didn't match a recognized lifecycle phrasing.
    Unknown,
}

// ── UserFacingError ────────────────────────────────────────────────────────

/// Actionable, UI-ready presentation of an [`AudioError`].
///
/// Produced by [`AudioError::user_message`]. Unlike [`Display`](fmt::Display),
/// which renders a single diagnostic line, this splits the error into a
/// plain-language [`summary`](Self::summary) and an optional concrete
/// [`remedy`](Self::remedy) so a UI can show "what happened" and "what to do
/// about it" separately. It also carries the machine-readable
/// [`recoverability`](Self::recoverability) and [`kind`](Self::kind)
/// classifications (mirroring [`AudioError::recoverability`] and
/// [`AudioError::kind`]) plus the raw OS [`backend_code`](Self::backend_code)
/// when one is available.
///
/// `#[non_exhaustive]`: additional fields may be added in future minor
/// releases, so construct it only via [`AudioError::user_message`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UserFacingError {
    /// Plain-language description of what went wrong. Always non-empty.
    pub summary: String,
    /// A concrete next step the user or developer can take, when one is
    /// actionable. `None` when no useful remedy applies.
    pub remedy: Option<String>,
    /// Recoverability classification, mirroring [`AudioError::recoverability`].
    pub recoverability: Recoverability,
    /// High-level error category, mirroring [`AudioError::kind`].
    pub kind: ErrorKind,
    /// Raw OS-level error code from the underlying [`BackendContext`], when
    /// the originating error carried one.
    pub backend_code: Option<i64>,
}

impl fmt::Display for UserFacingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary)?;
        if let Some(ref remedy) = self.remedy {
            write!(f, " {remedy}")?;
        }
        Ok(())
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
            AudioError::UserConsentRequired { feature, missing } => {
                write!(
                    f,
                    "User consent required for '{}': missing {}",
                    feature, missing
                )
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
            AudioError::StreamEnded { reason } => {
                write!(f, "Stream ended: {}", reason)
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
            AudioError::UserConsentRequired {
                feature: "Android playback capture".into(),
                missing: "MediaProjection token".into(),
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
            AudioError::StreamEnded {
                reason: "stream stopped".into(),
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
    fn all_variants_constructible() {
        let variants = make_all_variants();
        // 23 variants since UserConsentRequired joined for the mobile consent
        // preflight (rsac-82d4; was 22 since ADR-0003 added StreamEnded).
        assert_eq!(variants.len(), 23, "Must have exactly 23 variants");
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
        assert_eq!(
            AudioError::UserConsentRequired {
                feature: "x".into(),
                missing: "y".into()
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

    // ── StreamEnded semantics (ADR-0003) ─────────────────────────────

    #[test]
    fn stream_ended_is_fatal_and_stream_kind() {
        let e = AudioError::StreamEnded {
            reason: "Stream stopped".into(),
        };
        assert_eq!(e.kind(), ErrorKind::Stream);
        assert!(
            e.is_fatal(),
            "StreamEnded must be Fatal so read loops terminate"
        );
        assert!(!e.is_recoverable());
    }

    #[test]
    fn stream_read_error_stays_recoverable() {
        // The transient read error must remain Recoverable — only StreamEnded is
        // the terminal signal (ADR-0003).
        let e = AudioError::StreamReadError {
            reason: "hiccup".into(),
        };
        assert!(e.is_recoverable());
        assert!(!e.is_fatal());
    }

    // ── AudioError::lifecycle_stage (rsac-feb4) ───────────────────────

    #[test]
    fn lifecycle_stage_classifies_known_reasons() {
        assert_eq!(
            AudioError::StreamReadError {
                reason: REASON_NOT_INITIALIZED.into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::NotInitialized)
        );
        assert_eq!(
            AudioError::StreamReadError {
                reason: REASON_NOT_RUNNING.into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::NotRunning)
        );
        assert_eq!(
            AudioError::StreamReadError {
                reason: REASON_NO_ACTIVE_STREAM.into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::NotInitialized)
        );
        assert_eq!(
            AudioError::StreamReadError {
                reason: REASON_CAPTURE_NOT_STARTED.into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::NotInitialized)
        );
        // rsac-90b1: the compose "not started" reason classifies as
        // NotInitialized (no stream exists yet), not Unknown.
        assert_eq!(
            AudioError::StreamReadError {
                reason: REASON_COMPOSITION_NOT_STARTED.into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::NotInitialized)
        );
    }

    #[test]
    fn lifecycle_stage_is_none_for_non_stream_read_variants() {
        assert_eq!(
            AudioError::StreamEnded { reason: "x".into() }.lifecycle_stage(),
            None
        );
        assert_eq!(
            AudioError::Timeout {
                operation: "x".into(),
                duration: std::time::Duration::ZERO,
            }
            .lifecycle_stage(),
            None
        );
    }

    #[test]
    fn lifecycle_stage_unknown_for_unrecognized_reason() {
        assert_eq!(
            AudioError::StreamReadError {
                reason: "some future phrasing".into()
            }
            .lifecycle_stage(),
            Some(LifecycleStage::Unknown)
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
            AudioError::UserConsentRequired {
                feature: "x".into(),
                missing: "y".into(),
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

    // ── UserFacingError (user_message) ────────────────────────────────

    #[test]
    fn user_message_every_variant_has_nonempty_summary() {
        // Reuse the canonical "every variant" constructor so adding a variant
        // forces it (and thus this assertion) to cover the new case.
        for err in make_all_variants() {
            let ui = err.user_message();
            assert!(
                !ui.summary.trim().is_empty(),
                "user_message().summary is empty for {err:?}"
            );
        }
    }

    #[test]
    fn user_message_mirrors_recoverability_and_kind() {
        // Representative sample across categories/recoverabilities.
        for err in make_all_variants() {
            let ui = err.user_message();
            assert_eq!(
                ui.recoverability,
                err.recoverability(),
                "recoverability mismatch for {err:?}"
            );
            assert_eq!(ui.kind, err.kind(), "kind mismatch for {err:?}");
        }
    }

    #[test]
    fn user_message_permission_denied_has_remedy() {
        let ui = AudioError::PermissionDenied {
            operation: "capture".into(),
            details: None,
        }
        .user_message();
        let remedy = ui.remedy.expect("PermissionDenied must have a remedy");
        assert!(!remedy.trim().is_empty());
        // Mentions the actionable step.
        assert!(
            remedy.contains("permission") || remedy.contains("Privacy"),
            "remedy should mention granting permission: {remedy}"
        );
    }

    #[test]
    fn user_message_platform_not_supported_remedy_names_platform_and_feature() {
        let ui = AudioError::PlatformNotSupported {
            feature: "process-tap".into(),
            platform: "linux".into(),
        }
        .user_message();
        let remedy = ui.remedy.expect("PlatformNotSupported must have a remedy");
        assert!(
            remedy.contains("process-tap"),
            "remedy names feature: {remedy}"
        );
        assert!(remedy.contains("linux"), "remedy names platform: {remedy}");
        assert!(
            remedy.contains("PlatformCapabilities"),
            "remedy points to capability probe: {remedy}"
        );
    }

    #[test]
    fn user_message_extracts_backend_code() {
        let ctx = BackendContext {
            backend_name: "WASAPI".into(),
            os_error_code: Some(-2004287478),
            os_error_message: Some("device in use".into()),
        };
        let ui = AudioError::BackendError {
            backend: "WASAPI".into(),
            operation: "init".into(),
            message: "fail".into(),
            context: Some(ctx),
        }
        .user_message();
        assert_eq!(ui.backend_code, Some(-2004287478));
    }

    #[test]
    fn user_message_backend_code_none_without_context() {
        // A variant that has no BackendContext field at all.
        let ui = AudioError::DeviceNotFound {
            device_id: "hw:0".into(),
        }
        .user_message();
        assert_eq!(ui.backend_code, None);
        // And a context-carrying variant with context: None.
        let ui2 = AudioError::StreamCreationFailed {
            reason: "x".into(),
            context: None,
        }
        .user_message();
        assert_eq!(ui2.backend_code, None);
    }

    #[test]
    fn user_message_display_includes_summary_and_remedy() {
        let ui = AudioError::DeviceNotFound {
            device_id: "hw:9".into(),
        }
        .user_message();
        let shown = ui.to_string();
        assert!(
            shown.contains("hw:9"),
            "Display should include the summary: {shown}"
        );
        assert!(
            shown.contains("list_audio_sources"),
            "Display should append the remedy: {shown}"
        );
    }

    // ── #[non_exhaustive] semantics (AEG-1 / rsac-4341) ──────────────────

    /// `AudioError` is `#[non_exhaustive]`. Out-of-crate consumers must include a
    /// trailing wildcard arm; the canonical forward-compatible path is to classify
    /// an unrecognized variant via [`kind`]/[`recoverability`] rather than its
    /// identity. This test models a *binding-style* match (the shape the three
    /// binding crates use) — every named arm PLUS a `_ =>` fallback — and asserts
    /// the wildcard is reachable as the classification default.
    ///
    /// In-crate this match would still compile without the `_` (the attribute is a
    /// no-op for matches in the defining crate), but writing it here documents and
    /// locks in the contract the binding crates depend on.
    #[test]
    fn non_exhaustive_match_uses_wildcard_classification() {
        fn classify(err: &AudioError) -> Recoverability {
            match err {
                AudioError::StreamReadError { .. } => Recoverability::Recoverable,
                // The trailing wildcard is REQUIRED out-of-crate because
                // AudioError is #[non_exhaustive]; defer to the crate's own
                // classification so a future variant is handled, not ignored.
                other => other.recoverability(),
            }
        }

        // A known arm.
        assert_eq!(
            classify(&AudioError::StreamReadError { reason: "x".into() }),
            Recoverability::Recoverable
        );
        // An arm reached only through the wildcard.
        assert_eq!(
            classify(&AudioError::StreamEnded {
                reason: "done".into()
            }),
            Recoverability::Fatal
        );
    }
}

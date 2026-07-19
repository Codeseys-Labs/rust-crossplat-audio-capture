// Plugin error type (ADR-0014 §4). Every command returns `Result<T, Error>`;
// no panics (repo rule). `AudioError` is flattened to a structured JS object
// `{ kind, recoverability, message }` reusing rsac's existing
// `ErrorKind`/`Recoverability` classification (every `AudioError` carries both).

use serde::{ser::SerializeStruct, Serialize, Serializer};

/// Command-boundary result alias: every plugin command returns
/// `Result<T>` with the structured [`Error`] payload.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced across the plugin command boundary.
///
/// `AudioError` is boxed: it is a large enum (~136 bytes), and every command
/// returns `Result<T, Error>`, so an unboxed variant would bloat every
/// `Result` on the hot return path (clippy `result_large_err`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An rsac capture/introspection error, carrying its kind + recoverability.
    #[error("{0}")]
    Audio(Box<rsac::AudioError>),

    /// A misuse of the plugin API itself (unknown capture id, bad enum string).
    #[error("{0}")]
    Plugin(String),

    /// Error returned by the native mobile plugin (Kotlin/Swift) via
    /// `run_mobile_plugin`. Only constructible on mobile targets.
    #[cfg(mobile)]
    #[error(transparent)]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
}

impl From<rsac::AudioError> for Error {
    fn from(e: rsac::AudioError) -> Self {
        Error::Audio(Box::new(e))
    }
}

impl Error {
    /// Short machine-readable kind slug.
    fn kind(&self) -> &'static str {
        match self {
            Error::Audio(e) => match e.kind() {
                rsac::ErrorKind::Configuration => "configuration",
                rsac::ErrorKind::Device => "device",
                rsac::ErrorKind::Stream => "stream",
                rsac::ErrorKind::Backend => "backend",
                rsac::ErrorKind::Application => "application",
                rsac::ErrorKind::Platform => "platform",
                rsac::ErrorKind::Internal => "internal",
            },
            Error::Plugin(_) => "plugin",
            #[cfg(mobile)]
            Error::PluginInvoke(_) => "plugin",
        }
    }

    /// Recoverability classification: `recoverable`, `transient`, or `fatal`.
    fn recoverability(&self) -> &'static str {
        match self {
            Error::Audio(e) => match e.recoverability() {
                rsac::Recoverability::Recoverable => "recoverable",
                rsac::Recoverability::TransientRetry => "transient",
                rsac::Recoverability::Fatal => "fatal",
            },
            // A plugin-misuse or mobile-invoke error is not a transient audio
            // condition — classify as fatal so a JS caller does not blind-retry.
            Error::Plugin(_) => "fatal",
            #[cfg(mobile)]
            Error::PluginInvoke(_) => "fatal",
        }
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_struct("Error", 3)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("recoverability", self.recoverability())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

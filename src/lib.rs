//! Cross-platform Audio Capture Library
//!
//! This library provides functionality for capturing audio from specific applications
//! across different platforms (Windows, Linux, and macOS).

pub mod audio;

/// Error type for the library
pub type Error = color_eyre::Report;
/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

/// Initialize the library
pub fn init() -> Result<()> {
    color_eyre::install()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_library_initialization() {
        assert!(init().is_ok());
    }
}

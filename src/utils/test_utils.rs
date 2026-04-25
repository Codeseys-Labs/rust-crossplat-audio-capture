//! Placeholder test-utility surface.
//!
//! The full test-utility module (audio generation, mock streams, validation
//! helpers) was removed during the architectural refactor because its old
//! `AudioConfig` dependency no longer exists. Nothing in-tree currently
//! consumes these helpers — integration tests live in `tests/ci_audio/` and
//! use their own helpers, and the bindings crates import the real public
//! API.
//!
//! The two functions below are deliberately trivial stubs retained only so
//! downstream code that imports `rsac::utils::test_utils::*` behind the
//! `test-utils` feature still type-checks. They produce no audio and make no
//! real assertions. If you need mock producers in your own tests, use
//! [`crate::bridge::mock::MockDeviceEnumerator`] instead, which drives the
//! real ring-buffer bridge with a synthetic 440 Hz sine producer.

pub mod generation {
    /// Placeholder for audio generation utilities
    pub fn create_sine_wave(_frequency: f32, _duration_ms: u32, _sample_rate: u32) -> Vec<f32> {
        vec![]
    }
}

pub mod validation {
    /// Placeholder for audio validation utilities
    pub fn validate_audio_data(_data: &[f32]) -> bool {
        true
    }
}

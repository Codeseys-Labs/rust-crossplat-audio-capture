// src/core/mod.rs

//! The `core` module provides the central traits, types, and interfaces
//! for the audio capture library.
//!
//! It defines platform-agnostic abstractions for audio devices, streams,
//! formats, and error handling, allowing backend implementations to plug
//! into a common framework.

pub mod buffer;
pub mod config;
pub mod error;
pub mod interface;
pub mod processing; // Added processing module

pub use buffer::AudioBuffer; // Changed from VecAudioBuffer to the new AudioBuffer struct
pub use error::ProcessError; // Added ProcessError export
pub use processing::AudioProcessor; // Added AudioProcessor export

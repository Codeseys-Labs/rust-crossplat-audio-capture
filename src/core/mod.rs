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

pub use buffer::VecAudioBuffer;

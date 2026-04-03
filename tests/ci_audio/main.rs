//! CI Audio Integration Tests for rsac
//!
//! These tests validate the rsac library's audio capture functionality
//! in CI environments with real audio infrastructure (PipeWire on Linux).
//!
//! Tests gracefully skip when audio infrastructure is not available,
//! so they pass on machines without audio hardware.

#[macro_use]
mod helpers;

mod app_capture;
mod device_capture;
mod device_enumeration;
mod platform_caps;
mod process_tree_capture;
mod stream_lifecycle;
mod system_capture;

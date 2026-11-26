//! Windows audio backend: WASAPI loopback + Process Loopback.
//!
//! The Windows backend uses WASAPI's event-driven shared-mode capture for
//! device/system capture, and the Windows 10 21H1+ Process Loopback API
//! (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`) for per-application and
//! per-process-tree capture. The capture worker initialises COM as MTA on its
//! dedicated thread with a [`ComInitializer`] RAII guard, and audio data
//! crosses from that thread into the consumer thread through the common
//! `BridgeStream` ring-buffer adapter (`crate::bridge::stream`; `pub(crate)`,
//! so referenced by name to keep the `--all-features` docs build clean).
//!
//! ## COM apartments
//!
//! The control-plane handles hold no COM interface pointer.
//! [`WindowsDeviceEnumerator`] and [`WindowsAudioDevice`] keep plain data and
//! an MTA usage pin; each call creates, uses and releases its COM objects on
//! the calling thread, so they work on a thread in an STA, in the MTA, or with
//! COM never initialised, and may be moved, shared and dropped across threads.
//! [`enumerate_application_audio_sessions`] works the same way. The device
//! watcher's COM objects live and die on its own MTA helper thread
//! (`rsac-wasapi-device-watch`). ADR-0020
//! (`docs/designs/0020-windows-watcher-com-ownership.md`) records this model.
//!
//! ## Capture strategy
//!
//! - `SystemDefault` / `Device(DeviceId)` — WASAPI shared-mode loopback on
//!   the selected render endpoint.
//! - `Application(ApplicationId)` / `ApplicationByName` — WASAPI Process
//!   Loopback, keyed by the resolved PID (sysinfo-based lookup for the
//!   name case).
//! - `ProcessTree(ProcessId)` — WASAPI Process Loopback with
//!   `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`.
//!
//! ## Platform requirements
//!
//! Windows 10 build 19043 (21H1) or newer for Process Loopback; older
//! builds support only system/device capture. No extra system packages —
//! WASAPI is part of the OS.

#[cfg(test)]
mod apartment_tests;
pub(crate) mod thread;
pub mod wasapi;

// Re-export public types from wasapi module
pub use wasapi::{ComInitializer, WindowsAudioDevice, WindowsDeviceEnumerator};

// Re-export application session types from wasapi (canonical definitions)
pub use wasapi::enumerate_application_audio_sessions;
pub use wasapi::ApplicationAudioSessionInfo;

// Note: WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream are
// imported directly via `super::thread::*` in wasapi.rs, not through this re-export.

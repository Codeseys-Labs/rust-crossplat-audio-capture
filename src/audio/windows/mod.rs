//! Windows audio backend: WASAPI loopback + Process Loopback.
//!
//! The Windows backend uses WASAPI's event-driven shared-mode capture for
//! device/system capture, and the Windows 10 21H1+ Process Loopback API
//! (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`) for per-application and
//! per-process-tree capture. COM is initialized on a dedicated MTA thread
//! by a [`ComInitializer`] RAII guard, and audio data crosses from that
//! thread into the consumer thread through the common
//! [`BridgeStream`](crate::bridge::stream::BridgeStream) ring-buffer
//! adapter.
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

pub(crate) mod thread;
pub mod wasapi;

// Re-export public types from wasapi module
pub use wasapi::{
    ComInitializer, WindowsApplicationCapture, WindowsAudioDevice, WindowsDeviceEnumerator,
};

// Re-export application session types from wasapi (canonical definitions)
pub use wasapi::enumerate_application_audio_sessions;
pub use wasapi::ApplicationAudioSessionInfo;

// Note: WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream are
// imported directly via `super::thread::*` in wasapi.rs, not through this re-export.

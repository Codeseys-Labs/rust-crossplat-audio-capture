//! Audio capture manager — wraps rsac for multi-source audio capture.
//!
//! Responsibilities:
//! - Enumerate audio devices and applications via rsac
//! - Start/stop capture sessions
//! - Tag audio buffers with source ID and wall-clock time
//! - Forward tagged buffers to the processing pipeline via crossbeam channel

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::state::AudioSourceInfo;

/// Handle to a running audio capture thread.
#[allow(dead_code)]
struct CaptureHandle {
    thread: Option<JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    source_info: AudioSourceInfo,
}

/// Manages multiple concurrent audio capture sources.
#[allow(dead_code)]
pub struct AudioCaptureManager {
    sources: HashMap<String, CaptureHandle>,
    // TODO: Add pipeline_tx: crossbeam_channel::Sender<TaggedAudioBuffer>
}

impl AudioCaptureManager {
    /// Create a new capture manager.
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
        }
    }

    /// List available audio sources (devices + running applications).
    pub fn list_sources(&self) -> Vec<AudioSourceInfo> {
        // TODO: Use rsac to enumerate devices
        // let enumerator = rsac::get_device_enumerator().ok();
        // let devices = enumerator.map(|e| e.enumerate_devices()).unwrap_or_default();
        log::info!("Listing audio sources (stub)");
        vec![]
    }

    /// Start capturing audio from the specified source.
    pub fn start_capture(&mut self, _source_id: &str) -> Result<(), String> {
        // TODO: Create AudioCaptureBuilder with the correct CaptureTarget
        // TODO: Spawn a capture thread that reads audio and sends to pipeline
        log::info!("start_capture stub");
        Ok(())
    }

    /// Stop capturing audio from the specified source.
    pub fn stop_capture(&mut self, _source_id: &str) -> Result<(), String> {
        // TODO: Signal the capture thread to stop
        // TODO: Remove from active sources map
        log::info!("stop_capture stub");
        Ok(())
    }
}

impl Default for AudioCaptureManager {
    fn default() -> Self {
        Self::new()
    }
}

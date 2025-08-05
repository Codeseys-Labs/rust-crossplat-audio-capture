//! Linux-specific audio capture backend using PipeWire.
//!
//! ## Error Handling
//! Errors originating from the `pipewire` crate or related SPA (Simple Plugin API)
//! operations are generally mapped to `AudioError::BackendError(String)`.
//! The string payload provides context about the failed PipeWire operation
//! and includes the original error message for detailed diagnostics.
#![cfg(target_os = "linux")]
/// Represents the detected status of PipeWire on the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipewireStatus {
    /// PipeWire is active, and `pipewire-pulse` (or equivalent) is managing PulseAudio clients.
    ActiveAndPrimary,
    /// Only the core PipeWire daemon is detected; `pipewire-pulse` might not be active.
    OnlyPipeWireCore,
    /// PipeWire does not appear to be available or active.
    NotAvailable,
}

/// Checks the availability and status of PipeWire on the system.
///
/// This function uses a combination of environment variable checks and socket file
/// existence to determine if PipeWire is running and if it's managing PulseAudio
/// clients (via `pipewire-pulse` or a similar mechanism).
///
/// Detection Logic:
/// 1. Environment Variables:
///    - `PIPEWIRE_RUNTIME_DIR`: Its presence suggests PipeWire is active.
///    - `PULSE_SERVER`: If set and points to a PipeWire socket, indicates `pipewire-pulse` is active.
/// 2. Socket Files:
///    - Main PipeWire socket (e.g., `$XDG_RUNTIME_DIR/pipewire-0`): Existence indicates PipeWire core is running.
///    - PipeWire PulseAudio emulation socket (e.g., `$XDG_RUNTIME_DIR/pulse/native`): Existence indicates `pipewire-pulse` is active.
///
/// The function prioritizes `ActiveAndPrimary` if `pipewire-pulse` indicators are found.
/// If only core PipeWire indicators are found, it returns `OnlyPipeWireCore`.
/// Otherwise, it returns `NotAvailable`.
///
/// The function is designed to be robust and avoid panicking, returning `NotAvailable`
/// on errors like permission issues when checking paths (though these should be rare
/// for standard runtime directories).
pub fn check_pipewire_availability() -> PipewireStatus {
    let xdg_runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    let pipewire_runtime_dir_env = std::env::var("PIPEWIRE_RUNTIME_DIR").ok();

    let mut pipewire_core_present = false;
    let mut pipewire_pulse_present = false;

    // Check PIPEWIRE_RUNTIME_DIR environment variable
    if pipewire_runtime_dir_env.is_some() {
        pipewire_core_present = true;
    }

    // Check PULSE_SERVER environment variable
    if let Ok(pulse_server_var) = std::env::var("PULSE_SERVER") {
        if pulse_server_var.contains("pipewire") {
            pipewire_pulse_present = true;
            pipewire_core_present = true; // pipewire-pulse implies pipewire is also running
        }
    }

    // Check for PipeWire sockets
    let check_socket = |base_dir: &Option<String>, path_suffix: &str| -> bool {
        base_dir.as_ref().map_or(false, |dir| {
            let socket_path = std::path::Path::new(dir).join(path_suffix);
            socket_path.exists()
        })
    };

    // Check main PipeWire socket (e.g., pipewire-0)
    if !pipewire_core_present {
        // Only check if not already confirmed by env var
        if check_socket(&xdg_runtime_dir, "pipewire-0")
            || check_socket(&pipewire_runtime_dir_env, "pipewire-0")
        {
            pipewire_core_present = true;
        }
    }

    // Check PipeWire PulseAudio emulation socket (e.g., pulse/native)
    if !pipewire_pulse_present {
        // Only check if not already confirmed by PULSE_SERVER
        if check_socket(&xdg_runtime_dir, "pulse/native")
            || check_socket(&pipewire_runtime_dir_env, "pulse/native")
        {
            pipewire_pulse_present = true;
            pipewire_core_present = true; // Finding pulse/native implies core is also there
        }
    }

    // Determine status based on findings
    if pipewire_pulse_present {
        PipewireStatus::ActiveAndPrimary
    } else if pipewire_core_present {
        PipewireStatus::OnlyPipeWireCore
    } else {
        // As a last resort, try to check if 'pipewire' process is running.
        // This is a weaker check and might require more permissions or be less reliable.
        // For simplicity and to avoid external crates for this subtask, we'll skip direct process checking.
        // If the user wants more robust process checking, it can be added later with a crate like `sysinfo`.
        PipewireStatus::NotAvailable
    }
}

#[cfg(target_os = "linux")]
pub mod pipewire;

// Re-export items for application-level capture if needed by other modules
#[cfg(target_os = "linux")]
pub use pipewire::{enumerate_audio_applications_pipewire, LinuxApplicationInfo};

use crate::api::AudioCaptureConfig; // AudioCaptureConfig is in api.rs
use crate::core::buffer::AudioBuffer; // This is the new AudioBuffer struct
use crate::core::config::StreamConfig; // StreamConfig is in core::config
use crate::core::error::{AudioError, Result as AudioResult}; // Removed CaptureError as it doesn't exist
use crate::core::interface::{
    AudioBackend,
    AudioDevice,
    CapturingStream,
    DeviceEnumerator,
    DeviceKind, // Added AudioBackend
                // AudioBuffer trait is removed, struct will be imported from crate::core::buffer
};
use crate::{AudioFormat, SampleFormat}; // AudioFormat is re-exported from lib.rs
use log::{debug, error, info, warn}; // Added for logging
use std::sync::Once; // For one-time initialization of PipeWire

// PulseAudio support removed - using PipeWire only
use futures_channel::mpsc; // For MPSC channel
use futures_core::Stream;
use pipewire::{
    keys as pw_keys,
    pod::pod::PodBuilder, // Added PodBuilder
    spa::{
        param::audio::AudioFormat as SpaAudioFormat, // Added SpaAudioFormat
        param::format::{FormatProperties, SpaFormat},
        param::format_utils,
        param::ParamType,
        param::{MediaSubtype, MediaType}, // Added MediaType, MediaSubtype
        pod::{Object, Pod},               // Added Object for type compatibility
        utils::{Direction, SpaChannel},   // Added Direction and SpaChannel
        Id,
    },
    stream::StreamState, // Added StreamState
    types as pw_types,
}; // Added for PipeWire keys and types
use std::collections::VecDeque; // For data_queue
use std::pin::Pin; // For Pin<Box<dyn Stream>>
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering}; // Renamed Ordering to avoid conflict if any
use std::sync::Arc; // Added for Arc
use std::time::Instant; // Added for timestamping // For the Stream trait

// TODO: Remove these once the actual PipeWire logic is integrated with the new traits.
// These are placeholders from the old structure.
use std::{process::Command, sync::Mutex, thread, time::Duration};

// Adjusted imports for PipewireCoreContext
use pipewire::{
    channel,
    context::Context as PwContext, // Import PwContext for old backend compatibility
    core::Core as PwCore,          // Import PwCore for old backend compatibility
    main_loop::MainLoop as PwMainLoop, // Import PwMainLoop for old backend compatibility
    properties::properties,        // This is fine
    registry::Registry,            // This is fine
    spa,
    stream::Listener as StreamListener, // For listener_handle type
    stream::{Stream as PwStream, StreamFlags},
    Context,    // For PipewireCoreContext
    Core,       // For PipewireCoreContext
    MainLoop,   // For PipewireCoreContext
    Properties, // Explicitly import Properties
};
// Removed: use crate::core::buffer::VecAudioBuffer; // Will use AudioBuffer struct directly

use super::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};

// --- Linux Backend Abstraction ---

/// Represents the available audio backends on Linux.
/// Uses PipeWire only.
#[derive(Debug)]
pub enum LinuxAudioBackend {
    PipeWire(Arc<PipewireCoreContext>),
}

/// Represents an active audio capturing stream on Linux, using PipeWire only.
#[derive(Debug)]
pub enum LinuxCapturingStream {
    PipeWire(Box<LinuxAudioStream>), // This is Box<dyn CapturingStream> which is Box<LinuxAudioStream>
}

static PIPEWIRE_INIT: Once = Once::new();

impl LinuxAudioBackend {
    /// Creates a new `LinuxAudioBackend`.
    ///
    /// It attempts to initialize PipeWire. If PipeWire initialization fails
    /// or is not available, an error is returned.
    pub fn new() -> AudioResult<Self> {
        debug!("Attempting to initialize Linux audio backend...");

        match check_pipewire_availability() {
            PipewireStatus::ActiveAndPrimary | PipewireStatus::OnlyPipeWireCore => {
                debug!("PipeWire detected. Attempting to initialize PipeWire backend.");
                // Ensure pipewire::init() is called only once.
                PIPEWIRE_INIT.call_once(|| {
                    pipewire::init();
                    debug!("Global PipeWire initialized.");
                });

                match PipewireCoreContext::new() {
                    Ok(pw_core_ctx) => {
                        info!("PipeWire backend initialized successfully.");
                        return Ok(LinuxAudioBackend::PipeWire(Arc::new(pw_core_ctx)));
                    }
                    Err(e) => {
                        error!("PipeWire backend initialization failed: {:?}", e);
                        return Err(e);
                    }
                }
            }
            PipewireStatus::NotAvailable => {
                error!("PipeWire not available on system.");
                return Err(AudioError::BackendInitializationFailed(
                    "PipeWire is not available on the system.".into(),
                ));
            }
        }
    }
}

impl AudioBackend for LinuxAudioBackend {
    fn create_stream(
        &mut self,
        config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>> {
        match self {
            LinuxAudioBackend::PipeWire(ref core_ctx) => {
                debug!(
                    "LinuxAudioBackend (PipeWire): Creating stream with config: {:?}",
                    config
                );
                // Create a temporary enumerator to get a device.
                // This assumes LinuxDeviceEnumerator doesn't have significant side effects on creation beyond init.
                let enumerator = LinuxDeviceEnumerator {
                    core_context: Arc::clone(core_ctx),
                };

                let mut device_to_use: LinuxAudioDevice = if let Some(id_str) = &config.device_name
                {
                    enumerator.get_device_by_id(&id_str.to_string())?
                } else {
                    enumerator.get_default_device(DeviceKind::Input)?
                };

                // device_to_use is LinuxAudioDevice. Its create_stream method must adhere to AudioDevice trait.
                // AudioDevice::create_stream returns AudioResult<Box<dyn CapturingStream + 'static>>
                // So, device_to_use.create_stream(config) will return the correctly boxed stream.
                let pw_dyn_capturing_stream = device_to_use.create_stream(config)?;
                // pw_dyn_capturing_stream is already Box<dyn CapturingStream + 'static>
                // which in this case is Box<LinuxCapturingStream::PipeWire(...)>
                Ok(pw_dyn_capturing_stream)
            }
        }
    }

    fn default_capture_device(&self) -> AudioResult<Option<Box<dyn AudioDevice + 'static>>> {
        match self {
            LinuxAudioBackend::PipeWire(ref core_ctx) => {
                debug!("LinuxAudioBackend (PipeWire): Getting default capture device.");
                let enumerator = LinuxDeviceEnumerator {
                    core_context: Arc::clone(core_ctx),
                };
                match enumerator.get_default_device(DeviceKind::Input) {
                    // Returns AudioResult<LinuxAudioDevice>
                    Ok(device) => Ok(Some(Box::new(device) as Box<dyn AudioDevice + 'static>)),
                    Err(AudioError::DeviceNotFound) => Ok(None), // Corrected: DeviceNotFound is unit-like
                    Err(e) => Err(e),                            // Propagate other errors
                }
            }
        }
    }

    fn enumerate_capture_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice + 'static>>> {
        match self {
            LinuxAudioBackend::PipeWire(ref core_ctx) => {
                debug!("LinuxAudioBackend (PipeWire): Enumerating capture devices.");
                let enumerator = LinuxDeviceEnumerator {
                    core_context: Arc::clone(core_ctx),
                };
                let devices = enumerator.get_input_devices()?; // get_input_devices now returns AudioResult<Vec<LinuxAudioDevice>>
                Ok(devices
                    .into_iter()
                    .map(|d| Box::new(d) as Box<dyn AudioDevice + 'static>)
                    .collect())
            }
        }
    }
}

impl CapturingStream for LinuxCapturingStream {
    fn start(&mut self) -> AudioResult<()> {
        match self {
            LinuxCapturingStream::PipeWire(ref mut pw_stream) => pw_stream.start(),
        }
    }

    fn stop(&mut self) -> AudioResult<()> {
        match self {
            LinuxCapturingStream::PipeWire(ref mut pw_stream) => pw_stream.stop(),
        }
    }

    fn close(&mut self) -> AudioResult<()> {
        match self {
            LinuxCapturingStream::PipeWire(ref mut pw_stream) => pw_stream.close(),
        }
    }

    fn is_running(&self) -> bool {
        match self {
            LinuxCapturingStream::PipeWire(ref pw_stream) => pw_stream.is_running(),
        }
    }

    fn read_chunk(&mut self, timeout_ms: Option<u32>) -> AudioResult<Option<AudioBuffer>> {
        match self {
            LinuxCapturingStream::PipeWire(ref mut pw_stream) => pw_stream.read_chunk(timeout_ms),
        }
    }

    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + 'a>,
        >,
    > {
        match self {
            LinuxCapturingStream::PipeWire(ref mut pw_stream) => pw_stream.to_async_stream(),
        }
    }
}

// --- New Skeleton Implementations ---

/// Manages the core PipeWire objects like MainLoop, Context, and Core.
/// This struct is responsible for initializing and holding the essential PipeWire state
/// required for device enumeration and stream creation.
#[derive(Debug)] // derive Debug, or implement manually if fields are not Debug
pub(crate) struct PipewireCoreContext {
    // TODO: Manage pipewire::init() and pipewire::deinit() globally,
    // possibly with std::sync::Once when the first context is created.
    // For now, init/deinit are omitted as per subtask instructions.
    // _init_token: pipewire::InitGuard, // If pipewire::init() returns a guard
    main_loop: MainLoop,
    context: Context,
    core: Core,
}

impl PipewireCoreContext {
    /// Creates a new `PipewireCoreContext`.
    /// Initializes the PipeWire main loop, context, and connects to the core.
    pub fn new() -> AudioResult<Self> {
        // pipewire::init(); // Call once globally if not using an InitGuard.
        // Or, if pipewire::init() returns a guard:
        // let _init_token = pipewire::init().map_err(|()| AudioError::BackendError("Failed to initialize global PipeWire state".to_string()))?;

        let main_loop = MainLoop::new(None).map_err(|e| {
            AudioError::BackendError(format!("Failed to create PipeWire MainLoop: {}", e))
        })?;
        let context = Context::new(&main_loop).map_err(|e| {
            AudioError::BackendError(format!("Failed to create PipeWire Context: {}", e))
        })?;
        let core = context.connect(None).map_err(|e| {
            AudioError::BackendError(format!("Failed to connect to PipeWire Core: {}", e))
        })?;

        Ok(Self {
            // _init_token,
            main_loop,
            context,
            core,
        })
    }

    /// Returns a reference to the PipeWire Core.
    pub fn core(&self) -> &Core {
        &self.core
    }

    /// Returns a reference to the PipeWire MainLoop.
    pub fn main_loop(&self) -> &MainLoop {
        &self.main_loop
    }

    /// Returns a reference to the PipeWire Context.
    pub fn context(&self) -> &Context {
        &self.context
    }
}

// impl Drop for PipewireCoreContext {
//     fn drop(&mut self) {
//         // Core, Context, MainLoop should clean up on drop.
//         // If pipewire::init() was called without a guard, call pipewire::deinit();
//         // pipewire::deinit(); // If init was called manually
//     }
// }

/// Device ID for PipeWire devices, represented as a String (typically a node ID).
pub type LinuxPipeWireDeviceId = String;

/// Represents a PipeWire audio device.
/// For subtask 6.2, this is a basic struct holding node information.
/// Full implementation will be in subtask 6.3.
#[derive(Debug, Clone)]
pub(crate) struct LinuxAudioDevice {
    id: u32,                                // PipeWire node ID
    props: Option<Properties>,              // Store node properties
    core_context: Arc<PipewireCoreContext>, // Needed for device operations
}

impl LinuxAudioDevice {
    /// Creates a new LinuxAudioDevice.
    fn new(id: u32, props: Option<Properties>, core_context: Arc<PipewireCoreContext>) -> Self {
        Self {
            id,
            props,
            core_context,
        }
    }

    /// Helper function to get a default audio format if `get_default_format` returns None.
    fn default_audio_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            bits_per_sample: 32,
            sample_format: SampleFormat::F32LE,
        }
    }
}

impl AudioDevice for LinuxAudioDevice {
    type DeviceId = LinuxPipeWireDeviceId; // This is String

    /// Returns a unique identifier for the audio device.
    /// For PipeWire, this is the node ID converted to a String.
    fn get_id(&self) -> Self::DeviceId {
        self.id.to_string()
    }

    /// Returns a human-readable name for the audio device.
    /// Extracts `NODE_DESCRIPTION` or `NODE_NAME` from properties.
    /// Falls back to "Unknown PipeWire Node <id>" if properties or name are missing.
    fn get_name(&self) -> String {
        self.props
            .as_ref()
            .and_then(|p| {
                p.get(pw_keys::NODE_DESCRIPTION)
                    .or_else(|| p.get(pw_keys::NODE_NAME))
            })
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Unknown PipeWire Node {}", self.id))
    }

    /// Returns `true` if the device is an input device (e.g., microphone, system audio monitor).
    /// Inspects `media.class` property. System capture targets monitor *sources*.
    fn is_input(&self) -> bool {
        if let Some(props) = &self.props {
            if let Some(media_class) = props.get(pw_keys::MEDIA_CLASS) {
                return media_class == "Audio/Source/Virtual" || media_class == "Audio/Source";
            }
        }
        // Default to false if props or media_class is missing, or class doesn't match.
        // This is safer than assuming it's an input.
        // log::warn!("Could not determine if PipeWire node {} is input from props: {:?}", self.id, self.props);
        false
    }

    /// Returns `true` if the device is an output device (e.g., speakers).
    /// This enumerator focuses on capture sources, so this typically returns `false`.
    fn is_output(&self) -> bool {
        // Could check for "Audio/Sink" if full device type detection was needed.
        false
    }

    /// Returns `true` if the device is currently active or available.
    /// For this subtask, this is a placeholder. A real implementation would check node state.
    fn is_active(&self) -> bool {
        // TODO: Implement actual check for PipeWire node state.
        // This might involve checking self.core_context.core().get_node_info(self.id)
        // and inspecting its state, or if it's connected to anything.
        // For now, assume active if it exists.
        true // Placeholder
    }

    /// Returns the default audio format for this device.
    /// This is complex with PipeWire's SPA `EnumFormat`.
    /// For this subtask, provides a common default and a `TODO`.
    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        // TODO: Implement actual format negotiation using PipeWire node params (EnumFormat).
        // This would involve:
        // 1. Getting node info: self.core_context.core().get_node_info(self.id)
        // 2. Iterating info.params() for Id::EnumFormat.
        // 3. Parsing the SpaPod for each format.
        // 4. Selecting a preferred default (e.g., highest quality, or a common format).
        Ok(AudioFormat {
            sample_rate: 48000,
            channels: 2,
            bits_per_sample: 32,
            sample_format: SampleFormat::F32LE,
        })
    }

    /// Returns a list of audio formats supported by this device.
    /// Similar to `get_default_format`, this is simplified with a `TODO`.
    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // TODO: Implement full EnumFormat parsing.
        // This would involve iterating all EnumFormat params and converting them.
        let default_fmt = self.get_default_format()?; // No unwrap_or_else needed due to trait change
        Ok(vec![default_fmt])
    }

    /// Checks if the device supports the given audio format.
    /// Simplified for this subtask with a `TODO`.
    fn is_format_supported(&self, format_to_check: &AudioFormat) -> AudioResult<bool> {
        // TODO: Implement actual PipeWire format checking using node EnumFormat params.
        // This involves:
        // 1. Get node info: self.core_context.core().get_node_info(self.id)
        // 2. Iterate info.params() for Id::EnumFormat.
        // 3. For each SpaPod representing a format:
        //    a. Parse it (e.g., using format_utils or manually extracting properties).
        //    b. Compare media_type, media_subtype, format, rate, channels, etc.
        //       with the `_format_to_check` (converted to SPA terms).
        //
        // Example of converting AudioFormat to SPA (conceptual):
        // let spa_media_type = spa::param::MediaType::Audio;
        // let spa_media_subtype = match _format_to_check.sample_format {
        //     SampleFormat::F32LE => spa::param::MediaSubtype::Dsp, // Or Raw if appropriate
        //     // ... other mappings
        //     _ => return Err(AudioError::FormatNotSupported("Unsupported sample format for SPA conversion".to_string())),
        // };
        // let spa_audio_format = match _format_to_check.sample_format {
        //     SampleFormat::F32LE => spa::param::audio::AudioFormat::F32LE,
        //     // ... other mappings
        //     _ => return Err(AudioError::FormatNotSupported("Unsupported sample format for SPA conversion".to_string())),
        // };
        // let spa_rate = _format_to_check.sample_rate;
        // let spa_channels = _format_to_check.channels as u32;

        Ok(true) // Placeholder
    }

    /// Creates an audio stream for capturing data from this device.
    /// Sets up basic PipeWire stream properties.
    fn create_stream(
        &mut self,
        capture_config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>> {
        // Corrected return type to match trait
        debug!("LinuxAudioDevice::create_stream for device ID: {}", self.id);

        let core = self.core_context.core();
        // MainLoop is managed by PipewireCoreContext, stream will use it.

        let stream_props = pipewire::Properties::new()
            .set(pw_keys::MEDIA_TYPE, "Audio")
            .set(pw_keys::MEDIA_CATEGORY, "Capture") // For capture streams
            .set(pw_keys::MEDIA_ROLE, "Music") // Or a more generic role like "Generic" or "System"
            // .set(pw_keys::NODE_ID, &self.id.to_string()) // Target node is set during connect for capture
            // For capturing from a monitor source, we usually don't set STREAM_CAPTURE_SINK.
            // STREAM_CAPTURE_SINK = true is for capturing the output of a *sink* application.
            // If self.id refers to a monitor node directly, this is not needed.
            // If self.id refers to a sink and we want its monitor, then it might be,
            // but device enumeration should give us the monitor node ID directly.
            // .set(pw_keys::STREAM_CAPTURE_SINK, "true") // Only if capturing from a sink's monitor port explicitly
            .set(
                pw_keys::STREAM_WANT_FORMAT,
                match capture_config.stream_config.format.sample_format {
                    SampleFormat::F32LE => "F32LE",
                    SampleFormat::S16LE => "S16LE",
                    // Add other formats if common, otherwise rely on negotiation
                    _ => "F32LE", // Default to F32LE if not specified or uncommon
                },
            ) // Request a common format, negotiation happens later
            .set(
                "audio.channels",
                &capture_config.stream_config.format.channels.to_string(),
            )
            .set(
                "audio.rate",
                &capture_config.stream_config.format.sample_rate.to_string(),
            );

        let stream = PwStream::new_with_properties(
            core,
            &format!("audio-capture-{}", self.id), // Unique stream name
            stream_props,
        )
        .map_err(|e| {
            AudioError::BackendError(format!("Failed to create PipeWire stream: {}", e))
        })?;

        info!("PipeWire stream created for device ID: {}", self.id);
        let concrete_stream = LinuxAudioStream::new(
            stream,
            Arc::clone(&self.core_context),
            capture_config.stream_config.format.clone(),
            self.id,
        );
        // Wrap the concrete LinuxAudioStream in LinuxCapturingStream::PipeWire and then Box it.
        Ok(Box::new(LinuxCapturingStream::PipeWire(Box::new(
            concrete_stream,
        ))))
    }
}

/// Enumerates PipeWire audio devices.
pub struct LinuxDeviceEnumerator {
    core_context: Arc<PipewireCoreContext>, // Use Arc for potential sharing
}

impl LinuxDeviceEnumerator {
    /// Creates a new `LinuxDeviceEnumerator`.
    /// Initializes the PipeWire core context.
    pub(crate) fn new() -> AudioResult<Self> {
        // Initialize pipewire globally once.
        // This should ideally be done using std::sync::Once or similar.
        // For this subtask, we'll call it here directly.
        // A more robust solution would manage this globally.
        // pipewire::init(); // Moved to LinuxAudioBackend::new with Once guard

        let core_context = Arc::new(PipewireCoreContext::new()?);
        Ok(Self { core_context })
    }

    // Removed get_device_by_id_str as it's not needed if get_device_by_id is correctly used.
}

// TODO: Implement Drop for LinuxDeviceEnumerator if pipewire::init() needs a corresponding pipewire::deinit()
// and it's managed here.
// impl Drop for LinuxDeviceEnumerator {
//     fn drop(&mut self) {
//         // TODO: Call pipewire::deinit() if init was called in new() and not managed by a guard.
//         // This depends on the pipewire crate's init/deinit mechanism.
//         // For now, assuming Core/Context/MainLoop handle their cleanup.
//         // pipewire::deinit();
//     }
// }

struct DefaultDeviceSearchState {
    default_sink_name: Option<String>,
    default_sink_id: Option<u32>,
    monitor_device: Option<LinuxAudioDevice>, // Changed type
    main_loop_quit_handle: MainLoop,          // To call quit
}

impl DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    /// Enumerates available audio capture devices (monitor sources).
    fn enumerate_devices(&self) -> AudioResult<Vec<LinuxAudioDevice>> {
        // Changed return type
        debug!("LinuxDeviceEnumerator::enumerate_devices()");
        match self.get_default_device(DeviceKind::Input) {
            // Returns AudioResult<LinuxAudioDevice>
            Ok(device) => Ok(vec![device]),
            Err(AudioError::DeviceNotFound) => Ok(Vec::new()), // If no default, return empty list
            Err(e) => Err(e),
        }
    }

    /// Gets the default audio device of the specified kind.
    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<LinuxAudioDevice> {
        // Changed return type
        debug!(
            "LinuxDeviceEnumerator::get_default_device(kind: {:?})",
            kind
        );
        if kind == DeviceKind::Output {
            // This enumerator is for capture (input) devices.
            // The trait expects Self::Device, so we must return an error if it's not an input.
            return Err(AudioError::DeviceNotFound); // Corrected: DeviceNotFound is unit-like
        }

        let core = self.core_context.core();
        let registry = core.get_registry().map_err(|e| {
            AudioError::BackendError(format!("Failed to get PipeWire registry: {}", e))
        })?;

        let search_state = Arc::new(Mutex::new(DefaultDeviceSearchState {
            default_sink_name: None,
            default_sink_id: None,
            monitor_device: None,
            main_loop_quit_handle: self.core_context.main_loop().clone(),
        }));

        // Keep the listener alive until the main loop finishes.
        let _listener = registry
            .add_listener_local()
            .global({
                let state_clone = Arc::clone(&search_state);
                move |global| {
                    let mut state = state_clone.lock().unwrap();
                    if state.monitor_device.is_some() {
                        return; // Already found
                    }

                    if global.type_ == pw_types::Metadata::type_() {
                        if let Some(props) = &global.props {
                            if props.get(pw_keys::METADATA_NAME) == Some("default") {
                                if let Some(name) = props.get("default.audio.sink") {
                                    // log::debug!("Found default metadata, default.audio.sink name: {}", name);
                                    state.default_sink_name = Some(name.to_string());
                                }
                            }
                        }
                    } else if global.type_ == pw_types::Node::type_() {
                        if let Some(props) = &global.props {
                            // Step 1: Identify the default sink node by name
                            if state.default_sink_id.is_none() {
                                if let Some(ref target_sink_name) = state.default_sink_name {
                                    let node_name =
                                        props.get(pw_keys::NODE_NAME).unwrap_or_default();
                                    let node_desc =
                                        props.get(pw_keys::NODE_DESCRIPTION).unwrap_or_default();
                                    if (node_name == target_sink_name
                                        || node_desc == target_sink_name)
                                        && props.get(pw_keys::MEDIA_CLASS) == Some("Audio/Sink")
                                    {
                                        // log::debug!("Found default sink node: id={}, name='{}', desc='{}'", global.id, node_name, node_desc);
                                        state.default_sink_id = Some(global.id);
                                    }
                                }
                            }

                            // Step 2: Identify the monitor of the (now known) default sink
                            if let Some(sink_id) = state.default_sink_id {
                                if props.get(pw_keys::MEDIA_CLASS) == Some("Audio/Source") {
                                    let node_name =
                                        props.get(pw_keys::NODE_NAME).unwrap_or_default();
                                    let node_description =
                                        props.get(pw_keys::NODE_DESCRIPTION).unwrap_or_default();

                                    // Heuristic: Monitor node name often contains "Monitor of <Sink Name/Description>"
                                    // Or, it might have a "node.target" property pointing to the sink_id (less common for implicit monitors)
                                    let mut is_monitor_of_default = false;
                                    if let Some(ref sink_name_from_meta) = state.default_sink_name {
                                        if node_name.contains(&format!(
                                            "Monitor of {}",
                                            sink_name_from_meta
                                        )) || node_description.contains(&format!(
                                            "Monitor of {}",
                                            sink_name_from_meta
                                        )) {
                                            is_monitor_of_default = true;
                                        }
                                    }
                                    // Fallback: if node.target points to the sink_id (might not always be set for implicit monitors)
                                    if !is_monitor_of_default {
                                        if let Some(target_str) = props.get("node.target") {
                                            // "node.target" is not in pw_keys
                                            if target_str.parse::<u32>().ok() == Some(sink_id) {
                                                is_monitor_of_default = true;
                                            }
                                        }
                                    }
                                    // General "Monitor" check if specific link not found
                                    if !is_monitor_of_default
                                        && (node_name.contains("Monitor")
                                            || node_description.contains("Monitor"))
                                    {
                                        // This is a weaker heuristic, might pick a non-default monitor if the default sink/monitor naming is unusual.
                                        // For this simplified subtask, if we have a default_sink_id, any "Audio/Source" named "Monitor" is a candidate.
                                        // log::warn!("Found a generic monitor source (id: {}) after identifying default sink (id: {}). Assuming it's the one.", global.id, sink_id);
                                        is_monitor_of_default = true;
                                    }

                                    if is_monitor_of_default {
                                        // log::debug!("Found monitor source for default sink: id={}, name='{}', desc='{}'", global.id, node_name, node_desc);
                                        let device = LinuxAudioDevice::new(
                                            global.id,
                                            props.cloned(),
                                            Arc::clone(&self.core_context),
                                        );
                                        state.monitor_device = Some(device); // Store concrete type
                                        state.main_loop_quit_handle.quit();
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .register()
            .map_err(|e| {
                AudioError::BackendError(format!("Failed to register registry listener: {}", e))
            })?;

        // Run the main loop. It will be stopped by the callback when the device is found or implicitly times out.
        // For a more robust solution, MainLoop::iterate with a timeout loop would be better if quit is not guaranteed.
        self.core_context.main_loop().run();

        // Extract the device from the state
        let mut state_guard = search_state.lock().unwrap();
        state_guard.monitor_device.take().ok_or_else(|| {
            AudioError::DeviceNotFound // Corrected: DeviceNotFound is unit-like
        })
    }

    /// Gets a specific audio device by its ID.
    fn get_device_by_id(
        &self,
        id_str: &LinuxPipeWireDeviceId, // This is &String, as per Self::Device::DeviceId
    ) -> AudioResult<LinuxAudioDevice> {
        // Changed return type
        debug!(
            "LinuxDeviceEnumerator::get_device_by_id(id_str: {:?})",
            id_str
        );

        let _node_id_u32 = match id_str.parse::<u32>() {
            Ok(id) => id,
            Err(_) => {
                return Err(AudioError::InvalidParameter(format!(
                    // Changed to InvalidParameter for clarity
                    "Invalid PipeWire node ID string: {}. Expected a u32.",
                    id_str
                )));
            }
        };

        // TODO: Implement actual device lookup by ID using the PipeWire registry.
        // For now, per subtask instructions and to satisfy trait, return DeviceNotFound.
        warn!(
            "get_device_by_id for PipeWire is not fully implemented. ID requested: {}",
            id_str
        );
        Err(AudioError::DeviceNotFound) // Corrected: DeviceNotFound is unit-like
    }

    /// Gets a list of available input audio devices.
    fn get_input_devices(&self) -> AudioResult<Vec<LinuxAudioDevice>> {
        // Changed return type
        debug!("LinuxDeviceEnumerator::get_input_devices()");
        // This should ideally perform a full enumeration. For now, it relies on enumerate_devices.
        self.enumerate_devices()
    }

    /// Gets a list of available output audio devices.
    fn get_output_devices(&self) -> AudioResult<Vec<LinuxAudioDevice>> {
        // Changed return type
        debug!("LinuxDeviceEnumerator::get_output_devices()");
        // This enumerator focuses on capture sources.
        Ok(Vec::new())
    }
}

/// Represents an active PipeWire audio capture stream.
/// This struct holds the PipeWire stream and the core context necessary for its operation.
/// Actual data handling, format negotiation, and state management will be implemented
/// in subtask 6.4.
#[derive(Debug)]
pub(crate) struct LinuxAudioStream {
    stream: PwStream,                       // The underlying PipeWire stream object
    core_context: Arc<PipewireCoreContext>, // Keeps MainLoop, Context, Core alive and accessible
    /// Handle to the stream listener to keep it alive.
    listener_handle: Option<StreamListener>, // Changed from pipewire::stream::Listener for consistency with prompt
    /// Indicates if the stream has been started and is (or attempting to be) streaming.
    is_started: Arc<AtomicBool>,
    /// Stores the audio format negotiated with PipeWire.
    current_format: Arc<Mutex<Option<AudioFormat>>>,
    /// Queue for audio data received from PipeWire, for subtasks 6.5/6.6.
    data_queue: Arc<Mutex<VecDeque<AudioResult<AudioBuffer>>>>, // Changed to AudioBuffer struct
    /// The initial audio format requested for the stream.
    initial_config_format: AudioFormat,
    /// The PipeWire node ID to connect to for capture.
    target_node_id: u32,
    stream_start_time: Instant, // Epoch for timestamping audio buffers
}

impl LinuxAudioStream {
    /// Creates a new `LinuxAudioStream`.
    /// Records the stream creation time to be used as an epoch for `AudioBuffer` timestamps.
    ///
    /// # Arguments
    /// * `stream` - The `pipewire::Stream` object already created and configured with basic properties.
    /// * `core_context` - An `Arc` to the `PipewireCoreContext` to keep it alive.
    /// * `initial_config_format` - The audio format requested for capture.
    /// * `target_node_id` - The PipeWire node ID to capture from.
    fn new(
        stream: PwStream,
        core_context: Arc<PipewireCoreContext>,
        initial_config_format: AudioFormat,
        target_node_id: u32,
    ) -> Self {
        Self {
            stream,
            core_context,
            listener_handle: None,
            is_started: Arc::new(AtomicBool::new(false)),
            current_format: Arc::new(Mutex::new(None)),
            data_queue: Arc::new(Mutex::new(VecDeque::with_capacity(10))), // Initialize data_queue
            initial_config_format,
            target_node_id,
            stream_start_time: Instant::now(), // Record stream start time as epoch
        }
    }

    /// Converts our internal `AudioFormat` to a `pipewire::spa::param::audio::AudioFormat`.
    fn to_spa_audio_format(format: SampleFormat) -> Option<SpaAudioFormat> {
        match format {
            SampleFormat::F32LE => Some(SpaAudioFormat::F32LE),
            SampleFormat::S16LE => Some(SpaAudioFormat::S16LE),
            SampleFormat::S24LE => Some(SpaAudioFormat::S24LE),
            SampleFormat::S32LE => Some(SpaAudioFormat::S32LE),
            // TODO: Add more mappings as needed (e.g., F32BE, S16BE, etc.)
            _ => None,
        }
    }

    /// Converts a `pipewire::spa::param::audio::AudioFormat` and other SPA properties
    /// back to our internal `AudioFormat`.
    fn from_spa_format_properties(
        parsed_format: &spa::param::format_utils::ParsedSpaFormat,
    ) -> Option<AudioFormat> {
        let spa_audio_fmt = parsed_format.format_properties.as_ref()?.format?;
        let sample_format = match spa_audio_fmt {
            SpaAudioFormat::F32LE => SampleFormat::F32LE,
            SpaAudioFormat::S16LE => SampleFormat::S16LE,
            SpaAudioFormat::S24LE => SampleFormat::S24LE,
            SpaAudioFormat::S32LE => SampleFormat::S32LE,
            _ => return None, // Unsupported format
        };
        let bits_per_sample = match sample_format {
            SampleFormat::F32LE | SampleFormat::S32LE => 32,
            SampleFormat::S24LE => 24,
            SampleFormat::S16LE => 16,
            _ => return None, // Should not happen if mapped above
        };

        Some(AudioFormat {
            sample_rate: parsed_format.format_properties.as_ref()?.rate?,
            channels: parsed_format.format_properties.as_ref()?.channels? as u16,
            bits_per_sample,
            sample_format,
        })
    }
}

impl CapturingStream for LinuxAudioStream {
    /// Starts the audio capture stream.
    ///
    /// This method performs the following steps:
    /// 1. Checks if the stream is already started.
    /// 2. Converts the `initial_config_format` into PipeWire SPA (Simple Plugin API) Pods
    ///    to describe the desired audio format (e.g., F32LE, 48kHz, stereo).
    ///    Currently, it offers a single format based on `initial_config_format`.
    /// 3. Sets up listeners for stream events:
    ///    - `state_changed`: Updates the `is_started` flag based on the stream's state (e.g., `Streaming`).
    ///    - `param_changed`: When PipeWire negotiates or confirms the format, this callback
    ///      is triggered. It's responsible for parsing the confirmed format (SPA Pod) and
    ///      updating `current_format`. (Parsing logic is a TODO).
    ///    - `process`: This callback is invoked by PipeWire when new audio data is available.
    ///      It should dequeue the buffer from PipeWire. (Actual data handling and queuing
    ///      is for subtask 6.5).
    /// 4. Connects the stream to the specified `target_node_id` (e.g., a monitor source)
    ///    for input, using the format parameters built in step 2. Flags like `AUTOCONNECT`
    ///    and `RT_PROCESS` are used.
    ///
    /// The PipeWire `MainLoop` (managed by `PipewireCoreContext`) is assumed to be running
    /// in a separate thread, allowing these callbacks to be processed.
    /// TODO: The PipeWire MainLoop in `core_context` must be iterated (e.g., in a dedicated thread)
    /// for the `process` callback to fire and data to be queued. Ensure this is handled by the
    /// application or a higher-level manager.
    fn start(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::start() for stream: {:?}", self.stream.name());
        if self.is_started.load(AtomicOrdering::SeqCst) {
            // log::warn!("Stream already started or start attempt in progress.");
            return Ok(()); // Or return an error like AudioError::InvalidOperation
        }

        // 1. Convert AudioFormat to PipeWire SPA Pods
        let mut pod_buffer = Vec::new();
        let mut builder = PodBuilder::from_buffer(&mut pod_buffer);

        let spa_audio_format = Self::to_spa_audio_format(self.initial_config_format.sample_format)
            .ok_or_else(|| {
                AudioError::FormatNotSupported(format!(
                    "Unsupported sample format for PipeWire: {:?}",
                    self.initial_config_format.sample_format
                ))
            })?;

        let props_builder = spa::param::format_utils::PropsBuilder::new()
            .media_type(MediaType::Audio)
            .media_subtype(MediaSubtype::Raw) // Common for PCM
            .audio_format(spa_audio_format)
            .audio_rate(self.initial_config_format.sample_rate)
            .audio_channels(self.initial_config_format.channels as u32);

        // Add channel positions for common layouts
        let positions: Vec<SpaChannel> = match self.initial_config_format.channels {
            1 => vec![SpaChannel::Mono],
            2 => vec![SpaChannel::FL, SpaChannel::FR],
            // TODO: Add more channel layouts (e.g., 4.0, 5.1) if needed
            _ => vec![], // No specific layout for >2 channels for now, PipeWire might default
        };
        let props_builder = if !positions.is_empty() {
            props_builder.audio_position(&positions)
        } else {
            props_builder
        };

        let built_props = props_builder.build();

        let (enum_format_pod, _len) = spa::param::format_utils::encode_format(
            &mut builder,
            ParamType::EnumFormat.to_u32(), // ID for the EnumFormat parameter
            &[built_props],                 // Slice of formats to offer (just one for now)
        )
        .map_err(|e| {
            AudioError::BackendError(format!("Failed to encode format properties: {:?}", e))
        })?;

        let format_pod_array: &[Pod] = std::slice::from_ref(&enum_format_pod);

        // 2. Set up Stream Listeners
        let is_started_clone = self.is_started.clone();
        let current_format_clone = self.current_format.clone();
        let data_queue_clone = self.data_queue.clone(); // For 6.5
        let stream_start_time_clone = self.stream_start_time; // Clone for the closure

        let listener = self
            .stream
            .add_listener_local()
            .state_changed(move |old, new_state| {
                // log::debug!("Stream state changed: {:?} -> {:?}", old, new_state);
                is_started_clone.store(new_state == StreamState::Streaming, AtomicOrdering::SeqCst);
            })
            .param_changed(move |_stream, id, pod_option| {
                // log::debug!("Stream param changed: id={:?}, pod_option is some: {}", id, pod_option.is_some());
                if id == ParamType::Format.to_u32() {
                    if let Some(pod) = pod_option {
                        match spa::param::format_utils::parse_format(pod) {
                            Ok(parsed_format) => {
                                // log::debug!("Negotiated format parsed: {:?}", parsed_format);
                                if let Some(audio_fmt) =
                                    Self::from_spa_format_properties(&parsed_format)
                                {
                                    // log::info!("Negotiated audio format: {:?}", audio_fmt);
                                    *current_format_clone.lock().unwrap() = Some(audio_fmt);
                                } else {
                                    // log::error!("Failed to convert parsed SPA format to internal AudioFormat.");
                                     let err = AudioError::BackendError("Failed to convert parsed SPA format to internal AudioFormat".to_string());
                                     data_queue_clone.lock().unwrap().push_back(Err(err));
                                }
                            }
                            Err(e) => {
                                // log::error!("Failed to parse negotiated format pod: {:?}", e);
                                let err = AudioError::BackendError(format!("Failed to parse negotiated format pod: {:?}", e));
                                data_queue_clone.lock().unwrap().push_back(Err(err));
                            }
                        }
                    } else {
                        // log::warn!("Format param changed, but pod_option is None. Format might have been removed.");
                        *current_format_clone.lock().unwrap() = None;
                        // Potentially push an error or a specific marker if format becomes None during streaming
                        let err = AudioError::BackendError("PipeWire stream format became None".to_string());
                        data_queue_clone.lock().unwrap().push_back(Err(err));
                    }
                }
            })
            .process(move |stream_ref| {
                // This callback is invoked by PipeWire when new audio data is available.
                // It dequeues the buffer, converts data to f32, generates a timestamp,
                // and pushes to data_queue_clone.
                let negotiated_format_opt = current_format_clone.lock().unwrap().clone();
                let mut data_queue_locked = data_queue_clone.lock().unwrap();

                let pw_buffer = match stream_ref.dequeue_buffer() {
                    Some(b) => b,
                    None => {
                        // log::trace!("No buffer dequeued in process callback.");
                        return; // No data to process
                    }
                };

                let negotiated_audio_format = match negotiated_format_opt {
                    Some(fmt) => fmt,
                    None => {
                        // log::error!("Process callback: No negotiated format available.");
                        data_queue_locked.push_back(Err(AudioError::BackendError(
                            "No negotiated audio format available in process callback".to_string(),
                        )));
                        return;
                    }
                };

                let data_ptr_opt = pw_buffer.data(0); // Assuming interleaved audio, single data plane
                let data_ptr = match data_ptr_opt {
                    Some(ptr) if !ptr.is_empty() => ptr,
                    _ => {
                        // log::warn!("Process callback: PipeWire buffer has no data plane or is empty.");
                        // It's possible to get empty buffers, especially at stream start/end.
                        // Depending on requirements, this might not be an error to push to queue.
                        // For now, we'll skip pushing anything for an empty data plane.
                        return;
                    }
                };

                let chunk_size_bytes = data_ptr.len();
                let channels = negotiated_audio_format.channels as usize;
                let bytes_per_sample_source = negotiated_audio_format.bits_per_sample as usize / 8;

                if channels == 0 || bytes_per_sample_source == 0 {
                    data_queue_locked.push_back(Err(AudioError::BackendError(
                        "Invalid channel count or bytes_per_sample from negotiated format".to_string()
                    )));
                    return;
                }

                let num_frames = chunk_size_bytes / (channels * bytes_per_sample_source);
                if num_frames == 0 && chunk_size_bytes > 0 {
                     // This case means incomplete frame data, which is unusual for full buffers.
                    data_queue_locked.push_back(Err(AudioError::BackendError(
                        "Incomplete frame data received from PipeWire".to_string()
                    )));
                    return;
                }
                if num_frames == 0 { // No data to process
                    return;
                }


                let mut converted_samples_vec: Vec<f32> = Vec::with_capacity(num_frames * channels);

                match negotiated_audio_format.sample_format {
                    SampleFormat::F32LE => {
                        if chunk_size_bytes % 4 != 0 {
                            data_queue_locked.push_back(Err(AudioError::BackendError(
                                "F32LE data size not multiple of 4".to_string()
                            )));
                            return;
                        }
                        for chunk in data_ptr.chunks_exact(4) {
                            converted_samples_vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                        }
                    }
                    SampleFormat::S16LE => {
                        if chunk_size_bytes % 2 != 0 {
                             data_queue_locked.push_back(Err(AudioError::BackendError(
                                "S16LE data size not multiple of 2".to_string()
                            )));
                            return;
                        }
                        for chunk in data_ptr.chunks_exact(2) {
                            let sample_i16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                            converted_samples_vec.push(sample_i16 as f32 / 32768.0); // Normalize to [-1.0, 1.0)
                        }
                    }
                    // TODO: Add other format conversions (S24LE, S32LE, etc.) as needed
                    unsupported_format => {
                        // log::error!("Process callback: Unsupported source sample format: {:?}", unsupported_format);
                        data_queue_locked.push_back(Err(AudioError::FormatNotSupported(format!(
                            "Unsupported source sample format for conversion: {:?}",
                            unsupported_format
                        ))));
                        return;
                    }
                }

                // Create an AudioFormat for the VecAudioBuffer (which is always F32LE after conversion)
                let output_buffer_format = AudioFormat {
                    sample_rate: negotiated_audio_format.sample_rate,
                    channels: negotiated_audio_format.channels,
                    bits_per_sample: 32, // f32
                    sample_format: SampleFormat::F32LE,
                };

                let audio_buffer_struct = AudioBuffer {
                    data: converted_samples_vec,
                    channels: output_buffer_format.channels,
                    sample_rate: output_buffer_format.sample_rate,
                    format: output_buffer_format,
                    timestamp: Instant::now().duration_since(stream_start_time_clone), // Timestamp relative to stream start
                };
                data_queue_locked.push_back(Ok(audio_buffer_struct)); // Changed to AudioBuffer struct
            })
            .register()
            .map_err(|e| {
                AudioError::BackendError(format!(
                    "Failed to register stream listener: {}",
                    e
                ))
            })?;

        self.listener_handle = Some(listener);

        // 3. Connect the Stream
        // log::debug!(
        //     "Connecting stream to target_node_id: {}, with format_pod_array: {:?}",
        //     self.target_node_id,
        //     format_pod_array
        // );
        self.stream
            .connect(
                Direction::Input, // Capturing input from the node
                Some(self.target_node_id),
                StreamFlags::AUTOCONNECT | StreamFlags::RT_PROCESS | StreamFlags::MAP_BUFFERS,
                format_pod_array,
            )
            .map_err(|e| {
                AudioError::BackendError(format!("Failed to connect PipeWire stream: {}", e))
            })?;

        // is_started will be set by the state_changed callback.
        // For an immediate check or if connection is synchronous and successful:
        // self.is_started.store(true, AtomicOrdering::SeqCst);
        // However, relying on state_changed is more robust for async nature.
        // log::info!("PipeWire stream connection initiated for target node ID: {}", self.target_node_id);
        Ok(())
    }

    /// Stops the audio capture stream.
    /// This will involve disconnecting the PipeWire stream.
    fn stop(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::stop() for stream: {:?}", self.stream.name());
        if !self.is_started.load(AtomicOrdering::SeqCst)
            && self.stream.state() != StreamState::Streaming
        {
            // log::warn!("Stream is not running or already stopped.");
            // return Ok(()); // Or allow disconnect attempt anyway
        }

        self.stream.disconnect().map_err(|e| {
            AudioError::BackendError(format!("Failed to disconnect PipeWire stream: {}", e))
        })?;
        self.is_started.store(false, AtomicOrdering::SeqCst);
        // The listener_handle will be dropped when LinuxAudioStream is dropped,
        // or can be explicitly removed/cleared here if needed.
        // self.listener_handle.take(); // This would unregister the listener.
        // log::info!("PipeWire stream disconnected.");
        Ok(())
    }

    /// Closes the audio capture stream, releasing all resources.
    fn close(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::close() for stream: {:?}", self.stream.name());
        if self.is_started.load(AtomicOrdering::SeqCst)
            || self.stream.state() != StreamState::Unconnected
        {
            self.stop()?; // Ensure stream is stopped and disconnected
        }
        // Drop the listener handle to unregister callbacks
        self.listener_handle.take();

        // PipeWire stream itself will be cleaned up when `LinuxAudioStream` is dropped.
        // Additional cleanup if any specific resources were allocated by the stream
        // that are not handled by PwStream's Drop.
        // log::info!("PipeWire stream closed and listener removed.");
        todo!("LinuxAudioStream::close - Subtask 6.4: Finalize resource release if any beyond stop().")
    }

    /// Checks if the stream is currently capturing audio (i.e., in Streaming state).
    fn is_running(&self) -> bool {
        // log::trace!("LinuxAudioStream::is_running() check, is_started: {}", self.is_started.load(AtomicOrdering::SeqCst));
        self.is_started.load(AtomicOrdering::SeqCst)
            && self.stream.state() == StreamState::Streaming
    }

    /// Reads a chunk of audio data from the stream.
    ///
    /// This method attempts to retrieve an `AudioBuffer` from an internal queue populated
    /// by the PipeWire `process` callback. The `process` callback handles dequeuing raw
    /// data from PipeWire, converting it to `f32` samples, and packaging it.
    ///
    /// If the queue is empty, this method returns `Ok(None)` immediately (non-blocking).
    /// The `_timeout_ms` parameter is currently ignored.
    ///
    /// # Returns
    /// - `Ok(Some(AudioBuffer))`: If an audio buffer was successfully read (the struct).
    /// - `Ok(None)`: If the internal queue is empty (no data currently available).
    /// - `Err(AudioError)`: If the stream is not running, or if an error was dequeued from
    ///   the `process` callback (e.g., format conversion error).
    fn read_chunk(&mut self, _timeout_ms: Option<u32>) -> AudioResult<Option<AudioBuffer>> {
        // Changed return type
        // log::trace!("LinuxAudioStream::read_chunk(timeout_ms: {:?})", _timeout_ms);
        if !self.is_running() {
            return Err(AudioError::InvalidOperation(
                "Stream not started or not in streaming state".to_string(),
            ));
        }

        match self.data_queue.lock().unwrap().pop_front() {
            Some(audio_result) => audio_result.map(Some), // Propagates Err or wraps Ok(AudioBuffer) in Some
            None => Ok(None),                             // Queue is empty
        }
    }

    /// Provides an asynchronous stream of audio buffers.
    ///
    /// This method sets up a helper thread that continuously reads from an internal
    /// `data_queue` (populated by the PipeWire `process` callback) and sends the
    /// `AudioResult<AudioBuffer>` items to an MPSC (multi-producer,
    /// single-consumer) channel. The receiver end of this channel is returned as a
    /// `Stream`.
    ///
    /// The helper thread monitors the stream's running state (`is_started`) and the
    /// MPSC channel's health. It terminates if the stream is stopped and the queue
    /// is empty, or if the receiver end of the MPSC channel is dropped.
    ///
    /// # Returns
    /// - `Ok(Pin<Box<dyn Stream>>)`: An asynchronous stream of audio buffers (structs).
    /// - `Err(AudioError::InvalidOperation)`: If the stream is not currently running.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        Pin<
            Box<
                dyn Stream<Item = AudioResult<AudioBuffer>> // Changed to AudioBuffer struct
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        // log::debug!("LinuxAudioStream::to_async_stream()");

        if !self.is_running() {
            return Err(AudioError::InvalidOperation(
                "Stream not started or not in streaming state".to_string(),
            ));
        }

        let (tx, rx) = mpsc::unbounded::<AudioResult<AudioBuffer>>(); // Changed to AudioBuffer struct
        let data_queue_clone = Arc::clone(&self.data_queue);
        let is_started_clone = Arc::clone(&self.is_started);

        std::thread::spawn(move || {
            // log::debug!("Async stream helper thread started.");
            loop {
                let audio_result_option = {
                    // Limit scope of data_queue_locked
                    let mut data_queue_locked = data_queue_clone.lock().unwrap();
                    data_queue_locked.pop_front()
                };

                if let Some(audio_result) = audio_result_option {
                    if tx.unbounded_send(audio_result).is_err() {
                        // Receiver has been dropped, meaning the stream is no longer being consumed.
                        // log::info!("Async stream helper thread: MPSC receiver dropped. Terminating.");
                        break;
                    }
                } else {
                    // Queue is empty
                    if !is_started_clone.load(AtomicOrdering::SeqCst) {
                        // Stream is stopped and queue is empty, so we can terminate.
                        // log::info!("Async stream helper thread: Stream stopped and queue empty. Terminating.");
                        break;
                    }
                    // Stream is still running but queue is empty, wait a bit.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            // log::debug!("Async stream helper thread finished.");
        });

        Ok(Box::pin(rx))
    }
}

// Note: The old `AudioStream` trait implementation for `LinuxAudioStream` is removed
// as `CapturingStream` is the relevant trait for capture.
// If a unified `AudioStream` is needed later, it can be re-added.

// --- Old PipeWire Backend (To be refactored/removed) ---
// This section contains the previous implementation and will be gradually
// replaced or integrated into the new trait-based structure.

pub struct PipeWireBackend {
    main_loop: MainLoop,
    context: PwContext,
    core: Core,
    registry: Registry,
    _stream_threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        Self::check_pipewire_installed()?;
        pipewire::init();
        let main_loop = MainLoop::new(None).map_err(|e| {
            AudioError::BackendError(format!("Failed to create PipeWire main loop: {}", e))
        })?;
        let context = PwContext::new(&main_loop).map_err(|e| {
            AudioError::BackendError(format!("Failed to create PipeWire context: {}", e))
        })?;
        let core = context.connect(None).map_err(|e| {
            AudioError::BackendError(format!("Failed to connect to PipeWire: {}", e))
        })?;
        let registry = core.get_registry().map_err(|e| {
            AudioError::BackendError(format!("Failed to get PipeWire registry: {}", e))
        })?;
        Ok(Self {
            main_loop,
            context,
            core,
            registry,
            _stream_threads: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn check_pipewire_installed() -> Result<(), AudioError> {
        let library_check = Command::new("sh")
            .args(["-c", "ldconfig -p | grep -q libpipewire"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !library_check {
            return Err(AudioError::ConfigurationError(
                "PipeWire libraries not found. Please install libpipewire-0.3-0 or equivalent for your distribution".to_string()
            ));
        }
        let daemon_check = Command::new("sh")
            .args(["-c", "ps -e | grep -q pipewire"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !daemon_check {
            return Err(AudioError::ConfigurationError(
                "PipeWire daemon is not running. Please make sure PipeWire is properly installed and running".to_string()
            ));
        }
        Ok(())
    }

    pub fn is_available() -> bool {
        if let Err(e) = Self::check_pipewire_installed() {
            println!("PipeWire availability check failed: {}", e);
            return false;
        }
        println!("PipeWire check passed (simplified)");
        true
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let mut apps = Vec::new();
        apps.push(AudioApplication {
            name: "System".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let apps_arc = Arc::new(Mutex::new(apps));
        let listener = self.registry.add_listener_local().global({
            let apps_clone = Arc::clone(&apps_arc);
            let tx_clone = Arc::clone(&tx);
            move |global| {
                if let Some(props) = &global.props {
                    let media_class = props.get("media.class").unwrap_or("");
                    if media_class == "Stream/Input/Audio" || media_class == "Stream/Output/Audio" {
                        let mut apps_guard = apps_clone.lock().unwrap();
                        let app = AudioApplication {
                            name: props
                                .get("application.name")
                                .or_else(|| props.get("media.name"))
                                .unwrap_or("Unknown")
                                .to_string(),
                            id: global.id.to_string(),
                            executable_name: props
                                .get("application.process.binary")
                                .unwrap_or("unknown")
                                .to_string(),
                            pid: props
                                .get("application.process.id")
                                .and_then(|pid| pid.parse().ok())
                                .unwrap_or(0),
                        };
                        apps_guard.push(app);
                        let _ = tx_clone.lock().unwrap().send(());
                    }
                }
            }
        });
        let timeout = Duration::from_secs(1);
        let _ = rx.recv_timeout(timeout);
        drop(listener);
        thread::sleep(Duration::from_millis(10));
        match Arc::try_unwrap(apps_arc) {
            Ok(mutex) => Ok(mutex
                .into_inner()
                .map_err(|e| AudioError::Unknown(e.to_string()))?),
            Err(arc_again) => {
                println!(
                    "Warning: Could not obtain exclusive ownership of app list Arc. Cloning data."
                );
                Ok(arc_again.lock().unwrap().clone())
            }
        }
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let stream =
            PipeWireStream::new(&self.core, app, config, Arc::clone(&self._stream_threads))?;
        Ok(Box::new(stream))
    }
}

unsafe impl Send for PipeWireBackend {}

impl AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }
    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        self.list_applications()
    }
    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        self.capture_application(app, config)
    }
}

pub struct PipeWireStream {
    config: AudioConfig,
    buffer: Arc<Mutex<Vec<u8>>>,
    stream_command_tx: Option<channel::Sender<StreamCommand>>,
    _stream_thread: Option<thread::JoinHandle<()>>,
}

#[derive(Debug)] // Added Debug derive
enum StreamCommand {
    Connect,
    Disconnect,
    Shutdown,
}

impl PipeWireStream {
    fn new(
        _core: &Core,
        app: &AudioApplication,
        config: AudioConfig,
        _threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    ) -> Result<Self, AudioError> {
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(16384)));
        let buffer_clone_for_thread = Arc::clone(&buffer);
        let (cmd_tx, cmd_rx) = channel::channel::<StreamCommand>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let app_id = app.id.clone();
        let app_pid = app.pid;
        let config_clone = config.clone();

        let thread_handle = thread::spawn(move || {
            pipewire::init();
            let main_loop = match MainLoop::new(None) {
                Ok(ml) => ml,
                Err(e) => {
                    ready_tx
                        .send(Err(format!(
                            "Old Backend: Failed to create MainLoop: {}",
                            e
                        )))
                        .unwrap();
                    return;
                }
            };
            let context = match PwContext::new(&main_loop) {
                Ok(ctx) => ctx,
                Err(e) => {
                    ready_tx
                        .send(Err(format!("Old Backend: Failed to create Context: {}", e)))
                        .unwrap();
                    return;
                }
            };
            let core = match context.connect(None) {
                Ok(c) => c,
                Err(e) => {
                    ready_tx
                        .send(Err(format!(
                            "Old Backend: Failed to connect to Core: {}",
                            e
                        )))
                        .unwrap();
                    return;
                }
            };
            let props = properties! {
                "media.class" => "Audio/Source",
                // Access channels and sample_rate through the 'format' field of StreamConfig
                "audio.channels" => config_clone.format.channels.to_string(),
                "audio.rate" => config_clone.format.sample_rate.to_string(),
                "target.object" => if app_pid == 0 { "default.monitor" } else { &app_id },
            };
            let stream_name = if app_pid == 0 {
                "system-audio-capture"
            } else {
                "application-audio-capture"
            };
            let mut stream = match PwStream::new(&core, stream_name, props) {
                Ok(s) => s,
                Err(e) => {
                    ready_tx
                        .send(Err(format!("Failed to create PipeWire stream: {}", e)))
                        .unwrap();
                    return;
                }
            };
            let _listener = stream
                .add_local_listener_with_user_data(buffer_clone_for_thread)
                .process(|stream, user_data_buffer_arc| {
                    if let Some(mut buffer) = stream.dequeue_buffer() {
                        if let Some(data_plane) = buffer.datas_mut().get_mut(0) {
                            if let Some(data) = data_plane.data() {
                                if let Ok(mut shared_buf) = user_data_buffer_arc.lock() {
                                    shared_buf.extend_from_slice(data);
                                }
                            }
                        }
                    }
                })
                .register()
                .map_err(|e| format!("Failed to register stream listener: {}", e));
            if let Err(e) = _listener {
                ready_tx.send(Err(e)).unwrap();
                return;
            }
            let main_loop_clone = main_loop.clone();
            let receiver_loop = main_loop.loop_();
            let _receiver_attachment = cmd_rx.attach(&receiver_loop, move |cmd| {
                match cmd {
                    StreamCommand::Connect => {
                        let mut params_slice: Vec<&Pod> = Vec::new();
                        match stream.connect(
                            Direction::Input, // Use Direction
                            None,
                            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                            &mut params_slice,
                        ) {
                            Ok(_) => {
                                println!("PipeWire stream connected via command.");
                            }
                            Err(e) => {
                                eprintln!("Error connecting PipeWire stream via command: {:?}", e);
                            }
                        }
                    }
                    StreamCommand::Disconnect => {
                        let _ = stream.disconnect();
                    }
                    StreamCommand::Shutdown => {
                        main_loop_clone.quit();
                    }
                }
            });
            ready_tx.send(Ok(())).unwrap();
            main_loop.run();
            drop(core);
            drop(context);
        });

        match ready_rx.recv().map_err(|e| {
            AudioError::BackendError(format!("Failed to initialize PipeWire thread: {}", e))
        })? {
            Ok(()) => Ok(Self {
                config,
                buffer,
                stream_command_tx: Some(cmd_tx),
                _stream_thread: Some(thread_handle),
            }),
            Err(e_str) => Err(AudioError::BackendError(e_str)),
        }
    }
}

impl AudioCaptureStream for PipeWireStream {
    fn start(&mut self) -> Result<(), AudioError> {
        if let Some(tx) = &self.stream_command_tx {
            tx.send(StreamCommand::Connect).map_err(|e| {
                AudioError::BackendError(format!(
                    "Old Backend: Failed to send connect command: {:?}",
                    e
                ))
            })?;
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        if let Some(tx) = &self.stream_command_tx {
            tx.send(StreamCommand::Disconnect).map_err(|e| {
                AudioError::BackendError(format!(
                    "Old Backend: Failed to send disconnect command: {:?}",
                    e
                ))
            })?;
        }
        Ok(())
    }
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut shared_buf = self.buffer.lock().map_err(|_| {
            AudioError::BackendError("Old Backend: Mutex poisoned in read".to_string())
        })?;
        let copy_size = std::cmp::min(buffer.len(), shared_buf.len());
        if copy_size > 0 {
            buffer[..copy_size].copy_from_slice(&shared_buf[..copy_size]);
            shared_buf.drain(..copy_size);
        }
        Ok(copy_size)
    }
    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        if let Some(tx) = &self.stream_command_tx {
            let _ = tx.send(StreamCommand::Shutdown);
        }
    }
}

unsafe impl Send for PipeWireStream {}

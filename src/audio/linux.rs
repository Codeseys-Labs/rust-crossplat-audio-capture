//! Linux-specific audio capture backend using PipeWire.
#![cfg(target_os = "linux")]

use crate::core::config::{AudioCaptureConfig, AudioConfig, StreamConfig}; // Corrected import path
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioBuffer, AudioDevice, AudioStream, CapturingStream, DeviceEnumerator, DeviceKind,
    StreamDataCallback,
};
use crate::{AudioFormat, SampleFormat}; // AudioFormat is re-exported from lib.rs
use pipewire::{
    keys as pw_keys,
    spa::{
        param::format::{FormatProperties, SpaFormat},
        param::format_utils,
        param::ParamType,
        pod::Pod, // Keep this if Pod is used directly, otherwise remove if only through spa::Pod
        utils::Direction as PwDirection,
        Id,
    },
    types as pw_types,
}; // Added for PipeWire keys and types
use std::fmt::Display; // Added for DeviceId Display trait
use std::sync::Arc; // Added for Arc

// TODO: Remove these once the actual PipeWire logic is integrated with the new traits.
// These are placeholders from the old structure.
use std::{process::Command, sync::Mutex, thread, time::Duration};

// Adjusted imports for PipewireCoreContext
use pipewire::{
    self,
    channel,
    // context::Context as PwContext, // Keep PwContext for old code if needed
    // core::Core as PwCore, // Keep PwCore for old code if needed
    // main_loop::MainLoop as PwMainLoop, // Keep PwMainLoop for old code if needed
    properties::properties, // This is fine
    registry::Registry,     // This is fine
    spa,
    // spa::pod::{Object, Pod}, // Pod is used directly from spa::pod::Pod
    stream::{Stream as PwStream, StreamFlags},
    Context,    // For PipewireCoreContext
    Core,       // For PipewireCoreContext
    MainLoop,   // For PipewireCoreContext
    Properties, // Explicitly import Properties
};

use super::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};

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
        // let _init_token = pipewire::init().map_err(|()| AudioError::BackendSpecificError("Failed to initialize global PipeWire state".to_string()))?;

        let main_loop = MainLoop::new(None).map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to create PipeWire MainLoop: {}", e))
        })?;
        let context = Context::new(&main_loop).map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to create PipeWire Context: {}", e))
        })?;
        let core = context.connect(None).map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to connect to PipeWire Core: {}", e))
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
        _capture_config: &AudioCaptureConfig, // capture_config will be used in LinuxAudioStream setup
    ) -> AudioResult<Box<dyn CapturingStream>> {
        // log::debug!("LinuxAudioDevice::create_stream for device ID: {}", self.id);

        let core = self.core_context.core();
        // MainLoop is managed by PipewireCoreContext, stream will use it.

        let stream_props = pipewire::Properties::new()
            .set(pw_keys::MEDIA_TYPE, "Audio")
            .set(pw_keys::MEDIA_CATEGORY, "Capture") // For capture streams
            .set(pw_keys::MEDIA_ROLE, "Music") // Or a more generic role like "Generic" or "System"
            .set(pw_keys::NODE_ID, &self.id.to_string()) // Target this specific node for capture
            // For capturing from a monitor source, we usually don't set STREAM_CAPTURE_SINK.
            // STREAM_CAPTURE_SINK = true is for capturing the output of a *sink* application.
            // If self.id refers to a monitor node directly, this is not needed.
            // If self.id refers to a sink and we want its monitor, then it might be,
            // but device enumeration should give us the monitor node ID directly.
            // .set(pw_keys::STREAM_CAPTURE_SINK, "true") // Only if capturing from a sink's monitor port explicitly
            .set(pw_keys::STREAM_WANT_FORMAT, "F32LE") // Request a common format, negotiation happens later
            .set("audio.channels", "2") // Example, should come from capture_config or negotiation
            .set("audio.rate", "48000"); // Example

        let stream = PwStream::new_with_properties(
            core,
            &format!("audio-capture-{}", self.id), // Unique stream name
            stream_props,
        )
        .map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to create PipeWire stream: {}", e))
        })?;

        // log::info!("PipeWire stream created for device ID: {}", self.id);
        Ok(Box::new(LinuxAudioStream::new(
            stream,
            Arc::clone(&self.core_context),
        )))
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
        // TODO: Move pipewire::init() to a global, once-per-application call.
        pipewire::init();

        let core_context = Arc::new(PipewireCoreContext::new()?);
        Ok(Self { core_context })
    }
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
    monitor_device: Option<Box<dyn AudioDevice>>,
    main_loop_quit_handle: MainLoop, // To call quit
}

impl DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    /// Enumerates available audio capture devices (monitor sources).
    ///
    /// For subtask 6.2, this is a simplified implementation that attempts to find
    /// the monitor source of the default audio sink.
    ///
    /// TODO: Implement full enumeration of all available monitor sources in a later subtask.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // log::debug!("LinuxDeviceEnumerator::enumerate_devices()");
        // TODO: Implement full enumeration of all available monitor sources.
        // For now, it calls get_default_device for DeviceKind::Input.
        match self.get_default_device(DeviceKind::Input) {
            Ok(Some(device)) => Ok(vec![device]),
            Ok(None) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Gets the default audio device of the specified kind.
    ///
    /// For `DeviceKind::Input`, this attempts to find the monitor source of the
    /// default audio sink. For `DeviceKind::Output`, it returns `Ok(None)`.
    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        // log::debug!("LinuxDeviceEnumerator::get_default_device(kind: {:?})", kind);
        if kind == DeviceKind::Output {
            return Ok(None); // This enumerator is for capture (input) devices
        }

        let core = self.core_context.core();
        let registry = core.get_registry().map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to get PipeWire registry: {}", e))
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
                                        state.monitor_device = Some(Box::new(device));
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
                AudioError::BackendSpecificError(format!(
                    "Failed to register registry listener: {}",
                    e
                ))
            })?;

        // Run the main loop. It will be stopped by the callback when the device is found or implicitly times out.
        // For a more robust solution, MainLoop::iterate with a timeout loop would be better if quit is not guaranteed.
        self.core_context.main_loop().run();

        // Extract the device from the state
        let mut state_guard = search_state.lock().unwrap();
        Ok(state_guard.monitor_device.take())
    }

    /// Gets a specific audio device by its ID.
    ///
    /// The `id_str` is the string representation of the PipeWire node ID.
    /// `kind` can be used to filter, but is ignored for this simplified implementation.
    ///
    /// TODO: Implement actual device lookup and validation in a later subtask.
    fn get_device_by_id(
        &self,
        id_str: &LinuxPipeWireDeviceId,
        _kind: Option<DeviceKind>,
    ) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        // log::debug!("LinuxDeviceEnumerator::get_device_by_id(id_str: {:?}, kind: {:?})", id_str, _kind);
        // The DeviceId (String) needs to be parsed to a u32 if it represents a PipeWire node ID.
        // Use the registry to get information about the node with this ID.
        // Check if it's a suitable capture node (monitor source). If so, create and return LinuxAudioDevice.

        // Attempt to parse the ID string to u32.
        let _node_id = match id_str.parse::<u32>() {
            Ok(id) => id,
            Err(_) => {
                return Err(AudioError::InvalidDeviceId(format!(
                    "Invalid PipeWire node ID string: {}",
                    id_str
                )))
            }
        };

        // TODO: Implement actual device lookup by ID using the PipeWire registry
        // and verify it's a suitable monitor source. For subtask 6.2, this is not implemented.
        // log::warn!("get_device_by_id for PipeWire is not fully implemented yet. Returning Ok(None). ID requested: {}", id_str);
        Ok(None)
        // Alternatively, to strictly follow the prompt's allowance for NotImplemented:
        // Err(AudioError::NotImplemented("Device lookup by ID for PipeWire is not yet implemented.".to_string()))
    }

    /// Gets a list of available input audio devices.
    /// For PipeWire, these are typically monitor sources.
    fn get_input_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // log::debug!("LinuxDeviceEnumerator::get_input_devices()");
        // For now, this is consistent with enumerate_devices which focuses on the default input.
        // A full implementation would list all suitable input (monitor) nodes.
        self.enumerate_devices()
    }

    /// Gets a list of available output audio devices.
    /// This enumerator focuses on capture sources, so this will return an empty list.
    fn get_output_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // log::debug!("LinuxDeviceEnumerator::get_output_devices()");
        Ok(Vec::new())
    }
}

/// Represents an active PipeWire audio capture stream.
/// This struct holds the PipeWire stream and the core context necessary for its operation.
/// Actual data handling, format negotiation, and state management will be implemented
/// in subtask 6.4.
#[derive(Debug)]
pub(crate) struct LinuxAudioStream {
    stream: PwStream, // The underlying PipeWire stream object
    core_context: Arc<PipewireCoreContext>, // Keeps MainLoop, Context, Core alive and accessible
                      // TODO (Subtask 6.4): Add fields for:
                      // - AudioCaptureConfig
                      // - Actual AudioFormat negotiated
                      // - Buffer for incoming data (e.g., Arc<Mutex<VecDeque<u8>>> or similar)
                      // - State (e.g., Idle, Capturing, Paused, Stopped, Error)
                      // - Listeners for stream events (state changes, data ready, errors)
                      // - Callback for delivering data (StreamDataCallback)
}

impl LinuxAudioStream {
    /// Creates a new `LinuxAudioStream`.
    ///
    /// # Arguments
    /// * `stream` - The `pipewire::Stream` object already created and configured with basic properties.
    /// * `core_context` - An `Arc` to the `PipewireCoreContext` to keep it alive.
    fn new(stream: PwStream, core_context: Arc<PipewireCoreContext>) -> Self {
        Self {
            stream,
            core_context,
            // Initialize other fields here in subtask 6.4
        }
    }
}

impl CapturingStream for LinuxAudioStream {
    /// Starts the audio capture stream.
    /// This will involve connecting the PipeWire stream and setting up parameters.
    fn start(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::start() for stream: {:?}", self.stream.name());
        // TODO (Subtask 6.4):
        // 1. Define parameters for the stream (e.g., format, buffer size).
        //    - This might involve using self.capture_config.format or negotiating.
        //    - Build SPA Pods for format parameters (e.g., using spa::param::format_utils).
        // 2. Connect the stream: self.stream.connect(...)
        //    - Specify Direction::Input.
        //    - Provide format parameters.
        //    - Use flags like StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS.
        // 3. Set up listeners for stream events:
        //    - `state_changed`: To know when the stream is active, paused, etc.
        //    - `param_changed`: To get the negotiated format.
        //    - `process`: To receive audio data.
        //    - `error`: To handle stream errors.
        // 4. Update internal state to Capturing.
        // 5. Potentially run the main_loop in a dedicated thread if blocking, or use iterate.
        //    The core_context.main_loop() can be used.
        todo!("LinuxAudioStream::start - Subtask 6.4: Connect stream, set params, handle events.")
    }

    /// Stops the audio capture stream.
    /// This will involve disconnecting the PipeWire stream.
    fn stop(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::stop() for stream: {:?}", self.stream.name());
        // TODO (Subtask 6.4):
        // 1. Disconnect the stream: self.stream.disconnect()?;
        // 2. Update internal state to Stopped.
        // 3. Clean up any resources if necessary.
        todo!("LinuxAudioStream::stop - Subtask 6.4: Disconnect stream.")
    }

    /// Closes the audio capture stream, releasing all resources.
    fn close(&mut self) -> AudioResult<()> {
        // log::debug!("LinuxAudioStream::close() for stream: {:?}", self.stream.name());
        // TODO (Subtask 6.4):
        // 1. Ensure the stream is stopped.
        // 2. Explicitly drop or clean up the self.stream object if needed (usually handled by Drop trait).
        // 3. Clear listeners.
        // 4. Update internal state.
        todo!("LinuxAudioStream::close - Subtask 6.4: Release all stream resources.")
    }

    /// Checks if the stream is currently capturing audio.
    fn is_running(&self) -> bool {
        // log::trace!("LinuxAudioStream::is_running() check");
        // TODO (Subtask 6.4):
        // Return true if the internal state is Capturing and stream state is Streaming.
        // Check self.stream.state() or an internal state variable.
        false // Placeholder
    }

    /// Reads a chunk of audio data from the stream.
    /// This method might block until data is available or a timeout occurs.
    fn read_chunk(
        &mut self,
        _timeout_ms: Option<u32>,
    ) -> AudioResult<Option<Box<dyn AudioBuffer>>> {
        // log::trace!("LinuxAudioStream::read_chunk(timeout_ms: {:?})", _timeout_ms);
        // TODO (Subtask 6.4):
        // 1. Check if data is available in the internal buffer (filled by the `process` callback).
        // 2. If not, wait for data (potentially using a condition variable or by iterating the main loop).
        // 3. Dequeue data from PipeWire buffers if not using an intermediate buffer.
        // 4. Package data into an AudioBuffer.
        // 5. Handle timeouts.
        todo!("LinuxAudioStream::read_chunk - Subtask 6.4: Read data from internal buffer or PipeWire.")
    }

    /// Provides an asynchronous stream of audio buffers.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>
                    // Assuming f32 for now
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        // log::debug!("LinuxAudioStream::to_async_stream()");
        // TODO (Subtask 6.4 or later):
        // Implement an async wrapper around the data capture mechanism.
        // This might involve a channel or a custom Future/Stream implementation
        // that integrates with the PipeWire event loop and data callbacks.
        Err(AudioError::NotImplemented(
            "Async stream for PipeWire not yet implemented.".to_string(),
        ))
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
            let main_loop = MainLoop::new(None).unwrap();
            let context = PwContext::new(&main_loop).unwrap();
            let core = context.connect(None).unwrap();
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
                            PwDirection::Input, // Use aliased PwDirection
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
                AudioError::Unknown(format!("Failed to send connect command: {:?}", e))
                // Use {:?} for Debug
            })?;
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        if let Some(tx) = &self.stream_command_tx {
            tx.send(StreamCommand::Disconnect).map_err(|e| {
                AudioError::Unknown(format!("Failed to send disconnect command: {:?}", e))
                // Use {:?} for Debug
            })?;
        }
        Ok(())
    }
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut shared_buf = self.buffer.lock().unwrap();
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

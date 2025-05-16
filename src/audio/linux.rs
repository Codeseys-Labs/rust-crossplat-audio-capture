//! Linux-specific audio capture backend using PipeWire.
#![cfg(target_os = "linux")]

use crate::core::config::{AudioCaptureConfig, AudioConfig, StreamConfig}; // Corrected import path
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioBuffer, AudioDevice, AudioStream, CapturingStream, DeviceEnumerator, DeviceKind,
    StreamDataCallback,
};
use crate::AudioFormat; // AudioFormat is re-exported from lib.rs
use pipewire::{keys as pw_keys, types as pw_types}; // Added for PipeWire keys and types
use std::fmt::Display; // Added for DeviceId Display trait

// TODO: Remove these once the actual PipeWire logic is integrated with the new traits.
// These are placeholders from the old structure.
use std::{
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use pipewire::spa::utils::Direction as PwDirection;
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
    spa::pod::{Object, Pod},
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
    id: u32, // PipeWire node ID
    props: Option<Properties>, // Store node properties
             // core_context: Arc<PipewireCoreContext>, // If needed for device operations
}

impl LinuxAudioDevice {
    /// Creates a new LinuxAudioDevice.
    fn new(id: u32, props: Option<Properties>) -> Self {
        Self { id, props }
    }
}

impl AudioDevice for LinuxAudioDevice {
    type DeviceId = LinuxPipeWireDeviceId;

    /// Gets the unique identifier of the audio device.
    /// For PipeWire, this is the node ID converted to a String.
    fn get_id(&self) -> Self::DeviceId {
        self.id.to_string()
    }

    /// Gets the human-readable name of the audio device.
    fn get_name(&self) -> String {
        self.props
            .as_ref()
            .and_then(|p| {
                p.get(pw_keys::NODE_DESCRIPTION)
                    .or_else(|| p.get(pw_keys::NODE_NAME))
            })
            .unwrap_or_else(|| format!("PipeWire Node {}", self.id))
            .to_string()
    }

    /// Gets a list of supported audio formats for this device.
    /// TODO: Implement in subtask 6.3.
    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // log::debug!("LinuxAudioDevice::get_supported_formats() - props: {:?}", self.props);
        todo!("LinuxAudioDevice::get_supported_formats - Subtask 6.3")
    }

    /// Gets the default audio format for this device.
    /// TODO: Implement in subtask 6.3.
    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        // log::debug!("LinuxAudioDevice::get_default_format() - props: {:?}", self.props);
        todo!("LinuxAudioDevice::get_default_format - Subtask 6.3")
    }

    /// Returns `true` if this device is an input device (e.g., microphone, system audio monitor).
    fn is_input(&self) -> bool {
        // Monitor sources are considered inputs from the application's perspective.
        // PipeWire nodes with "Audio/Source" are sources of audio data.
        if let Some(props) = &self.props {
            if props.get(pw_keys::MEDIA_CLASS) == Some("Audio/Source") {
                return true;
            }
        }
        // Fallback or further checks if needed
        true // Assuming for now that devices enumerated are capture-capable
    }

    /// Returns `true` if this device is an output device (e.g., speakers).
    fn is_output(&self) -> bool {
        // This enumerator focuses on capture sources.
        false
    }

    /// Checks if the device is currently active or available.
    /// TODO: Implement in subtask 6.3.
    fn is_active(&self) -> bool {
        // log::debug!("LinuxAudioDevice::is_active() - props: {:?}", self.props);
        todo!("LinuxAudioDevice::is_active - Subtask 6.3")
    }

    /// Checks if the specified audio format is supported by this device.
    /// TODO: Implement in subtask 6.3.
    fn is_format_supported(&self, _format: &AudioFormat) -> AudioResult<bool> {
        // log::debug!("LinuxAudioDevice::is_format_supported({:?}) - props: {:?}", _format, self.props);
        todo!("LinuxAudioDevice::is_format_supported - Subtask 6.3")
    }

    /// Creates an audio stream for capturing data from this device.
    /// TODO: Implement in subtask 6.3.
    fn create_stream(
        &mut self,
        _capture_config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>> {
        // log::debug!("LinuxAudioDevice::create_stream(capture_config: {:?})", _capture_config);
        todo!("LinuxAudioDevice::create_stream - Subtask 6.3")
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
                                        let device =
                                            LinuxAudioDevice::new(global.id, props.cloned());
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

pub struct LinuxAudioStream {
    // TODO: Add fields specific to a Linux audio stream (e.g., PipeWire stream, buffer, config)
    // This struct will be properly defined and implemented in subtask 6.4.
    #[allow(dead_code)] // Will be used in subtask 6.4
    config: Option<StreamConfig>,
}

impl AudioStream for LinuxAudioStream {
    type Config = StreamConfig;
    type Device = LinuxAudioDevice; // This now refers to the new LinuxAudioDevice

    fn open(&mut self, device: &Self::Device, config: Self::Config) -> AudioResult<()> {
        // log::debug!(
        //     "TODO: LinuxAudioStream::open(device_id: {:?}, config: {:?})",
        //     device.get_id(),
        //     config
        // );
        self.config = Some(config);
        todo!("LinuxAudioStream::open - Subtask 6.4")
    }

    fn start(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::start()");
        todo!("LinuxAudioStream::start - Subtask 6.4")
    }

    fn pause(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::pause()");
        todo!("LinuxAudioStream::pause - Subtask 6.4")
    }

    fn resume(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::resume()");
        todo!("LinuxAudioStream::resume - Subtask 6.4")
    }

    fn stop(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::stop()");
        todo!("LinuxAudioStream::stop - Subtask 6.4")
    }

    fn close(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::close()");
        self.config = None;
        todo!("LinuxAudioStream::close - Subtask 6.4")
    }

    fn set_format(&mut self, _format: &AudioFormat) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::set_format({:?})", _format);
        todo!("LinuxAudioStream::set_format - Subtask 6.4")
    }

    fn set_callback(&mut self, _callback: StreamDataCallback) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream::set_callback()");
        todo!("LinuxAudioStream::set_callback - Subtask 6.4")
    }

    fn is_running(&self) -> bool {
        // log::debug!("TODO: LinuxAudioStream::is_running()");
        false
    }

    fn get_latency_frames(&self) -> AudioResult<u64> {
        // log::debug!("TODO: LinuxAudioStream::get_latency_frames()");
        todo!("LinuxAudioStream::get_latency_frames - Subtask 6.4")
    }

    fn get_current_format(&self) -> AudioResult<AudioFormat> {
        // log::debug!("TODO: LinuxAudioStream::get_current_format()");
        todo!("LinuxAudioStream::get_current_format - Subtask 6.4")
    }
}

impl CapturingStream for LinuxAudioStream {
    fn start(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream (CapturingStream)::start()");
        todo!("LinuxAudioStream (CapturingStream)::start - Subtask 6.4")
    }

    fn stop(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream (CapturingStream)::stop()");
        todo!("LinuxAudioStream (CapturingStream)::stop - Subtask 6.4")
    }

    fn close(&mut self) -> AudioResult<()> {
        // log::debug!("TODO: LinuxAudioStream (CapturingStream)::close()");
        todo!("LinuxAudioStream (CapturingStream)::close - Subtask 6.4")
    }

    fn is_running(&self) -> bool {
        // log::debug!("TODO: LinuxAudioStream (CapturingStream)::is_running()");
        false
    }

    fn read_chunk(
        &mut self,
        _timeout_ms: Option<u32>,
    ) -> AudioResult<Option<Box<dyn AudioBuffer>>> {
        // log::debug!(
        //     "TODO: LinuxAudioStream (CapturingStream)::read_chunk(timeout_ms: {:?})",
        //     _timeout_ms
        // );
        todo!("LinuxAudioStream (CapturingStream)::read_chunk - Subtask 6.4")
    }

    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        // log::debug!("TODO: LinuxAudioStream (CapturingStream)::to_async_stream()");
        todo!("LinuxAudioStream (CapturingStream)::to_async_stream - Subtask 6.4")
    }
}

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

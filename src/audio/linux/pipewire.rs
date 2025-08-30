// src/audio/linux/pipewire.rs

use crate::core::error::AudioError;
use pipewire::registry::ListenerBuilderError;
use pipewire::spa::pod::PodError;
use pipewire::{
    self,
    spa::{
        self, pod::object::PodObjectRef, sys::pw_core_sync, types::ObjectType, utils::Direction,
    },
    sys::{pw_main_loop_quit, pw_main_loop_run},
    types::PW_ID_CORE,
};
use std::collections::HashMap;
use std::ffi::CStr;
use std::rc::Rc;
use std::sync::{Arc, Mutex}; // Added Mutex

// Import PipewireCoreContext from the parent module (linux.rs)
// This was added by a previous step, ensuring it's here.
use super::PipewireCoreContext;

/// Holds information about a Linux application providing an audio stream.
#[derive(Debug, Clone)]
pub struct LinuxApplicationInfo {
    pub process_id: Option<u32>,
    pub name: Option<String>,
    pub executable_path: Option<String>, // Consider std::path::PathBuf
    pub pipewire_node_id: Option<u32>,   // Specific to PipeWire, useful for targeting capture
    // Add other fields if readily available and useful, e.g., stream description
    pub stream_description: Option<String>,
    pub pulseaudio_sink_input_index: Option<u32>, // For PulseAudio subtask
}

// Helper to map PipeWire errors to our AudioError
fn pw_error_to_capture_error(e: impl std::fmt::Display) -> AudioError {
    AudioError::CaptureError(format!("PipeWire error: {}", e))
}

fn pod_error_to_capture_error(e: PodError) -> AudioError {
    AudioError::CaptureError(format!("PipeWire POD error: {:?}", e))
}

fn listener_builder_error_to_capture_error(e: ListenerBuilderError) -> AudioError {
    AudioError::CaptureError(format!("PipeWire ListenerBuilder error: {}", e))
}

/// Enumerates audio-producing applications using PipeWire.
///
/// This function connects to the PipeWire daemon, inspects nodes,
/// and attempts to identify applications that are currently outputting audio.
pub(crate) fn enumerate_audio_applications_pipewire(
    core_context: &PipewireCoreContext,
) -> Result<Vec<LinuxApplicationInfo>, AudioError> {
    let mainloop = core_context.main_loop();
    let core = core_context.core();
    let registry = core.get_registry().map_err(pw_error_to_capture_error)?;

    // Arc and Mutex to share applications list between main thread and listener
    let applications = Arc::new(Mutex::new(Vec::new()));
    let done = Rc::new(std::cell::Cell::new(false)); // To signal completion

    // Temporary listener for initial node enumeration
    let listener_done = done.clone();
    let apps_clone_registry = applications.clone();

    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            // We are interested in Nodes
            if global.type_ == pipewire::types::PW_TYPE_INTERFACE_Node {
                if let Some(props) = &global.props {
                    // Filter for audio output nodes from applications
                    // "media.class" == "Stream/Output/Audio" is a good indicator
                    // "application.name" and "application.process.id" are key properties
                    let mut app_name = None;
                    let mut app_pid = None;
                    let mut app_binary = None;
                    let mut node_name = None;
                    let mut media_class = None;

                    for (key, value) in props.iter() {
                        let key_str = key.to_string_lossy();
                        let value_str = value.to_string_lossy();

                        match key_str.as_ref() {
                            "application.name" => app_name = Some(value_str.into_owned()),
                            "application.process.id" => app_pid = value_str.parse::<u32>().ok(),
                            "application.process.binary" => {
                                app_binary = Some(value_str.into_owned())
                            }
                            "node.name" => node_name = Some(value_str.into_owned()),
                            "media.class" => media_class = Some(value_str.into_owned()),
                            _ => {}
                        }
                    }

                    // Heuristic: if it has an application name and is an audio output stream
                    if let (Some(name), Some(mc)) = (&app_name, &media_class) {
                        if mc.starts_with("Stream/Output/Audio")
                            || mc.starts_with("Audio/Source")
                            || mc.starts_with("Audio/Sink")
                        {
                            // Sink might be relevant for loopback
                            // Check if this application (by PID if available, or name) is already listed
                            let mut apps_guard = apps_clone_registry.lock().unwrap();
                            let already_exists =
                                apps_guard.iter().any(|app: &LinuxApplicationInfo| {
                                    (app_pid.is_some() && app.process_id == app_pid)
                                        || app.name.as_deref() == Some(name.as_str())
                                });

                            if !already_exists {
                                let app_info = LinuxApplicationInfo {
                                    process_id: app_pid,
                                    name: Some(name.clone()),
                                    executable_path: app_binary,
                                    pipewire_node_id: Some(global.id),
                                    stream_description: node_name.or_else(|| Some(name.clone())),
                                    pulseaudio_sink_input_index: None, // Not applicable for PipeWire enumeration
                                };
                                apps_guard.push(app_info);
                            }
                        }
                    }
                }
            }
        })
        .removed(move |_id| {
            // Optionally handle nodes being removed if enumeration is long-running
            // For a one-shot, this might not be critical
        })
        .register()
        .map_err(listener_builder_error_to_capture_error)?;

    // Sync with PipeWire to ensure all current globals are processed by the listener
    let pending = Rc::new(std::cell::Cell::new(None));
    let mainloop_rc = mainloop.clone(); // Clone for the callback

    let callback_pending = pending.clone();
    let _sync_callback = core
        .sync(0, move |seq| {
            if let Some(p) = callback_pending.get() {
                if p == seq {
                    mainloop_rc.quit();
                    listener_done.set(true);
                }
            }
        })
        .map_err(pw_error_to_capture_error)?;

    pending.set(Some(0)); // Set the sequence number we are waiting for

    // Run the mainloop until quit (signaled by sync callback)
    // This gives time for the registry listener to receive global events.
    // A timeout might be good practice here in a real application.
    let mut iterations = 0;
    let max_iterations = 500; // Timeout after ~500ms if mainloop_quit isn't called (1ms per iter)

    while !done.get() && iterations < max_iterations {
        mainloop
            .iterate(true) // true for block = wait for event
            .map_err(|e| AudioError::BackendError(format!("Mainloop iteration failed: {}", e)))?;
        iterations += 1;
        // Small sleep to prevent busy-waiting if iterate doesn't block as expected or no events
        // std::thread::sleep(std::time::Duration::from_millis(1));
    }

    if iterations >= max_iterations && !done.get() {
        return Err(AudioError::Timeout(
            "PipeWire enumeration timed out waiting for sync.".to_string(),
        ));
    }

    // Listener goes out of scope here and is dropped, cleaning itself up.
    // Or explicitly: drop(registry_listener);

    // Retrieve the collected applications
    let final_apps = Arc::try_unwrap(applications)
        .map_err(|_e| {
            AudioError::BackendError("Failed to unwrap Arc for applications list".to_string())
        })?
        .into_inner()
        .map_err(|_e| {
            AudioError::BackendError("Failed to get applications from Mutex".to_string())
        })?;

    Ok(final_apps)
}

// Example of how one might get NodeInfo if binding directly (more complex for enumeration)
#[allow(dead_code)]
fn get_node_info_example(
    registry: &pipewire::Registry,
    node_id: u32,
    mainloop: &pipewire::MainLoop, // Mainloop needed for node listener
) -> Result<pipewire::node::NodeInfo, AudioError> {
    let node_info_arc = Arc::new(Mutex::new(None));
    let done = Rc::new(std::cell::Cell::new(false));

    let node = registry
        .bind::<pipewire::node::Node>(node_id)
        .map_err(pw_error_to_capture_error)?;

    let node_info_clone = node_info_arc.clone();
    let listener_done = done.clone();
    let mainloop_clone = mainloop.clone();

    let _listener = node
        .add_listener_local()
        .info(move |info| {
            let mut guard = node_info_clone.lock().unwrap();
            *guard = Some(info.clone());
            listener_done.set(true);
            mainloop_clone.quit(); // Quit mainloop once info is received
        })
        .register()
        .map_err(listener_builder_error_to_capture_error)?;

    // Sync to ensure info event is processed if node is already active
    // This part is tricky; often info is emitted immediately or after a short delay.
    // For a robust solution, you'd run the mainloop.
    let mut iterations = 0;
    while !done.get() && iterations < 100 {
        // Timeout
        mainloop
            .iterate(false)
            .map_err(|e| AudioError::BackendError(format!("Mainloop iteration failed: {}", e)))?;
        iterations += 1;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    if !done.get() {
        return Err(AudioError::Timeout(format!(
            "Timeout getting info for node {}",
            node_id
        )));
    }

    let info_opt = node_info_arc.lock().unwrap().take();
    info_opt.ok_or_else(|| {
        AudioError::CaptureError(format!("NodeInfo not received for node {}", node_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Assuming PipewireCoreContext is available from super (linux.rs)
    // and that it can be instantiated for testing.
    // If PipewireCoreContext::new() is not public or suitable, this test needs adjustment.
    // For now, we assume `super::PipewireCoreContext::new()` is a valid way to get an instance.
    // This might require `PipewireCoreContext::new()` to be `pub(crate)` or `pub`.

    // This test would typically require a running PipeWire session.
    #[test]
    #[ignore] // Ignored because it requires a running PipeWire instance and might be flaky in CI
    fn test_enumerate_pipewire_applications() {
        // This test requires a running PipeWire server.
        // You might need to spawn some audio-playing applications for it to find something.
        // e.g., `paplay /usr/share/sounds/freedesktop/stereo/audio-volume-change.oga`
        // or play a YouTube video in a browser.

        // Initialize PipeWire globally once for tests in this module, if not handled by PipewireCoreContext itself.
        // This depends on how PipewireCoreContext manages global init.
        // For now, assuming PipewireCoreContext::new() handles necessary global init or it's done elsewhere.
        // pipewire::init(); // If needed and not handled by PipewireCoreContext::new()

        match crate::audio::linux::PipewireCoreContext::new() {
            // Use the actual path to PipewireCoreContext
            Ok(core_context) => {
                let apps = enumerate_audio_applications_pipewire(&core_context);
                println!("PipeWire enumeration result: {:?}", apps);
                assert!(
                    apps.is_ok(),
                    "Enumeration should not fail catastrophically. Error: {:?}",
                    apps.err()
                );

                if let Ok(app_vec) = apps {
                    println!("Found applications: {:#?}", app_vec);
                    // If you have a known application playing audio, you could assert its presence.
                    // For example, if firefox is playing:
                    // if app_vec.iter().any(|app| app.name.as_deref() == Some("Firefox")) {
                    //     println!("Found Firefox playing audio.");
                    // } else {
                    //     println!("Firefox not found or not playing audio during test.");
                    // }
                }
            }
            Err(e) => {
                eprintln!("Failed to initialize PipewireCoreContext for test: {:?}", e);
                // If PipeWire is not running, this might be an acceptable failure for the test.
                // Consider if this should panic or pass if backend init fails.
                if format!("{}", e).contains("Failed to connect to PipeWire Core")
                    || format!("{}", e).contains("No such file or directory")
                {
                    println!("PipeWire server not available or not found, skipping enumeration test logic. Error: {}", e);
                } else {
                    panic!(
                        "PipewireCoreContext initialization failed unexpectedly: {:?}",
                        e
                    );
                }
            }
        }
        // pipewire::deinit(); // If init was called manually
    }
}

// --- Application-Specific Capture (PipeWire Monitor Stream) ---

/// PipeWire-specific application capture using monitor streams
/// Based on research from tsowell/wiremix src/wirehose/stream.rs
pub struct PipeWireApplicationCapture {
    node_id: Option<u32>,
    node_serial: Option<String>,
    app_selector: ApplicationSelector,
    stream: Option<pipewire::stream::Stream>,
    is_capturing: std::sync::atomic::AtomicBool,
}

/// Selector for targeting specific applications in PipeWire
#[derive(Debug, Clone)]
pub enum ApplicationSelector {
    /// Target by process ID
    ProcessId(u32),
    /// Target by application name (application.name property)
    ApplicationName(String),
    /// Target by PipeWire node ID/serial
    NodeId(u32),
    /// Target by node serial string
    NodeSerial(String),
}

impl PipeWireApplicationCapture {
    /// Create a new application capture instance
    ///
    /// # Arguments
    /// * `selector` - How to identify the target application
    ///
    /// # Example
    /// ```rust,no_run
    /// use rust_crossplat_audio_capture::audio::linux::pipewire::{PipeWireApplicationCapture, ApplicationSelector};
    ///
    /// let capture = PipeWireApplicationCapture::new(ApplicationSelector::ProcessId(1234));
    /// ```
    pub fn new(selector: ApplicationSelector) -> Self {
        Self {
            node_id: None,
            node_serial: None,
            app_selector: selector,
            stream: None,
            is_capturing: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Discover and resolve the target application to a PipeWire node
    ///
    /// # Implementation Notes
    /// - Enumerates PipeWire nodes and matches against selector criteria
    /// - Looks for media.class = "Stream/Output/Audio" (playback) or "Stream/Input/Audio" (recording)
    /// - Matches application.name, application.process.id, or node properties
    pub fn discover_target_node(&mut self) -> Result<u32, Box<dyn std::error::Error>> {
        use pipewire::{MainLoop, Context, Core, Registry};
        use std::sync::{Arc, Mutex};
        use std::collections::HashMap;

        let mainloop = MainLoop::new()?;
        let context = Context::new(&mainloop)?;
        let core = context.connect(None)?;
        let registry = core.get_registry()?;

        // Shared state for collecting nodes
        let found_nodes = Arc::new(Mutex::new(HashMap::<u32, NodeInfo>::new()));
        let found_nodes_clone = found_nodes.clone();

        // Node information structure
        #[derive(Debug, Clone)]
        struct NodeInfo {
            id: u32,
            media_class: Option<String>,
            app_name: Option<String>,
            app_pid: Option<u32>,
            node_name: Option<String>,
        }

        // Registry listener to collect node information
        let _listener = registry
            .add_listener_local()
            .global(move |global| {
                if global.type_ == pipewire::types::ObjectType::Node {
                    if let Some(props) = &global.props {
                        let mut node_info = NodeInfo {
                            id: global.id,
                            media_class: props.get("media.class").map(|s| s.to_string()),
                            app_name: props.get("application.name").map(|s| s.to_string()),
                            app_pid: props.get("application.process.id")
                                .and_then(|s| s.parse().ok()),
                            node_name: props.get("node.name").map(|s| s.to_string()),
                        };

                        // Only interested in audio stream nodes
                        if let Some(ref media_class) = node_info.media_class {
                            if media_class == "Stream/Output/Audio" || media_class == "Stream/Input/Audio" {
                                found_nodes_clone.lock().unwrap().insert(global.id, node_info);
                            }
                        }
                    }
                }
            })
            .register();

        // Run the main loop briefly to collect nodes
        for _ in 0..10 {
            mainloop.iterate(std::time::Duration::from_millis(10));
        }

        // Find matching node based on selector
        let nodes = found_nodes.lock().unwrap();
        let target_node_id = match &self.app_selector {
            ApplicationSelector::ProcessId(pid) => {
                nodes.values()
                    .find(|node| node.app_pid == Some(*pid))
                    .map(|node| node.id)
            }
            ApplicationSelector::ApplicationName(name) => {
                nodes.values()
                    .find(|node| {
                        node.app_name.as_ref()
                            .map_or(false, |app_name| app_name.contains(name))
                    })
                    .map(|node| node.id)
            }
            ApplicationSelector::NodeId(id) => {
                if nodes.contains_key(id) {
                    Some(*id)
                } else {
                    None
                }
            }
            ApplicationSelector::NodeSerial(serial) => {
                // For now, treat serial as node ID
                serial.parse().ok().and_then(|id| {
                    if nodes.contains_key(&id) {
                        Some(id)
                    } else {
                        None
                    }
                })
            }
        };

        if let Some(node_id) = target_node_id {
            self.node_id = Some(node_id);
            self.node_serial = Some(node_id.to_string());
            Ok(node_id)
        } else {
            Err("Target application node not found".into())
        }
    }

    /// Create a monitor stream targeting the discovered node
    ///
    /// # Implementation Notes
    /// - Creates Stream with TARGET_OBJECT = node serial
    /// - Sets STREAM_MONITOR = "true" for non-invasive monitoring
    /// - Optionally sets STREAM_CAPTURE_SINK = "true" for sink monitoring
    /// - Negotiates format (prefer F32LE interleaved)
    pub fn create_monitor_stream(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        use pipewire::{MainLoop, Context, Core, Stream, stream::StreamFlags};
        use pipewire::keys;
        use libspa::{param::ParamType, utils::Direction, pod::Pod};
        use std::collections::HashMap;

        let node_serial = self.node_serial.as_ref()
            .ok_or("Node serial not set - call discover_target_node first")?;

        let mainloop = MainLoop::new()?;
        let context = Context::new(&mainloop)?;
        let core = context.connect(None)?;

        // Create stream properties for monitoring
        let mut props = HashMap::new();
        props.insert(keys::TARGET_OBJECT.to_string(), node_serial.clone());
        props.insert(keys::STREAM_MONITOR.to_string(), "true".to_string());
        props.insert(keys::NODE_NAME.to_string(), "app-capture-monitor".to_string());

        // Create the stream
        let stream = Stream::new(&core, "app-capture-monitor", props)?;

        // Set up format parameters (Float32 LE, stereo, 48kHz)
        let mut audio_info = libspa::param::audio::AudioInfoRaw::new();
        audio_info.set_format(libspa::param::audio::AudioFormat::F32LE);
        audio_info.set_channels(2);
        audio_info.set_rate(48000);

        let obj = libspa::pod::object!(
            libspa::utils::SpaTypes::ObjectParamFormat,
            ParamType::EnumFormat,
            audio_info
        );

        let mut params = [Pod::from_bytes(&obj.serialize()?)?];

        // Connect the stream with monitor flags
        stream.connect(
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )?;

        self.stream = Some(stream);
        Ok(())
    }

    /// Start capturing audio from the target application
    ///
    /// # Implementation Notes
    /// - Sets up process callback to dequeue buffers
    /// - Reads interleaved f32 samples from buffer data
    /// - Calls user callback with PCM frames
    pub fn start_capture<F>(&mut self, callback: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        use pipewire::MainLoop;
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::Duration;

        let stream = self.stream.as_ref()
            .ok_or("Monitor stream not created - call create_monitor_stream first")?;

        self.is_capturing.store(true, std::sync::atomic::Ordering::SeqCst);

        let callback = Arc::new(Mutex::new(callback));
        let is_capturing = self.is_capturing.clone();

        // Set up stream listeners
        let callback_clone = callback.clone();
        let _listener = stream
            .add_listener_local()
            .process(move |stream| {
                // Dequeue buffer and process audio data
                if let Some(mut buffer) = stream.dequeue_buffer() {
                    let datas = buffer.datas_mut();
                    if !datas.is_empty() {
                        let data = &mut datas[0];

                        if let Some(chunk) = data.chunk() {
                            let size = chunk.size() as usize;
                            if size > 0 {
                                if let Some(raw_data) = data.data() {
                                    // Assume F32LE format, 2 channels
                                    let sample_count = size / std::mem::size_of::<f32>();
                                    let samples = unsafe {
                                        std::slice::from_raw_parts(
                                            raw_data.as_ptr() as *const f32,
                                            sample_count,
                                        )
                                    };

                                    // Call the user callback with the samples
                                    if let Ok(cb) = callback_clone.lock() {
                                        cb(samples);
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .register();

        // Run the main loop in a separate thread
        let mainloop = MainLoop::new()?;
        let mainloop_clone = mainloop.clone();

        thread::spawn(move || {
            while is_capturing.load(std::sync::atomic::Ordering::SeqCst) {
                mainloop_clone.iterate(Duration::from_millis(10));
            }
        });

        Ok(())
    }

    /// Stop capturing audio
    pub fn stop_capture(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.is_capturing.store(false, std::sync::atomic::Ordering::SeqCst);

        // Disconnect and clean up the stream
        if let Some(stream) = self.stream.take() {
            stream.disconnect()?;
        }

        // Clear node information
        self.node_id = None;
        self.node_serial = None;

        Ok(())
    }

    /// Check if currently capturing
    pub fn is_capturing(&self) -> bool {
        self.is_capturing.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// List available applications with audio streams
    ///
    /// # Returns
    /// Vector of LinuxApplicationInfo for apps currently producing audio
    pub fn list_audio_applications() -> Result<Vec<LinuxApplicationInfo>, Box<dyn std::error::Error>> {
        use pipewire::{MainLoop, Context, Core, Registry};
        use std::sync::{Arc, Mutex};
        use std::collections::HashMap;

        #[derive(Debug, Clone)]
        pub struct LinuxApplicationInfo {
            pub node_id: u32,
            pub app_name: Option<String>,
            pub process_id: Option<u32>,
            pub media_class: String,
            pub node_name: Option<String>,
        }

        let mainloop = MainLoop::new()?;
        let context = Context::new(&mainloop)?;
        let core = context.connect(None)?;
        let registry = core.get_registry()?;

        let applications = Arc::new(Mutex::new(Vec::<LinuxApplicationInfo>::new()));
        let applications_clone = applications.clone();

        // Registry listener to collect application nodes
        let _listener = registry
            .add_listener_local()
            .global(move |global| {
                if global.type_ == pipewire::types::ObjectType::Node {
                    if let Some(props) = &global.props {
                        if let Some(media_class) = props.get("media.class") {
                            // Only include audio stream nodes
                            if media_class == "Stream/Output/Audio" || media_class == "Stream/Input/Audio" {
                                let app_info = LinuxApplicationInfo {
                                    node_id: global.id,
                                    app_name: props.get("application.name").map(|s| s.to_string()),
                                    process_id: props.get("application.process.id")
                                        .and_then(|s| s.parse().ok()),
                                    media_class: media_class.to_string(),
                                    node_name: props.get("node.name").map(|s| s.to_string()),
                                };

                                applications_clone.lock().unwrap().push(app_info);
                            }
                        }
                    }
                }
            })
            .register();

        // Run the main loop briefly to collect applications
        for _ in 0..20 {
            mainloop.iterate(std::time::Duration::from_millis(10));
        }

        let apps = applications.lock().unwrap().clone();
        Ok(apps)
    }
}

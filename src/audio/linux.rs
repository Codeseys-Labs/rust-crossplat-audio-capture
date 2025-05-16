//! Linux-specific audio capture backend using PipeWire.
#![cfg(target_os = "linux")]

use crate::core::config::StreamConfig;
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioBuffer, AudioDevice, AudioStream, CapturingStream, DeviceEnumerator, DeviceKind,
    StreamDataCallback,
};
use crate::{AudioConfig, AudioFormat}; // AudioConfig & AudioFormat are re-exported from lib.rs

// TODO: Remove these once the actual PipeWire logic is integrated with the new traits.
// These are placeholders from the old structure.
use std::{
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use pipewire::spa::utils::Direction as PwDirection;
use pipewire::{
    self,
    channel,
    context::Context as PwContext,
    core::Core,
    main_loop::MainLoop,
    properties::properties,
    registry::Registry,
    spa,
    spa::pod::{Object, Pod},
    // spa::utils::Direction, // This might conflict with a new Direction if defined
    stream::{Stream as PwStream, StreamFlags},
}; // Alias to avoid conflict

use super::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};

// --- New Skeleton Implementations ---

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinuxDeviceId(String); // Example: Use a String for now

pub struct LinuxAudioDevice {
    id: LinuxDeviceId,
    name: String,
    kind: DeviceKind,
    // TODO: Add other necessary fields, e.g., PipeWire node ID
}

impl AudioDevice for LinuxAudioDevice {
    type DeviceId = LinuxDeviceId;

    fn get_id(&self) -> Self::DeviceId {
        println!("TODO: LinuxAudioDevice::get_id()");
        self.id.clone()
    }

    fn get_name(&self) -> String {
        println!("TODO: LinuxAudioDevice::get_name()");
        self.name.clone()
    }

    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        println!("TODO: LinuxAudioDevice::get_supported_formats()");
        todo!()
    }

    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        println!("TODO: LinuxAudioDevice::get_default_format()");
        todo!()
    }

    fn is_input(&self) -> bool {
        println!("TODO: LinuxAudioDevice::is_input()");
        self.kind == DeviceKind::Input
    }

    fn is_output(&self) -> bool {
        println!("TODO: LinuxAudioDevice::is_output()");
        self.kind == DeviceKind::Output
    }

    fn is_active(&self) -> bool {
        println!("TODO: LinuxAudioDevice::is_active()");
        // TODO: Implement actual status check
        false
    }

    fn create_stream(
        &self,
        config: StreamConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>> {
        println!(
            "TODO: LinuxAudioDevice::create_stream(config: {:?})",
            config
        );
        // For now, let's return a new, unconfigured LinuxAudioStream.
        // In a real implementation, this would be configured based on `self` and `config`.
        Ok(Box::new(LinuxAudioStream {
            config: Some(config),
        }))
    }
}

pub struct LinuxDeviceEnumerator;

impl DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    fn enumerate_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: LinuxDeviceEnumerator::enumerate_devices()");
        todo!()
    }

    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Self::Device> {
        println!(
            "TODO: LinuxDeviceEnumerator::get_default_device({:?})",
            kind
        );
        todo!()
    }

    fn get_input_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: LinuxDeviceEnumerator::get_input_devices()");
        todo!()
    }

    fn get_output_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: LinuxDeviceEnumerator::get_output_devices()");
        todo!()
    }

    fn get_device_by_id(
        &self,
        id: &<Self::Device as AudioDevice>::DeviceId,
    ) -> AudioResult<Self::Device> {
        println!("TODO: LinuxDeviceEnumerator::get_device_by_id({:?})", id);
        todo!()
    }
}

pub struct LinuxAudioStream {
    // TODO: Add fields specific to a Linux audio stream (e.g., PipeWire stream, buffer, config)
    config: Option<StreamConfig>,
}

impl AudioStream for LinuxAudioStream {
    type Config = StreamConfig;
    type Device = LinuxAudioDevice;

    fn open(&mut self, device: &Self::Device, config: Self::Config) -> AudioResult<()> {
        println!(
            "TODO: LinuxAudioStream::open(device_id: {:?}, config: {:?})",
            device.get_id(),
            config
        );
        self.config = Some(config);
        todo!()
    }

    fn start(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::start()");
        todo!()
    }

    fn pause(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::pause()");
        todo!()
    }

    fn resume(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::resume()");
        todo!()
    }

    fn stop(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::stop()");
        todo!()
    }

    fn close(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::close()");
        self.config = None;
        todo!()
    }

    fn set_format(&mut self, format: &AudioFormat) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::set_format({:?})", format);
        todo!()
    }

    fn set_callback(&mut self, _callback: StreamDataCallback) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream::set_callback()");
        todo!()
    }

    fn is_running(&self) -> bool {
        println!("TODO: LinuxAudioStream::is_running()");
        false
    }

    fn get_latency_frames(&self) -> AudioResult<u64> {
        println!("TODO: LinuxAudioStream::get_latency_frames()");
        todo!()
    }

    fn get_current_format(&self) -> AudioResult<AudioFormat> {
        println!("TODO: LinuxAudioStream::get_current_format()");
        todo!()
    }
}

impl CapturingStream for LinuxAudioStream {
    fn start(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream (CapturingStream)::start()");
        todo!()
    }

    fn stop(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream (CapturingStream)::stop()");
        todo!()
    }

    fn close(&mut self) -> AudioResult<()> {
        println!("TODO: LinuxAudioStream (CapturingStream)::close()");
        todo!()
    }

    fn is_running(&self) -> bool {
        println!("TODO: LinuxAudioStream (CapturingStream)::is_running()");
        false
    }

    fn read_chunk(&mut self, timeout_ms: Option<u32>) -> AudioResult<Option<Box<dyn AudioBuffer>>> {
        println!(
            "TODO: LinuxAudioStream (CapturingStream)::read_chunk(timeout_ms: {:?})",
            timeout_ms
        );
        todo!()
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

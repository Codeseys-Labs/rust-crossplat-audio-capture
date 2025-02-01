use std::{
    ffi::CString,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use libpulse_binding::{
    self as pulse,
    context::{Context, FlagSet as ContextFlagSet},
    mainloop::threaded::Mainloop,
    proplist::Proplist,
    stream::{self, Stream},
};

use pipewire::{
    self,
    context::Context as PwContext,
    core::Core,
    main_loop::MainLoop,
    properties::properties,
    registry::{GlobalObject, Registry},
    spa::pod::Pod,
    spa::type_info::Direction,
    stream::{Stream as PwStream, StreamFlags, StreamState},
};

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError, AudioFormat,
};

pub struct PulseAudioBackend {
    mainloop: Arc<Mainloop>,
    context: Arc<Context>,
}

impl PulseAudioBackend {
    pub fn new() -> Result<Self, AudioError> {
        // Create a new property list
        let mut proplist = Proplist::new().map_err(|e| {
            AudioError::InitializationFailed(format!("Failed to create proplist: {}", e))
        })?;
        proplist
            .set_str(
                pulse::proplist::properties::APPLICATION_NAME,
                "Rust Audio Capture",
            )
            .map_err(|e| {
                AudioError::InitializationFailed(
                    e.to_string().expect("Failed to convert error to string"),
                )
            })?;

        // Create a mainloop
        let mainloop = Mainloop::new().ok_or_else(|| {
            AudioError::InitializationFailed("Failed to create PulseAudio mainloop".into())
        })?;

        // Create a new context
        let context = Context::new_with_proplist(&mainloop, "RustAudioCapture", &proplist)
            .ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create PulseAudio context".into())
            })?;

        // Connect the context
        context
            .connect(None, ContextFlagSet::NOFLAGS, None)
            .map_err(|e| {
                AudioError::InitializationFailed(
                    e.to_string().expect("Failed to convert error to string"),
                )
            })?;

        // Start the mainloop
        mainloop.start().map_err(|e| {
            AudioError::InitializationFailed(
                e.to_string().expect("Failed to convert error to string"),
            )
        })?;

        // Wait for context to be ready
        loop {
            match context.get_state() {
                pulse::context::State::Ready => break,
                pulse::context::State::Failed | pulse::context::State::Terminated => {
                    return Err(AudioError::InitializationFailed(
                        "PulseAudio context failed or terminated".into(),
                    ))
                }
                _ => thread::sleep(Duration::from_millis(10)),
            }
        }

        Ok(Self {
            mainloop: Arc::new(mainloop),
            context: Arc::new(context),
        })
    }

    pub fn is_available() -> bool {
        // Try to create a simple connection to check availability
        if let Ok(mainloop) = Mainloop::new() {
            if let Some(context) = Context::new(&mainloop, "TestConnection") {
                if context.connect(None, ContextFlagSet::NOFLAGS, None).is_ok() {
                    return true;
                }
            }
        }
        false
    }
}

// Mark PulseAudioBackend as Send to satisfy AudioCaptureBackend trait
unsafe impl Send for PulseAudioBackend {}

impl AudioCaptureBackend for PulseAudioBackend {
    fn name(&self) -> &'static str {
        "PulseAudio"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let apps = Arc::new(Mutex::new(Vec::new()));
        let apps_clone = Arc::clone(&apps);

        // Add system-wide audio capture option
        apps_clone.lock().unwrap().push(AudioApplication {
            name: "System".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        let op = self
            .context
            .introspect()
            .get_sink_input_info_list(move |list| {
                if let Some(info) = list {
                    let proplist = info.proplist();
                    let app_name = proplist
                        .get_str(pulse::proplist::properties::APPLICATION_NAME)
                        .unwrap_or("Unknown");
                    let process_id = proplist
                        .get_str(pulse::proplist::properties::APPLICATION_PROCESS_ID)
                        .and_then(|pid| pid.parse().ok())
                        .unwrap_or(0);

                    let app = AudioApplication {
                        name: app_name.to_string(),
                        id: info.index.to_string(),
                        executable_name: proplist
                            .get_str(pulse::proplist::properties::APPLICATION_PROCESS_BINARY)
                            .unwrap_or("unknown")
                            .to_string(),
                        pid: process_id,
                    };

                    apps_clone.lock().unwrap().push(app);
                }
            });

        // Wait for the operation to complete
        loop {
            match op.get_state() {
                pulse::operation::State::Done => break,
                pulse::operation::State::Running => thread::sleep(Duration::from_millis(10)),
                pulse::operation::State::Cancelled => {
                    return Err(AudioError::CaptureError(
                        "Operation cancelled while listing applications".into(),
                    ))
                }
            }
        }

        let mut apps = Arc::try_unwrap(apps)
            .unwrap()
            .into_inner()
            .unwrap_or_default();

        // Sort applications: System first, then by name
        apps.sort_by(|a, b| {
            if a.name == "System" {
                std::cmp::Ordering::Less
            } else if b.name == "System" {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let stream = PulseAudioStream::new(
            Arc::clone(&self.context),
            Arc::clone(&self.mainloop),
            app,
            config,
        )?;
        Ok(Box::new(stream))
    }
}

pub struct PulseAudioStream {
    stream: Stream,
    _mainloop: Arc<Mainloop>,
    _context: Arc<Context>,
    config: AudioConfig,
}

// Mark PulseAudioStream as Send to satisfy AudioCaptureStream trait
unsafe impl Send for PulseAudioStream {}

impl PulseAudioStream {
    fn new(
        context: Arc<Context>,
        mainloop: Arc<Mainloop>,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Self, AudioError> {
        let ss = pulse::sample::Spec {
            format: match config.format {
                AudioFormat::F32LE => pulse::sample::Format::FLOAT32NE,
                AudioFormat::S16LE => pulse::sample::Format::S16NE,
                AudioFormat::S32LE => pulse::sample::Format::S32NE,
            },
            channels: config.channels,
            rate: config.sample_rate,
        };

        if !ss.is_valid() {
            return Err(AudioError::InvalidFormat("Invalid sample format".into()));
        }

        let stream_name = CString::new(format!("capture_{}", app.name)).map_err(|e| {
            AudioError::InitializationFailed(
                e.to_string().expect("Failed to convert error to string"),
            )
        })?;

        let stream = Stream::new(
            &context,
            &stream_name,
            &ss,
            None, // Use default channel map
        )
        .ok_or_else(|| {
            AudioError::InitializationFailed("Failed to create PulseAudio stream".into())
        })?;

        // Monitor the sink input of the target application
        let attr = stream::BufferAttr {
            maxlength: std::u32::MAX,
            fragsize: 1024, // Read in 1KB chunks
            ..Default::default()
        };

        let target = if app.name == "System" {
            "sink.default".to_string()
        } else {
            format!("sink_input.{}", app.id)
        };

        stream
            .connect_record(Some(&target), Some(&attr), stream::FlagSet::ADJUST_LATENCY)
            .map_err(|e| {
                AudioError::CaptureError(e.to_string().expect("Failed to convert error to string"))
            })?;

        Ok(Self {
            stream,
            _mainloop: mainloop,
            _context: context,
            config,
        })
    }
}

impl AudioCaptureStream for PulseAudioStream {
    fn start(&mut self) -> Result<(), AudioError> {
        self.stream.cork(None).map_err(|e| {
            AudioError::CaptureError(e.to_string().expect("Failed to convert error to string"))
        })
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream.cork(None).map_err(|e| {
            AudioError::CaptureError(e.to_string().expect("Failed to convert error to string"))
        })
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut bytes_read = 0;
        while bytes_read < buffer.len() {
            match self.stream.peek() {
                Ok(data) => {
                    if data.is_empty() {
                        break;
                    }
                    let to_copy = std::cmp::min(buffer.len() - bytes_read, data.len());
                    buffer[bytes_read..bytes_read + to_copy].copy_from_slice(&data[..to_copy]);
                    bytes_read += to_copy;
                    self.stream.discard().map_err(|e| {
                        AudioError::CaptureError(
                            e.to_string().expect("Failed to convert error to string"),
                        )
                    })?;
                }
                Err(e) => {
                    return Err(AudioError::CaptureError(
                        e.to_string().expect("Failed to convert error to string"),
                    ))
                }
            }
        }
        Ok(bytes_read)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

pub struct PipeWireBackend {
    context: PwContext,
    core: Core,
    main_loop: MainLoop,
    registry: Registry,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        pipewire::init();

        let main_loop = MainLoop::new().map_err(|e| {
            AudioError::InitializationFailed(format!("Failed to create PipeWire main loop: {}", e))
        })?;

        let context = PwContext::new(&main_loop).map_err(|e| {
            AudioError::InitializationFailed(format!("Failed to create PipeWire context: {}", e))
        })?;

        let core = context.connect(None).map_err(|e| {
            AudioError::InitializationFailed(format!("Failed to connect to PipeWire: {}", e))
        })?;

        let registry = core.get_registry().map_err(|e| {
            AudioError::InitializationFailed(format!("Failed to get PipeWire registry: {}", e))
        })?;

        Ok(Self {
            context,
            core,
            main_loop,
            registry,
        })
    }

    pub fn is_available() -> bool {
        // Try to initialize PipeWire and create a connection
        pipewire::init();
        if let Ok(main_loop) = MainLoop::new() {
            if let Ok(context) = PwContext::new(&main_loop) {
                if let Ok(_) = context.connect(None) {
                    return true;
                }
            }
        }
        false
    }
}

impl AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let mut apps = Vec::new();

        // Add system-wide audio capture option
        apps.push(AudioApplication {
            name: "System".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        // Listen for nodes in the PipeWire graph
        let apps_clone = Arc::new(Mutex::new(&mut apps));
        self.registry.add_listener_local().global({
            let apps_clone = Arc::clone(&apps_clone);
            move |global: GlobalObject| {
                if let Some(props) = global.props.as_ref() {
                    let media_class = props.get("media.class").unwrap_or("");

                    // Add application-specific streams
                    if media_class == "Stream/Input/Audio" || media_class == "Stream/Output/Audio" {
                        let app_name = props
                            .get("application.name")
                            .or_else(|| props.get("media.name"))
                            .unwrap_or("Unknown")
                            .to_string();

                        let pid = props
                            .get("application.process.id")
                            .and_then(|pid| pid.parse().ok())
                            .unwrap_or(0);

                        // Only add if we have a valid PID (except for system audio)
                        if pid > 0 || app_name == "System" {
                            let app = AudioApplication {
                                name: app_name,
                                id: global.id.to_string(),
                                executable_name: props
                                    .get("application.process.binary")
                                    .unwrap_or("unknown")
                                    .to_string(),
                                pid,
                            };

                            let mut apps = apps_clone.lock().unwrap();
                            // Avoid duplicates
                            if !apps
                                .iter()
                                .any(|existing| existing.pid == app.pid && existing.pid != 0)
                            {
                                apps.push(app);
                            }
                        }
                    }
                }
            }
        });

        // Process events to populate the applications list
        for _ in 0..10 {
            // Give some time for events to arrive
            self.main_loop.iterate(Duration::from_millis(100));
        }

        // Sort applications: System first, then by name
        apps.sort_by(|a, b| {
            if a.name == "System" {
                std::cmp::Ordering::Less
            } else if b.name == "System" {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        // Create stream properties
        let props = if app.name == "System" {
            properties! {
                "media.class" => "Audio/Source",
                "stream.capture.sink" => "true",
                "audio.position" => if config.channels == 1 { "MONO" } else { "FL,FR" },
                // Target the default monitor/sink for system-wide capture
                "target.object" => "default.monitor",
                "stream.is_monitor" => "true",
            }
        } else {
            // Process-specific capture
            properties! {
                "media.class" => "Audio/Source",
                "audio.capture.app" => &app.name,
                "target.object" => &app.id,
                "stream.capture.sink" => "true",
                "audio.position" => if config.channels == 1 { "MONO" } else { "FL,FR" },
                "application.process.id" => app.pid.to_string(),
                "application.name" => &app.name,
                "stream.capture.pid" => app.pid.to_string(),
            }
        };

        // Create the stream
        let stream = PwStream::new(
            &self.core,
            if app.name == "System" {
                "system-audio-capture"
            } else {
                "application-audio-capture"
            },
            props,
        )
        .map_err(|e| {
            AudioError::CaptureError(format!("Failed to create PipeWire stream: {}", e))
        })?;

        Ok(Box::new(PipeWireStream::new(stream, config)))
    }
}

pub struct PipeWireStream {
    stream: PwStream,
    config: AudioConfig,
    buffer: Vec<u8>,
    shared_buffer: Arc<Mutex<Vec<u8>>>,
}

impl PipeWireStream {
    fn new(stream: PwStream, config: AudioConfig) -> Self {
        Self {
            stream,
            config,
            buffer: Vec::with_capacity(16384),
            shared_buffer: Arc::new(Mutex::new(Vec::with_capacity(16384))),
        }
    }
}

impl AudioCaptureStream for PipeWireStream {
    fn start(&mut self) -> Result<(), AudioError> {
        // Configure the stream format based on AudioConfig
        let stream_flags = StreamFlags::AUTOCONNECT | StreamFlags::RT_PROCESS;

        // Set up stream parameters based on config
        let params = Pod::builder()
            .object(
                "Format",
                "audio/raw",
                properties! {
                    "format" => match self.config.format {
                        AudioFormat::F32LE => "F32LE",
                        AudioFormat::S16LE => "S16LE",
                        AudioFormat::S32LE => "S32LE",
                    },
                    "rate" => self.config.sample_rate,
                    "channels" => self.config.channels,
                    "layout" => if self.config.channels == 1 { "mono" } else { "interleaved" },
                },
            )
            .build();

        // Set up process callback
        let buffer_clone = self.shared_buffer.clone();

        self.stream.add_listener_local().process(move |stream| {
            if let Some(mut buffer_guard) = buffer_clone.try_lock() {
                if let Some(input) = stream.input_buffer() {
                    for data in input.datas() {
                        if data.chunk().is_valid() {
                            if let Some(ptr) = data.data() {
                                // Ensure we don't accumulate too much data
                                if buffer_guard.len() > 1_048_576 {
                                    // 1MB limit
                                    buffer_guard.clear();
                                }
                                buffer_guard.extend_from_slice(ptr);
                            }
                        }
                    }
                }
            }
            Ok(())
        });

        // Connect the stream
        self.stream
            .connect(Direction::Input, None, stream_flags, &[params])
            .map_err(|e| {
                AudioError::CaptureError(format!("Failed to start PipeWire stream: {}", e))
            })?;

        // Wait for stream to be ready
        let mut retries = 0;
        while retries < 10 {
            if self.stream.state() == StreamState::Streaming {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
            retries += 1;
        }

        if self.stream.state() != StreamState::Streaming {
            Err(AudioError::CaptureError(
                "Stream failed to start streaming".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream
            .disconnect()
            .map_err(|e| AudioError::CaptureError(format!("Failed to stop PipeWire stream: {}", e)))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut bytes_read = 0;

        // Process any pending data in our internal buffer first
        if !self.buffer.is_empty() {
            let copy_size = std::cmp::min(buffer.len(), self.buffer.len());
            buffer[..copy_size].copy_from_slice(&self.buffer[..copy_size]);
            self.buffer.drain(..copy_size);
            bytes_read = copy_size;
        }

        // Try to get data from the shared buffer
        if bytes_read < buffer.len() {
            if let Ok(mut shared_buf) = self.shared_buffer.try_lock() {
                if !shared_buf.is_empty() {
                    let remaining_space = buffer.len() - bytes_read;
                    let copy_size = std::cmp::min(remaining_space, shared_buf.len());

                    buffer[bytes_read..bytes_read + copy_size]
                        .copy_from_slice(&shared_buf[..copy_size]);

                    // Remove the copied data from shared buffer
                    shared_buf.drain(..copy_size);
                    bytes_read += copy_size;

                    // If we still have data, store it in our local buffer
                    if !shared_buf.is_empty() {
                        self.buffer.extend(shared_buf.drain(..));
                    }
                }
            }
        }

        // If we got no data, wait a bit to avoid busy-waiting
        if bytes_read == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(bytes_read)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

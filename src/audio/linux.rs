use std::{
    ffi::CString,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use libpulse_binding::{
    self as pulse,
    context::{Context, FlagSet as ContextFlagSet},
    def::BufferAttr,
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
    spa::utils::Direction,
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
        let mut proplist = Proplist::new()
            .ok_or_else(|| AudioError::InitializationFailed("Failed to create proplist".into()))?;
        proplist
            .set_str(
                pulse::proplist::properties::APPLICATION_NAME,
                "Rust Audio Capture",
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

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
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Start the mainloop
        mainloop
            .start()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

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
        if let Some(mainloop) = Mainloop::new() {
            if let Some(mut context) = Context::new(&mainloop, "TestConnection") {
                if context.connect(None, ContextFlagSet::NOFLAGS, None).is_ok() {
                    return true;
                }
            }
        }
        false
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let mut apps = Vec::new();

        // Add system-wide audio capture option
        apps.push(AudioApplication {
            name: "System Audio".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        // Get running applications with audio streams
        let context = &self.context;
        let mainloop = &self.mainloop;

        // Wait for the context to be ready
        loop {
            match context.get_state() {
                pulse::context::State::Ready => break,
                pulse::context::State::Failed | pulse::context::State::Terminated => {
                    return Err(AudioError::BackendUnavailable(
                        "PulseAudio context failed".into(),
                    ));
                }
                _ => {
                    mainloop.wait();
                }
            }
        }

        // Get client info
        let mut client_list = Vec::new();
        let (sender, receiver) = std::sync::mpsc::channel();

        context.introspect().get_client_info_list(move |list| {
            if let Some(list) = list {
                for client in list {
                    if let Some(app_name) = client.name.as_ref() {
                        if let Some(process_id) = client.process_id {
                            client_list.push(AudioApplication {
                                name: app_name.to_string(),
                                id: format!("app_{}", process_id),
                                executable_name: app_name.to_string(),
                                pid: process_id,
                            });
                        }
                    }
                }
            }
            sender.send(()).unwrap();
        });

        // Wait for the callback
        mainloop.wait();
        let _ = receiver.recv();

        apps.extend(client_list);
        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let context = Arc::clone(&self.context);
        let mainloop = Arc::clone(&self.mainloop);

        // Create a stream for either system or application audio
        let stream = if app.pid == 0 {
            // System-wide capture
            PulseAudioStream::new_system(context, mainloop, config)?
        } else {
            // Application-specific capture
            PulseAudioStream::new_application(context, mainloop, app, config)?
        };

        Ok(Box::new(stream))
    }
}

// Mark PulseAudioBackend as Send to satisfy AudioCaptureBackend trait
unsafe impl Send for PulseAudioBackend {}

impl AudioCaptureBackend for PulseAudioBackend {
    fn name(&self) -> &'static str {
        "PulseAudio"
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
            channels: config.channels as u8,
            rate: config.sample_rate,
        };

        if !ss.is_valid() {
            return Err(AudioError::InvalidFormat("Invalid sample format".into()));
        }

        let stream_name = CString::new(format!("capture_{}", app.name))
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Create stream properties
        let mut proplist = Proplist::new()
            .ok_or_else(|| AudioError::InitializationFailed("Failed to create proplist".into()))?;
        proplist
            .set_str(
                pulse::proplist::properties::APPLICATION_NAME,
                "Rust Audio Capture",
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let stream = Stream::new_with_proplist(
            &context,
            &stream_name,
            &ss,
            None, // Use default channel map
            &proplist,
        )
        .ok_or_else(|| {
            AudioError::InitializationFailed("Failed to create PulseAudio stream".into())
        })?;

        // Monitor the sink input of the target application
        let attr = BufferAttr {
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
            .map_err(|e| AudioError::CaptureError(e.to_string()))?;

        Ok(Self {
            stream,
            _mainloop: mainloop,
            _context: context,
            config,
        })
    }

    fn new_system(
        context: Arc<Context>,
        mainloop: Arc<Mainloop>,
        config: AudioConfig,
    ) -> Result<Self, AudioError> {
        // ... existing system capture implementation ...
        Ok(Self {
            stream: Stream::new(
                &context,
                "system-audio-capture",
                &pulse::sample::Spec {
                    format: pulse::sample::Format::FLOAT32NE,
                    channels: config.channels as u8,
                    rate: config.sample_rate,
                },
                None,
            )?,
            _mainloop: mainloop,
            _context: context,
            config,
        })
    }

    fn new_application(
        context: Arc<Context>,
        mainloop: Arc<Mainloop>,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Self, AudioError> {
        let spec = pulse::sample::Spec {
            format: match config.format {
                AudioFormat::F32LE => pulse::sample::Format::FLOAT32LE,
                AudioFormat::S16LE => pulse::sample::Format::S16LE,
                AudioFormat::S32LE => pulse::sample::Format::S32LE,
            },
            channels: config.channels as u8,
            rate: config.sample_rate,
        };

        let attrs = BufferAttr {
            maxlength: std::u32::MAX,
            tlength: std::u32::MAX,
            prebuf: 0,
            minreq: std::u32::MAX,
            fragsize: config.buffer_size as u32,
        };

        let stream_name = format!("rsac_capture_{}", app.pid);

        // Create a monitor stream for the application
        let stream = Stream::new(
            &context,
            &stream_name,
            &spec,
            None, // No channel map
        )
        .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Set up stream flags for monitoring
        let flags = stream::FlagSet::DONT_MOVE
            | stream::FlagSet::PEAK_DETECT
            | stream::FlagSet::ADJUST_LATENCY;

        // Get the sink input index for the application
        let mut sink_input_index = None;
        let (sender, receiver) = std::sync::mpsc::channel();

        context.introspect().get_sink_input_info_list(move |list| {
            if let Some(list) = list {
                for input in list {
                    if let Some(client_index) = input.client {
                        if let Some(process_id) = input.proplist.get_str("application.process.id") {
                            if process_id == app.pid.to_string() {
                                sink_input_index = Some(input.index);
                                break;
                            }
                        }
                    }
                }
            }
            sender.send(()).unwrap();
        });

        mainloop.wait();
        let _ = receiver.recv();

        let sink_input = sink_input_index.ok_or_else(|| {
            AudioError::CaptureError("Could not find audio stream for application".into())
        })?;

        // Set up monitor of sink input
        unsafe {
            stream.set_monitor_stream(sink_input);
        }

        // Connect the stream
        stream
            .connect_record(
                None, // Let PA choose the device
                Some(&attrs),
                flags,
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

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
        self.stream
            .uncork(None)
            .map_err(|_| AudioError::CaptureError("Failed to start stream".to_string()))?;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream
            .cork(None)
            .map_err(|_| AudioError::CaptureError("Failed to stop stream".to_string()))?;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut bytes_read = 0;
        while bytes_read < buffer.len() {
            match self.stream.peek() {
                Ok(data) => {
                    let data_slice = data.as_slice();
                    if data_slice.is_empty() {
                        break;
                    }
                    let to_copy = std::cmp::min(buffer.len() - bytes_read, data_slice.len());
                    buffer[bytes_read..bytes_read + to_copy]
                        .copy_from_slice(&data_slice[..to_copy]);
                    bytes_read += to_copy;
                    self.stream.discard().map_err(|_| {
                        AudioError::CaptureError("Failed to discard data".to_string())
                    })?;
                }
                Err(e) => return Err(AudioError::CaptureError(e.to_string())),
            }
        }
        Ok(bytes_read)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

pub struct PipeWireBackend {
    main_loop: Arc<MainLoop>,
    context: Arc<PwContext>,
    core: Arc<Core>,
    registry: Registry,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        pipewire::init();

        let main_loop = MainLoop::new(None).map_err(|e| {
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
            main_loop: Arc::new(main_loop),
            context: Arc::new(context),
            core: Arc::new(core),
            registry,
        })
    }

    pub fn is_available() -> bool {
        pipewire::init();
        if let Ok(main_loop) = MainLoop::new(None) {
            if let Ok(context) = PwContext::new(&main_loop) {
                if context.connect(None).is_ok() {
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
        let apps_clone = Arc::new(Mutex::new(apps));
        let _listener = self.registry.add_listener_local().global({
            let apps_clone = Arc::clone(&apps_clone);
            move |global: GlobalObject<_>| {
                if let Some(props) = global.props {
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
        self.main_loop.iterate(Duration::from_millis(100));

        // Get the apps list back
        let mut apps = Arc::try_unwrap(apps_clone)
            .map_err(|_| AudioError::CaptureError("Failed to unwrap apps".into()))?
            .into_inner()
            .map_err(|_| AudioError::CaptureError("Failed to get inner value".into()))?;

        // Sort applications
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
        let props = if app.name == "System" {
            properties! {
                "media.class" => "Audio/Source",
                "stream.capture.sink" => "true",
                "audio.position" => if config.channels == 1 { "MONO" } else { "FL,FR" },
                "target.object" => "default.monitor",
                "stream.is_monitor" => "true",
            }
        } else {
            properties! {
                "media.class" => "Audio/Source",
                "audio.capture.app" => app.name.as_str(),
                "target.object" => app.id.as_str(),
                "stream.capture.sink" => "true",
                "audio.position" => if config.channels == 1 { "MONO" } else { "FL,FR" },
                "application.process.id" => app.pid.to_string().as_str(),
                "application.name" => app.name.as_str(),
                "stream.capture.pid" => app.pid.to_string().as_str(),
            }
        };

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

        Ok(Box::new(PipeWireStream::new(
            stream,
            config,
            Arc::clone(&self.main_loop),
        )))
    }
}

pub struct PipeWireStream {
    stream: PwStream,
    config: AudioConfig,
    buffer: Arc<Mutex<Vec<u8>>>,
    main_loop: Arc<MainLoop>,
}

impl PipeWireStream {
    fn new(stream: PwStream, config: AudioConfig, main_loop: Arc<MainLoop>) -> Self {
        Self {
            stream,
            config,
            buffer: Arc::new(Mutex::new(Vec::with_capacity(16384))),
            main_loop,
        }
    }
}

impl AudioCaptureStream for PipeWireStream {
    fn start(&mut self) -> Result<(), AudioError> {
        let stream_flags = StreamFlags::AUTOCONNECT | StreamFlags::RT_PROCESS;
        let buffer_clone = Arc::clone(&self.buffer);

        let pod = pipewire::spa::pod::Pod::builder()
            .object(
                "Format",
                "audio/raw",
                properties! {
                    "format" => match self.config.format {
                        AudioFormat::F32LE => "F32LE",
                        AudioFormat::S16LE => "S16LE",
                        AudioFormat::S32LE => "S32LE",
                    },
                    "rate" => self.config.sample_rate.to_string().as_str(),
                    "channels" => self.config.channels.to_string().as_str(),
                },
            )
            .build()
            .map_err(|e| {
                AudioError::CaptureError(format!("Failed to build format parameters: {}", e))
            })?;

        let _listener = self.stream.add_local_listener().process(move |stream| {
            if let Ok(mut buffer_guard) = buffer_clone.try_lock() {
                if let Some(input) = stream.input_buffer() {
                    for data in input.datas() {
                        if data.chunk().is_valid() {
                            if let Some(ptr) = data.data() {
                                if buffer_guard.len() > 1_048_576 {
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

        self.stream
            .connect(Direction::Input, None, stream_flags, &[pod])
            .map_err(|e| AudioError::CaptureError(format!("Failed to connect stream: {}", e)))?;

        // Wait for stream to be ready
        let mut retries = 0;
        while retries < 10 {
            self.main_loop.iterate(Duration::from_millis(10));
            if self.stream.state() == StreamState::Streaming {
                return Ok(());
            }
            retries += 1;
        }

        if self.stream.state() != StreamState::Streaming {
            Err(AudioError::CaptureError("Stream failed to start".into()))
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream
            .disconnect()
            .map_err(|e| AudioError::CaptureError(format!("Failed to stop stream: {}", e)))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        if let Ok(mut shared_buf) = self.buffer.try_lock() {
            if !shared_buf.is_empty() {
                let copy_size = std::cmp::min(buffer.len(), shared_buf.len());
                buffer[..copy_size].copy_from_slice(&shared_buf[..copy_size]);
                shared_buf.drain(..copy_size);
                return Ok(copy_size);
            }
        }

        self.main_loop.iterate(Duration::from_millis(1));
        Ok(0)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

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
    registry::Registry,
    spa::utils::Direction,
    stream::{Stream as PwStream, StreamFlags},
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
            .map_err(|_| {
                AudioError::InitializationFailed("Failed to set application name".into())
            })?;

        // Create a mainloop
        let mut mainloop = Mainloop::new().ok_or_else(|| {
            AudioError::InitializationFailed("Failed to create PulseAudio mainloop".into())
        })?;

        // Create a new context
        let mut context = Context::new_with_proplist(&mainloop, "RustAudioCapture", &proplist)
            .ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create PulseAudio context".into())
            })?;

        // Connect the context
        context
            .connect(None, ContextFlagSet::NOFLAGS, None)
            .map_err(|_| {
                AudioError::InitializationFailed("Failed to connect PulseAudio context".into())
            })?;

        // Start the mainloop
        mainloop.start().map_err(|_| {
            AudioError::InitializationFailed("Failed to start PulseAudio mainloop".into())
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
        let context = Arc::clone(&self.context);
        let mainloop = Arc::clone(&self.mainloop);

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
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        // Get client info
        let (tx, rx) = std::sync::mpsc::channel();
        let apps_clone = Arc::new(Mutex::new(apps));

        context.introspect().get_client_info_list({
            let apps = Arc::clone(&apps_clone);
            move |result| {
                if let pulse::callbacks::ListResult::Item(client) = result {
                    if let Some(app_name) = client.name.as_ref() {
                        if let Some(process_id) = client
                            .proplist
                            .get_str("application.process.id")
                            .and_then(|pid| pid.parse().ok())
                        {
                            let mut apps = apps.lock().unwrap();
                            apps.push(AudioApplication {
                                name: app_name.to_string(),
                                id: format!("app_{}", process_id),
                                executable_name: app_name.to_string(),
                                pid: process_id,
                            });
                        }
                    }
                }
                if let pulse::callbacks::ListResult::End = result {
                    let _ = tx.send(());
                }
            }
        });

        // Wait for the callback
        let _ = rx.recv();

        Ok(Arc::try_unwrap(apps_clone)
            .map_err(|_| AudioError::CaptureError("Failed to unwrap apps".into()))?
            .into_inner()
            .map_err(|_| AudioError::CaptureError("Failed to get inner value".into()))?)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let context = Arc::clone(&self.context);
        let _mainloop = Arc::clone(&self.mainloop);

        // Create a stream for either system or application audio
        let stream = if app.pid == 0 {
            // System-wide capture
            PulseAudioStream::new_system(context, _mainloop, config)?
        } else {
            // Application-specific capture
            PulseAudioStream::new_application(context, _mainloop, app, config)?
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

        // Fix Stream::new_with_proplist call by using a different approach
        // We need a fresh context that we can get a mutable reference to
        let mut new_context =
            Context::new(&mainloop.borrow_mut(), "rsac-context").ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create new context".into())
            })?;

        // Connect and wait for context to be ready
        new_context
            .connect(None, ContextFlagSet::NOFLAGS, None)
            .ok_or_else(|| AudioError::InitializationFailed("Failed to connect context".into()))?;

        mainloop.lock();
        while new_context.get_state() != pulse::context::State::Ready {
            mainloop.wait();
        }
        mainloop.unlock();

        let stream = Stream::new_with_proplist(
            &mut new_context,
            stream_name.to_str().unwrap(),
            &ss,
            None, // Use default channel map
            &mut proplist,
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
        let spec = pulse::sample::Spec {
            format: match config.format {
                AudioFormat::F32LE => pulse::sample::Format::FLOAT32NE,
                AudioFormat::S16LE => pulse::sample::Format::S16NE,
                AudioFormat::S32LE => pulse::sample::Format::S32NE,
            },
            channels: config.channels as u8,
            rate: config.sample_rate,
        };

        if !spec.is_valid() {
            return Err(AudioError::InvalidFormat("Invalid sample format".into()));
        }

        // We need a fresh context that we can get a mutable reference to
        let mut new_context = Context::new(&mainloop.borrow_mut(), "rsac-system-context")
            .ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create new context".into())
            })?;

        // Connect and wait for context to be ready
        new_context
            .connect(None, ContextFlagSet::NOFLAGS, None)
            .ok_or_else(|| AudioError::InitializationFailed("Failed to connect context".into()))?;

        mainloop.lock();
        while new_context.get_state() != pulse::context::State::Ready {
            mainloop.wait();
        }
        mainloop.unlock();

        let stream = Stream::new(&mut new_context, "system-audio-capture", &spec, None)
            .ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create PulseAudio stream".into())
            })?;

        // Connect to the default monitor source
        stream
            .connect_record(
                None,
                Some(&BufferAttr {
                    maxlength: std::u32::MAX,
                    fragsize: 4096, // Use a reasonable default buffer size
                    ..Default::default()
                }),
                stream::FlagSet::ADJUST_LATENCY,
            )
            .map_err(|_| AudioError::CaptureError("Failed to connect stream".into()))?;

        Ok(Self {
            stream,
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
                AudioFormat::F32LE => pulse::sample::Format::FLOAT32NE,
                AudioFormat::S16LE => pulse::sample::Format::S16NE,
                AudioFormat::S32LE => pulse::sample::Format::S32NE,
            },
            channels: config.channels as u8,
            rate: config.sample_rate,
        };

        if !spec.is_valid() {
            return Err(AudioError::InvalidFormat("Invalid sample format".into()));
        }

        let stream_name = format!("rsac_capture_{}", app.pid);

        // We need a fresh context that we can get a mutable reference to
        let mut new_context =
            Context::new(&mainloop.borrow_mut(), "rsac-app-context").ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create new context".into())
            })?;

        // Connect and wait for context to be ready
        new_context
            .connect(None, ContextFlagSet::NOFLAGS, None)
            .ok_or_else(|| AudioError::InitializationFailed("Failed to connect context".into()))?;

        mainloop.lock();
        while new_context.get_state() != pulse::context::State::Ready {
            mainloop.wait();
        }
        mainloop.unlock();

        let stream = Stream::new(&mut new_context, &stream_name, &spec, None).ok_or_else(|| {
            AudioError::InitializationFailed("Failed to create PulseAudio stream".into())
        })?;

        // Find the sink input for the application
        let (tx, rx) = std::sync::mpsc::channel();
        let pid = app.pid;
        let mut sink_input_index = None;

        context.introspect().get_sink_input_info_list({
            let tx = tx.clone();
            move |result| {
                if let pulse::callbacks::ListResult::Item(input) = result {
                    if let Some(client) = input.client {
                        if let Some(process_id) = input.proplist.get_str("application.process.id") {
                            if process_id == pid.to_string() {
                                sink_input_index = Some(input.index);
                            }
                        }
                    }
                }
                if let pulse::callbacks::ListResult::End = result {
                    let _ = tx.send(sink_input_index);
                }
            }
        });

        let sink_input = rx
            .recv()
            .map_err(|_| AudioError::CaptureError("Failed to receive sink input index".into()))?
            .ok_or_else(|| {
                AudioError::CaptureError("Could not find audio stream for application".into())
            })?;

        // Connect to the sink input's monitor source
        stream
            .connect_record(
                Some(&format!("sink-input-{}.monitor", sink_input)),
                Some(&BufferAttr {
                    maxlength: std::u32::MAX,
                    fragsize: 4096, // Use a reasonable default buffer size
                    ..Default::default()
                }),
                stream::FlagSet::ADJUST_LATENCY | stream::FlagSet::DONT_MOVE,
            )
            .map_err(|_| AudioError::CaptureError("Failed to connect stream".into()))?;

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
        let op = self.stream.uncork(None);
        if op.is_null() {
            return Err(AudioError::CaptureError("Failed to start stream".into()));
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        let op = self.stream.cork(None);
        if op.is_null() {
            return Err(AudioError::CaptureError("Failed to stop stream".into()));
        }
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut bytes_read = 0;
        while bytes_read < buffer.len() {
            match self.stream.peek() {
                Ok(data) => {
                    if let pulse::stream::PeekResult::Data(data_slice) = data {
                        if data_slice.is_empty() {
                            break;
                        }
                        let to_copy = std::cmp::min(buffer.len() - bytes_read, data_slice.len());
                        buffer[bytes_read..bytes_read + to_copy]
                            .copy_from_slice(&data_slice[..to_copy]);
                        bytes_read += to_copy;
                        self.stream.discard().map_err(|e| {
                            AudioError::CaptureError(format!("Failed to discard data: {}", e))
                        })?;
                    }
                }
                Err(e) => {
                    return Err(AudioError::CaptureError(format!(
                        "Failed to peek data: {}",
                        e
                    )))
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
    main_loop: MainLoop,
    context: PwContext,
    core: Core,
    registry: Registry,
    _stream_threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        pipewire::init();

        let main_loop = MainLoop::new(None).map_err(|_| {
            AudioError::InitializationFailed("Failed to create PipeWire main loop".into())
        })?;

        let context = PwContext::new(&main_loop).map_err(|_| {
            AudioError::InitializationFailed("Failed to create PipeWire context".into())
        })?;

        let core = context.connect(None).map_err(|_| {
            AudioError::InitializationFailed("Failed to connect to PipeWire".into())
        })?;

        let registry = core.get_registry().map_err(|_| {
            AudioError::InitializationFailed("Failed to get PipeWire registry".into())
        })?;

        Ok(Self {
            main_loop,
            context,
            core,
            registry,
            _stream_threads: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub fn is_available() -> bool {
        pipewire::init();
        MainLoop::new(None)
            .and_then(|main_loop| {
                PwContext::new(&main_loop)
                    .and_then(|context| context.connect(None))
                    .map(|_| true)
            })
            .unwrap_or(false)
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

        // Create a channel to signal when we're done collecting apps
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let apps = Arc::new(Mutex::new(apps));

        let _listener = self.registry.add_listener_local().global({
            let apps = Arc::clone(&apps);
            let tx = Arc::clone(&tx);
            move |global| {
                if let Some(props) = &global.props {
                    let media_class = props.get("media.class").unwrap_or("");
                    if media_class == "Stream/Input/Audio" || media_class == "Stream/Output/Audio" {
                        let mut apps = apps.lock().unwrap();
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
                        apps.push(app);
                    }
                }
                let _ = tx.lock().unwrap().send(());
            }
        });

        // Process events and wait for completion
        let timeout = Duration::from_millis(100);
        let _ = rx.recv_timeout(timeout);

        Ok(Arc::try_unwrap(apps)
            .map_err(|_| AudioError::CaptureError("Failed to unwrap apps".into()))?
            .into_inner()
            .map_err(|e| AudioError::CaptureError(e.to_string().unwrap_or_default()))?)
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

// Implement Send for PipeWireBackend since we manage thread safety ourselves
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
    stream_command_tx: Option<std::sync::mpsc::Sender<StreamCommand>>,
    _stream_thread: Option<thread::JoinHandle<()>>,
}

// Define commands we can send to the stream thread
enum StreamCommand {
    Connect,
    Disconnect,
    Shutdown,
}

impl PipeWireStream {
    fn new(
        core: &Core,
        app: &AudioApplication,
        config: AudioConfig,
        threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    ) -> Result<Self, AudioError> {
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(16384)));
        let buffer_clone = Arc::clone(&buffer);

        // Create channels for communication
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        // Create stream parameters to pass to the thread
        let app_id = app.id.clone();
        let app_pid = app.pid;
        let config_clone = config.clone();

        // Create stream in a separate thread
        let thread_handle = thread::spawn(move || {
            // Initialize PipeWire in this thread
            pipewire::init();

            let main_loop = MainLoop::new(None).unwrap();
            let context = PwContext::new(&main_loop).unwrap();
            let core = context.connect(None).unwrap();

            // Create properties for the stream
            let props = properties! {
                "media.class" => "Audio/Source",
                "audio.channels" => config_clone.channels.to_string(),
                "audio.rate" => config_clone.sample_rate.to_string(),
                "target.object" => if app_pid == 0 { "default.monitor" } else { &app_id },
            };

            // Create the stream (without connecting)
            let stream_name = if app_pid == 0 {
                "system-audio-capture"
            } else {
                "application-audio-capture"
            };

            let stream = match PwStream::new(&core, stream_name, props) {
                Ok(s) => s,
                Err(e) => {
                    ready_tx
                        .send(Err(format!("Failed to create PipeWire stream: {}", e)))
                        .unwrap();
                    return;
                }
            };

            // We successfully created the stream
            ready_tx.send(Ok(())).unwrap();

            // Process commands from the main thread
            let mut is_running = true;
            while is_running {
                // Process any pending PipeWire events
                main_loop.iteration(Duration::from_millis(10));

                // Check for commands
                if let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        StreamCommand::Connect => {
                            let _ = stream.connect(
                                Direction::Input,
                                None,
                                StreamFlags::AUTOCONNECT | StreamFlags::RT_PROCESS,
                                &[],
                            );
                        }
                        StreamCommand::Disconnect => {
                            let _ = stream.disconnect();
                        }
                        StreamCommand::Shutdown => {
                            is_running = false;
                        }
                    }
                }
            }

            // Clean up
            drop(stream);
            drop(core);
            drop(context);
            main_loop.destroy();
        });

        // Store thread handle
        threads.lock().unwrap().push(thread_handle.clone());

        // Wait for the stream to be created
        match ready_rx.recv().map_err(|_| {
            AudioError::InitializationFailed("Failed to initialize PipeWire thread".into())
        })? {
            Ok(()) => {
                // Stream was created successfully
                Ok(Self {
                    config,
                    buffer,
                    stream_command_tx: Some(cmd_tx),
                    _stream_thread: Some(thread_handle),
                })
            }
            Err(e) => {
                // Stream creation failed
                Err(AudioError::InitializationFailed(e))
            }
        }
    }
}

impl AudioCaptureStream for PipeWireStream {
    fn start(&mut self) -> Result<(), AudioError> {
        if let Some(tx) = &self.stream_command_tx {
            tx.send(StreamCommand::Connect)
                .map_err(|_| AudioError::CaptureError("Failed to send connect command".into()))?;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        if let Some(tx) = &self.stream_command_tx {
            tx.send(StreamCommand::Disconnect).map_err(|_| {
                AudioError::CaptureError("Failed to send disconnect command".into())
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
        // Send shutdown command when the stream is dropped
        if let Some(tx) = &self.stream_command_tx {
            let _ = tx.send(StreamCommand::Shutdown);
        }
    }
}

// Implement Send for PipeWireStream since we manage thread safety ourselves
unsafe impl Send for PipeWireStream {}

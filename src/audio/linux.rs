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
    stream::{Stream as PwStream, StreamFlags, StreamState},
};

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError, AudioFormat,
};

pub struct PulseAudioBackend {
    mainloop: Arc<Mainloop>,
    context: Arc<Mutex<Context>>,
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
            context: Arc::new(Mutex::new(context)),
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
            match context.lock().unwrap().get_state() {
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

        context.lock().unwrap().introspect().get_client_info_list({
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
    _context: Arc<Mutex<Context>>,
    config: AudioConfig,
}

// Mark PulseAudioStream as Send to satisfy AudioCaptureStream trait
unsafe impl Send for PulseAudioStream {}

impl PulseAudioStream {
    fn new(
        context: Arc<Mutex<Context>>,
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

        let stream_name = format!("capture_{}", app.name);

        // Create stream properties
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

        let mut ctx_lock = context.lock().unwrap();
        let mut stream = Stream::new_with_proplist(
            &mut *ctx_lock,
            &stream_name,
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
            .map_err(|e| {
                AudioError::CaptureError(e.to_string().unwrap_or_else(|| "unknown".into()))
            })?;

        drop(ctx_lock);

        Ok(Self {
            stream,
            _mainloop: mainloop,
            _context: context,
            config,
        })
    }

    fn new_system(
        context: Arc<Mutex<Context>>,
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

        let mut ctx_lock = context.lock().unwrap();
        let mut stream = Stream::new(&mut *ctx_lock, "system-audio-capture", &spec, None)
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

        drop(ctx_lock);

        Ok(Self {
            stream,
            _mainloop: mainloop,
            _context: context,
            config,
        })
    }

    fn new_application(
        context: Arc<Mutex<Context>>,
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

        let mut ctx_lock = context.lock().unwrap();
        let mut stream =
            Stream::new(&mut *ctx_lock, &stream_name, &spec, None).ok_or_else(|| {
                AudioError::InitializationFailed("Failed to create PulseAudio stream".into())
            })?;

        // Find the sink input for the application
        let (tx, rx) = std::sync::mpsc::channel();
        let pid = app.pid;
        let mut sink_input_index = None;

        context
            .lock()
            .unwrap()
            .introspect()
            .get_sink_input_info_list({
                let tx = tx.clone();
                move |result| {
                    if let pulse::callbacks::ListResult::Item(input) = result {
                        if let Some(client) = input.client {
                            if let Some(process_id) =
                                input.proplist.get_str("application.process.id")
                            {
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
            .map_err(|e| {
                AudioError::CaptureError(e.to_string().unwrap_or_else(|| "unknown".into()))
            })?;

        drop(ctx_lock);

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
        self.stream.uncork(None);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream.cork(None);
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

pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        Err(AudioError::BackendUnavailable(
            "PipeWire backend not implemented",
        ))
    }

    pub fn is_available() -> bool {
        false
    }
}

unsafe impl Send for PipeWireBackend {}

impl AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        Err(AudioError::BackendUnavailable(
            "PipeWire backend not implemented",
        ))
    }

    fn capture_application(
        &self,
        _app: &AudioApplication,
        _config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        Err(AudioError::BackendUnavailable(
            "PipeWire backend not implemented",
        ))
    }
}

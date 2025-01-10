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

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
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
            .map_err(|e| AudioError::InitializationFailed(e.to_string().expect("Failed to convert error to string")))?;

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
            .map_err(|e| AudioError::InitializationFailed(e.to_string().expect("Failed to convert error to string")))?;

        // Start the mainloop
        mainloop
            .start()
            .map_err(|e| AudioError::InitializationFailed(e.to_string().expect("Failed to convert error to string")))?;

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

impl AudioCaptureBackend for PulseAudioBackend {
    fn name(&self) -> &'static str {
        "PulseAudio"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let apps = Arc::new(Mutex::new(Vec::new()));
        let apps_clone = Arc::clone(&apps);

        let op = self.context.introspect().get_sink_input_info_list(move |list| {
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

        Ok(Arc::try_unwrap(apps)
            .unwrap()
            .into_inner()
            .unwrap_or_default())
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

        let stream_name = CString::new(format!("capture_{}", app.name))
            .map_err(|e| AudioError::InitializationFailed(e.to_string().expect("Failed to convert error to string")))?;

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

        stream
            .connect_record(
                Some(&format!("sink_input.{}", app.id)),
                Some(&attr),
                stream::FlagSet::ADJUST_LATENCY,
            )
            .map_err(|e| AudioError::CaptureError(e.to_string().expect("Failed to convert error to string")))?;

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
            .cork(None)
            .map_err(|e| AudioError::CaptureError(e.to_string().expect("Failed to convert error to string")))
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stream
            .cork(None)
            .map_err(|e| AudioError::CaptureError(e.to_string().expect("Failed to convert error to string")))
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
                    buffer[bytes_read..bytes_read + to_copy]
                        .copy_from_slice(&data[..to_copy]);
                    bytes_read += to_copy;
                    self.stream
                        .discard()
                        .map_err(|e| AudioError::CaptureError(e.to_string().expect("Failed to convert error to string")))?;
                }
                Err(e) => return Err(AudioError::CaptureError(e.to_string().expect("Failed to convert error to string"))),
            }
        }
        Ok(bytes_read)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}

pub struct PipeWireBackend {
    // TODO: Add PipeWire context and other required fields
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        // TODO: Initialize PipeWire connection
        Err(AudioError::BackendUnavailable("PipeWire support not yet implemented"))
    }

    pub fn is_available() -> bool {
        // TODO: Check if PipeWire is available
        false
    }
}

impl AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        Err(AudioError::BackendUnavailable("PipeWire support not yet implemented"))
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        Err(AudioError::BackendUnavailable("PipeWire support not yet implemented"))
    }
}
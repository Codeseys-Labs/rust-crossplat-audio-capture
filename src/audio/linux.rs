use std::{
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use pipewire::{
    self, channel,
    context::Context as PwContext,
    core::Core,
    main_loop::MainLoop,
    properties::properties,
    registry::Registry,
    spa,
    spa::pod::{Object, Pod},
    spa::utils::Direction,
    stream::{Stream as PwStream, StreamFlags},
};

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError,
};

use crate::AudioFormat as RsacAudioFormat;

pub struct PipeWireBackend {
    main_loop: MainLoop,
    context: PwContext,
    core: Core,
    registry: Registry,
    _stream_threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
}

impl PipeWireBackend {
    pub fn new() -> Result<Self, AudioError> {
        // Check if PipeWire is installed at runtime
        Self::check_pipewire_installed()?;

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

    // Check if PipeWire is properly installed
    fn check_pipewire_installed() -> Result<(), AudioError> {
        // First check if the library is installed
        let library_check = Command::new("sh")
            .args(["-c", "ldconfig -p | grep -q libpipewire"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);

        if !library_check {
            return Err(AudioError::BackendUnavailable(
                "PipeWire libraries not found. Please install libpipewire-0.3-0 or equivalent for your distribution".into()
            ));
        }

        // Then check if the daemon is running
        let daemon_check = Command::new("sh")
            .args(["-c", "ps -e | grep -q pipewire"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);

        if !daemon_check {
            return Err(AudioError::BackendUnavailable(
                "PipeWire daemon is not running. Please make sure PipeWire is properly installed and running".into()
            ));
        }

        Ok(())
    }

    pub fn is_available() -> bool {
        // Try to check if PipeWire is installed
        if let Err(e) = Self::check_pipewire_installed() {
            println!("PipeWire availability check failed: {}", e);
            return false;
        }
        // Basic check: Assume available if installed checks pass.
        // A more robust check would involve trying to connect.
        println!("PipeWire check passed (simplified)");
        true
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
        let apps_arc = Arc::new(Mutex::new(apps)); // Rename to avoid shadowing

        let listener = self.registry.add_listener_local().global({
            let apps_clone = Arc::clone(&apps_arc); // Clone Arc for closure
            let tx_clone = Arc::clone(&tx); // Clone Arc for closure
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
                        // Send a signal only when an app is found
                        let _ = tx_clone.lock().unwrap().send(());
                    }
                }
                // Removed sending signal for every global object
            }
        });

        // Process events and wait for completion with a longer timeout
        let timeout = Duration::from_secs(1); // Increased timeout to 1 second
                                              // Wait for at least one app signal or timeout
        let _ = rx.recv_timeout(timeout);

        // Explicitly drop the listener *before* trying to unwrap the Arc
        drop(listener);
        // Give a tiny bit more time for context switching if needed
        thread::sleep(Duration::from_millis(10));

        // Try to unwrap the Arc, fallback to cloning if it fails
        match Arc::try_unwrap(apps_arc) {
            Ok(mutex) => Ok(mutex
                .into_inner()
                .map_err(|e| AudioError::CaptureError(e.to_string()))?),
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
    stream_command_tx: Option<channel::Sender<StreamCommand>>,
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
        _core: &Core,
        app: &AudioApplication,
        config: AudioConfig,
        _threads: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
    ) -> Result<Self, AudioError> {
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(16384)));
        let buffer_clone_for_thread = Arc::clone(&buffer); // Clone for the thread

        // Create pipewire channel for communication
        let (cmd_tx, cmd_rx) = channel::channel::<StreamCommand>();
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

            // Create the stream mutably
            let mut stream = match PwStream::new(&core, stream_name, props) {
                Ok(s) => s,
                Err(e) => {
                    ready_tx
                        .send(Err(format!("Failed to create PipeWire stream: {}", e)))
                        .unwrap();
                    return;
                }
            };

            // Set up the listener with the process callback
            let _listener = stream
                .add_local_listener_with_user_data(buffer_clone_for_thread)
                .process(|stream, user_data_buffer_arc| {
                    // Add detailed logging back
                    println!("[Callback] Process callback entered."); 
                    // Dequeue a buffer from the stream
                    if let Some(mut buffer) = stream.dequeue_buffer() {
                        println!("[Callback] Dequeued a buffer."); 
                        // Get the first data plane/chunk mutably
                        if let Some(data_plane) = buffer.datas_mut().get_mut(0) {
                            println!("[Callback] Got data plane 0."); 
                            // Get the raw data slice from the data plane
                            if let Some(data) = data_plane.data() { 
                                println!("[Callback] Got data slice with length: {}", data.len()); 
                                // Lock the shared buffer (passed as user_data)
                                if let Ok(mut shared_buf) = user_data_buffer_arc.lock() {
                                    // Append the captured data
                                    shared_buf.extend_from_slice(data);
                                    println!("[Callback] Appended {} bytes to shared buffer.", data.len());
                                } else {
                                     eprintln!("[Callback] Warning: Failed to lock shared buffer.");
                                }
                            } else {
                                eprintln!("[Callback] Warning: Buffer data plane has no data slice (data() returned None).");
                            }
                        } else {
                           eprintln!("[Callback] Warning: Failed to get mutable data plane 0 from buffer.");
                        }
                    } else {
                         println!("[Callback] dequeue_buffer() returned None."); 
                    }// end if let Some(buffer)
                })
                .register()
                .map_err(|e| format!("Failed to register stream listener: {}", e));

            if let Err(e) = _listener {
                ready_tx.send(Err(e)).unwrap();
                return;
            }

            // Clone MainLoop for the closure before borrowing for attach
            let main_loop_clone = main_loop.clone();

            // Attach the command receiver to the main loop
            let receiver_loop = main_loop.loop_();
            let _receiver_attachment = cmd_rx.attach(&receiver_loop, move |cmd| {
                match cmd {
                    StreamCommand::Connect => {
                        println!("[Receiver] Received Connect command.");
                        
                        // Revert SPA parameter creation, use simple placeholder
                        let mut params_slice: Vec<&Pod> = Vec::new(); // Empty params

                        match stream.connect( 
                            Direction::Input,
                            None, 
                            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                            &mut params_slice // Pass empty &mut [&Pod]
                        ) {
                           Ok(_) => { println!("PipeWire stream connected via command."); }
                           Err(e) => {
                               eprintln!("Error connecting PipeWire stream via command: {:?}", e);
                           }
                        }
                    }
                    StreamCommand::Disconnect => {
                        println!("[Receiver] Received Disconnect command.");
                        let _ = stream.disconnect(); // stream is captured by move
                    }
                    StreamCommand::Shutdown => {
                        println!("[Receiver] Received Shutdown command. Quitting main loop...");
                        main_loop_clone.quit(); // Use the captured clone directly
                    }
                }
            });

            // Signal that the stream thread is fully ready (listener and receiver attached)
            ready_tx.send(Ok(())).unwrap();
            println!("PipeWire stream thread setup complete. Running main loop...");

            // Run the original main loop (this blocks until mainloop_quit.quit() is called)
            main_loop.run();

            // Clean up happens after main_loop.run() returns
            println!("Cleaning up PipeWire stream thread resources...");
            drop(core);
            drop(context);
            println!("PipeWire stream thread finished.");
        });

        // Wait for the stream thread to be fully ready
        match ready_rx.recv().map_err(|_| {
            AudioError::InitializationFailed("Failed to initialize PipeWire thread".into())
        })? {
            Ok(()) => {
                // Stream thread ready
                Ok(Self {
                    config,
                    buffer,
                    stream_command_tx: Some(cmd_tx),
                    _stream_thread: Some(thread_handle),
                })
            }
            Err(e) => {
                // Stream thread setup failed
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

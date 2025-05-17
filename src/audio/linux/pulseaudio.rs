use crate::core::buffer::AudioBuffer;
use crate::core::config::{SampleFormat as MySampleFormat, StreamConfig};
use crate::core::error::{AudioError, CaptureError, Result as AudioResult};
use crate::core::interface::CapturingStream;
use futures_core::Stream as FuturesStream; // Renamed to avoid conflict
use libpulse_binding as pulse;
use log::{debug, error, info, warn};
use pulse::context::Context;
use pulse::mainloop::standard::Mainloop;
use pulse::operation::State as OperationState;
use pulse::sample::{Format as PAFormat, Spec as SampleSpec};
use pulse::stream::{State as StreamState, Stream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const PULSEAUDIO_CLIENT_NAME: &str = "rust-crossplat-audio-capture";
const CONNECTION_TIMEOUT_SECONDS: u64 = 5;
const OPERATION_TIMEOUT_SECONDS: u64 = 5; // For stream operations like cork, connect
const MPSC_BUFFER_SIZE: usize = 10; // Size of the channel buffer for audio data

// Helper function to map PAErr to CaptureError
fn pa_err_to_capture_error(pa_err: pulse::error::PAErr, operation_context: &str) -> CaptureError {
    let pa_err_code = pa_err.0;
    let error_message = format!(
        "PulseAudio Error during '{}': {} (Code: {})",
        operation_context,
        pulse::error::PAErr::to_string(&pa_err).unwrap_or_else(|| "Unknown PAErr".into()),
        pa_err_code
    );
    // Potentially map specific pa_err_code to more specific CaptureError variants
    // For now, a generic BackendError is fine.
    CaptureError::BackendError(error_message)
}
/// Manages the PulseAudio connection and main loop.
#[derive(Debug)]
pub struct PulseAudioBackend {
    mainloop: Arc<Mutex<Mainloop>>,
    context: Arc<Context>,
    // Optional: A flag to track if the context reached the 'Ready' state during initialization.
    // This can be useful for debugging or for methods that need to confirm readiness post-initialization.
    // For the `new` method's logic, the local `ready_flag` suffices.
    // _is_initialized_ready: Arc<AtomicBool>,
}

/// Represents a PulseAudio recording stream.
#[derive(Debug)]
pub struct PulseAudioCaptureStream {
    stream: Arc<Stream>,
    mainloop: Arc<Mutex<Mainloop>>,
    context: Arc<Context>, // Shares the context from PulseAudioBackend
    receiver: Option<Receiver<Result<AudioBuffer, CaptureError>>>,
    stop_flag: Arc<AtomicBool>,
    is_corked: Arc<AtomicBool>,
    sample_spec: SampleSpec,
    stream_thread_handle: Option<thread::JoinHandle<()>>,
}

impl PulseAudioBackend {
    /// Creates a new `PulseAudioBackend` and connects to the PulseAudio server.
    ///
    /// This method initializes a PulseAudio mainloop and context, then attempts to
    /// connect to the server. It waits for the context to reach the 'Ready' state
    /// by iterating the mainloop.
    ///
    /// # Errors
    ///
    /// Returns `Err(CaptureError::BackendInitializationFailed)` if:
    /// - The mainloop cannot be created.
    /// - The context cannot be created.
    /// - The context fails to connect to the PulseAudio server.
    /// - The connection attempt times out before the context becomes ready.
    /// - The context enters a 'Failed' or 'Terminated' state during connection.
    pub fn new() -> AudioResult<Self> {
        debug!("Creating new PulseAudioBackend.");
        let mainloop = Mainloop::new()
            .ok_or_else(|| {
                error!("PulseAudio: Failed to create mainloop.");
                // Assuming CaptureError::BackendInitializationFailed is a valid variant
                // and we map its message to AudioError::BackendError
                CaptureError::BackendInitializationFailed(
                    "PulseAudio: Failed to create mainloop.".to_string(),
                )
            })
            .map_err(|ce| {
                AudioError::BackendError(format!("Mainloop creation failed: {:?}", ce))
            })?;
        info!("PulseAudio mainloop created.");

        let context = Context::new(&mainloop, PULSEAUDIO_CLIENT_NAME)
            .ok_or_else(|| {
                error!("PulseAudio: Failed to create context.");
                CaptureError::BackendInitializationFailed(
                    "PulseAudio: Failed to create context.".to_string(),
                )
            })
            .map_err(|ce| AudioError::BackendError(format!("Context creation failed: {:?}", ce)))?;
        info!("PulseAudio context created.");

        let ready_flag = Arc::new(AtomicBool::new(false));
        let error_flag = Arc::new(AtomicBool::new(false));

        let context_for_callback = context.clone();
        context.set_state_callback(Some(Box::new({
            let flag_ready = ready_flag.clone();
            let flag_error = error_flag.clone();
            move || {
                let state = context_for_callback.get_state();
                match state {
                    pulse::context::State::Ready => {
                        debug!("PulseAudio context state callback: Ready.");
                        flag_ready.store(true, Ordering::SeqCst);
                    }
                    pulse::context::State::Failed | pulse::context::State::Terminated => {
                        error!("PulseAudio context state callback: Failed or Terminated.");
                        flag_error.store(true, Ordering::SeqCst);
                    }
                    _ => {
                        debug!("PulseAudio context state callback: {:?}", state);
                    }
                }
            }
        })));
        debug!("PulseAudio context state callback set.");

        info!("Attempting to connect PulseAudio context...");
        context
            .connect(None, pulse::context::flags::NOFLAGS, None)
            .map_err(|pae| {
                let ce = pa_err_to_capture_error(pae, "context connection");
                error!("PulseAudio context connection failed: {:?}", ce);
                // Extract message from CaptureError::BackendError
                if let CaptureError::BackendError(msg) = ce {
                    AudioError::BackendError(msg)
                } else {
                    AudioError::BackendError(format!(
                        "Context connection failed with unhandled CaptureError: {:?}",
                        ce
                    ))
                }
            })?;
        debug!("PulseAudio context connect call initiated.");

        let start_time = std::time::Instant::now();
        loop {
            mainloop.iterate(true);

            if ready_flag.load(Ordering::SeqCst) {
                info!("PulseAudio context is Ready.");
                break Ok(Self {
                    mainloop: Arc::new(Mutex::new(mainloop)),
                    context: Arc::new(context),
                });
            }
            if error_flag.load(Ordering::SeqCst) {
                error!("PulseAudio context entered Failed or Terminated state during connection wait loop.");
                break Err(AudioError::BackendError(
                    "PulseAudio: Context entered Failed or Terminated state during connection."
                        .to_string(),
                ));
            }
            if start_time.elapsed() > Duration::from_secs(CONNECTION_TIMEOUT_SECONDS) {
                error!("PulseAudio connection timed out.");
                break Err(AudioError::BackendError(
                    "PulseAudio: Connection attempt timed out.".to_string(),
                ));
            }
        }
    }

    pub fn create_capture_stream(
        &mut self, // Changed to &mut self
        config: &StreamConfig,
        device_source_name: Option<&str>,
    ) -> Result<PulseAudioCaptureStream, CaptureError> {
        debug!(
            "Creating PulseAudio capture stream. Device: {:?}, Config: {:?}",
            device_source_name, config
        );

        let pa_format = match config.format.sample_format {
            MySampleFormat::F32LE => PAFormat::FLOAT32LE,
            MySampleFormat::S16LE => PAFormat::S16LE,
            _ => {
                let err_msg = format!(
                    "Unsupported sample format for PulseAudio: {:?}",
                    config.format.sample_format
                );
                error!("{}", err_msg);
                return Err(CaptureError::FormatNotSupported(err_msg));
            }
        };
        debug!("PulseAudio format selected: {:?}", pa_format);

        let sample_spec = SampleSpec {
            format: pa_format,
            rate: config.format.sample_rate,
            channels: config.format.channels as u8,
        };
        debug!("PulseAudio sample spec created: {:?}", sample_spec);

        if !sample_spec.is_valid() {
            let err_msg = "PulseAudio: Invalid sample spec.".to_string();
            error!("{}", err_msg);
            return Err(CaptureError::InvalidStreamConfig(err_msg));
        }
        info!("PulseAudio sample spec is valid.");

        // Attempt to get a mutable reference to the context.
        // This requires that self.context (the Arc<Context>) is uniquely held at this point.
        let context_mut_ref = Arc::get_mut(&mut self.context).ok_or_else(|| {
            let err_msg = "PulseAudio: Failed to get mutable context reference (Arc not unique). This is required to create a new stream.".to_string();
            error!("{}", err_msg);
            CaptureError::BackendOperationFailed(err_msg)
        })?;
        debug!("Successfully got mutable reference to PulseAudio context.");

        let stream = Stream::new(
            context_mut_ref, // Use the &mut Context
            "rust-crossplat-capture",
            &sample_spec,
            None, // No channel map
        )
        .ok_or_else(|| {
            let err_msg = "PulseAudio: Failed to create stream object.".to_string();
            error!("{}", err_msg);
            CaptureError::StreamCreationFailed(err_msg)
        })?;
        info!("PulseAudio stream object created.");

        let stream_arc = Arc::new(stream);
        // Note: After passing context_mut_ref, self.context (the Arc) is still valid
        // and can be cloned for the PulseAudioCaptureStream.

        let (audio_buffer_sender, receiver) = sync_channel(MPSC_BUFFER_SIZE);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let is_corked = Arc::new(AtomicBool::new(true));

        let stream_for_callback = stream_arc.clone();
        let read_stop_flag = stop_flag.clone();
        let read_sample_spec = sample_spec.clone();
        let read_sender = audio_buffer_sender; // Sender is moved into closure
        let captured_config_format = config.format.clone();

        stream_for_callback.set_read_callback(Some(Box::new(move |_length: usize| {
            if read_stop_flag.load(Ordering::SeqCst) {
                debug!("PulseAudio read callback: stop_flag is set, returning.");
                return;
            }

            match stream_for_callback.peek() {
                Ok(data_slice_result) => {
                    if let Some(data_slice) = data_slice_result {
                        if data_slice.is_empty() {
                            debug!("PulseAudio read callback: peeked empty data slice, dropping.");
                            if let Err(e) = stream_for_callback.drop_data() {
                                warn!("PulseAudio read callback: error dropping empty data slice: {:?}", e);
                                // Attempt to send this error, best effort
                                let drop_err = pa_err_to_capture_error(e, "stream drop_data after empty peek");
                                if read_sender.try_send(Err(drop_err)).is_err() {
                                    warn!("PulseAudio read callback: MPSC channel disconnected while sending empty peek drop_data error.");
                                }
                            }
                            return;
                        }
                        // debug!("PulseAudio read callback: peeked {} bytes of data.", data_slice.len());

                        let samples_f32: Vec<f32> = match read_sample_spec.format {
                            PAFormat::FLOAT32LE => {
                                if data_slice.len() % 4 != 0 {
                                    let err = CaptureError::BackendOperationFailed(
                                        "PulseAudio: Invalid data slice length for FLOAT32LE.".to_string(),
                                    );
                                    error!("PulseAudio read callback: {}", err);
                                    if read_sender.try_send(Err(err)).is_err() {
                                        warn!("PulseAudio read callback: MPSC channel disconnected while sending FLOAT32LE length error.");
                                    }
                                    if let Err(e) = stream_for_callback.drop_data() {
                                         warn!("PulseAudio read callback: error dropping data after FLOAT32LE length error: {:?}", e);
                                    }
                                    return;
                                }
                                data_slice
                                    .chunks_exact(4)
                                    .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                                    .collect()
                            }
                            PAFormat::S16LE => {
                                if data_slice.len() % 2 != 0 {
                                    let err = CaptureError::BackendOperationFailed(
                                        "PulseAudio: Invalid data slice length for S16LE.".to_string(),
                                    );
                                    error!("PulseAudio read callback: {}", err);
                                    if read_sender.try_send(Err(err)).is_err() {
                                        warn!("PulseAudio read callback: MPSC channel disconnected while sending S16LE length error.");
                                    }
                                    if let Err(e) = stream_for_callback.drop_data() {
                                        warn!("PulseAudio read callback: error dropping data after S16LE length error: {:?}", e);
                                    }
                                    return;
                                }
                                data_slice
                                    .chunks_exact(2)
                                    .map(|c| {
                                        (i16::from_le_bytes(c.try_into().unwrap()) as f32) / 32768.0
                                    })
                                    .collect()
                            }
                            _ => {
                                let err = CaptureError::FormatNotSupported(
                                    "PulseAudio: Unhandled sample format in read callback.".to_string(),
                                );
                                error!("PulseAudio read callback: {}", err);
                                if read_sender.try_send(Err(err)).is_err() {
                                     warn!("PulseAudio read callback: MPSC channel disconnected while sending unhandled format error.");
                                }
                                if let Err(e) = stream_for_callback.drop_data() {
                                    warn!("PulseAudio read callback: error dropping data after unhandled format error: {:?}", e);
                                }
                                return;
                            }
                        };

                        let audio_buffer = AudioBuffer::new(
                            samples_f32,
                            read_sample_spec.channels as u16,
                            read_sample_spec.rate,
                            captured_config_format.clone(),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default(),
                        );
                        // trace!("PulseAudio read callback: processed {} samples.", audio_buffer.samples_f32().len());

                        if let Err(e) = read_sender.try_send(Ok(audio_buffer)) {
                            warn!("PulseAudio read callback: MPSC channel try_send error (likely disconnected): {:?}", e);
                            // If disconnected, further processing in this callback is futile.
                            if matches!(e, std::sync::mpsc::TrySendError::Disconnected(_)) {
                                 debug!("PulseAudio read callback: MPSC channel disconnected, stopping further sends.");
                                 // Cannot set read_stop_flag from here as it's not mutable.
                                 // The loop in read_chunk will eventually detect disconnection.
                            }
                        }
                        if let Err(e) = stream_for_callback.drop_data() {
                             warn!("PulseAudio read callback: error dropping data after successful send: {:?}", e);
                             let drop_err = pa_err_to_capture_error(e, "stream drop_data after send");
                             if read_sender.try_send(Err(drop_err)).is_err() {
                                 warn!("PulseAudio read callback: MPSC channel disconnected while sending drop_data error (after send).");
                             }
                        }
                    } else {
                        debug!("PulseAudio read callback: peeked None (no data available).");
                    }
                }
                Err(pa_err) => {
                    let capture_err = pa_err_to_capture_error(pa_err, "stream peek");
                    error!("Error peeking stream data in read callback: {:?}", capture_err);
                    if read_sender.try_send(Err(capture_err)).is_err() {
                        warn!("PulseAudio read callback: MPSC channel disconnected while sending peek error.");
                    }
                    // Attempt to drop data even after a peek error, as PA might require it.
                    if let Err(e_drop) = stream_for_callback.drop_data() {
                        warn!("PulseAudio read callback: error dropping data after peek error: {:?}", e_drop);
                    }
                }
            }
        })));
        debug!("PulseAudio stream read callback set.");

        let stream_flags = pulse::stream::flags::START_CORKED
            | pulse::stream::flags::ADJUST_LATENCY
            | pulse::stream::flags::AUTO_TIMING_UPDATE;

        let buffer_attr = pulse::def::BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };

        info!(
            "Attempting to connect PulseAudio record stream for device: {:?}...",
            device_source_name.unwrap_or("default")
        );
        stream_arc
            .connect_record(device_source_name, Some(&buffer_attr), stream_flags)
            .map_err(|pae| {
                let ce = pa_err_to_capture_error(pae, "stream connect_record");
                error!("PulseAudio stream connect_record failed: {:?}", ce);
                if let CaptureError::BackendError(msg) = ce {
                    CaptureError::StreamCreationFailed(msg)
                } else {
                    CaptureError::StreamCreationFailed(format!(
                        "Stream connect_record failed with unhandled CaptureError: {:?}",
                        ce
                    ))
                }
            })?;
        debug!("PulseAudio stream connect_record call initiated.");

        let stream_ready_flag = Arc::new(AtomicBool::new(false));
        let stream_error_flag = Arc::new(AtomicBool::new(false));
        let stream_for_state_cb = stream_arc.clone();

        stream_for_state_cb.set_state_callback(Some(Box::new({
            let flag_ready = stream_ready_flag.clone();
            let flag_error = stream_error_flag.clone();
            move || {
                let pa_stream_state = stream_for_state_cb.get_state();
                match pa_stream_state {
                    StreamState::Ready => {
                        debug!("PulseAudio stream state callback: Ready.");
                        flag_ready.store(true, Ordering::SeqCst);
                    }
                    StreamState::Failed | StreamState::Terminated => {
                        error!("PulseAudio stream state callback: Failed or Terminated.");
                        flag_error.store(true, Ordering::SeqCst);
                    }
                    _ => {
                        debug!("PulseAudio stream state callback: {:?}", pa_stream_state);
                    }
                }
            }
        })));
        debug!("PulseAudio stream state callback set.");

        let mainloop_clone_for_wait = self.mainloop.clone();
        let start_time = std::time::Instant::now();
        loop {
            mainloop_clone_for_wait.lock().unwrap().iterate(true);
            if stream_ready_flag.load(Ordering::SeqCst) {
                info!("PulseAudio stream is Ready.");
                break;
            }
            if stream_error_flag.load(Ordering::SeqCst) {
                let err_msg = "PulseAudio: Stream entered Failed or Terminated state during connection wait loop.".to_string();
                error!("{}", err_msg);
                return Err(CaptureError::StreamCreationFailed(err_msg));
            }
            if start_time.elapsed() > Duration::from_secs(OPERATION_TIMEOUT_SECONDS) {
                let err_msg = "PulseAudio: Stream connection timed out.".to_string();
                error!("{}", err_msg);
                return Err(CaptureError::StreamCreationFailed(err_msg));
            }
        }
        stream_for_state_cb.set_state_callback(None); // Clear callback once connected or failed
        debug!("PulseAudio stream connection wait loop finished.");

        info!("PulseAudioCaptureStream created successfully.");
        Ok(PulseAudioCaptureStream {
            stream: stream_arc,
            mainloop: Arc::clone(&self.mainloop),
            context: Arc::clone(&self.context), // Clone the Arc for the stream's own reference
            receiver: Some(receiver),
            stop_flag,
            is_corked,
            sample_spec,
            stream_thread_handle: None,
        })
    }
}
/// Enumerates audio applications using PulseAudio by introspecting sink inputs.
///
/// This function connects to the PulseAudio server via the provided `backend`,
/// then uses the introspection API to list all sink inputs. Each sink input
/// typically represents an audio stream from an application.
///
/// It extracts properties like application name, process ID, and executable path
/// from the sink input's property list.
///
/// The operation is asynchronous and involves iterating the PulseAudio mainloop
/// until the introspection callback signals completion or an error occurs.
/// A timeout is used to prevent indefinite blocking.
///
/// # Arguments
/// * `backend` - A reference to an initialized `PulseAudioBackend` which provides
///   access to the PulseAudio context and mainloop.
///
/// # Returns
/// * `Ok(Vec<super::LinuxApplicationInfo>)` - A list of applications found,
///   each populated with available information.
/// * `Err(crate::core::error::CaptureError)` - If the enumeration fails, times out,
///   or an error occurs during PulseAudio communication.
pub(crate) fn enumerate_audio_applications_pulseaudio(
    backend: &PulseAudioBackend,
) -> Result<Vec<super::LinuxApplicationInfo>, crate::core::error::CaptureError> {
    debug!("Enumerating audio applications using PulseAudio.");

    let app_list = Arc::new(Mutex::new(Vec::<super::LinuxApplicationInfo>::new()));
    let operation_complete = Arc::new(AtomicBool::new(false));
    let operation_failed = Arc::new(AtomicBool::new(false));

    let operation = backend
        .context
        .introspect()
        .get_sink_input_info_list(Some(Box::new({
            let app_list_clone = app_list.clone();
            let operation_complete_clone = operation_complete.clone();
            let operation_failed_clone = operation_failed.clone();
            move |list_result: libpulse_binding::callbacks::ListResult<
                &libpulse_binding::context::introspect::SinkInputInfo,
            >| {
                match list_result {
                    libpulse_binding::callbacks::ListResult::Item(info) => {
                        // debug!("PulseAudio: Found sink input: index={}, name={:?}", info.index, info.name);
                        let mut app_info = super::LinuxApplicationInfo {
                            name: None,
                            process_id: None,
                            executable_path: None,
                            stream_description: None,
                            pipewire_node_id: None, // Not applicable for PulseAudio
                            pulseaudio_sink_input_index: Some(info.index),
                        };

                        if let Some(props) = &info.proplist {
                            if let Some(name_cow) = props.get_str("application.name") {
                                app_info.name = Some(name_cow.into_owned());
                            }
                            if let Some(pid_cow) = props.get_str("application.process.id") {
                                app_info.process_id = pid_cow.parse().ok();
                            }
                            if let Some(bin_cow) = props.get_str("application.process.binary") {
                                app_info.executable_path = Some(bin_cow.into_owned());
                            }
                            if let Some(media_name_cow) = props.get_str("media.name") {
                                app_info.stream_description = Some(media_name_cow.into_owned());
                            }

                            // Fallback for name if application.name is not present but info.name is
                            if app_info.name.is_none() && info.name.is_some() {
                                app_info.name =
                                    info.name.as_ref().map(|s| s.to_string_lossy().into_owned());
                            }
                            // Fallback for stream_description if media.name is not present but info.name is
                            if app_info.stream_description.is_none()
                                && info.name.is_some()
                                && app_info.name.as_deref()
                                    != info
                                        .name
                                        .as_ref()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .as_deref()
                            {
                                app_info.stream_description =
                                    info.name.as_ref().map(|s| s.to_string_lossy().into_owned());
                            }
                        } else if info.name.is_some() {
                            // If proplist is None, use info.name as a fallback for app_info.name
                            app_info.name =
                                info.name.as_ref().map(|s| s.to_string_lossy().into_owned());
                            app_info.stream_description =
                                info.name.as_ref().map(|s| s.to_string_lossy().into_owned());
                        }

                        // debug!("PulseAudio: Parsed app info: {:?}", app_info);
                        app_list_clone.lock().unwrap().push(app_info);
                    }
                    libpulse_binding::callbacks::ListResult::End => {
                        debug!("PulseAudio: Sink input info list ended.");
                        operation_complete_clone.store(true, Ordering::SeqCst);
                    }
                    libpulse_binding::callbacks::ListResult::Error => {
                        error!(
                            "PulseAudio: Failed to get sink input info list during introspection."
                        );
                        operation_failed_clone.store(true, Ordering::SeqCst);
                        operation_complete_clone.store(true, Ordering::SeqCst); // Also mark as complete to exit loop
                    }
                }
            }
        })));

    if operation.is_none() {
        error!(
            "PulseAudio: Failed to initiate get_sink_input_info_list operation (returned None)."
        );
        return Err(CaptureError::BackendOperationFailed(
            "PulseAudio: Failed to initiate application enumeration.".into(),
        ));
    }
    debug!("PulseAudio: get_sink_input_info_list operation initiated.");

    let start_time = std::time::Instant::now();
    let timeout_duration = Duration::from_secs(3);

    loop {
        backend.mainloop.lock().unwrap().iterate(true); // Blocking iterate

        if operation_complete.load(Ordering::SeqCst) {
            debug!("PulseAudio enumeration: Operation marked complete.");
            break;
        }
        if start_time.elapsed() > timeout_duration {
            warn!("PulseAudio enumeration: Timed out waiting for operation to complete.");
            // If it timed out but didn't explicitly fail, we might still have partial results.
            // However, the contract implies completion or failure.
            // For robustness, if it times out and isn't marked complete, consider it a failure.
            if !operation_complete.load(Ordering::SeqCst) {
                operation_failed.store(true, Ordering::SeqCst); // Mark as failed due to timeout
            }
            break;
        }
        // Brief sleep to prevent tight loop if iterate returns quickly without events,
        // though iterate(true) should block. This is a safeguard.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    if operation_failed.load(Ordering::SeqCst) {
        error!("PulseAudio enumeration: Operation failed.");
        return Err(CaptureError::BackendOperationFailed(
            "PulseAudio: Failed to enumerate application audio streams.".into(),
        ));
    }

    if !operation_complete.load(Ordering::SeqCst) {
        // This case implies timeout without explicit failure or completion signal from callback
        error!("PulseAudio enumeration: Operation timed out without explicit completion or failure signal.");
        return Err(CaptureError::BackendOperationFailed(
            "PulseAudio: Application enumeration timed out without completion signal.".into(),
        ));
    }

    // Retrieve the list
    match Arc::try_unwrap(app_list) {
        Ok(mutex) => match mutex.into_inner() {
            Ok(final_list) => {
                info!(
                    "PulseAudio: Successfully enumerated {} application audio streams.",
                    final_list.len()
                );
                Ok(final_list)
            }
            Err(_) => {
                error!("PulseAudio: Failed to acquire lock on app_list (Mutex poisoned).");
                Err(CaptureError::BackendOperationFailed(
                    "PulseAudio: Failed to acquire lock for final application list.".into(),
                ))
            }
        },
        Err(_) => {
            error!("PulseAudio: Failed to unwrap Arc for app_list (references still exist). This should not happen.");
            Err(CaptureError::BackendOperationFailed(
                "PulseAudio: Internal error unwrapping application list.".into(),
            ))
        }
    }
}

impl CapturingStream for PulseAudioCaptureStream {
    fn start(&mut self) -> Result<(), CaptureError> {
        debug!("PulseAudioCaptureStream start() called.");
        if self.is_corked.load(Ordering::SeqCst) {
            info!("Stream is corked, attempting to uncork.");
            let op = self.stream.cork(false).ok_or_else(|| {
                let err_msg =
                    "PulseAudio: Cork(false) operation failed to initiate (returned None)."
                        .to_string();
                error!("{}", err_msg);
                CaptureError::BackendOperationFailed(err_msg)
            })?;
            debug!("Cork(false) operation initiated.");

            let start_time = std::time::Instant::now();
            loop {
                self.mainloop.lock().unwrap().iterate(false); // Non-blocking iterate
                match op.get_state() {
                    OperationState::Done => {
                        self.is_corked.store(false, Ordering::SeqCst);
                        self.stop_flag.store(false, Ordering::SeqCst); // Ensure stop_flag is clear for new data
                        info!("PulseAudio stream uncorked successfully.");
                        return Ok(());
                    }
                    OperationState::Cancelled => {
                        let err_msg = "PulseAudio: Uncork operation cancelled.".to_string();
                        error!("{}", err_msg);
                        return Err(CaptureError::BackendOperationFailed(err_msg));
                    }
                    OperationState::Running => { /* continue */ }
                }
                if start_time.elapsed() > Duration::from_secs(OPERATION_TIMEOUT_SECONDS) {
                    let err_msg = "PulseAudio: Uncork operation timed out.".to_string();
                    error!("{}", err_msg);
                    return Err(CaptureError::BackendOperationFailed(err_msg));
                }
                thread::sleep(Duration::from_millis(10)); // Polling interval
            }
        } else {
            info!("PulseAudio stream already uncorked.");
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        debug!("PulseAudioCaptureStream stop() called.");
        self.stop_flag.store(true, Ordering::SeqCst); // Signal read callback to stop sending

        if !self.is_corked.load(Ordering::SeqCst) {
            info!("Stream is running, attempting to cork.");
            let op = self.stream.cork(true).ok_or_else(|| {
                let err_msg =
                    "PulseAudio: Cork(true) operation failed to initiate (returned None)."
                        .to_string();
                error!("{}", err_msg);
                CaptureError::BackendOperationFailed(err_msg)
            })?;
            debug!("Cork(true) operation initiated.");

            let start_time = std::time::Instant::now();
            loop {
                self.mainloop.lock().unwrap().iterate(false); // Non-blocking iterate
                match op.get_state() {
                    OperationState::Done => {
                        self.is_corked.store(true, Ordering::SeqCst);
                        info!("PulseAudio stream corked successfully.");
                        return Ok(());
                    }
                    OperationState::Cancelled => {
                        let err_msg = "PulseAudio: Cork operation cancelled.".to_string();
                        error!("{}", err_msg);
                        return Err(CaptureError::BackendOperationFailed(err_msg));
                    }
                    OperationState::Running => { /* continue */ }
                }
                if start_time.elapsed() > Duration::from_secs(OPERATION_TIMEOUT_SECONDS) {
                    let err_msg = "PulseAudio: Cork operation timed out.".to_string();
                    error!("{}", err_msg);
                    return Err(CaptureError::BackendOperationFailed(err_msg));
                }
                thread::sleep(Duration::from_millis(10)); // Polling interval
            }
        } else {
            info!("PulseAudio stream already corked.");
            Ok(())
        }
    }

    fn close(&mut self) -> Result<(), CaptureError> {
        debug!("PulseAudioCaptureStream close() called.");
        self.stop()?; // Ensure stream is stopped (corked) and read callback signaled

        if self.stream.is_connected() {
            info!("Disconnecting PulseAudio stream.");
            // disconnect() is synchronous and might block.
            self.stream.disconnect().map_err(|pae| {
                let ce = pa_err_to_capture_error(pae, "stream disconnect in close");
                error!("Failed to disconnect stream in close: {:?}", ce);
                // Return BackendOperationFailed with the detailed message from ce
                if let CaptureError::BackendError(msg) = ce {
                    CaptureError::BackendOperationFailed(msg)
                } else {
                    CaptureError::BackendOperationFailed(format!(
                        "Unhandled CaptureError during stream disconnect: {:?}",
                        ce
                    ))
                }
            })?;
            info!("PulseAudio stream disconnected.");
        } else {
            debug!("PulseAudio stream already disconnected.");
        }

        if let Some(handle) = self.stream_thread_handle.take() {
            debug!("Joining stream_thread_handle (if it was ever created).");
            if handle.join().is_err() {
                warn!("Error joining stream_thread_handle (thread may have panicked).");
            }
        }
        // Ensure receiver is dropped/closed if it hasn't been taken by to_async_stream
        // Taking it here ensures that if close() is called, subsequent read_chunk/to_async_stream will fail cleanly.
        if self.receiver.take().is_some() {
            debug!("MPSC receiver taken and dropped in close().");
        }
        info!("PulseAudioCaptureStream closed successfully.");
        Ok(())
    }

    fn is_running(&self) -> bool {
        let running =
            !self.is_corked.load(Ordering::SeqCst) && !self.stop_flag.load(Ordering::SeqCst);
        // trace!("PulseAudioCaptureStream is_running(): {}", running);
        running
    }

    fn read_chunk(&mut self, timeout_ms: Option<u32>) -> Result<Option<AudioBuffer>, CaptureError> {
        // trace!("PulseAudioCaptureStream read_chunk() called with timeout: {:?}", timeout_ms);
        if self.stop_flag.load(Ordering::SeqCst) && self.is_corked.load(Ordering::SeqCst) {
            debug!("read_chunk: Stream is stopped and corked, returning StreamClosed.");
            return Err(CaptureError::StreamClosed("Stream is stopped.".to_string()));
        }

        let receiver = self.receiver.as_ref().ok_or_else(|| {
            error!("read_chunk: Receiver has been taken (e.g., by to_async_stream).");
            CaptureError::InvalidOperation(
                "Receiver has been taken (e.g., by to_async_stream).".to_string(),
            )
        })?;

        let recv_timeout_duration = timeout_ms.map_or_else(
            || {
                // trace!("read_chunk: No timeout specified, using very long default for recv_timeout.");
                Duration::from_secs(3600 * 24) // Effectively infinite for practical purposes if None
            },
            |ms| {
                // trace!("read_chunk: Using timeout of {}ms for recv_timeout.", ms);
                Duration::from_millis(ms as u64)
            },
        );

        // Iterate mainloop once if channel is empty to allow PA callbacks to run
        // This helps ensure the read_callback gets a chance to populate the channel.
        if receiver.is_empty() {
            // trace!("read_chunk: MPSC receiver is empty, iterating mainloop once (non-blocking).");
            self.mainloop.lock().unwrap().iterate(false); // Non-blocking iterate
        }

        match receiver.recv_timeout(recv_timeout_duration) {
            Ok(Ok(buffer)) => {
                // trace!("read_chunk: Received AudioBuffer with {} samples.", buffer.samples_f32().len());
                Ok(Some(buffer))
            }
            Ok(Err(e)) => {
                error!(
                    "read_chunk: Received CaptureError from MPSC channel: {:?}",
                    e
                );
                Err(e)
            }
            Err(RecvTimeoutError::Timeout) => {
                // trace!("read_chunk: MPSC channel recv_timeout: Timeout.");
                Ok(None)
            }
            Err(RecvTimeoutError::Disconnected) => {
                error!("read_chunk: MPSC channel disconnected.");
                self.stop_flag.store(true, Ordering::SeqCst); // Ensure stream state reflects this
                self.is_corked.store(true, Ordering::SeqCst); // Treat as stopped
                Err(CaptureError::BackendDisconnected(
                    "PulseAudio read_chunk: MPSC channel from read callback disconnected."
                        .to_string(),
                ))
            }
        }
    }

    fn to_async_stream<'a>(
        &'a mut self,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = Result<AudioBuffer, CaptureError>>
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
        CaptureError,
    > {
        if let Some(recv) = self.receiver.take() {
            // Consume the receiver
            Ok(Box::pin(async_stream::try_stream! {
                loop {
                    // This will block the thread on which the async task is polled if not careful.
                    // For `std::sync::mpsc`, `recv()` is blocking.
                    // A production solution might use `tokio::task::spawn_blocking` or an async-native channel.
                    // Given the "simple approach" context, this is a direct translation.
                    // Using tokio::task::spawn_blocking to handle the blocking recv call.
                    // This assumes tokio is part of the project's async runtime.
                    // If not, this part needs adjustment (e.g., a different async channel or manual polling).
                    match tokio::task::spawn_blocking(move || recv.recv()).await {
                        Ok(Ok(Ok(buffer))) => yield buffer,
                        Ok(Ok(Err(e))) => Err(e)?, // Propagate CaptureError from channel
                        Ok(Err(_recv_error)) => break, // MPSC channel disconnected
                        Err(_join_error) => { // Task panicked or was cancelled
                            Err(CaptureError::BackendOperationFailed("Async stream reader task failed.".to_string()))?;
                            break;
                        }
                    }
                }
            }))
        } else {
            Err(CaptureError::InvalidOperation(
                "Receiver already taken for async stream or stream closed.".to_string(),
            ))
        }
    }
}

impl Drop for PulseAudioBackend {
    fn drop(&mut self) {
        debug!("Dropping PulseAudioBackend.");
        let state = self.context.get_state();
        if state != pulse::context::State::Terminated
            && state != pulse::context::State::Failed
            && state != pulse::context::State::Unconnected
        {
            info!(
                "PulseAudioBackend drop: Disconnecting context (state: {:?}).",
                state
            );
            self.context.disconnect();
            // It might be good to iterate the mainloop a few times to process disconnect,
            // but in drop, we should avoid complex operations or blocking.
        } else {
            debug!(
                "PulseAudioBackend drop: Context already in a terminal/disconnected state ({:?}).",
                state
            );
        }
        // Mainloop (Arc<Mutex<Mainloop>>) and Context (Arc<Context>) will be dropped
        // when their reference counts go to zero.
    }
}

impl Drop for PulseAudioCaptureStream {
    fn drop(&mut self) {
        debug!("Dropping PulseAudioCaptureStream.");
        // Ensure stop is called, which also signals the read callback and attempts to cork.
        // close() also handles stream disconnect and MPSC receiver cleanup.
        if let Err(e) = self.close() {
            error!(
                "Error during PulseAudioCaptureStream drop (in close()): {:?}",
                e
            );
        }
        info!("PulseAudioCaptureStream dropped.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::AudioFormat as CoreAudioFormat; // Alias to avoid confusion

    // Basic test to check if PulseAudioBackend can be created.
    // This test requires a running PulseAudio server.
    // It might fail in CI environments without PulseAudio.
    #[test]
    #[ignore] // Ignored by default as it requires a running PulseAudio server
    fn test_pulseaudio_backend_new() {
        match PulseAudioBackend::new() {
            Ok(_backend) => {
                // Successfully created and connected
                // println!("Successfully connected to PulseAudio.");
            }
            Err(e) => {
                // eprintln!("Failed to connect to PulseAudio: {:?}", e);
                // Consider if this should panic or just print, depending on test environment expectations.
                // For now, let it panic to clearly indicate failure if PulseAudio is expected.
                panic!("PulseAudioBackend::new() failed: {:?}", e);
            }
        }
    }

    #[test]
    #[ignore] // Requires PulseAudio server and might capture actual audio
    fn test_create_and_start_stop_stream() {
        let backend = match PulseAudioBackend::new() {
            Ok(b) => b,
            Err(e) => {
                panic!("Failed to initialize PulseAudio backend for test: {:?}", e);
            }
        };

        let stream_config = StreamConfig {
            format: CoreAudioFormat {
                sample_rate: 44100,
                channels: 1,
                bits_per_sample: 32, // For F32LE
                sample_format: MySampleFormat::F32LE,
            },
            buffer_size_frames: None,
            latency_mode: crate::core::config::LatencyMode::LowLatency,
        };

        // Original plan: create_capture_stream returns (stream, receiver)
        // For CapturingStream trait to work as specified, stream must own receiver.
        // I will proceed with the original plan for create_capture_stream's signature for now,
        // and the CapturingStream methods `read_chunk` and `to_async_stream` will be placeholders.
        // This highlights the design conflict.

        let mut backend = backend; // Make backend mutable
        let mut capture_stream = backend
            .create_capture_stream(&stream_config, None) // create_capture_stream now takes &mut self
            .expect("Failed to create capture stream");

        // log::info!("Stream created, attempting to start...");
        capture_stream.start().expect("Failed to start stream");
        // log::info!("Stream started. Running for 100ms then stopping.");
        assert!(capture_stream.is_running());

        // Let it run for a bit (not actually reading, just testing lifecycle)
        thread::sleep(Duration::from_millis(100));

        // log::info!("Attempting to stop stream...");
        capture_stream.stop().expect("Failed to stop stream");
        // log::info!("Stream stopped.");
        assert!(!capture_stream.is_running());

        // log::info!("Attempting to close stream...");
        capture_stream.close().expect("Failed to close stream");
        // log::info!("Stream closed.");
    }
}

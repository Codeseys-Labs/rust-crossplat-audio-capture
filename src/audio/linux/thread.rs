//! PipeWire dedicated thread infrastructure.
//!
//! This module provides the thread + channel infrastructure for running PipeWire
//! objects (`Rc`/`!Send`) on a dedicated thread, communicating with the caller
//! via `std::sync::mpsc` channels.
//!
//! # Architecture
//!
//! ```text
//! User Thread                          PipeWire Thread (dedicated)
//! ────────────                         ──────────────────────────
//! AudioCapture / CapturingStream       MainLoop, Context, Core, Registry
//! BridgeConsumer                       Stream, StreamListener
//! command_tx ─────mpsc::channel────►  command_rx
//!                                      BridgeProducer (writes to ring buffer)
//! ◄──────mpsc::Sender──────────────   response_tx
//! ```
//!
//! All PipeWire `Rc`-based objects live exclusively on the dedicated thread.
//! The `PipeWireThread` handle is `Send + Sync` and safe to use from any thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::ring_buffer::BridgeProducer;
use crate::bridge::stream::PlatformStream;
use crate::core::buffer::AudioBuffer;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

// ── CaptureConfig ────────────────────────────────────────────────────────

/// Resolved capture parameters passed to the PipeWire thread.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the PipeWire backend to create a stream.
#[derive(Debug)]
pub(crate) struct CaptureConfig {
    /// What to capture (system default, specific app, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

// ── PipeWireCommand ──────────────────────────────────────────────────────

/// Commands sent from the caller thread to the dedicated PipeWire thread.
///
/// Each command that expects a response includes a `response_tx` oneshot sender
/// so the PipeWire thread can reply with the result.
pub(crate) enum PipeWireCommand {
    /// Begin capturing audio with the given configuration.
    ///
    /// The [`BridgeProducer`] is moved to the PipeWire thread — it is `Send`
    /// and will be used by the PipeWire `process` callback to push audio data
    /// into the ring buffer.
    StartCapture {
        config: CaptureConfig,
        producer: BridgeProducer,
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Stop the current capture session and clean up PipeWire stream objects.
    StopCapture {
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Shut down the PipeWire thread entirely. No response needed — the thread exits.
    Shutdown,
}

// ── CaptureStreamData ────────────────────────────────────────────────────

/// User data stored inside the PipeWire stream listener.
///
/// Passed to `Stream::add_local_listener_with_user_data()` and accessible
/// from the `param_changed` and `process` callbacks as `&mut CaptureStreamData`.
///
/// # Real-time safety
///
/// The `producer` field uses `rtrb` lock-free push — safe for the PipeWire
/// process callback thread. The `Vec<f32>` allocation in the process callback
/// is acceptable for the initial implementation but should be optimized with
/// a pre-allocated scratch buffer in future iterations.
struct CaptureStreamData {
    /// Negotiated audio format — updated by the `param_changed` callback
    /// when PipeWire negotiates the actual stream format.
    format: libspa::param::audio::AudioInfoRaw,
    /// Ring buffer producer — pushes `AudioBuffer`s to the consumer thread.
    producer: BridgeProducer,
    /// Number of audio channels (updated from negotiated format, falls back to requested).
    channels: u16,
    /// Sample rate in Hz (updated from negotiated format, falls back to requested).
    sample_rate: u32,
}

// ── PipeWireThread ───────────────────────────────────────────────────────

/// Handle to the dedicated PipeWire thread.
///
/// All PipeWire `Rc`-based objects (MainLoop, Context, Core, Registry, Stream)
/// live on the spawned thread. The caller communicates via [`PipeWireCommand`]s
/// sent through the command channel, and receives responses via per-command
/// response senders.
///
/// # Lifecycle
///
/// 1. [`PipeWireThread::spawn()`] creates the thread and waits for PipeWire init.
/// 2. [`start_capture()`](PipeWireThread::start_capture) / [`stop_capture()`](PipeWireThread::stop_capture)
///    send commands and block for the response.
/// 3. On [`Drop`], a `Shutdown` command is sent and the thread is joined.
pub(crate) struct PipeWireThread {
    /// Channel to send commands to the PipeWire thread.
    command_tx: std_mpsc::Sender<PipeWireCommand>,
    /// Join handle for the dedicated thread (taken on drop).
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Shared flag: `true` while the PipeWire thread's event loop is running.
    /// Read by `is_alive()`, which is called from `LinuxPlatformStream::is_active()`.
    #[allow(dead_code)]
    is_running: Arc<AtomicBool>,
}

impl PipeWireThread {
    /// Spawn the dedicated PipeWire thread.
    ///
    /// This creates a new OS thread named `"rsac-pipewire"` that:
    /// 1. Initializes PipeWire (`pipewire::init()`)
    /// 2. Creates `MainLoop`, `Context`, `Core`, and `Registry`
    /// 3. Enters the event loop, pumping PipeWire events and processing commands
    ///
    /// The call blocks until PipeWire initialization completes on the new thread.
    /// Returns an error if any PipeWire initialization step fails.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendInitializationFailed`] if the thread cannot be spawned
    ///   or if PipeWire initialization fails (MainLoop, Context, Core, or Registry).
    pub fn spawn() -> AudioResult<Self> {
        let (command_tx, command_rx) = std_mpsc::channel();
        let (init_tx, init_rx) = std_mpsc::channel();
        let is_running = Arc::new(AtomicBool::new(true));
        let is_running_thread = Arc::clone(&is_running);

        let thread_handle = std::thread::Builder::new()
            .name("rsac-pipewire".to_string())
            .spawn(move || {
                pw_thread_main(command_rx, init_tx, is_running_thread);
            })
            .map_err(|e| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to spawn PipeWire thread: {}", e),
            })?;

        // Block until the PipeWire thread reports init success or failure.
        let init_result = init_rx
            .recv()
            .map_err(|_| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: "PipeWire thread exited before reporting init status".to_string(),
            })?;

        // If PipeWire init failed, the thread has already exited. Propagate the error.
        init_result?;

        Ok(PipeWireThread {
            command_tx,
            thread_handle: Some(thread_handle),
            is_running,
        })
    }

    /// Send a `StartCapture` command to the PipeWire thread and wait for the response.
    ///
    /// The `BridgeProducer` is moved to the PipeWire thread where it will be used
    /// by the PipeWire `process` callback to push captured audio into the ring buffer.
    ///
    /// This creates a PipeWire stream, registers listener callbacks (param_changed
    /// for format negotiation, process for audio data), and connects the stream.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   does not respond, or if stream creation/connection fails.
    pub fn start_capture(
        &self,
        config: CaptureConfig,
        producer: BridgeProducer,
    ) -> AudioResult<()> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StartCapture {
                config,
                producer,
                response_tx,
            })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "start_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        response_rx.recv().map_err(|_| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "start_capture".to_string(),
            message: "PipeWire thread did not respond to StartCapture".to_string(),
            context: None,
        })?
    }

    /// Send a `StopCapture` command to the PipeWire thread and wait for the response.
    ///
    /// Tells the PipeWire thread to tear down the current capture stream and
    /// release the `BridgeProducer`.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   does not respond.
    pub fn stop_capture(&self) -> AudioResult<()> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StopCapture { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "stop_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        response_rx.recv().map_err(|_| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "stop_capture".to_string(),
            message: "PipeWire thread did not respond to StopCapture".to_string(),
            context: None,
        })?
    }

    /// Returns `true` if the PipeWire thread is still alive.
    ///
    /// This checks the shared atomic flag, which is set to `false` when the
    /// thread's event loop exits (either due to `Shutdown` or an error).
    /// Called by `LinuxPlatformStream::is_active()` (PlatformStream trait contract).
    #[allow(dead_code)]
    pub fn is_alive(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

impl Drop for PipeWireThread {
    fn drop(&mut self) {
        // Send Shutdown command — ignore errors (thread may already be dead).
        let _ = self.command_tx.send(PipeWireCommand::Shutdown);

        // Join the thread to ensure clean shutdown.
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ── LinuxPlatformStream ──────────────────────────────────────────────────

/// Platform-specific stream handle for Linux (PipeWire backend).
///
/// Wraps a shared [`PipeWireThread`] handle and implements [`PlatformStream`]
/// so it can be used with [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `LinuxPlatformStream` is `Send` (required by `PlatformStream`). The inner
/// `Arc<Mutex<PipeWireThread>>` provides shared ownership and interior mutability.
pub(crate) struct LinuxPlatformStream {
    pw_thread: Arc<Mutex<PipeWireThread>>,
}

impl LinuxPlatformStream {
    /// Create a new `LinuxPlatformStream` wrapping the given PipeWire thread.
    pub fn new(pw_thread: Arc<Mutex<PipeWireThread>>) -> Self {
        Self { pw_thread }
    }
}

impl PlatformStream for LinuxPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.pw_thread
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "PipeWire thread mutex poisoned".to_string(),
                source: None,
            })?
            .stop_capture()
    }

    fn is_active(&self) -> bool {
        self.pw_thread.lock().map(|t| t.is_alive()).unwrap_or(false)
    }
}

// ── Process Tree Discovery ───────────────────────────────────────────────

/// Discovers all PIDs in a process tree rooted at `parent_pid`.
///
/// Walks the Linux `/proc` filesystem to find all descendant processes
/// (children, grandchildren, etc.) of the given parent PID. Returns a
/// deduplicated, sorted `Vec<u32>` containing the parent PID and all
/// discovered descendants.
///
/// # Algorithm
///
/// For each process in `/proc`, reads `/proc/{pid}/stat` to extract the
/// parent PID (field 4). Builds a parent→children map, then performs a
/// breadth-first traversal from `parent_pid` to collect all descendants.
///
/// If `/proc` cannot be read (e.g., in a containerized environment without
/// procfs), returns a single-element vector containing just `parent_pid`
/// (graceful degradation to single-process capture).
///
/// # Example
///
/// If process 1000 has children 1001 and 1002, and 1001 has child 1003:
/// ```text
/// discover_process_tree_pids(1000) → [1000, 1001, 1002, 1003]
/// ```
fn discover_process_tree_pids(parent_pid: u32) -> Vec<u32> {
    use std::collections::{HashMap, VecDeque};
    use std::fs;

    // Build a map of pid → parent_pid by reading /proc/*/stat
    let mut parent_map: HashMap<u32, u32> = HashMap::new();

    let proc_entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!(
                "ProcessTree: cannot read /proc: {}. Falling back to single PID {}",
                e,
                parent_pid
            );
            return vec![parent_pid];
        }
    };

    for entry in proc_entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Only process numeric directory names (PIDs)
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Read /proc/{pid}/stat to extract PPID (field 4)
        let stat_path = format!("/proc/{}/stat", pid);
        if let Ok(stat_contents) = fs::read_to_string(&stat_path) {
            if let Some(ppid) = parse_ppid_from_stat(&stat_contents) {
                parent_map.insert(pid, ppid);
            }
        }
    }

    // BFS from parent_pid to find all descendants
    let mut all_pids: Vec<u32> = vec![parent_pid];
    let mut queue: VecDeque<u32> = VecDeque::new();
    queue.push_back(parent_pid);

    // Build children map for efficient lookup
    let mut children_map: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&child, &parent) in &parent_map {
        children_map.entry(parent).or_default().push(child);
    }

    while let Some(current_pid) = queue.pop_front() {
        if let Some(children) = children_map.get(&current_pid) {
            for &child_pid in children {
                if !all_pids.contains(&child_pid) {
                    all_pids.push(child_pid);
                    queue.push_back(child_pid);
                }
            }
        }
    }

    all_pids.sort();
    all_pids.dedup();

    log::info!(
        "ProcessTree: parent_pid={}, discovered {} total PIDs: {:?}",
        parent_pid,
        all_pids.len(),
        all_pids
    );

    all_pids
}

/// Parses the parent PID (PPID) from the contents of `/proc/{pid}/stat`.
///
/// The `/proc/{pid}/stat` format is:
/// ```text
/// pid (comm) state ppid pgid sid ...
/// ```
///
/// The process name (`comm`) may contain spaces and parentheses, so we
/// find the last `)` to locate the end of the comm field, then parse
/// the fourth field (PPID) from the remaining fields.
fn parse_ppid_from_stat(stat_contents: &str) -> Option<u32> {
    // Find the end of the comm field (last ')' in the line)
    let after_comm = stat_contents.rfind(')')? + 1;
    let remainder = &stat_contents[after_comm..];

    // Fields after comm: state ppid pgid ...
    // Split by whitespace and get the 2nd field (ppid, 0-indexed: state=0, ppid=1)
    let mut fields = remainder.split_whitespace();
    fields.next()?; // skip state
    let ppid_str = fields.next()?;
    ppid_str.parse::<u32>().ok()
}

// ── pw-dump Node Lookup ──────────────────────────────────────────────────

/// Specifies how to look up a PipeWire node via `pw-dump`.
enum PwNodeLookup<'a> {
    /// Match by application name (case-insensitive against `application.name`
    /// or `application.process.binary`).
    ByAppName(&'a str),
    /// Match by process ID (exact match against `application.process.id`).
    ByPid(u32),
    /// Match by any PID in a set (for process tree capture).
    /// Searches for the first audio output node whose `application.process.id`
    /// matches any PID in the provided set.
    ByPidSet(&'a [u32]),
}

/// Runs `pw-dump`, parses the JSON output, and finds the `object.serial` of
/// the first PipeWire node that:
/// - has `type == "PipeWire:Interface:Node"`
/// - has `info.props.media.class` containing `"Stream/Output/Audio"`
/// - matches the given [`PwNodeLookup`] criteria
///
/// Returns the `object.serial` as a `String` suitable for use as `TARGET_OBJECT`.
///
/// # Errors
///
/// - [`AudioError::BackendError`] if `pw-dump` cannot be executed or returns
///   non-zero, or if the output cannot be parsed as JSON.
/// - [`AudioError::ApplicationNotFound`] if no matching node is found.
fn find_pipewire_node_serial(lookup: &PwNodeLookup<'_>) -> AudioResult<String> {
    // Run pw-dump and capture its JSON output.
    let output = std::process::Command::new("pw-dump")
        .arg("--no-colors")
        .output()
        .map_err(|e| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump".to_string(),
            message: format!("Failed to run pw-dump: {}. Is pipewire-utils installed?", e),
            context: None,
        })?;

    if !output.status.success() {
        return Err(AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump".to_string(),
            message: format!(
                "pw-dump exited with status: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            context: None,
        });
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let entries: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump parse".to_string(),
            message: format!("Failed to parse pw-dump JSON: {}", e),
            context: None,
        })?;

    let array = entries.as_array().ok_or_else(|| AudioError::BackendError {
        backend: "PipeWire".to_string(),
        operation: "pw-dump parse".to_string(),
        message: "pw-dump output is not a JSON array".to_string(),
        context: None,
    })?;

    let pid_string; // storage for PID → String conversion (avoids per-iteration alloc)
    let pid_str = match lookup {
        PwNodeLookup::ByPid(pid) => {
            pid_string = pid.to_string();
            Some(pid_string.as_str())
        }
        _ => None,
    };

    // For ByPidSet, pre-compute string representations of all PIDs.
    let pid_set_strings: Vec<String> = match lookup {
        PwNodeLookup::ByPidSet(pids) => pids.iter().map(|p| p.to_string()).collect(),
        _ => Vec::new(),
    };

    for entry in array {
        // Filter: must be a PipeWire node.
        let entry_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if entry_type != "PipeWire:Interface:Node" {
            continue;
        }

        // Get info.props (where all the metadata lives).
        let props = match entry.get("info").and_then(|i| i.get("props")) {
            Some(p) => p,
            None => continue,
        };

        // Filter: media.class must indicate an audio output stream.
        let media_class = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !media_class.contains("Stream/Output/Audio") {
            continue;
        }

        // Check if this node matches the lookup criteria.
        let matches = match lookup {
            PwNodeLookup::ByAppName(name) => {
                let app_name = props
                    .get("application.name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let app_binary = props
                    .get("application.process.binary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                app_name.eq_ignore_ascii_case(name) || app_binary.eq_ignore_ascii_case(name)
            }
            PwNodeLookup::ByPid(_) => {
                let proc_id = props
                    .get("application.process.id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                proc_id == pid_str.unwrap()
            }
            PwNodeLookup::ByPidSet(_) => {
                let proc_id = props
                    .get("application.process.id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                pid_set_strings.iter().any(|s| s.as_str() == proc_id)
            }
        };

        if !matches {
            continue;
        }

        // Extract object.serial — may be a JSON string or number.
        if let Some(serial) = props.get("object.serial") {
            if let Some(s) = serial.as_str() {
                log::debug!("pw-dump: matched node with object.serial={}", s);
                return Ok(s.to_string());
            }
            if let Some(n) = serial.as_u64() {
                log::debug!("pw-dump: matched node with object.serial={}", n);
                return Ok(n.to_string());
            }
        }

        // Fallback: use the top-level node `id` if object.serial is missing.
        if let Some(id) = entry.get("id").and_then(|v| v.as_u64()) {
            log::warn!(
                "pw-dump: matched node has no object.serial, falling back to id={}",
                id
            );
            return Ok(id.to_string());
        }
    }

    // No matching node found.
    match lookup {
        PwNodeLookup::ByAppName(name) => Err(AudioError::ApplicationNotFound {
            identifier: name.to_string(),
        }),
        PwNodeLookup::ByPid(pid) => Err(AudioError::ApplicationNotFound {
            identifier: format!("PID {}", pid),
        }),
        PwNodeLookup::ByPidSet(pids) => Err(AudioError::ApplicationNotFound {
            identifier: format!("process tree PIDs {:?}", pids),
        }),
    }
}

// ── PipeWire Thread Main Function ────────────────────────────────────────

/// The main function for the dedicated PipeWire thread.
///
/// This runs on the spawned thread and owns all PipeWire `Rc` objects.
/// It communicates with the caller thread via the command channel and
/// reports initialization status via `init_tx`.
///
/// # Event Loop
///
/// The loop alternates between:
/// 1. Pumping PipeWire events via `main_loop.loop_().iterate(50ms)` — this is
///    where PipeWire callbacks (including the `process` callback) fire.
/// 2. Checking for incoming commands via `command_rx.try_recv()`.
///
/// The loop exits on `Shutdown` command or if the command channel disconnects.
///
/// # Audio Data Flow
///
/// When a `StartCapture` command is received, the thread:
/// 1. Creates a PipeWire `Stream` with properties matching the [`CaptureTarget`]
/// 2. Registers a **process callback** that converts raw PipeWire audio data
///    (F32LE bytes) into [`AudioBuffer`]s and pushes them to the [`BridgeProducer`]
/// 3. Registers a **param_changed callback** for format negotiation
/// 4. Connects the stream with `AUTOCONNECT | MAP_BUFFERS` flags
///
/// The `BridgeProducer::push_or_drop()` call in the process callback is lock-free
/// and non-blocking, making it safe for the real-time PipeWire callback context.
fn pw_thread_main(
    command_rx: std_mpsc::Receiver<PipeWireCommand>,
    init_tx: std_mpsc::Sender<AudioResult<()>>,
    is_running: Arc<AtomicBool>,
) {
    use pipewire::context::ContextBox;
    use pipewire::main_loop::MainLoopBox;
    use pipewire::properties::properties;
    use pipewire::stream::{StreamBox, StreamFlags, StreamListener};

    use libspa::param::audio::{AudioFormat as SpaAudioFormat, AudioInfoRaw};
    use libspa::param::format::{MediaSubtype, MediaType};
    use libspa::param::{format_utils, ParamType};
    use libspa::pod::{Object, Pod};

    // Step 1: Initialize PipeWire library.
    pipewire::init();

    // Step 2: Create the MainLoop (non-threaded — we drive it manually via iterate()).
    let main_loop = match MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create MainLoop: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Step 3: Create Context and connect to the PipeWire daemon.
    let context = match ContextBox::new(main_loop.loop_(), None) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create PipeWire Context: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to connect to PipeWire daemon: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let _registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to get PipeWire registry: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Signal successful initialization back to the caller.
    if init_tx.send(Ok(())).is_err() {
        // Caller dropped the receiver — no point continuing.
        is_running.store(false, Ordering::SeqCst);
        return;
    }

    // ── Step 4: Enter the event loop ─────────────────────────────────

    // Thread-local state for the current capture session.
    // The stream must outlive its listener (the listener registers C callbacks
    // against the stream's raw pointer). We enforce this by dropping the
    // listener before the stream in all cleanup paths.
    let mut capture_stream: Option<StreamBox> = None;
    let mut capture_listener: Option<StreamListener<CaptureStreamData>> = None;

    loop {
        // Pump PipeWire events. The `process` callback fires during iterate()
        // on this same thread, pushing audio data via BridgeProducer::push_or_drop().
        let _ = main_loop.loop_().iterate(Duration::from_millis(50));

        // Check for incoming commands (non-blocking).
        match command_rx.try_recv() {
            Ok(PipeWireCommand::StartCapture {
                config,
                producer,
                response_tx,
            }) => {
                log::debug!(
                    "PipeWire thread: StartCapture received (target={:?}, {}Hz, {}ch)",
                    config.target,
                    config.sample_rate,
                    config.channels
                );

                // Clean up any existing capture session first.
                if capture_listener.is_some() || capture_stream.is_some() {
                    log::debug!("PipeWire thread: cleaning up previous capture session");
                    capture_listener = None;
                    capture_stream = None;
                }

                // ── Build PipeWire stream properties based on CaptureTarget ──

                let mut props = properties! {
                    *pipewire::keys::NODE_NAME => "rsac-capture",
                    *pipewire::keys::STREAM_CAPTURE_SINK => "true",
                    *pipewire::keys::STREAM_MONITOR => "true",
                };

                match &config.target {
                    CaptureTarget::SystemDefault => {
                        // No TARGET_OBJECT — captures from the default output
                        // sink monitor. STREAM_CAPTURE_SINK + STREAM_MONITOR
                        // handle the routing.
                        log::debug!("PipeWire: SystemDefault — no TARGET_OBJECT");
                    }
                    CaptureTarget::Device(device_id) => {
                        // TARGET_OBJECT = the device's PipeWire node ID or serial.
                        props.insert(*pipewire::keys::TARGET_OBJECT, device_id.0.as_str());
                        log::debug!("PipeWire: Device target — TARGET_OBJECT={}", device_id.0);
                    }
                    CaptureTarget::Application(app_id) => {
                        // TARGET_OBJECT = the application's PipeWire node ID.
                        // The ApplicationId string should contain the PW node
                        // ID or serial assigned by the caller.
                        props.insert(*pipewire::keys::TARGET_OBJECT, app_id.0.as_str());
                        log::debug!("PipeWire: Application target — TARGET_OBJECT={}", app_id.0);
                    }
                    CaptureTarget::ApplicationByName(name) => {
                        // Resolve application name → PipeWire node object.serial
                        // via pw-dump. TARGET_OBJECT requires a serial, not a name.
                        let serial = match find_pipewire_node_serial(&PwNodeLookup::ByAppName(name))
                        {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!(
                                    "PipeWire: ApplicationByName '{}' — node lookup failed: {}",
                                    name,
                                    e
                                );
                                let _ = response_tx.send(Err(e));
                                continue;
                            }
                        };
                        props.insert(*pipewire::keys::TARGET_OBJECT, serial.as_str());
                        log::debug!(
                            "PipeWire: ApplicationByName '{}' resolved to node serial={}",
                            name,
                            serial
                        );
                    }
                    CaptureTarget::ProcessTree(pid) => {
                        // ── Step 1: Discover the full process tree ──
                        // Walk /proc to find all child/descendant PIDs of the
                        // target process. Falls back to single PID if /proc
                        // is unavailable.
                        let tree_pids = discover_process_tree_pids(pid.0);

                        log::debug!(
                            "PipeWire: ProcessTree PID {} — discovered {} PIDs in tree: {:?}",
                            pid.0,
                            tree_pids.len(),
                            tree_pids
                        );

                        // ── Step 2: Find PipeWire node for any PID in the tree ──
                        // Search pw-dump for the first audio output node belonging
                        // to any process in the tree. This handles the common case
                        // where a parent process (e.g., browser main) spawns a
                        // child that does the actual audio output (e.g., renderer).
                        let serial = match find_pipewire_node_serial(&PwNodeLookup::ByPidSet(
                            &tree_pids,
                        )) {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!(
                                    "PipeWire: ProcessTree PID {} — node lookup failed for {} PIDs: {}",
                                    pid.0,
                                    tree_pids.len(),
                                    e
                                );
                                let _ = response_tx.send(Err(e));
                                continue;
                            }
                        };
                        props.insert(*pipewire::keys::TARGET_OBJECT, serial.as_str());
                        log::debug!(
                            "PipeWire: ProcessTree PID {} resolved to node serial={} (searched {} PIDs)",
                            pid.0,
                            serial,
                            tree_pids.len()
                        );
                    }
                }

                // ── Create the PipeWire stream ──

                let stream = match StreamBox::new(&core, "rsac-capture", props) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "create_stream".to_string(),
                            message: format!("Failed to create PipeWire stream: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build user data for stream callbacks ──

                let user_data = CaptureStreamData {
                    format: AudioInfoRaw::new(),
                    producer,
                    channels: config.channels,
                    sample_rate: config.sample_rate,
                };

                // ── Register stream listener with callbacks ──

                let listener = match stream
                    .add_local_listener_with_user_data(user_data)
                    .param_changed(|_stream, user_data, id, param| {
                        // Format negotiation callback.
                        // PipeWire calls this when the actual stream format is
                        // negotiated (may differ from what we requested).

                        let Some(param) = param else {
                            // NULL param means format cleared.
                            return;
                        };

                        if id != ParamType::Format.as_raw() {
                            // Not a format parameter — ignore.
                            return;
                        }

                        let (media_type, media_subtype) = match format_utils::parse_format(param) {
                            Ok(v) => v,
                            Err(_) => return,
                        };

                        // Only accept raw audio.
                        if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                            return;
                        }

                        // Parse the negotiated format into our AudioInfoRaw.
                        let _ = user_data.format.parse(param);

                        // Update channels/sample_rate from the negotiated format
                        // so the process callback creates AudioBuffers with the
                        // correct metadata.
                        let negotiated_channels = user_data.format.channels();
                        let negotiated_rate = user_data.format.rate();
                        if negotiated_channels > 0 {
                            user_data.channels = negotiated_channels as u16;
                        }
                        if negotiated_rate > 0 {
                            user_data.sample_rate = negotiated_rate;
                        }

                        log::debug!(
                            "PipeWire format negotiated: {:?}, {}ch @ {}Hz",
                            user_data.format.format(),
                            negotiated_channels,
                            negotiated_rate
                        );
                    })
                    .process(|stream, user_data| {
                        // Audio data callback — runs in the PipeWire real-time
                        // context during main_loop.iterate().
                        //
                        // REAL-TIME SAFETY:
                        // - BridgeProducer::push_or_drop() is lock-free (rtrb)
                        // - Vec allocation is acceptable for initial impl
                        //   (optimize with scratch buffer later)
                        // - No locks, no blocking, no I/O

                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };

                        let datas = buffer.datas_mut();
                        if datas.is_empty() {
                            return;
                        }

                        let data = &mut datas[0];
                        let chunk_size = data.chunk().size() as usize;
                        let n_samples = chunk_size / std::mem::size_of::<f32>();

                        if n_samples == 0 {
                            return;
                        }

                        if let Some(raw_bytes) = data.data() {
                            // Convert raw F32LE bytes to f32 samples.
                            let mut samples = Vec::with_capacity(n_samples);
                            for i in 0..n_samples {
                                let offset = i * 4;
                                if offset + 4 <= raw_bytes.len() {
                                    let sample = f32::from_le_bytes([
                                        raw_bytes[offset],
                                        raw_bytes[offset + 1],
                                        raw_bytes[offset + 2],
                                        raw_bytes[offset + 3],
                                    ]);
                                    samples.push(sample);
                                }
                            }

                            if !samples.is_empty() {
                                let audio_buffer = AudioBuffer::new(
                                    samples,
                                    user_data.channels,
                                    user_data.sample_rate,
                                );
                                // Push to ring buffer. If full, the buffer is
                                // silently dropped (back-pressure).
                                user_data.producer.push_or_drop(audio_buffer);
                            }
                        }

                        // The PipeWire buffer is automatically re-queued when
                        // `buffer` goes out of scope (RAII).
                    })
                    .register()
                {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "register_listener".to_string(),
                            message: format!("Failed to register PipeWire stream listener: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build format Pod for stream.connect() ──

                let mut audio_info = AudioInfoRaw::new();
                audio_info.set_format(SpaAudioFormat::F32LE);
                audio_info.set_rate(config.sample_rate);
                audio_info.set_channels(config.channels as u32);

                let pod_object = Object {
                    type_: pipewire::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                    id: ParamType::EnumFormat.as_raw(),
                    properties: audio_info.into(),
                };

                let values: Vec<u8> = match pipewire::spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &pipewire::spa::pod::Value::Object(pod_object),
                ) {
                    Ok(result) => result.0.into_inner(),
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "format_pod".to_string(),
                            message: format!("Failed to serialize format Pod: {:?}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                let Some(pod) = Pod::from_bytes(&values) else {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "format_pod".to_string(),
                        message: "Failed to create Pod from serialized bytes".to_string(),
                        context: None,
                    }));
                    continue;
                };
                let mut params = [pod];

                // ── Connect the stream ──

                if let Err(e) = stream.connect(
                    libspa::utils::Direction::Input,
                    None,
                    StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                    &mut params,
                ) {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "stream_connect".to_string(),
                        message: format!("Failed to connect PipeWire stream: {}", e),
                        context: None,
                    }));
                    continue;
                }

                log::debug!(
                    "PipeWire thread: stream created and connected (state={:?})",
                    stream.state()
                );

                // Store the stream and listener — they must stay alive for
                // callbacks to fire. Listener is dropped before stream in all
                // cleanup paths.
                capture_stream = Some(stream);
                capture_listener = Some(listener);

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::StopCapture { response_tx }) => {
                log::debug!("PipeWire thread: StopCapture received");

                // Drop listener first (unregisters callbacks from the C stream),
                // then drop the stream (destroys the C stream object).
                capture_listener = None;
                capture_stream = None;

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::Shutdown) => {
                log::debug!("PipeWire thread: Shutdown received, exiting event loop");
                // Clean up any active capture before exiting.
                // Drop listener before stream — listener callbacks reference stream internals.
                drop(capture_listener.take());
                drop(capture_stream.take());
                break;
            }

            Err(std_mpsc::TryRecvError::Empty) => {
                // No commands waiting — continue pumping PipeWire events.
            }

            Err(std_mpsc::TryRecvError::Disconnected) => {
                // Command channel closed — caller is gone, exit gracefully.
                log::debug!("PipeWire thread: command channel disconnected, exiting");
                // Drop listener before stream — listener callbacks reference stream internals.
                drop(capture_listener.take());
                drop(capture_stream.take());
                break;
            }
        }
    }

    // Cleanup: PipeWire objects are dropped via RAII when this function returns.
    // The MainLoop, Context, Core, and Registry are all dropped here.
    is_running.store(false, Ordering::SeqCst);
    log::debug!("PipeWire thread: exited cleanly");
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `LinuxPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_linux_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<LinuxPlatformStream>();
}

/// Assert that `PipeWireThread` is `Send`.
fn _assert_pipewire_thread_send() {
    fn _assert<T: Send>() {}
    _assert::<PipeWireThread>();
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    // ── parse_ppid_from_stat ─────────────────────────────────────────

    #[test]
    fn test_parse_ppid_from_stat_typical() {
        // Typical /proc/{pid}/stat: pid (name) state ppid ...
        let stat = "1234 (my_process) S 567 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 123456 12345678 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(567));
    }

    #[test]
    fn test_parse_ppid_from_stat_name_with_spaces() {
        // Process name can contain spaces
        let stat = "5678 (Web Content) S 1000 5678 5678 0 -1 4194560 200 0 0 0 0 0 0 0 20 0 3 0 654321 87654321 500 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(1000));
    }

    #[test]
    fn test_parse_ppid_from_stat_name_with_parens() {
        // Process name can contain parentheses: "(sd-pam)"
        let stat = "42 ((sd-pam)) S 1 42 42 0 -1 1077936384 50 0 0 0 0 0 0 0 20 0 1 0 100 1234567 10 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(1));
    }

    #[test]
    fn test_parse_ppid_from_stat_pid_1_init() {
        // PID 1 (init/systemd) has PPID 0
        let stat = "1 (systemd) S 0 1 1 0 -1 4194560 100000 200000 10 20 1000 500 2000 300 20 0 1 0 1 200000000 1500 18446744073709551615 0 0 0 0 0 0 671173123 4096 1260 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(0));
    }

    #[test]
    fn test_parse_ppid_from_stat_empty_string() {
        assert_eq!(parse_ppid_from_stat(""), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_malformed() {
        // No closing parenthesis
        assert_eq!(parse_ppid_from_stat("1234 (broken"), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_truncated() {
        // Only comm field, no state or ppid
        assert_eq!(parse_ppid_from_stat("1234 (name)"), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_state_only() {
        // Has state but no ppid
        assert_eq!(parse_ppid_from_stat("1234 (name) S"), None);
    }

    // ── discover_process_tree_pids ───────────────────────────────────

    #[test]
    fn test_discover_process_tree_pids_current_process() {
        // The current process PID should always be in the result
        let current_pid = std::process::id();
        let pids = discover_process_tree_pids(current_pid);
        assert!(
            pids.contains(&current_pid),
            "Result should contain the parent PID. Got: {:?}",
            pids
        );
    }

    #[test]
    fn test_discover_process_tree_pids_nonexistent_pid() {
        // A PID that (almost certainly) doesn't exist should return
        // just that PID (graceful degradation).
        let fake_pid = 4_000_000_000; // max PID on Linux is typically 4194304
        let pids = discover_process_tree_pids(fake_pid);
        assert_eq!(pids, vec![fake_pid]);
    }

    #[test]
    fn test_discover_process_tree_pids_pid_1() {
        // PID 1 (init/systemd) should have children
        let pids = discover_process_tree_pids(1);
        assert!(
            pids.len() > 1,
            "PID 1 should have child processes. Got: {:?}",
            pids
        );
        assert!(pids.contains(&1), "Result should contain PID 1");
        // Result should be sorted
        for window in pids.windows(2) {
            assert!(window[0] <= window[1], "PIDs should be sorted: {:?}", pids);
        }
    }

    #[test]
    fn test_discover_process_tree_pids_sorted_and_deduped() {
        let current_pid = std::process::id();
        let pids = discover_process_tree_pids(current_pid);

        // Check sorted
        for window in pids.windows(2) {
            assert!(
                window[0] < window[1],
                "PIDs should be sorted and unique: {:?}",
                pids
            );
        }
    }

    // ── PwNodeLookup::ByPidSet matching ──────────────────────────────

    #[test]
    fn test_pw_node_lookup_by_pid_set_display() {
        // Verify the error message for ByPidSet includes the PID list
        let pids = vec![100, 200, 300];
        let result = find_pipewire_node_serial(&PwNodeLookup::ByPidSet(&pids));
        // This will fail (pw-dump not available in test), but we can verify
        // the error message format if pw-dump is available or the error type
        match result {
            Err(AudioError::ApplicationNotFound { identifier }) => {
                assert!(
                    identifier.contains("100")
                        && identifier.contains("200")
                        && identifier.contains("300"),
                    "Error should list all PIDs. Got: {}",
                    identifier
                );
            }
            Err(AudioError::BackendError { message, .. }) => {
                // pw-dump not available — that's fine for this test
                assert!(
                    message.contains("pw-dump"),
                    "Should mention pw-dump in error: {}",
                    message
                );
            }
            Ok(_) => {
                // Unlikely but possible if pw-dump returns matching data
            }
            Err(e) => {
                panic!("Unexpected error type: {:?}", e);
            }
        }
    }
}

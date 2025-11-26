use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use hound::{SampleFormat, WavSpec, WavWriter};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use rsac::audio::discovery::{AudioSourceDiscovery, AudioSourceType as DiscoveredAudioSourceType};
use rsac::audio::linux::pipewire::{ApplicationSelector, PipeWireApplicationCapture};
use std::fs::File;
use std::io;
use std::io::BufWriter;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct AudioSource {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub source_type: AudioSourceType,
}

#[derive(Debug, Clone)]
pub enum AudioSourceType {
    Application,
    SystemAudio,
    ProcessTree,
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub source: AudioSource,
    pub children: Vec<TreeNode>,
    pub depth: usize,
    pub is_expanded: bool,
}

impl TreeNode {
    pub fn new(source: AudioSource, depth: usize) -> Self {
        Self {
            source,
            children: Vec::new(),
            depth,
            is_expanded: true,
        }
    }

    pub fn add_child(&mut self, child: TreeNode) {
        self.children.push(child);
    }

    pub fn flatten(&self) -> Vec<(AudioSource, usize, bool)> {
        let mut result = Vec::new();
        result.push((self.source.clone(), self.depth, !self.children.is_empty()));

        if self.is_expanded {
            for child in &self.children {
                result.extend(child.flatten());
            }
        }

        result
    }
}

#[derive(Debug, Clone)]
pub struct RecordingSession {
    pub id: usize,
    pub source: AudioSource,
    pub output_path: String,
    pub start_time: Instant,
    pub duration_limit: Option<Duration>,
    pub samples_captured: Arc<AtomicUsize>,
    pub is_active: Arc<AtomicBool>,
    pub file_size: u64,
}

#[derive(Debug)]
pub enum AppEvent {
    StartRecording(AudioSource, String, Option<Duration>),
    StopRecording(usize),
    UpdateProgress(usize, usize, u64),
    RecordingFinished(usize),
    Error(String),
}

pub struct App {
    // UI State
    pub current_tab: usize,
    pub audio_sources: Vec<AudioSource>,
    pub audio_tree: Vec<TreeNode>,
    pub flattened_sources: Vec<(AudioSource, usize, bool)>, // (source, depth, has_children)
    pub selected_source: usize,
    pub source_list_state: ListState,

    // Recording State
    pub recording_sessions: Vec<RecordingSession>,
    pub next_session_id: usize,

    // Input State
    pub input_mode: InputMode,
    pub output_filename: String,
    pub duration_input: String,

    // UI State
    pub show_help: bool,
    pub show_duration_dialog: bool,
    pub show_process_tree: bool,
    pub tree_view: bool,
    pub error_message: Option<String>,

    // Discovery
    pub discovery: AudioSourceDiscovery,

    // Event handling
    pub event_receiver: Option<mpsc::UnboundedReceiver<AppEvent>>,
    pub event_sender: mpsc::UnboundedSender<AppEvent>,
}

#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    EditingFilename,
    EditingDuration,
}

impl App {
    pub fn new() -> Self {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        let discovery = AudioSourceDiscovery::new().unwrap_or_else(|e| {
            eprintln!("Failed to initialize audio discovery: {}", e);
            // Create a minimal discovery instance
            AudioSourceDiscovery::new().unwrap()
        });

        let mut app = Self {
            current_tab: 0,
            audio_sources: Vec::new(),
            audio_tree: Vec::new(),
            flattened_sources: Vec::new(),
            selected_source: 0,
            source_list_state: ListState::default(),
            recording_sessions: Vec::new(),
            next_session_id: 1,
            input_mode: InputMode::Normal,
            output_filename: "recording.wav".to_string(),
            duration_input: String::new(),
            show_help: false,
            show_duration_dialog: false,
            show_process_tree: false,
            tree_view: true, // Default to tree view
            error_message: None,
            discovery,
            event_receiver: Some(event_receiver),
            event_sender,
        };

        app.source_list_state.select(Some(0));
        app.discover_audio_sources();
        app
    }

    pub fn discover_audio_sources(&mut self) {
        // Discover audio sources using the new discovery system
        self.audio_sources.clear();

        match self.discovery.discover_active_audio_sources() {
            Ok(discovered_sources) => {
                // Convert discovered sources to our internal format
                for discovered in discovered_sources {
                    let source_type = match discovered.source_type {
                        DiscoveredAudioSourceType::SystemAudio => AudioSourceType::SystemAudio,
                        DiscoveredAudioSourceType::Application => AudioSourceType::Application,
                        DiscoveredAudioSourceType::ProcessTree => AudioSourceType::ProcessTree,
                    };

                    self.audio_sources.push(AudioSource {
                        id: discovered.id,
                        name: discovered.name,
                        description: discovered.description,
                        source_type,
                    });
                }

                if self.audio_sources.is_empty() {
                    // Add fallback system audio if no sources found
                    self.audio_sources.push(AudioSource {
                        id: 0,
                        name: "🔊 System Audio".to_string(),
                        description: "Capture all system audio output".to_string(),
                        source_type: AudioSourceType::SystemAudio,
                    });
                }
            }
            Err(e) => {
                // Fallback to static sources if discovery fails
                self.error_message = Some(format!("Discovery failed: {}", e));
                self.audio_sources.push(AudioSource {
                    id: 0,
                    name: "🔊 System Audio".to_string(),
                    description: "Capture all system audio output".to_string(),
                    source_type: AudioSourceType::SystemAudio,
                });

                // Add some common fallback applications
                let fallback_apps = vec![
                    (62, "🎬 VLC Media Player", "Video and audio player"),
                    (63, "🦊 Firefox", "Web browser audio"),
                    (64, "🌐 Chrome", "Web browser audio"),
                ];

                for (id, name, desc) in fallback_apps {
                    self.audio_sources.push(AudioSource {
                        id,
                        name: name.to_string(),
                        description: desc.to_string(),
                        source_type: AudioSourceType::Application,
                    });
                }
            }
        }

        // Build tree structure
        self.build_audio_tree();

        // Update selection if needed
        if self.selected_source >= self.flattened_sources.len() {
            self.selected_source = 0;
        }
        self.source_list_state.select(Some(self.selected_source));
    }

    fn build_audio_tree(&mut self) {
        self.audio_tree.clear();
        self.flattened_sources.clear();

        if self.audio_sources.is_empty() {
            return;
        }

        // Group sources by application/process
        let mut app_groups: std::collections::HashMap<String, Vec<AudioSource>> =
            std::collections::HashMap::new();
        let mut system_sources = Vec::new();

        for source in &self.audio_sources {
            match source.source_type {
                AudioSourceType::SystemAudio => {
                    system_sources.push(source.clone());
                }
                _ => {
                    // Extract application name from the display name
                    let app_name = self.extract_app_name(&source.name);
                    app_groups
                        .entry(app_name)
                        .or_insert_with(Vec::new)
                        .push(source.clone());
                }
            }
        }

        // Add system audio first
        for source in system_sources {
            self.audio_tree.push(TreeNode::new(source, 0));
        }

        // Add application groups
        for (app_name, mut sources) in app_groups {
            if sources.len() == 1 {
                // Single process - add directly
                self.audio_tree
                    .push(TreeNode::new(sources.into_iter().next().unwrap(), 0));
            } else {
                // Multiple processes - create a parent node
                sources.sort_by(|a, b| a.name.cmp(&b.name));

                let parent_source = AudioSource {
                    id: sources[0].id,
                    name: format!("📁 {} ({} processes)", app_name, sources.len()),
                    description: format!("{} audio processes", sources.len()),
                    source_type: AudioSourceType::ProcessTree,
                };

                let mut parent_node = TreeNode::new(parent_source, 0);

                for source in sources {
                    let child_source = AudioSource {
                        id: source.id,
                        name: source
                            .name
                            .replace(&format!("📱 {}", app_name), "├─")
                            .replace(&format!("🎬 {}", app_name), "├─")
                            .replace(&format!("🦊 {}", app_name), "├─")
                            .replace(&format!("🌐 {}", app_name), "├─"),
                        description: source.description,
                        source_type: source.source_type,
                    };
                    parent_node.add_child(TreeNode::new(child_source, 1));
                }

                self.audio_tree.push(parent_node);
            }
        }

        // Flatten the tree for display
        for node in &self.audio_tree {
            self.flattened_sources.extend(node.flatten());
        }
    }

    fn extract_app_name(&self, display_name: &str) -> String {
        // Extract the application name from display names like "🎬 VLC Media Player"
        if let Some(space_pos) = display_name.find(' ') {
            let after_icon = &display_name[space_pos + 1..];
            if let Some(colon_pos) = after_icon.find(':') {
                after_icon[..colon_pos].to_string()
            } else {
                after_icon.to_string()
            }
        } else {
            display_name.to_string()
        }
    }

    pub fn start_recording(&mut self) {
        let source = if self.tree_view && !self.flattened_sources.is_empty() {
            if self.selected_source >= self.flattened_sources.len() {
                self.error_message = Some("Invalid source selection".to_string());
                return;
            }
            self.flattened_sources[self.selected_source].0.clone()
        } else {
            if self.audio_sources.is_empty() {
                self.error_message = Some("No audio sources available".to_string());
                return;
            }
            if self.selected_source >= self.audio_sources.len() {
                self.error_message = Some("Invalid source selection".to_string());
                return;
            }
            self.audio_sources[self.selected_source].clone()
        };
        let output_path = if self.output_filename.is_empty() {
            format!("recording_{}.wav", self.next_session_id)
        } else {
            self.output_filename.clone()
        };

        let duration_limit = if !self.duration_input.is_empty() {
            match self.duration_input.parse::<u64>() {
                Ok(seconds) => Some(Duration::from_secs(seconds)),
                Err(_) => {
                    self.error_message = Some("Invalid duration format".to_string());
                    return;
                }
            }
        } else {
            None
        };

        let session = RecordingSession {
            id: self.next_session_id,
            source: source.clone(),
            output_path: output_path.clone(),
            start_time: Instant::now(),
            duration_limit,
            samples_captured: Arc::new(AtomicUsize::new(0)),
            is_active: Arc::new(AtomicBool::new(true)),
            file_size: 0,
        };

        self.recording_sessions.push(session.clone());
        self.next_session_id += 1;

        // Start recording in background thread
        let event_sender = self.event_sender.clone();
        let session_id = session.id;
        let samples_counter = session.samples_captured.clone();
        let is_active = session.is_active.clone();

        thread::spawn(move || {
            if let Err(e) = start_recording_thread(
                source,
                output_path,
                duration_limit,
                samples_counter,
                is_active,
                event_sender.clone(),
                session_id,
            ) {
                let _ = event_sender.send(AppEvent::Error(format!("Recording failed: {}", e)));
            }
        });

        // Reset input fields
        self.output_filename = "recording.wav".to_string();
        self.duration_input.clear();
        self.show_duration_dialog = false;
    }

    pub fn stop_recording(&mut self, session_id: usize) {
        if let Some(session) = self.recording_sessions.iter().find(|s| s.id == session_id) {
            session.is_active.store(false, Ordering::SeqCst);
        }
    }

    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::UpdateProgress(session_id, _samples, file_size) => {
                if let Some(session) = self
                    .recording_sessions
                    .iter_mut()
                    .find(|s| s.id == session_id)
                {
                    session.file_size = file_size;
                }
            }
            AppEvent::RecordingFinished(session_id) => {
                if let Some(session) = self
                    .recording_sessions
                    .iter_mut()
                    .find(|s| s.id == session_id)
                {
                    session.is_active.store(false, Ordering::SeqCst);
                }
            }
            AppEvent::Error(msg) => {
                self.error_message = Some(msg);
            }
            _ => {}
        }
    }

    pub fn next_source(&mut self) {
        let max_len = if self.tree_view && !self.flattened_sources.is_empty() {
            self.flattened_sources.len()
        } else {
            self.audio_sources.len()
        };

        if max_len > 0 {
            self.selected_source = (self.selected_source + 1) % max_len;
            self.source_list_state.select(Some(self.selected_source));
        }
    }

    pub fn previous_source(&mut self) {
        let max_len = if self.tree_view && !self.flattened_sources.is_empty() {
            self.flattened_sources.len()
        } else {
            self.audio_sources.len()
        };

        if max_len > 0 {
            self.selected_source = if self.selected_source == 0 {
                max_len - 1
            } else {
                self.selected_source - 1
            };
            self.source_list_state.select(Some(self.selected_source));
        }
    }
}

fn start_recording_thread(
    source: AudioSource,
    output_path: String,
    duration_limit: Option<Duration>,
    samples_counter: Arc<AtomicUsize>,
    is_active: Arc<AtomicBool>,
    event_sender: mpsc::UnboundedSender<AppEvent>,
    session_id: usize,
) -> Result<(), String> {
    // Create WAV writer
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let file = File::create(&output_path).map_err(|e| e.to_string())?;
    let writer = WavWriter::new(BufWriter::new(file), spec).map_err(|e| e.to_string())?;
    let writer = Arc::new(Mutex::new(Some(writer)));

    // Set up PipeWire capture
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(source.id));
    capture.discover_target_node().map_err(|e| e.to_string())?;
    capture.create_monitor_stream().map_err(|e| e.to_string())?;

    let start_time = Instant::now();
    let writer_clone = writer.clone();
    let samples_counter_clone = samples_counter.clone();
    let is_active_clone = is_active.clone();
    let event_sender_clone = event_sender.clone();

    // Start capture with callback
    capture
        .start_capture(move |samples| {
            if !is_active_clone.load(Ordering::SeqCst) {
                return;
            }

            let count = samples_counter_clone.fetch_add(samples.len(), Ordering::SeqCst);

            // Write samples to file
            if let Ok(mut writer_option) = writer_clone.lock() {
                if let Some(ref mut writer) = writer_option.as_mut() {
                    for &sample in samples {
                        let sample_i16 = (sample * i16::MAX as f32) as i16;
                        if let Err(_) = writer.write_sample(sample_i16) {
                            break;
                        }
                    }
                }
            }

            // Send progress update every 48000 samples (1 second at 48kHz)
            if count % 48000 == 0 {
                let file_size = std::fs::metadata(&output_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                let _ = event_sender_clone.send(AppEvent::UpdateProgress(
                    session_id,
                    count + samples.len(),
                    file_size,
                ));
            }
        })
        .map_err(|e| e.to_string())?;

    // Run main loop with duration check
    loop {
        if !is_active.load(Ordering::SeqCst) {
            break;
        }

        if let Some(limit) = duration_limit {
            if start_time.elapsed() >= limit {
                is_active.store(false, Ordering::SeqCst);
                break;
            }
        }

        // Run main loop iteration
        capture
            .run_main_loop_with_options(Some(Duration::from_millis(100)), false)
            .map_err(|e| e.to_string())?;
    }

    // Finalize recording
    if let Ok(mut writer_option) = writer.lock() {
        if let Some(writer) = writer_option.take() {
            let _ = writer.finalize();
        }
    }

    let _ = event_sender.send(AppEvent::RecordingFinished(session_id));
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging (set to WARN level to reduce noise in TUI)
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Warn)
        .init();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new();
    let mut event_receiver = app.event_receiver.take().unwrap();

    // Run app
    let res = run_app(&mut terminal, &mut app, &mut event_receiver);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    event_receiver: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        // Handle events with timeout
        if crossterm::event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.input_mode {
                        InputMode::Normal => {
                            match key.code {
                                KeyCode::Char('q') => return Ok(()),
                                KeyCode::Char('h') => app.show_help = !app.show_help,
                                KeyCode::Char('r') => app.discover_audio_sources(),
                                KeyCode::Up => app.previous_source(),
                                KeyCode::Down => app.next_source(),
                                KeyCode::Enter => {
                                    if app.show_duration_dialog {
                                        app.start_recording();
                                    } else {
                                        app.show_duration_dialog = true;
                                    }
                                }
                                KeyCode::Char('s') => {
                                    if !app.recording_sessions.is_empty() {
                                        let last_id = app.recording_sessions.last().unwrap().id;
                                        app.stop_recording(last_id);
                                    }
                                }
                                KeyCode::Char('f') => {
                                    app.input_mode = InputMode::EditingFilename;
                                }
                                KeyCode::Char('d') => {
                                    app.input_mode = InputMode::EditingDuration;
                                }
                                KeyCode::Char('t') => {
                                    app.tree_view = !app.tree_view;
                                    // Rebuild the tree when toggling
                                    app.build_audio_tree();
                                    // Reset selection
                                    app.selected_source = 0;
                                    app.source_list_state.select(Some(0));
                                }
                                KeyCode::Esc => {
                                    app.show_help = false;
                                    app.show_duration_dialog = false;
                                    app.show_process_tree = false;
                                    app.error_message = None;
                                }
                                _ => {}
                            }
                        }
                        InputMode::EditingFilename => match key.code {
                            KeyCode::Enter => {
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Esc => {
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Backspace => {
                                app.output_filename.pop();
                            }
                            KeyCode::Char(c) => {
                                app.output_filename.push(c);
                            }
                            _ => {}
                        },
                        InputMode::EditingDuration => match key.code {
                            KeyCode::Enter => {
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Esc => {
                                app.input_mode = InputMode::Normal;
                            }
                            KeyCode::Backspace => {
                                app.duration_input.pop();
                            }
                            KeyCode::Char(c) if c.is_ascii_digit() => {
                                app.duration_input.push(c);
                            }
                            _ => {}
                        },
                    }
                }
            }
        }

        // Handle app events
        while let Ok(event) = event_receiver.try_recv() {
            app.handle_event(event);
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // Status bar
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("🎙️ Audio Recorder TUI")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Main content area
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Left panel - Audio sources
    render_audio_sources(f, app, main_chunks[0]);

    // Right panel - Recording sessions
    render_recording_sessions(f, app, main_chunks[1]);

    // Status bar
    render_status_bar(f, app, chunks[2]);

    // Overlays
    if app.show_help {
        render_help_popup(f, app);
    }

    if app.show_duration_dialog {
        render_duration_dialog(f, app);
    }

    if let Some(ref error) = app.error_message {
        render_error_popup(f, error);
    }
}

fn render_audio_sources(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = if app.tree_view && !app.flattened_sources.is_empty() {
        // Tree view
        app.flattened_sources
            .iter()
            .enumerate()
            .map(|(i, (source, depth, has_children))| {
                let style = if i == app.selected_source {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let indent = "  ".repeat(*depth);
                let tree_prefix = if *depth > 0 { "├─ " } else { "" };

                let icon = if *depth == 0 {
                    match source.source_type {
                        AudioSourceType::SystemAudio => "🔊",
                        AudioSourceType::Application => "📱",
                        AudioSourceType::ProcessTree => "📁",
                    }
                } else {
                    "  " // No icon for child items
                };

                let display_name = if *depth > 0 {
                    // For child nodes, clean up the name
                    source.name.replace("├─", "").trim().to_string()
                } else {
                    source.name.clone()
                };

                ListItem::new(Line::from(vec![
                    Span::raw(format!("{}{}", indent, tree_prefix)),
                    Span::raw(icon),
                    Span::raw(" "),
                    Span::styled(display_name, style),
                ]))
            })
            .collect()
    } else {
        // Flat view (fallback)
        app.audio_sources
            .iter()
            .enumerate()
            .map(|(i, source)| {
                let style = if i == app.selected_source {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                let icon = match source.source_type {
                    AudioSourceType::SystemAudio => "🔊",
                    AudioSourceType::Application => "📱",
                    AudioSourceType::ProcessTree => "🌳",
                };

                ListItem::new(Line::from(vec![
                    Span::raw(icon),
                    Span::raw(" "),
                    Span::styled(&source.name, style),
                ]))
            })
            .collect()
    };

    let title = if app.tree_view {
        "🌳 Audio Sources - Tree View (↑↓ to select, Enter to record, t to toggle view)"
    } else {
        "📋 Audio Sources - List View (↑↓ to select, Enter to record, t to toggle view)"
    };

    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray));

    f.render_stateful_widget(list, area, &mut app.source_list_state.clone());
}

fn render_recording_sessions(f: &mut Frame, app: &App, area: Rect) {
    let sessions_text = if app.recording_sessions.is_empty() {
        vec![Line::from("No active recordings")]
    } else {
        app.recording_sessions
            .iter()
            .map(|session| {
                let duration = session.start_time.elapsed();
                let samples = session.samples_captured.load(Ordering::SeqCst);
                let status = if session.is_active.load(Ordering::SeqCst) {
                    "🔴 Recording"
                } else {
                    "⏹️ Stopped"
                };

                Line::from(vec![
                    Span::raw(format!("#{} ", session.id)),
                    Span::styled(status, Style::default().fg(Color::Red)),
                    Span::raw(format!(" {} ", session.source.name)),
                    Span::raw(format!(
                        "({:.1}s, {} samples)",
                        duration.as_secs_f32(),
                        samples
                    )),
                ])
            })
            .collect()
    };

    let sessions = Paragraph::new(sessions_text)
        .block(
            Block::default()
                .title("Recording Sessions (s to stop)")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(sessions, area);
}

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let status_text = match app.input_mode {
        InputMode::Normal => {
            format!(
                "File: {} | Duration: {} | Press 'h' for help, 'q' to quit",
                app.output_filename,
                if app.duration_input.is_empty() {
                    "unlimited"
                } else {
                    &app.duration_input
                }
            )
        }
        InputMode::EditingFilename => {
            "Editing filename... (Enter to confirm, Esc to cancel)".to_string()
        }
        InputMode::EditingDuration => {
            "Editing duration in seconds... (Enter to confirm, Esc to cancel)".to_string()
        }
    };

    let status = Paragraph::new(status_text)
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(status, area);
}

fn render_help_popup(f: &mut Frame, _app: &App) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from("🎙️ Audio Recorder TUI - Help"),
        Line::from(""),
        Line::from("Navigation:"),
        Line::from("  ↑/↓     - Select audio source"),
        Line::from("  Enter   - Start recording"),
        Line::from("  s       - Stop last recording"),
        Line::from(""),
        Line::from("Configuration:"),
        Line::from("  f       - Edit output filename"),
        Line::from("  d       - Edit duration (seconds)"),
        Line::from("  r       - Refresh audio sources"),
        Line::from("  t       - Toggle tree/list view"),
        Line::from(""),
        Line::from("Audio Sources:"),
        Line::from("  🔊      - System audio"),
        Line::from("  🎬🦊🌐  - Applications with audio"),
        Line::from("  📱      - Other processes"),
        Line::from(""),
        Line::from("Other:"),
        Line::from("  h       - Toggle this help"),
        Line::from("  Esc     - Close dialogs"),
        Line::from("  q       - Quit application"),
    ];

    let help = Paragraph::new(help_text)
        .block(Block::default().title("Help").borders(Borders::ALL))
        .style(Style::default().fg(Color::White));

    f.render_widget(help, area);
}

fn render_duration_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(40, 20, f.area());
    f.render_widget(Clear, area);

    let text = vec![
        Line::from("Start Recording?"),
        Line::from(""),
        Line::from(format!(
            "Source: {}",
            app.audio_sources[app.selected_source].name
        )),
        Line::from(format!("File: {}", app.output_filename)),
        Line::from(format!(
            "Duration: {}",
            if app.duration_input.is_empty() {
                "unlimited".to_string()
            } else {
                format!("{}s", app.duration_input)
            }
        )),
        Line::from(""),
        Line::from("Press Enter to start, Esc to cancel"),
    ];

    let dialog = Paragraph::new(text)
        .block(
            Block::default()
                .title("Confirm Recording")
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(dialog, area);
}

fn render_error_popup(f: &mut Frame, error: &str) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);

    let error_text = vec![
        Line::from("❌ Error"),
        Line::from(""),
        Line::from(error),
        Line::from(""),
        Line::from("Press Esc to close"),
    ];

    let error_popup = Paragraph::new(error_text)
        .block(Block::default().title("Error").borders(Borders::ALL))
        .style(Style::default().fg(Color::Red));

    f.render_widget(error_popup, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

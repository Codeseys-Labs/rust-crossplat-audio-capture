//! AudioGraph — Real-time audio capture → transcription → knowledge graph
//!
//! This is the Tauri backend for the AudioGraph application.
//! Module structure:
//!   state     — AppState definition (Arc<Mutex<...>>)
//!   commands  — Tauri IPC command handlers
//!   events    — Event name constants and payload types
//!   audio     — Audio capture manager + processing pipeline
//!   asr       — Automatic speech recognition (whisper-rs)
//!   diarization — Speaker diarization (pyannote-rs)
//!   graph     — Temporal knowledge graph (petgraph)
//!   models    — Model management and downloading

pub mod asr;
pub mod audio;
pub mod commands;
pub mod diarization;
pub mod events;
pub mod graph;
pub mod llm;
pub mod models;
pub mod settings;
pub mod speech;
pub mod state;

use state::AppState;

/// Initialize and run the Tauri application.
pub fn run() {
    env_logger::init();

    let app_state = AppState::new();

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::list_audio_sources,
            commands::start_capture,
            commands::stop_capture,
            commands::get_graph_snapshot,
            commands::get_transcript,
            commands::get_pipeline_status,
            commands::send_chat_message,
            commands::get_chat_history,
            commands::clear_chat_history,
            commands::list_available_models,
            commands::download_model_cmd,
            commands::get_model_status,
            commands::load_llm_model,
            commands::configure_api_endpoint,
            commands::load_settings_cmd,
            commands::save_settings_cmd,
            commands::delete_model_cmd,
            commands::list_running_processes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AudioGraph");
}

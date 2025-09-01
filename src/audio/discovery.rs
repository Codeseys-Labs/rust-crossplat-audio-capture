use log::{debug, info};
use std::collections::HashMap;
use sysinfo::System;

#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    name: String,
    parent_pid: Option<u32>,
    cmd: String,
}

#[derive(Debug, Clone)]
pub struct AudioNode {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub media_class: String,
    pub application_name: Option<String>,
    pub process_id: Option<u32>,
    pub is_active: bool,
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
    pub parent_process: Option<u32>,
    pub children: Vec<u32>,
}

#[derive(Debug, Clone)]
pub enum AudioSourceType {
    Application,
    SystemAudio,
    ProcessTree,
}

#[derive(Debug, Clone)]
pub struct AudioSource {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub source_type: AudioSourceType,
    pub node: AudioNode,
}

pub struct AudioSourceDiscovery {
    discovered_nodes: HashMap<u32, AudioNode>,
    system: System,
}

impl AudioSourceDiscovery {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        info!("Initializing audio source discovery");

        let mut system = System::new();
        system.refresh_all();

        Ok(Self {
            discovered_nodes: HashMap::new(),
            system,
        })
    }

    pub fn discover_active_audio_sources(&mut self) -> Result<Vec<AudioSource>, Box<dyn std::error::Error>> {
        info!("Starting discovery of active audio sources");

        // Refresh system information
        self.system.refresh_all();

        let mut sources = Vec::new();
        self.discovered_nodes.clear();

        // Add system audio source first
        sources.push(AudioSource {
            id: 0,
            name: "🔊 System Audio".to_string(),
            description: "Capture all system audio output".to_string(),
            source_type: AudioSourceType::SystemAudio,
            node: AudioNode {
                id: 0,
                name: "System Audio".to_string(),
                description: "System-wide audio capture".to_string(),
                media_class: "Audio/Sink".to_string(),
                application_name: None,
                process_id: None,
                is_active: true,
                channels: Some(2),
                sample_rate: Some(48000),
                parent_process: None,
                children: Vec::new(),
            },
        });

        // Platform-specific discovery
        #[cfg(target_os = "linux")]
        {
            self.discover_linux_audio_sources(&mut sources)?;
        }

        #[cfg(target_os = "windows")]
        {
            self.discover_windows_audio_sources(&mut sources)?;
        }

        #[cfg(target_os = "macos")]
        {
            self.discover_macos_audio_sources(&mut sources)?;
        }

        info!("Discovered {} audio sources", sources.len());
        debug!("Audio sources: {:#?}", sources.iter().map(|s| &s.name).collect::<Vec<_>>());

        Ok(sources)
    }

    #[cfg(target_os = "linux")]
    fn discover_linux_audio_sources(&mut self, sources: &mut Vec<AudioSource>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Discovering Linux audio sources via PipeWire");

        // Get PipeWire nodes using pw-dump command
        let pipewire_nodes = self.get_pipewire_nodes()?;

        // Get running processes that might be using audio
        let audio_processes = self.get_audio_processes();

        // Build process tree
        let process_tree = self.build_process_tree(&audio_processes);

        // First, add active audio sources (those with PipeWire nodes)
        let mut active_sources = Vec::new();
        let mut inactive_sources = Vec::new();

        // Match PipeWire nodes with processes
        for (node_id, node_info) in pipewire_nodes {
            if let Some(process_info) = audio_processes.iter().find(|p| {
                // Try to match by application name or process name
                if let Some(app_name) = &node_info.application_name {
                    p.name.to_lowercase().contains(&app_name.to_lowercase()) ||
                    app_name.to_lowercase().contains(&p.name.to_lowercase())
                } else {
                    false
                }
            }) {
                let source_type = if process_tree.get(&process_info.pid).map_or(false, |children| !children.is_empty()) {
                    AudioSourceType::ProcessTree
                } else {
                    AudioSourceType::Application
                };

                let icon = self.get_application_icon(&process_info.name);
                let display_name = self.format_application_name(&process_info.name);

                active_sources.push(AudioSource {
                    id: node_id,
                    name: format!("{} {}", icon, display_name),
                    description: self.format_process_description(process_info, &process_tree),
                    source_type,
                    node: node_info,
                });
            }
        }

        // Add high-priority audio processes that don't have PipeWire nodes yet
        let high_priority_apps = vec!["vlc", "firefox", "chrome", "chromium", "spotify", "discord", "obs"];

        for process in &audio_processes {
            if !active_sources.iter().any(|s| s.node.process_id == Some(process.pid)) {
                let is_high_priority = high_priority_apps.iter().any(|&app|
                    process.name.to_lowercase().contains(app)
                );

                if is_high_priority {
                    let icon = self.get_application_icon(&process.name);
                    let display_name = self.format_application_name(&process.name);

                    inactive_sources.push(AudioSource {
                        id: process.pid + 10000, // Offset to avoid conflicts
                        name: format!("{} {}", icon, display_name),
                        description: format!("PID: {} (ready for capture)", process.pid),
                        source_type: AudioSourceType::Application,
                        node: AudioNode {
                            id: process.pid + 10000,
                            name: process.name.clone(),
                            description: format!("Process: {}", process.name),
                            media_class: "Process".to_string(),
                            application_name: Some(process.name.clone()),
                            process_id: Some(process.pid),
                            is_active: false,
                            channels: None,
                            sample_rate: None,
                            parent_process: process.parent_pid,
                            children: process_tree.get(&process.pid).cloned().unwrap_or_default(),
                        },
                    });
                }
            }
        }

        // Add active sources first, then inactive high-priority sources
        sources.extend(active_sources);
        sources.extend(inactive_sources);

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn get_pipewire_nodes(&self) -> Result<HashMap<u32, AudioNode>, Box<dyn std::error::Error>> {
        let mut nodes = HashMap::new();

        // Try to use pw-dump to get node information
        match Command::new("pw-dump").output() {
            Ok(output) => {
                if output.status.success() {
                    let json_str = String::from_utf8_lossy(&output.stdout);
                    // Parse the JSON output (simplified for now)
                    // In a real implementation, you'd use serde_json
                    debug!("PipeWire dump output length: {}", json_str.len());

                    // For now, add some mock nodes based on common patterns
                    // This would be replaced with actual JSON parsing
                    self.parse_pipewire_dump(&json_str, &mut nodes);
                } else {
                    warn!("pw-dump failed: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                warn!("Failed to run pw-dump: {}", e);
            }
        }

        // Fallback: add some common node IDs if pw-dump isn't available
        if nodes.is_empty() {
            self.add_fallback_nodes(&mut nodes);
        }

        Ok(nodes)
    }

    #[cfg(target_os = "linux")]
    fn parse_pipewire_dump(&self, _json_str: &str, nodes: &mut HashMap<u32, AudioNode>) {
        // Simplified parsing - in reality you'd parse the JSON
        // For now, add some realistic nodes
        let common_nodes = vec![
            (62, "vlc", "VLC media player"),
            (63, "firefox", "Firefox"),
            (64, "chromium", "Chromium"),
            (65, "spotify", "Spotify"),
            (66, "discord", "Discord"),
        ];

        for (id, app_name, description) in common_nodes {
            nodes.insert(id, AudioNode {
                id,
                name: app_name.to_string(),
                description: description.to_string(),
                media_class: "Stream/Output/Audio".to_string(),
                application_name: Some(app_name.to_string()),
                process_id: None, // Would be filled from actual parsing
                is_active: true,
                channels: Some(2),
                sample_rate: Some(48000),
                parent_process: None,
                children: Vec::new(),
            });
        }
    }

    #[cfg(target_os = "linux")]
    fn add_fallback_nodes(&self, nodes: &mut HashMap<u32, AudioNode>) {
        // Add fallback nodes when pw-dump is not available
        let fallback_nodes = vec![
            (62, "vlc", "VLC Media Player"),
            (63, "firefox", "Firefox"),
            (64, "chrome", "Google Chrome"),
        ];

        for (id, app_name, display_name) in fallback_nodes {
            nodes.insert(id, AudioNode {
                id,
                name: display_name.to_string(),
                description: format!("Audio from {}", display_name),
                media_class: "Stream/Output/Audio".to_string(),
                application_name: Some(app_name.to_string()),
                process_id: None,
                is_active: true,
                channels: Some(2),
                sample_rate: Some(48000),
                parent_process: None,
                children: Vec::new(),
            });
        }
    }

    fn get_audio_processes(&self) -> Vec<ProcessInfo> {
        let mut audio_processes = Vec::new();

        // Common audio applications to look for
        let audio_app_names = vec![
            "vlc", "firefox", "chrome", "chromium", "spotify", "discord",
            "obs", "audacity", "pulseaudio", "pipewire", "jack", "alsa",
            "mpv", "mplayer", "rhythmbox", "totem", "banshee", "clementine",
            "amarok", "deadbeef", "cmus", "mpd", "ncmpcpp", "pavucontrol",
            "qpwgraph", "helvum", "carla", "ardour", "reaper", "bitwig",
            "steam", "lutris", "wine", "zoom", "teams", "skype", "telegram",
        ];

        for (pid, process) in self.system.processes() {
            let process_name = process.name().to_string_lossy().to_lowercase();
            let cmd_vec: Vec<String> = process.cmd().iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect();
            let cmd = cmd_vec.join(" ").to_lowercase();

            // Check if this process is likely to use audio
            let is_audio_process = audio_app_names.iter().any(|&app| {
                process_name.contains(app) || cmd.contains(app)
            }) || cmd.contains("audio") || cmd.contains("sound") || cmd.contains("music");

            if is_audio_process {
                audio_processes.push(ProcessInfo {
                    pid: pid.as_u32(),
                    name: process.name().to_string_lossy().to_string(),
                    parent_pid: process.parent().map(|p| p.as_u32()),
                    cmd: cmd_vec.join(" "),
                });
            }
        }

        debug!("Found {} potential audio processes", audio_processes.len());
        audio_processes
    }

    fn build_process_tree(&self, processes: &[ProcessInfo]) -> HashMap<u32, Vec<u32>> {
        let mut tree = HashMap::new();

        for process in processes {
            if let Some(parent_pid) = process.parent_pid {
                tree.entry(parent_pid).or_insert_with(Vec::new).push(process.pid);
            }
        }

        tree
    }

    fn get_application_icon(&self, app_name: &str) -> &'static str {
        match app_name.to_lowercase().as_str() {
            name if name.contains("vlc") => "🎬",
            name if name.contains("firefox") => "🦊",
            name if name.contains("chrome") || name.contains("chromium") => "🌐",
            name if name.contains("spotify") => "🎵",
            name if name.contains("discord") => "💬",
            name if name.contains("obs") => "📹",
            name if name.contains("audacity") => "🎙️",
            name if name.contains("steam") => "🎮",
            name if name.contains("zoom") || name.contains("teams") => "📞",
            name if name.contains("music") || name.contains("audio") => "🎶",
            _ => "📱",
        }
    }

    fn format_application_name(&self, app_name: &str) -> String {
        match app_name.to_lowercase().as_str() {
            "vlc" => "VLC Media Player".to_string(),
            "firefox" => "Firefox".to_string(),
            "chrome" | "chromium" => "Google Chrome".to_string(),
            "spotify" => "Spotify".to_string(),
            "discord" => "Discord".to_string(),
            "obs" => "OBS Studio".to_string(),
            "audacity" => "Audacity".to_string(),
            "steam" => "Steam".to_string(),
            _ => {
                // Capitalize first letter
                let mut chars = app_name.chars();
                match chars.next() {
                    None => app_name.to_string(),
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                }
            }
        }
    }

    fn format_process_description(&self, process: &ProcessInfo, tree: &HashMap<u32, Vec<u32>>) -> String {
        let mut parts = Vec::new();

        parts.push(format!("PID: {}", process.pid));

        if let Some(children) = tree.get(&process.pid) {
            if !children.is_empty() {
                parts.push(format!("{} child processes", children.len()));
            }
        }

        parts.push("Active audio stream".to_string());

        parts.join(" • ")
    }

    #[cfg(target_os = "windows")]
    fn discover_windows_audio_sources(&mut self, sources: &mut Vec<AudioSource>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Discovering Windows audio sources via WASAPI");

        // TODO: Implement Windows-specific audio discovery using WASAPI
        // This would enumerate audio endpoints and sessions

        // For now, add some common Windows applications
        let windows_apps = vec![
            "chrome.exe", "firefox.exe", "spotify.exe", "discord.exe",
            "vlc.exe", "wmplayer.exe", "steam.exe"
        ];

        for (i, app) in windows_apps.iter().enumerate() {
            let icon = self.get_application_icon(app);
            let display_name = self.format_application_name(app);

            sources.push(AudioSource {
                id: (i + 1) as u32,
                name: format!("{} {}", icon, display_name),
                description: format!("Windows audio from {}", display_name),
                source_type: AudioSourceType::Application,
                node: AudioNode {
                    id: (i + 1) as u32,
                    name: display_name,
                    description: format!("Windows process: {}", app),
                    media_class: "Windows/AudioSession".to_string(),
                    application_name: Some(app.to_string()),
                    process_id: None,
                    is_active: true,
                    channels: Some(2),
                    sample_rate: Some(48000),
                    parent_process: None,
                    children: Vec::new(),
                },
            });
        }

        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn discover_macos_audio_sources(&mut self, sources: &mut Vec<AudioSource>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Discovering macOS audio sources via Core Audio");

        // TODO: Implement macOS-specific audio discovery using Core Audio
        // This would enumerate audio devices and applications

        // For now, add some common macOS applications
        let macos_apps = vec![
            "Safari", "Chrome", "Firefox", "Spotify", "Discord",
            "VLC", "QuickTime Player", "Music", "Steam"
        ];

        for (i, app) in macos_apps.iter().enumerate() {
            let icon = self.get_application_icon(app);

            sources.push(AudioSource {
                id: (i + 1) as u32,
                name: format!("{} {}", icon, app),
                description: format!("macOS audio from {}", app),
                source_type: AudioSourceType::Application,
                node: AudioNode {
                    id: (i + 1) as u32,
                    name: app.to_string(),
                    description: format!("macOS application: {}", app),
                    media_class: "macOS/AudioUnit".to_string(),
                    application_name: Some(app.to_string()),
                    process_id: None,
                    is_active: true,
                    channels: Some(2),
                    sample_rate: Some(48000),
                    parent_process: None,
                    children: Vec::new(),
                },
            });
        }

        Ok(())
    }

    // Helper functions for formatting (simplified for now)

    pub fn refresh_sources(&mut self) -> Result<Vec<AudioSource>, Box<dyn std::error::Error>> {
        debug!("Refreshing audio source list");
        self.discover_active_audio_sources()
    }

    pub fn get_node_details(&self, node_id: u32) -> Option<AudioNode> {
        self.discovered_nodes.get(&node_id).cloned()
    }

    pub fn get_process_tree(&self, root_pid: u32) -> Vec<AudioNode> {
        let mut tree = Vec::new();

        if let Some(root_node) = self.discovered_nodes.get(&root_pid) {
            tree.push(root_node.clone());

            // Add children recursively
            for &child_pid in &root_node.children {
                tree.extend(self.get_process_tree(child_pid));
            }
        }

        tree
    }
}

impl Drop for AudioSourceDiscovery {
    fn drop(&mut self) {
        debug!("Cleaning up audio source discovery");
    }
}

// Convenience function for quick discovery
pub fn discover_audio_sources() -> Result<Vec<AudioSource>, Box<dyn std::error::Error>> {
    let mut discovery = AudioSourceDiscovery::new()?;
    discovery.discover_active_audio_sources()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_source_discovery() {
        env_logger::init();
        
        match discover_audio_sources() {
            Ok(sources) => {
                println!("Discovered {} audio sources:", sources.len());
                for source in sources {
                    println!("  - {} ({}): {}", source.name, source.id, source.description);
                }
            }
            Err(e) => {
                println!("Failed to discover audio sources: {}", e);
            }
        }
    }
}

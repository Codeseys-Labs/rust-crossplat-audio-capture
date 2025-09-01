//! Linux audio implementation using PipeWire

pub mod pipewire;

// Re-export for convenience
pub use pipewire::{PipeWireApplicationCapture, ApplicationSelector};

// Stub implementation for LinuxDeviceEnumerator to fix compilation
use crate::{AudioApplication, AudioCaptureStream, Result};
use crate::core::config::StreamConfig;

pub struct LinuxDeviceEnumerator;

impl LinuxDeviceEnumerator {
    pub fn new() -> Self {
        LinuxDeviceEnumerator
    }

    pub fn list_applications(&self) -> Result<Vec<AudioApplication>> {
        Ok(vec![])
    }

    pub fn capture_application(
        &self,
        _app: &AudioApplication,
        _config: StreamConfig,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "Linux application capture not yet fully implemented".to_string(),
        ).into())
    }

    /// Get PipeWire devices using pw-cli
    fn get_pipewire_devices(&self) -> crate::core::error::Result<Vec<LinuxAudioDevice>> {
        let mut devices = Vec::new();

        // Try to get devices using pw-cli list-objects
        if let Ok(output) = std::process::Command::new("pw-cli")
            .args(&["list-objects", "Node"])
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                devices.extend(self.parse_pw_cli_nodes(&output_str));
            }
        }

        // If no devices found via pw-cli, try pw-dump
        if devices.is_empty() {
            if let Ok(output) = std::process::Command::new("pw-dump")
                .output()
            {
                if output.status.success() {
                    let output_str = String::from_utf8_lossy(&output.stdout);
                    devices.extend(self.parse_pw_dump_nodes(&output_str));
                }
            }
        }

        Ok(devices)
    }

    /// Get the default PipeWire device for a given kind
    fn get_pipewire_default_device(&self, kind: crate::core::interface::DeviceKind) -> crate::core::error::Result<LinuxAudioDevice> {
        // Try to get default device info from pw-metadata
        if let Ok(output) = std::process::Command::new("pw-metadata")
            .args(&["-n", "settings"])
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                if let Some(device) = self.parse_default_device(&output_str, kind) {
                    return Ok(device);
                }
            }
        }

        // Fallback: try to find any suitable device from our enumerated list
        let devices = self.get_pipewire_devices()?;
        match kind {
            crate::core::interface::DeviceKind::Input => {
                if let Some(device) = devices.into_iter().find(|d| d.is_input) {
                    return Ok(device);
                }
            }
            crate::core::interface::DeviceKind::Output => {
                if let Some(device) = devices.into_iter().find(|d| d.is_output) {
                    return Ok(device);
                }
            }
        }

        Err(crate::core::error::AudioError::DeviceNotFound.into())
    }

    /// Parse pw-cli list-objects Node output
    fn parse_pw_cli_nodes(&self, output: &str) -> Vec<LinuxAudioDevice> {
        let mut devices = Vec::new();
        let mut current_node: Option<(String, String)> = None; // (id, name)
        let mut is_audio_source = false;
        let mut is_audio_sink = false;

        for line in output.lines() {
            let line = line.trim();

            // Look for node ID and name
            if line.starts_with("id ") {
                if let Some(id) = line.split_whitespace().nth(1) {
                    current_node = Some((id.trim_matches(',').to_string(), String::new()));
                }
            } else if line.contains("node.name") {
                if let Some((id, _)) = &current_node {
                    if let Some(name_part) = line.split('"').nth(1) {
                        current_node = Some((id.clone(), name_part.to_string()));
                    }
                }
            } else if line.contains("media.class") {
                if line.contains("Audio/Source") {
                    is_audio_source = true;
                } else if line.contains("Audio/Sink") {
                    is_audio_sink = true;
                }
            } else if line.starts_with("}") {
                // End of node definition
                if let Some((id, name)) = current_node.take() {
                    if is_audio_source || is_audio_sink {
                        devices.push(LinuxAudioDevice {
                            id,
                            name: if name.is_empty() { format!("PipeWire Device") } else { name },
                            is_input: is_audio_source,
                            is_output: is_audio_sink,
                        });
                    }
                }
                is_audio_source = false;
                is_audio_sink = false;
            }
        }

        devices
    }

    /// Parse pw-dump JSON output (simplified)
    fn parse_pw_dump_nodes(&self, output: &str) -> Vec<LinuxAudioDevice> {
        let mut devices = Vec::new();

        // Simple JSON-like parsing for pw-dump output
        // In a production system, you'd use a proper JSON parser
        for line in output.lines() {
            if line.contains("\"type\": \"PipeWire:Interface:Node\"") {
                // This is a node, look for audio-related properties in the surrounding lines
                if let Some(device) = self.extract_device_from_dump_context(output, line) {
                    devices.push(device);
                }
            }
        }

        devices
    }

    /// Extract device info from pw-dump context around a node line
    fn extract_device_from_dump_context(&self, full_output: &str, node_line: &str) -> Option<LinuxAudioDevice> {
        // Find the position of this line in the full output
        let lines: Vec<&str> = full_output.lines().collect();
        let node_line_index = lines.iter().position(|&line| line == node_line)?;

        // Look in a window around this line for relevant properties
        let start = node_line_index.saturating_sub(20);
        let end = (node_line_index + 20).min(lines.len());

        let mut id = None;
        let mut name = None;
        let mut media_class = None;

        for i in start..end {
            let line = lines[i].trim();

            if line.contains("\"id\":") {
                if let Some(id_str) = line.split(':').nth(1) {
                    id = Some(id_str.trim().trim_matches(',').trim_matches('"').to_string());
                }
            } else if line.contains("\"node.name\":") {
                if let Some(name_str) = line.split(':').nth(1) {
                    name = Some(name_str.trim().trim_matches(',').trim_matches('"').to_string());
                }
            } else if line.contains("\"media.class\":") {
                if let Some(class_str) = line.split(':').nth(1) {
                    media_class = Some(class_str.trim().trim_matches(',').trim_matches('"').to_string());
                }
            }
        }

        if let (Some(id), Some(media_class)) = (id, media_class) {
            let is_source = media_class.contains("Audio/Source");
            let is_sink = media_class.contains("Audio/Sink");

            if is_source || is_sink {
                return Some(LinuxAudioDevice {
                    id,
                    name: name.unwrap_or_else(|| "PipeWire Device".to_string()),
                    is_input: is_source,
                    is_output: is_sink,
                });
            }
        }

        None
    }

    /// Parse default device from pw-metadata output
    fn parse_default_device(&self, output: &str, kind: crate::core::interface::DeviceKind) -> Option<LinuxAudioDevice> {
        use crate::core::interface::DeviceKind;

        let target_key = match kind {
            DeviceKind::Input => "default.audio.source",
            DeviceKind::Output => "default.audio.sink",
        };

        for line in output.lines() {
            if line.contains(target_key) {
                if let Some(id_part) = line.split_whitespace().last() {
                    let id = id_part.trim_matches('"').to_string();
                    return Some(LinuxAudioDevice {
                        id: id.clone(),
                        name: format!("Default {} Device", if kind == DeviceKind::Input { "Input" } else { "Output" }),
                        is_input: kind == DeviceKind::Input,
                        is_output: kind == DeviceKind::Output,
                    });
                }
            }
        }

        None
    }
}

// Simple Linux audio device implementation
#[derive(Debug, Clone)]
pub struct LinuxAudioDevice {
    pub id: String,
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
}

impl crate::core::interface::AudioDevice for LinuxAudioDevice {
    type DeviceId = String;

    fn get_id(&self) -> Self::DeviceId {
        self.id.clone()
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_supported_formats(&self) -> crate::core::error::Result<Vec<crate::core::config::AudioFormat>> {
        Ok(vec![])
    }

    fn get_default_format(&self) -> crate::core::error::Result<crate::core::config::AudioFormat> {
        Ok(crate::core::config::AudioFormat {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            sample_format: crate::core::config::SampleFormat::S16LE,
        })
    }

    fn is_input(&self) -> bool {
        self.is_input
    }

    fn is_output(&self) -> bool {
        self.is_output
    }

    fn is_active(&self) -> bool {
        false
    }

    fn is_format_supported(&self, _format: &crate::core::config::AudioFormat) -> crate::core::error::Result<bool> {
        Ok(false)
    }

    fn create_stream(&mut self, _config: &crate::api::AudioCaptureConfig) -> crate::core::error::Result<Box<dyn crate::core::interface::CapturingStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "Linux audio streams not yet implemented".to_string(),
        ))
    }
}

// Implement DeviceEnumerator trait
impl crate::core::interface::DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    fn enumerate_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        let mut devices = Vec::new();

        // Try to get devices from PipeWire
        if let Ok(pw_devices) = self.get_pipewire_devices() {
            devices.extend(pw_devices);
        }

        // If no devices found, add a fallback default
        if devices.is_empty() {
            devices.push(LinuxAudioDevice {
                id: "default".to_string(),
                name: "Default Linux Audio Device".to_string(),
                is_input: true, // Mark as input device
                is_output: false,
            });
        }

        Ok(devices)
    }

    fn get_default_device(&self, kind: crate::core::interface::DeviceKind) -> crate::core::error::Result<Self::Device> {
        // Try to get the actual default device from PipeWire
        if let Ok(device) = self.get_pipewire_default_device(kind) {
            return Ok(device);
        }

        // Fallback to a generic default
        match kind {
            crate::core::interface::DeviceKind::Input => {
                Ok(LinuxAudioDevice {
                    id: "default_input".to_string(),
                    name: "Default Linux Input Device".to_string(),
                    is_input: true,
                    is_output: false,
                })
            }
            crate::core::interface::DeviceKind::Output => {
                Ok(LinuxAudioDevice {
                    id: "default_output".to_string(),
                    name: "Default Linux Output Device".to_string(),
                    is_input: false,
                    is_output: true,
                })
            }
        }
    }

    fn get_input_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        let all_devices = self.enumerate_devices()?;
        Ok(all_devices.into_iter().filter(|d| d.is_input).collect())
    }

    fn get_output_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        let all_devices = self.enumerate_devices()?;
        Ok(all_devices.into_iter().filter(|d| d.is_output).collect())
    }

    fn get_device_by_id(&self, id: &String) -> crate::core::error::Result<Self::Device> {
        // Try to find the device in our enumerated list first
        let devices = self.enumerate_devices()?;
        if let Some(device) = devices.into_iter().find(|d| &d.id == id) {
            return Ok(device);
        }

        // Fallback to creating a device with the given ID
        Ok(LinuxAudioDevice {
            id: id.clone(),
            name: format!("Linux Audio Device {}", id),
            is_input: true, // Assume input by default for capture
            is_output: false,
        })
    }
}

// Stub for PipeWireBackend
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> crate::core::error::Result<Self> {
        Ok(PipeWireBackend)
    }

    pub fn is_available() -> bool {
        false // Simplified for now
    }
}

// Implement AudioCaptureBackend trait
impl crate::audio::core::AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }

    fn list_applications(&self) -> crate::core::error::Result<Vec<AudioApplication>> {
        Ok(vec![])
    }

    fn capture_application(
        &self,
        _app: &AudioApplication,
        _config: StreamConfig,
    ) -> crate::core::error::Result<Box<dyn crate::audio::core::AudioCaptureStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "PipeWire backend not yet implemented".to_string(),
        ))
    }
}

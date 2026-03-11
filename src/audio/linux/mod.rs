//! Linux audio implementation using PipeWire

pub mod pipewire;

// Re-export for convenience
pub use pipewire::{ApplicationSelector, PipeWireApplicationCapture};

pub struct LinuxDeviceEnumerator;

impl Default for LinuxDeviceEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxDeviceEnumerator {
    pub fn new() -> Self {
        LinuxDeviceEnumerator
    }

    /// Get PipeWire devices using pw-cli
    fn get_pipewire_devices(&self) -> crate::core::error::Result<Vec<LinuxAudioDevice>> {
        let mut devices = Vec::new();

        // Try to get devices using pw-cli list-objects
        if let Ok(output) = std::process::Command::new("pw-cli")
            .args(["list-objects", "Node"])
            .output()
        {
            if output.status.success() {
                let output_str = String::from_utf8_lossy(&output.stdout);
                devices.extend(self.parse_pw_cli_nodes(&output_str));
            }
        }

        // If no devices found via pw-cli, try pw-dump
        if devices.is_empty() {
            if let Ok(output) = std::process::Command::new("pw-dump").output() {
                if output.status.success() {
                    let output_str = String::from_utf8_lossy(&output.stdout);
                    devices.extend(self.parse_pw_dump_nodes(&output_str));
                }
            }
        }

        Ok(devices)
    }

    /// Get the default PipeWire device for a given kind
    fn get_pipewire_default_device(
        &self,
        kind: crate::core::interface::DeviceKind,
    ) -> crate::core::error::Result<LinuxAudioDevice> {
        // Try to get default device info from pw-metadata
        if let Ok(output) = std::process::Command::new("pw-metadata")
            .args(["-n", "settings"])
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

        Err(crate::core::error::AudioError::DeviceNotFound {
            device_id: format!("default_{:?}", kind),
        })
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
                            name: if name.is_empty() {
                                "PipeWire Device".to_string()
                            } else {
                                name
                            },
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
    fn extract_device_from_dump_context(
        &self,
        full_output: &str,
        node_line: &str,
    ) -> Option<LinuxAudioDevice> {
        // Find the position of this line in the full output
        let lines: Vec<&str> = full_output.lines().collect();
        let node_line_index = lines.iter().position(|&line| line == node_line)?;

        // Look in a window around this line for relevant properties
        let start = node_line_index.saturating_sub(20);
        let end = (node_line_index + 20).min(lines.len());

        let mut id = None;
        let mut name = None;
        let mut media_class = None;

        for line in lines.iter().take(end).skip(start) {
            let line = line.trim();

            if line.contains("\"id\":") {
                if let Some(id_str) = line.split(':').nth(1) {
                    id = Some(
                        id_str
                            .trim()
                            .trim_matches(',')
                            .trim_matches('"')
                            .to_string(),
                    );
                }
            } else if line.contains("\"node.name\":") {
                if let Some(name_str) = line.split(':').nth(1) {
                    name = Some(
                        name_str
                            .trim()
                            .trim_matches(',')
                            .trim_matches('"')
                            .to_string(),
                    );
                }
            } else if line.contains("\"media.class\":") {
                if let Some(class_str) = line.split(':').nth(1) {
                    media_class = Some(
                        class_str
                            .trim()
                            .trim_matches(',')
                            .trim_matches('"')
                            .to_string(),
                    );
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
    fn parse_default_device(
        &self,
        output: &str,
        kind: crate::core::interface::DeviceKind,
    ) -> Option<LinuxAudioDevice> {
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
                        name: format!(
                            "Default {} Device",
                            if kind == DeviceKind::Input {
                                "Input"
                            } else {
                                "Output"
                            }
                        ),
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
    fn id(&self) -> crate::core::config::DeviceId {
        crate::core::config::DeviceId(self.id.clone())
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_default(&self) -> bool {
        // Default detection is handled at the enumerator level
        false
    }

    fn supported_formats(&self) -> Vec<crate::core::config::AudioFormat> {
        // TODO: Query actual supported formats from PipeWire
        vec![]
    }

    fn create_stream(
        &self,
        _config: &crate::core::config::StreamConfig,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::CapturingStream>> {
        Err(crate::core::error::AudioError::PlatformNotSupported {
            feature: "audio streams".to_string(),
            platform: "linux".to_string(),
        })
    }
}

// Implement DeviceEnumerator trait
impl crate::core::interface::DeviceEnumerator for LinuxDeviceEnumerator {
    fn enumerate_devices(
        &self,
    ) -> crate::core::error::Result<Vec<Box<dyn crate::core::interface::AudioDevice>>> {
        let mut devices: Vec<Box<dyn crate::core::interface::AudioDevice>> = Vec::new();

        // Try to get devices from PipeWire
        if let Ok(pw_devices) = self.get_pipewire_devices() {
            for d in pw_devices {
                devices.push(Box::new(d));
            }
        }

        // If no devices found, add a fallback default
        if devices.is_empty() {
            devices.push(Box::new(LinuxAudioDevice {
                id: "default".to_string(),
                name: "Default Linux Audio Device".to_string(),
                is_input: true,
                is_output: false,
            }));
        }

        Ok(devices)
    }

    fn default_device(
        &self,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::AudioDevice>> {
        // Try to get the actual default output device from PipeWire
        // (output is most relevant for audio capture / loopback)
        if let Ok(device) =
            self.get_pipewire_default_device(crate::core::interface::DeviceKind::Output)
        {
            return Ok(Box::new(device));
        }

        // Fallback to a generic default
        Ok(Box::new(LinuxAudioDevice {
            id: "default".to_string(),
            name: "Default Linux Audio Device".to_string(),
            is_input: false,
            is_output: true,
        }))
    }
}

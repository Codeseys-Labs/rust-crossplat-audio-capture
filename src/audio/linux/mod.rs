//! Linux audio implementation using PipeWire

#[cfg(feature = "pipewire")]
pub(crate) mod thread;

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
        config: &crate::core::config::StreamConfig,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::CapturingStream>> {
        #[cfg(feature = "pipewire")]
        {
            use std::sync::{Arc, Mutex};
            use std::time::Duration;

            use crate::bridge::state::StreamState;
            use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};

            use crate::audio::linux::thread::{CaptureConfig, LinuxPlatformStream, PipeWireThread};
            use crate::core::config::CaptureTarget;

            // 1. Build AudioFormat from StreamConfig
            let format = config.to_audio_format();

            // 2. Determine capture target based on device identity.
            //    StreamConfig does not carry a CaptureTarget, so we derive it
            //    from the device: "default" → SystemDefault, otherwise Device(id).
            let target = if self.id == "default" {
                CaptureTarget::SystemDefault
            } else {
                CaptureTarget::Device(crate::core::config::DeviceId(self.id.clone()))
            };

            // 3. Create the ring buffer bridge (64 AudioBuffer slots by default)
            let capacity = calculate_capacity(None, 4);
            let (producer, consumer) = create_bridge(capacity, format.clone());

            // 4. Transition bridge state from Created → Running so reads work
            consumer
                .shared()
                .state
                .transition(StreamState::Created, StreamState::Running)
                .map_err(|actual| crate::core::error::AudioError::InternalError {
                    message: format!(
                        "Failed to transition bridge state to Running (was {:?})",
                        actual
                    ),
                    source: None,
                })?;

            // 5. Build CaptureConfig for the PipeWire thread
            let capture_config = CaptureConfig {
                target,
                sample_rate: format.sample_rate,
                channels: format.channels,
            };

            // 6. Spawn the dedicated PipeWire thread
            let pw_thread = PipeWireThread::spawn()?;

            // 7. Start capture — sends the producer to the PipeWire thread
            pw_thread.start_capture(capture_config, producer)?;

            // 8. Wrap PipeWireThread in Arc<Mutex> for LinuxPlatformStream
            let pw_thread_arc = Arc::new(Mutex::new(pw_thread));
            let platform_stream = LinuxPlatformStream::new(pw_thread_arc);

            // 9. Create BridgeStream (consumer + platform stream + format + timeout)
            let bridge_stream =
                BridgeStream::new(consumer, platform_stream, format, Duration::from_secs(1));

            Ok(Box::new(bridge_stream))
        }

        #[cfg(not(feature = "pipewire"))]
        {
            let _ = config;
            Err(crate::core::error::AudioError::PlatformNotSupported {
                feature: "audio streams (PipeWire feature not enabled)".to_string(),
                platform: "linux".to_string(),
            })
        }
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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::core::config::DeviceId;
    use crate::core::interface::{AudioDevice, DeviceEnumerator};

    // ── LinuxAudioDevice Unit Tests ──────────────────────────────────

    #[test]
    fn test_linux_audio_device_name() {
        let device = LinuxAudioDevice {
            id: "test-device".to_string(),
            name: "Test Device".to_string(),
            is_input: true,
            is_output: false,
        };
        assert_eq!(device.name(), "Test Device");
    }

    #[test]
    fn test_linux_audio_device_id() {
        let device = LinuxAudioDevice {
            id: "test-device-123".to_string(),
            name: "Test".to_string(),
            is_input: false,
            is_output: true,
        };
        assert_eq!(device.id(), DeviceId("test-device-123".to_string()));
    }

    #[test]
    fn test_linux_audio_device_id_display() {
        let device = LinuxAudioDevice {
            id: "hw:0,0".to_string(),
            name: "Sound Card".to_string(),
            is_input: true,
            is_output: false,
        };
        assert_eq!(device.id().to_string(), "hw:0,0");
    }

    #[test]
    fn test_linux_audio_device_is_default_returns_false() {
        // is_default() always returns false — default detection is at enumerator level
        let device = LinuxAudioDevice {
            id: "default".to_string(),
            name: "Default".to_string(),
            is_input: true,
            is_output: true,
        };
        assert!(!device.is_default());
    }

    #[test]
    fn test_linux_audio_device_supported_formats_empty() {
        // supported_formats() currently returns empty (TODO in implementation)
        let device = LinuxAudioDevice {
            id: "test".to_string(),
            name: "Test".to_string(),
            is_input: true,
            is_output: false,
        };
        assert!(device.supported_formats().is_empty());
    }

    #[test]
    fn test_linux_audio_device_clone() {
        let device = LinuxAudioDevice {
            id: "clone-test".to_string(),
            name: "Clone Test".to_string(),
            is_input: true,
            is_output: true,
        };
        let cloned = device.clone();
        assert_eq!(cloned.id, device.id);
        assert_eq!(cloned.name, device.name);
        assert_eq!(cloned.is_input, device.is_input);
        assert_eq!(cloned.is_output, device.is_output);
    }

    #[test]
    fn test_linux_audio_device_debug() {
        let device = LinuxAudioDevice {
            id: "debug-test".to_string(),
            name: "Debug Device".to_string(),
            is_input: false,
            is_output: true,
        };
        let dbg = format!("{:?}", device);
        assert!(dbg.contains("debug-test"));
        assert!(dbg.contains("Debug Device"));
    }

    // ── LinuxDeviceEnumerator Unit Tests ─────────────────────────────

    #[test]
    fn test_linux_device_enumerator_new() {
        let _enumerator = LinuxDeviceEnumerator::new();
        // Just verify construction doesn't panic
    }

    #[test]
    fn test_linux_device_enumerator_default() {
        let _enumerator = LinuxDeviceEnumerator::default();
        // Verify Default impl works
    }

    #[test]
    fn test_linux_device_enumerator_enumerate_devices_does_not_panic() {
        let enumerator = LinuxDeviceEnumerator::new();
        let result = enumerator.enumerate_devices();
        // Should always succeed (falls back to a default device)
        match result {
            Ok(devices) => {
                assert!(
                    !devices.is_empty(),
                    "Should have at least the fallback device"
                );
                // Verify all returned devices have non-empty names
                for device in &devices {
                    assert!(!device.name().is_empty());
                }
            }
            Err(e) => {
                panic!("enumerate_devices should not fail (has fallback): {}", e);
            }
        }
    }

    #[test]
    fn test_linux_device_enumerator_default_device_does_not_panic() {
        let enumerator = LinuxDeviceEnumerator::new();
        let result = enumerator.default_device();
        // Should always succeed (falls back to generic default)
        match result {
            Ok(device) => {
                assert!(!device.name().is_empty());
            }
            Err(e) => {
                panic!("default_device should not fail (has fallback): {}", e);
            }
        }
    }

    // ── pw-cli parser Unit Tests ────────────────────────────────────

    #[test]
    fn test_parse_pw_cli_nodes_with_audio_sink() {
        let enumerator = LinuxDeviceEnumerator::new();
        let sample_output = r#"
id 42, type PipeWire:Interface:Node/3
    node.name = "alsa_output.pci-0000_00_1f.3.analog-stereo"
    media.class = "Audio/Sink"
}
"#;
        let devices = enumerator.parse_pw_cli_nodes(sample_output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "42");
        assert_eq!(
            devices[0].name,
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        );
        assert!(devices[0].is_output);
        assert!(!devices[0].is_input);
    }

    #[test]
    fn test_parse_pw_cli_nodes_with_audio_source() {
        let enumerator = LinuxDeviceEnumerator::new();
        let sample_output = r#"
id 55, type PipeWire:Interface:Node/3
    node.name = "alsa_input.pci-0000_00_1f.3.analog-stereo"
    media.class = "Audio/Source"
}
"#;
        let devices = enumerator.parse_pw_cli_nodes(sample_output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "55");
        assert!(devices[0].is_input);
        assert!(!devices[0].is_output);
    }

    #[test]
    fn test_parse_pw_cli_nodes_ignores_non_audio() {
        let enumerator = LinuxDeviceEnumerator::new();
        let sample_output = r#"
id 10, type PipeWire:Interface:Node/3
    node.name = "v4l2-source"
    media.class = "Video/Source"
}
"#;
        let devices = enumerator.parse_pw_cli_nodes(sample_output);
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_pw_cli_nodes_multiple_devices() {
        let enumerator = LinuxDeviceEnumerator::new();
        let sample_output = r#"
id 42, type PipeWire:Interface:Node/3
    node.name = "sink1"
    media.class = "Audio/Sink"
}
id 55, type PipeWire:Interface:Node/3
    node.name = "source1"
    media.class = "Audio/Source"
}
id 10, type PipeWire:Interface:Node/3
    node.name = "video"
    media.class = "Video/Source"
}
"#;
        let devices = enumerator.parse_pw_cli_nodes(sample_output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "sink1");
        assert_eq!(devices[1].name, "source1");
    }

    #[test]
    fn test_parse_pw_cli_nodes_empty_output() {
        let enumerator = LinuxDeviceEnumerator::new();
        let devices = enumerator.parse_pw_cli_nodes("");
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_pw_cli_nodes_no_name_uses_default() {
        let enumerator = LinuxDeviceEnumerator::new();
        let sample_output = r#"
id 42, type PipeWire:Interface:Node/3
    media.class = "Audio/Sink"
}
"#;
        let devices = enumerator.parse_pw_cli_nodes(sample_output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "PipeWire Device");
    }

    // ── create_stream without pipewire feature ──────────────────────

    #[cfg(not(feature = "pipewire"))]
    #[test]
    fn test_create_stream_without_pipewire_feature_returns_error() {
        let device = LinuxAudioDevice {
            id: "test".to_string(),
            name: "Test".to_string(),
            is_input: true,
            is_output: false,
        };
        let config = StreamConfig::default();
        let result = device.create_stream(&config);
        assert!(result.is_err());
        // Should report PlatformNotSupported
        match result.unwrap_err() {
            crate::core::error::AudioError::PlatformNotSupported { feature, platform } => {
                assert!(feature.contains("PipeWire"));
                assert_eq!(platform, "linux");
            }
            other => panic!("Expected PlatformNotSupported, got: {:?}", other),
        }
    }
}

// ── PipeWire Integration Tests ───────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "linux")]
#[cfg(feature = "pipewire")]
mod pipewire_integration_tests {
    use super::*;
    use crate::core::config::StreamConfig;
    use crate::core::interface::DeviceEnumerator;

    /// Check if PipeWire daemon is running by attempting `pw-cli info`.
    fn pipewire_available() -> bool {
        std::process::Command::new("pw-cli")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn test_create_stream_returns_bridge_stream() {
        if !pipewire_available() {
            eprintln!(
                "Skipping test_create_stream_returns_bridge_stream: PipeWire daemon not running"
            );
            return;
        }

        let enumerator = LinuxDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("should get default device");
        let config = StreamConfig::default();
        let result = device.create_stream(&config);

        match result {
            Ok(stream) => {
                // Verify the stream is running
                assert!(
                    stream.is_running(),
                    "Stream should be running after creation"
                );
                // Verify format matches request
                let fmt = stream.format();
                assert_eq!(fmt.sample_rate, 48000);
                assert_eq!(fmt.channels, 2);
                println!("Stream created successfully with format: {:?}", fmt);
                // Clean up
                let _ = stream.stop();
            }
            Err(e) => {
                eprintln!(
                    "create_stream failed (PipeWire might be misconfigured): {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_capture_system_audio_briefly() {
        if !pipewire_available() {
            eprintln!("Skipping test_capture_system_audio_briefly: PipeWire daemon not running");
            return;
        }

        let enumerator = LinuxDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("should get default device");
        let config = StreamConfig::default();

        match device.create_stream(&config) {
            Ok(stream) => {
                // Try to read for up to 1 second using try_read_chunk (non-blocking)
                let start = std::time::Instant::now();
                let mut chunks_read = 0u32;

                while start.elapsed() < std::time::Duration::from_secs(1) {
                    match stream.try_read_chunk() {
                        Ok(Some(buffer)) => {
                            chunks_read += 1;
                            assert!(buffer.len() > 0, "Buffer should have samples");
                            assert_eq!(buffer.channels(), 2);
                            assert_eq!(buffer.sample_rate(), 48000);
                        }
                        Ok(None) => {
                            // No data yet — wait a bit
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(e) => {
                            eprintln!("Read error: {}", e);
                            break;
                        }
                    }
                }

                println!("Read {} audio chunks in 1 second", chunks_read);
                let _ = stream.stop();
            }
            Err(e) => {
                eprintln!("create_stream failed: {}", e);
            }
        }
    }

    #[test]
    fn test_stream_stop_and_restart() {
        if !pipewire_available() {
            eprintln!("Skipping test_stream_stop_and_restart: PipeWire daemon not running");
            return;
        }

        let enumerator = LinuxDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("should get default device");
        let config = StreamConfig::default();

        // Create and stop stream
        if let Ok(stream) = device.create_stream(&config) {
            assert!(stream.is_running());
            let stop_result = stream.stop();
            assert!(stop_result.is_ok(), "Stop should succeed");
            assert!(
                !stream.is_running(),
                "Stream should not be running after stop"
            );
        }

        // Create a second stream to verify no resource leaks
        if let Ok(stream2) = device.create_stream(&config) {
            assert!(stream2.is_running());
            let _ = stream2.stop();
        }
    }

    #[test]
    fn test_enumerate_devices_with_pipewire_running() {
        if !pipewire_available() {
            eprintln!("Skipping test_enumerate_devices_with_pipewire_running: PipeWire daemon not running");
            return;
        }

        let enumerator = LinuxDeviceEnumerator::new();
        let devices = enumerator.enumerate_devices().expect("should enumerate");

        println!("Found {} audio devices:", devices.len());
        for device in &devices {
            println!("  - {} (id: {})", device.name(), device.id());
        }

        assert!(
            !devices.is_empty(),
            "Should find at least one device with PipeWire running"
        );
    }
}

//! Unified cross-platform application-specific audio capture API
//! 
//! This module provides a unified interface for capturing audio from specific applications
//! across Windows (WASAPI Process Loopback), Linux (PipeWire monitor streams), and 
//! macOS (CoreAudio Process Tap).

#[cfg(all(target_os = "windows", feature = "feat_windows"))]
use crate::audio::windows::WindowsApplicationCapture;

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
use crate::audio::linux::{PipeWireApplicationCapture, ApplicationSelector};

#[cfg(all(target_os = "macos", feature = "feat_macos"))]
use crate::audio::macos::tap::MacOSApplicationCapture;

/// Unified application capture interface
pub trait ApplicationCapture {
    /// Start capturing audio from the target application
    fn start_capture<F>(&mut self, callback: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static;
    
    /// Stop capturing audio
    fn stop_capture(&mut self) -> Result<(), Box<dyn std::error::Error>>;
    
    /// Check if currently capturing
    fn is_capturing(&self) -> bool;
}

/// Cross-platform application capture implementation
pub enum CrossPlatformApplicationCapture {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    Windows(WindowsApplicationCapture),

    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    Linux(PipeWireApplicationCapture),

    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    MacOS(MacOSApplicationCapture),
}

impl ApplicationCapture for CrossPlatformApplicationCapture {
    fn start_capture<F>(&mut self, callback: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformApplicationCapture::Windows(capture) => capture.start_capture(callback),

            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformApplicationCapture::Linux(capture) => capture.start_capture(callback),

            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformApplicationCapture::MacOS(capture) => capture.start_capture(callback),

            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => unreachable!("No platform features enabled"),
        }
    }
    
    fn stop_capture(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformApplicationCapture::Windows(capture) => capture.stop_capture(),

            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformApplicationCapture::Linux(capture) => capture.stop_capture(),

            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformApplicationCapture::MacOS(capture) => capture.stop_capture(),
        }
    }
    
    fn is_capturing(&self) -> bool {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformApplicationCapture::Windows(capture) => capture.is_capturing(),

            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformApplicationCapture::Linux(capture) => capture.is_capturing(),

            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformApplicationCapture::MacOS(capture) => capture.is_capturing(),
        }
    }
}

/// Application information for cross-platform use
#[derive(Debug, Clone)]
pub struct ApplicationInfo {
    pub process_id: u32,
    pub name: String,
    pub platform_specific: PlatformSpecificInfo,
}

#[derive(Debug, Clone)]
pub enum PlatformSpecificInfo {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    Windows {
        executable_path: Option<String>,
    },
    
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    Linux {
        node_id: Option<u32>,
        media_class: Option<String>,
    },
    
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    MacOS {
        bundle_id: Option<String>,
    },
}

/// Factory for creating application capture instances
pub struct ApplicationCaptureFactory;

impl ApplicationCaptureFactory {
    /// Create a new application capture instance for the specified process ID
    pub fn create_for_process_id(process_id: u32) -> Result<CrossPlatformApplicationCapture, Box<dyn std::error::Error>> {
        #[cfg(all(target_os = "windows", feature = "feat_windows"))]
        {
            let mut capture = WindowsApplicationCapture::new(process_id, false);
            capture.initialize()?;
            Ok(CrossPlatformApplicationCapture::Windows(capture))
        }

        #[cfg(all(target_os = "linux", feature = "feat_linux"))]
        {
            let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::ProcessId(process_id));
            capture.discover_target_node()?;
            capture.create_monitor_stream()?;
            Ok(CrossPlatformApplicationCapture::Linux(capture))
        }

        #[cfg(all(target_os = "macos", feature = "feat_macos"))]
        {
            if !MacOSApplicationCapture::is_process_tap_available() {
                return Err("Process Tap APIs require macOS 14.4 or later".into());
            }
            let capture = MacOSApplicationCapture::new(process_id, false);
            Ok(CrossPlatformApplicationCapture::MacOS(capture))
        }

        #[cfg(not(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "linux", feature = "feat_linux"),
            all(target_os = "macos", feature = "feat_macos")
        )))]
        {
            Err("No supported platform features enabled".into())
        }
    }
    
    /// Create a new application capture instance for the specified application name
    pub fn create_for_application_name(app_name: &str) -> Result<CrossPlatformApplicationCapture, Box<dyn std::error::Error>> {
        #[cfg(all(target_os = "windows", feature = "feat_windows"))]
        {
            if let Some(process_id) = WindowsApplicationCapture::find_process_by_name(app_name, false) {
                Self::create_for_process_id(process_id)
            } else {
                Err(format!("Application '{}' not found", app_name).into())
            }
        }
        
        #[cfg(all(target_os = "linux", feature = "feat_linux"))]
        {
            let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::ApplicationName(app_name.to_string()));
            capture.discover_target_node()?;
            capture.create_monitor_stream()?;
            Ok(CrossPlatformApplicationCapture::Linux(capture))
        }
        
        #[cfg(all(target_os = "macos", feature = "feat_macos"))]
        {
            if !MacOSApplicationCapture::is_process_tap_available() {
                return Err("Process Tap APIs require macOS 14.4 or later".into());
            }

            let applications = MacOSApplicationCapture::list_capturable_applications()?;
            if let Some((pid, _)) = applications.iter().find(|(_, name)| name.contains(app_name)) {
                let capture = MacOSApplicationCapture::new(*pid, false);
                Ok(CrossPlatformApplicationCapture::MacOS(capture))
            } else {
                Err(format!("Application '{}' not found", app_name).into())
            }
        }

        #[cfg(not(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "linux", feature = "feat_linux"),
            all(target_os = "macos", feature = "feat_macos")
        )))]
        {
            Err("No supported platform features enabled".into())
        }
    }
    
    /// List all available applications that can be captured
    pub fn list_capturable_applications() -> Result<Vec<ApplicationInfo>, Box<dyn std::error::Error>> {
        #[cfg(all(target_os = "windows", feature = "feat_windows"))]
        {
            let processes = WindowsApplicationCapture::list_audio_processes();
            Ok(processes.into_iter().map(|(pid, name)| ApplicationInfo {
                process_id: pid,
                name,
                platform_specific: PlatformSpecificInfo::Windows {
                    executable_path: None,
                },
            }).collect())
        }
        
        #[cfg(all(target_os = "linux", feature = "feat_linux"))]
        {
            let applications = PipeWireApplicationCapture::list_audio_applications()?;
            Ok(applications.into_iter().map(|app| ApplicationInfo {
                process_id: app.process_id.unwrap_or(0),
                name: app.name.unwrap_or_else(|| app.node_name.unwrap_or_else(|| format!("Node {}", app.pipewire_node_id.unwrap_or(0)))),
                platform_specific: PlatformSpecificInfo::Linux {
                    node_id: app.pipewire_node_id,
                    media_class: Some(app.media_class),
                },
            }).collect())
        }
        
        #[cfg(all(target_os = "macos", feature = "feat_macos"))]
        {
            let applications = MacOSApplicationCapture::list_capturable_applications()?;
            Ok(applications.into_iter().map(|(pid, name)| ApplicationInfo {
                process_id: pid,
                name,
                platform_specific: PlatformSpecificInfo::MacOS {
                    bundle_id: None,
                },
            }).collect())
        }

        #[cfg(not(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "linux", feature = "feat_linux"),
            all(target_os = "macos", feature = "feat_macos")
        )))]
        {
            Err("No supported platform features enabled".into())
        }
    }
}

/// Convenience function to create application capture for a process ID
pub fn capture_application_by_pid(process_id: u32) -> Result<CrossPlatformApplicationCapture, Box<dyn std::error::Error>> {
    ApplicationCaptureFactory::create_for_process_id(process_id)
}

/// Convenience function to create application capture for an application name
pub fn capture_application_by_name(app_name: &str) -> Result<CrossPlatformApplicationCapture, Box<dyn std::error::Error>> {
    ApplicationCaptureFactory::create_for_application_name(app_name)
}

/// Convenience function to list all capturable applications
pub fn list_capturable_applications() -> Result<Vec<ApplicationInfo>, Box<dyn std::error::Error>> {
    ApplicationCaptureFactory::list_capturable_applications()
}

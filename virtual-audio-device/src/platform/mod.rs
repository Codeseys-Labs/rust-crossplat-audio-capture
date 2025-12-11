//! Platform-specific virtual audio device implementations

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

// Re-export platform-specific functions
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "macos")]
pub use macos::*;

// Fallback for unsupported platforms
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn create_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    Err("Unsupported platform".into())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn remove_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    Err("Unsupported platform".into())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn check_device_status() -> Result<(), Box<dyn std::error::Error>> {
    Err("Unsupported platform".into())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn test_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    Err("Unsupported platform".into())
}

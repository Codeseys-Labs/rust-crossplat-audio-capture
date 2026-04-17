//! Diagnostic binary: Enumerate ALL WASAPI audio sessions across ALL render devices.
//!
//! This tool is used to investigate why `enumerate_application_audio_sessions()` may only
//! show Chrome when other apps are also playing audio.
//!
//! Usage:
//!   cargo run --bin wasapi_session_test --features feat_windows
//!
//! What it tests:
//!   1. Sessions on the DEFAULT render device (what the library currently does)
//!   2. Sessions on ALL render devices (to check if apps use non-default devices)
//!   3. Shows ALL session states (Active, Inactive, Expired) — not just Active
//!   4. PID → process name resolution for each session
//!   5. Display name resolution via GetDisplayName, PID lookup, and session ID parsing

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("wasapi_session_test is only supported on Windows");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
mod imp {
    use windows::core::*;
    use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName as PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::*;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    const VT_LPWSTR: u16 = 31;

    fn session_state_name(state: AudioSessionState) -> &'static str {
        #[allow(non_upper_case_globals)]
        match state {
            AudioSessionStateInactive => "Inactive",
            AudioSessionStateActive => "Active",
            AudioSessionStateExpired => "Expired",
            _ => "Unknown",
        }
    }

    fn get_process_name(pid: u32) -> String {
        if pid == 0 {
            return "(System / PID 0)".to_string();
        }
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(handle) if handle != INVALID_HANDLE_VALUE => {
                    let mut buf = [0u16; 260];
                    let len = K32GetModuleFileNameExW(
                        Some(handle),
                        Some(HMODULE(std::ptr::null_mut())),
                        &mut buf,
                    );
                    let _ = CloseHandle(handle);
                    if len > 0 {
                        let path = String::from_utf16_lossy(&buf[..len as usize]);
                        path.rsplit('\\')
                            .next()
                            .unwrap_or(&path)
                            .strip_suffix(".exe")
                            .unwrap_or(&path)
                            .to_string()
                    } else {
                        format!("(PID {} — name unavailable)", pid)
                    }
                }
                _ => format!("(PID {} — access denied)", pid),
            }
        }
    }

    unsafe fn get_device_name(device: &IMMDevice) -> String {
        let store: IPropertyStore = match device.OpenPropertyStore(STGM_READ) {
            Ok(s) => s,
            Err(_) => return "(unknown device)".to_string(),
        };
        let prop = match store.GetValue(&PKEY_Device_FriendlyName as *const _ as *const _) {
            Ok(p) => p,
            Err(_) => return "(unknown device)".to_string(),
        };
        if prop.vt() == windows::Win32::System::Variant::VARENUM(VT_LPWSTR) {
            let pwstr = prop.Anonymous.Anonymous.Anonymous.pwszVal;
            if !pwstr.is_null() {
                return pwstr
                    .to_string()
                    .unwrap_or_else(|_| "(invalid UTF-16)".to_string());
            }
        }
        "(unknown device)".to_string()
    }

    unsafe fn get_device_id(device: &IMMDevice) -> String {
        match device.GetId() {
            Ok(id_pwstr) => {
                let result = if !id_pwstr.is_null() {
                    id_pwstr
                        .to_string()
                        .unwrap_or_else(|_| "(invalid)".to_string())
                } else {
                    "(null)".to_string()
                };
                CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
                result
            }
            Err(_) => "(error)".to_string(),
        }
    }

    fn enumerate_sessions_on_device(device: &IMMDevice, device_label: &str) -> Result<()> {
        unsafe {
            let device_name = get_device_name(device);
            let device_id = get_device_id(device);
            println!("\n{}", "=".repeat(60));
            println!("  Device: {} ({})", device_name, device_label);
            println!("  ID: {}", device_id);
            println!("{}", "=".repeat(60));

            let session_manager: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
                Ok(sm) => sm,
                Err(hr) => {
                    println!("  ⚠ Could not activate IAudioSessionManager2: {:?}", hr);
                    return Ok(());
                }
            };

            let session_enum: IAudioSessionEnumerator = match session_manager.GetSessionEnumerator()
            {
                Ok(se) => se,
                Err(hr) => {
                    println!("  ⚠ Could not get session enumerator: {:?}", hr);
                    return Ok(());
                }
            };

            let count = session_enum.GetCount().unwrap_or(0);
            println!("  Total sessions: {}", count);

            if count == 0 {
                println!("  (no sessions on this device)");
                return Ok(());
            }

            let mut active_count = 0u32;
            let mut inactive_count = 0u32;
            let mut expired_count = 0u32;

            for i in 0..count {
                let session_control: IAudioSessionControl = match session_enum.GetSession(i) {
                    Ok(sc) => sc,
                    Err(hr) => {
                        println!("  [{}/{}] ⚠ GetSession failed: {:?}", i + 1, count, hr);
                        continue;
                    }
                };

                let state = session_control
                    .GetState()
                    .unwrap_or(AudioSessionStateExpired);

                #[allow(non_upper_case_globals)]
                match state {
                    AudioSessionStateActive => active_count += 1,
                    AudioSessionStateInactive => inactive_count += 1,
                    AudioSessionStateExpired => expired_count += 1,
                    _ => {}
                }

                let session_control2: IAudioSessionControl2 = match session_control.cast() {
                    Ok(sc2) => sc2,
                    Err(_) => {
                        println!(
                            "  [{}/{}] State: {} — (cannot get IAudioSessionControl2)",
                            i + 1,
                            count,
                            session_state_name(state)
                        );
                        continue;
                    }
                };

                let pid = session_control2.GetProcessId().unwrap_or(0);
                let process_name = get_process_name(pid);

                // Get display name
                let display_name = {
                    let dn_pwstr = session_control2.GetDisplayName().unwrap_or(PWSTR::null());
                    if !dn_pwstr.is_null() {
                        let dn = dn_pwstr.to_string().unwrap_or_else(|_| String::new());
                        CoTaskMemFree(Some(dn_pwstr.as_ptr().cast()));
                        dn
                    } else {
                        String::new()
                    }
                };

                // Get session identifier
                let session_id = {
                    let sid_pwstr = session_control2
                        .GetSessionIdentifier()
                        .unwrap_or(PWSTR::null());
                    if !sid_pwstr.is_null() {
                        let sid = sid_pwstr.to_string().unwrap_or_else(|_| String::new());
                        CoTaskMemFree(Some(sid_pwstr.as_ptr().cast()));
                        sid
                    } else {
                        String::new()
                    }
                };

                // Check if system sounds session
                let is_system = session_control2.IsSystemSoundsSession().is_ok();

                println!();
                println!(
                    "  [{}/{}] State: {} | PID: {} | Process: {}",
                    i + 1,
                    count,
                    session_state_name(state),
                    pid,
                    process_name
                );
                println!("         DisplayName: {:?}", display_name);
                println!(
                    "         SessionId:   {}",
                    &session_id[..session_id.len().min(80)]
                );
                if is_system {
                    println!("         ** System sounds session **");
                }
            }

            println!();
            println!(
                "  Summary: {} active, {} inactive, {} expired (total: {})",
                active_count, inactive_count, expired_count, count
            );
        }
        Ok(())
    }

    pub fn run() {
        println!("╔══════════════════════════════════════════════════════════╗");
        println!("║  WASAPI Audio Session Diagnostic                       ║");
        println!("║  Enumerating ALL sessions on ALL render devices        ║");
        println!("╚══════════════════════════════════════════════════════════╝");

        unsafe {
            // Initialize COM
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() {
                eprintln!("COM initialization failed: {:?}", hr);
                return;
            }

            let enumerator: IMMDeviceEnumerator =
                match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                    Ok(e) => e,
                    Err(hr) => {
                        eprintln!("Failed to create device enumerator: {:?}", hr);
                        CoUninitialize();
                        return;
                    }
                };

            // ── Part 1: Default render device (what the library does) ──
            println!("\n\n=== PART 1: DEFAULT RENDER DEVICE (eConsole) ===");
            match enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                Ok(device) => {
                    let _ = enumerate_sessions_on_device(&device, "default render / eConsole");
                }
                Err(hr) => {
                    println!("No default render device (eConsole): {:?}", hr);
                }
            }

            // Also check eMultimedia and eCommunications defaults
            println!("\n\n=== PART 1b: DEFAULT RENDER DEVICE (eMultimedia) ===");
            match enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) {
                Ok(device) => {
                    let dev_id = get_device_id(&device);
                    println!("  (Device ID: {})", dev_id);
                    // Only enumerate if it's a different device than eConsole
                    let _ = enumerate_sessions_on_device(&device, "default render / eMultimedia");
                }
                Err(hr) => {
                    println!("No default render device (eMultimedia): {:?}", hr);
                }
            }

            println!("\n\n=== PART 1c: DEFAULT RENDER DEVICE (eCommunications) ===");
            match enumerator.GetDefaultAudioEndpoint(eRender, eCommunications) {
                Ok(device) => {
                    let _ =
                        enumerate_sessions_on_device(&device, "default render / eCommunications");
                }
                Err(hr) => {
                    println!("No default render device (eCommunications): {:?}", hr);
                }
            }

            // ── Part 2: ALL render devices ──
            println!("\n\n=== PART 2: ALL ACTIVE RENDER DEVICES ===");
            match enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                Ok(collection) => {
                    let count = collection.GetCount().unwrap_or(0);
                    println!("Total active render devices: {}", count);

                    for i in 0..count {
                        match collection.Item(i) {
                            Ok(device) => {
                                let label = format!("render device #{}", i);
                                let _ = enumerate_sessions_on_device(&device, &label);
                            }
                            Err(hr) => {
                                println!("  Failed to get device {}: {:?}", i, hr);
                            }
                        }
                    }
                }
                Err(hr) => {
                    println!("Failed to enumerate render devices: {:?}", hr);
                }
            }

            // ── Part 3: Also check capture devices (some apps may use them) ──
            println!("\n\n=== PART 3: ALL ACTIVE CAPTURE (INPUT) DEVICES ===");
            match enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
                Ok(collection) => {
                    let count = collection.GetCount().unwrap_or(0);
                    println!("Total active capture devices: {}", count);

                    for i in 0..count {
                        match collection.Item(i) {
                            Ok(device) => {
                                let label = format!("capture device #{}", i);
                                let _ = enumerate_sessions_on_device(&device, &label);
                            }
                            Err(hr) => {
                                println!("  Failed to get device {}: {:?}", i, hr);
                            }
                        }
                    }
                }
                Err(hr) => {
                    println!("Failed to enumerate capture devices: {:?}", hr);
                }
            }

            // ── Part 4: Library function comparison ──
            println!("\n\n=== PART 4: LIBRARY FUNCTION OUTPUT ===");
            println!("Calling rsac::audio::windows::enumerate_application_audio_sessions()...\n");

            // Need to drop the COM state first since the library initializes its own
            CoUninitialize();

            match rsac::audio::windows::enumerate_application_audio_sessions() {
                Ok(sessions) => {
                    if sessions.is_empty() {
                        println!("  (empty — no active sessions returned by library)");
                    } else {
                        println!("  Library returned {} session(s):", sessions.len());
                        for s in &sessions {
                            println!(
                                "    PID: {:>6} | Name: {:20} | Path: {}",
                                s.process_id,
                                s.display_name,
                                s.executable_path.as_deref().unwrap_or("N/A")
                            );
                        }
                    }
                }
                Err(e) => {
                    println!("  Library function error: {}", e);
                }
            }

            println!("\n=== DIAGNOSTIC COMPLETE ===");
        }
    }
} // mod imp

#[cfg(target_os = "windows")]
fn main() {
    imp::run();
}

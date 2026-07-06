//! macOS system-audio-capture (Process Tap) TCC permission preflight.
//!
//! **This module is compiled only under the opt-in `macos-tcc-spi` feature**
//! (see [ADR-0015](../../../docs/designs/0015-macos-tcc-audiocapture-preflight.md)).
//! It exists because there is **no public API** to query the
//! `kTCCServiceAudioCapture` authorization status before attempting a Process
//! Tap — `AVCaptureDevice.authorizationStatus(for: .audio)` covers the
//! *microphone* service, not the tap. The only working preflight is the
//! **private, undocumented** `TCCAccessPreflight` SPI in
//! `/System/Library/PrivateFrameworks/TCC.framework`, the same mechanism the
//! reference implementation `insidegui/AudioCap` uses (behind its own
//! `ENABLE_TCC_SPI` build flag).
//!
//! Because the symbol is private it is `dlopen`/`dlsym`'d at runtime, never
//! linked. Any failure (framework missing, symbol absent) degrades to
//! [`PermissionStatus::NotDetermined`] — this module never panics and never
//! claims an authorization answer it cannot actually obtain.

use std::ffi::c_void;
use std::os::raw::c_long;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;

use crate::core::introspection::PermissionStatus;

/// The TCC service name for system-audio capture (Process Tap), as an ASCII
/// C-string literal. This is a *different* service from `kTCCServiceMicrophone`
/// (the microphone) and `kTCCServiceScreenCapture` (screen recording).
const TCC_SERVICE_AUDIO_CAPTURE: &str = "kTCCServiceAudioCapture";

/// Path to the private framework that exports `TCCAccessPreflight`.
const TCC_FRAMEWORK_PATH: &[u8] = b"/System/Library/PrivateFrameworks/TCC.framework/TCC\0";

/// `TCCAccessPreflight`'s exported symbol name.
const TCC_PREFLIGHT_SYMBOL: &[u8] = b"TCCAccessPreflight\0";

/// Signature of the private `TCCAccessPreflight` SPI.
///
/// ```objc
/// long TCCAccessPreflight(CFStringRef service, CFDictionaryRef options);
/// ```
///
/// Return values (empirically, per AudioCap): `0` = authorized, `1` = denied,
/// `2` (and anything else) = not-yet-determined / unknown.
type TccAccessPreflightFn =
    unsafe extern "C" fn(service: CFStringRef, options: CFDictionaryRef) -> c_long;

/// Queries the macOS system-audio-capture (Process Tap) TCC authorization
/// status via the private `TCCAccessPreflight` SPI, without triggering a
/// consent prompt.
///
/// Returns [`PermissionStatus::NotDetermined`] on any failure to reach the SPI,
/// so the caller always gets an honest, non-panicking answer.
pub(crate) fn audio_capture_permission() -> PermissionStatus {
    // SAFETY: dlopen/dlsym of a system framework. We validate every pointer
    // before use and only call the resolved symbol with a valid CFString and a
    // null options dictionary (which the SPI accepts).
    unsafe {
        let handle = libc::dlopen(TCC_FRAMEWORK_PATH.as_ptr() as *const _, libc::RTLD_LAZY);
        if handle.is_null() {
            log::debug!("macos-tcc-spi: dlopen(TCC.framework) failed; reporting NotDetermined");
            return PermissionStatus::NotDetermined;
        }

        // Resolve the symbol. Note we intentionally do NOT dlclose(handle):
        // the framework stays mapped for the process lifetime (cheap, and a
        // second call re-dlopens the already-resident image), which avoids any
        // window where a resolved fn pointer could dangle.
        let sym = libc::dlsym(handle, TCC_PREFLIGHT_SYMBOL.as_ptr() as *const _);
        if sym.is_null() {
            log::debug!(
                "macos-tcc-spi: TCCAccessPreflight symbol not found; reporting NotDetermined"
            );
            return PermissionStatus::NotDetermined;
        }

        // Transmute the resolved address to the function signature. This is the
        // load-bearing unsafe step; the signature is fixed by the ABI above.
        let preflight: TccAccessPreflightFn =
            std::mem::transmute::<*mut c_void, TccAccessPreflightFn>(sym);

        let service = CFString::new(TCC_SERVICE_AUDIO_CAPTURE);
        let result = preflight(service.as_concrete_TypeRef(), std::ptr::null());

        match result {
            0 => PermissionStatus::Granted,
            1 => PermissionStatus::Denied,
            // 2 = not-yet-determined; any other value is undocumented, so we
            // treat it conservatively as "we don't know" rather than guessing.
            _ => PermissionStatus::NotDetermined,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preflight must always return a well-formed `PermissionStatus` and
    /// never panic, regardless of the host's actual TCC state or whether the
    /// SPI resolves. (On CI the process is unbundled/terminal-attributed, so
    /// this typically observes `NotDetermined`/`Denied`; on a granted dev box
    /// it may observe `Granted`. All three are valid — we only assert liveness
    /// and non-panic here.)
    #[test]
    fn preflight_returns_a_status_without_panicking() {
        let status = audio_capture_permission();
        assert!(matches!(
            status,
            PermissionStatus::Granted | PermissionStatus::Denied | PermissionStatus::NotDetermined
        ));
    }
}

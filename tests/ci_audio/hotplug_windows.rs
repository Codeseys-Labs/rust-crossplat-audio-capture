//! Windows evidence tests for `hotplug-recovery` (ADR-0019, issue #94).
//!
//! One `SystemDefault` capture session stays open while the default render
//! endpoint changes or disappears, and the tests check **which endpoint's
//! audio** the capture delivers, not merely that a recovery event arrived: an
//! in-process WASAPI player renders 440 Hz on endpoint A and another 997 Hz on
//! endpoint B, and [`crate::hotplug_common`] classifies every 200 ms window of
//! captured audio by tone.
//!
//! Every test changes machine-wide audio state, so each holds
//! [`machine_lock`] for its whole body; run them with `--test-threads=1` as
//! well, for example:
//!
//! ```text
//! RSAC_HOTPLUG_SWITCH=1 RSAC_HOTPLUG_ENDPOINT_A=... RSAC_HOTPLUG_ENDPOINT_B=... \
//!   cargo test --test ci_audio --features hotplug-recovery -- hotplug_ \
//!   --test-threads=1 --nocapture
//! ```
//!
//! - `RSAC_HOTPLUG_SWITCH=1` enables the switch tests, which then require
//!   `RSAC_HOTPLUG_ENDPOINT_A` and `RSAC_HOTPLUG_ENDPOINT_B`: two different
//!   active render endpoint ids as `rsac list` prints them.
//! - `RSAC_HOTPLUG_LOSS=1` enables the loss tests, which hide the current
//!   default render endpoint via `IPolicyConfig::SetEndpointVisibility`.
//!   They require `GITHUB_ACTIONS=true` (a hosted runner) or
//!   `RSAC_HOTPLUG_ALLOW_DISABLE=1`. Plain `CI=true` is not enough: developer
//!   tools and agent shells set it on workstations.
//!
//! A test whose enabling variable is not `1` prints
//! `HOTPLUG-SKIP test=<name> reason=<variable> not set`. A test that passes
//! every check prints exactly one `HOTPLUG-EVIDENCE test=<name> result=pass …`
//! line on stdout, after the machine has been restored; no failure path prints
//! it. The guards restore the saved default roles (all three for the
//! communications and loss tests) and make a hidden endpoint visible again
//! on every exit path, including a failed assertion unwinding. A restore
//! that fails prints `HOTPLUG-RESTORE-FAILED …`
//! on stderr.
//!
//! Default endpoints are set through the undocumented `IPolicyConfig` COM
//! interface, the one the Sound settings use; Windows ships no public API for
//! it.

#![cfg(all(
    target_os = "windows",
    feature = "feat_windows",
    feature = "hotplug-recovery"
))]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rsac::{AudioError, AutoRecoveryConfig, RecoveryEvent};
use windows::core::{IUnknown, IUnknown_Vtbl, Interface, BOOL, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::Media::Audio::{eCommunications, eConsole, eMultimedia, ERole};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::helpers::init_test_logging;
use crate::hotplug_common::{
    enabled, evidence_line, fmt_at, is_default_changed, machine_lock, recovered_generation,
    variant_name, Machine, Session, Step, Timed, Verdict, ENDPOINT_ACTIVE_TIMEOUT,
    EXHAUST_DEVICE_WAIT, EXHAUST_MARGIN, PLAYER_FIRST_BUFFER_TIMEOUT, PLAYER_REOPEN_INTERVAL,
    READBACK_INTERVAL, READBACK_TIMEOUT, RECOVER_TIMEOUT, RETURN_DEVICE_WAIT, RETURN_OUTAGE,
    RETURN_RECOVER_TIMEOUT, SEE_TIMEOUT, TONE_AMPLITUDE, TONE_A_HZ, TONE_B_HZ,
};

// ── Environment ──────────────────────────────────────────────────────────

const SWITCH_VAR: &str = "RSAC_HOTPLUG_SWITCH";
const ENDPOINT_A_VAR: &str = "RSAC_HOTPLUG_ENDPOINT_A";
const ENDPOINT_B_VAR: &str = "RSAC_HOTPLUG_ENDPOINT_B";
const LOSS_VAR: &str = "RSAC_HOTPLUG_LOSS";
const ALLOW_DISABLE_VAR: &str = "RSAC_HOTPLUG_ALLOW_DISABLE";

// ── Player format ────────────────────────────────────────────────────────

/// Format the tone players render (shared mode converts it to the mix format).
const PLAYER_RATE: u32 = 48_000;
const PLAYER_CHANNELS: usize = 2;

/// `E_NOTFOUND`: `GetDefaultAudioEndpoint` finds no endpoint for the role.
const E_NOTFOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

const RESTORE_TRIES: usize = 3;
const RESTORE_RETRY_DELAY: Duration = Duration::from_millis(200);
/// Recovery events carry receipt times rather than emission times. Allow
/// 100 ms for polling and scheduling skew in the lower bound only.
const EXHAUST_OBSERVATION_TOLERANCE: Duration = Duration::from_millis(100);

fn same_id(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

// ── COM ──────────────────────────────────────────────────────────────────

/// COM initialised (MTA) on the calling thread for one scope.
struct ComScope {
    initialized: bool,
}

impl ComScope {
    fn enter() -> Result<Self, String> {
        // SAFETY: no reserved pointer. A thread already in an STA gets
        // RPC_E_CHANGED_MODE and keeps a usable apartment; only a successful
        // call (S_OK or S_FALSE) is balanced with CoUninitialize on drop.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        match hr {
            S_OK | S_FALSE => Ok(Self { initialized: true }),
            RPC_E_CHANGED_MODE => Ok(Self { initialized: false }),
            _ => Err(format!("CoInitializeEx(MTA) failed: {hr:?}")),
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `f` with COM initialised on this thread. `f` must return plain data:
/// every COM object it creates is released inside it, before COM is
/// uninitialised.
fn in_com<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let _com = ComScope::enter()?;
    f()
}

/// NUL-terminated UTF-16 copy of `s`.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── IPolicyConfig ────────────────────────────────────────────────────────

/// `CLSID_PolicyConfigClient` {870af99c-171d-4f9e-af0d-e63df40c2bc9}.
const CLSID_POLICY_CONFIG: GUID = GUID::from_values(
    0x870a_f99c,
    0x171d,
    0x4f9e,
    [0xaf, 0x0d, 0xe6, 0x3d, 0xf4, 0x0c, 0x2b, 0xc9],
);
const IID_POLICY_CONFIG_TEXT: &str = "f8679f50-850a-41cf-9c72-430f290290c8";

/// The undocumented `IPolicyConfig` interface (IID
/// {f8679f50-850a-41cf-9c72-430f290290c8}), declared by hand because the
/// `windows` crate does not ship it.
#[repr(transparent)]
struct PolicyConfig(IUnknown);

impl Clone for PolicyConfig {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// `IPolicyConfig`'s vtable: `IUnknown`, the ten methods this test never
/// calls (`GetMixFormat`, `GetDeviceFormat`, `ResetDeviceFormat`,
/// `SetDeviceFormat`, `GetProcessingPeriod`, `SetProcessingPeriod`,
/// `GetShareMode`, `SetShareMode`, `GetPropertyValue`, `SetPropertyValue`),
/// then `SetDefaultEndpoint` and `SetEndpointVisibility`, in that order.
// COM builds this table, never Rust code: it is only read through the
// interface pointer, which the dead-code lint cannot see as a construction.
#[allow(dead_code)]
#[repr(C)]
struct PolicyConfigVtbl {
    _base: IUnknown_Vtbl,
    _get_mix_format: usize,
    _get_device_format: usize,
    _reset_device_format: usize,
    _set_device_format: usize,
    _get_processing_period: usize,
    _set_processing_period: usize,
    _get_share_mode: usize,
    _set_share_mode: usize,
    _get_property_value: usize,
    _set_property_value: usize,
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
    set_endpoint_visibility: unsafe extern "system" fn(*mut c_void, PCWSTR, BOOL) -> HRESULT,
}

// SAFETY: `PolicyConfig` is a transparent COM interface pointer, and
// `PolicyConfigVtbl` reproduces IPolicyConfig's vtable layout (IUnknown, ten
// pointer-sized slots, then the two methods used here).
unsafe impl Interface for PolicyConfig {
    type Vtable = PolicyConfigVtbl;
    const IID: GUID = GUID::from_values(
        0xf867_9f50,
        0x850a,
        0x41cf,
        [0x9c, 0x72, 0x43, 0x0f, 0x29, 0x02, 0x90, 0xc8],
    );
}

impl PolicyConfig {
    /// Creates the policy-config object; COM must be initialised.
    fn create() -> Result<Self, String> {
        // SAFETY: a valid CLSID and no aggregation; the interface is used only
        // once the call succeeded.
        let unknown: IUnknown = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
            .map_err(|e| {
                format!("CoCreateInstance(PolicyConfig, CLSID {CLSID_POLICY_CONFIG:?}) failed: {e}")
            })?;
        unknown.cast::<PolicyConfig>().map_err(|e| {
            format!(
                "the PolicyConfig object does not answer IPolicyConfig \
                 (IID {IID_POLICY_CONFIG_TEXT}): {e}"
            )
        })
    }

    fn set_default_endpoint(&self, id: &str, role: Role) -> Result<(), String> {
        let id_w = wide(id);
        // SAFETY: `self` is a live IPolicyConfig and `id_w` is NUL-terminated
        // and outlives the call.
        unsafe {
            (self.vtable().set_default_endpoint)(self.as_raw(), PCWSTR(id_w.as_ptr()), role.erole())
        }
        .ok()
        .map_err(|e| {
            format!(
                "IPolicyConfig::SetDefaultEndpoint({id}, {}) failed: {e}",
                role.name()
            )
        })
    }

    fn set_endpoint_visibility(&self, id: &str, visible: bool) -> Result<(), String> {
        let id_w = wide(id);
        // SAFETY: as in `set_default_endpoint`.
        unsafe {
            (self.vtable().set_endpoint_visibility)(
                self.as_raw(),
                PCWSTR(id_w.as_ptr()),
                BOOL::from(visible),
            )
        }
        .ok()
        .map_err(|e| format!("IPolicyConfig::SetEndpointVisibility({id}, {visible}) failed: {e}"))
    }
}

/// Runs `f` against a fresh policy-config object, with COM initialised.
fn with_policy_config(f: impl FnOnce(&PolicyConfig) -> Result<(), String>) -> Result<(), String> {
    in_com(|| {
        let policy = PolicyConfig::create()?;
        f(&policy)
    })
}

// ── Default endpoints ────────────────────────────────────────────────────

/// Default-endpoint roles of the render direction.
#[derive(Clone, Copy, Debug)]
enum Role {
    Console,
    Multimedia,
    Communications,
}

impl Role {
    /// The roles the Sound settings change together.
    const SOUND_SETTINGS: [Role; 2] = [Role::Console, Role::Multimedia];
    /// All roles, for the communications and endpoint-loss tests.
    const WITH_COMMUNICATIONS: [Role; 3] = [Role::Console, Role::Multimedia, Role::Communications];

    fn name(self) -> &'static str {
        match self {
            Role::Console => "eConsole",
            Role::Multimedia => "eMultimedia",
            Role::Communications => "eCommunications",
        }
    }

    fn erole(self) -> ERole {
        match self {
            Role::Console => eConsole,
            Role::Multimedia => eMultimedia,
            Role::Communications => eCommunications,
        }
    }

    fn wasapi(self) -> wasapi::Role {
        match self {
            Role::Console => wasapi::Role::Console,
            Role::Multimedia => wasapi::Role::Multimedia,
            Role::Communications => wasapi::Role::Communications,
        }
    }
}

/// The default render endpoint's id for `role`, `None` when there is none.
fn read_default(role: Role) -> Result<Option<String>, String> {
    in_com(|| {
        let enumerator = wasapi::DeviceEnumerator::new()
            .map_err(|e| format!("create the device enumerator: {e}"))?;
        match enumerator.get_default_device_for_role(wasapi::Direction::Render, role.wasapi()) {
            Ok(device) => device
                .get_id()
                .map(Some)
                .map_err(|e| format!("read the {} default endpoint's id: {e}", role.name())),
            Err(wasapi::WasapiError::Windows(e)) if e.code() == E_NOTFOUND => Ok(None),
            Err(e) => Err(format!(
                "read the {} default render endpoint: {e}",
                role.name()
            )),
        }
    })
}

fn default_text(role: Role) -> String {
    match read_default(role) {
        Ok(Some(id)) => id,
        Ok(None) => "<none>".to_string(),
        Err(e) => format!("<unreadable: {e}>"),
    }
}

/// The current default endpoints, for failure messages.
fn machine_state() -> String {
    format!(
        "current defaults: eConsole={} eMultimedia={} eCommunications={}",
        default_text(Role::Console),
        default_text(Role::Multimedia),
        default_text(Role::Communications)
    )
}

fn sleep_idle(pause: Duration) {
    std::thread::sleep(pause);
}

/// Polls `role`'s default every 50 ms for up to 2 s until it is `id`. `idle`
/// runs between reads (a session keeps its capture polled there).
fn wait_for_default(role: Role, id: &str, idle: &mut dyn FnMut(Duration)) -> Result<(), String> {
    let deadline = Instant::now() + READBACK_TIMEOUT;
    loop {
        let current = read_default(role);
        let observed_at = Instant::now();
        let matched = matches!(&current, Ok(Some(now)) if same_id(now, id));
        if matched && observed_at <= deadline {
            return Ok(());
        }
        if observed_at >= deadline {
            if matched {
                return Err(format!(
                    "default endpoint became {id} after {READBACK_TIMEOUT:?}"
                ));
            }
            let still = match current {
                Ok(Some(now)) => now,
                Ok(None) => "<none>".to_string(),
                Err(e) => format!("<unreadable: {e}>"),
            };
            return Err(format!(
                "default endpoint did not become {id} (still {still})"
            ));
        }
        idle(READBACK_INTERVAL);
    }
}

/// Sets `id` as the default render endpoint the way the Sound settings do
/// (eConsole and eMultimedia; eCommunications is left alone), then reads the
/// eConsole default back until it is `id`.
fn set_default(id: &str, idle: &mut dyn FnMut(Duration)) -> Result<(), String> {
    with_policy_config(|policy| {
        for role in Role::SOUND_SETTINGS {
            policy.set_default_endpoint(id, role)?;
        }
        Ok(())
    })?;
    wait_for_default(Role::Console, id, idle)
}

/// Sets `id` as the default render endpoint for `role` alone, then reads that
/// role's default back until it is `id`.
fn set_role_default(id: &str, role: Role, idle: &mut dyn FnMut(Duration)) -> Result<(), String> {
    with_policy_config(|policy| policy.set_default_endpoint(id, role))?;
    wait_for_default(role, id, idle)
}

fn verify_default(role: Role, wanted: &str) -> Result<(), String> {
    match read_default(role)? {
        Some(id) if same_id(&id, wanted) => Ok(()),
        current => Err(format!(
            "{} should be {wanted}, read {current:?}",
            role.name()
        )),
    }
}

fn retry_restore(mut restore: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    for _ in 1..RESTORE_TRIES {
        if restore().is_ok() {
            return Ok(());
        }
        std::thread::sleep(RESTORE_RETRY_DELAY);
    }
    restore()
}

/// Records the original defaults of the given roles and puts them back when
/// dropped, including while a failed assertion unwinds. Declare it before the
/// players and the capture so it drops after both have stopped.
struct DefaultsGuard {
    original: Vec<(Role, String)>,
    armed: bool,
    restore_result: Option<bool>,
}

impl DefaultsGuard {
    fn record(roles: &[Role]) -> Result<Self, String> {
        let mut original = Vec::with_capacity(roles.len());
        for &role in roles {
            let id = read_default(role)?.ok_or_else(|| {
                format!(
                    "{} has no original default; refusing to change it",
                    role.name()
                )
            })?;
            original.push((role, id));
        }
        Ok(Self {
            original,
            armed: false,
            restore_result: None,
        })
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn describe(&self) -> String {
        self.original
            .iter()
            .map(|(role, id)| format!("{}={id}", role.name()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Restores all saved roles, then verifies them together: a later setter
    /// must not silently change a role already checked by an earlier setter.
    fn restore(&mut self) -> bool {
        if !self.armed {
            return true;
        }
        if let Some(result) = self.restore_result {
            return result;
        }
        let mut failures = Vec::new();
        let original = &self.original;
        let result = retry_restore(|| {
            failures.clear();
            for (role, wanted) in original {
                if let Err(e) = set_role_default(wanted, *role, &mut sleep_idle) {
                    failures.push((*role, wanted, e));
                }
            }
            for (role, wanted) in original {
                if let Err(e) = verify_default(*role, wanted) {
                    failures.push((*role, wanted, e));
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err("not every default role was restored".to_owned())
            }
        });
        for (role, wanted, e) in failures {
            eprintln!(
                "HOTPLUG-RESTORE-FAILED role={} wanted={wanted}",
                role.name()
            );
            eprintln!("  {e}");
        }
        let restored = result.is_ok();
        self.restore_result = Some(restored);
        restored
    }
}

impl Drop for DefaultsGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

// ── Endpoints ────────────────────────────────────────────────────────────

/// State of the render endpoint `id`, or `None` if Windows no longer lists it.
fn endpoint_state(id: &str) -> Result<Option<wasapi::DeviceState>, String> {
    in_com(|| {
        let enumerator = wasapi::DeviceEnumerator::new()
            .map_err(|e| format!("create the device enumerator: {e}"))?;
        let device = match enumerator.get_device(id) {
            Ok(device) => device,
            Err(wasapi::WasapiError::Windows(e)) if e.code() == E_NOTFOUND => return Ok(None),
            Err(e) => return Err(format!("lookup for endpoint {id} failed: {e}")),
        };
        if device.get_direction() != wasapi::Direction::Render {
            return Err(format!("{id} is a capture endpoint, not a render endpoint"));
        }
        device
            .get_state()
            .map(Some)
            .map_err(|e| format!("read the state of {id}: {e}"))
    })
}

/// Verifies the single-endpoint loss fixture has actually lost the endpoint.
fn verify_outage(id: &str) -> Result<(), String> {
    let state = endpoint_state(id)?;
    let default = read_default(Role::Console)?;
    if state == Some(wasapi::DeviceState::Active) || default.is_some() {
        return Err(format!(
            "expected endpoint {id} to be inactive or absent and eConsole unset; \
             endpoint state={state:?}, eConsole={default:?}"
        ));
    }
    Ok(())
}

fn wait_until_absent(id: &str) -> Result<(), String> {
    let deadline = Instant::now() + READBACK_TIMEOUT;
    loop {
        let result = verify_outage(id);
        let observed_at = Instant::now();
        if result.is_ok() && observed_at <= deadline {
            return Ok(());
        }
        if observed_at >= deadline {
            return Err(format!(
                "endpoint outage not verified within {READBACK_TIMEOUT:?}: {result:?}"
            ));
        }
        std::thread::sleep(READBACK_INTERVAL);
    }
}

fn wait_until_active(id: &str, deadline: Instant) -> Result<(), String> {
    loop {
        let state = endpoint_state(id);
        let observed_at = Instant::now();
        if matches!(state, Ok(Some(wasapi::DeviceState::Active))) && observed_at <= deadline {
            return Ok(());
        }
        if observed_at >= deadline {
            let last = match state {
                Ok(Some(state)) => state.to_string(),
                Ok(None) => "<absent>".to_owned(),
                Err(e) => e,
            };
            return Err(format!(
                "endpoint {id} was not active within {ENDPOINT_ACTIVE_TIMEOUT:?} (last: {last})"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// An endpoint id from `var`: required, and an active render endpoint.
fn endpoint_from_env(var: &str) -> String {
    let id = std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{SWITCH_VAR}=1 requires {var}: an active render endpoint id as `rsac list` \
                 prints it ({{0.0.0.00000000}}.{{guid}}); it is not set\n{}",
                machine_state()
            )
        });
    match endpoint_state(&id) {
        Ok(Some(wasapi::DeviceState::Active)) => id,
        Ok(state) => panic!(
            "{var}={id} is not an active render endpoint (state {state:?})\n{}",
            machine_state()
        ),
        Err(e) => panic!(
            "{var}={id} is not a usable render endpoint: {e}\n{}",
            machine_state()
        ),
    }
}

/// Endpoints A and B for the switch tests.
fn switch_endpoints() -> (String, String) {
    let a = endpoint_from_env(ENDPOINT_A_VAR);
    let b = endpoint_from_env(ENDPOINT_B_VAR);
    assert!(
        !same_id(&a, &b),
        "{ENDPOINT_A_VAR} and {ENDPOINT_B_VAR} must be two different endpoints, both are {a}"
    );
    (a, b)
}

// ── Endpoint loss (CI only) ──────────────────────────────────────────────

/// Hides the selected render endpoint. On drop, makes it visible and checks
/// that it is active again, printing `HOTPLUG-RESTORE-FAILED endpoint=<id>`
/// if restoration fails.
struct EndpointLoss {
    endpoint: String,
    disabled: bool,
    armed: bool,
    restore_result: Option<bool>,
}

impl EndpointLoss {
    fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            disabled: false,
            armed: false,
            restore_result: None,
        }
    }

    fn disable(&mut self, defaults: &mut DefaultsGuard) -> Result<(), String> {
        verify_default(Role::Console, &self.endpoint)?;
        if endpoint_state(&self.endpoint)? != Some(wasapi::DeviceState::Active) {
            return Err(format!("{} is not active before disable", self.endpoint));
        }
        // Arm both guards only after preflight, but before the command: a
        // disable that failed halfway still needs rollback.
        defaults.arm();
        self.armed = true;
        self.disabled = true;
        self.set_visibility(false)?;
        wait_until_absent(&self.endpoint)
    }

    /// Re-enables the endpoint and waits until it is active again.
    fn enable(&mut self) -> Result<(), String> {
        if self.disabled {
            self.set_visibility(true)?;
        }
        let deadline = Instant::now() + ENDPOINT_ACTIVE_TIMEOUT;
        wait_until_active(&self.endpoint, deadline)?;
        self.disabled = false;
        Ok(())
    }

    fn set_visibility(&self, visible: bool) -> Result<(), String> {
        with_policy_config(|policy| policy.set_endpoint_visibility(&self.endpoint, visible))
    }

    fn restore(&mut self) -> bool {
        if !self.armed {
            return true;
        }
        if let Some(result) = self.restore_result {
            return result;
        }
        let result = retry_restore(|| self.enable());
        if let Err(e) = &result {
            eprintln!("HOTPLUG-RESTORE-FAILED endpoint={}", self.endpoint);
            eprintln!("  {e}");
        }
        let restored = result.is_ok();
        self.restore_result = Some(restored);
        restored
    }
}

impl Drop for EndpointLoss {
    fn drop(&mut self) {
        self.restore();
    }
}

// ── Tone players ─────────────────────────────────────────────────────────

#[derive(Default)]
struct PlayerStatus {
    /// Set once a stream started with its first buffer written.
    started: AtomicBool,
    opens: AtomicU64,
    failures: AtomicU64,
    frames: AtomicU64,
    /// Written only on the error path, never while rendering.
    last_error: Mutex<Option<String>>,
}

/// A player's identity and live counters, for failure messages.
#[derive(Clone)]
struct PlayerView {
    label: &'static str,
    endpoint: String,
    freq: f64,
    status: Arc<PlayerStatus>,
}

impl PlayerView {
    fn describe(&self) -> String {
        let s = &self.status;
        let frames = s.frames.load(Ordering::Relaxed);
        let last_error = s
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| "none".to_string());
        format!(
            "player {} ({} Hz on {}): started={} opens={} failures={} rendered={:.1} s \
             last_error={last_error}",
            self.label,
            self.freq,
            self.endpoint,
            s.started.load(Ordering::Relaxed),
            s.opens.load(Ordering::Relaxed),
            s.failures.load(Ordering::Relaxed),
            frames as f64 / f64::from(PLAYER_RATE),
        )
    }
}

/// A shared-mode WASAPI render thread bound to one endpoint **by id**,
/// playing a continuous sine on every channel until dropped. When its
/// endpoint becomes invalid it reopens it every 200 ms.
struct TonePlayer {
    view: PlayerView,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl TonePlayer {
    /// Starts the player and waits up to 5 s for its first buffer.
    fn start(label: &'static str, endpoint: &str, freq: f64) -> Result<Self, String> {
        let view = PlayerView {
            label,
            endpoint: endpoint.to_string(),
            freq,
            status: Arc::default(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let endpoint = view.endpoint.clone();
            let status = Arc::clone(&view.status);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name(format!("hotplug-player-{label}"))
                .spawn(move || {
                    in_com(|| {
                        run_player(&endpoint, freq, &stop, &status);
                        Ok(())
                    })
                })
                .map_err(|e| format!("cannot spawn player {label}: {e}"))?
        };
        let mut player = Self {
            view,
            stop,
            thread: Some(thread),
        };
        let deadline = Instant::now() + PLAYER_FIRST_BUFFER_TIMEOUT;
        loop {
            let started = player.view.status.started.load(Ordering::Acquire);
            let at = Instant::now();
            if player.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                player.stop()?;
                return Err(format!("{} exited before startup", player.view.describe()));
            }
            if started && player.frames() > 0 && at <= deadline {
                return Ok(player);
            }
            if at >= deadline {
                return Err(format!(
                    "{} delivered no first buffer within {PLAYER_FIRST_BUFFER_TIMEOUT:?}",
                    player.view.describe()
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn frames(&self) -> u64 {
        self.view.status.frames.load(Ordering::Relaxed)
    }

    fn stop(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err(format!("{} panicked", self.view.describe())),
            Some(Ok(Err(e))) => Err(format!("{} exited: {e}", self.view.describe())),
            Some(Ok(Ok(()))) | None => Ok(()),
        }
    }

    fn view(&self) -> PlayerView {
        self.view.clone()
    }
}

impl Drop for TonePlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn require_player_progress(s: &Session, players: [(&TonePlayer, u64); 2]) {
    for (player, before) in players {
        let after = player.frames();
        if after <= before {
            s.fail(&format!(
                "no successful render writes during the control ({before} → {after} frames): {}",
                player.view.describe()
            ));
        }
    }
}

fn require_no_events(s: &Session, from: usize) -> Result<(), String> {
    if let Some(e) = s.events()[from..].first() {
        return Err(format!(
            "unexpected event {:?} at {}",
            e.value,
            fmt_at(e.at)
        ));
    }
    Ok(())
}

fn stop_players<const N: usize>(s: &mut Session, mut players: [TonePlayer; N]) {
    if let Some(snapshot) = s.capture.recovery_snapshot() {
        if snapshot.dropped_events != 0 {
            s.fail(&format!(
                "cannot certify the recovery event log: {snapshot:?}"
            ));
        }
    }
    s.stop();
    for player in players.iter_mut().rev() {
        if let Err(e) = player.stop() {
            s.fail(&e);
        }
    }
}

fn run_player(endpoint: &str, freq: f64, stop: &AtomicBool, status: &PlayerStatus) {
    let mut phase = 0.0_f64;
    while !stop.load(Ordering::Acquire) {
        if let Err(error) = play_session(endpoint, freq, stop, status, &mut phase) {
            status.failures.fetch_add(1, Ordering::Relaxed);
            *status
                .last_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(error);
            let resume = Instant::now() + PLAYER_REOPEN_INTERVAL;
            while !stop.load(Ordering::Acquire) && Instant::now() < resume {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// One render stream on `endpoint`, from open until `stop` (Ok) or the first
/// failure (Err). `phase` carries the sine across reopens.
fn play_session(
    endpoint: &str,
    freq: f64,
    stop: &AtomicBool,
    status: &PlayerStatus,
    phase: &mut f64,
) -> Result<(), String> {
    let enumerator = wasapi::DeviceEnumerator::new()
        .map_err(|e| format!("create the device enumerator: {e}"))?;
    let device = enumerator
        .get_device(endpoint)
        .map_err(|e| format!("look up the endpoint: {e}"))?;
    let state = device
        .get_state()
        .map_err(|e| format!("read the endpoint state: {e}"))?;
    if state != wasapi::DeviceState::Active {
        return Err(format!("the endpoint is {state}"));
    }
    let mut client = device
        .get_iaudioclient()
        .map_err(|e| format!("activate IAudioClient: {e}"))?;
    let format = wasapi::WaveFormat::new(
        32,
        32,
        wasapi::SampleType::Float,
        PLAYER_RATE as usize,
        PLAYER_CHANNELS,
        None,
    );
    let mode = wasapi::StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client
        .initialize_client(&format, wasapi::Direction::Render, mode, None)
        .map_err(|e| format!("initialize the render client: {e}"))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| format!("set the event handle: {e}"))?;
    let render = client
        .get_audiorenderclient()
        .map_err(|e| format!("get IAudioRenderClient: {e}"))?;
    let capacity = client
        .get_buffer_size()
        .map_err(|e| format!("read the buffer size: {e}"))? as usize;
    // Sized once for a whole device buffer, so rendering never reallocates.
    let mut bytes = Vec::with_capacity(capacity * PLAYER_CHANNELS * std::mem::size_of::<f32>());
    let step = std::f64::consts::TAU * freq / f64::from(PLAYER_RATE);
    let amplitude = f64::from(TONE_AMPLITUDE);
    let mut started = false;
    loop {
        if stop.load(Ordering::Acquire) {
            if started {
                let _ = client.stop_stream();
            }
            return Ok(());
        }
        let frames = client
            .get_available_space_in_frames()
            .map_err(|e| format!("query free buffer space: {e}"))? as usize;
        if frames > 0 {
            bytes.clear();
            for _ in 0..frames {
                let sample = (amplitude * phase.sin()) as f32;
                for _ in 0..PLAYER_CHANNELS {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                *phase += step;
                if *phase >= std::f64::consts::TAU {
                    *phase -= std::f64::consts::TAU;
                }
            }
            render
                .write_to_device(&bytes, None)
                .map_err(|e| format!("write {frames} frames: {e}"))?;
            status.frames.fetch_add(frames as u64, Ordering::Relaxed);
        }
        if !started {
            // The first write above filled the whole buffer before the start.
            client
                .start_stream()
                .map_err(|e| format!("start the stream: {e}"))?;
            started = true;
            status.opens.fetch_add(1, Ordering::Relaxed);
            status.started.store(true, Ordering::Release);
        }
        match event.wait_for_event(200) {
            Ok(()) | Err(wasapi::WasapiError::EventTimeout) => {}
            Err(e) => return Err(format!("wait for the render event: {e}")),
        }
    }
}

fn start_player(test: &str, label: &'static str, endpoint: &str, freq: f64) -> TonePlayer {
    TonePlayer::start(label, endpoint, freq)
        .unwrap_or_else(|e| panic!("{test}: {e}\n{}", machine_state()))
}

// ── Capture session glue ─────────────────────────────────────────────────

/// What a failure dump prints about this machine: the default endpoints of
/// all three roles and the tone players.
struct WindowsMachine {
    players: Vec<PlayerView>,
}

impl Machine for WindowsMachine {
    fn defaults(&self) -> String {
        machine_state()
    }

    fn players(&self) -> Vec<String> {
        self.players.iter().map(PlayerView::describe).collect()
    }
}

fn machine(players: Vec<PlayerView>) -> Box<dyn Machine> {
    Box::new(WindowsMachine { players })
}

/// Switches the default render endpoint (eConsole and eMultimedia) to `id`,
/// polling the capture while the switch is read back.
fn switch_default(s: &mut Session, id: &str) {
    let result = set_default(id, &mut |pause| s.pump_for(pause));
    if let Err(e) = result {
        s.fail(&e);
    }
}

/// Sets only `role`'s default to `id`, polling the capture while it is read
/// back.
fn switch_role_default(s: &mut Session, id: &str, role: Role) {
    let result = set_role_default(id, role, &mut |pause| s.pump_for(pause));
    if let Err(e) = result {
        s.fail(&e);
    }
}

/// The pass path's last step, after the capture and players stopped:
/// re-enable a disabled endpoint, restore the original defaults, and only
/// then print the evidence line.
fn finish(
    test: &str,
    loss: Option<&mut EndpointLoss>,
    defaults: &mut DefaultsGuard,
    evidence: &str,
) {
    if let Some(loss) = loss {
        if !loss.restore() {
            panic!(
                "{test}: re-enabling {} was not verified; see HOTPLUG-RESTORE-FAILED\n{}",
                loss.endpoint,
                machine_state()
            );
        }
    }
    if !defaults.restore() {
        panic!(
            "{test}: restoring the original defaults ({}) failed; see HOTPLUG-RESTORE-FAILED \
             above\n{}",
            defaults.describe(),
            machine_state()
        );
    }
    println!("{evidence}");
}

fn record_defaults(test: &str, roles: &[Role]) -> DefaultsGuard {
    DefaultsGuard::record(roles)
        .unwrap_or_else(|e| panic!("{test}: cannot read the original default endpoints: {e}"))
}

/// Gate and inputs of a loss test: `None` when it skips.
fn loss_setup(test: &str) -> Option<String> {
    if !enabled(test, LOSS_VAR) {
        return None;
    }
    // The device-node ("pnp") mode was removed. A caller that still selects it
    // must not get a different test under the old name: any value other than
    // exactly `visibility` (or empty) is refused, including one that is not
    // valid Unicode.
    if let Some(mode) = std::env::var_os("RSAC_HOTPLUG_LOSS_MODE") {
        assert!(
            mode.is_empty() || mode == "visibility",
            "{test}: RSAC_HOTPLUG_LOSS_MODE={mode:?} is no longer supported: the loss tests only \
             hide the endpoint. Unset it (and RSAC_HOTPLUG_PNP_INSTANCE_ID)"
        );
    }
    // `GITHUB_ACTIONS`, not `CI`: many developer tools export `CI=true` on a
    // workstation, and this test takes an endpoint away.
    let on_runner = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
    let allowed = std::env::var(ALLOW_DISABLE_VAR).as_deref() == Ok("1");
    assert!(
        on_runner || allowed,
        "{test}: refusing to disable an audio endpoint: {LOSS_VAR}=1 also needs \
         GITHUB_ACTIONS=true or {ALLOW_DISABLE_VAR}=1, so that a workstation's device is never \
         disabled by accident"
    );
    let endpoint = match read_default(Role::Console) {
        Ok(Some(id)) => id,
        Ok(None) => panic!("{test}: there is no default render endpoint to take away"),
        Err(e) => panic!("{test}: cannot read the default render endpoint: {e}"),
    };
    match endpoint_state(&endpoint) {
        Ok(Some(wasapi::DeviceState::Active)) => {}
        other => panic!("{test}: the default render endpoint {endpoint} is not active: {other:?}"),
    }
    Some(endpoint)
}

// ── Switch tests (RSAC_HOTPLUG_SWITCH=1) ─────────────────────────────────

/// A supervised `SystemDefault` capture follows the default render endpoint
/// from A to B and back, and the delivered audio switches tone with it.
#[test]
fn hotplug_switch_follows_default_with_recovery() {
    const TEST: &str = "hotplug_switch_follows_default_with_recovery";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    init_test_logging();
    let (a, b) = switch_endpoints();

    // 1. Guard, default to A, both players.
    let mut defaults = record_defaults(TEST, &Role::SOUND_SETTINGS);
    defaults.arm();
    set_default(&a, &mut sleep_idle).unwrap_or_else(|e| panic!("{TEST}: {e}"));
    let player_a = start_player(TEST, "A", &a, TONE_A_HZ);
    let player_b = start_player(TEST, "B", &b, TONE_B_HZ);

    // 2. Supervised capture, subscribed before start().
    let mut s = Session::open(
        TEST,
        Some(AutoRecoveryConfig::new()),
        machine(vec![player_a.view(), player_b.view()]),
        format!("test={TEST} a={a} b={b} original {}", defaults.describe()),
    );
    s.forbid_exhausted = true;
    s.start();

    // 3. A, all in generation 0.
    let deadline = s.elapsed() + SEE_TIMEOUT;
    s.expect_sees("capture sees A in generation 0", Verdict::A, 0, 0, deadline);
    if let Some(generation) = s.generations.keys().find(|&&g| g != 0) {
        s.fail(&format!(
            "generation {generation} delivered audio before any default change"
        ));
    }

    // 4. Default to B; from here a B-dominant window must never be followed by an A-dominant one.
    s.forbid_a_after_b = true;
    let switch_1 = s.checkpoint();
    switch_default(&mut s, &b);
    s.mark(format!("default switched to B ({b}) and read back"));

    // 5. DefaultChanged, then Recovered with generation 1.
    let recovered_1 = |e: &Timed<RecoveryEvent>| recovered_generation(e) == Some(1);
    let steps: [Step<'_, Timed<RecoveryEvent>>; 2] = [&is_default_changed, &recovered_1];
    let found = s.expect_events_in_order(
        "DefaultChanged then Recovered (generation 1) after switching to B",
        switch_1.events,
        switch_1.at + RECOVER_TIMEOUT,
        &steps,
    );
    let recovered_1_at = s.events()[found[1]].at;
    s.elapsed_between(
        "DefaultChanged after switching to B",
        switch_1.at,
        s.events()[found[0]].at,
    );
    let fresh = s.fresh_checkpoint();

    // 6. B in generation 1.
    s.expect_sees(
        "capture sees B in generation 1 after the first Recovered",
        Verdict::B,
        1,
        fresh.windows,
        recovered_1_at + SEE_TIMEOUT,
    );

    // 7. Back to A: Recovered with generation 2, then A in generation 2.
    s.forbid_a_after_b = false;
    let switch_2 = s.checkpoint();
    switch_default(&mut s, &a);
    s.mark(format!("default switched back to A ({a}) and read back"));
    let second = |e: &Timed<RecoveryEvent>| recovered_generation(e) == Some(2);
    let steps: [Step<'_, Timed<RecoveryEvent>>; 1] = [&second];
    let found = s.expect_events_in_order(
        "a second Recovered (generation 2) after switching back to A",
        switch_2.events,
        switch_2.at + RECOVER_TIMEOUT,
        &steps,
    );
    let recovered_2_at = s.events()[found[0]].at;
    let fresh = s.fresh_checkpoint();
    s.expect_sees(
        "capture sees A in generation 2 after the second Recovered",
        Verdict::A,
        2,
        fresh.windows,
        recovered_2_at + SEE_TIMEOUT,
    );

    // 8. Snapshot.
    s.pump();
    s.expect_running_snapshot(2, 2);

    // 9. Stop, restore, evidence.
    let evidence = evidence_line(
        TEST,
        &format!(
            "a={a} b={b} generations={} recover_ms={},{} min_dominance_db={:.1} windows={}",
            s.generation_list(),
            s.elapsed_between("recovery on B", switch_1.at, recovered_1_at)
                .as_millis(),
            s.elapsed_between("recovery back on A", switch_2.at, recovered_2_at)
                .as_millis(),
            s.min_dominance_db(),
            s.windows.len()
        ),
    );
    stop_players(&mut s, [player_a, player_b]);
    finish(TEST, None, &mut defaults, &evidence);
}

/// The negative control: without auto-recovery the capture stays on the old
/// endpoint after a default switch, which proves the tones identify the
/// endpoint and that recovery is what moves the capture.
#[test]
fn hotplug_switch_without_recovery_stays_on_old_endpoint() {
    const TEST: &str = "hotplug_switch_without_recovery_stays_on_old_endpoint";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    init_test_logging();
    let (a, b) = switch_endpoints();

    let mut defaults = record_defaults(TEST, &Role::SOUND_SETTINGS);
    defaults.arm();
    set_default(&a, &mut sleep_idle).unwrap_or_else(|e| panic!("{TEST}: {e}"));
    let player_a = start_player(TEST, "A", &a, TONE_A_HZ);
    let player_b = start_player(TEST, "B", &b, TONE_B_HZ);

    let mut s = Session::open(
        TEST,
        None,
        machine(vec![player_a.view(), player_b.view()]),
        format!("test={TEST} a={a} b={b} original {}", defaults.describe()),
    );
    s.start();

    let deadline = s.elapsed() + SEE_TIMEOUT;
    s.expect_sees("capture sees A", Verdict::A, 0, 0, deadline);

    // Observe only complete windows after B is confirmed as default.
    switch_default(&mut s, &b);
    s.mark(format!("default switched to B ({b}) and read back"));
    let since = s.checkpoint();
    let before = [
        (&player_a, player_a.frames()),
        (&player_b, player_b.frames()),
    ];
    let a_windows = s.expect_stays_on_a(
        "3 s of delivered audio after switching to B stay on A, in generation 0",
        since,
        |_| Ok(()),
    );
    require_player_progress(&s, before);
    if let Err(e) = verify_default(Role::Console, &b) {
        s.fail(&format!("default moved during the control: {e}"));
    }

    let evidence = evidence_line(
        TEST,
        &format!("a={a} b={b} a_windows={a_windows} b_windows=0"),
    );
    stop_players(&mut s, [player_a, player_b]);
    finish(TEST, None, &mut defaults, &evidence);
}

/// rsac opens and watches the eConsole default only, so a change of the
/// communications default must leave a supervised capture alone: no recovery,
/// and still endpoint A's audio.
#[test]
fn hotplug_communications_role_switch_is_ignored() {
    const TEST: &str = "hotplug_communications_role_switch_is_ignored";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    init_test_logging();
    let (a, b) = switch_endpoints();

    // 1. This test changes eCommunications, so its guard restores that role too.
    let mut defaults = record_defaults(TEST, &Role::WITH_COMMUNICATIONS);

    // 2. Verify A is the default for all three roles before starting.
    defaults.arm();
    set_default(&a, &mut sleep_idle).unwrap_or_else(|e| panic!("{TEST}: {e}"));
    set_role_default(&a, Role::Communications, &mut sleep_idle)
        .and_then(|()| verify_default(Role::Multimedia, &a))
        .and_then(|()| verify_default(Role::Console, &a))
        .unwrap_or_else(|e| panic!("{TEST}: {e}\n{}", machine_state()));
    let player_a = start_player(TEST, "A", &a, TONE_A_HZ);
    let player_b = start_player(TEST, "B", &b, TONE_B_HZ);
    let mut s = Session::open(
        TEST,
        Some(AutoRecoveryConfig::new()),
        machine(vec![player_a.view(), player_b.view()]),
        format!("test={TEST} a={a} b={b} original {}", defaults.describe()),
    );
    s.forbid_exhausted = true;
    s.start();
    let deadline = s.elapsed() + SEE_TIMEOUT;
    s.expect_sees("capture sees A in generation 0", Verdict::A, 0, 0, deadline);

    // 3. Prove an A → B communications transition without moving the other roles.
    if let Err(e) = verify_default(Role::Communications, &a) {
        s.fail(&e);
    }
    s.mark(format!("eCommunications verified on A ({a})"));
    let switching = s.checkpoint();
    switch_role_default(&mut s, &b, Role::Communications);
    for role in Role::SOUND_SETTINGS {
        if let Err(e) = verify_default(role, &a) {
            s.fail(&format!(
                "communications-only switch moved another role: {e}"
            ));
        }
    }
    s.mark(format!(
        "eCommunications verified on B ({b}); eConsole and eMultimedia still A"
    ));

    // 4. Count only fresh audio but cover recovery events during read-back too.
    let switched = s.checkpoint();
    let before = [
        (&player_a, player_a.frames()),
        (&player_b, player_b.frames()),
    ];
    let a_windows = s.expect_stays_on_a(
        "three seconds of fresh generation-0 A audio after the communications switch",
        switched,
        |s| require_no_events(s, switching.events),
    );
    require_player_progress(&s, before);
    for (role, wanted) in [
        (Role::Console, a.as_str()),
        (Role::Multimedia, a.as_str()),
        (Role::Communications, b.as_str()),
    ] {
        if let Err(e) = verify_default(role, wanted) {
            s.fail(&e);
        }
    }
    s.pump();
    if let Err(e) = require_no_events(&s, switching.events) {
        s.fail(&e);
    }
    s.expect_running_snapshot(0, 0);

    // 5. Stop, restore (eCommunications included), evidence.
    let evidence = evidence_line(
        TEST,
        &format!("a={a} b={b} a_windows={a_windows} b_windows=0 recovery_events=0"),
    );
    stop_players(&mut s, [player_a, player_b]);
    finish(TEST, None, &mut defaults, &evidence);
}

// ── Loss tests (RSAC_HOTPLUG_LOSS=1, CI only) ────────────────────────────

/// With the only render endpoint gone for longer than `device_wait_timeout`,
/// recovery gives up: an `Exhausted` event, an `Exhausted` snapshot, and a
/// terminal read.
#[test]
fn hotplug_loss_exhausts_after_wait() {
    const TEST: &str = "hotplug_loss_exhausts_after_wait";
    let _lock = machine_lock();
    let Some(endpoint) = loss_setup(TEST) else {
        return;
    };
    init_test_logging();

    let mut defaults = record_defaults(TEST, &Role::WITH_COMMUNICATIONS);
    let mut loss = EndpointLoss::new(&endpoint);
    let player = start_player(TEST, "A", &endpoint, TONE_A_HZ);

    let config = AutoRecoveryConfig::new().with_device_wait_timeout(EXHAUST_DEVICE_WAIT);
    let mut s = Session::open(
        TEST,
        Some(config),
        machine(vec![player.view()]),
        format!(
            "test={TEST} endpoint={endpoint} original {}",
            defaults.describe()
        ),
    );
    s.start();

    // 1. A, against the empty 997 Hz bin.
    let deadline = s.elapsed() + SEE_TIMEOUT;
    s.expect_sees("capture sees A in generation 0", Verdict::A, 0, 0, deadline);

    // 2. Take the endpoint away.
    s.expect_terminal = true;
    if let Err(e) = loss.disable(&mut defaults) {
        s.fail(&format!("hiding {endpoint} failed: {e}"));
    }
    let outage = s.checkpoint();
    s.mark("verified endpoint loss: inactive or absent and eConsole unset");

    // Exhausted.reason is public diagnostic text; give_up_waiting is the
    // only supervisor path that reports it. An initial retry is normal
    // after invalidation, before the first open proves the device is absent.
    // Recovery does not expose when the first lookup returned DeviceNotFound.
    // Use the first invalidation or default change observed after verified loss
    // as the start of the supervisor's observable timeline.
    let deadline = outage.at + EXHAUST_DEVICE_WAIT + EXHAUST_MARGIN;
    let minimum_wait = EXHAUST_DEVICE_WAIT - EXHAUST_OBSERVATION_TOLERANCE;
    let mut exhausted_at = None;
    s.wait_for_observation(
        "absent-device wait expires, then reads terminate",
        deadline,
        |s, _| {
            let after_outage = &s.events()[outage.events..];
            let Some((index, reason)) =
                after_outage
                    .iter()
                    .enumerate()
                    .find_map(|(index, event)| match &event.value {
                        RecoveryEvent::Exhausted { reason, .. } => Some((index, reason)),
                        _ => None,
                    })
            else {
                return Ok(None);
            };
            let event = &after_outage[index];
            let Some(start) = after_outage[..index].iter().find(|event| {
                matches!(
                    event.value,
                    RecoveryEvent::DeviceInvalidated { .. } | RecoveryEvent::DefaultChanged { .. }
                )
            }) else {
                return Err(format!(
                    "Exhausted at {} without a preceding DeviceInvalidated or DefaultChanged \
                     after verified loss at {}",
                    fmt_at(event.at),
                    fmt_at(outage.at)
                ));
            };
            let earliest = start.at + minimum_wait;
            if event.at < earliest
                || !reason.starts_with("auto-recovery gave up: no capture device appeared within ")
            {
                return Err(format!(
                    "expected absent-device expiry no earlier than {} (recovery began with \
                     {:?} at {}), got {:?} at {}",
                    fmt_at(earliest),
                    start.value,
                    fmt_at(start.at),
                    event.value,
                    fmt_at(event.at)
                ));
            }
            let Some(terminal) = &s.terminal else {
                return Ok(None);
            };
            if terminal.at < earliest {
                return Err(format!(
                    "terminal read arrived before the absent-device wait: {terminal:?}"
                ));
            }
            if !s.snapshot_is_exhausted() {
                return Ok(None);
            }
            exhausted_at = Some(event.at);
            Ok(Some(event.at.max(terminal.at)))
        },
    );
    let exhausted_at = exhausted_at.expect("observed absent-device exhaustion");
    if let Err(e) = verify_outage(&endpoint) {
        s.fail(&e);
    }
    // src/api/recovery.rs: once recovery gave up, every read returns
    // StreamEnded.
    let error = match &s.terminal {
        Some(t)
            if !t.value.is_recoverable() && matches!(t.value, AudioError::StreamEnded { .. }) =>
        {
            variant_name(&t.value)
        }
        other => s.fail(&format!(
            "the terminal read should be a non-recoverable StreamEnded (src/api/recovery.rs), \
             got {other:?}"
        )),
    };

    // 4. Stop, re-enable, restore, evidence.
    let evidence = evidence_line(
        TEST,
        &format!(
            "exhausted_ms={} error={error}",
            s.elapsed_between("verified loss to exhaustion", outage.at, exhausted_at)
                .as_millis()
        ),
    );
    stop_players(&mut s, [player]);
    finish(TEST, Some(&mut loss), &mut defaults, &evidence);
}

/// The render endpoint disappears for 2 s and returns: recovery publishes a
/// new generation and the capture delivers that endpoint's tone again.
#[test]
fn hotplug_loss_then_return_recovers() {
    const TEST: &str = "hotplug_loss_then_return_recovers";
    let _lock = machine_lock();
    let Some(endpoint) = loss_setup(TEST) else {
        return;
    };
    init_test_logging();

    let mut defaults = record_defaults(TEST, &Role::WITH_COMMUNICATIONS);
    let mut loss = EndpointLoss::new(&endpoint);
    let player = start_player(TEST, "A", &endpoint, TONE_A_HZ);

    let config = AutoRecoveryConfig::new().with_device_wait_timeout(RETURN_DEVICE_WAIT);
    let mut s = Session::open(
        TEST,
        Some(config),
        machine(vec![player.view()]),
        format!(
            "test={TEST} endpoint={endpoint} original {}",
            defaults.describe()
        ),
    );
    s.forbid_exhausted = true;
    s.start();

    // 1. A.
    let deadline = s.elapsed() + SEE_TIMEOUT;
    s.expect_sees("capture sees A in generation 0", Verdict::A, 0, 0, deadline);

    // 2. Hold a verified outage for at least 2 s while polling recovery.
    if let Err(e) = loss.disable(&mut defaults) {
        s.fail(&format!("hiding {endpoint} failed: {e}"));
    }
    let outage = s.elapsed();
    s.mark("verified endpoint loss: inactive or absent and eConsole unset");
    let until = outage + RETURN_OUTAGE;
    loop {
        if let Err(e) = verify_outage(&endpoint) {
            s.fail(&format!("outage did not persist: {e}"));
        }
        let Some(remaining) = until.checked_sub(s.elapsed()).filter(|d| !d.is_zero()) else {
            break;
        };
        s.pump_for(remaining.min(READBACK_INTERVAL));
    }
    if let Some(e) = s
        .events()
        .iter()
        .find(|e| recovered_generation(e).is_some())
    {
        s.fail(&format!("unexpected recovery during outage: {e:?}"));
    }

    let enabling = s.checkpoint();
    s.mark("issuing the endpoint enable command");
    if let Err(e) = loss.enable() {
        s.fail(&format!("making {endpoint} visible again failed: {e}"));
    }
    let active_at = s.elapsed();
    s.mark(format!(
        "endpoint verified active {} after enable was issued",
        fmt_at(s.elapsed_between("enable to active", enabling.at, active_at))
    ));

    // 3. Search the enable suffix only. Count complete A windows starting
    // after the matching event, with one budget from verified activation.
    let deadline = active_at + RETURN_RECOVER_TIMEOUT;
    let recovered = |e: &Timed<RecoveryEvent>| {
        e.at >= active_at && recovered_generation(e).is_some_and(|g| g >= 1)
    };
    let found = s.expect_events_in_order(
        "a Recovered (generation >= 1) after verified activation",
        enabling.events,
        deadline,
        &[&recovered],
    );
    let event = &s.events()[found[0]];
    let recovered_at = event.at;
    let generation = recovered_generation(event).expect("matched a Recovered event");
    let fresh = s.fresh_checkpoint();
    s.expect_sees(
        "capture sees A after recovery",
        Verdict::A,
        generation,
        fresh.windows,
        deadline,
    );

    // 4. Stop, restore, evidence.
    let evidence = evidence_line(
        TEST,
        &format!(
            "recovered_ms={} generation={generation}",
            s.elapsed_between(
                "recovery after verified activation",
                active_at,
                recovered_at
            )
            .as_millis()
        ),
    );
    stop_players(&mut s, [player]);
    finish(TEST, Some(&mut loss), &mut defaults, &evidence);
}

//! C FFI surface for rsac's multi-source channel composition (ADR-0011).
//!
//! Compiled only when the `compose` cargo feature is enabled (it forwards to
//! `rsac/compose`). The C declarations for this module live in both headers
//! behind `#if defined(RSAC_FEATURE_COMPOSE)` — define that macro when
//! compiling C code against a `librsac_ffi` built with `--features compose`.
//!
//! # Memory ownership
//!
//! The crate-level rules apply unchanged:
//!
//! - `rsac_group_new()` / `rsac_composition_builder_new()` /
//!   `rsac_composition_builder_build()` transfer ownership of the returned
//!   handle to the caller, who must free it with the matching `rsac_*_free()`.
//! - `rsac_composition_builder_add_group()` **consumes the group handle on
//!   success** (`RSAC_OK`): do not use or free it afterwards. On any error —
//!   including a caught panic — the group is untouched and the caller still
//!   owns it (the handle is consumed only after the append has succeeded).
//! - `rsac_composition_builder_build()` **always consumes the builder** (even
//!   on failure — Rust ownership semantics; create a new builder to retry),
//!   matching `rsac_builder_build()`.
//! - The composition **owns its inner captures**: freeing it with
//!   `rsac_composition_free()` stops the compositor engine (joining its
//!   thread), which stops every inner capture. `RsacAudioBuffer` handles
//!   returned by reads own their sample data and may outlive the composition.
//! - Free order: `rsac_composition_stop()` (optional) → drain in-flight reads
//!   → `rsac_composition_free()`. Never free concurrently with a read.
//!
//! # Threading
//!
//! `rsac_composition_stop()` is the **only** call on this surface that may
//! overlap another in-flight call on the same handle (it exists to unblock a
//! parked `rsac_composition_read()` / `rsac_composition_try_read()`, and it is
//! still never safe against `rsac_composition_free()`). Every other pair of
//! calls on **one** handle — composition, builder, or group — must not overlap
//! from multiple threads; the caller provides external synchronization (e.g.
//! `rsac_composition_start()` racing a parked `rsac_composition_read()` is a
//! data race). Distinct handles are independent and need no coordination.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::time::Duration;

use rsac::compose::{Composition, CompositionBuilder, Group, GroupLayout};
use rsac::{CaptureTarget, CapturingStream};

use crate::{catch, handle_rsac_error, rsac_error_t, set_last_error, RsacAudioBuffer};

// ── C-ABI enums and value types ──────────────────────────────────────────

/// How a composition group's sources map onto the composed output channels.
/// Mirrors [`rsac::compose::GroupLayout`].
///
/// This is a **constants-only** type at the ABI boundary:
/// [`rsac_group_set_layout`] takes its `layout` parameter as a plain
/// `int32_t`, never as this enum by value, because materializing an
/// out-of-range integer as a fieldless Rust enum is immediate undefined
/// behavior — before any range check could run (rsac-a273). Out-of-range
/// values are rejected with `RSAC_ERROR_INVALID_PARAMETER` instead.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum rsac_group_layout_t {
    /// Fold every source in the group to mono and gain-weighted-sum them into
    /// **one** output channel.
    RSAC_GROUP_LAYOUT_MONO = 0,
    /// Fold every source to stereo and sum into **two** output channels
    /// (the default layout of a new group).
    RSAC_GROUP_LAYOUT_STEREO = 1,
    /// Pass the group's **single** source through with its native channel
    /// count (a keep-channels group must contain exactly one source).
    RSAC_GROUP_LAYOUT_KEEP_CHANNELS = 2,
}

/// A point-in-time snapshot of a running composition's counters.
///
/// Filled by [`rsac_composition_stats`] from `Composition::stats`. Plain C-ABI
/// value type (no heap, no free required). Before [`rsac_composition_start`]
/// succeeds every field is `0` (the compositor engine does not exist yet).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RsacCompositionStats {
    /// Composed buffers (ticks) emitted so far.
    pub ticks: u64,
    /// Ticks emitted by the wall-clock stall fallback (master had no data).
    pub fallback_ticks: u64,
    /// Number of composed sources, in flat declaration order. `0` before
    /// start. Valid indices for the per-source accessors are
    /// `[0, num_sources)` (half-open).
    pub num_sources: usize,
}

/// A point-in-time snapshot of one composed source's counters.
///
/// Filled by [`rsac_composition_source_stats`]. Plain C-ABI value type (no
/// heap, no free required). The source's group name and target string are
/// available via [`rsac_composition_source_group`] and
/// [`rsac_composition_source_target`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RsacSourceStats {
    /// Buffers received from the inner capture so far.
    pub buffers_received: u64,
    /// Frames of silence inserted because the source was behind at tick time.
    pub padded_frames: u64,
    /// Frames trimmed because the source drifted past the buffering bound.
    pub trimmed_frames: u64,
    /// `1` if this source is being resampled to the session rate, else `0`.
    pub resampling: i32,
    /// `1` if the source's stream has ended, else `0`.
    pub ended: i32,
}

// ── Opaque handle types ──────────────────────────────────────────────────

/// Opaque handle to a `CompositionBuilder`.
pub struct RsacCompositionBuilder {
    inner: CompositionBuilder,
}

/// Opaque handle to a composition `Group` under construction.
pub struct RsacGroup {
    inner: Group,
}

/// Opaque handle to a `Composition` session.
pub struct RsacComposition {
    inner: Composition,
}

// ── Thread-local string storage ──────────────────────────────────────────

thread_local! {
    static COMPOSE_STRING_BUF: RefCell<CString> = RefCell::new(CString::default());
}

/// Stores `s` in the compose thread-local string buffer and returns a pointer
/// valid until the next compose string accessor call on the same thread.
fn compose_string(s: &str) -> *const c_char {
    COMPOSE_STRING_BUF.with(|buf| {
        *buf.borrow_mut() = CString::new(s).unwrap_or_default();
        buf.borrow().as_ptr()
    })
}

/// Reads a required, UTF-8 C string argument. On failure sets the last error
/// and returns the appropriate code via `Err`.
///
/// # Safety
///
/// `p` must be null or a valid NUL-terminated C string pointer.
unsafe fn read_c_str<'a>(p: *const c_char, what: &str) -> Result<&'a str, rsac_error_t> {
    if p.is_null() {
        set_last_error(&format!("{what} is null"));
        return Err(rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }
    match unsafe { CStr::from_ptr(p) }.to_str() {
        Ok(s) => Ok(s),
        Err(_) => {
            set_last_error(&format!("{what} is not valid UTF-8"));
            Err(rsac_error_t::RSAC_ERROR_INVALID_PARAMETER)
        }
    }
}

// ── Group functions ──────────────────────────────────────────────────────

/// Creates a new composition group with the given name and the default
/// stereo layout (`RSAC_GROUP_LAYOUT_STEREO`).
///
/// The name must be unique within a composition and non-empty (both enforced
/// at [`rsac_composition_builder_build`], not here). On success the caller
/// owns the handle and must either hand it to
/// [`rsac_composition_builder_add_group`] (which consumes it) or free it with
/// [`rsac_group_free`].
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `name` or `out` is null, and
/// `RSAC_ERROR_INVALID_PARAMETER` if `name` is not valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn rsac_group_new(
    name: *const c_char,
    out: *mut *mut RsacGroup,
) -> rsac_error_t {
    catch(|| {
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let name_str = match unsafe { read_c_str(name, "name") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        let handle = Box::new(RsacGroup {
            inner: Group::new(name_str),
        });
        unsafe { *out = Box::into_raw(handle) };
        rsac_error_t::RSAC_OK
    })
}

/// Frees a group handle. No-op if null.
///
/// Only call this on a group that was **not** consumed by a successful
/// [`rsac_composition_builder_add_group`].
#[no_mangle]
pub unsafe extern "C" fn rsac_group_free(group: *mut RsacGroup) {
    if !group.is_null() {
        let _ = unsafe { Box::from_raw(group) };
    }
}

/// Sets the group's layout (how its sources map onto output channels).
///
/// `layout` is one of the [`rsac_group_layout_t`] constants, accepted as a
/// plain `int32_t` (C's implicit enum→int conversion keeps call sites
/// source-compatible). Taking the raw integer rather than the enum by value
/// is deliberate: an out-of-range integer materialized as a fieldless Rust
/// enum at the ABI boundary would be undefined behavior before any check
/// could run. Any value other than the defined constants (0, 1, 2) is
/// rejected with `RSAC_ERROR_INVALID_PARAMETER` and the group is unchanged.
///
/// A `RSAC_GROUP_LAYOUT_KEEP_CHANNELS` group must contain exactly one source;
/// that arity is enforced at [`rsac_composition_builder_build`].
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `group` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_group_set_layout(group: *mut RsacGroup, layout: i32) -> rsac_error_t {
    catch(|| {
        if group.is_null() {
            set_last_error("group is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        // Validate the raw integer BEFORE any enum value exists: rejecting an
        // out-of-range `layout` here is only possible because the parameter is
        // an i32 — an invalid fieldless-enum value would already be UB.
        let g = unsafe { &mut *group };
        g.inner = match layout {
            x if x == rsac_group_layout_t::RSAC_GROUP_LAYOUT_MONO as i32 => {
                g.inner.clone().mixdown(GroupLayout::Mono)
            }
            x if x == rsac_group_layout_t::RSAC_GROUP_LAYOUT_STEREO as i32 => {
                g.inner.clone().mixdown(GroupLayout::Stereo)
            }
            x if x == rsac_group_layout_t::RSAC_GROUP_LAYOUT_KEEP_CHANNELS as i32 => {
                g.inner.clone().keep_channels()
            }
            _ => {
                set_last_error(&format!(
                    "layout {layout} is not a valid rsac_group_layout_t (expected 0, 1, or 2)"
                ));
                return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
            }
        };
        rsac_error_t::RSAC_OK
    })
}

/// Adds a capture source to the group with unit gain (1.0).
///
/// `spec` uses the same `CaptureTarget` string grammar as
/// `rsac_builder_set_target_str()` (case-insensitive scheme): `system`,
/// `device:<id>`, `app:<pid-or-id>`, `name:<name>`, or `tree:<pid>`.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `group` or `spec` is null, and
/// `RSAC_ERROR_INVALID_PARAMETER` if `spec` is not valid UTF-8 or not a valid
/// target string (the group is unchanged on error — parse-then-commit).
#[no_mangle]
pub unsafe extern "C" fn rsac_group_add_source(
    group: *mut RsacGroup,
    spec: *const c_char,
) -> rsac_error_t {
    unsafe { rsac_group_add_source_with_gain(group, spec, 1.0) }
}

/// Adds a capture source to the group with an explicit linear gain applied
/// during mixdown (1.0 = unity).
///
/// `spec` follows the same grammar as `rsac_group_add_source()`. The gain must
/// be finite and >= 0; an invalid gain is rejected **eagerly** here with
/// `RSAC_ERROR_INVALID_PARAMETER` (the same values would also be rejected at
/// [`rsac_composition_builder_build`], but failing at the call site gives a
/// precise diagnostic).
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `group` or `spec` is null, and
/// `RSAC_ERROR_INVALID_PARAMETER` for an invalid `spec` or gain (the group is
/// unchanged on error).
#[no_mangle]
pub unsafe extern "C" fn rsac_group_add_source_with_gain(
    group: *mut RsacGroup,
    spec: *const c_char,
    gain: f32,
) -> rsac_error_t {
    catch(|| {
        if group.is_null() {
            set_last_error("group is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let spec_str = match unsafe { read_c_str(spec, "spec") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        if !gain.is_finite() || gain < 0.0 {
            set_last_error(&format!("gain {gain} is invalid (must be finite and >= 0)"));
            return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
        }
        let target = match spec_str.parse::<CaptureTarget>() {
            Ok(t) => t,
            Err(e) => return handle_rsac_error(e),
        };
        let g = unsafe { &mut *group };
        g.inner = g.inner.clone().source_with_gain(target, gain);
        rsac_error_t::RSAC_OK
    })
}

// ── CompositionBuilder functions ─────────────────────────────────────────

/// Creates a new `CompositionBuilder` with default settings (48 kHz session
/// rate, no output clamping, no groups).
///
/// Returns a handle that must be freed with
/// [`rsac_composition_builder_free`] (or consumed by
/// [`rsac_composition_builder_build`]).
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_new(
    out: *mut *mut RsacCompositionBuilder,
) -> rsac_error_t {
    catch(|| {
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let handle = Box::new(RsacCompositionBuilder {
            inner: CompositionBuilder::new(),
        });
        unsafe { *out = Box::into_raw(handle) };
        rsac_error_t::RSAC_OK
    })
}

/// Frees a composition builder handle. No-op if null.
///
/// Only call this on a builder that was **not** consumed by
/// [`rsac_composition_builder_build`].
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_free(builder: *mut RsacCompositionBuilder) {
    if !builder.is_null() {
        let _ = unsafe { Box::from_raw(builder) };
    }
}

/// Sets the session sample rate in Hz (default 48000). Sources delivering a
/// different rate are resampled. An unsupported rate is rejected at
/// [`rsac_composition_builder_build`].
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_set_sample_rate(
    builder: *mut RsacCompositionBuilder,
    sample_rate: u32,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().sample_rate(sample_rate);
        rsac_error_t::RSAC_OK
    })
}

/// Enables (`clamp` nonzero) or disables (`clamp` 0) saturating output
/// clamping to `[-1.0, 1.0]` after summation. Default off: plain summation may
/// exceed unity, which is legal for f32 pipelines.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_set_clamp_output(
    builder: *mut RsacCompositionBuilder,
    clamp: i32,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().clamp_output(clamp != 0);
        rsac_error_t::RSAC_OK
    })
}

/// Sets the composed tick quantum (output buffer duration) in milliseconds.
/// Default 10 ms. Maps to `CompositionBuilder::quantum`.
///
/// The setter is deliberately thin (any `millis` value is accepted here, like
/// `rsac_composition_builder_set_sample_rate`): validation lives in one place
/// — a **zero** quantum is rejected at [`rsac_composition_builder_preflight`]
/// / [`rsac_composition_builder_build`] with `RSAC_ERROR_CONFIGURATION`. At
/// start the quantum is additionally clamped to at least one frame at the
/// session rate.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `builder` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_set_quantum_ms(
    builder: *mut RsacCompositionBuilder,
    millis: u64,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().quantum(Duration::from_millis(millis));
        rsac_error_t::RSAC_OK
    })
}

/// Sets how long the compositor waits for the master-clock source before
/// emitting a wall-clock fallback tick (so a stalled master never freezes the
/// session), in milliseconds. Default 250 ms. Maps to
/// `CompositionBuilder::stall_timeout`.
///
/// Thin like the other setters: a **zero** timeout is rejected at
/// [`rsac_composition_builder_preflight`] /
/// [`rsac_composition_builder_build`] with `RSAC_ERROR_CONFIGURATION`.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `builder` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_set_stall_timeout_ms(
    builder: *mut RsacCompositionBuilder,
    millis: u64,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().stall_timeout(Duration::from_millis(millis));
        rsac_error_t::RSAC_OK
    })
}

/// Sets the per-source buffering bound in milliseconds. A source drifting
/// ahead of the master beyond this bound has its oldest samples trimmed
/// (counted in `RsacSourceStats::trimmed_frames`). Default 1000 ms. Maps to
/// `CompositionBuilder::max_buffer`.
///
/// Any value — including 0 — is accepted and passes validation: the Rust
/// builder clamps the bound to at least one quantum when the composition
/// starts, so there is no rejectable "invalid" bound.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `builder` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_set_max_buffer_ms(
    builder: *mut RsacCompositionBuilder,
    millis: u64,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().max_buffer(Duration::from_millis(millis));
        rsac_error_t::RSAC_OK
    })
}

/// Runs every device-independent validation
/// [`rsac_composition_builder_build`] performs, **without consuming the
/// builder** (maps to `CompositionBuilder::preflight`, which takes `&self`).
///
/// Because `build` always consumes its builder — even on failure — this is
/// how a C caller iterates on a configuration: preflight, fix the reported
/// error on the *same* builder, preflight again, and only then build.
///
/// `RSAC_OK` means build's validation phase would pass. It is **not** a
/// guarantee the composition will start: no devices are touched here, so
/// device/capability errors (device resolution, format negotiation, stream
/// creation) can still surface at [`rsac_composition_start`].
///
/// Error codes mirror `build`'s validation phase exactly:
/// `RSAC_ERROR_CONFIGURATION` (no groups, empty group, duplicate/empty group
/// name, keep-channels group without exactly one source, invalid gain, too
/// many sources or channels, zero quantum or stall timeout),
/// `RSAC_ERROR_INVALID_PARAMETER` (unsupported session sample rate), or
/// `RSAC_ERROR_PLATFORM_NOT_SUPPORTED` (a target this platform cannot
/// capture). Returns `RSAC_ERROR_NULL_POINTER` if `builder` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_preflight(
    builder: *const RsacCompositionBuilder,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &*builder };
        match b.inner.preflight() {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Appends a group to the composition. Groups contribute output channels in
/// the order they are added.
///
/// On success (`RSAC_OK`) **the group handle is consumed**: do not use or
/// free it afterwards. On any error — including a caught panic
/// (`RSAC_ERROR_PANIC`) — the group is untouched and the caller still owns
/// it (and must eventually call [`rsac_group_free`]): the handle is consumed
/// only after the append has fully succeeded.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `builder` or `group` is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_add_group(
    builder: *mut RsacCompositionBuilder,
    group: *mut RsacGroup,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if group.is_null() {
            set_last_error("group is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        // Both handles are non-null. ORDER MATTERS from here (rsac-2932): the
        // documented contract is "on ANY error the caller still owns the
        // group" — and a caught panic surfaces as RSAC_ERROR_PANIC, which is
        // an error like any other. So do every fallible step (the clones and
        // the append, which can panic e.g. on OOM) through borrows FIRST, and
        // consume the handle only once nothing can fail. Consuming it before
        // the fallible work would let a panicking clone drop (free) the group
        // during unwind while the contract tells the caller to free it too —
        // a double free.
        let b = unsafe { &mut *builder };
        let g_ref = unsafe { &*group };
        let updated = b.inner.clone().group(g_ref.inner.clone());
        b.inner = updated;
        // Success path only: reclaiming and dropping the handle is infallible.
        drop(unsafe { Box::from_raw(group) });
        rsac_error_t::RSAC_OK
    })
}

/// Validates the configuration and builds a (not yet started)
/// `Composition`.
///
/// **The builder is always consumed** — on failure too (Rust ownership
/// semantics; create a new builder to retry), matching `rsac_builder_build()`.
/// No devices are touched here; inner captures are created and started by
/// [`rsac_composition_start`]. On success `*out` receives the composition
/// handle, which must be freed with [`rsac_composition_free`]. On failure
/// `*out` is null.
///
/// Validation failures map to `RSAC_ERROR_CONFIGURATION` (no groups, empty
/// group, duplicate/empty group name, keep-channels group without exactly one
/// source, invalid gain, too many sources or channels),
/// `RSAC_ERROR_INVALID_PARAMETER` (unsupported session sample rate), or
/// `RSAC_ERROR_PLATFORM_NOT_SUPPORTED` (a target this platform cannot
/// capture). Call `rsac_error_message()` for details.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_builder_build(
    builder: *mut RsacCompositionBuilder,
    out: *mut *mut RsacComposition,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let b = unsafe { Box::from_raw(builder) };
        match b.inner.build() {
            Ok(composition) => {
                let handle = Box::new(RsacComposition { inner: composition });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

// ── Composition lifecycle ────────────────────────────────────────────────

/// Builds and starts one capture per source, resolves the composed channel
/// layout, and spawns the compositor thread.
///
/// On failure every already-started inner capture is stopped before the error
/// is returned. Starting an already-started composition returns
/// `RSAC_ERROR_CONFIGURATION`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_start(comp: *mut RsacComposition) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &mut *comp };
        match c.inner.start() {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Signals the composition to stop: ends the composed ring and tells the
/// compositor engine to stop every inner capture. Idempotent; a not-started
/// composition returns `RSAC_OK`.
///
/// Takes a **const** composition: like `rsac_capture_request_stop()`, this is
/// safe to call concurrently with an in-flight [`rsac_composition_read`] /
/// [`rsac_composition_try_read`] to unblock it (no `&mut` alias is formed).
/// The compositor thread itself is joined later, by
/// [`rsac_composition_free`]. It is **NOT** safe to call this concurrently
/// with `rsac_composition_free()` — order stop + a drain of in-flight reads
/// **before** freeing the handle.
///
/// An explicit stop discards any buffered composed tail: subsequent reads
/// return the terminal stream error rather than draining. To capture
/// everything, read until the terminal error *before* stopping.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_stop(comp: *const RsacComposition) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*comp };
        // The trait stop (&self) signals ring + engine without joining, so a
        // parked reader thread can be unblocked concurrently. The engine join
        // happens in Drop (rsac_composition_free).
        match CapturingStream::stop(&c.inner) {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Returns 1 if the composed stream is currently running, 0 otherwise.
/// Returns -1 if the composition handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_is_running(comp: *const RsacComposition) -> i32 {
    if comp.is_null() {
        return -1;
    }
    let c = unsafe { &*comp };
    if c.inner.is_running() {
        1
    } else {
        0
    }
}

/// Returns the number of composed-ring overruns: composed buffers dropped
/// because the C consumer read slower than the compositor produced (the ring
/// holds ~128 composed buffers ≈ 1.3 s at the default 10 ms quantum). Mirrors
/// [`rsac_capture_overrun_count`](crate::rsac_capture_overrun_count).
///
/// This counts loss at the **composed** ring only. Loss inside an inner
/// source's own capture is reported per source via
/// `RsacSourceStats` / [`rsac_composition_source_stats`].
///
/// Returns 0 if the composition handle is null or the composition has not
/// been started.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_overrun_count(comp: *const RsacComposition) -> u64 {
    if comp.is_null() {
        return 0;
    }
    let c = unsafe { &*comp };
    c.inner.overrun_count()
}

/// Frees a composition handle. Stops the composition if running (joining the
/// compositor thread, which stops every inner capture). No-op if null.
///
/// The composition owns its inner captures; freeing it releases them all.
/// `RsacAudioBuffer` handles previously returned by reads own their sample
/// data and remain valid after this call.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_free(comp: *mut RsacComposition) {
    if !comp.is_null() {
        // This teardown runs far more logic than the other frees: Composition's
        // Drop stops the engine and JOINS the compositor thread. A panic
        // unwinding out of an `extern "C"` fn is an abort, so guard the drop.
        // The return is void — there is no error code to surface — so swallow
        // and log (the SendCallback convention), plus set the thread-local
        // message for callers that poll rsac_error_message(). Box's drop glue
        // still deallocates the handle's memory when the value's drop panics,
        // so nothing leaks.
        let boxed = unsafe { Box::from_raw(comp) };
        if panic::catch_unwind(AssertUnwindSafe(move || drop(boxed))).is_err() {
            set_last_error("Rust panic caught in rsac_composition_free teardown");
            log::error!(
                "rsac FFI: rsac_composition_free teardown panicked; panic caught at FFI boundary"
            );
        }
    }
}

// ── Reading composed audio ───────────────────────────────────────────────

/// Attempts a non-blocking read of the next composed buffer (mirrors
/// `rsac_capture_try_read()`).
///
/// On success with data available, `*out` receives a buffer handle that must
/// be freed with `rsac_audio_buffer_free()`; the data is interleaved f32 at
/// the session rate with [`rsac_composition_channel_count`] channels. On
/// success with no data available yet, `*out` is set to null and `RSAC_OK` is
/// returned. Once the composition has ended and drained, the read returns the
/// fatal `RSAC_ERROR_STREAM_FAILED` (terminal — do not retry). A composition
/// that has not been started returns `RSAC_ERROR_STREAM_READ`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_try_read(
    comp: *const RsacComposition,
    out: *mut *mut RsacAudioBuffer,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let c = unsafe { &*comp };
        // Composition::read_chunk_nonblocking is the terminal-observable
        // non-blocking read (renamed from `read_buffer` pre-release to mirror
        // AudioCapture's terminal-observable sibling): the drained end
        // surfaces as the fatal StreamEnded → RSAC_ERROR_STREAM_FAILED, so C
        // pumps end cleanly (same rationale as rsac_capture_try_read).
        match c.inner.read_chunk_nonblocking() {
            Ok(Some(buf)) => {
                let handle = Box::new(RsacAudioBuffer { inner: buf });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Ok(None) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Reads the next composed buffer, blocking until data is available (mirrors
/// `rsac_capture_read()`).
///
/// On success, `*out` receives a buffer handle that must be freed with
/// `rsac_audio_buffer_free()`. Once the composition ends and drains, this
/// returns the fatal `RSAC_ERROR_STREAM_FAILED`. A concurrent
/// [`rsac_composition_stop`] unblocks a thread parked here.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_read(
    comp: *const RsacComposition,
    out: *mut *mut RsacAudioBuffer,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let c = unsafe { &*comp };
        // Composition::read_chunk_blocking is the terminal-observable blocking
        // read (renamed from `read_buffer_blocking` pre-release — see
        // rsac_composition_try_read).
        match c.inner.read_chunk_blocking() {
            Ok(buf) => {
                let handle = Box::new(RsacAudioBuffer { inner: buf });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

// ── Channel-map introspection ────────────────────────────────────────────

/// Returns the number of composed output channels.
///
/// The layout is resolved by [`rsac_composition_start`] (keep-channels widths
/// depend on the source's negotiated format), so this returns 0 before a
/// successful start — and also 0 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_channel_count(comp: *const RsacComposition) -> u16 {
    if comp.is_null() {
        return 0;
    }
    let c = unsafe { &*comp };
    c.inner.channel_map().map(|m| m.channels()).unwrap_or(0)
}

/// Returns the name of the group that produces composed output channel
/// `channel` (0-based).
///
/// The returned pointer is valid until the next compose string accessor call
/// ([`rsac_composition_channel_group`], [`rsac_composition_source_group`], or
/// [`rsac_composition_source_target`]) on the same thread. Returns null if the
/// handle is null, the composition has not been started, or `channel` is out
/// of bounds.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_channel_group(
    comp: *const RsacComposition,
    channel: usize,
) -> *const c_char {
    if comp.is_null() {
        return ptr::null();
    }
    let c = unsafe { &*comp };
    let Some(map) = c.inner.channel_map() else {
        return ptr::null();
    };
    let Some(entry) = map.entries().get(channel) else {
        return ptr::null();
    };
    compose_string(&entry.group)
}

/// Returns the index of composed output channel `channel` *within* its group
/// (0-based; e.g. 0 = L, 1 = R for a stereo group).
///
/// Returns -1 if the handle is null, the composition has not been started, or
/// `channel` is out of bounds.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_channel_in_group(
    comp: *const RsacComposition,
    channel: usize,
) -> i32 {
    if comp.is_null() {
        return -1;
    }
    let c = unsafe { &*comp };
    let Some(map) = c.inner.channel_map() else {
        return -1;
    };
    match map.entries().get(channel) {
        Some(entry) => i32::from(entry.channel_in_group),
        None => -1,
    }
}

// ── Stats ────────────────────────────────────────────────────────────────

/// Fills `*out` with a point-in-time [`RsacCompositionStats`] snapshot.
///
/// Before [`rsac_composition_start`] succeeds the snapshot is all-zero
/// (`num_sources == 0`) — the compositor engine does not exist yet. `out` is
/// an out-parameter, not a handle: there is nothing to free.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp` or `out` is null; otherwise
/// `RSAC_OK`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_stats(
    comp: *const RsacComposition,
    out: *mut RsacCompositionStats,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*comp };
        let c_stats = match c.inner.stats() {
            Some(stats) => RsacCompositionStats {
                ticks: stats.ticks,
                fallback_ticks: stats.fallback_ticks,
                num_sources: stats.sources.len(),
            },
            None => RsacCompositionStats {
                ticks: 0,
                fallback_ticks: 0,
                num_sources: 0,
            },
        };
        unsafe { *out = c_stats };
        rsac_error_t::RSAC_OK
    })
}

/// Fills `*out` with the [`RsacSourceStats`] counters of the source at
/// `index` (flat declaration order across all groups; valid indices are
/// `[0, num_sources)` (half-open) from [`rsac_composition_stats`]).
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp` or `out` is null,
/// `RSAC_ERROR_STREAM_READ` if the composition has not been started (per-source
/// counters exist only while the engine does), and
/// `RSAC_ERROR_INVALID_PARAMETER` if `index` is out of bounds. `out` is an
/// out-parameter, not a handle: there is nothing to free.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_source_stats(
    comp: *const RsacComposition,
    index: usize,
    out: *mut RsacSourceStats,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*comp };
        let Some(stats) = c.inner.stats() else {
            set_last_error("Composition is not started. Call rsac_composition_start() first.");
            return rsac_error_t::RSAC_ERROR_STREAM_READ;
        };
        let Some(source) = stats.sources.get(index) else {
            set_last_error(&format!(
                "index {} out of bounds (composition has {} sources)",
                index,
                stats.sources.len()
            ));
            return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
        };
        let c_stats = RsacSourceStats {
            buffers_received: source.buffers_received,
            padded_frames: source.padded_frames,
            trimmed_frames: source.trimmed_frames,
            resampling: i32::from(source.resampling),
            ended: i32::from(source.ended),
        };
        unsafe { *out = c_stats };
        rsac_error_t::RSAC_OK
    })
}

/// Returns the group name of the source at `index` (flat declaration order).
///
/// The returned pointer is valid until the next compose string accessor call
/// on the same thread. Returns null if the handle is null, the composition has
/// not been started, or `index` is out of bounds.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_source_group(
    comp: *const RsacComposition,
    index: usize,
) -> *const c_char {
    if comp.is_null() {
        return ptr::null();
    }
    let c = unsafe { &*comp };
    let Some(stats) = c.inner.stats() else {
        return ptr::null();
    };
    match stats.sources.get(index) {
        Some(source) => compose_string(&source.group),
        None => ptr::null(),
    }
}

/// Returns the capture-target string of the source at `index` (flat
/// declaration order), rendered in the canonical `CaptureTarget` grammar
/// (e.g. `"system"`, `"name:discord"`).
///
/// The returned pointer is valid until the next compose string accessor call
/// on the same thread. Returns null if the handle is null, the composition has
/// not been started, or `index` is out of bounds.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_source_target(
    comp: *const RsacComposition,
    index: usize,
) -> *const c_char {
    if comp.is_null() {
        return ptr::null();
    }
    let c = unsafe { &*comp };
    let Some(stats) = c.inner.stats() else {
        return ptr::null();
    };
    match stats.sources.get(index) {
        Some(source) => compose_string(&source.target),
        None => ptr::null(),
    }
}

// ── Live per-source gain / mute (rsac-5a2d) ──────────────────────────────

/// Sets a source's live mix gain on a **running** composition (rsac-5a2d).
///
/// The source is addressed by its group `name` (a NUL-terminated UTF-8 string)
/// plus its `source_idx` **within that group** (0-based, declaration order —
/// this is *not* the flat cross-group index the `_source_*` accessors take).
/// `gain` must be finite and ≥ 0; it is validated by the core (after any f32
/// narrowing the caller performed), not eagerly here.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp` or `group` is null,
/// `RSAC_ERROR_INVALID_PARAMETER` if `group` is not valid UTF-8,
/// `RSAC_ERROR_STREAM_READ` if the composition has not been started or has
/// stopped/ended (no tick would apply the change), and
/// `RSAC_ERROR_CONFIGURATION` for an unknown group, an out-of-range
/// `source_idx`, or an invalid `gain`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_set_gain(
    comp: *const RsacComposition,
    group: *const c_char,
    source_idx: usize,
    gain: f32,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let group_str = match unsafe { read_c_str(group, "group") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        let c = unsafe { &*comp };
        match c.inner.set_gain(group_str, source_idx, gain) {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Mutes (`muted` nonzero) or unmutes (`muted` 0) a source on a **running**
/// composition (rsac-5a2d). Muting is a separate flag from gain: while muted the
/// source contributes silence; unmuting restores its gain untouched. Addressed
/// exactly like [`rsac_composition_set_gain`].
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp` or `group` is null,
/// `RSAC_ERROR_INVALID_PARAMETER` if `group` is not valid UTF-8,
/// `RSAC_ERROR_STREAM_READ` before start / after stop or end, and
/// `RSAC_ERROR_CONFIGURATION` for an unknown group or out-of-range `source_idx`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_set_muted(
    comp: *const RsacComposition,
    group: *const c_char,
    source_idx: usize,
    muted: i32,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let group_str = match unsafe { read_c_str(group, "group") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        let c = unsafe { &*comp };
        match c.inner.set_muted(group_str, source_idx, muted != 0) {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Reads back a source's current effective mix gain into `*out_gain`
/// (rsac-5a2d). Same addressing as [`rsac_composition_set_gain`]. Unlike the
/// setter this **keeps working on a stopped or ended composition** — it only
/// fails before the first successful start.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp`, `group`, or `out_gain` is null,
/// `RSAC_ERROR_INVALID_PARAMETER` if `group` is not valid UTF-8,
/// `RSAC_ERROR_STREAM_READ` only if the composition has never been started, and
/// `RSAC_ERROR_CONFIGURATION` for an unknown group or out-of-range `source_idx`.
/// `out_gain` is an out-parameter, not a handle — nothing to free; it is written
/// only on `RSAC_OK`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_gain(
    comp: *const RsacComposition,
    group: *const c_char,
    source_idx: usize,
    out_gain: *mut f32,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out_gain.is_null() {
            set_last_error("out_gain pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let group_str = match unsafe { read_c_str(group, "group") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        let c = unsafe { &*comp };
        match c.inner.gain(group_str, source_idx) {
            Ok(g) => {
                unsafe { *out_gain = g };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Reads back whether a source is currently muted into `*out_muted` (0/1)
/// (rsac-5a2d). Same addressing and stopped-composition behavior as
/// [`rsac_composition_gain`].
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `comp`, `group`, or `out_muted` is null,
/// `RSAC_ERROR_INVALID_PARAMETER` if `group` is not valid UTF-8,
/// `RSAC_ERROR_STREAM_READ` only if the composition has never been started, and
/// `RSAC_ERROR_CONFIGURATION` for an unknown group or out-of-range `source_idx`.
/// `out_muted` is an out-parameter, not a handle — nothing to free; it is
/// written only on `RSAC_OK`.
#[no_mangle]
pub unsafe extern "C" fn rsac_composition_is_muted(
    comp: *const RsacComposition,
    group: *const c_char,
    source_idx: usize,
    out_muted: *mut i32,
) -> rsac_error_t {
    catch(|| {
        if comp.is_null() {
            set_last_error("composition is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out_muted.is_null() {
            set_last_error("out_muted pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let group_str = match unsafe { read_c_str(group, "group") } {
            Ok(s) => s,
            Err(code) => return code,
        };
        let c = unsafe { &*comp };
        match c.inner.is_muted(group_str, source_idx) {
            Ok(m) => {
                unsafe { *out_muted = i32::from(m) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    /// Builds a `*mut RsacGroup` with the given name via the FFI, panicking on
    /// failure. The caller owns the handle.
    fn new_group(name: &str) -> *mut RsacGroup {
        let name_c = CString::new(name).unwrap();
        let mut group: *mut RsacGroup = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_group_new(name_c.as_ptr(), &mut group) },
            rsac_error_t::RSAC_OK
        );
        assert!(!group.is_null());
        group
    }

    /// Builds a `*mut RsacCompositionBuilder` via the FFI.
    fn new_builder() -> *mut RsacCompositionBuilder {
        let mut builder: *mut RsacCompositionBuilder = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_new(&mut builder) },
            rsac_error_t::RSAC_OK
        );
        assert!(!builder.is_null());
        builder
    }

    /// Adds a "system" source to `group`, panicking on failure.
    fn add_system_source(group: *mut RsacGroup) {
        let spec = CString::new("system").unwrap();
        assert_eq!(
            unsafe { rsac_group_add_source(group, spec.as_ptr()) },
            rsac_error_t::RSAC_OK
        );
    }

    // ── Null-pointer contract for every new fn ─────────────────────────

    #[test]
    fn group_fns_reject_null() {
        let name = CString::new("g").unwrap();
        let mut out: *mut RsacGroup = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_group_new(ptr::null(), &mut out) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_group_new(name.as_ptr(), ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe {
                rsac_group_set_layout(
                    ptr::null_mut(),
                    rsac_group_layout_t::RSAC_GROUP_LAYOUT_MONO as i32,
                )
            },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        let spec = CString::new("system").unwrap();
        assert_eq!(
            unsafe { rsac_group_add_source(ptr::null_mut(), spec.as_ptr()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_group_add_source_with_gain(ptr::null_mut(), spec.as_ptr(), 1.0) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        // Null spec: dangling-but-non-null group, never dereferenced (the
        // null-spec check returns first — matches the lib.rs test pattern).
        let dangling = ptr::dangling_mut::<RsacGroup>();
        assert_eq!(
            unsafe { rsac_group_add_source(dangling, ptr::null()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        // Free of null is a no-op (must not crash).
        unsafe { rsac_group_free(ptr::null_mut()) };
    }

    #[test]
    fn builder_fns_reject_null() {
        assert_eq!(
            unsafe { rsac_composition_builder_new(ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_sample_rate(ptr::null_mut(), 48_000) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_clamp_output(ptr::null_mut(), 1) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        // add_group: null builder is checked first (dangling group is never
        // touched, so it is not consumed and needs no free).
        let dangling_group = ptr::dangling_mut::<RsacGroup>();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(ptr::null_mut(), dangling_group) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        let dangling_builder = ptr::dangling_mut::<RsacCompositionBuilder>();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(dangling_builder, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        // build: both null checks precede consumption, so a dangling builder
        // with a null out is rejected without being freed.
        let mut out: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(ptr::null_mut(), &mut out) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_build(dangling_builder, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        unsafe { rsac_composition_builder_free(ptr::null_mut()) };
    }

    #[test]
    fn composition_fns_reject_null() {
        assert_eq!(
            unsafe { rsac_composition_start(ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_stop(ptr::null()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(unsafe { rsac_composition_is_running(ptr::null()) }, -1);
        assert_eq!(unsafe { rsac_composition_channel_count(ptr::null()) }, 0);
        assert!(unsafe { rsac_composition_channel_group(ptr::null(), 0) }.is_null());
        assert_eq!(
            unsafe { rsac_composition_channel_in_group(ptr::null(), 0) },
            -1
        );
        assert!(unsafe { rsac_composition_source_group(ptr::null(), 0) }.is_null());
        assert!(unsafe { rsac_composition_source_target(ptr::null(), 0) }.is_null());

        let mut buf_out: *mut RsacAudioBuffer = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_try_read(ptr::null(), &mut buf_out) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_read(ptr::null(), &mut buf_out) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        // Null `out`: dangling-but-non-null composition, never dereferenced
        // (the null-out check returns first).
        let dangling = ptr::dangling::<RsacComposition>();
        assert_eq!(
            unsafe { rsac_composition_try_read(dangling, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_read(dangling, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );

        let mut stats = MaybeUninit::<RsacCompositionStats>::uninit();
        assert_eq!(
            unsafe { rsac_composition_stats(ptr::null(), stats.as_mut_ptr()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_stats(dangling, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        let mut src_stats = MaybeUninit::<RsacSourceStats>::uninit();
        assert_eq!(
            unsafe { rsac_composition_source_stats(ptr::null(), 0, src_stats.as_mut_ptr()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_source_stats(dangling, 0, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );

        // Live per-source gain/mute (rsac-5a2d): null comp is rejected for all
        // four; the getters additionally reject a null out-param with a
        // dangling-but-non-null comp (the out-param check returns first).
        let group = CString::new("g").unwrap();
        assert_eq!(
            unsafe { rsac_composition_set_gain(ptr::null(), group.as_ptr(), 0, 1.0) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_set_muted(ptr::null(), group.as_ptr(), 0, 1) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        let mut out_gain: f32 = 0.0;
        assert_eq!(
            unsafe { rsac_composition_gain(ptr::null(), group.as_ptr(), 0, &mut out_gain) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_gain(dangling, group.as_ptr(), 0, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        let mut out_muted: i32 = 0;
        assert_eq!(
            unsafe { rsac_composition_is_muted(ptr::null(), group.as_ptr(), 0, &mut out_muted) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_is_muted(dangling, group.as_ptr(), 0, ptr::null_mut()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );

        unsafe { rsac_composition_free(ptr::null_mut()) };
    }

    /// Live per-source gain/mute on a built-but-not-started composition: every
    /// call goes through the composition's not-started guard and surfaces the
    /// recoverable `RSAC_ERROR_STREAM_READ` (the runtime roundtrip, bounds, and
    /// not-running paths need a started composition — device-gated, see §6).
    #[test]
    fn live_control_before_start_is_stream_read() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut comp: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut comp) },
            rsac_error_t::RSAC_OK
        );
        assert!(!comp.is_null());

        let g = CString::new("main").unwrap();
        assert_eq!(
            unsafe { rsac_composition_set_gain(comp, g.as_ptr(), 0, 0.5) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );
        assert_eq!(
            unsafe { rsac_composition_set_muted(comp, g.as_ptr(), 0, 1) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );
        let mut out_gain: f32 = -1.0;
        assert_eq!(
            unsafe { rsac_composition_gain(comp, g.as_ptr(), 0, &mut out_gain) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );
        let mut out_muted: i32 = -1;
        assert_eq!(
            unsafe { rsac_composition_is_muted(comp, g.as_ptr(), 0, &mut out_muted) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );
        // The last error message is populated and non-empty.
        let msg = unsafe { CStr::from_ptr(crate::rsac_error_message()) };
        assert!(!msg.to_bytes().is_empty());

        unsafe { rsac_composition_free(comp) };
    }

    // ── Builder validation error paths through the FFI ─────────────────

    #[test]
    fn empty_builder_build_is_configuration_error() {
        let builder = new_builder();
        let mut out: *mut RsacComposition = ptr::null_mut();
        let code = unsafe { rsac_composition_builder_build(builder, &mut out) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_CONFIGURATION);
        assert!(out.is_null());
        // The builder was consumed by build — do NOT free it here.
        let msg = unsafe { CStr::from_ptr(crate::rsac_error_message()) };
        assert!(!msg.to_bytes().is_empty());
    }

    #[test]
    fn keep_channels_with_two_sources_rejected_at_build() {
        // The keep-channels arity check fires before any per-source platform
        // capability validation, so this is deterministic on every platform.
        let group = new_group("keep");
        add_system_source(group);
        add_system_source(group);
        assert_eq!(
            unsafe {
                rsac_group_set_layout(
                    group,
                    rsac_group_layout_t::RSAC_GROUP_LAYOUT_KEEP_CHANNELS as i32,
                )
            },
            rsac_error_t::RSAC_OK
        );
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        // group is consumed now.
        let mut out: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut out) },
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        );
        assert!(out.is_null());
    }

    #[test]
    fn invalid_gain_rejected_eagerly() {
        let group = new_group("g");
        let spec = CString::new("system").unwrap();
        for bad in [-0.5f32, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                unsafe { rsac_group_add_source_with_gain(group, spec.as_ptr(), bad) },
                rsac_error_t::RSAC_ERROR_INVALID_PARAMETER,
                "gain {bad} must be rejected"
            );
        }
        // A valid gain still works on the same (unchanged) group.
        assert_eq!(
            unsafe { rsac_group_add_source_with_gain(group, spec.as_ptr(), 0.8) },
            rsac_error_t::RSAC_OK
        );
        unsafe { rsac_group_free(group) };
    }

    #[test]
    fn bad_target_spec_rejected() {
        let group = new_group("g");
        // Unknown scheme → parse error from CaptureTarget::from_str.
        let garbage = CString::new("not-a-real-scheme:whatever").unwrap();
        assert_eq!(
            unsafe { rsac_group_add_source(group, garbage.as_ptr()) },
            rsac_error_t::RSAC_ERROR_INVALID_PARAMETER
        );
        // Invalid UTF-8 (0xFF is never valid) → INVALID_PARAMETER before parse.
        let bad_utf8 = CString::new(vec![0xFFu8, 0xFEu8]).unwrap();
        assert_eq!(
            unsafe { rsac_group_add_source(group, bad_utf8.as_ptr()) },
            rsac_error_t::RSAC_ERROR_INVALID_PARAMETER
        );
        unsafe { rsac_group_free(group) };
    }

    #[test]
    fn empty_group_name_rejected_at_build() {
        // Empty names are accepted at rsac_group_new (matching Group::new) and
        // rejected by build's preflight — before any capability check.
        let group = new_group("");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut out: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut out) },
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        );
        assert!(out.is_null());
    }

    #[test]
    fn duplicate_group_names_rejected_at_build() {
        let builder = new_builder();
        for _ in 0..2 {
            let group = new_group("dup");
            add_system_source(group);
            assert_eq!(
                unsafe { rsac_composition_builder_add_group(builder, group) },
                rsac_error_t::RSAC_OK
            );
        }
        let mut out: *mut RsacComposition = ptr::null_mut();
        let code = unsafe { rsac_composition_builder_build(builder, &mut out) };
        assert!(out.is_null());
        // Deterministic on every platform: "system" sources always pass the
        // capture builder's preflight (only app/tree targets are capability-
        // gated there), so the duplicate-name check is always reached.
        assert_eq!(code, rsac_error_t::RSAC_ERROR_CONFIGURATION);
    }

    #[test]
    fn unsupported_sample_rate_rejected_at_build() {
        // The session-rate whitelist check precedes the group loop, so this is
        // deterministic on every platform.
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_set_sample_rate(builder, 12_345) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut out: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut out) },
            rsac_error_t::RSAC_ERROR_INVALID_PARAMETER
        );
        assert!(out.is_null());
    }

    // ── Happy path + not-started contract (device-free) ────────────────

    #[test]
    fn minimal_composition_builds_and_reports_not_started_state() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_set_clamp_output(builder, 1) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut comp: *mut RsacComposition = ptr::null_mut();
        let code = unsafe { rsac_composition_builder_build(builder, &mut comp) };

        // Building touches no devices and a "system" target always passes the
        // capture builder's preflight (only app/tree targets are capability-
        // gated), so this succeeds on every platform; device errors would
        // surface at rsac_composition_start().
        assert_eq!(code, rsac_error_t::RSAC_OK);
        assert!(!comp.is_null());

        // Not-started contract: no layout, no engine, no stream.
        assert_eq!(unsafe { rsac_composition_is_running(comp) }, 0);
        assert_eq!(unsafe { rsac_composition_channel_count(comp) }, 0);
        assert!(unsafe { rsac_composition_channel_group(comp, 0) }.is_null());
        assert_eq!(unsafe { rsac_composition_channel_in_group(comp, 0) }, -1);
        assert!(unsafe { rsac_composition_source_group(comp, 0) }.is_null());
        assert!(unsafe { rsac_composition_source_target(comp, 0) }.is_null());

        let mut stats = MaybeUninit::<RsacCompositionStats>::uninit();
        assert_eq!(
            unsafe { rsac_composition_stats(comp, stats.as_mut_ptr()) },
            rsac_error_t::RSAC_OK
        );
        let stats = unsafe { stats.assume_init() };
        assert_eq!(stats.ticks, 0);
        assert_eq!(stats.fallback_ticks, 0);
        assert_eq!(stats.num_sources, 0);

        let mut src_stats = MaybeUninit::<RsacSourceStats>::uninit();
        assert_eq!(
            unsafe { rsac_composition_source_stats(comp, 0, src_stats.as_mut_ptr()) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );

        // Reads on a not-started composition are recoverable STREAM_READ.
        let mut buf: *mut RsacAudioBuffer = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_try_read(comp, &mut buf) },
            rsac_error_t::RSAC_ERROR_STREAM_READ
        );
        assert!(buf.is_null());

        // Stop before start is an idempotent no-op.
        assert_eq!(
            unsafe { rsac_composition_stop(comp) },
            rsac_error_t::RSAC_OK
        );
        unsafe { rsac_composition_free(comp) };
    }

    #[test]
    fn group_layout_setter_accepts_every_variant() {
        let group = new_group("layouts");
        for layout in [
            rsac_group_layout_t::RSAC_GROUP_LAYOUT_MONO,
            rsac_group_layout_t::RSAC_GROUP_LAYOUT_STEREO,
            rsac_group_layout_t::RSAC_GROUP_LAYOUT_KEEP_CHANNELS,
        ] {
            assert_eq!(
                unsafe { rsac_group_set_layout(group, layout as i32) },
                rsac_error_t::RSAC_OK
            );
        }
        unsafe { rsac_group_free(group) };
    }

    /// An out-of-range layout integer must be rejected with
    /// `INVALID_PARAMETER` — never materialized as a Rust enum (rsac-a273) —
    /// and must leave the group unchanged (subsequent valid ops still work).
    #[test]
    fn group_set_layout_rejects_out_of_range() {
        let group = new_group("g");
        for bad in [-1, 3, 99, i32::MIN, i32::MAX] {
            assert_eq!(
                unsafe { rsac_group_set_layout(group, bad) },
                rsac_error_t::RSAC_ERROR_INVALID_PARAMETER,
                "layout {bad} must be rejected"
            );
            let msg = unsafe { CStr::from_ptr(crate::rsac_error_message()) };
            assert!(!msg.to_bytes().is_empty());
        }
        // The rejections left the group untouched: valid ops still succeed.
        assert_eq!(
            unsafe {
                rsac_group_set_layout(group, rsac_group_layout_t::RSAC_GROUP_LAYOUT_MONO as i32)
            },
            rsac_error_t::RSAC_OK
        );
        add_system_source(group);
        unsafe { rsac_group_free(group) };
    }

    // ── rsac-789f: builder knobs, preflight, overrun count ─────────────

    #[test]
    fn knob_and_preflight_fns_reject_null() {
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(ptr::null_mut(), 10) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_stall_timeout_ms(ptr::null_mut(), 250) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_max_buffer_ms(ptr::null_mut(), 1_000) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(ptr::null()) },
            rsac_error_t::RSAC_ERROR_NULL_POINTER
        );
    }

    #[test]
    fn overrun_count_null_and_fresh_composition_are_zero() {
        // Null handle: 0, mirroring rsac_capture_overrun_count.
        assert_eq!(unsafe { rsac_composition_overrun_count(ptr::null()) }, 0);

        // A built-but-not-started composition has no composed ring yet: 0.
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut comp: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut comp) },
            rsac_error_t::RSAC_OK
        );
        assert!(!comp.is_null());
        assert_eq!(unsafe { rsac_composition_overrun_count(comp) }, 0);
        unsafe { rsac_composition_free(comp) };
    }

    /// Preflight must not consume the builder: the C workflow it enables is
    /// preflight → fix the reported error on the SAME builder → preflight →
    /// build (build consumes even on failure, so without preflight every
    /// validation retry means rebuilding the whole configuration).
    #[test]
    fn preflight_is_non_consuming_and_repeatable() {
        let builder = new_builder();
        // No groups yet → CONFIGURATION, twice (repeatable, non-consuming).
        for _ in 0..2 {
            assert_eq!(
                unsafe { rsac_composition_builder_preflight(builder) },
                rsac_error_t::RSAC_ERROR_CONFIGURATION
            );
            let msg = unsafe { CStr::from_ptr(crate::rsac_error_message()) };
            assert!(!msg.to_bytes().is_empty());
        }
        // Fix the error on the same builder…
        let group = new_group("main");
        add_system_source(group);
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        // …and preflight now passes, repeatedly.
        for _ in 0..2 {
            assert_eq!(
                unsafe { rsac_composition_builder_preflight(builder) },
                rsac_error_t::RSAC_OK
            );
        }
        // The builder is still alive and buildable after every preflight.
        let mut comp: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut comp) },
            rsac_error_t::RSAC_OK
        );
        assert!(!comp.is_null());
        unsafe { rsac_composition_free(comp) };
    }

    /// Preflight surfaces the same code build would — pinned on the
    /// session-rate whitelist check, which precedes every capability gate and
    /// is deterministic on all platforms (same rationale as
    /// `unsupported_sample_rate_rejected_at_build`).
    #[test]
    fn preflight_reports_unsupported_sample_rate_and_recovers() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_sample_rate(builder, 12_345) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_ERROR_INVALID_PARAMETER
        );
        // Fix without rebuilding: same builder, corrected rate.
        assert_eq!(
            unsafe { rsac_composition_builder_set_sample_rate(builder, 48_000) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_OK
        );
        unsafe { rsac_composition_builder_free(builder) };
    }

    /// Pins where millis validation lives: the setters are thin (any u64 is
    /// accepted, RSAC_OK), zero quantum/stall_timeout are rejected by
    /// preflight/build as CONFIGURATION, and max_buffer has NO rejectable
    /// value (zero is clamped to one quantum at start).
    #[test]
    fn zero_millis_validation_placement() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );

        // Zero quantum: setter accepts, preflight rejects.
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(builder, 0) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(builder, 10) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_OK
        );

        // Zero stall timeout: setter accepts, preflight rejects.
        assert_eq!(
            unsafe { rsac_composition_builder_set_stall_timeout_ms(builder, 0) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_stall_timeout_ms(builder, 250) },
            rsac_error_t::RSAC_OK
        );

        // Zero max_buffer: no validation error anywhere (clamped at start).
        assert_eq!(
            unsafe { rsac_composition_builder_set_max_buffer_ms(builder, 0) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_OK
        );

        // Extreme values: the Rust builder defines no upper bound, so u64::MAX
        // passes the setters AND preflight (only zero is validated).
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(builder, u64::MAX) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_stall_timeout_ms(builder, u64::MAX) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_max_buffer_ms(builder, u64::MAX) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_preflight(builder) },
            rsac_error_t::RSAC_OK
        );
        unsafe { rsac_composition_builder_free(builder) };
    }

    /// The zero-quantum rejection also fires at build (preflight and build
    /// share one validation), and build still consumes the builder on failure.
    #[test]
    fn zero_quantum_rejected_at_build_too() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(builder, 0) },
            rsac_error_t::RSAC_OK
        );
        let mut out: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut out) },
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        );
        assert!(out.is_null());
        // The builder was consumed by build — do NOT free it here.
    }

    /// All three knobs set to sane values still build (the happy path).
    #[test]
    fn knobs_with_sane_values_build_ok() {
        let group = new_group("main");
        add_system_source(group);
        let builder = new_builder();
        assert_eq!(
            unsafe { rsac_composition_builder_set_quantum_ms(builder, 5) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_stall_timeout_ms(builder, 100) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_set_max_buffer_ms(builder, 500) },
            rsac_error_t::RSAC_OK
        );
        assert_eq!(
            unsafe { rsac_composition_builder_add_group(builder, group) },
            rsac_error_t::RSAC_OK
        );
        let mut comp: *mut RsacComposition = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_composition_builder_build(builder, &mut comp) },
            rsac_error_t::RSAC_OK
        );
        assert!(!comp.is_null());
        unsafe { rsac_composition_free(comp) };
    }

    /// The group NAME path through read_c_str must reject non-UTF-8 exactly
    /// like the *spec* path (`bad_target_spec_rejected` covers that one).
    #[test]
    fn group_new_rejects_non_utf8_name() {
        // 0xFF/0xFE are never valid UTF-8 (and contain no NUL, so CString
        // construction succeeds — the rejection is rsac_group_new's).
        let bad_utf8 = CString::new(vec![0xFFu8, 0xFEu8]).unwrap();
        let mut out: *mut RsacGroup = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_group_new(bad_utf8.as_ptr(), &mut out) },
            rsac_error_t::RSAC_ERROR_INVALID_PARAMETER
        );
        assert!(out.is_null());
        let msg = unsafe { CStr::from_ptr(crate::rsac_error_message()) };
        assert!(!msg.to_bytes().is_empty());
    }
}

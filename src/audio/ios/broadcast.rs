//! ReplayKit broadcast → host: the App-Group mmap-ring **consumer** (rsac-b3aa).
//!
//! This module wires [`CaptureTarget::SystemDefault`] on iOS to the ReplayKit
//! Broadcast Upload Extension transport decided in ADR-0013: the extension
//! (producer, `mobile/ios/Sources/RsacBroadcastKit/RingProducer.swift`) writes
//! interleaved f32 frames into a memory-mapped SPSC ring in the shared App
//! Group container; this module is the host-side consumer that drains the ring
//! into a normal [`BridgeProducer`], so everything downstream is the standard
//! `BridgeStream` pipeline (terminal semantics per ADR-0010/ADR-0003, overrun
//! accounting per ADR-0007).
//!
//! # THE contract
//!
//! `mobile/ios/Sources/RsacBroadcastKit/RingLayout.swift` is the **canonical**
//! cross-process ring contract (layout v1). The constants and cursor/ordering
//! semantics below mirror it byte-for-byte. Any layout change is a breaking
//! cross-process ABI change: bump the version **on both sides in lockstep**
//! and make both sides reject a mismatch (this side does, see
//! [`classify_publish_word`]).
//!
//! # Consumer shape
//!
//! ```text
//! Broadcast extension process              Host app process
//! ──────────────────────────               ─────────────────────────────
//! RPBroadcastSampleHandler                 create_stream():
//!   → RsacRingProducer::write                bounded poll for header publish
//!        │  (mmap ring, App Group)           → read geometry → set_negotiated_format
//!        ▼                                   → spawn NON-RT drain thread
//!   release-store writeCursor                     │ acquire-load writeCursor
//!                                                 │ copy frames → BridgeProducer
//!                                                 │ release-store readCursor
//!                                                 ▼
//!                                           BridgeStream<BroadcastPlatformStream>
//! ```
//!
//! # Consumer obligations (documented loudly, per ADR-0013)
//!
//! - The **host app** must be entitled to the App Group
//!   (`com.apple.security.application-groups`) and pass its identifier via
//!   [`AudioCaptureBuilder::with_ios_app_group`](crate::api::AudioCaptureBuilder::with_ios_app_group).
//! - The **consumer app** must embed a Broadcast Upload Extension built on
//!   `RsacBroadcastKit` (see `mobile/ios/README.md`), entitled to the same
//!   App Group.
//! - Capture is **user-initiated only** (control-center picker /
//!   `RPSystemBroadcastPickerView`); there is no programmatic start. Stream
//!   creation polls a bounded window ([`PUBLISH_POLL_TIMEOUT`]) for the ring
//!   header to be published, then fails with actionable guidance.
//! - The broadcast captures **everything** (no per-app filter) — per-app
//!   capture remains permanently unsupported on iOS (ADR-0013).
//!
//! # Terminal semantics (ADR-0010 / ADR-0003)
//!
//! A heartbeat stamp older than [`HEARTBEAT_TIMEOUT_MILLIS`] means the
//! extension was killed or the broadcast ended ⇒ the drain thread empties the
//! ring, then `signal_error()` — the fatal producer terminal, observed by
//! readers as [`AudioError::StreamEnded`] *after* any bridge-buffered tail is
//! drained (`pop_blocking` pops data before the terminal check). Host-initiated
//! `stop()` is the graceful path: `signal_done()` (`Running → Stopping`, tail
//! stays drainable).
//!
//! # Contract note: no Darwin-notification dependency (rsac-7e0a)
//!
//! This consumer is **heartbeat-poll-only** by design: it never registers a
//! Darwin notification observer (the Core Foundation notify-center APIs).
//! The extension (`SampleHandlerTemplate.swift`) posts `RsacDarwinNotification`
//! strings as an optional signal for a future Swift-side host-app UI observer
//! — this module does not consume them. If a future change adds a Darwin
//! listener here, update this comment, `RingLayout.swift`'s banner, and
//! `docs/MOBILE_BACKEND_DESIGN.md`'s "Signaling" bullet in lockstep. (A
//! `tests/ios_darwin_notification_contract.rs` grep guard enforces this by
//! failing if the notify-center symbol names appear in this file.)
//!
//! # Unit tests
//!
//! The pure ring math below (publish-word classification, header/geometry
//! validation, slot & wrap math, heartbeat staleness) has unit tests that
//! **compile under `--tests` for the iOS target** and run on-device later
//! under rsac-97c8 — they are contract-pinning tests, not host-run tests.
//!
//! [`CaptureTarget::SystemDefault`]: crate::core::config::CaptureTarget::SystemDefault
//! [`AudioError::StreamEnded`]: crate::core::error::AudioError::StreamEnded

#![cfg(all(target_os = "ios", feature = "feat_ios"))]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSString};

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};
use crate::core::config::{AudioFormat, CaptureTarget, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceKind};

// ═══════════════════════════════════════════════════════════════════════════
// Ring contract v1 — mirrors RingLayout.swift EXACTLY. Do not change without
// a version bump on both sides.
// ═══════════════════════════════════════════════════════════════════════════

/// ASCII `"RSAC"` read as a little-endian u32 (bytes `52 53 41 43`).
pub(crate) const RING_MAGIC: u32 = 0x4341_5352;

/// Ring layout version this consumer speaks. Bump in lockstep with
/// `RsacRingLayout.layoutVersion`.
pub(crate) const RING_LAYOUT_VERSION: u32 = 1;

/// The u64 the producer release-stores at offset 0 as the header publish
/// point: little-endian `(layoutVersion << 32) | magic`.
pub(crate) const PUBLISHED_MAGIC_VERSION: u64 =
    ((RING_LAYOUT_VERSION as u64) << 32) | RING_MAGIC as u64;

/// File name of the ring inside the App Group container (contract constant).
pub(crate) const RING_FILE_NAME: &str = "rsac_broadcast_ring_v1";

/// Header field offsets, in bytes (see the RingLayout.swift table).
pub(crate) const OFFSET_SAMPLE_RATE: usize = 8;
/// Offset of the interleaved channel count (u32).
pub(crate) const OFFSET_CHANNELS: usize = 12;
/// Offset of the ring capacity in frames (u32).
pub(crate) const OFFSET_CAPACITY_FRAMES: usize = 16;
/// Offset of the producer-owned monotonic write cursor (atomic u64).
pub(crate) const OFFSET_WRITE_CURSOR: usize = 24;
/// Offset of the consumer-owned monotonic read cursor (atomic u64).
pub(crate) const OFFSET_READ_CURSOR: usize = 32;
/// Offset of the producer liveness stamp (atomic u64, CLOCK_MONOTONIC ms).
pub(crate) const OFFSET_HEARTBEAT_MILLIS: usize = 40;
/// Offset of the producer's ring-full drop counter (atomic u64).
pub(crate) const OFFSET_PRODUCER_DROP_COUNT: usize = 48;
/// Byte offset where the interleaved f32 frame data begins (64-byte aligned).
pub(crate) const DATA_OFFSET: usize = 64;

/// A heartbeat stamp older than this ⇒ the producer is dead (terminal,
/// ADR-0010/ADR-0003). Contract constant (`heartbeatTimeoutMillis`).
pub(crate) const HEARTBEAT_TIMEOUT_MILLIS: u64 = 2_000;

// ── Consumer tuning (host-side policy, not part of the cross-process ABI) ──

/// How long `create_stream` waits for the broadcast ring header to be
/// published before failing.
///
/// The broadcast is **user-initiated** (picker UI) and the extension creates
/// the ring lazily on the first delivered audio buffer, so this window covers
/// "the user is tapping the picker right now". No existing [`StreamConfig`]
/// knob fits a wall-clock creation timeout (`buffer_size` is a ring *slot*
/// count), so this is a documented constant rather than a config field.
const PUBLISH_POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// Cadence of the header-publish poll during [`PUBLISH_POLL_TIMEOUT`].
const PUBLISH_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Sleep between drain passes when the cross-process ring is empty. Small
/// enough to keep worst-case added latency well under one tap period; large
/// enough not to burn CPU on a non-RT thread.
const DRAIN_IDLE_SLEEP: Duration = Duration::from_millis(3);

/// Frames copied out of the cross-process ring per bridge push: ~20 ms at the
/// delivered rate (floored at 256 frames). Keeps individual `AudioBuffer`s at
/// a desktop-like cadence instead of pushing multi-second slabs.
fn drain_chunk_frames(sample_rate: u32) -> usize {
    ((sample_rate as usize) / 50).max(256)
}

/// The [`DeviceId`] string of the logical broadcast-capture device.
///
/// Honest naming: this is not a hardware endpoint — it is the ReplayKit
/// broadcast transport surfaced as rsac's system-capture device.
pub(crate) const BROADCAST_DEVICE_ID: &str = "replaykit-broadcast";

// ═══════════════════════════════════════════════════════════════════════════
// Pure ring math (unit-tested; no OS access)
// ═══════════════════════════════════════════════════════════════════════════

/// Classification of the u64 at ring offset 0 (the publish word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishState {
    /// Header not (yet) published — keep polling.
    Unpublished,
    /// Published with layout v1 — the header is trustworthy.
    PublishedV1,
    /// Published by a producer speaking a **different** layout version. Both
    /// sides must reject a mismatch (contract rule) — hard error, not a poll.
    VersionMismatch(u32),
}

/// Classifies a publish word per the RingLayout torn-read-defense protocol:
/// the header is trustworthy only once offset 0 equals
/// `(layoutVersion << 32) | magic`; a matching magic with a different version
/// is a producer/consumer version skew.
pub(crate) fn classify_publish_word(word: u64) -> PublishState {
    if word == PUBLISHED_MAGIC_VERSION {
        return PublishState::PublishedV1;
    }
    if (word & 0xFFFF_FFFF) == u64::from(RING_MAGIC) {
        return PublishState::VersionMismatch((word >> 32) as u32);
    }
    PublishState::Unpublished
}

/// Validated, immutable geometry of a published ring header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RingGeometry {
    /// Frames per second (Hz), from the header.
    pub sample_rate: u32,
    /// Interleaved channel count, from the header.
    pub channels: u32,
    /// Ring capacity in **frames**, from the header.
    pub capacity_frames: u64,
}

/// Validates published header fields against sanity bounds and the mapped
/// file length. `reserved` is deliberately **not** checked: v1 says the
/// consumer must ignore it.
///
/// Rejects zero geometry, channel counts above rsac's 32-channel builder
/// ceiling (a published header claiming more is corrupt in practice), and a
/// file too small to hold `DATA_OFFSET + capacity × channels × 4` bytes
/// (all size math is overflow-checked).
pub(crate) fn validate_geometry(
    sample_rate: u32,
    channels: u32,
    capacity_frames: u32,
    file_len: usize,
) -> Result<RingGeometry, String> {
    if sample_rate == 0 {
        return Err("header sampleRate is 0".to_string());
    }
    if channels == 0 || channels > 32 {
        return Err(format!("header channels {} outside 1..=32", channels));
    }
    if capacity_frames == 0 {
        return Err("header capacityFrames is 0".to_string());
    }
    let needed = ring_file_size(capacity_frames, channels)
        .ok_or_else(|| "header geometry overflows the file-size computation".to_string())?;
    if file_len < needed {
        return Err(format!(
            "ring file is {} bytes but the published geometry needs {}",
            file_len, needed
        ));
    }
    Ok(RingGeometry {
        sample_rate,
        channels,
        capacity_frames: u64::from(capacity_frames),
    })
}

/// Total ring file size for a geometry (`dataOffset + frames × channels × 4`),
/// or `None` on overflow.
pub(crate) fn ring_file_size(capacity_frames: u32, channels: u32) -> Option<usize> {
    (capacity_frames as usize)
        .checked_mul(channels as usize)?
        .checked_mul(4)?
        .checked_add(DATA_OFFSET)
}

/// Frames readable in `[read, write)`, or `None` when the cursors are
/// inconsistent (fill level above capacity — e.g. a new broadcast generation
/// re-initialized the file under a live consumer, or header corruption).
///
/// Cursors are monotonic frame counts (never wrapped); `wrapping_sub` keeps
/// the inconsistency detectable instead of panicking in a release build.
pub(crate) fn available_frames(write: u64, read: u64, capacity_frames: u64) -> Option<u64> {
    let avail = write.wrapping_sub(read);
    if avail > capacity_frames {
        return None;
    }
    Some(avail)
}

/// Splits a read of `frames` frames starting at monotonic cursor `read` into
/// the two physically contiguous segments of the ring: `(first, second)`
/// frame counts, where `first` runs from `read % capacity` to the physical
/// end and `second` wraps to the physical start (`0` when no wrap occurs).
pub(crate) fn contiguous_segments(read: u64, frames: u64, capacity_frames: u64) -> (u64, u64) {
    debug_assert!(frames <= capacity_frames);
    let slot = read % capacity_frames;
    let first = frames.min(capacity_frames - slot);
    (first, frames - first)
}

/// `true` when a heartbeat stamp is **strictly older** than
/// [`HEARTBEAT_TIMEOUT_MILLIS`] relative to `now` (both CLOCK_MONOTONIC ms).
/// A stamp "from the future" (should not happen — same clock domain) reads as
/// fresh via the saturating subtraction.
pub(crate) fn heartbeat_is_stale(heartbeat_millis: u64, now_millis: u64) -> bool {
    now_millis.saturating_sub(heartbeat_millis) > HEARTBEAT_TIMEOUT_MILLIS
}

/// Current CLOCK_MONOTONIC time in milliseconds — the heartbeat clock domain.
///
/// The Swift producer stamps `clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1e6`;
/// `clock_gettime(CLOCK_MONOTONIC)` reads the **same** boot-scoped clock, so
/// the comparison is process-independent. Deliberately not `SystemTime`
/// (wall-clock jumps would fake producer death).
fn monotonic_now_millis() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec and CLOCK_MONOTONIC is
    // always supported on iOS; on the (impossible) failure path we fall
    // through with the zeroed timespec, which reads as "very stale" and
    // errs on the terminal side rather than hanging.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000 + (ts.tv_nsec as u64) / 1_000_000
}

// ═══════════════════════════════════════════════════════════════════════════
// BroadcastRing — the mapped consumer view of one published ring generation
// ═══════════════════════════════════════════════════════════════════════════

// UNSAFE-AUDIT (ADR-0013 consequence: the cross-process ring is a hand-rolled
// second ring implementation and carries its own soundness argument):
//
// 1. **Mapping lifetime/aliasing.** `base` points at a `MAP_SHARED` mapping of
//    the ring file, created in `try_open_ring` and unmapped exactly once in
//    `Drop`. The struct is the mapping's single in-process owner; it is moved
//    into the drain thread and never aliased by another Rust reference. The
//    backing fd is closed immediately after `mmap` succeeds (the mapping
//    remains valid per POSIX).
// 2. **SIGBUS avoidance.** Pages beyond EOF fault on access, so the file
//    length is `fstat`ed and validated against the published geometry
//    (`validate_geometry`) BEFORE any access past the header, and the whole
//    mapping length equals the fstat'ed length. The producer never shrinks
//    the file (RingProducer deliberately avoids `O_TRUNC` and re-initializes
//    through the mapping), so the length cannot go stale under us.
// 3. **Cross-language atomics (C11 interop).** The header's atomic fields are
//    accessed via `AtomicU64::from_ptr` on 8-aligned offsets (0, 24, 32, 40,
//    48 — the mmap base is page-aligned). The Swift producer accesses the
//    same addresses through a C shim over C11 `atomic_*` builtins
//    (`CRsacRingAtomics`). Rust's `AtomicU64` is documented to have the same
//    representation and lock-free operations as the platform's 8-byte C11
//    atomics on arm64, so release/acquire pairs synchronize across the
//    process boundary. Both sides only ever touch these words atomically.
// 4. **Torn-read defense for non-atomic data.** The u32 header fields are
//    written before the producer's release-store of the publish word and are
//    immutable afterwards; this consumer only reads them after an
//    acquire-load observes `PUBLISHED_MAGIC_VERSION` (happens-before). Frame
//    bytes in `[read, write)` are written before the release-store of
//    `writeCursor` and are never rewritten until the consumer release-stores
//    `readCursor` past them (the producer acquire-loads `readCursor` before
//    reusing slots), so the plain `copy_nonoverlapping` reads in
//    `drain_once` are data-race-free.
// 5. **Generation churn.** A new broadcast re-initializes the file through a
//    fresh mapping in the producer. If that happens while this consumer is
//    attached, the cursors reset and `available_frames` detects the
//    inconsistent fill level (> capacity) ⇒ fatal terminal — never an
//    out-of-bounds access (slot math is `% capacity` over the validated
//    geometry of *this* mapping).
/// One mapped, validated, published generation of the broadcast ring
/// (consumer side).
pub(crate) struct BroadcastRing {
    /// Base of the `MAP_SHARED` mapping (page-aligned).
    base: *mut u8,
    /// Length of the mapping (== the fstat'ed file length at open).
    map_len: usize,
    /// Validated header geometry (immutable once published).
    geometry: RingGeometry,
}

// SAFETY: `BroadcastRing` is moved into exactly one drain thread and never
// shared between Rust threads; the raw pointer targets process-shared memory
// whose concurrent (cross-process) accesses are synchronized by the atomic
// cursor protocol audited above. Sending the owning handle to another thread
// is therefore sound. (No `Sync` impl — it is never shared by reference.)
unsafe impl Send for BroadcastRing {}

impl BroadcastRing {
    /// Borrow an atomic header field at `offset` (must be one of the 8-aligned
    /// atomic offsets of the v1 layout).
    fn atomic_u64_at(&self, offset: usize) -> &AtomicU64 {
        debug_assert!(offset.is_multiple_of(8) && offset + 8 <= DATA_OFFSET);
        // SAFETY: `base + offset` is 8-aligned (page-aligned base, offset a
        // multiple of 8), in-bounds of the mapping (validated ≥ DATA_OFFSET),
        // valid for the lifetime of `&self`, and only ever accessed
        // atomically by both processes (UNSAFE-AUDIT items 1 and 3).
        unsafe { AtomicU64::from_ptr(self.base.add(offset).cast::<u64>()) }
    }

    /// Acquire-load of the producer-owned write cursor: frames in
    /// `[readCursor, writeCursor)` are fully written after this load.
    fn write_cursor(&self) -> u64 {
        self.atomic_u64_at(OFFSET_WRITE_CURSOR)
            .load(Ordering::Acquire)
    }

    /// Relaxed load of the consumer-owned read cursor (this thread is its
    /// only writer).
    fn read_cursor(&self) -> u64 {
        self.atomic_u64_at(OFFSET_READ_CURSOR)
            .load(Ordering::Relaxed)
    }

    /// Release-store of the read cursor **after** the frames were copied out,
    /// so the producer's acquire-load never reuses slots we are still reading.
    fn store_read_cursor(&self, value: u64) {
        self.atomic_u64_at(OFFSET_READ_CURSOR)
            .store(value, Ordering::Release)
    }

    /// Relaxed load of the heartbeat stamp (a standalone liveness value; it
    /// guards no other data, matching the producer's relaxed store).
    fn heartbeat_millis(&self) -> u64 {
        self.atomic_u64_at(OFFSET_HEARTBEAT_MILLIS)
            .load(Ordering::Relaxed)
    }

    /// Relaxed load of the producer's ring-full drop counter.
    fn producer_drop_count(&self) -> u64 {
        self.atomic_u64_at(OFFSET_PRODUCER_DROP_COUNT)
            .load(Ordering::Relaxed)
    }

    /// Start of the interleaved f32 data region (64-byte aligned).
    fn data_ptr(&self) -> *const f32 {
        // SAFETY: DATA_OFFSET is in-bounds (map_len ≥ DATA_OFFSET + data).
        unsafe { self.base.add(DATA_OFFSET).cast::<f32>() }
    }
}

impl Drop for BroadcastRing {
    fn drop(&mut self) {
        // SAFETY: `base`/`map_len` are exactly the pointer/length returned by
        // the successful mmap in `try_open_ring`, unmapped exactly once (this
        // struct is the mapping's single owner — UNSAFE-AUDIT item 1).
        unsafe {
            libc::munmap(self.base.cast::<libc::c_void>(), self.map_len);
        }
    }
}

// ── Opening / polling ─────────────────────────────────────────────────────

/// Why a probe of the ring file did not yield a live, published ring yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingReason {
    /// The ring file does not exist yet (broadcast not started, or no audio
    /// buffer delivered yet — the extension creates the ring lazily).
    NoFile,
    /// The file exists but the header publish word is not committed yet.
    Unpublished,
    /// The file is published but its heartbeat is stale — a dead previous
    /// broadcast generation, not a live one.
    StaleHeartbeat,
    /// A transient OS error (open/fstat/mmap) — retried until the deadline.
    OsError,
}

impl PendingReason {
    /// Actionable one-liner for the timeout error message.
    fn describe(self) -> &'static str {
        match self {
            PendingReason::NoFile => {
                "the ring file was never created (broadcast not started, or the \
                 broadcast extension is not built on RsacBroadcastKit / not \
                 entitled to this App Group)"
            }
            PendingReason::Unpublished => {
                "the ring file exists but its header was never published \
                 (the extension may have received no audio buffers yet)"
            }
            PendingReason::StaleHeartbeat => {
                "only a stale ring from a previous, ended broadcast was found \
                 (start a new broadcast)"
            }
            PendingReason::OsError => "the ring file could not be opened/mapped",
        }
    }
}

/// Resolves the App Group container directory for `group` via
/// `-[NSFileManager containerURLForSecurityApplicationGroupIdentifier:]`.
///
/// `None` means the identifier is wrong or the **host app** lacks the App
/// Group entitlement (the same failure the Swift producer surfaces as
/// `appGroupUnavailable`).
fn app_group_container_path(group: &str) -> Option<PathBuf> {
    autoreleasepool(|_| {
        let ns_group = NSString::from_str(group);
        let url = NSFileManager::defaultManager()
            .containerURLForSecurityApplicationGroupIdentifier(&ns_group)?;
        let path = url.path()?;
        Some(PathBuf::from(path.to_string()))
    })
}

/// One probe of the ring file: open → size-check → map → publish-word check →
/// geometry validation → heartbeat-freshness check.
///
/// - `Ok(Ok(ring))` — a live, published, validated ring (mapping owned).
/// - `Ok(Err(reason))` — not ready yet; keep polling.
/// - `Err(_)` — a **hard** contract failure (version mismatch / corrupt
///   published geometry) that polling cannot fix.
fn try_open_ring(path: &Path) -> AudioResult<Result<BroadcastRing, PendingReason>> {
    let c_path = match CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(p) => p,
        Err(_) => {
            // A NUL inside an App-Group path cannot happen in practice; treat
            // as a hard configuration error rather than polling forever.
            return Err(AudioError::StreamCreationFailed {
                reason: format!("ring path {:?} contains an interior NUL byte", path),
                context: None,
            });
        }
    };

    // SAFETY: `c_path` is a valid NUL-terminated C string; O_RDWR because the
    // consumer owns (and release-stores) `readCursor` in the shared mapping.
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        let errno = std::io::Error::last_os_error();
        return Ok(Err(match errno.raw_os_error() {
            Some(code) if code == libc::ENOENT => PendingReason::NoFile,
            _ => {
                log::debug!("broadcast ring open({:?}) failed: {}", path, errno);
                PendingReason::OsError
            }
        }));
    }
    // Ensure the fd is closed on every exit path below; a successful mmap
    // stays valid after close(2) per POSIX.
    struct FdGuard(libc::c_int);
    impl Drop for FdGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is an fd this guard exclusively owns.
            unsafe {
                libc::close(self.0);
            }
        }
    }
    let _fd_guard = FdGuard(fd);

    // SAFETY: zero-initialized stat buffer; fstat on an owned, open fd.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `st` is a valid, writable stat buffer.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        log::debug!(
            "broadcast ring fstat failed: {}",
            std::io::Error::last_os_error()
        );
        return Ok(Err(PendingReason::OsError));
    }
    let file_len = st.st_size.max(0) as usize;
    if file_len < DATA_OFFSET {
        // Producer created the file but has not sized/published it yet.
        return Ok(Err(PendingReason::Unpublished));
    }

    // SAFETY: mapping `file_len` bytes of an open O_RDWR fd, MAP_SHARED so the
    // cursor stores are visible cross-process; length matches the fstat'ed
    // size, so no access below can fault past EOF (UNSAFE-AUDIT item 2).
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            file_len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if mapped == libc::MAP_FAILED {
        log::debug!(
            "broadcast ring mmap failed: {}",
            std::io::Error::last_os_error()
        );
        return Ok(Err(PendingReason::OsError));
    }
    let base = mapped.cast::<u8>();

    /// Unmaps on early exits; disarmed once ownership moves into BroadcastRing.
    struct MapGuard {
        base: *mut u8,
        len: usize,
        armed: bool,
    }
    impl Drop for MapGuard {
        fn drop(&mut self) {
            if self.armed {
                // SAFETY: exactly the pointer/length of the mmap above; the
                // guard is disarmed before BroadcastRing takes ownership, so
                // there is a single munmap per mapping.
                unsafe {
                    libc::munmap(self.base.cast::<libc::c_void>(), self.len);
                }
            }
        }
    }
    let mut map_guard = MapGuard {
        base,
        len: file_len,
        armed: true,
    };

    // Publish-word acquire-load: only a committed v1 header is trusted
    // (torn-read defense — UNSAFE-AUDIT item 4).
    //
    // SAFETY: offset 0 is 8-aligned and in-bounds; accessed atomically by
    // both processes (UNSAFE-AUDIT item 3).
    let publish = unsafe { AtomicU64::from_ptr(base.cast::<u64>()) }.load(Ordering::Acquire);
    match classify_publish_word(publish) {
        PublishState::Unpublished => return Ok(Err(PendingReason::Unpublished)),
        PublishState::VersionMismatch(version) => {
            return Err(AudioError::StreamCreationFailed {
                reason: format!(
                    "broadcast ring layout version mismatch: the extension wrote \
                     v{version}, this rsac speaks v{RING_LAYOUT_VERSION}. Update rsac and the \
                     RsacBroadcastKit SwiftPM package in lockstep (the ring \
                     layout is a versioned cross-process ABI)"
                ),
                context: None,
            });
        }
        PublishState::PublishedV1 => {}
    }

    // Non-atomic header fields: written before the publish release-store and
    // immutable afterwards, so plain reads are race-free after the acquire
    // above. Stored little-endian; `from_le` keeps the intent explicit (a
    // no-op on arm64).
    //
    // SAFETY: 4-aligned, in-bounds header reads (file_len ≥ DATA_OFFSET).
    let read_u32 =
        |offset: usize| -> u32 { u32::from_le(unsafe { base.add(offset).cast::<u32>().read() }) };
    let sample_rate = read_u32(OFFSET_SAMPLE_RATE);
    let channels = read_u32(OFFSET_CHANNELS);
    let capacity_frames = read_u32(OFFSET_CAPACITY_FRAMES);

    let geometry = match validate_geometry(sample_rate, channels, capacity_frames, file_len) {
        Ok(g) => g,
        Err(why) => {
            // Published geometry is immutable — polling cannot fix this.
            return Err(AudioError::StreamCreationFailed {
                reason: format!(
                    "broadcast ring header is published but invalid ({why}) — \
                     the ring file is corrupt or produced by an incompatible \
                     RsacBroadcastKit build"
                ),
                context: None,
            });
        }
    };

    map_guard.armed = false;
    let ring = BroadcastRing {
        base,
        map_len: file_len,
        geometry,
    };

    // A published ring with a stale heartbeat is the remnant of a previous,
    // ended broadcast — not a live producer. Keep polling for a fresh
    // generation (the producer re-publishes through the same file).
    if heartbeat_is_stale(ring.heartbeat_millis(), monotonic_now_millis()) {
        return Ok(Err(PendingReason::StaleHeartbeat));
    }

    Ok(Ok(ring))
}

/// Bounded poll for a live, published ring, per the create_stream contract.
fn poll_for_published_ring(path: &Path, timeout: Duration) -> AudioResult<BroadcastRing> {
    let deadline = Instant::now() + timeout;
    // Assigned on every iteration before the deadline check reads it.
    let mut last_pending;
    loop {
        match try_open_ring(path)? {
            Ok(ring) => return Ok(ring),
            Err(reason) => last_pending = reason,
        }
        if Instant::now() >= deadline {
            return Err(AudioError::StreamCreationFailed {
                reason: format!(
                    "broadcast not started: no live rsac broadcast ring appeared \
                     within {}s ({}). System-audio capture on iOS is \
                     user-initiated: the user must start the broadcast via the \
                     control-center screen-recording picker or an \
                     RPSystemBroadcastPickerView, and the app must embed a \
                     Broadcast Upload Extension built on RsacBroadcastKit \
                     sharing this App Group — see mobile/ios/README.md",
                    timeout.as_secs(),
                    last_pending.describe()
                ),
                context: None,
            });
        }
        std::thread::sleep(PUBLISH_POLL_INTERVAL);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Drain thread — cross-process ring → BridgeProducer
// ═══════════════════════════════════════════════════════════════════════════

/// Outcome of one drain pass over the cross-process ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    /// Frames were copied into the bridge; try again immediately.
    Drained,
    /// The ring is currently empty.
    Empty,
    /// The cursors are inconsistent (fill level above capacity) — the file
    /// was re-initialized under us or is corrupt. Terminal.
    Corrupt,
}

/// Copies up to `chunk_frames` frames out of the ring into `scratch` and
/// pushes them into the bridge (drop-not-block), then advances `readCursor`.
///
/// The read cursor advances **whether or not** the bridge push succeeded: the
/// frames were consumed from the cross-process ring either way, and a bridge
/// ring-full drop is already counted by `push_samples_or_drop_stamped`
/// (ADR-0007 drop-don't-block accounting).
fn drain_once(
    ring: &BroadcastRing,
    producer: &mut BridgeProducer,
    scratch: &mut [f32],
    chunk_frames: usize,
) -> DrainOutcome {
    let capacity = ring.geometry.capacity_frames;
    let channels = ring.geometry.channels as usize;

    let write = ring.write_cursor(); // acquire: frames below are fully written
    let read = ring.read_cursor();
    let avail = match available_frames(write, read, capacity) {
        Some(a) => a,
        None => return DrainOutcome::Corrupt,
    };
    if avail == 0 {
        return DrainOutcome::Empty;
    }

    let take = avail.min(chunk_frames as u64);
    let (first, second) = contiguous_segments(read, take, capacity);
    let slot = (read % capacity) as usize;
    let data = ring.data_ptr();

    // SAFETY: `validate_geometry` proved the mapping holds the whole data
    // region; `slot + first ≤ capacity` and `second ≤ capacity` by the
    // segment math, so both source ranges are in-bounds. `scratch` is sized
    // to `chunk_frames × channels ≥ take × channels` by the caller. The
    // frames in `[read, write)` are stable until we release-store the
    // advanced read cursor below (UNSAFE-AUDIT item 4).
    unsafe {
        std::ptr::copy_nonoverlapping(
            data.add(slot * channels),
            scratch.as_mut_ptr(),
            (first as usize) * channels,
        );
        if second > 0 {
            std::ptr::copy_nonoverlapping(
                data,
                scratch.as_mut_ptr().add((first as usize) * channels),
                (second as usize) * channels,
            );
        }
    }

    let samples = (take as usize) * channels;
    // Non-RT rsac-owned thread → the stamped (stream-position) push variant,
    // like the Windows capture loop. Ring-full ⇒ drop + count, never block.
    producer.push_samples_or_drop_stamped(
        &scratch[..samples],
        ring.geometry.channels as u16,
        ring.geometry.sample_rate,
    );

    // Release: the producer may reuse these slots only after this store.
    ring.store_read_cursor(read + take);

    DrainOutcome::Drained
}

/// Forwards the producer's cross-process `producerDropCount` into the
/// bridge's overrun accounting so `overrun_count()` reflects **all** loss.
///
/// Only `buffers_dropped` is advanced: `consecutive_drops` means "consecutive
/// bridge pushes that dropped, with no success in between" (it feeds
/// `is_under_backpressure`), and producer-side drops interleave arbitrarily
/// with successful bridge pushes — folding them in would corrupt that signal.
fn forward_producer_drops(ring: &BroadcastRing, shared: &BridgeShared, last_seen: &mut u64) {
    let now = ring.producer_drop_count();
    if now > *last_seen {
        let delta = now - *last_seen;
        *last_seen = now;
        shared.buffers_dropped.fetch_add(delta, Ordering::Relaxed);
        log::debug!("broadcast extension dropped {delta} buffer(s) ring-full (total {now})");
    }
}

/// Body of the drain thread. Runs until the host stops the stream (graceful
/// terminal) or the producer dies / the ring corrupts (fatal terminal).
fn run_drain_loop(
    ring: BroadcastRing,
    mut producer: BridgeProducer,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
) {
    let channels = ring.geometry.channels as usize;
    let chunk_frames = drain_chunk_frames(ring.geometry.sample_rate);
    // The ONLY buffer allocation, made once on this non-RT thread before the
    // loop; `drain_once` never grows it.
    let mut scratch = vec![0.0f32; chunk_frames * channels];
    let mut forwarded_drops = ring.producer_drop_count();

    loop {
        if stop.load(Ordering::SeqCst) {
            // Host-initiated stop: graceful producer terminal (ADR-0010) —
            // `Running → Stopping` keeps the bridge-buffered tail drainable.
            producer.signal_done();
            break;
        }

        forward_producer_drops(&ring, producer.shared(), &mut forwarded_drops);

        match drain_once(&ring, &mut producer, &mut scratch, chunk_frames) {
            DrainOutcome::Drained => {
                // Non-RT thread: wake a parked blocking reader promptly (the
                // Windows-capture-loop precedent; ADR-0001 forbids this only
                // on RT callback paths).
                producer.notify_consumers();
            }
            DrainOutcome::Empty => {
                if heartbeat_is_stale(ring.heartbeat_millis(), monotonic_now_millis()) {
                    // Extension killed or broadcast ended: the ring is drained
                    // (we only reach here on Empty) ⇒ fatal producer terminal
                    // (ADR-0010) ⇒ readers observe StreamEnded after draining
                    // any bridge-buffered tail (ADR-0003).
                    log::info!(
                        "broadcast ring heartbeat older than {HEARTBEAT_TIMEOUT_MILLIS} ms \
                         — broadcast ended or extension killed; ending stream"
                    );
                    producer.signal_error();
                    break;
                }
                std::thread::sleep(DRAIN_IDLE_SLEEP);
            }
            DrainOutcome::Corrupt => {
                log::error!(
                    "broadcast ring cursors inconsistent (fill level above \
                     capacity) — file re-initialized under a live consumer or \
                     corrupt; ending stream"
                );
                producer.signal_error();
                break;
            }
        }
    }

    active.store(false, Ordering::SeqCst);
    // `ring` drops here: the mapping is unmapped only after the loop can no
    // longer touch it.
}

// ═══════════════════════════════════════════════════════════════════════════
// BroadcastPlatformStream — PlatformStream over the drain thread
// ═══════════════════════════════════════════════════════════════════════════

/// Platform-specific stream handle for the iOS broadcast path.
///
/// Owns the drain thread's lifecycle; all fields are `Send + Sync` types, so
/// no `unsafe impl` is needed (unlike the ObjC-holding mic stream).
///
/// # Shutdown
///
/// [`stop_capture`](PlatformStream::stop_capture) (and `Drop`, via the same
/// choke point) sets the stop flag, joins the drain thread (which unmaps the
/// ring on exit), and then makes sure the bridge reached an ending state and
/// parked readers are woken (ADR-0010) — the CAS no-ops when the thread
/// already signalled a terminal itself (heartbeat death / corruption).
pub(crate) struct BroadcastPlatformStream {
    /// Tells the drain thread to exit (host-initiated, graceful).
    stop: Arc<AtomicBool>,
    /// The drain thread's handle; taken exactly once by the stop choke point.
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// Cleared by the drain thread on exit (any cause), so `is_active()` is
    /// honest even when the producer died without a host-side stop.
    active: Arc<AtomicBool>,
    /// Producer-terminal-signal handle (ADR-0010): drives `Running → Stopping`
    /// + reader wake from the stop path.
    terminal: Arc<BridgeShared>,
}

impl BroadcastPlatformStream {
    /// Stops the drain thread (once) and lands the bridge in an ending state.
    /// Idempotent: the JoinHandle is taken exactly once; later calls no-op.
    fn stop_thread(&self) -> AudioResult<()> {
        self.stop.store(true, Ordering::SeqCst);
        let handle = {
            let mut guard = self.join.lock().map_err(|_| AudioError::InternalError {
                message: "broadcast drain-thread handle mutex poisoned".to_string(),
                source: None,
            })?;
            guard.take()
        };
        if let Some(handle) = handle {
            // The thread exits within one DRAIN_IDLE_SLEEP/drain pass of the
            // stop flag; a panicked drain thread must not panic the stopper.
            let _ = handle.join();
        }
        self.active.store(false, Ordering::SeqCst);

        // Belt-and-suspenders graceful terminal: no-ops when the drain thread
        // already drove the state (signal_done / signal_error).
        let _ = self
            .terminal
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        self.terminal.notify_wake();
        #[cfg(feature = "async-stream")]
        self.terminal.waker.wake();
        Ok(())
    }
}

impl PlatformStream for BroadcastPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_thread()
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

impl Drop for BroadcastPlatformStream {
    /// Deterministic shutdown: dropping the handle never leaks a drain thread
    /// or leaves a parked reader hanging (ADR-0010).
    fn drop(&mut self) {
        if self.active.load(Ordering::SeqCst) {
            if let Err(e) = self.stop_thread() {
                log::warn!("BroadcastPlatformStream::drop: stop failed: {:?}", e);
            }
        }
    }
}

/// Assert that `BroadcastPlatformStream` is `Send + Sync` (required by
/// `PlatformStream` and `BridgeStream<S>`).
fn _assert_broadcast_platform_stream_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<BroadcastPlatformStream>();
}

// ═══════════════════════════════════════════════════════════════════════════
// BroadcastAudioDevice — the logical system-capture device
// ═══════════════════════════════════════════════════════════════════════════

/// The logical iOS system-audio capture device (ReplayKit broadcast).
///
/// A metadata-only handle: constructing it touches no OS resources; the ring
/// polling/mapping happens in [`create_stream`](AudioDevice::create_stream).
/// This is what [`IosDeviceEnumerator::default_device`] returns — matching
/// the desktop convention where the *default device* is the system-loopback
/// endpoint — and it also appears in `enumerate_devices()` alongside the mic.
///
/// [`IosDeviceEnumerator::default_device`]: crate::audio::ios::IosDeviceEnumerator
#[derive(Debug, Clone, Copy)]
pub struct BroadcastAudioDevice;

impl BroadcastAudioDevice {
    /// Creates the logical broadcast-capture device handle.
    pub fn new() -> Self {
        Self
    }
}

impl Default for BroadcastAudioDevice {
    fn default() -> Self {
        Self::new()
    }
}

/// Validates a [`CaptureTarget`] against the broadcast device.
///
/// | Target | Outcome |
/// |---|---|
/// | `SystemDefault` | `Ok(())` — the broadcast mix |
/// | `Device("replaykit-broadcast")` | `Ok(())` — this device, selected explicitly |
/// | `Device(other)` | [`AudioError::DeviceNotFound`] |
/// | `Application*` / `ProcessTree` | [`AudioError::PlatformNotSupported`] — **permanent** (no iOS API) |
///
/// Exhaustive on purpose (no wildcard) so a new `CaptureTarget` variant must
/// be classified before the crate compiles for iOS.
fn ensure_broadcast_target(target: &CaptureTarget) -> AudioResult<()> {
    match target {
        CaptureTarget::SystemDefault => Ok(()),
        CaptureTarget::Device(id) if id.0.eq_ignore_ascii_case(BROADCAST_DEVICE_ID) => Ok(()),
        CaptureTarget::Device(id) => Err(AudioError::DeviceNotFound {
            device_id: id.0.clone(),
        }),
        CaptureTarget::Application(_)
        | CaptureTarget::ApplicationByName(_)
        | CaptureTarget::ProcessTree(_) => Err(AudioError::PlatformNotSupported {
            feature: "per-application / process-tree capture on iOS: Apple provides \
                      no API for capturing another app's audio — this is permanent, \
                      not a pending feature (ADR-0013). The ReplayKit broadcast \
                      (CaptureTarget::SystemDefault) captures the whole system mix, \
                      with no per-app filter"
                .to_string(),
            platform: "ios".to_string(),
        }),
    }
}

impl AudioDevice for BroadcastAudioDevice {
    fn id(&self) -> DeviceId {
        DeviceId(BROADCAST_DEVICE_ID.to_string())
    }

    fn name(&self) -> String {
        "System audio (ReplayKit broadcast)".to_string()
    }

    /// `true`: this is the default device **of its kind** — the endpoint
    /// rsac's loopback-oriented `default_device()` returns for system capture
    /// (the mic device stays the default [`DeviceKind::Input`]).
    fn is_default(&self) -> bool {
        true
    }

    /// Empty by design (the Linux/PipeWire convention): the delivered format
    /// is whatever the broadcast extension published in the ring header,
    /// known only at stream creation. `build()`'s negotiation treats an empty
    /// list as "open with the request as-is"; the authoritative format is
    /// reported via [`CapturingStream::format`] once the stream exists.
    fn supported_formats(&self) -> Vec<AudioFormat> {
        Vec::new()
    }

    /// [`DeviceKind::Output`]: the broadcast is a loopback of the system's
    /// audio **output** mix — the closest honest classification for a
    /// non-hardware transport endpoint.
    fn kind(&self) -> AudioResult<DeviceKind> {
        Ok(DeviceKind::Output)
    }

    /// Attaches to the broadcast ring and returns the live capture stream.
    ///
    /// Flow: validate target → require the App Group id
    /// ([`StreamConfig::ios_app_group`], the ADR-0013 consent artifact) →
    /// resolve the container → **bounded poll** ([`PUBLISH_POLL_TIMEOUT`]) for
    /// a live, published ring → read geometry, publish the delivered format
    /// on the bridge → spawn the non-RT drain thread → wrap in `BridgeStream`.
    ///
    /// # Errors
    ///
    /// - [`AudioError::UserConsentRequired`] when no App Group id was
    ///   configured (`AudioCaptureBuilder::with_ios_app_group`) — normally
    ///   caught earlier by the builder preflight; kept here for direct
    ///   `AudioDevice` users.
    /// - [`AudioError::StreamCreationFailed`] when the App Group container is
    ///   unavailable (entitlement missing / wrong id), when no live ring
    ///   appears within the poll window (broadcast not started), or on a ring
    ///   version/geometry contract violation.
    /// - [`AudioError::PlatformNotSupported`] / [`AudioError::DeviceNotFound`]
    ///   per [`ensure_broadcast_target`].
    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        ensure_broadcast_target(&config.capture_target)?;

        let group =
            config
                .ios_app_group
                .as_deref()
                .ok_or_else(|| AudioError::UserConsentRequired {
                    feature: "iOS broadcast capture".to_string(),
                    missing: "App Group identifier — call \
                          AudioCaptureBuilder::with_ios_app_group(\"group.…\") and \
                          embed the RsacBroadcastKit extension"
                        .to_string(),
                })?;

        let container =
            app_group_container_path(group).ok_or_else(|| AudioError::StreamCreationFailed {
                reason: format!(
                    "App Group container unavailable for '{group}' — the host app's \
                     com.apple.security.application-groups entitlement must list \
                     this identifier (and the broadcast extension must share it)"
                ),
                context: None,
            })?;
        let ring_path = container.join(RING_FILE_NAME);

        let ring = poll_for_published_ring(&ring_path, PUBLISH_POLL_TIMEOUT)?;

        // The ring header is the authoritative delivered format (always f32
        // per the contract's data-region definition).
        let delivered = AudioFormat {
            sample_rate: ring.geometry.sample_rate,
            channels: ring.geometry.channels as u16,
            sample_format: SampleFormat::F32,
        };

        // Ring sizing: honour the requested slot count like the mic path
        // (ADR-0007 direction), defaulting to calculate_capacity(None, 4) = 64.
        let capacity = calculate_capacity(config.buffer_size, 4);
        let (producer, consumer) = create_bridge(capacity, delivered.clone());

        // Publish the delivery format BEFORE any push (M1 pattern).
        producer.set_negotiated_format(&delivered);

        // Transition bridge state Created → Running before the drain thread
        // starts pushing, so the first buffers are readable.
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .map_err(|actual| AudioError::InternalError {
                message: format!(
                    "Failed to transition bridge state to Running (was {:?})",
                    actual
                ),
                source: None,
            })?;

        let terminal = Arc::clone(consumer.shared());
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));

        let ring_capacity_frames = ring.geometry.capacity_frames;
        let thread_stop = Arc::clone(&stop);
        let thread_active = Arc::clone(&active);
        let handle = std::thread::Builder::new()
            .name("rsac-ios-broadcast-drain".to_string())
            .spawn(move || run_drain_loop(ring, producer, thread_stop, thread_active))
            .map_err(|e| AudioError::StreamCreationFailed {
                reason: format!("failed to spawn the broadcast drain thread: {e}"),
                context: None,
            })?;

        log::debug!(
            "ReplayKit broadcast capture attached ({} Hz, {} ch, ring {} frames)",
            delivered.sample_rate,
            delivered.channels,
            ring_capacity_frames
        );

        let platform_stream = BroadcastPlatformStream {
            stop,
            join: Mutex::new(Some(handle)),
            active,
            terminal,
        };

        Ok(Box::new(BridgeStream::new(
            consumer,
            platform_stream,
            delivered,
            Duration::from_secs(1),
        )))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — PURE ring math only (no mmap, no ObjC, no threads). They pin the
// RingLayout.swift v1 contract byte-for-byte, compile for the iOS target
// under `--tests`, and run on-device later under rsac-97c8.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, ProcessId};

    // ── Contract pinning ─────────────────────────────────────────────

    #[test]
    fn contract_constants_match_ring_layout_v1() {
        // Byte-for-byte mirror of RingLayout.swift (v1). If any of these
        // asserts needs changing, the layout version must be bumped on BOTH
        // sides.
        assert_eq!(RING_MAGIC, 0x4341_5352, "ASCII \"RSAC\" as LE u32");
        assert_eq!(RING_LAYOUT_VERSION, 1);
        assert_eq!(PUBLISHED_MAGIC_VERSION, 0x0000_0001_4341_5352);
        assert_eq!(RING_FILE_NAME, "rsac_broadcast_ring_v1");
        assert_eq!(OFFSET_SAMPLE_RATE, 8);
        assert_eq!(OFFSET_CHANNELS, 12);
        assert_eq!(OFFSET_CAPACITY_FRAMES, 16);
        assert_eq!(OFFSET_WRITE_CURSOR, 24);
        assert_eq!(OFFSET_READ_CURSOR, 32);
        assert_eq!(OFFSET_HEARTBEAT_MILLIS, 40);
        assert_eq!(OFFSET_PRODUCER_DROP_COUNT, 48);
        assert_eq!(DATA_OFFSET, 64);
        assert_eq!(HEARTBEAT_TIMEOUT_MILLIS, 2_000);
    }

    #[test]
    fn atomic_header_offsets_are_8_aligned() {
        for offset in [
            0,
            OFFSET_WRITE_CURSOR,
            OFFSET_READ_CURSOR,
            OFFSET_HEARTBEAT_MILLIS,
            OFFSET_PRODUCER_DROP_COUNT,
        ] {
            assert_eq!(offset % 8, 0, "atomic field at {offset} must be 8-aligned");
        }
    }

    // ── Publish-word classification ──────────────────────────────────

    #[test]
    fn publish_word_v1_is_published() {
        assert_eq!(
            classify_publish_word(PUBLISHED_MAGIC_VERSION),
            PublishState::PublishedV1
        );
    }

    #[test]
    fn publish_word_zero_and_garbage_are_unpublished() {
        assert_eq!(classify_publish_word(0), PublishState::Unpublished);
        // Magic bytes absent → unpublished, regardless of the high half.
        assert_eq!(
            classify_publish_word(0x0000_0001_DEAD_BEEF),
            PublishState::Unpublished
        );
        // Magic alone (version half still 0) is NOT the committed word: the
        // producer commits magic+version as a single u64 store.
        assert_eq!(
            classify_publish_word(u64::from(RING_MAGIC)),
            PublishState::VersionMismatch(0)
        );
    }

    #[test]
    fn publish_word_future_version_is_a_mismatch_not_a_wait() {
        assert_eq!(
            classify_publish_word((2u64 << 32) | u64::from(RING_MAGIC)),
            PublishState::VersionMismatch(2)
        );
    }

    // ── Geometry validation ──────────────────────────────────────────

    #[test]
    fn geometry_happy_path_48k_stereo_2s() {
        // The producer default: 2 s at 48 kHz stereo = 96 000 frames,
        // 64 + 96_000 * 2 * 4 = 768_064 bytes.
        let len = ring_file_size(96_000, 2).unwrap();
        assert_eq!(len, 64 + 96_000 * 2 * 4);
        let g = validate_geometry(48_000, 2, 96_000, len).unwrap();
        assert_eq!(g.sample_rate, 48_000);
        assert_eq!(g.channels, 2);
        assert_eq!(g.capacity_frames, 96_000);
    }

    #[test]
    fn geometry_rejects_zero_fields() {
        let len = ring_file_size(1_000, 2).unwrap();
        assert!(validate_geometry(0, 2, 1_000, len).is_err());
        assert!(validate_geometry(48_000, 0, 1_000, len).is_err());
        assert!(validate_geometry(48_000, 2, 0, len).is_err());
    }

    #[test]
    fn geometry_rejects_absurd_channel_counts() {
        let len = ring_file_size(1_000, 33).unwrap();
        assert!(validate_geometry(48_000, 33, 1_000, len).is_err());
    }

    #[test]
    fn geometry_rejects_file_too_small() {
        let needed = ring_file_size(96_000, 2).unwrap();
        assert!(validate_geometry(48_000, 2, 96_000, needed - 1).is_err());
        assert!(validate_geometry(48_000, 2, 96_000, needed).is_ok());
    }

    #[test]
    fn geometry_size_math_never_panics_on_huge_headers() {
        // A (corrupt) header claiming a gigantic ring must degrade to a clean
        // rejection — checked math, no overflow panic, no wrap-around into a
        // small "needed" size that a tiny file could satisfy.
        assert!(validate_geometry(48_000, 32, u32::MAX, 1024).is_err());
    }

    // ── Cursor / slot math ───────────────────────────────────────────

    #[test]
    fn available_frames_basic_and_empty() {
        assert_eq!(available_frames(0, 0, 100), Some(0));
        assert_eq!(available_frames(42, 0, 100), Some(42));
        assert_eq!(available_frames(150, 70, 100), Some(80));
        // Full ring is legal.
        assert_eq!(available_frames(100, 0, 100), Some(100));
    }

    #[test]
    fn available_frames_detects_inconsistent_cursors() {
        // Fill level above capacity: corrupt / re-initialized generation.
        assert_eq!(available_frames(201, 100, 100), None);
        // A reset writeCursor behind readCursor wraps to a huge value → None.
        assert_eq!(available_frames(0, 1_000, 100), None);
    }

    #[test]
    fn segments_without_wrap() {
        // read at slot 10 of 100, taking 20: one contiguous segment.
        assert_eq!(contiguous_segments(10, 20, 100), (20, 0));
    }

    #[test]
    fn segments_exactly_to_the_edge() {
        // Taking exactly up to the physical end: still no wrap.
        assert_eq!(contiguous_segments(90, 10, 100), (10, 0));
    }

    #[test]
    fn segments_with_wrap() {
        // read at slot 90 of 100, taking 30: 10 to the edge + 20 wrapped.
        assert_eq!(contiguous_segments(90, 30, 100), (10, 20));
    }

    #[test]
    fn segments_at_large_monotonic_cursors() {
        // Cursors are monotonic (never wrapped); slot math must hold at
        // arbitrary magnitudes. 1e12 % 96_000 = 62_500... compute directly.
        let read: u64 = 1_000_000_000_000;
        let capacity: u64 = 96_000;
        let slot = read % capacity;
        let take = 96_000; // full ring
        let (first, second) = contiguous_segments(read, take, capacity);
        assert_eq!(first, capacity - slot);
        assert_eq!(second, take - first);
        assert_eq!(first + second, take);
    }

    // ── Heartbeat staleness ──────────────────────────────────────────

    #[test]
    fn heartbeat_fresh_and_boundary() {
        assert!(!heartbeat_is_stale(10_000, 10_000));
        assert!(!heartbeat_is_stale(10_000, 11_999));
        // Exactly the timeout is NOT stale ("older than" is strict, matching
        // the Swift contract's wording).
        assert!(!heartbeat_is_stale(10_000, 12_000));
        assert!(heartbeat_is_stale(10_000, 12_001));
    }

    #[test]
    fn heartbeat_from_the_future_reads_fresh() {
        // Same clock domain, so this cannot happen — but it must not
        // underflow into "stale".
        assert!(!heartbeat_is_stale(20_000, 10_000));
    }

    // ── Drain sizing ─────────────────────────────────────────────────

    #[test]
    fn drain_chunk_is_about_20ms_with_a_floor() {
        assert_eq!(drain_chunk_frames(48_000), 960);
        assert_eq!(drain_chunk_frames(44_100), 882);
        // Low rates hit the 256-frame floor.
        assert_eq!(drain_chunk_frames(8_000), 256);
    }

    // ── Target classification / device metadata ──────────────────────

    #[test]
    fn broadcast_target_accepts_system_default_and_own_id() {
        assert!(ensure_broadcast_target(&CaptureTarget::SystemDefault).is_ok());
        for id in [BROADCAST_DEVICE_ID, "REPLAYKIT-BROADCAST"] {
            assert!(
                ensure_broadcast_target(&CaptureTarget::Device(DeviceId(id.to_string()))).is_ok()
            );
        }
    }

    #[test]
    fn broadcast_target_rejects_other_devices_and_per_app() {
        match ensure_broadcast_target(&CaptureTarget::Device(DeviceId("default".into()))) {
            Err(AudioError::DeviceNotFound { device_id }) => assert_eq!(device_id, "default"),
            other => panic!("expected DeviceNotFound, got {other:?}"),
        }
        let per_app = [
            CaptureTarget::Application(ApplicationId("1".into())),
            CaptureTarget::ApplicationByName("Safari".into()),
            CaptureTarget::ProcessTree(ProcessId(1)),
        ];
        for target in per_app {
            match ensure_broadcast_target(&target) {
                Err(AudioError::PlatformNotSupported { feature, platform }) => {
                    assert_eq!(platform, "ios");
                    assert!(feature.contains("permanent"), "{feature}");
                }
                other => panic!("expected PlatformNotSupported for {target:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn broadcast_device_metadata_is_honest() {
        let device = BroadcastAudioDevice::new();
        assert_eq!(device.id(), DeviceId(BROADCAST_DEVICE_ID.to_string()));
        assert!(device.name().contains("ReplayKit"));
        assert_eq!(device.kind().unwrap(), DeviceKind::Output);
        assert!(device.is_default());
        // Formats are negotiated from the ring header at stream creation —
        // the advertised list is empty by design (Linux/PipeWire precedent).
        assert!(device.supported_formats().is_empty());
    }
}

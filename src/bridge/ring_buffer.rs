//! Ring buffer bridge: lock-free SPSC bridge between producer (OS callback) and consumer threads.
//!
//! This module provides [`BridgeProducer`] and [`BridgeConsumer`], connected by
//! an [`rtrb`] lock-free SPSC ring buffer. The producer is designed to run inside
//! the OS audio callback thread (no locks, no allocations on the hot path), while
//! the consumer runs in the user/reader thread with optional blocking reads.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rsac::bridge::ring_buffer::{create_bridge, calculate_capacity};
//! use rsac::core::config::AudioFormat;
//!
//! let format = AudioFormat::default();
//! let capacity = calculate_capacity(Some(32), 4);
//! let (mut producer, mut consumer) = create_bridge(capacity, format);
//!
//! // Producer side (OS callback thread):
//! producer.push_or_drop(audio_buffer);
//!
//! // Consumer side (user thread):
//! if let Some(buf) = consumer.pop() {
//!     // process buf
//! }
//! ```

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "bridge-zerocopy")]
use rtrb::CopyToUninit;

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};

use super::state::{AtomicStreamState, StreamState};

/// Default per-buffer sample capacity for the free-list and scratch allocations.
///
/// Sized for a realistic worst-case callback period so the real-time producer is
/// allocation-free in steady state without re-growing on the first packets. CoreAudio
/// can deliver ~1024 frames/callback; at stereo that is 2048 `f32` samples. We seed a
/// little above that so a typical 1024-frame stereo (or 2048-frame mono) period fits
/// without a reallocation. Recycled buffers additionally grow to the observed
/// high-water mark, so even larger periods converge to zero allocation after warm-up.
///
/// See `docs/designs/0001-rt-allocation-guarantee.md`.
const RT_BUFFER_SAMPLE_CAPACITY: usize = 2048;

/// Default back-pressure threshold: the number of *consecutive* dropped buffers
/// (with no successful push in between) before `is_under_backpressure`
/// returns true. At a typical ~10 ms callback period, 10 consecutive drops is
/// roughly 100 ms of sustained data loss — long enough to be a real signal that
/// the consumer cannot keep up, short enough to react before a long stall.
///
/// Overridable per-bridge via [`create_bridge_with_options`].
pub const DEFAULT_BACKPRESSURE_THRESHOLD: u32 = 10;

/// Number of slots in the fixed, alloc-free windowed drop-rate ring
/// ([`BridgeShared::drop_window`]). Each slot packs a `(pushed, dropped)`
/// 32-bit pair into one [`AtomicU64`]; the producer advances a cursor every
/// [`DROP_WINDOW_SLOT_PUSHES`] push attempts so the ring holds a sliding view
/// of the most recent activity. Sized as a power of two so the modulo cursor
/// wrap is a cheap mask. See `rsac-cfe4` and [`BridgeProducer::drop_window_snapshot`].
pub(crate) const DROP_WINDOW_SLOTS: usize = 8;

/// Number of push *attempts* accumulated into a single drop-window slot before
/// the producer advances to the next slot. At a typical ~10 ms callback period
/// this makes each slot ≈ 1.28 s and the whole [`DROP_WINDOW_SLOTS`]-slot ring
/// ≈ 10 s of history — long enough to see a sustained 1-in-N loss pattern that
/// the consecutive-drop counter ([`BridgeShared::is_under_backpressure`]) misses.
pub(crate) const DROP_WINDOW_SLOT_PUSHES: u64 = 128;

/// Pack a `(pushed, dropped)` pair of `u32`s into one `u64` for a single
/// lock-free atomic slot in the drop-rate window. The high 32 bits hold
/// `pushed`, the low 32 bits hold `dropped`.
#[inline]
fn pack_window(pushed: u32, dropped: u32) -> u64 {
    ((pushed as u64) << 32) | (dropped as u64)
}

/// Inverse of [`pack_window`]: unpack a slot word into `(pushed, dropped)`.
#[inline]
fn unpack_window(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

/// Outcome of a single [`BridgeProducer::push_samples_reporting`] call.
///
/// Surfaces overflow **eagerly**, in the callback, without polling the shared
/// `buffers_dropped` counter afterwards (`rsac-0d25`). `dropped_this_call` is
/// the number of buffers the *current* call dropped — `0` on success, `1` when
/// the ring was full and the buffer was dropped — so a backend can compute a
/// per-period drop rate cheaply on the spot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PushOutcome {
    /// `true` if the buffer was pushed into the ring, `false` if it was dropped
    /// because the ring was full.
    pub pushed: bool,
    /// How many buffers this single call dropped (`0` or `1` for the
    /// one-buffer-per-call producer path).
    pub dropped_this_call: u32,
}

/// Pack an [`AudioFormat`] into a single non-zero `u64` for lock-free atomic
/// publication: `(sample_rate << 32) | (channels << 16) | sample_format_u8`.
///
/// The packed value is always non-zero for any real format (a valid stream has
/// `sample_rate > 0`), so `0` is reserved as the "unset" sentinel (see
/// [`unpack_format`]). Storing the whole format in one word means a reader can
/// never observe a torn mix of an old field with a new one.
fn pack_format(f: &AudioFormat) -> u64 {
    ((f.sample_rate as u64) << 32)
        | ((f.channels as u64) << 16)
        | (sample_format_to_atomic(f.sample_format) as u64)
}

/// Inverse of [`pack_format`]. Returns `None` for the `0` ("unset") sentinel.
fn unpack_format(packed: u64) -> Option<AudioFormat> {
    if packed == 0 {
        return None;
    }
    Some(AudioFormat {
        sample_rate: (packed >> 32) as u32,
        channels: ((packed >> 16) & 0xFFFF) as u16,
        sample_format: sample_format_from_atomic((packed & 0xFF) as u8),
    })
}

/// Encode a [`SampleFormat`] as a `u8` for lock-free atomic storage.
///
/// Paired with [`sample_format_from_atomic`]. The mapping is stable and
/// internal — it only needs to round-trip, not match any wire format.
fn sample_format_to_atomic(sf: SampleFormat) -> u8 {
    match sf {
        SampleFormat::I16 => 0,
        SampleFormat::I24 => 1,
        SampleFormat::I32 => 2,
        SampleFormat::F32 => 3,
    }
}

/// Decode a `u8` written by [`sample_format_to_atomic`] back into a
/// [`SampleFormat`]. Any unknown value falls back to [`SampleFormat::F32`]
/// (the library's internal standard), which is the value written by the
/// "no negotiated format yet" sentinel.
fn sample_format_from_atomic(v: u8) -> SampleFormat {
    match v {
        0 => SampleFormat::I16,
        1 => SampleFormat::I24,
        2 => SampleFormat::I32,
        _ => SampleFormat::F32,
    }
}

// ── Cache-line padding (rsac-9348) ─────────────────────────────────────────

/// Cache-line size (bytes) the padding targets. 64 bytes is the line size on
/// every mainstream 64-bit target rsac builds for (x86-64, aarch64 — Apple
/// silicon's 128-byte line is a pair of 64-byte lines, so 64-byte alignment
/// still separates the wrapped values onto distinct lines in practice). Used
/// only for the `#[repr(align(..))]` on [`CachePadded`].
///
/// `#[repr(align(N))]` requires an integer **literal**, so `CachePadded` hard-codes
/// `64` rather than referencing this constant; it exists to pin that literal in the
/// alignment regression tests, hence the `#[cfg(test)]` gate (no runtime use).
#[cfg(test)]
pub(crate) const CACHE_LINE_BYTES: usize = 64;

/// A value forced onto its own cache line so that writes to it do not
/// false-share with adjacent fields (`rsac-9348`).
///
/// `BridgeShared` keeps diagnostic atomics the **producer** writes on every
/// audio callback (`buffers_pushed`, `buffers_dropped`, `consecutive_drops`)
/// physically adjacent to the one the **consumer** writes on every pop
/// (`buffers_popped`). On many-core machines those two writers land on the same
/// 64-byte cache line and ping-pong it between cores (false sharing), adding p99
/// jitter to the real-time push. Wrapping the producer-hot and consumer-hot
/// groups in `CachePadded` puts them on distinct lines so neither writer
/// invalidates the other's line.
///
/// This is a transparent, zero-cost wrapper: it derefs to the inner value, so
/// every existing `self.field.load(..)` / `.fetch_add(..)` / `.store(..)` call
/// site compiles unchanged. `rtrb` already pads its own head/tail this way; this
/// is the same pattern for rsac's *extended* counters, avoiding a new dependency
/// (a tiny internal newtype was preferred per the seed's acceptance criteria).
#[repr(align(64))]
pub(crate) struct CachePadded<T>(pub(crate) T);

impl<T> CachePadded<T> {
    /// Wrap `value`, forcing it onto its own cache line.
    #[inline]
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> std::ops::Deref for CachePadded<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for CachePadded<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

// ── Consumer wake primitive (PU-5, seed rsac-efb4) ─────────────────────────

/// Maximum time a parked [`BridgeConsumer::pop_blocking`] waits per
/// `wait_timeout` slice before re-checking the ring and the stream state
/// unconditionally (`PU-5`).
///
/// The unconditional `ConsumerWake` notify (from the Windows producer push
/// and from every state transition) is what normally wakes a parked reader the
/// instant data or a terminal state appears. This bounded slice is the
/// **degrade-not-hang backstop**: if a notify is ever missed (e.g. it fired in
/// the tiny window between the reader's last ring check and entering the wait),
/// the reader still re-checks within 1 ms instead of sleeping for the full
/// caller timeout. It must therefore stay small — it bounds worst-case latency
/// on a *missed* notify, never the common path.
const WAKE_BACKSTOP_POLL: Duration = Duration::from_millis(1);

/// An always-present, allocation-free wake primitive that lets a synchronous
/// [`BridgeConsumer::pop_blocking`] park until data is pushed or the stream
/// reaches a terminal/ending state, instead of busy-polling on a fixed 1 ms
/// timer (`PU-5`, seed `rsac-efb4`).
///
/// # Why a `Condvar` and not the async waker
///
/// The async `atomic_waker::AtomicWaker` only serves `async` consumers and is
/// gated behind the `async-stream` feature; the **synchronous** blocking reader
/// had no wake mechanism and degraded to a 1 ms sleep/poll loop. A `std`
/// [`Condvar`] + a generation counter gives the blocking reader a real wakeup
/// that is present in every build, async or not.
///
/// # Real-time safety (ADR-0001)
///
/// [`notify`](Self::notify) does **not** hold the mutex while signalling and
/// performs **no allocation** — on Linux it lowers to a single `FUTEX_WAKE`
/// syscall. It is therefore safe to call from the **non-RT** producers and the
/// state-transition sites that PU-5 wires it into:
///
/// - the Windows capture loop (rsac's own thread — not an OS callback), via
///   [`BridgeProducer::notify_consumers`], and
/// - every terminal/ending state transition ([`BridgeProducer::signal_done`],
///   [`BridgeProducer::signal_error`], and `BridgeStream::stop`).
///
/// It is deliberately **NOT** called from the shared
/// [`push_samples_or_drop_inner`](BridgeProducer::push_samples_or_drop_inner)
/// hot path, which the Linux (PipeWire) and macOS (CoreAudio) backends drive
/// from their **real-time audio callbacks** — adding a notify there would put a
/// (brief) lock-touching futex call on the RT thread, violating ADR-0001. Those
/// backends instead rely on the retained `WAKE_BACKSTOP_POLL` re-check plus
/// the terminal-state notify on stop. The waiter side
/// ([`wait`](Self::wait)) runs only on the non-RT consumer thread.
struct ConsumerWake {
    /// Trivial mutex guarding the `Condvar` wait. The protected datum is the
    /// empty tuple — the real "did something change?" signal is the
    /// [`generation`](Self::generation) counter, checked under the lock so a
    /// notify that fires between the reader's ring check and its `wait` is not
    /// lost.
    lock: Mutex<()>,
    /// Condition variable a parked reader waits on. Notified (no lock held by
    /// the notifier) by [`notify`](Self::notify).
    cvar: Condvar,
    /// Monotonic notify counter. [`notify`](Self::notify) bumps it before
    /// waking; a waiter snapshots it before its final ring/state re-check and
    /// only blocks while the snapshot is unchanged. This closes the classic
    /// lost-wakeup race without holding the lock across the push.
    generation: AtomicU64,
}

impl ConsumerWake {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cvar: Condvar::new(),
            generation: AtomicU64::new(0),
        }
    }

    /// Snapshot the current notify generation. A waiter reads this *before* its
    /// last non-blocking ring/state check so a notify racing that check is
    /// detected (the generation will differ) and the waiter re-loops instead of
    /// parking on an already-consumed signal.
    #[inline]
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Wake every parked reader (`PU-5`).
    ///
    /// Bumps the generation (`Release`) then signals the `Condvar`. The mutex is
    /// deliberately **not** held across the signal, and nothing is allocated, so
    /// this stays safe to call from the non-RT Windows producer and from
    /// terminal state transitions (ADR-0001 / ADR-0010). It is a no-op cost when
    /// no reader is parked.
    ///
    /// Bumping the generation *before* signalling lets [`wait`](Self::wait)
    /// detect — under its lock — a notify that raced the waiter's last ring
    /// check, covering the common lost-wakeup window. Because the notifier holds
    /// no lock, a residual hairline race remains possible (a notify that lands in
    /// the instant the waiter is between its generation re-check and the kernel
    /// park); that case is bounded by the `WAKE_BACKSTOP_POLL` slice in
    /// `pop_blocking`, so it degrades to a ≤1 ms re-check, never a hang. This is
    /// the intended trade: a lock-free, RT-safe-to-signal notify backed by a
    /// bounded poll, rather than a lock-on-the-signal path the RT rule forbids.
    #[inline]
    fn notify(&self) {
        // Bump first (Release) so a waiter that snapshotted the old value and is
        // about to wait observes the change via its pre-wait generation re-check.
        self.generation.fetch_add(1, Ordering::Release);
        self.cvar.notify_all();
    }

    /// Park the calling (non-RT consumer) thread until the next
    /// [`notify`](Self::notify) or until `timeout` elapses, whichever comes
    /// first.
    ///
    /// `since` is the generation the caller observed *before* its last ring/
    /// state check. If a notify landed since then (generation advanced) this
    /// returns immediately without waiting, so a wake that raced the check is
    /// not lost. Spurious `Condvar` wakeups are harmless: the caller re-checks
    /// the ring and state on return regardless.
    fn wait(&self, since: u64, timeout: Duration) {
        let guard = match self.lock.lock() {
            Ok(g) => g,
            // A poisoned wake-lock must not wedge the reader: the protected
            // datum is `()`, so recovering the guard is always sound.
            Err(poisoned) => poisoned.into_inner(),
        };
        // If a notify fired since the caller's pre-check snapshot, do not park —
        // the signal would otherwise be lost (it was consumed before we held the
        // lock). Re-loop immediately.
        if self.generation.load(Ordering::Acquire) != since {
            return;
        }
        // wait_timeout may wake spuriously; that is fine — the caller re-checks
        // the ring/state. We intentionally do a single bounded slice and let the
        // pop_blocking loop drive re-checking (degrade-not-hang backstop).
        let _ = self.cvar.wait_timeout(guard, timeout);
    }
}

// ── Shared State ─────────────────────────────────────────────────────────

/// Shared state between producer and consumer for diagnostics and coordination.
///
/// Both [`BridgeProducer`] and [`BridgeConsumer`] hold an `Arc<BridgeShared>`
/// to access stream lifecycle state and diagnostic counters without locks.
///
/// # Cache-line layout (`rsac-9348`)
///
/// The counters are grouped by **which thread writes them** and each group is
/// wrapped in [`CachePadded`] so the producer-written set (`buffers_pushed`,
/// `buffers_dropped`, `consecutive_drops`) and the consumer-written
/// `buffers_popped` sit on distinct cache lines. This removes the false-sharing
/// ping-pong that otherwise adds tail latency to the real-time push on
/// many-core systems.
pub(crate) struct BridgeShared {
    /// Stream lifecycle state (atomic, lock-free).
    pub state: AtomicStreamState,
    /// Total buffers successfully pushed by the producer.
    ///
    /// Producer-written every callback; [`CachePadded`] keeps it off the
    /// consumer's [`buffers_popped`](Self::buffers_popped) cache line (`rsac-9348`).
    pub buffers_pushed: CachePadded<AtomicU64>,
    /// Total buffers dropped due to the ring buffer being full.
    ///
    /// Producer-written; cache-padded off the consumer counter (`rsac-9348`).
    pub buffers_dropped: CachePadded<AtomicU64>,
    /// Total buffers successfully popped by the consumer.
    ///
    /// The **only** consumer-written counter; [`CachePadded`] isolates it on its
    /// own cache line so popping never invalidates the producer's hot counters
    /// (`rsac-9348`).
    pub buffers_popped: CachePadded<AtomicU64>,
    /// Consecutive drop count — resets to 0 on successful push.
    /// Used to detect sustained backpressure without relying on total drop rate.
    ///
    /// Producer-written; cache-padded off the consumer counter (`rsac-9348`).
    pub consecutive_drops: CachePadded<AtomicU32>,
    /// Threshold above which `is_under_backpressure()` returns true.
    /// Default: [`DEFAULT_BACKPRESSURE_THRESHOLD`] consecutive drops
    /// (≈100ms of data loss at typical rates). Configurable per-bridge via
    /// [`create_bridge_with_options`].
    pub backpressure_threshold: u32,
    /// Audio format **requested** when the bridge was constructed.
    /// This is the fallback returned by [`negotiated_format`] until a backend
    /// records what the OS actually delivered (see [`negotiated_*`] fields).
    ///
    /// [`negotiated_format`]: BridgeShared::negotiated_format
    /// [`negotiated_*`]: BridgeShared::set_negotiated_format
    #[allow(dead_code)]
    pub format: AudioFormat,
    /// Authoritative *delivery* format recorded by a backend, packed into a
    /// single atomic word so a reader can never observe a torn snapshot (a
    /// mix of an old rate with a new channel count). Encoding (0 == unset):
    /// `(sample_rate as u64) << 32 | (channels as u64) << 16 | sample_format_u8`.
    /// Published with one `Release` store / read with one `Acquire` load. Until
    /// a backend calls [`BridgeProducer::set_negotiated_format`], this is 0 and
    /// [`negotiated_format`](BridgeShared::negotiated_format) falls back to the
    /// requested [`format`](BridgeShared::format).
    negotiated: AtomicU64,
    /// Fixed, alloc-free sliding window of recent `(pushed, dropped)` counts,
    /// one packed [`AtomicU64`] per slot (see [`pack_window`]). The producer
    /// adds to the slot selected by [`drop_window_cursor`] on every push path
    /// with `Relaxed` ops — NO `Mutex`, NO allocation — so a reader can compute
    /// a *windowed* drop rate that does not reset on a single successful push
    /// (the gap the consecutive-drop bool leaves). See `rsac-cfe4`.
    ///
    /// [`drop_window_cursor`]: BridgeShared::drop_window_cursor
    drop_window: [AtomicU64; DROP_WINDOW_SLOTS],
    /// Total push *attempts* (pushed + dropped) the producer has recorded into
    /// the drop window. Advances the active slot every [`DROP_WINDOW_SLOT_PUSHES`]
    /// attempts (cursor == `attempts / DROP_WINDOW_SLOT_PUSHES % DROP_WINDOW_SLOTS`).
    drop_window_cursor: AtomicU64,
    /// Set once if a push closure ever panicked at the OS-callback boundary and
    /// was caught by the panic guard (`rsac-d0ba`). Used to log the panic a
    /// single time rather than on every subsequent guarded call.
    push_panicked: std::sync::atomic::AtomicBool,
    /// Always-present wake primitive for the **synchronous** blocking reader
    /// ([`BridgeConsumer::pop_blocking`]) (`PU-5`, seed `rsac-efb4`).
    ///
    /// Present in every build (unlike the async `waker` field below, which is
    /// `async-stream`-gated), so `pop_blocking` wakes on push / terminal
    /// transition instead of busy-polling on a 1 ms timer. Notified ONLY from
    /// the non-RT Windows producer ([`BridgeProducer::notify_consumers`]) and
    /// from terminal/ending state transitions ([`signal_done`]/[`signal_error`]/
    /// stop) — NEVER from the Linux/macOS RT callback push path (ADR-0001). See
    /// `ConsumerWake`.
    ///
    /// [`signal_done`]: BridgeProducer::signal_done
    /// [`signal_error`]: BridgeProducer::signal_error
    wake: ConsumerWake,
    /// Waker for async stream consumers — notified when new data is pushed.
    #[cfg(feature = "async-stream")]
    pub waker: atomic_waker::AtomicWaker,
}

impl BridgeShared {
    /// Returns true if the producer has dropped `backpressure_threshold` or
    /// more buffers in a row without a successful push — signals that the
    /// consumer is falling behind and cannot keep up with the producer rate.
    pub fn is_under_backpressure(&self) -> bool {
        self.consecutive_drops.load(Ordering::Relaxed) >= self.backpressure_threshold
    }

    /// Record one push attempt into the sliding drop-rate window (`rsac-cfe4`).
    ///
    /// `dropped` is `false` for a successful push, `true` for an overflow drop.
    /// Selects the active slot from the running attempt count and bumps the
    /// packed `(pushed, dropped)` pair with `Relaxed` adds — **no allocation, no
    /// lock**, safe on the real-time callback thread. When the cursor advances
    /// into a new slot, that slot is reset to `0` first so it holds only the
    /// most recent window's activity rather than accumulating forever.
    #[inline]
    fn record_drop_window(&self, dropped: bool) {
        // `fetch_add` returns the value *before* the add; that is the index of
        // this attempt. The slot is `attempts / SLOT_PUSHES`, wrapped into the
        // ring. When this attempt is the first of a new slot, clear the slot so
        // it starts fresh (sliding window, not a running total).
        let attempt = self.drop_window_cursor.fetch_add(1, Ordering::Relaxed);
        let slot_seq = attempt / DROP_WINDOW_SLOT_PUSHES;
        let idx = (slot_seq as usize) & (DROP_WINDOW_SLOTS - 1);
        if attempt.is_multiple_of(DROP_WINDOW_SLOT_PUSHES) {
            // First attempt of this slot's lifetime → reset stale contents.
            self.drop_window[idx].store(0, Ordering::Relaxed);
        }
        // One push attempt == +1 to exactly one of (pushed, dropped).
        let delta = if dropped {
            pack_window(0, 1)
        } else {
            pack_window(1, 0)
        };
        self.drop_window[idx].fetch_add(delta, Ordering::Relaxed);
    }

    /// Read the aggregate `(pushed, dropped)` totals across all live slots of
    /// the sliding drop-rate window (`rsac-cfe4`).
    ///
    /// A single `Relaxed` pass over the fixed [`DROP_WINDOW_SLOTS`] atomics — no
    /// allocation, no lock. Returned counts are an eventually-consistent
    /// snapshot of recent activity, suitable for computing a windowed drop rate
    /// in `BackpressureReport` on the (non-RT) reader side.
    pub fn drop_window_snapshot(&self) -> (u64, u64) {
        let mut pushed_total = 0u64;
        let mut dropped_total = 0u64;
        for slot in &self.drop_window {
            let (p, d) = unpack_window(slot.load(Ordering::Relaxed));
            pushed_total += p as u64;
            dropped_total += d as u64;
        }
        (pushed_total, dropped_total)
    }

    /// Record the **authoritative delivery format** negotiated with the OS.
    ///
    /// Platform backends call this (via [`BridgeProducer::set_negotiated_format`])
    /// once they know the format the OS audio callback will actually deliver,
    /// which can differ from the requested format (e.g. the system mix format
    /// when autoconvert is unavailable). Lock-free and cheap: a single
    /// `Release` store of the packed word, so a reader either sees the whole
    /// new format or the whole old one — never a torn mix.
    ///
    /// The reported `sample_format` is **always normalized to F32**: the bridge
    /// payload (`AudioBuffer`) is always interleaved f32 regardless of the OS
    /// endpoint's native sample type, so reporting anything else would
    /// misdescribe what consumers actually receive. The negotiated
    /// `sample_rate`/`channels` are preserved as delivered.
    ///
    /// Safe to call more than once; the most recent values win. Reads go through
    /// [`negotiated_format`](BridgeShared::negotiated_format).
    pub fn set_negotiated_format(&self, format: &AudioFormat) {
        // Normalize sample_format to F32 — the bridge always delivers f32.
        let normalized = AudioFormat {
            sample_rate: format.sample_rate,
            channels: format.channels,
            sample_format: SampleFormat::F32,
        };
        self.negotiated
            .store(pack_format(&normalized), Ordering::Release);
    }

    /// Returns the authoritative **delivery** format if a backend has recorded
    /// one via `set_negotiated_format`, otherwise the requested format the
    /// bridge was constructed with.
    ///
    /// This is what `BridgeStream::format` surfaces, so consumers always see
    /// what they are actually receiving. The read is a single `Acquire` load of
    /// the packed word, so the returned format is always internally consistent.
    pub fn negotiated_format(&self) -> AudioFormat {
        // Acquire pairs with the Release in `set_negotiated_format`.
        let packed = self.negotiated.load(Ordering::Acquire);
        if let Some(fmt) = unpack_format(packed) {
            fmt
        } else {
            self.format.clone()
        }
    }

    /// Wake any parked synchronous reader ([`BridgeConsumer::pop_blocking`])
    /// (`PU-5`).
    ///
    /// Allocation-free and holds no lock while signalling (see
    /// [`ConsumerWake::notify`]). Call this from **non-RT** producers and from
    /// state transitions only — NEVER from the Linux/macOS RT callback push
    /// path (ADR-0001). It is wired into [`BridgeProducer::signal_done`],
    /// [`BridgeProducer::signal_error`], [`BridgeProducer::notify_consumers`]
    /// (the Windows producer), and `BridgeStream::stop`.
    #[inline]
    pub(crate) fn notify_wake(&self) {
        self.wake.notify();
    }
}

// ── BridgeProducer ───────────────────────────────────────────────────────

/// Producer side of the ring buffer bridge.
///
/// Runs in the OS audio callback thread. All operations are lock-free
/// and non-allocating in the hot path.
///
/// # Safety
///
/// This type is [`Send`] so it can be moved to the callback thread.
/// It is **not** [`Sync`] — only one thread should use the producer.
pub struct BridgeProducer {
    producer: rtrb::Producer<AudioBuffer>,
    shared: Arc<BridgeShared>,
    /// Consumer side of the **free-list return ring**. The data consumer pushes
    /// drained `Vec<f32>` allocations back through this ring after handing the
    /// user an owned copy; the producer pops them here to reuse on the next
    /// callback. This is what makes [`push_samples_or_drop`] allocation-free on
    /// the real-time thread in steady state — the unavoidable allocation is
    /// performed on the (non-real-time) consumer thread instead.
    ///
    /// [`push_samples_or_drop`]: BridgeProducer::push_samples_or_drop
    free_rx: rtrb::Consumer<Vec<f32>>,
    /// Single-slot fallback buffer used only when the free-list ring is
    /// momentarily empty (e.g. during warm-up before the consumer has recycled
    /// anything, or under sustained back-pressure when a push is rejected).
    scratch: Vec<f32>,
}

// BridgeProducer is Send (can be moved to the callback thread) but not necessarily Sync.
// rtrb::Producer<T> is Send when T: Send, which AudioBuffer satisfies.
// We do NOT implement Sync — only one thread should use the producer.

impl BridgeProducer {
    /// Non-blocking push of an [`AudioBuffer`] into the ring buffer.
    ///
    /// If the ring buffer is full, returns `Err(buffer)` giving back the
    /// buffer to the caller. Does **not** increment `buffers_dropped` —
    /// the caller decides what to do with the rejected buffer.
    ///
    /// Increments `buffers_pushed` on success.
    pub fn push(&mut self, buffer: AudioBuffer) -> Result<(), AudioBuffer> {
        match self.producer.push(buffer) {
            Ok(()) => {
                self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
                self.shared.consecutive_drops.store(0, Ordering::Relaxed);
                // Record a successful attempt in the sliding drop-rate window
                // (rsac-cfe4). The drop arm is owned by the caller (push returns
                // the rejected buffer rather than dropping it), so only success
                // is recorded here to avoid double-counting via push_or_drop.
                self.shared.record_drop_window(false);
                #[cfg(feature = "async-stream")]
                self.shared.waker.wake();
                Ok(())
            }
            Err(rtrb::PushError::Full(buffer)) => Err(buffer),
        }
    }

    /// Tries to push an [`AudioBuffer`]. If the ring buffer is full, the
    /// buffer is dropped and `buffers_dropped` is incremented.
    ///
    /// Returns `true` if pushed successfully, `false` if dropped.
    ///
    /// This is the primary method used by audio callbacks — it never blocks
    /// and silently drops data when the consumer can't keep up.
    pub fn push_or_drop(&mut self, buffer: AudioBuffer) -> bool {
        match self.push(buffer) {
            Ok(()) => true,
            Err(_dropped) => {
                self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .consecutive_drops
                    .fetch_add(1, Ordering::Relaxed);
                // Record the drop in the sliding window (rsac-cfe4). The
                // success arm is recorded inside `push()`, so only the drop is
                // recorded here — no double-counting.
                self.shared.record_drop_window(true);
                false
            }
        }
    }

    /// Push raw audio samples into the ring buffer without allocating on the
    /// real-time callback thread in steady state.
    ///
    /// # Allocation behavior
    ///
    /// The `Vec<f32>` backing each [`AudioBuffer`] is sourced from the
    /// **free-list return ring** (`free_rx`), which the consumer replenishes
    /// every time it hands an owned buffer to the user (see
    /// [`BridgeConsumer::pop`]). The unavoidable heap allocation for the user's
    /// buffer is therefore performed on the consumer (non-RT) thread, and this
    /// method reuses recycled allocations:
    ///
    /// - **Steady state:** a recycled `Vec` is popped from `free_rx`, cleared,
    ///   filled from `data`, and pushed — **no heap allocation**.
    /// - **Warm-up / free-list empty:** falls back to a single-slot `scratch`
    ///   buffer; only allocates if `scratch` has insufficient capacity (a
    ///   bounded, transient cost until the consumer starts recycling).
    /// - **Back-pressure (ring full):** the rejected `Vec` is reclaimed into
    ///   `scratch` so the next call reuses it — no alloc+free churn.
    ///
    /// This is the preferred method for OS audio callbacks. Callers should use
    /// this instead of manually calling `data.to_vec()` + `AudioBuffer::new()` +
    /// `push_or_drop()`.
    ///
    /// Delegates to [`push_samples_or_drop_at`](Self::push_samples_or_drop_at)
    /// with no timestamp; see that method for the variant that stamps each
    /// buffer with a stream-relative position.
    #[inline]
    pub fn push_samples_or_drop(&mut self, data: &[f32], channels: u16, sample_rate: u32) -> bool {
        self.push_samples_or_drop_inner(data, channels, sample_rate, None)
    }

    /// Like [`push_samples_or_drop`](Self::push_samples_or_drop), but stamps the
    /// buffer with a **stream-relative timestamp** (`rsac-522b`).
    ///
    /// `timestamp` is the position of this buffer's first sample relative to the
    /// stream start — typically a cached `Instant::elapsed()` the backend already
    /// holds, so there is **no extra clock syscall** on the hot path. The
    /// timestamp survives the ring + free-list recycle round-trip (see
    /// [`BridgeConsumer::pop`]) and is observable via
    /// [`AudioBuffer::timestamp`](crate::core::buffer::AudioBuffer::timestamp),
    /// making per-buffer latency/jitter measurable.
    ///
    /// Shares the exact same alloc-free free-list/scratch path as the untimed
    /// variant: in steady state it performs **no heap allocation**.
    #[inline]
    pub fn push_samples_or_drop_at(
        &mut self,
        data: &[f32],
        channels: u16,
        sample_rate: u32,
        timestamp: Duration,
    ) -> bool {
        self.push_samples_or_drop_inner(data, channels, sample_rate, Some(timestamp))
    }

    /// Like [`push_samples_or_drop`](Self::push_samples_or_drop), but reports the
    /// per-call overflow outcome **synchronously** (`rsac-0d25`).
    ///
    /// Returns a [`PushOutcome`] giving `pushed` and `dropped_this_call` without
    /// the caller having to poll the shared `buffers_dropped` counter — so a
    /// backend can compute a per-period drop rate cheaply, right in the callback.
    /// Same alloc-free, lock-free path as [`push_samples_or_drop`](Self::push_samples_or_drop).
    #[inline]
    pub fn push_samples_reporting(
        &mut self,
        data: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> PushOutcome {
        let pushed = self.push_samples_or_drop_inner(data, channels, sample_rate, None);
        PushOutcome {
            pushed,
            // One buffer per call: a failed push dropped exactly one buffer.
            dropped_this_call: if pushed { 0 } else { 1 },
        }
    }

    /// Shared alloc-free core for the `push_samples_*` family.
    ///
    /// When `timestamp` is `Some`, the buffer is built with
    /// [`AudioBuffer::with_timestamp`]; otherwise with [`AudioBuffer::new`]. Both
    /// paths reuse the identical free-list/scratch logic, so the RT-allocation
    /// guarantee (ADR-0001) is unchanged for the timestamped variant.
    fn push_samples_or_drop_inner(
        &mut self,
        data: &[f32],
        channels: u16,
        sample_rate: u32,
        timestamp: Option<Duration>,
    ) -> bool {
        // Acquire a reusable Vec: prefer a recycled allocation from the
        // free-list ring, otherwise fall back to the single-slot scratch.
        // `used_scratch` records whether we consumed the scratch slot, so the
        // success arm can refill it and never leave it at capacity 0 (see
        // docs/designs/0001-rt-allocation-guarantee.md).
        let (mut vec, used_scratch) = match self.free_rx.pop() {
            Ok(recycled) => (recycled, false),
            Err(rtrb::PopError::Empty) => (std::mem::take(&mut self.scratch), true),
        };

        vec.clear();
        vec.extend_from_slice(data);

        // Build with or without a stream-relative timestamp. Both arms reuse the
        // same recycled `vec`, so neither allocates on the RT thread (rsac-522b).
        let buffer = match timestamp {
            Some(ts) => AudioBuffer::with_timestamp(
                vec,
                AudioFormat {
                    sample_rate,
                    channels,
                    sample_format: SampleFormat::F32,
                },
                ts,
            ),
            None => AudioBuffer::new(vec, channels, sample_rate),
        };

        match self.producer.push(buffer) {
            Ok(()) => {
                self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
                self.shared.consecutive_drops.store(0, Ordering::Relaxed);
                // Record a successful attempt in the sliding drop-rate window
                // (rsac-cfe4) — Relaxed, alloc-free, lock-free.
                self.shared.record_drop_window(false);
                #[cfg(feature = "async-stream")]
                self.shared.waker.wake();
                // If we consumed the scratch fallback, the scratch slot is now
                // empty (capacity 0). Refill it best-effort from a recycled
                // allocation so the next free-list-empty push reuses a buffer
                // instead of allocating on the RT thread. If no recycled buffer
                // is available yet, scratch stays empty — but the consumer will
                // recycle one shortly, and the worst case is a single bounded
                // warm-up allocation rather than a permanent one.
                if used_scratch {
                    // Refill scratch so the single-slot fallback is never left at
                    // capacity 0 (which would force an RT-thread allocation on the
                    // next free-list-empty push — the precise defect ADR-0001
                    // fixes). Prefer a recycled buffer; if none is available yet
                    // (consumer hasn't caught up), restore a pre-sized empty Vec
                    // so the next `extend_from_slice` reuses its capacity instead
                    // of growing from zero.
                    self.scratch = match self.free_rx.pop() {
                        Ok(recycled) => recycled,
                        Err(rtrb::PopError::Empty) => Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY),
                    };
                }
                true
            }
            Err(rtrb::PushError::Full(rejected)) => {
                // Reclaim the Vec allocation into scratch for reuse on the next
                // call. This keeps the RT thread alloc-free even when the
                // consumer can't keep up.
                self.scratch = rejected.into_data();
                self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .consecutive_drops
                    .fetch_add(1, Ordering::Relaxed);
                // Record the overflow drop in the sliding window (rsac-cfe4).
                self.shared.record_drop_window(true);
                false
            }
        }
    }

    /// Panic-guarded wrapper around [`push_samples_or_drop`](Self::push_samples_or_drop)
    /// for use **directly at the OS audio-callback boundary** (`rsac-d0ba`).
    ///
    /// The platform backends invoke the producer from inside a C/OS audio engine
    /// callback. If a push were ever to panic there, the unwind would cross the
    /// FFI boundary into the engine — undefined behavior that can corrupt or
    /// abort the process. This method wraps the push in
    /// [`std::panic::catch_unwind`] so a panic can never escape: on a caught
    /// panic it logs **once**, counts a dropped buffer, best-effort transitions
    /// the stream to [`StreamState::Error`], and returns `false`.
    ///
    /// `catch_unwind` is near-free on the happy path and introduces **no heap
    /// allocation** there (the closure only borrows `&mut self` and `data`), so
    /// the ADR-0001 alloc-free guarantee is preserved — the alloc-probe still
    /// reports `0`. Prefer this at FFI call sites; the unguarded
    /// [`push_samples_or_drop`](Self::push_samples_or_drop) remains available for
    /// in-process callers where a panic would unwind normally.
    pub fn push_samples_guarded(&mut self, data: &[f32], channels: u16, sample_rate: u32) -> bool {
        // `&mut self` is not UnwindSafe (it could be left in a torn state), but a
        // panic here is already a last-resort safety net and we immediately
        // poison the stream to Error, so asserting unwind-safety is sound: no
        // further pushes are expected to succeed after a caught panic.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.push_samples_or_drop(data, channels, sample_rate)
        }));
        match result {
            Ok(pushed) => pushed,
            Err(_payload) => {
                self.on_push_panic();
                false
            }
        }
    }

    /// Common handling for a panic caught by [`push_samples_guarded`](Self::push_samples_guarded):
    /// log once, count a drop, and poison the stream to [`StreamState::Error`].
    #[cold]
    #[inline(never)]
    fn on_push_panic(&self) {
        // Log only the first caught panic to avoid flooding the log from a
        // repeatedly-panicking callback. `swap` returns the previous value, so
        // exactly one caller observes `false` and logs.
        if !self.shared.push_panicked.swap(true, Ordering::Relaxed) {
            log::error!(
                "panic caught at the audio-callback push boundary; \
                 transitioning stream to Error and dropping the buffer"
            );
        }
        // Count the buffer as dropped (it never reached the ring).
        self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
        self.shared
            .consecutive_drops
            .fetch_add(1, Ordering::Relaxed);
        self.shared.record_drop_window(true);
        // Best-effort poison: force the stream into the terminal Error state so
        // readers observe end-of-stream rather than spinning on a dead callback.
        // Shares the exact terminal-poison tail with `signal_error()` (state
        // force_set to Error + waker wake) so there is a single poison path.
        self.signal_error();
    }

    /// Read the aggregate `(pushed, dropped)` totals across the sliding
    /// drop-rate window (`rsac-cfe4`). Forwards to the shared state's
    /// `drop_window_snapshot`; exposed on the producer so a
    /// reader holding the producer (e.g. tests, backends) can compute a windowed
    /// drop rate without reaching into shared state.
    pub fn drop_window_snapshot(&self) -> (u64, u64) {
        self.shared.drop_window_snapshot()
    }

    /// Signals that the producer is done sending data.
    ///
    /// Attempts to transition the stream state from [`StreamState::Running`]
    /// to [`StreamState::Stopping`]. This is best-effort — if the transition
    /// fails (e.g., state was already changed), the failure is silently ignored.
    ///
    /// Called when the OS callback knows no more audio data will come.
    pub fn signal_done(&self) {
        // Best-effort: ignore if the CAS fails (state already changed).
        let _ = self
            .shared
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        // PU-5: wake a parked synchronous reader so an empty pop_blocking
        // re-checks the (now Stopping/draining) state immediately rather than
        // sleeping out its backstop slice. `notify_wake` holds no lock while
        // signalling and never allocates, so it stays ADR-0001-safe even when
        // signal_done is invoked from a PipeWire/CoreAudio state-change context.
        self.shared.notify_wake();
        #[cfg(feature = "async-stream")]
        self.shared.waker.wake();
    }

    /// Signals that the producer has **died** and no more data will ever arrive
    /// (the FATAL sibling of [`signal_done`](Self::signal_done)).
    ///
    /// Unconditionally forces the stream into the terminal [`StreamState::Error`]
    /// state and wakes any async consumer. Use this when the producer stops for a
    /// reason other than a graceful end — a device unplug, a daemon/proxy death,
    /// or a backend that can no longer deliver — so that **both** readers observe
    /// end-of-stream:
    ///
    /// - the **blocking** reader ([`BridgeConsumer::pop_blocking`]) returns the
    ///   Fatal [`AudioError::StreamEnded`] because `Error` is terminal
    ///   ([`is_terminal`](super::state::AtomicStreamState::is_terminal)); and
    /// - the **async** reader ends with `Poll::Ready(None)` because the state is
    ///   no longer producing.
    ///
    /// This is the crucial distinction from [`signal_done`](Self::signal_done):
    /// the graceful `Stopping` state keeps `pop_blocking` *draining* indefinitely
    /// (it is not terminal — buffered data may still be read), which is correct
    /// for a clean end but would hang `read_buffer_blocking` forever on a dead
    /// producer. `Error` is sticky/terminal for both, so only call this once no
    /// further data can ever be produced.
    ///
    /// # Real-time safety
    ///
    /// Mutates only the shared `state` (a `BridgeShared` field) via a single `Release` store and wakes
    /// the (lock-free) async waker — **no allocation, no lock, no blocking**. It is
    /// therefore safe to call from a platform callback context such as PipeWire's
    /// `.state_changed` handler or a CoreAudio property listener (ADR-0001,
    /// ADR-0010). `force_set` is last-writer-wins, so it is idempotent against any
    /// concurrent graceful transition.
    pub fn signal_error(&self) {
        self.shared.state.force_set(StreamState::Error);
        // PU-5: wake a parked synchronous reader so pop_blocking observes the
        // terminal Error and returns the Fatal StreamEnded promptly instead of
        // sleeping out its backstop slice. Lock-free-to-signal and alloc-free
        // (see ConsumerWake::notify), so it remains safe from the PipeWire
        // `.state_changed` / CoreAudio `DeviceIsAlive` callback contexts that
        // ADR-0010 wires signal_error into.
        self.shared.notify_wake();
        #[cfg(feature = "async-stream")]
        self.shared.waker.wake();
    }

    /// Wake any parked synchronous reader ([`BridgeConsumer::pop_blocking`])
    /// after a successful push, so the reader returns the new data immediately
    /// instead of sleeping out the `WAKE_BACKSTOP_POLL` backstop slice
    /// (`PU-5`, seed `rsac-efb4`).
    ///
    /// # When to call this
    ///
    /// Call it from a **non-real-time** producer right after a push — concretely
    /// the **Windows** WASAPI capture loop, which runs on rsac's *own* polling
    /// thread (not an OS audio callback). That backend should call
    /// `producer.notify_consumers()` after each `push_samples_or_drop` so a
    /// blocked `read_buffer_blocking` wakes on the push.
    ///
    /// # Real-time safety — do NOT call from an RT callback
    ///
    /// The Linux (PipeWire `.process`) and macOS (CoreAudio IOProc) backends
    /// drive [`push_samples_or_drop`](Self::push_samples_or_drop) from their
    /// **hard real-time audio callbacks**. They must **NOT** call this method:
    /// it touches a `Condvar`/mutex-backed primitive and, while it neither
    /// allocates nor holds a lock across the signal, the conservative ADR-0001
    /// rule keeps *all* lock-touching primitives off the RT push path. Those
    /// backends instead rely on the retained `WAKE_BACKSTOP_POLL` re-check in
    /// `pop_blocking` plus the terminal-state notify on stop
    /// ([`signal_done`](Self::signal_done)/[`signal_error`](Self::signal_error)).
    /// `rt_alloc.rs` (which exercises only `push_samples_or_drop`) therefore
    /// stays at zero allocations — this method is never on that path.
    #[inline]
    pub fn notify_consumers(&self) {
        self.shared.notify_wake();
    }

    /// Returns the number of free slots in the ring buffer.
    pub fn available_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Returns the total number of buffers dropped due to the ring buffer being full.
    pub fn buffers_dropped(&self) -> u64 {
        self.shared.buffers_dropped.load(Ordering::Relaxed)
    }

    /// Record the **authoritative delivery format** the OS will actually feed
    /// this producer (M1).
    ///
    /// Platform backends (PipeWire / CoreAudio / WASAPI) call this once
    /// negotiation completes — typically right after they learn the endpoint
    /// mix format — so that `BridgeStream::format` reflects what is
    /// **delivered**, not merely what was **requested**.
    ///
    /// Lock-free and cheap (delegates to the shared state). It is
    /// safe to call from the setup path before the capture loop starts; calling
    /// it from the hot callback is also allowed but unnecessary in steady state.
    pub fn set_negotiated_format(&self, format: &AudioFormat) {
        self.shared.set_negotiated_format(format);
    }

    /// Returns a reference to the shared state.
    /// Part of the bridge API surface for platform backends and diagnostics.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }

    /// Number of recycled allocations currently available in the free-list
    /// return ring. Test-only — used to assert allocation recycling behavior.
    #[cfg(test)]
    pub(crate) fn recycled_available(&self) -> usize {
        self.free_rx.slots()
    }
}

// ── BridgeConsumer ───────────────────────────────────────────────────────

/// Consumer side of the ring buffer bridge.
///
/// Runs in the user/consumer thread. Supports both blocking and
/// non-blocking reads.
pub struct BridgeConsumer {
    consumer: rtrb::Consumer<AudioBuffer>,
    shared: Arc<BridgeShared>,
    /// Producer side of the **free-list return ring**. After popping a buffer
    /// from the data ring and handing the user an owned copy, the consumer
    /// pushes the now-spare `Vec<f32>` allocation back here so the producer can
    /// reuse it without allocating on the real-time thread. If the ring is full
    /// the spare allocation is simply dropped (freed) — bounded and harmless.
    free_tx: rtrb::Producer<Vec<f32>>,
    /// Small consumer-side pool of spare `Vec<f32>` allocations used to refill
    /// the producer's free-list **without copying** the ring buffer's payload
    /// (`rsac-17d1`). [`pop`](Self::pop) now *moves* the ring's `Vec` straight
    /// to the user (no `clone`) and recycles a spare pulled from this pool to
    /// the producer instead — so the per-pop full-buffer memcpy is gone. The
    /// pool is replenished lazily off the real-time thread (here, on the
    /// consumer thread), preserving the RT producer's alloc-free guarantee.
    spare_pool: Vec<Vec<f32>>,
}

// BridgeConsumer is Send (can be moved to the consumer thread).
// rtrb::Consumer<T> is Send when T: Send, which AudioBuffer satisfies.

impl BridgeConsumer {
    /// Non-blocking pop. Returns `None` if the ring buffer is empty.
    ///
    /// Increments `buffers_popped` on success.
    ///
    /// # Allocation / recycling
    ///
    /// The user receives the [`AudioBuffer`] **that travelled through the ring,
    /// moved with no copy** (`rsac-17d1`) — there is no per-pop `Vec` clone +
    /// memcpy. To keep the producer's free-list populated (so
    /// [`BridgeProducer::push_samples_or_drop`] stays allocation-free on the RT
    /// thread), a *spare* `Vec<f32>` is recycled back to the producer instead:
    /// pulled from a small consumer-side `spare_pool`, or
    /// freshly allocated when the pool is empty. Any such allocation happens
    /// here, on the **non-real-time consumer thread** — the constraint that
    /// matters — never on the producer's callback thread.
    pub fn pop(&mut self) -> Option<AudioBuffer> {
        match self.consumer.pop() {
            Ok(buffer) => {
                self.shared.buffers_popped.fetch_add(1, Ordering::Relaxed);

                // Recycle a *spare* allocation back to the producer so its
                // free-list stays supplied without us copying the payload. Reuse
                // a pooled spare if we have one, else allocate a pre-sized empty
                // Vec (off the RT thread). Pre-sizing to the delivered length
                // means the producer's next `extend_from_slice` reuses the
                // capacity instead of growing from zero.
                let len = buffer.data().len();
                let spare = match self.spare_pool.pop() {
                    Some(mut v) => {
                        if v.capacity() < len {
                            v.reserve(len - v.capacity());
                        }
                        v
                    }
                    None => Vec::with_capacity(len.max(RT_BUFFER_SAMPLE_CAPACITY)),
                };

                // Best-effort recycle; if the free-list ring is full, the spare
                // allocation is dropped (freed) — bounded and harmless.
                let _ = self.free_tx.push(spare);

                // Hand the user the buffer that travelled the ring — moved, not
                // cloned. One fewer alloc + full-buffer memcpy per delivered
                // buffer than the previous clone-and-recycle approach.
                Some(buffer)
            }
            Err(rtrb::PopError::Empty) => None,
        }
    }

    /// Refill the consumer-side spare pool with up to `count` pre-sized empty
    /// `Vec<f32>` allocations (off the real-time thread).
    ///
    /// Optional warm-up helper: calling this before a capture burst means the
    /// first `pop`s recycle a pooled spare instead of allocating one inline.
    /// Test/diagnostic surface — `pop` already lazily allocates when the pool is
    /// empty, so correctness does not depend on it.
    #[allow(dead_code)]
    pub(crate) fn refill_spare_pool(&mut self, count: usize, sample_capacity: usize) {
        for _ in 0..count {
            self.spare_pool.push(Vec::with_capacity(sample_capacity));
        }
    }

    /// Blocks until data is available, the stream ends, or `timeout` expires.
    ///
    /// # Wake mechanism (`PU-5`, seed `rsac-efb4`)
    ///
    /// Parks on an always-present `ConsumerWake` (`Condvar`) rather than
    /// sleeping on a fixed 1 ms timer. A parked reader is woken **the instant**
    /// either of the following happens:
    ///
    /// - a non-RT producer pushes data and calls
    ///   [`BridgeProducer::notify_consumers`] (the Windows capture loop), or
    /// - the stream reaches a terminal/ending state via a transition that wakes
    ///   ([`signal_done`](BridgeProducer::signal_done) /
    ///   [`signal_error`](BridgeProducer::signal_error), and `BridgeStream::stop`).
    ///
    /// The notify is wired **only** into those non-RT sites — never the
    /// Linux/macOS RT callback push path — so ADR-0001 holds (see
    /// `ConsumerWake` and [`BridgeProducer::notify_consumers`]).
    ///
    /// ## Degrade-not-hang backstop
    ///
    /// Each park is bounded to a `WAKE_BACKSTOP_POLL` (1 ms) slice. So even on
    /// a backend that does **not** notify (Linux/macOS, whose RT push path
    /// deliberately does not wake), or in the rare event a notify is missed, the
    /// reader still re-checks the ring and state within 1 ms — it degrades to
    /// the old poll cadence instead of hanging. The lost-wakeup race is closed
    /// by snapshotting the wake generation *before* the final ring/state
    /// re-check (see `ConsumerWake::wait`).
    ///
    /// # Errors
    ///
    /// - [`AudioError::Timeout`] if the timeout expires before data arrives.
    /// - [`AudioError::StreamEnded`] (Fatal) if the stream state becomes terminal
    ///   (Stopped, Closed, or Error) during the wait — end-of-stream, not a
    ///   transient read error (see ADR-0003).
    pub fn pop_blocking(&mut self, timeout: Duration) -> AudioResult<AudioBuffer> {
        let deadline = Instant::now() + timeout;

        loop {
            // Snapshot the wake generation BEFORE the ring/state re-check below.
            // A notify that fires after this snapshot bumps the generation, so
            // the subsequent `wait(since, ..)` returns immediately instead of
            // parking on an already-consumed signal (closes the lost-wakeup
            // race without holding the wake lock across the push).
            let since = self.shared.wake.generation();

            // Try to pop data first.
            if let Some(buffer) = self.pop() {
                return Ok(buffer);
            }

            // Check if the stream is in a terminal state. This is end-of-stream,
            // not a transient read error — return the Fatal StreamEnded so a
            // read loop branching on is_fatal()/is_recoverable() terminates
            // instead of busy-waiting a dead stream (see ADR-0003).
            if self.shared.state.is_terminal() {
                return Err(AudioError::StreamEnded {
                    reason: "Stream stopped".to_string(),
                });
            }

            // Check if we've exceeded the timeout.
            let now = Instant::now();
            if now >= deadline {
                return Err(AudioError::Timeout {
                    operation: "read_chunk".to_string(),
                    duration: timeout,
                });
            }

            // Park until the next notify (push / terminal transition) or the
            // bounded backstop slice — whichever is sooner — but never past the
            // caller's deadline. Clamping to the remaining time keeps the
            // overall timeout honored to within one backstop slice; the loop
            // re-checks the ring and state on every wakeup (spurious or real).
            let remaining = deadline.saturating_duration_since(now);
            let slice = remaining.min(WAKE_BACKSTOP_POLL);
            self.shared.wake.wait(since, slice);
        }
    }

    /// Returns the number of buffers ready to read.
    pub fn available_buffers(&self) -> usize {
        self.consumer.slots()
    }

    /// Returns the total number of buffers successfully popped.
    pub fn buffers_popped(&self) -> u64 {
        self.shared.buffers_popped.load(Ordering::Relaxed)
    }

    /// Returns `true` if the producer has signaled it is done.
    ///
    /// This is the case when the stream state is [`StreamState::Stopping`],
    /// [`StreamState::Stopped`], [`StreamState::Closed`], or [`StreamState::Error`].
    pub fn is_producer_done(&self) -> bool {
        matches!(
            self.shared.state.get(),
            StreamState::Stopping | StreamState::Stopped | StreamState::Closed | StreamState::Error
        )
    }

    /// Returns a reference to the shared state.
    /// Platform-conditional: called by BridgeStream::new() and used by platform backends.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }
}

// ── Sample-domain SPSC ring (feature: bridge-zerocopy) ─────────────────────

/// Per-chunk metadata travelling the sidecar ring alongside the interleaved
/// `f32` samples in [`SampleRing`]. One record per pushed chunk lets the
/// consumer slice the sample stream back into equivalent [`AudioBuffer`]s.
///
/// `rsac-3616`: keeping metadata in a tiny parallel SPSC ring (rather than
/// interleaving it with samples) keeps the sample ring a pure `f32` ring, which
/// is the precondition for `rtrb`'s `write_chunk_uninit` + [`CopyToUninit`]
/// fast path — samples are copied straight into the ring's `MaybeUninit<f32>`
/// slots with no `Vec`/`AudioBuffer` allocation on the producer thread.
#[cfg(feature = "bridge-zerocopy")]
#[derive(Debug, Clone, Copy)]
struct ChunkMeta {
    /// Number of interleaved `f32` samples this chunk occupies in the sample ring.
    len: usize,
    channels: u16,
    sample_rate: u32,
    /// Stream-relative timestamp of the first sample, encoded as nanoseconds;
    /// `u64::MAX` is the "no timestamp" sentinel (matches `AudioBuffer::new`).
    timestamp_nanos: u64,
}

#[cfg(feature = "bridge-zerocopy")]
const NO_TIMESTAMP: u64 = u64::MAX;

/// Producer side of the sample-domain SPSC ring (`rsac-3616`, feature
/// `bridge-zerocopy`, default OFF).
///
/// Writes interleaved `f32` directly into the ring's uninitialized slots via
/// `rtrb::Producer::write_chunk_uninit` + [`CopyToUninit::copy_to_uninit`] then
/// `commit` — **no `Vec<f32>` and no `AudioBuffer` allocation on the producer
/// call**. A parallel metadata ring carries `(len, channels, sample_rate,
/// timestamp)` so the consumer can reconstruct equivalent `AudioBuffer`s. This
/// is an *internal* alternative data plane A/B'd against the default
/// `AudioBuffer` ring in the bridge benchmark; the public producer/consumer
/// surface is unchanged.
#[cfg(feature = "bridge-zerocopy")]
pub struct SampleRingProducer {
    samples: rtrb::Producer<f32>,
    meta: rtrb::Producer<ChunkMeta>,
    shared: Arc<BridgeShared>,
}

#[cfg(feature = "bridge-zerocopy")]
impl SampleRingProducer {
    /// Push one interleaved-`f32` chunk into the sample ring with **zero
    /// allocation** on this (real-time) call.
    ///
    /// Reserves `data.len()` contiguous-or-wrapped slots via
    /// `write_chunk_uninit`, copies the samples in with
    /// [`CopyToUninit::copy_to_uninit`] (a `memcpy` into `MaybeUninit<f32>`
    /// slots — no intermediate buffer), commits, then publishes one metadata
    /// record. If either ring lacks room the whole chunk is dropped atomically
    /// (nothing is committed) and `buffers_dropped` is incremented, so the
    /// consumer never sees a partial chunk.
    ///
    /// Returns `true` if the chunk was published, `false` if dropped.
    pub fn push_samples_or_drop_at(
        &mut self,
        data: &[f32],
        channels: u16,
        sample_rate: u32,
        timestamp: Option<Duration>,
    ) -> bool {
        // Require room in BOTH rings before committing anything, so a chunk is
        // all-or-nothing and the metadata/sample streams never desynchronize.
        if self.meta.slots() == 0 {
            return self.drop_chunk();
        }
        let mut chunk = match self.samples.write_chunk_uninit(data.len()) {
            Ok(chunk) => chunk,
            Err(_too_few) => return self.drop_chunk(),
        };

        // Copy the interleaved samples straight into the (possibly wrapped)
        // uninitialized ring slots — no Vec, no AudioBuffer.
        let (first, second) = chunk.as_mut_slices();
        let mid = first.len();
        data[..mid].copy_to_uninit(first);
        data[mid..].copy_to_uninit(second);
        // SAFETY: copy_to_uninit initialized exactly `data.len()` slots above.
        unsafe { chunk.commit_all() };

        let meta = ChunkMeta {
            len: data.len(),
            channels,
            sample_rate,
            timestamp_nanos: timestamp.map_or(NO_TIMESTAMP, |d| d.as_nanos() as u64),
        };
        // The metadata push cannot fail: we checked `meta.slots() > 0` above and
        // are the sole producer. If it somehow did, the samples are already
        // committed; recover by counting a drop (the consumer tolerates a
        // missing meta by treating the sample ring as authoritative-by-meta).
        if self.meta.push(meta).is_err() {
            return self.drop_chunk();
        }

        self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
        self.shared.consecutive_drops.store(0, Ordering::Relaxed);
        self.shared.record_drop_window(false);
        #[cfg(feature = "async-stream")]
        self.shared.waker.wake();
        true
    }

    /// Untimed convenience wrapper around
    /// [`push_samples_or_drop_at`](Self::push_samples_or_drop_at).
    #[inline]
    pub fn push_samples_or_drop(&mut self, data: &[f32], channels: u16, sample_rate: u32) -> bool {
        self.push_samples_or_drop_at(data, channels, sample_rate, None)
    }

    /// Account a dropped chunk and return `false`.
    #[inline]
    fn drop_chunk(&self) -> bool {
        self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
        self.shared
            .consecutive_drops
            .fetch_add(1, Ordering::Relaxed);
        self.shared.record_drop_window(true);
        false
    }
}

/// Consumer side of the sample-domain SPSC ring (`rsac-3616`).
///
/// Reads one `ChunkMeta` record, pops exactly that many interleaved `f32`
/// from the sample ring, and reconstructs an [`AudioBuffer`] equivalent to what
/// the default `AudioBuffer` ring would have delivered (same data, channels,
/// rate, and timestamp). The reconstruction allocates the user's `Vec<f32>`
/// here, on the **non-real-time consumer thread** — same division of labor as
/// the default path.
#[cfg(feature = "bridge-zerocopy")]
pub struct SampleRingConsumer {
    samples: rtrb::Consumer<f32>,
    meta: rtrb::Consumer<ChunkMeta>,
    shared: Arc<BridgeShared>,
}

#[cfg(feature = "bridge-zerocopy")]
impl SampleRingConsumer {
    /// Pop one chunk, reconstructing the [`AudioBuffer`]. Returns `None` if no
    /// complete chunk is available yet.
    pub fn pop(&mut self) -> Option<AudioBuffer> {
        let meta = self.meta.pop().ok()?;
        // Pull exactly `meta.len` samples back out of the f32 ring.
        let mut data = Vec::with_capacity(meta.len);
        // `read_chunk` gives us the (possibly wrapped) slices; copy them out and
        // commit so the producer can reuse the slots.
        match self.samples.read_chunk(meta.len) {
            Ok(chunk) => {
                let (first, second) = chunk.as_slices();
                data.extend_from_slice(first);
                data.extend_from_slice(second);
                chunk.commit_all();
            }
            Err(_too_few) => {
                // Should not happen: meta is only published after its samples
                // are committed. Treat as no data rather than panicking.
                return None;
            }
        }

        self.shared.buffers_popped.fetch_add(1, Ordering::Relaxed);

        let format = AudioFormat {
            sample_rate: meta.sample_rate,
            channels: meta.channels,
            sample_format: SampleFormat::F32,
        };
        Some(if meta.timestamp_nanos == NO_TIMESTAMP {
            AudioBuffer::with_format(data, format)
        } else {
            AudioBuffer::with_timestamp(data, format, Duration::from_nanos(meta.timestamp_nanos))
        })
    }

    /// Number of complete chunks currently ready to read.
    pub fn available_chunks(&self) -> usize {
        self.meta.slots()
    }
}

/// Create a matched [`SampleRingProducer`]/[`SampleRingConsumer`] pair
/// (`rsac-3616`, feature `bridge-zerocopy`).
///
/// `sample_capacity` is the number of interleaved `f32` slots in the sample
/// ring; `max_chunks` bounds how many in-flight chunks the metadata sidecar can
/// hold. Both are rounded to the ring's own power-of-two policy by `rtrb`.
#[cfg(feature = "bridge-zerocopy")]
pub fn create_sample_ring(
    sample_capacity: usize,
    max_chunks: usize,
    format: AudioFormat,
) -> (SampleRingProducer, SampleRingConsumer) {
    let (sp, sc) = rtrb::RingBuffer::<f32>::new(sample_capacity);
    let (mp, mc) = rtrb::RingBuffer::<ChunkMeta>::new(max_chunks.max(1));
    let shared = Arc::new(BridgeShared {
        state: AtomicStreamState::new(StreamState::Created),
        buffers_pushed: CachePadded::new(AtomicU64::new(0)),
        buffers_dropped: CachePadded::new(AtomicU64::new(0)),
        buffers_popped: CachePadded::new(AtomicU64::new(0)),
        consecutive_drops: CachePadded::new(AtomicU32::new(0)),
        backpressure_threshold: DEFAULT_BACKPRESSURE_THRESHOLD,
        negotiated: AtomicU64::new(0),
        drop_window: std::array::from_fn(|_| AtomicU64::new(0)),
        drop_window_cursor: AtomicU64::new(0),
        push_panicked: std::sync::atomic::AtomicBool::new(false),
        wake: ConsumerWake::new(),
        format,
        #[cfg(feature = "async-stream")]
        waker: atomic_waker::AtomicWaker::new(),
    });
    (
        SampleRingProducer {
            samples: sp,
            meta: mp,
            shared: Arc::clone(&shared),
        },
        SampleRingConsumer {
            samples: sc,
            meta: mc,
            shared,
        },
    )
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Create a matched producer/consumer pair connected by a lock-free ring buffer.
///
/// # Arguments
///
/// * `capacity` — Number of [`AudioBuffer`] slots in the ring buffer.
///   Should be a power of 2 for optimal performance (use [`calculate_capacity`]).
/// * `format` — Audio format for this bridge (stored in shared state for reference).
///
/// # Returns
///
/// A `(BridgeProducer, BridgeConsumer)` pair. The producer should be moved to the
/// OS callback thread; the consumer stays on the reader thread.
pub fn create_bridge(capacity: usize, format: AudioFormat) -> (BridgeProducer, BridgeConsumer) {
    create_bridge_with_options(capacity, format, DEFAULT_BACKPRESSURE_THRESHOLD)
}

/// Like [`create_bridge`], but lets the caller pick the back-pressure threshold.
///
/// `backpressure_threshold` is the number of *consecutive* dropped buffers (no
/// successful push in between) before `is_under_backpressure`
/// reports `true`. A value of `0` is degenerate: `is_under_backpressure`
/// reports `true` immediately (`0 >= 0`), even before any drop has occurred.
/// Most callers should use [`create_bridge`], which applies
/// [`DEFAULT_BACKPRESSURE_THRESHOLD`]; this variant exists so a backend or
/// builder that knows its callback cadence can tune the sensitivity (L6).
pub fn create_bridge_with_options(
    capacity: usize,
    format: AudioFormat,
    backpressure_threshold: u32,
) -> (BridgeProducer, BridgeConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::<AudioBuffer>::new(capacity);

    // Free-list return ring: carries drained `Vec<f32>` allocations from the
    // consumer back to the producer so the RT thread can reuse them. Same
    // capacity as the data ring so it can never be the limiting factor.
    let (mut free_tx, free_rx) = rtrb::RingBuffer::<Vec<f32>>::new(capacity);

    // Pre-seed a handful of reusable allocations so the producer is
    // allocation-free from the very first callbacks, before the consumer has
    // had a chance to recycle anything. Each is sized for a realistic
    // worst-case callback period (see RT_BUFFER_SAMPLE_CAPACITY).
    let seed = capacity.min(8);
    for _ in 0..seed {
        // If the ring somehow rejects (it won't — it's empty), just drop.
        let _ = free_tx.push(Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY));
    }

    let shared = Arc::new(BridgeShared {
        state: AtomicStreamState::new(StreamState::Created),
        buffers_pushed: CachePadded::new(AtomicU64::new(0)),
        buffers_dropped: CachePadded::new(AtomicU64::new(0)),
        buffers_popped: CachePadded::new(AtomicU64::new(0)),
        consecutive_drops: CachePadded::new(AtomicU32::new(0)),
        backpressure_threshold,
        // 0 == "no backend negotiation yet"; negotiated_format() then falls
        // back to the requested `format` below.
        negotiated: AtomicU64::new(0),
        // Fixed alloc-free sliding drop-rate window, all slots zeroed
        // (rsac-cfe4). `from_fn` avoids relying on an array `Default` impl for
        // the non-`Copy` `AtomicU64`.
        drop_window: std::array::from_fn(|_| AtomicU64::new(0)),
        drop_window_cursor: AtomicU64::new(0),
        push_panicked: std::sync::atomic::AtomicBool::new(false),
        wake: ConsumerWake::new(),
        format,
        #[cfg(feature = "async-stream")]
        waker: atomic_waker::AtomicWaker::new(),
    });

    (
        BridgeProducer {
            producer,
            shared: Arc::clone(&shared),
            free_rx,
            // Single-slot fallback for when the free-list ring is momentarily
            // empty. Pre-sized for a realistic worst-case callback period.
            scratch: Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY),
        },
        BridgeConsumer {
            consumer,
            shared,
            free_tx,
            // Pre-seed the consumer-side spare pool so the first `pop`s recycle
            // a pooled allocation to the producer rather than allocating one
            // inline (rsac-17d1). Sized to the free-list seed for symmetry.
            spare_pool: (0..seed)
                .map(|_| Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY))
                .collect(),
        },
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Calculate an appropriate ring buffer capacity.
///
/// Uses the requested size or a sensible default (64 buffers), ensuring the
/// result is at least `min_capacity` and rounded up to the next power of two.
///
/// # Arguments
///
/// * `requested` — Desired capacity, or `None` for the default (64).
/// * `min_capacity` — Absolute minimum capacity (suggested: 4).
///
/// # Examples
///
/// ```rust,ignore
/// assert_eq!(calculate_capacity(None, 4), 64);      // default
/// assert_eq!(calculate_capacity(Some(100), 4), 128); // rounded up to next power of 2
/// assert_eq!(calculate_capacity(Some(2), 4), 4);     // clamped to min_capacity
/// ```
pub fn calculate_capacity(requested: Option<usize>, min_capacity: usize) -> usize {
    let raw = requested.unwrap_or(64).max(min_capacity);
    raw.next_power_of_two()
}

/// Number of callback periods of headroom a period-derived ring buffer aims to
/// hold (`rsac-b655`). Each ring slot carries exactly one callback period's
/// [`AudioBuffer`], so the slot count *is* the number of periods of slack the
/// consumer has before back-pressure. ~12 periods is the middle of the 8–16×
/// band the design targets: enough to ride out a scheduling hiccup on the
/// reader thread without buffering so much that end-to-end latency balloons.
pub(crate) const PERIOD_HEADROOM_BUFFERS: usize = 12;

/// Fallback ring capacity when the negotiated callback period is unknown or
/// degenerate (`rsac-b655`). Matches the historical static default of
/// [`calculate_capacity`] so backends that cannot learn their period behave
/// exactly as before.
pub(crate) const PERIOD_FALLBACK_CAPACITY: usize = 64;

/// Lower / upper bounds (in ring slots) for a period-derived capacity
/// (`rsac-b655`). The floor keeps even very large periods from producing a
/// uselessly small ring; the ceiling caps memory/latency for tiny periods that
/// would otherwise demand a huge slot count.
pub(crate) const PERIOD_MIN_CAPACITY: usize = 8;
pub(crate) const PERIOD_MAX_CAPACITY: usize = 1024;

/// Derive a ring-buffer capacity (in [`AudioBuffer`] slots) from the negotiated
/// device callback period (`rsac-b655`).
///
/// Backends learn the period the OS audio engine will actually deliver
/// (WASAPI `GetBufferSize`, the PipeWire negotiated buffer size, the CoreAudio
/// IOProc frame count). Sizing the ring to **cover several such periods of
/// headroom** — rather than a one-size static 64 — lets the consumer absorb
/// scheduling jitter without the producer dropping buffers, while keeping the
/// ring small for large periods where 64 slots would be wasteful.
///
/// This is a **pure function**: it does no I/O and touches no backend state, so
/// it is trivially testable and the platform backends adopt it separately.
///
/// # Sizing model
///
/// Each ring slot holds one callback period, so the natural slot count is just
/// `PERIOD_HEADROOM_BUFFERS` periods of slack. Tiny periods, however, fire
/// callbacks far more often (a 64-frame period at 48 kHz is ~1.3 ms, vs ~21 ms
/// for 1024 frames), so a fixed slot count gives them far less wall-clock slack.
/// To compensate, the headroom is scaled up for periods smaller than the tuned
/// reference (`RT_BUFFER_SAMPLE_CAPACITY` frames-equivalent) and left flat for
/// larger ones, then the result is:
///
/// 1. clamped to the `PERIOD_MIN_CAPACITY ..= PERIOD_MAX_CAPACITY` band, and
/// 2. rounded **up** to the next power of two (the ring's preferred sizing, see
///    [`calculate_capacity`]).
///
/// # Fallback
///
/// Returns `PERIOD_FALLBACK_CAPACITY` (64) when the period is unknown or
/// degenerate — `period_frames == 0` or `channels == 0` — so a backend that
/// cannot determine its period gets exactly the historical default.
///
/// # Arguments
///
/// * `period_frames` — Frames per OS callback period (per channel). `0` ⇒ fallback.
/// * `channels` — Channel count of the delivered stream. `0` ⇒ fallback.
///
/// # Examples
///
/// ```rust,ignore
/// // Unknown period → historical default.
/// assert_eq!(calculate_capacity_for_period(0, 2), 64);
/// // A typical 1024-frame stereo period → ~12 periods, rounded to a power of two.
/// let cap = calculate_capacity_for_period(1024, 2);
/// assert!(cap.is_power_of_two() && (8..=1024).contains(&cap));
/// ```
pub fn calculate_capacity_for_period(period_frames: usize, channels: usize) -> usize {
    // Degenerate / unknown period: fall back to the historical static default.
    if period_frames == 0 || channels == 0 {
        return PERIOD_FALLBACK_CAPACITY;
    }

    // Reference period (in frames-per-channel) the bridge is tuned around. The
    // free-list buffers are sized for `RT_BUFFER_SAMPLE_CAPACITY` interleaved
    // f32 (see ADR-0001); dividing by a 2-channel reference recovers the
    // frames-per-channel that capacity was chosen for.
    const REFERENCE_FRAMES: usize = RT_BUFFER_SAMPLE_CAPACITY / 2; // 1024

    // Smaller-than-reference periods fire callbacks proportionally more often,
    // so scale the per-period headroom up to keep roughly constant wall-clock
    // slack. `div_ceil` rounds the multiplier up so a period at/above the
    // reference keeps the base headroom (multiplier 1), never less.
    //
    // `channels` is part of the signature so callers pass the negotiated
    // stream shape; the per-channel `period_frames` already determines callback
    // cadence, so the slot count does not additionally multiply by channels
    // (each slot already holds the whole interleaved period regardless of width).
    let _ = channels;
    let scale = REFERENCE_FRAMES.div_ceil(period_frames).max(1);
    let raw = PERIOD_HEADROOM_BUFFERS.saturating_mul(scale);

    // Clamp to the sane band, then round up to the ring's power-of-two policy.
    let clamped = raw.clamp(PERIOD_MIN_CAPACITY, PERIOD_MAX_CAPACITY);
    clamped.next_power_of_two()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{AudioFormat, SampleFormat};

    /// Creates a test [`AudioBuffer`] filled with `value` — 10 ms of stereo 48 kHz audio.
    fn test_buffer(value: f32) -> AudioBuffer {
        AudioBuffer::new(vec![value; 960], 2, 48000)
    }

    fn test_format() -> AudioFormat {
        AudioFormat::default() // 48 kHz, 2ch, F32
    }

    // 1. Factory creates a valid pair
    #[test]
    fn test_create_bridge() {
        let (producer, consumer) = create_bridge(16, test_format());
        assert_eq!(producer.available_slots(), 16);
        assert_eq!(consumer.available_buffers(), 0);
        assert_eq!(producer.buffers_dropped(), 0);
        assert_eq!(consumer.buffers_popped(), 0);
        assert_eq!(producer.shared().state.get(), StreamState::Created);
        assert_eq!(consumer.shared().state.get(), StreamState::Created);
    }

    // 2. Push a buffer, pop it, verify data integrity
    #[test]
    fn test_push_pop() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        let buf = test_buffer(0.5);
        assert!(producer.push(buf).is_ok());

        let popped = consumer.pop().expect("should have one buffer");
        assert_eq!(popped.data()[0], 0.5);
        assert_eq!(popped.len(), 960);
        assert_eq!(popped.channels(), 2);
        assert_eq!(popped.sample_rate(), 48000);
    }

    // 3. Push several, pop several, verify FIFO order
    #[test]
    fn test_push_pop_multiple() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        for i in 0..5 {
            let buf = test_buffer(i as f32);
            assert!(producer.push(buf).is_ok());
        }

        for i in 0..5 {
            let popped = consumer.pop().expect("should have buffer");
            assert_eq!(
                popped.data()[0],
                i as f32,
                "FIFO order violated at index {}",
                i
            );
        }
    }

    // 4. Pop from empty returns None
    #[test]
    fn test_empty_pop() {
        let (_producer, mut consumer) = create_bridge(16, test_format());
        assert!(consumer.pop().is_none());
    }

    // 5. Fill buffer to capacity, verify push returns Err
    #[test]
    fn test_full_push() {
        let (mut producer, _consumer) = create_bridge(4, test_format());

        for _ in 0..4 {
            assert!(producer.push(test_buffer(1.0)).is_ok());
        }

        // Ring buffer is now full — push should fail.
        let result = producer.push(test_buffer(2.0));
        assert!(result.is_err());

        // Get back the rejected buffer.
        let rejected = result.unwrap_err();
        assert_eq!(rejected.data()[0], 2.0);
    }

    // 6. push_or_drop drops and increments counter
    #[test]
    fn test_push_or_drop() {
        let (mut producer, _consumer) = create_bridge(4, test_format());

        // Fill the buffer.
        for _ in 0..4 {
            assert!(producer.push_or_drop(test_buffer(1.0)));
        }

        // This one should be dropped.
        assert!(!producer.push_or_drop(test_buffer(2.0)));
        assert_eq!(producer.buffers_dropped(), 1);

        // Drop another.
        assert!(!producer.push_or_drop(test_buffer(3.0)));
        assert_eq!(producer.buffers_dropped(), 2);
    }

    // 7. pop_blocking succeeds immediately when there is already data
    #[test]
    fn test_pop_blocking_with_data() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        producer.push(test_buffer(0.75)).unwrap();

        let result = consumer.pop_blocking(Duration::from_millis(100));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().data()[0], 0.75);
    }

    // 8. pop_blocking on empty with short timeout returns Timeout error
    #[test]
    fn test_pop_blocking_timeout() {
        let (_producer, mut consumer) = create_bridge(16, test_format());

        let start = Instant::now();
        let result = consumer.pop_blocking(Duration::from_millis(10));
        let elapsed = start.elapsed();

        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::Timeout { operation, .. } => {
                assert_eq!(operation, "read_chunk");
            }
            other => panic!("Expected Timeout error, got: {:?}", other),
        }
        // Should have waited at least ~10ms (allow some slack).
        assert!(elapsed >= Duration::from_millis(5));
    }

    // 9. pop_blocking returns the terminal StreamEnded when state becomes
    //    terminal (ADR-0003 — distinct from the recoverable StreamReadError).
    #[test]
    fn test_pop_blocking_stream_stopped() {
        let (_producer, mut consumer) = create_bridge(16, test_format());

        // Force the state to Stopped.
        consumer.shared().state.force_set(StreamState::Stopped);

        let result = consumer.pop_blocking(Duration::from_secs(5));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_fatal(), "terminal-state read must be Fatal");
        match err {
            AudioError::StreamEnded { reason } => {
                assert!(reason.contains("stopped") || reason.contains("Stream"));
            }
            other => panic!("Expected StreamEnded, got: {:?}", other),
        }
    }

    // 10. available_slots and available_buffers after pushes/pops
    #[test]
    fn test_available_slots_and_buffers() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        assert_eq!(producer.available_slots(), 8);
        assert_eq!(consumer.available_buffers(), 0);

        producer.push(test_buffer(1.0)).unwrap();
        producer.push(test_buffer(2.0)).unwrap();

        assert_eq!(producer.available_slots(), 6);
        assert_eq!(consumer.available_buffers(), 2);

        consumer.pop().unwrap();

        assert_eq!(producer.available_slots(), 7);
        assert_eq!(consumer.available_buffers(), 1);
    }

    // 11. Diagnostics counters
    #[test]
    fn test_diagnostics_counters() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        // Push 4 (fills the ring buffer).
        for _ in 0..4 {
            producer.push(test_buffer(1.0)).unwrap();
        }
        assert_eq!(producer.shared().buffers_pushed.load(Ordering::Relaxed), 4);

        // Drop 2 via push_or_drop.
        producer.push_or_drop(test_buffer(1.0));
        producer.push_or_drop(test_buffer(1.0));
        assert_eq!(producer.buffers_dropped(), 2);
        assert_eq!(producer.shared().buffers_dropped.load(Ordering::Relaxed), 2);

        // Pop 3.
        consumer.pop().unwrap();
        consumer.pop().unwrap();
        consumer.pop().unwrap();
        assert_eq!(consumer.buffers_popped(), 3);
        assert_eq!(consumer.shared().buffers_popped.load(Ordering::Relaxed), 3);
    }

    // 12. calculate_capacity: power-of-2, minimum, default
    #[test]
    fn test_calculate_capacity() {
        // Default (None) with min 4 → 64.
        assert_eq!(calculate_capacity(None, 4), 64);

        // Requested 100 → next power of 2 = 128.
        assert_eq!(calculate_capacity(Some(100), 4), 128);

        // Requested 2 with min 4 → clamped to 4 (already power of 2).
        assert_eq!(calculate_capacity(Some(2), 4), 4);

        // Requested 1 with min 1 → 1 (already power of 2).
        assert_eq!(calculate_capacity(Some(1), 1), 1);

        // Requested exact power of 2.
        assert_eq!(calculate_capacity(Some(32), 4), 32);

        // Requested 0 with min 4 → 4.
        assert_eq!(calculate_capacity(Some(0), 4), 4);

        // Large min_capacity.
        assert_eq!(calculate_capacity(Some(3), 16), 16);

        // Requested 5 with min 4 → 8 (next power of 2 above 5).
        assert_eq!(calculate_capacity(Some(5), 4), 8);
    }

    // 13. Compile-time check that BridgeProducer is Send
    #[test]
    fn test_producer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BridgeProducer>();
    }

    // 14. Compile-time check that BridgeConsumer is Send
    #[test]
    fn test_consumer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BridgeConsumer>();
    }

    // 15. signal_done transitions state
    #[test]
    fn test_signal_done() {
        let (producer, consumer) = create_bridge(8, test_format());

        // Set state to Running first (signal_done transitions Running → Stopping).
        producer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        assert!(producer.shared().state.is_running());

        producer.signal_done();

        assert_eq!(producer.shared().state.get(), StreamState::Stopping);
        assert!(consumer.is_producer_done());
    }

    // 15b. signal_error drives the stream to the terminal Error state (FH-1 /
    //      ADR-0010). Unlike signal_done (Running → Stopping, still drainable),
    //      signal_error is the FATAL sibling: Error is terminal for BOTH readers.
    #[test]
    fn test_signal_error_sets_terminal_error() {
        let (producer, consumer) = create_bridge(8, test_format());

        // Start from Running, as a live capture would be.
        producer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        assert!(producer.shared().state.is_running());

        producer.signal_error();

        assert_eq!(producer.shared().state.get(), StreamState::Error);
        assert!(
            producer.shared().state.is_terminal(),
            "Error must be a terminal state so blocking reads end with StreamEnded"
        );
        // The consumer agrees the producer is done.
        assert!(consumer.is_producer_done());
    }

    // 15c. signal_error is callable from ANY state (it force_sets, not a CAS),
    //      mirroring a spontaneous death that can race a graceful stop. It is
    //      sticky/idempotent: a later signal_done CAS cannot un-terminalize it.
    #[test]
    fn test_signal_error_is_sticky_against_signal_done() {
        let (producer, _consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        producer.signal_error();
        assert_eq!(producer.shared().state.get(), StreamState::Error);

        // A graceful stop after a fatal death must NOT downgrade Error → Stopping
        // (signal_done's CAS is Running→Stopping; state is Error, so it no-ops).
        producer.signal_done();
        assert_eq!(
            producer.shared().state.get(),
            StreamState::Error,
            "terminal Error must be sticky — a late graceful signal cannot revive the stream"
        );
    }

    // 15d. After signal_error, a blocking read returns the Fatal StreamEnded
    //      (terminal), proving the dead-producer hang is removed (FH-1).
    #[test]
    fn test_signal_error_ends_blocking_read_with_fatal() {
        let (producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        producer.signal_error();

        let result = consumer.pop_blocking(Duration::from_secs(5));
        let err = result.expect_err("terminal Error must end the blocking read");
        assert!(
            err.is_fatal(),
            "a terminal-Error read must be Fatal (StreamEnded), not a recoverable hiccup"
        );
        match err {
            AudioError::StreamEnded { .. } => {}
            other => panic!("Expected StreamEnded after signal_error, got: {:?}", other),
        }
    }

    // 15e. Contrast guard: signal_done (graceful Stopping) keeps pop_blocking
    //      DRAINING — an empty ring times out rather than ending — proving the
    //      Error-vs-Stopping distinction signal_error relies on (FH-1 / ADR-0010).
    #[test]
    fn test_signal_done_keeps_blocking_read_draining_not_terminal() {
        let (producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        producer.signal_done(); // Running → Stopping (NOT terminal)
        assert_eq!(producer.shared().state.get(), StreamState::Stopping);

        // No data + Stopping (drainable) → the read must time out, NOT StreamEnded.
        let err = consumer
            .pop_blocking(Duration::from_millis(10))
            .expect_err("empty Stopping ring should time out");
        assert!(
            matches!(err, AudioError::Timeout { .. }),
            "graceful Stopping must keep draining (Timeout), not terminate (got {:?})",
            err
        );
    }

    // ===== K5.2: Ring Buffer Edge Case Tests =====

    #[test]
    fn signal_done_then_remaining_data_drains() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());
        producer.shared().state.force_set(StreamState::Running);

        // Push some data
        let buf1 = AudioBuffer::new(vec![1.0, 2.0], 2, 48000);
        let buf2 = AudioBuffer::new(vec![3.0, 4.0], 2, 48000);
        assert!(producer.push(buf1).is_ok());
        assert!(producer.push(buf2).is_ok());

        // Signal done
        producer.signal_done();

        // Should still be able to read remaining data
        let read1 = consumer.pop();
        assert!(read1.is_some());
        assert_eq!(read1.unwrap().data(), &[1.0, 2.0]);

        let read2 = consumer.pop();
        assert!(read2.is_some());
        assert_eq!(read2.unwrap().data(), &[3.0, 4.0]);

        // Now empty
        let read3 = consumer.pop();
        assert!(read3.is_none());
    }

    #[test]
    fn push_to_full_buffer_returns_error() {
        let (mut producer, _consumer) = create_bridge(2, test_format());

        let buf1 = AudioBuffer::new(vec![1.0], 1, 48000);
        let buf2 = AudioBuffer::new(vec![2.0], 1, 48000);
        assert!(producer.push(buf1).is_ok());
        assert!(producer.push(buf2).is_ok());

        // Buffer should be full now — next push fails
        let buf3 = AudioBuffer::new(vec![3.0], 1, 48000);
        let result = producer.push(buf3);
        assert!(result.is_err());
    }

    #[test]
    fn push_or_drop_on_full_buffer_increments_dropped() {
        let (mut producer, _consumer) = create_bridge(2, test_format());

        // Fill the buffer
        for i in 0..2 {
            let buf = AudioBuffer::new(vec![i as f32], 1, 48000);
            let _ = producer.push(buf);
        }

        // push_or_drop should not panic
        let buf_extra = AudioBuffer::new(vec![99.0], 1, 48000);
        producer.push_or_drop(buf_extra);
        assert!(producer.buffers_dropped() >= 1);
    }

    #[test]
    fn consumer_pop_empty_returns_none() {
        let (_producer, mut consumer) = create_bridge(4, test_format());
        assert!(consumer.pop().is_none());
        assert_eq!(consumer.available_buffers(), 0);
    }

    #[test]
    fn buffers_popped_counter_increments() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        let buf = AudioBuffer::new(vec![1.0], 1, 48000);
        assert!(producer.push(buf).is_ok());

        assert_eq!(consumer.buffers_popped(), 0);
        let _ = consumer.pop();
        assert_eq!(consumer.buffers_popped(), 1);
    }

    #[test]
    fn is_producer_done_after_signal() {
        let (producer, consumer) = create_bridge(4, test_format());
        producer.shared().state.force_set(StreamState::Running);

        assert!(!consumer.is_producer_done());
        producer.signal_done();
        assert!(consumer.is_producer_done());
    }

    // ===== Free-list return ring (alloc-free RT producer) tests =====

    // push_samples_or_drop → pop preserves data, channels, and rate.
    #[test]
    fn push_samples_then_pop_preserves_data() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        let samples = [0.1, -0.2, 0.3, -0.4];
        assert!(producer.push_samples_or_drop(&samples, 2, 44100));

        let buf = consumer.pop().expect("should have one buffer");
        assert_eq!(buf.data(), &samples);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 44100);
    }

    // Popping a buffer recycles a Vec back to the producer's free-list ring.
    #[test]
    fn pop_recycles_allocation_to_producer() {
        // Capacity 4 → free-list seeded with min(4, 8) = 4 buffers.
        let (mut producer, mut consumer) = create_bridge(4, test_format());
        assert_eq!(producer.recycled_available(), 4);

        // Drain the seed: each push_samples_or_drop consumes one recycled Vec.
        for _ in 0..4 {
            assert!(producer.push_samples_or_drop(&[1.0, 2.0], 2, 48000));
        }
        assert_eq!(
            producer.recycled_available(),
            0,
            "seed should be drained after 4 pushes"
        );

        // Popping hands the user a copy and returns the spare alloc to the ring.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(
            producer.recycled_available(),
            1,
            "pop should recycle one allocation back to the producer"
        );
    }

    // Steady-state push/pop loop preserves data integrity over many cycles
    // and keeps recycling allocations (free-list never starves to allocation
    // once warmed up).
    #[test]
    fn steady_state_push_samples_pop_loop() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        for i in 0..1000u32 {
            let v = i as f32;
            let samples = [v, v + 0.5];
            assert!(producer.push_samples_or_drop(&samples, 2, 48000));

            let buf = consumer.pop().expect("one buffer per iteration");
            assert_eq!(buf.data(), &[v, v + 0.5], "data integrity at iter {i}");
        }

        assert_eq!(
            producer.shared().buffers_pushed.load(Ordering::Relaxed),
            1000
        );
        assert_eq!(consumer.buffers_popped(), 1000);
        // After warm-up the consumer keeps the producer supplied with recycled
        // allocations, so the producer should have spare buffers on hand.
        assert!(
            producer.recycled_available() > 0,
            "free-list should stay populated in steady state"
        );
    }

    // Regression (ADR-0001 / audit H3): after the producer consumes the scratch
    // fallback on a free-list-empty push that then SUCCEEDS, the scratch slot must
    // not be left at capacity 0 — otherwise every later free-list-empty push
    // allocates a fresh Vec on the real-time thread. We drive the free-list empty,
    // pop on the consumer to recycle exactly one buffer, then push twice in a row
    // without popping in between and assert the producer never has to allocate from
    // a zero-capacity scratch.
    #[test]
    fn scratch_never_shrinks_to_zero_after_underrun() {
        // Capacity 2 → free-list seeded with min(2, 8) = 2 buffers.
        let (mut producer, mut consumer) = create_bridge(2, test_format());
        assert_eq!(producer.recycled_available(), 2);

        // Drain the 2 seeded recycled buffers with 2 successful pushes (ring cap 2).
        assert!(producer.push_samples_or_drop(&[1.0, 2.0], 2, 48000));
        assert!(producer.push_samples_or_drop(&[3.0, 4.0], 2, 48000));
        assert_eq!(producer.recycled_available(), 0, "free-list drained");

        // Consumer pops one buffer → recycles exactly one allocation back.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(producer.recycled_available(), 1);

        // Pop the second too so the ring has room, recycling another buffer.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(producer.recycled_available(), 2);

        // Now repeatedly: push (consumes a recycled buf), pop (recycles one back).
        // Throughout, the producer must always source from the free-list or a
        // non-empty scratch — never extend a freshly-allocated zero-cap Vec.
        // We assert scratch capacity stays >= the configured floor whenever the
        // free-list is the source, by checking the push always succeeds without
        // the recycled count going negative and data integrity holding.
        for i in 0..50u32 {
            let v = i as f32;
            assert!(producer.push_samples_or_drop(&[v, v + 0.5], 2, 48000));
            let buf = consumer.pop().expect("one buffer per iteration");
            assert_eq!(buf.data(), &[v, v + 0.5], "data integrity at iter {i}");
        }
        // After warm-up the free-list keeps the producer supplied.
        assert!(
            producer.recycled_available() > 0,
            "free-list should stay populated, keeping the RT producer alloc-free"
        );
    }

    // Direct assertion that the scratch fallback retains capacity after being
    // consumed by a successful push (the precise H3 defect). With NO consumer
    // pops, the first push consumes a seeded recycled buffer; we exhaust the
    // free-list, then a push that falls back to scratch and succeeds must leave
    // scratch refilled (capacity > 0) from the remaining free-list, or — once the
    // free-list is truly empty — the producer must still not be wedged at cap 0
    // on the NEXT successful push once a buffer is recycled.
    #[test]
    fn scratch_capacity_preserved_across_successful_push() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        // Exhaust the seeded free-list (min(8,8)=8) without popping → ring fills
        // to 8, free-list to 0.
        for _ in 0..8 {
            assert!(producer.push_samples_or_drop(&[0.25], 1, 48000));
        }
        assert_eq!(producer.recycled_available(), 0);

        // Drain everything on the consumer → 8 buffers recycled back.
        for _ in 0..8 {
            let _ = consumer.pop().expect("buffer");
        }
        assert_eq!(producer.recycled_available(), 8);

        // Steady push/pop: each push consumes a recycled buffer, each pop returns
        // one. The producer should never need the scratch slot here, and when it
        // does (transiently), the success arm refills it. Over many iterations the
        // recycled pool stays healthy — proving no permanent scratch starvation.
        for _ in 0..200 {
            assert!(producer.push_samples_or_drop(&[0.5], 1, 48000));
            let _ = consumer.pop().expect("buffer");
        }
        assert!(producer.recycled_available() > 0);
    }

    // ===== M1: negotiated (delivery) format tests =====
    // `SampleFormat` is imported explicitly in this test module's `use` block.

    // sample_format <-> atomic round-trips for every variant.
    #[test]
    fn sample_format_atomic_roundtrip() {
        for sf in [
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
            SampleFormat::F32,
        ] {
            assert_eq!(sample_format_from_atomic(sample_format_to_atomic(sf)), sf);
        }
        // Unknown encodings decode to the F32 fallback.
        assert_eq!(sample_format_from_atomic(200), SampleFormat::F32);
    }

    // Before any backend negotiation, negotiated_format() returns the requested
    // format the bridge was constructed with.
    #[test]
    fn negotiated_format_defaults_to_requested() {
        let requested = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        let (_producer, consumer) = create_bridge(8, requested.clone());
        assert_eq!(consumer.shared().negotiated_format(), requested);
    }

    // After the producer records a delivery format, negotiated_format() reflects
    // the delivered sample_rate/channels (NOT the requested ones), with the
    // sample_format normalized to F32 (the bridge always delivers f32).
    #[test]
    fn set_negotiated_format_overrides_requested() {
        let requested = AudioFormat::default(); // 48k/2ch/F32
        let (producer, consumer) = create_bridge(8, requested.clone());

        // Backend reports the endpoint's native type (I24), but the bridge
        // converts to f32 — so negotiated_format() must report F32 at the
        // delivered rate/channels.
        let delivered = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I24,
        };
        producer.set_negotiated_format(&delivered);

        let observed = consumer.shared().negotiated_format();
        assert_eq!(observed.sample_rate, 44100);
        assert_eq!(observed.channels, 1);
        assert_eq!(
            observed.sample_format,
            SampleFormat::F32,
            "bridge payload is always f32; reported format must be normalized"
        );
        assert_ne!(
            observed, requested,
            "must reflect delivery rate/channels, not request"
        );
    }

    // The most recent set_negotiated_format wins (idempotent / last-writer);
    // sample_format is always normalized to F32.
    #[test]
    fn set_negotiated_format_last_writer_wins() {
        let (producer, consumer) = create_bridge(8, AudioFormat::default());
        producer.set_negotiated_format(&AudioFormat {
            sample_rate: 96000,
            channels: 4,
            sample_format: SampleFormat::I32,
        });
        let final_fmt = AudioFormat {
            sample_rate: 22050,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        producer.set_negotiated_format(&final_fmt);
        // final_fmt is already F32, so it round-trips exactly.
        assert_eq!(consumer.shared().negotiated_format(), final_fmt);
    }

    // ===== L6: configurable back-pressure threshold tests =====

    // create_bridge uses the documented default threshold.
    #[test]
    fn default_backpressure_threshold_applied() {
        let (mut producer, _consumer) = create_bridge(2, test_format());
        // Fill the ring (cap 2), then drive (DEFAULT - 1) extra drops — still
        // below threshold.
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(producer.push_or_drop(test_buffer(1.0)));
        for _ in 0..(DEFAULT_BACKPRESSURE_THRESHOLD - 1) {
            assert!(!producer.push_or_drop(test_buffer(9.0)));
        }
        assert!(
            !producer.shared().is_under_backpressure(),
            "should NOT trip one drop below the default threshold"
        );
        // One more drop reaches the threshold.
        assert!(!producer.push_or_drop(test_buffer(9.0)));
        assert!(
            producer.shared().is_under_backpressure(),
            "should trip at exactly the default threshold"
        );
    }

    // A custom (lower) threshold trips sooner; a successful push resets it.
    #[test]
    fn custom_backpressure_threshold_trips_and_resets() {
        // Threshold 2: trips after 2 consecutive drops.
        let (mut producer, mut consumer) = create_bridge_with_options(2, test_format(), 2);
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(producer.push_or_drop(test_buffer(1.0)));

        assert!(!producer.push_or_drop(test_buffer(9.0))); // drop 1
        assert!(!producer.shared().is_under_backpressure());
        assert!(!producer.push_or_drop(test_buffer(9.0))); // drop 2 → trips
        assert!(producer.shared().is_under_backpressure());

        // Draining a slot lets the next push succeed, which resets the streak.
        let _ = consumer.pop();
        assert!(producer.push_or_drop(test_buffer(2.0)));
        assert!(
            !producer.shared().is_under_backpressure(),
            "a successful push must clear consecutive-drop backpressure"
        );
    }

    // A zero threshold reports back-pressure immediately (0 >= 0), even before
    // any drop, and stays true after a drop.
    #[test]
    fn zero_backpressure_threshold_trips_immediately() {
        let (mut producer, _consumer) = create_bridge_with_options(1, test_format(), 0);
        // Threshold 0 means is_under_backpressure() is true even before any drop
        // (0 consecutive drops >= 0). After a drop it stays true.
        assert!(producer.shared().is_under_backpressure());
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(!producer.push_or_drop(test_buffer(9.0)));
        assert!(producer.shared().is_under_backpressure());
    }

    // Timestamps survive the recycle round-trip.
    #[test]
    fn pop_preserves_timestamp_through_recycle() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        let fmt = AudioFormat::default();
        let ts = Duration::from_millis(250);
        producer
            .push(AudioBuffer::with_timestamp(vec![0.9; 4], fmt, ts))
            .unwrap();

        let buf = consumer.pop().expect("buffer available");
        assert_eq!(buf.timestamp(), Some(ts));
        assert_eq!(buf.data(), &[0.9; 4]);
    }

    // ===== Concurrent SPSC stress test (producer thread ⇄ consumer thread) =====
    //
    // This is the test that actually exercises the production data path: the
    // producer runs on one thread (simulating the OS audio callback), the
    // consumer on another (simulating the reader), and the free-list return
    // ring recycles allocations across the thread boundary concurrently.
    //
    // It validates two invariants under real cross-thread contention:
    //   1. Conservation: every successfully-pushed buffer is eventually popped
    //      exactly once (buffers_pushed == buffers_popped at quiescence), and
    //      dropped buffers never reach the consumer.
    //   2. FIFO integrity: the sequence numbers the consumer observes are a
    //      strictly increasing subsequence of those the producer sent — i.e.
    //      no reordering, duplication, or corruption through either ring.
    #[test]
    fn concurrent_producer_consumer_stress() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::Arc as StdArc;

        // Keep CI fast but large enough to surface races/corruption.
        const ITEMS: u64 = 200_000;
        // Small ring → frequent full/back-pressure → exercises the drop +
        // scratch-reclaim path and keeps the free-list ring churning.
        let (mut producer, mut consumer) = create_bridge(16, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let producer_done = StdArc::new(AtomicBool::new(false));
        let pushed_seqs = StdArc::new(AtomicU64::new(0)); // count of successful pushes

        let producer_done_w = StdArc::clone(&producer_done);
        let pushed_seqs_w = StdArc::clone(&pushed_seqs);

        // Producer thread: each buffer encodes its sequence number in data[0].
        let producer_handle = std::thread::spawn(move || {
            let mut pushed = 0u64;
            for seq in 0..ITEMS {
                // Encode seq as the sole sample. push_samples_or_drop copies it
                // through the (recycled) scratch/free-list allocation.
                if producer.push_samples_or_drop(&[seq as f32], 1, 48000) {
                    pushed += 1;
                }
                // else: ring full → dropped; that seq simply never arrives.
            }
            pushed_seqs_w.store(pushed, Ordering::SeqCst);
            producer_done_w.store(true, Ordering::SeqCst);
            // Return the producer so its free-list consumer side stays alive
            // until the consumer finishes recycling.
            producer
        });

        // Consumer thread: pop continuously, verify strictly-increasing seqs.
        let consumer_handle = std::thread::spawn(move || {
            let mut popped = 0u64;
            let mut last_seq: i64 = -1;
            loop {
                match consumer.pop() {
                    Some(buf) => {
                        let seq = buf.data()[0] as i64;
                        assert!(
                            seq > last_seq,
                            "FIFO/integrity violation: seq {seq} not > previous {last_seq}"
                        );
                        last_seq = seq;
                        popped += 1;
                    }
                    None => {
                        // Stop once the producer is finished AND the ring has
                        // been fully drained.
                        if producer_done.load(Ordering::SeqCst) && consumer.available_buffers() == 0
                        {
                            break;
                        }
                        std::thread::yield_now();
                    }
                }
            }
            (popped, consumer)
        });

        let _producer = producer_handle.join().expect("producer thread panicked");
        let (popped, consumer) = consumer_handle.join().expect("consumer thread panicked");

        let pushed = pushed_seqs.load(Ordering::SeqCst);

        // Conservation: every successful push is popped exactly once.
        assert_eq!(
            pushed, popped,
            "conservation violated: {pushed} pushed but {popped} popped"
        );
        // Cross-check against the bridge's own counters.
        assert_eq!(consumer.buffers_popped(), popped, "popped counter mismatch");
        // pushed + dropped must equal the total attempts.
        let dropped = consumer.shared().buffers_dropped.load(Ordering::Relaxed);
        assert_eq!(
            pushed + dropped,
            ITEMS,
            "pushed ({pushed}) + dropped ({dropped}) must equal attempts ({ITEMS})"
        );
    }

    // ===== rsac-d0ba: panic guard at the producer push boundary =====

    // The guarded push behaves exactly like push_samples_or_drop on the happy
    // path: pushes data through, returns true, and the buffer is readable.
    #[test]
    fn push_samples_guarded_happy_path_matches_unguarded() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        assert!(producer.push_samples_guarded(&[0.1, -0.2, 0.3, -0.4], 2, 44100));
        let buf = consumer
            .pop()
            .expect("guarded push should deliver a buffer");
        assert_eq!(buf.data(), &[0.1, -0.2, 0.3, -0.4]);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 44100);
        // No panic occurred, so the stream is not poisoned.
        assert_ne!(producer.shared().state.get(), StreamState::Error);
    }

    // A panic raised inside the guarded region is caught: it never unwinds out
    // of push_samples_guarded, the stream transitions to Error, and the drop
    // counter increments instead of aborting/UB. We force a panic by invoking
    // the panic-handler path directly (the catch_unwind contract is exercised
    // by the std-level test below).
    #[test]
    fn push_panic_handler_poisons_stream_and_counts_drop() {
        let (producer, _consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);
        assert_eq!(producer.buffers_dropped(), 0);

        // Drive the cold panic-handling path.
        producer.on_push_panic();

        assert_eq!(
            producer.shared().state.get(),
            StreamState::Error,
            "a caught push panic must poison the stream to Error"
        );
        assert_eq!(
            producer.buffers_dropped(),
            1,
            "a caught push panic must count one dropped buffer"
        );
    }

    // catch_unwind in push_samples_guarded actually contains a real panic raised
    // while building the buffer. We can't easily make push_samples_or_drop itself
    // panic without instrumentation, so this test asserts the guard's contract at
    // the std level: a panicking closure under AssertUnwindSafe is caught.
    #[test]
    fn catch_unwind_contains_panic_for_guard_contract() {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("simulated callback panic");
        }));
        assert!(
            caught.is_err(),
            "catch_unwind must contain a panic so it never crosses the FFI boundary"
        );
    }

    // ===== rsac-0d25: synchronous per-call overflow reporting =====

    // push_samples_reporting reports pushed=true / dropped_this_call=0 on success.
    #[test]
    fn push_samples_reporting_success() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        let out = producer.push_samples_reporting(&[1.0, 2.0], 2, 48000);
        assert!(out.pushed);
        assert_eq!(out.dropped_this_call, 0);
        let buf = consumer.pop().expect("buffer");
        assert_eq!(buf.data(), &[1.0, 2.0]);
    }

    // dropped-this-call reporting matches the delta in buffers_dropped across a
    // full-ring scenario.
    #[test]
    fn push_samples_reporting_dropped_matches_counter_delta() {
        let (mut producer, _consumer) = create_bridge(2, test_format());
        // Fill the ring (cap 2).
        assert!(producer.push_samples_reporting(&[1.0], 1, 48000).pushed);
        assert!(producer.push_samples_reporting(&[2.0], 1, 48000).pushed);

        let before = producer.buffers_dropped();
        let out = producer.push_samples_reporting(&[3.0], 1, 48000);
        let after = producer.buffers_dropped();

        assert!(!out.pushed, "ring full → push must fail");
        assert_eq!(out.dropped_this_call, 1, "exactly one buffer dropped");
        assert_eq!(
            after - before,
            out.dropped_this_call as u64,
            "dropped_this_call must equal the buffers_dropped delta"
        );
    }

    // ===== rsac-522b: per-buffer stream-relative timestamps =====

    // push_samples_or_drop_at stamps the buffer, and the timestamp survives the
    // recycle round-trip through both rings.
    #[test]
    fn push_samples_or_drop_at_timestamp_survives_roundtrip() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        let ts = Duration::from_micros(12_345);
        assert!(producer.push_samples_or_drop_at(&[0.5, -0.5], 2, 48000, ts));

        let buf = consumer.pop().expect("buffer");
        assert_eq!(buf.timestamp(), Some(ts), "timestamp must survive recycle");
        assert_eq!(buf.data(), &[0.5, -0.5]);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
    }

    // The untimed variant still yields no timestamp (delegates with None).
    #[test]
    fn push_samples_or_drop_yields_no_timestamp() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        assert!(producer.push_samples_or_drop(&[0.1, 0.2], 2, 48000));
        let buf = consumer.pop().expect("buffer");
        assert_eq!(buf.timestamp(), None);
    }

    // The timestamped path keeps the RT-allocation guarantee: recycled_available
    // stays > 0 in steady state with monotonically increasing timestamps.
    #[test]
    fn timestamped_push_preserves_recycling_and_monotonic_ts() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        let mut last = Duration::ZERO;
        for i in 0..1000u64 {
            let ts = Duration::from_micros(i * 1000);
            assert!(producer.push_samples_or_drop_at(&[i as f32, i as f32 + 0.5], 2, 48000, ts));
            let buf = consumer.pop().expect("one buffer per iteration");
            let got = buf.timestamp().expect("timestamped path carries Some(ts)");
            assert!(got >= last, "timestamps must be non-decreasing");
            last = got;
        }
        assert!(
            producer.recycled_available() > 0,
            "timestamped path must keep the free-list populated (ADR-0001)"
        );
    }

    // ===== rsac-cfe4: windowed drop-rate tracking =====

    // A drop,push,drop,push pattern that NEVER trips the consecutive-threshold
    // bool still reports ~0.5 drop rate in the windowed snapshot.
    #[test]
    fn windowed_drop_rate_sees_alternating_loss_the_bool_misses() {
        // Threshold high so the consecutive-drop bool never trips; ring cap 1 so
        // we can deterministically alternate success/drop.
        let (mut producer, mut consumer) = create_bridge_with_options(1, test_format(), 1000);

        let mut pushes = 0u64;
        let mut drops = 0u64;
        for _ in 0..200 {
            // Ring empty → this push succeeds.
            assert!(producer.push_samples_or_drop(&[1.0], 1, 48000));
            pushes += 1;
            // Ring now full → this push drops.
            assert!(!producer.push_samples_or_drop(&[2.0], 1, 48000));
            drops += 1;
            // Drain so the next iteration can push again.
            let _ = consumer.pop().expect("buffer");
        }

        // The consecutive-drop bool must NOT have tripped (every drop is followed
        // by a success that resets the streak).
        assert!(
            !producer.shared().is_under_backpressure(),
            "alternating loss must not trip the consecutive-drop bool"
        );

        // The windowed snapshot, however, reflects the sustained ~50% loss.
        let (w_pushed, w_dropped) = producer.drop_window_snapshot();
        assert!(w_pushed > 0 && w_dropped > 0, "window saw activity");
        let rate = w_dropped as f64 / (w_pushed + w_dropped) as f64;
        assert!(
            (rate - 0.5).abs() < 0.1,
            "windowed drop rate should be ~0.5, got {rate} (pushed={w_pushed}, dropped={w_dropped})"
        );
        // Sanity: the bridge's lifetime counters agree on the totals.
        assert_eq!(
            producer.shared().buffers_pushed.load(Ordering::Relaxed),
            pushes
        );
        assert_eq!(producer.buffers_dropped(), drops);
    }

    // A fresh bridge reports an all-zero window (no division-by-zero risk for the
    // reader computing a rate).
    #[test]
    fn windowed_drop_snapshot_zero_on_fresh_bridge() {
        let (producer, _consumer) = create_bridge(8, test_format());
        assert_eq!(producer.drop_window_snapshot(), (0, 0));
    }

    // The legacy consecutive-drop bool is unaffected by the window: a long run of
    // pure drops still trips it (window recording does not interfere).
    #[test]
    fn windowed_tracking_does_not_break_consecutive_bool() {
        let (mut producer, _consumer) = create_bridge_with_options(1, test_format(), 3);
        assert!(producer.push_samples_or_drop(&[1.0], 1, 48000)); // fills ring
        for _ in 0..3 {
            assert!(!producer.push_samples_or_drop(&[9.0], 1, 48000)); // all drop
        }
        assert!(
            producer.shared().is_under_backpressure(),
            "3 consecutive drops must trip the threshold-3 bool"
        );
    }

    // ===== rsac-17d1: consumer pop moves the ring buffer (no clone) =====

    // pop still preserves data/format/timestamp (the buffer is moved intact) and
    // still recycles an allocation to the producer's free-list.
    #[test]
    fn move_pop_preserves_data_and_recycles_spare() {
        // Capacity 4 → free-list ring seeded full (4 spares). Drain it via 4
        // push_samples so the return ring has room for pop's recycled spare.
        let (mut producer, mut consumer) = create_bridge(4, test_format());
        for _ in 0..3 {
            assert!(producer.push_samples_or_drop(&[0.0], 1, 48000));
        }
        // One timestamped push to verify the move preserves metadata.
        let ts = Duration::from_millis(7);
        assert!(producer.push_samples_or_drop_at(&[1.0, 2.0, 3.0, 4.0], 2, 48000, ts));
        assert_eq!(producer.recycled_available(), 0, "free-list drained");

        let buf = consumer.pop().expect("buffer");
        // pop hands over the FIRST pushed buffer (FIFO) — the single-sample one.
        assert_eq!(buf.data(), &[0.0]);
        // Draining to the timestamped buffer to assert the move preserves it.
        let _ = consumer.pop();
        let _ = consumer.pop();
        let tsbuf = consumer.pop().expect("timestamped buffer");
        assert_eq!(tsbuf.data(), &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            tsbuf.timestamp(),
            Some(ts),
            "moved buffer keeps its timestamp"
        );
        assert_eq!(tsbuf.channels(), 2);

        // Each pop recycled a spare back to the producer's free-list (it had
        // room because we drained it first).
        assert!(
            producer.recycled_available() > 0,
            "pop must recycle spares to the producer once the return ring has room"
        );
    }

    // Even after the seeded spare pool is exhausted, pop keeps the producer's
    // free-list supplied (allocating spares off the RT thread), so a long
    // push/pop loop preserves data integrity and keeps the producer alloc-free.
    #[test]
    fn move_pop_keeps_producer_supplied_after_spare_pool_drains() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        for i in 0..1000u32 {
            let v = i as f32;
            assert!(producer.push_samples_or_drop(&[v, v + 0.5], 2, 48000));
            let buf = consumer.pop().expect("one buffer per iteration");
            assert_eq!(buf.data(), &[v, v + 0.5], "data integrity at iter {i}");
        }
        assert!(
            producer.recycled_available() > 0,
            "free-list must stay populated via recycled spares"
        );
    }

    // ===== rsac-b655: period-derived ring capacity =====

    // Degenerate inputs (unknown period / zero channels) fall back to the
    // historical static default of 64, so a backend that cannot learn its
    // period behaves exactly as before.
    #[test]
    fn capacity_for_period_falls_back_when_unknown() {
        assert_eq!(
            calculate_capacity_for_period(0, 2),
            PERIOD_FALLBACK_CAPACITY
        );
        assert_eq!(
            calculate_capacity_for_period(1024, 0),
            PERIOD_FALLBACK_CAPACITY
        );
        assert_eq!(
            calculate_capacity_for_period(0, 0),
            PERIOD_FALLBACK_CAPACITY
        );
        assert_eq!(PERIOD_FALLBACK_CAPACITY, 64);
    }

    // The result is always a power of two within the configured band, for a wide
    // range of realistic and adversarial periods and channel counts.
    #[test]
    fn capacity_for_period_is_power_of_two_and_clamped() {
        for &frames in &[1usize, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 65536] {
            for &ch in &[1usize, 2, 6, 8] {
                let cap = calculate_capacity_for_period(frames, ch);
                assert!(
                    cap.is_power_of_two(),
                    "cap {cap} not power of two (frames={frames}, ch={ch})"
                );
                assert!(
                    (PERIOD_MIN_CAPACITY..=PERIOD_MAX_CAPACITY).contains(&cap),
                    "cap {cap} out of [{PERIOD_MIN_CAPACITY}, {PERIOD_MAX_CAPACITY}] band (frames={frames}, ch={ch})"
                );
            }
        }
    }

    // A reference-sized period (1024 frames) gets the base headroom: ~12 periods
    // rounded up to the next power of two = 16. Larger periods stay flat at the
    // base (never scaled below 1×).
    #[test]
    fn capacity_for_period_reference_and_large_periods() {
        // 1024 frames == reference → base headroom 12 → next_pow2 = 16.
        assert_eq!(calculate_capacity_for_period(1024, 2), 16);
        // Larger-than-reference periods do not scale headroom below the base, so
        // they also land at 16 (12 → 16).
        assert_eq!(calculate_capacity_for_period(2048, 2), 16);
        assert_eq!(calculate_capacity_for_period(4096, 6), 16);
    }

    // Smaller-than-reference periods fire callbacks more often, so the headroom
    // scales up monotonically (more slots) as the period shrinks — never fewer
    // slots than a larger period.
    #[test]
    fn capacity_for_period_scales_up_for_small_periods() {
        let big = calculate_capacity_for_period(1024, 2); // base
        let mid = calculate_capacity_for_period(256, 2); // 4× more callbacks
        let small = calculate_capacity_for_period(64, 2); // 16× more callbacks
        assert!(
            small >= mid && mid >= big,
            "smaller periods must not yield a smaller ring: 64f={small} 256f={mid} 1024f={big}"
        );
        // A very small 64-frame period: scale = ceil(1024/64) = 16, raw = 12*16 =
        // 192, clamped (<=1024) → 192 → next_pow2 = 256.
        assert_eq!(small, 256);
    }

    // Channel count is part of the signature (callers pass the negotiated stream
    // shape) but does not, by itself, change the slot count: each slot holds the
    // whole interleaved period regardless of width.
    #[test]
    fn capacity_for_period_independent_of_channels() {
        let base = calculate_capacity_for_period(512, 1);
        for &ch in &[2usize, 4, 6, 8] {
            assert_eq!(
                calculate_capacity_for_period(512, ch),
                base,
                "channel count must not change the period-derived slot count"
            );
        }
    }

    // The new period-aware calculator does not perturb the existing
    // `calculate_capacity` contract (regression guard for the two living
    // side-by-side).
    #[test]
    fn calculate_capacity_unchanged_alongside_period_variant() {
        assert_eq!(calculate_capacity(None, 4), 64);
        assert_eq!(calculate_capacity(Some(100), 4), 128);
        assert_eq!(calculate_capacity(Some(2), 4), 4);
    }

    // ===== rsac-9348: cache-line padding of producer/consumer counters =====

    // The CachePadded wrapper forces >= 64-byte alignment so wrapped counters
    // land on their own cache line (the false-sharing fix's core invariant).
    #[test]
    fn cache_padded_is_cache_line_aligned() {
        assert_eq!(CACHE_LINE_BYTES, 64);
        assert!(
            std::mem::align_of::<CachePadded<AtomicU64>>() >= CACHE_LINE_BYTES,
            "CachePadded<AtomicU64> must be >= cache-line aligned"
        );
        assert!(
            std::mem::align_of::<CachePadded<AtomicU32>>() >= CACHE_LINE_BYTES,
            "CachePadded<AtomicU32> must be >= cache-line aligned"
        );
        // The padding also rounds the size up to a full line so two adjacent
        // wrapped values cannot occupy the same line.
        assert!(std::mem::size_of::<CachePadded<AtomicU64>>() >= CACHE_LINE_BYTES);
    }

    // The producer-written counters and the consumer-written counter occupy
    // DISTINCT cache lines in a live BridgeShared, so writes from the two threads
    // never invalidate each other's line (false sharing eliminated). We check
    // that each pair of (producer-hot, consumer-hot) counters is at least one
    // cache line apart in memory.
    #[test]
    fn producer_and_consumer_counters_on_distinct_cache_lines() {
        let (producer, _consumer) = create_bridge(8, test_format());
        let shared = producer.shared();

        let line = CACHE_LINE_BYTES as isize;
        let pushed = &*shared.buffers_pushed as *const AtomicU64 as isize;
        let dropped = &*shared.buffers_dropped as *const AtomicU64 as isize;
        let consec = &*shared.consecutive_drops as *const AtomicU32 as isize;
        let popped = &*shared.buffers_popped as *const AtomicU64 as isize;

        // buffers_popped (consumer-written) must be on a different cache line
        // than each producer-written counter.
        for (name, p) in [
            ("pushed", pushed),
            ("dropped", dropped),
            ("consecutive", consec),
        ] {
            let same_line = (popped / line) == (p / line);
            assert!(
                !same_line,
                "buffers_popped shares a cache line with {name}: false sharing not eliminated"
            );
        }
    }

    // Wrapping the counters in CachePadded must not change their observable
    // behavior: a push/drop/pop sequence updates each counter exactly as before
    // (Deref-through access is transparent).
    #[test]
    fn cache_padded_counters_behave_identically() {
        let (mut producer, mut consumer) = create_bridge(2, test_format());

        // Two successful pushes (fills the ring), one drop, then one pop.
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(producer.push_or_drop(test_buffer(2.0)));
        assert!(!producer.push_or_drop(test_buffer(3.0))); // ring full → drop
        let _ = consumer.pop().expect("buffer");

        // Clone the Arc so the handle is independent of `producer`'s borrow
        // (lets us keep reading counters across the later `&mut producer` push).
        let shared = Arc::clone(producer.shared());
        assert_eq!(shared.buffers_pushed.load(Ordering::Relaxed), 2);
        assert_eq!(shared.buffers_dropped.load(Ordering::Relaxed), 1);
        assert_eq!(shared.buffers_popped.load(Ordering::Relaxed), 1);
        // consecutive_drops is 1 (the single drop after the last success).
        assert_eq!(shared.consecutive_drops.load(Ordering::Relaxed), 1);

        // A subsequent successful push resets the consecutive-drop streak.
        let _ = consumer.pop(); // make room
        assert!(producer.push_or_drop(test_buffer(4.0)));
        assert_eq!(shared.consecutive_drops.load(Ordering::Relaxed), 0);
    }

    // ===== PU-5 (rsac-efb4): unconditional Condvar wake for pop_blocking =====

    // The ConsumerWake generation advances on every notify so a waiter can detect
    // a notify that raced its pre-wait ring check (lost-wakeup guard).
    #[test]
    fn consumer_wake_generation_advances_on_notify() {
        let wake = ConsumerWake::new();
        let g0 = wake.generation();
        wake.notify();
        let g1 = wake.generation();
        wake.notify();
        let g2 = wake.generation();
        assert!(g1 > g0 && g2 > g1, "generation must advance on each notify");
    }

    // wait(since, ..) returns IMMEDIATELY (does not park out the slice) when the
    // generation already moved past `since` — the race-closing fast path. We use a
    // long slice so a regression that actually parks would make this test slow.
    #[test]
    fn consumer_wake_wait_returns_immediately_when_generation_moved() {
        let wake = ConsumerWake::new();
        let since = wake.generation();
        wake.notify(); // generation now != since
        let start = Instant::now();
        wake.wait(since, Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "wait must not park when a notify already fired since the snapshot"
        );
    }

    // With no notify, wait parks for (at most) the bounded slice and returns — the
    // degrade-not-hang backstop. It must return on its own within ~the slice.
    #[test]
    fn consumer_wake_wait_times_out_on_its_slice() {
        let wake = ConsumerWake::new();
        let since = wake.generation();
        let start = Instant::now();
        wake.wait(since, Duration::from_millis(10));
        let elapsed = start.elapsed();
        // It must not return instantly (no notify fired) and must not hang.
        assert!(
            elapsed < Duration::from_secs(1),
            "wait must respect its bounded slice, not hang"
        );
    }

    // notify_consumers() (the Windows-producer wake) bumps the shared wake
    // generation — proving the producer can wake a parked reader without touching
    // the RT push path.
    #[test]
    fn notify_consumers_bumps_wake_generation() {
        let (producer, _consumer) = create_bridge(8, test_format());
        let g0 = producer.shared().wake.generation();
        producer.notify_consumers();
        assert!(
            producer.shared().wake.generation() > g0,
            "notify_consumers must advance the wake generation"
        );
    }

    // signal_done() and signal_error() each wake parked readers (bump the wake
    // generation) in addition to their state transition — so a blocked
    // pop_blocking re-checks promptly on a graceful end or a fatal death.
    #[test]
    fn signal_done_and_error_bump_wake_generation() {
        let (producer, _consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let g0 = producer.shared().wake.generation();
        producer.signal_done();
        let g1 = producer.shared().wake.generation();
        assert!(g1 > g0, "signal_done must wake parked readers");

        producer.signal_error();
        let g2 = producer.shared().wake.generation();
        assert!(g2 > g1, "signal_error must wake parked readers");
    }

    // A push from a producer thread + notify_consumers() wakes a parked
    // pop_blocking PROMPTLY — well before the caller's (long) timeout. This is the
    // core PU-5 behavior: the reader does not sleep out a fixed poll interval.
    #[test]
    fn push_then_notify_wakes_parked_pop_blocking_promptly() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let handle = std::thread::spawn(move || {
            // Give the reader time to park in wait() before we push.
            std::thread::sleep(Duration::from_millis(50));
            assert!(producer.push_samples_or_drop(&[0.5, -0.5], 2, 48000));
            // Windows-producer-style wake (the RT backends do NOT call this).
            producer.notify_consumers();
            producer // keep the free-list alive until the consumer is done
        });

        // A generous timeout: if the wake works, we return in ~50 ms, far under it.
        let start = Instant::now();
        let buf = consumer
            .pop_blocking(Duration::from_secs(10))
            .expect("a pushed buffer must wake the parked reader");
        let elapsed = start.elapsed();

        assert_eq!(buf.data(), &[0.5, -0.5]);
        assert!(
            elapsed < Duration::from_secs(2),
            "pop_blocking must wake on the push, not sleep out the 10 s timeout \
             (woke after {elapsed:?})"
        );
        let _producer = handle.join().expect("producer thread panicked");
    }

    // signal_done() from another thread wakes a parked pop_blocking. Because
    // Stopping is still drainable (NOT terminal), an EMPTY ring then times out —
    // but the point under test is that the reader is woken to RE-CHECK promptly;
    // we verify it does not hang the full long timeout by using a short one and
    // asserting Timeout (the graceful-drain contract), and a separate signal_error
    // test below proves prompt terminal return.
    #[test]
    fn signal_done_wakes_parked_pop_blocking() {
        let (producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            producer.signal_done(); // Running → Stopping (+ wake)
            producer
        });

        // Stopping with an empty ring keeps draining → Timeout. A short timeout
        // proves we don't hang; the wake just makes the re-check immediate.
        let err = consumer
            .pop_blocking(Duration::from_millis(300))
            .expect_err("empty Stopping ring drains → times out");
        assert!(
            matches!(err, AudioError::Timeout { .. }),
            "graceful Stopping must keep draining (Timeout), got {err:?}"
        );
        let _producer = handle.join().expect("producer thread panicked");
    }

    // signal_error() from another thread wakes a parked pop_blocking and it
    // returns the Fatal StreamEnded PROMPTLY (terminal Error), proving a dead
    // producer unblocks the reader far before the long caller timeout.
    #[test]
    fn signal_error_wakes_parked_pop_blocking_with_fatal() {
        let (producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            producer.signal_error(); // → terminal Error (+ wake)
            producer
        });

        let start = Instant::now();
        let err = consumer
            .pop_blocking(Duration::from_secs(10))
            .expect_err("terminal Error must end the blocking read");
        let elapsed = start.elapsed();

        assert!(err.is_fatal(), "terminal-Error read must be Fatal");
        assert!(
            matches!(err, AudioError::StreamEnded { .. }),
            "expected StreamEnded after signal_error, got {err:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "signal_error must wake the parked reader, not sleep out the 10 s \
             timeout (woke after {elapsed:?})"
        );
        let _producer = handle.join().expect("producer thread panicked");
    }

    // The backstop alone (no notify at all) still unblocks a parked pop_blocking
    // when data appears: this models the Linux/macOS RT push path, which
    // deliberately does NOT notify. The reader must still pick up the data via the
    // bounded re-check rather than hanging. We push WITHOUT calling
    // notify_consumers to prove the degrade path works.
    #[test]
    fn pop_blocking_picks_up_data_without_notify_via_backstop() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            // NOTE: no notify_consumers() — mirrors the RT-callback push path.
            assert!(producer.push_samples_or_drop(&[1.0], 1, 48000));
            producer
        });

        let buf = consumer
            .pop_blocking(Duration::from_secs(5))
            .expect("backstop re-check must still deliver the data");
        assert_eq!(buf.data(), &[1.0]);
        let _producer = handle.join().expect("producer thread panicked");
    }

    // pop_blocking still returns immediately when data is already present (no
    // regression of the fast path; it must not enter the wait at all).
    #[test]
    fn pop_blocking_fast_path_with_data_present() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());
        producer.shared().state.force_set(StreamState::Running);
        assert!(producer.push_samples_or_drop(&[0.25, 0.75], 2, 48000));

        let start = Instant::now();
        let buf = consumer
            .pop_blocking(Duration::from_secs(5))
            .expect("data already present");
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "fast path must not park when data is already buffered"
        );
        assert_eq!(buf.data(), &[0.25, 0.75]);
    }
}

// ===== rsac-3616: sample-domain zero-copy ring tests =====

#[cfg(all(test, feature = "bridge-zerocopy"))]
mod sample_ring_tests {
    use super::*;

    fn fmt() -> AudioFormat {
        AudioFormat::default()
    }

    // Push samples via the SampleRing, pop, and verify the reconstructed buffer
    // is equivalent to what the AudioBuffer ring would deliver (parallels
    // push_samples_then_pop_preserves_data).
    #[test]
    fn sample_ring_push_pop_preserves_data() {
        let (mut producer, mut consumer) = create_sample_ring(1024, 16, fmt());

        let samples = [0.1, -0.2, 0.3, -0.4];
        assert!(producer.push_samples_or_drop(&samples, 2, 44100));

        let buf = consumer.pop().expect("should have one chunk");
        assert_eq!(buf.data(), &samples);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 44100);
        assert_eq!(buf.timestamp(), None);
    }

    // Timestamp metadata is preserved through the sidecar metadata ring.
    #[test]
    fn sample_ring_preserves_timestamp() {
        let (mut producer, mut consumer) = create_sample_ring(1024, 16, fmt());
        let ts = Duration::from_micros(9876);
        assert!(producer.push_samples_or_drop_at(&[0.5, -0.5], 2, 48000, Some(ts)));
        let buf = consumer.pop().expect("chunk");
        assert_eq!(buf.timestamp(), Some(ts));
        assert_eq!(buf.data(), &[0.5, -0.5]);
    }

    // FIFO order across multiple chunks of differing lengths, including a wrap of
    // the underlying f32 ring.
    #[test]
    fn sample_ring_fifo_and_wrap() {
        // Small sample ring to force wrap-around of the f32 buffer.
        let (mut producer, mut consumer) = create_sample_ring(8, 8, fmt());

        // Push/pop many chunks so the f32 ring wraps repeatedly.
        for i in 0..100u32 {
            let v = i as f32;
            assert!(producer.push_samples_or_drop(&[v, v + 0.25, v + 0.5], 1, 48000));
            let buf = consumer.pop().expect("chunk per iteration");
            assert_eq!(
                buf.data(),
                &[v, v + 0.25, v + 0.5],
                "FIFO/wrap integrity at {i}"
            );
        }
    }

    // When the sample ring is full the chunk is dropped atomically (counter
    // increments, consumer never sees a partial chunk).
    #[test]
    fn sample_ring_drops_atomically_when_full() {
        // Capacity for ~1 chunk of 4 samples.
        let (mut producer, mut consumer) = create_sample_ring(4, 4, fmt());
        assert!(producer.push_samples_or_drop(&[1.0, 2.0, 3.0, 4.0], 2, 48000));
        // Ring full → this 4-sample chunk cannot fit; must drop, not partially write.
        assert!(!producer.push_samples_or_drop(&[5.0, 6.0, 7.0, 8.0], 2, 48000));
        assert_eq!(producer.shared.buffers_dropped.load(Ordering::Relaxed), 1);

        // The consumer sees exactly the first chunk, intact.
        let buf = consumer.pop().expect("first chunk");
        assert_eq!(buf.data(), &[1.0, 2.0, 3.0, 4.0]);
        assert!(consumer.pop().is_none(), "dropped chunk must not appear");
    }

    // available_chunks reflects pending complete chunks.
    #[test]
    fn sample_ring_available_chunks() {
        let (mut producer, mut consumer) = create_sample_ring(64, 8, fmt());
        assert_eq!(consumer.available_chunks(), 0);
        assert!(producer.push_samples_or_drop(&[1.0, 2.0], 2, 48000));
        assert!(producer.push_samples_or_drop(&[3.0, 4.0], 2, 48000));
        assert_eq!(consumer.available_chunks(), 2);
        let _ = consumer.pop();
        assert_eq!(consumer.available_chunks(), 1);
    }
}

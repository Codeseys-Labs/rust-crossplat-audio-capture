// Multi-source channel composition (ADR-0011) for the Go bindings.
//
// This file wraps the rsac_composition_* / rsac_group_* C FFI surface, which
// is compiled into librsac_ffi only when the `compose` cargo feature is on and
// declared in rsac.h behind `#if defined(RSAC_FEATURE_COMPOSE)`. The Makefile
// builds the staticlib with `--features compose` (FFI_FEATURES += compose); the
// `#cgo CFLAGS: -DRSAC_FEATURE_COMPOSE` directive below makes those guarded
// declarations visible to cgo.
//
// # Why the define lives in CFLAGS, not a bare `#define`
//
// cgo concatenates every file's C preamble into one translation unit, in the
// order `go build` lists the files (alphabetical): callback.go includes rsac.h
// FIRST, without the compose define, so the `#ifndef RSAC_H` include guard would
// block a later `#define RSAC_FEATURE_COMPOSE` + re-`#include "rsac.h"` here from
// ever re-expanding the compose section. A `#cgo CFLAGS: -D...` directive is
// collected package-wide and passed to the C compiler for the WHOLE package, so
// the macro is defined before ANY file's rsac.h include — the only ordering-safe
// way to gate the section (rsac-fba7).
package rsac

/*
#cgo CFLAGS: -DRSAC_FEATURE_COMPOSE
#include "rsac.h"
#include <stdlib.h>
*/
import "C"
import (
	"context"
	"errors"
	"runtime"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"
)

// ── Group layout ──────────────────────────────────────────────────────────

// GroupLayout selects how a [Group]'s sources map onto the composed output
// channels. The discriminants match rsac_group_layout_t in the C ABI.
type GroupLayout int32

const (
	// LayoutMono folds every source in the group to mono and gain-weighted-sums
	// them into ONE output channel.
	LayoutMono GroupLayout = C.RSAC_GROUP_LAYOUT_MONO
	// LayoutStereo folds every source to stereo and sums into TWO output
	// channels (the default layout of a new group).
	LayoutStereo GroupLayout = C.RSAC_GROUP_LAYOUT_STEREO
	// LayoutKeepChannels passes the group's SINGLE source through with its native
	// channel count (a keep-channels group must contain exactly one source).
	LayoutKeepChannels GroupLayout = C.RSAC_GROUP_LAYOUT_KEEP_CHANNELS
)

// ── Group ─────────────────────────────────────────────────────────────────

// Group is a named set of capture sources sharing a [GroupLayout], added to a
// [CompositionBuilder] via [CompositionBuilder.AddGroup].
//
// A Group owns its C handle until it is consumed by a successful AddGroup;
// after that the handle belongs to the builder and must NOT be freed again.
// Call [Group.Free] to release a group that was never added (or whose AddGroup
// returned an error).
type Group struct {
	handle *C.RsacGroup
	// consumed is set true once a successful AddGroup takes ownership of the
	// handle, so Free()/the finalizer never double-frees it.
	consumed bool
}

// NewGroup creates a composition group with the given name and the default
// stereo layout. The name must be non-empty and unique within a composition
// (both enforced at [CompositionBuilder.Build], not here).
func NewGroup(name string) (*Group, error) {
	cname := C.CString(name)
	defer C.free(unsafe.Pointer(cname))

	var handle *C.RsacGroup
	if rc := C.rsac_group_new(cname, &handle); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if handle == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}
	g := &Group{handle: handle}
	runtime.SetFinalizer(g, (*Group).Free)
	return g, nil
}

// SetLayout sets how the group's sources map onto the composed output channels.
// A keep-channels group must contain exactly one source (arity enforced at
// [CompositionBuilder.Build]).
func (g *Group) SetLayout(layout GroupLayout) error {
	if g.handle == nil {
		return ErrClosed
	}
	// KeepAlive: g has a finalizer; without this the GC could collect g (and
	// free the handle) between reading g.handle and the cgo call returning.
	rc := C.rsac_group_set_layout(g.handle, C.int32_t(layout))
	runtime.KeepAlive(g)
	return newError(rc)
}

// AddSource adds a capture source with unit gain (1.0). The spec uses the same
// canonical target grammar as [ParseTarget]:
//
//	"system" | "device:<id>" | "app:<pid-or-id>" | "name:<name>" | "tree:<pid>"
//
// On a parse error the group is unchanged and an *Error with code
// ErrInvalidParameter is returned.
func (g *Group) AddSource(spec string) error {
	if g.handle == nil {
		return ErrClosed
	}
	cspec := C.CString(spec)
	defer C.free(unsafe.Pointer(cspec))
	rc := C.rsac_group_add_source(g.handle, cspec)
	runtime.KeepAlive(g)
	return newError(rc)
}

// AddSourceWithGain adds a capture source with an explicit linear mixdown gain
// (1.0 = unity). The gain must be finite and >= 0; an invalid gain is rejected
// eagerly with ErrInvalidParameter and the group is unchanged.
func (g *Group) AddSourceWithGain(spec string, gain float32) error {
	if g.handle == nil {
		return ErrClosed
	}
	cspec := C.CString(spec)
	defer C.free(unsafe.Pointer(cspec))
	rc := C.rsac_group_add_source_with_gain(g.handle, cspec, C.float(gain))
	runtime.KeepAlive(g)
	return newError(rc)
}

// Free releases a group handle that was never consumed by a successful
// [CompositionBuilder.AddGroup]. It is a no-op if the group is nil, already
// freed, or was consumed by the builder (which owns the handle after that).
// Idempotent and safe to call multiple times.
func (g *Group) Free() {
	if g == nil || g.consumed || g.handle == nil {
		return
	}
	C.rsac_group_free(g.handle)
	g.handle = nil
	runtime.SetFinalizer(g, nil)
}

// ── CompositionBuilder ──────────────────────────────────────────────────────

// CompositionBuilder configures and builds a [Composition]. Like the C FFI
// builder, [CompositionBuilder.Build] ALWAYS consumes the builder (even on
// error) — create a new builder to retry.
type CompositionBuilder struct {
	handle *C.RsacCompositionBuilder
}

// NewCompositionBuilder creates a composition builder with default settings
// (48 kHz session rate, 10 ms quantum, 250 ms stall timeout, 1 s buffering
// bound, no output clamping, no groups).
func NewCompositionBuilder() (*CompositionBuilder, error) {
	var handle *C.RsacCompositionBuilder
	if rc := C.rsac_composition_builder_new(&handle); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if handle == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}
	b := &CompositionBuilder{handle: handle}
	runtime.SetFinalizer(b, (*CompositionBuilder).free)
	return b, nil
}

// free releases an unbuilt builder handle (finalizer safety net). Build clears
// the handle itself since the C side consumes it.
func (b *CompositionBuilder) free() {
	if b.handle != nil {
		C.rsac_composition_builder_free(b.handle)
		b.handle = nil
		runtime.SetFinalizer(b, nil)
	}
}

// SetSampleRate sets the session sample rate in Hz (default 48000). Sources
// delivering a different rate are resampled. An unsupported rate is rejected at
// [CompositionBuilder.Build] / [CompositionBuilder.Preflight].
func (b *CompositionBuilder) SetSampleRate(rate uint32) error {
	if b.handle == nil {
		return ErrClosed
	}
	rc := C.rsac_composition_builder_set_sample_rate(b.handle, C.uint32_t(rate))
	runtime.KeepAlive(b)
	return newError(rc)
}

// SetClampOutput enables (true) or disables (false) saturating output clamping
// to [-1.0, 1.0] after summation. Default off.
func (b *CompositionBuilder) SetClampOutput(clamp bool) error {
	if b.handle == nil {
		return ErrClosed
	}
	var c C.int32_t
	if clamp {
		c = 1
	}
	rc := C.rsac_composition_builder_set_clamp_output(b.handle, c)
	runtime.KeepAlive(b)
	return newError(rc)
}

// SetQuantumMs sets the composed tick quantum (output buffer duration) in
// milliseconds (default 10). A zero quantum is rejected at Preflight/Build.
func (b *CompositionBuilder) SetQuantumMs(millis uint64) error {
	if b.handle == nil {
		return ErrClosed
	}
	rc := C.rsac_composition_builder_set_quantum_ms(b.handle, C.uint64_t(millis))
	runtime.KeepAlive(b)
	return newError(rc)
}

// SetStallTimeoutMs sets how long the compositor waits for the master-clock
// source before emitting a wall-clock fallback tick, in milliseconds (default
// 250). A zero timeout is rejected at Preflight/Build.
func (b *CompositionBuilder) SetStallTimeoutMs(millis uint64) error {
	if b.handle == nil {
		return ErrClosed
	}
	rc := C.rsac_composition_builder_set_stall_timeout_ms(b.handle, C.uint64_t(millis))
	runtime.KeepAlive(b)
	return newError(rc)
}

// SetMaxBufferMs sets the per-source buffering bound in milliseconds (default
// 1000). Any value — including 0 — passes validation (clamped to one quantum at
// start).
func (b *CompositionBuilder) SetMaxBufferMs(millis uint64) error {
	if b.handle == nil {
		return ErrClosed
	}
	rc := C.rsac_composition_builder_set_max_buffer_ms(b.handle, C.uint64_t(millis))
	runtime.KeepAlive(b)
	return newError(rc)
}

// AddGroup appends a group to the composition. Groups contribute output
// channels in the order added. On success the group's C handle is CONSUMED:
// the group must not be used or freed afterwards (this method marks it so
// [Group.Free] / its finalizer become no-ops). On any error the caller still
// owns the group and should Free it — mirroring the FFI ownership contract.
func (b *CompositionBuilder) AddGroup(g *Group) error {
	if b.handle == nil {
		return ErrClosed
	}
	if g == nil || g.handle == nil {
		return newError(C.RSAC_ERROR_NULL_POINTER)
	}
	rc := C.rsac_composition_builder_add_group(b.handle, g.handle)
	runtime.KeepAlive(b)
	runtime.KeepAlive(g)
	if rc != C.RSAC_OK {
		// On any error the group is untouched and the caller still owns it.
		return newError(rc)
	}
	// Consumed on RSAC_OK: the builder now owns the handle; forget it here so
	// Free()/the finalizer never double-frees.
	g.consumed = true
	g.handle = nil
	runtime.SetFinalizer(g, nil)
	return nil
}

// Preflight runs every device-independent validation Build performs WITHOUT
// consuming the builder, so a caller can fix a reported error on the same
// builder and retry. Returns the same error codes as Build's validation phase.
func (b *CompositionBuilder) Preflight() error {
	if b.handle == nil {
		return ErrClosed
	}
	rc := C.rsac_composition_builder_preflight(b.handle)
	runtime.KeepAlive(b)
	return newError(rc)
}

// Build validates the configuration and creates a (not yet started)
// [Composition]. The builder is ALWAYS consumed — on failure too — matching the
// C FFI; create a new builder to retry. No devices are touched here; inner
// captures are created and started by [Composition.Start].
func (b *CompositionBuilder) Build() (*Composition, error) {
	if b.handle == nil {
		return nil, ErrClosed
	}
	// build consumes the builder handle regardless of outcome; forget it now.
	handle := b.handle
	b.handle = nil
	runtime.SetFinalizer(b, nil)

	var chandle *C.RsacComposition
	if rc := C.rsac_composition_builder_build(handle, &chandle); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if chandle == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}
	comp := &Composition{handle: chandle}
	runtime.SetFinalizer(comp, (*Composition).destroy)
	return comp, nil
}

// ── Composition ─────────────────────────────────────────────────────────────

// Composition is a multi-source composed capture session (ADR-0011), created by
// [CompositionBuilder.Build] and inert until [Composition.Start].
//
// It is safe for concurrent use from multiple goroutines; all methods acquire
// an internal mutex before touching the C handle. The caller must call
// [Composition.Close] when done to release resources (a finalizer is registered
// as a safety net, but explicit Close is preferred).
//
// # Reading discards the buffered tail on explicit stop
//
// An explicit [Composition.Stop] / [Composition.Close] discards any composed
// buffers still in the ring: subsequent reads return the terminal stream error
// rather than draining. To capture everything, read until the terminal error
// (ErrStreamFailed) BEFORE stopping. The composition's NATURAL end (all sources
// ended) drains the tail first.
type Composition struct {
	mu     sync.Mutex
	handle *C.RsacComposition
	closed bool

	// reads tracks in-flight ReadBuffer/TryReadBuffer C calls so Close() can
	// drain them before freeing the handle (mirrors AudioCapture, #28 fix).
	reads sync.WaitGroup
	// closing is set true (under mu, before closed) at the start of Close(). A
	// reader that took the WaitGroup barrier re-checks it after dropping mu and
	// bails before entering C if a close has begun.
	closing atomic.Bool
}

// Start builds and starts one capture per source, resolves the composed channel
// layout, and spawns the compositor thread. Starting twice returns
// ErrConfiguration; a device/format error surfaces here.
func (c *Composition) Start() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrClosed
	}
	return newError(C.rsac_composition_start(c.handle))
}

// Stop signals the composition to stop (idempotent; a not-started composition
// returns nil). It unblocks a goroutine parked in [Composition.ReadBuffer]. The
// compositor thread is joined by [Composition.Close]. An explicit stop discards
// the buffered composed tail (see the type doc).
func (c *Composition) Stop() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil // already closed, stop is a no-op
	}
	return newError(C.rsac_composition_stop(c.handle))
}

// Close stops the composition (if running), joins the compositor thread, and
// frees all associated resources. Idempotent — safe to call multiple times.
// After Close the Composition must not be used.
func (c *Composition) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	return c.closeLocked()
}

// closeLocked frees the C handle. Must be called with c.mu held; it briefly
// RELEASES c.mu to drain in-flight reads and re-acquires it before freeing.
//
// The teardown mirrors AudioCapture.closeLocked's blocking-read (#28) fix: set
// closing+closed FIRST (so new reads bail), call rsac_composition_stop to
// UNBLOCK a parked reader (safe concurrently with an in-flight read — the sole
// documented FFI exception), then UNLOCK c.mu and reads.Wait() to drain,
// RE-LOCK, and only then rsac_composition_free. Releasing c.mu across Wait() is
// mandatory: the parked reader's error path re-takes c.mu, so holding it would
// deadlock.
func (c *Composition) closeLocked() error {
	c.closing.Store(true)
	c.closed = true
	runtime.SetFinalizer(c, nil)

	// request_stop equivalent: transitions the composed stream terminal and
	// signals the engine, unblocking any parked rsac_composition_read so
	// reads.Wait() returns promptly instead of waiting out the read timeout.
	C.rsac_composition_stop(c.handle)

	c.mu.Unlock()
	c.reads.Wait()
	c.mu.Lock()

	C.rsac_composition_free(c.handle)
	c.handle = nil
	return nil
}

// destroy is the Go finalizer — a safety net for compositions that were not
// explicitly closed.
func (c *Composition) destroy() {
	_ = c.Close()
}

// IsRunning returns true if the composed stream is currently running.
func (c *Composition) IsRunning() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return false
	}
	return C.rsac_composition_is_running(c.handle) == 1
}

// ChannelCount returns the number of composed output channels. The layout is
// resolved by Start, so this is 0 before a successful start (and after close).
func (c *Composition) ChannelCount() uint16 {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return 0
	}
	return uint16(C.rsac_composition_channel_count(c.handle))
}

// ChannelGroup returns the name of the group producing composed output channel
// `channel` (0-based) and true, or ("", false) if the composition is not
// started or the channel is out of bounds.
func (c *Composition) ChannelGroup(channel int) (string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return "", false
	}
	cname := C.rsac_composition_channel_group(c.handle, C.size_t(channel))
	if cname == nil {
		return "", false
	}
	return C.GoString(cname), true
}

// ChannelInGroup returns the index of composed output channel `channel` within
// its group (0-based; e.g. 0 = L, 1 = R for a stereo group) and true, or
// (0, false) if the composition is not started or the channel is out of bounds.
func (c *Composition) ChannelInGroup(channel int) (int, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return 0, false
	}
	idx := int(C.rsac_composition_channel_in_group(c.handle, C.size_t(channel)))
	if idx < 0 {
		return 0, false
	}
	return idx, true
}

// OverrunCount returns the number of composed-ring overruns (composed buffers
// dropped because the consumer read slower than the compositor produced). This
// counts loss at the composed ring only; per-source upstream loss is reported
// via [Composition.SourceStats]. Returns 0 if closed or not started.
func (c *Composition) OverrunCount() uint64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return 0
	}
	return uint64(C.rsac_composition_overrun_count(c.handle))
}

// ReadBuffer reads the next composed buffer, blocking until data is available.
// Once the composition ends and drains it returns a fatal *Error (code
// ErrStreamFailed). A concurrent [Composition.Stop] / [Composition.Close]
// unblocks a goroutine parked here.
func (c *Composition) ReadBuffer() (AudioBuffer, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return AudioBuffer{}, ErrClosed
	}
	handle := c.handle
	// Join the in-flight-read barrier under c.mu (ordered before any Close
	// observes closed=true); Close drains reads.Wait() before free (#28 fix).
	c.reads.Add(1)
	c.mu.Unlock()
	defer c.reads.Done()

	// A close may have started after we dropped c.mu (it sets closing first).
	// Bail before entering C so we never park against a handle Close is draining.
	if c.closing.Load() {
		return AudioBuffer{}, ErrClosed
	}

	var cbuf *C.RsacAudioBuffer
	rc := C.rsac_composition_read(handle, &cbuf)
	if rc != C.RSAC_OK {
		c.mu.Lock()
		defer c.mu.Unlock()
		if c.closed {
			return AudioBuffer{}, ErrClosed
		}
		return AudioBuffer{}, newError(rc)
	}
	if cbuf == nil {
		return AudioBuffer{}, nil
	}
	return audioBufferFromC(cbuf), nil
}

// TryReadBuffer attempts a non-blocking read of the next composed buffer.
// Returns (buffer, true, nil) if data was available, (AudioBuffer{}, false, nil)
// if none is buffered yet, or a fatal *Error (ErrStreamFailed) once the drained
// composition has ended.
func (c *Composition) TryReadBuffer() (AudioBuffer, bool, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return AudioBuffer{}, false, ErrClosed
	}
	handle := c.handle
	c.reads.Add(1)
	c.mu.Unlock()
	defer c.reads.Done()

	if c.closing.Load() {
		return AudioBuffer{}, false, ErrClosed
	}

	var cbuf *C.RsacAudioBuffer
	rc := C.rsac_composition_try_read(handle, &cbuf)
	if rc != C.RSAC_OK {
		c.mu.Lock()
		defer c.mu.Unlock()
		if c.closed {
			return AudioBuffer{}, false, ErrClosed
		}
		return AudioBuffer{}, false, newError(rc)
	}
	if cbuf == nil {
		return AudioBuffer{}, false, nil
	}
	return audioBufferFromC(cbuf), true, nil
}

// Stream returns a channel that delivers composed AudioBuffer values as they are
// produced. The channel is closed when ctx is cancelled, the composition ends,
// or a fatal error occurs. Only one Stream should be active per Composition.
func (c *Composition) Stream(ctx context.Context) <-chan AudioBuffer {
	ch := make(chan AudioBuffer, 16)
	go c.streamLoop(ctx, ch)
	return ch
}

// StreamWithErrors returns a channel of [StreamResult] values so callers can
// observe the terminal error. Recoverable hiccups are swallowed and retried;
// the fatal terminal error is delivered as the final StreamResult before the
// channel closes (same contract as [AudioCapture.StreamWithErrors]).
func (c *Composition) StreamWithErrors(ctx context.Context) <-chan StreamResult {
	ch := make(chan StreamResult, 16)
	go c.streamLoopWithErrors(ctx, ch)
	return ch
}

// pollBackoff sleeps ~1ms (context-aware) between empty/recoverable polls so
// an idle or not-started composition doesn't spin a CPU core (PR #59 review).
// One composed quantum is 10ms by default, so 1ms adds no perceptible latency.
// Returns false when the context was cancelled during the wait.
func pollBackoff(ctx context.Context) bool {
	t := time.NewTimer(time.Millisecond)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

func (c *Composition) streamLoop(ctx context.Context, ch chan<- AudioBuffer) {
	defer close(ch)
	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		buf, ok, err := c.TryReadBuffer()
		if err != nil {
			if IsRecoverable(err) && !errors.Is(err, ErrClosed) {
				if !pollBackoff(ctx) {
					return
				}
				continue
			}
			return // fatal terminal (or closed): stop delivering, close channel
		}
		if !ok {
			if !pollBackoff(ctx) {
				return
			}
			continue
		}
		select {
		case ch <- buf:
		case <-ctx.Done():
			return
		}
	}
}

func (c *Composition) streamLoopWithErrors(ctx context.Context, ch chan<- StreamResult) {
	defer close(ch)
	for {
		select {
		case <-ctx.Done():
			// Best-effort cancellation report: if the buffered channel is full
			// (consumer stopped reading), do NOT block — an unconditional send
			// here would leak this goroutine forever (PR #59 review).
			select {
			case ch <- StreamResult{Err: ctx.Err()}:
			default:
			}
			return
		default:
		}

		buf, ok, err := c.TryReadBuffer()
		if err != nil {
			// Recoverable hiccups are swallowed and retried (matching Stream and
			// AudioCapture.StreamWithErrors); the fatal terminal (or ErrClosed) is
			// delivered as the final item then closes the channel.
			if IsRecoverable(err) && !errors.Is(err, ErrClosed) {
				if !pollBackoff(ctx) {
					return
				}
				continue
			}
			select {
			case ch <- StreamResult{Err: err}:
			case <-ctx.Done():
			}
			return
		}
		if !ok {
			if !pollBackoff(ctx) {
				return
			}
			continue
		}
		select {
		case ch <- StreamResult{Buffer: buf}:
		case <-ctx.Done():
			return
		}
	}
}

// ── Composition stats ─────────────────────────────────────────────────────

// CompositionStats is a point-in-time snapshot of a running composition's
// counters. It mirrors the C-ABI RsacCompositionStats value type. Obtain one
// via [Composition.Stats].
type CompositionStats struct {
	// Ticks is the number of composed buffers emitted so far.
	Ticks uint64
	// FallbackTicks is the number of ticks emitted by the wall-clock stall
	// fallback (the master had no data).
	FallbackTicks uint64
	// NumSources is the number of composed sources, in flat declaration order.
	// Valid indices for [Composition.SourceStats] are 0..NumSources.
	NumSources int
}

// SourceStats is a point-in-time snapshot of one composed source's counters. It
// mirrors the C-ABI RsacSourceStats value type (the C struct omits the richer
// Rust gap_padded_frames / inner_dropped counters, so the Go binding — faithful
// to the FFI it wraps — does too). Obtain one via [Composition.SourceStats].
type SourceStats struct {
	// Group is the name of the group the source belongs to.
	Group string
	// Target is the source's capture target in canonical grammar (e.g. "system").
	Target string
	// BuffersReceived is the number of buffers received from the inner capture.
	BuffersReceived uint64
	// PaddedFrames is silence frames inserted because the source was behind.
	PaddedFrames uint64
	// TrimmedFrames is frames trimmed because the source drifted past the bound.
	TrimmedFrames uint64
	// Resampling is true if this source is being resampled to the session rate.
	Resampling bool
	// Ended is true if the source's stream has ended.
	Ended bool
}

// Stats returns a point-in-time snapshot of the composition's counters.
//
// Returns ErrClosed if the composition has been closed, and an *Error with code
// ErrStreamRead if it has not been started yet (a started composition always
// has at least one source; a NumSources of 0 means the compositor engine does
// not exist yet — the same "not started" signal rsac_composition_source_stats
// reports directly).
func (c *Composition) Stats() (CompositionStats, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return CompositionStats{}, ErrClosed
	}
	var cs C.RsacCompositionStats
	if rc := C.rsac_composition_stats(c.handle, &cs); rc != C.RSAC_OK {
		return CompositionStats{}, newError(rc)
	}
	if cs.num_sources == 0 {
		// Not started: the C snapshot is all-zero. Surface the same recoverable
		// not-started error the per-source accessor returns.
		return CompositionStats{}, newError(C.RSAC_ERROR_STREAM_READ)
	}
	return CompositionStats{
		Ticks:         uint64(cs.ticks),
		FallbackTicks: uint64(cs.fallback_ticks),
		NumSources:    int(cs.num_sources),
	}, nil
}

// SourceStats returns the counters of the source at index (flat declaration
// order across all groups; valid indices are 0..NumSources from
// [Composition.Stats]). Returns ErrClosed if closed, an *Error with code
// ErrStreamRead if the composition has not been started, or ErrInvalidParameter
// if index is out of bounds.
func (c *Composition) SourceStats(index int) (SourceStats, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return SourceStats{}, ErrClosed
	}
	var cs C.RsacSourceStats
	if rc := C.rsac_composition_source_stats(c.handle, C.size_t(index), &cs); rc != C.RSAC_OK {
		return SourceStats{}, newError(rc)
	}
	stats := SourceStats{
		BuffersReceived: uint64(cs.buffers_received),
		PaddedFrames:    uint64(cs.padded_frames),
		TrimmedFrames:   uint64(cs.trimmed_frames),
		Resampling:      cs.resampling != 0,
		Ended:           cs.ended != 0,
	}
	if cname := C.rsac_composition_source_group(c.handle, C.size_t(index)); cname != nil {
		stats.Group = C.GoString(cname)
	}
	if ctarget := C.rsac_composition_source_target(c.handle, C.size_t(index)); ctarget != nil {
		stats.Target = C.GoString(ctarget)
	}
	return stats, nil
}

// SourceGroup returns the group name of the source at index (flat declaration
// order) and true, or ("", false) if the composition is not started or index is
// out of bounds.
func (c *Composition) SourceGroup(index int) (string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return "", false
	}
	cname := C.rsac_composition_source_group(c.handle, C.size_t(index))
	if cname == nil {
		return "", false
	}
	return C.GoString(cname), true
}

// SourceTarget returns the canonical target string of the source at index (flat
// declaration order) and true, or ("", false) if the composition is not started
// or index is out of bounds.
func (c *Composition) SourceTarget(index int) (string, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return "", false
	}
	ctarget := C.rsac_composition_source_target(c.handle, C.size_t(index))
	if ctarget == nil {
		return "", false
	}
	return C.GoString(ctarget), true
}

// ── Live per-source gain / mute (rsac-5a2d) ────────────────────────────────

// SetGain sets a source's live mix gain on a running composition. The source is
// addressed by its group name plus its within-group source index (0-based,
// declaration order) — NOT the flat cross-group index the Source* accessors
// take. Returns an *Error with ErrStreamRead if the composition is not started,
// stopped, or ended, and ErrConfiguration for an unknown group, out-of-range
// index, or a gain that is not finite and >= 0 (validated by the core/FFI).
func (c *Composition) SetGain(group string, sourceIdx int, gain float32) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrClosed
	}
	cgroup := C.CString(group)
	defer C.free(unsafe.Pointer(cgroup))
	rc := C.rsac_composition_set_gain(c.handle, cgroup, C.size_t(sourceIdx), C.float(gain))
	runtime.KeepAlive(c)
	return newError(rc)
}

// SetMuted mutes (muted=true) or unmutes (muted=false) a source on a running
// composition. Muting is a separate flag from gain: unmute restores the prior
// gain. Same addressing and errors as [Composition.SetGain] (minus the
// invalid-gain case).
func (c *Composition) SetMuted(group string, sourceIdx int, muted bool) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrClosed
	}
	cgroup := C.CString(group)
	defer C.free(unsafe.Pointer(cgroup))
	var cmuted C.int32_t
	if muted {
		cmuted = 1
	}
	rc := C.rsac_composition_set_muted(c.handle, cgroup, C.size_t(sourceIdx), cmuted)
	runtime.KeepAlive(c)
	return newError(rc)
}

// Gain reads back a source's current stored per-source gain — the SetGain
// value (or the build-time seed); the actual mixed output also depends on the
// source's mute flag and the group's master gain. Same addressing as
// [Composition.SetGain]. Unlike the setter this keeps working on a stopped or
// ended composition; it returns an *Error with ErrStreamRead only before the
// first successful start, and ErrConfiguration for an unknown group or
// out-of-range index.
func (c *Composition) Gain(group string, sourceIdx int) (float32, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return 0, ErrClosed
	}
	cgroup := C.CString(group)
	defer C.free(unsafe.Pointer(cgroup))
	var out C.float
	rc := C.rsac_composition_gain(c.handle, cgroup, C.size_t(sourceIdx), &out)
	runtime.KeepAlive(c)
	if rc != C.RSAC_OK {
		return 0, newError(rc)
	}
	return float32(out), nil
}

// IsMuted reads back whether a source is currently muted. Same addressing and
// stopped-composition behavior as [Composition.Gain].
func (c *Composition) IsMuted(group string, sourceIdx int) (bool, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return false, ErrClosed
	}
	cgroup := C.CString(group)
	defer C.free(unsafe.Pointer(cgroup))
	var out C.int32_t
	rc := C.rsac_composition_is_muted(c.handle, cgroup, C.size_t(sourceIdx), &out)
	runtime.KeepAlive(c)
	if rc != C.RSAC_OK {
		return false, newError(rc)
	}
	return out != 0, nil
}

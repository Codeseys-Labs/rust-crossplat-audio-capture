package rsac

/*
#include "rsac.h"
#include "bridge.h"
*/
import "C"
import (
	"runtime/cgo"
	"unsafe"
)

// Callback delivery uses runtime/cgo.Handle to bridge a Go closure across the
// FFI boundary safely. cgo cannot pass a Go function pointer (or any Go pointer
// that itself contains Go pointers) directly to C, so we wrap the closure in a
// cgo.Handle — an opaque integer the Go runtime maps back to the value — and
// pass that as the void* user_data. goAudioCallback resolves the Handle back to
// the closure. This is GC-safe.
//
// AudioCapture.callback stores the live cgo.Handle so Close() (and a replacing
// SetCallback) can Delete() it deterministically; otherwise a Handle would leak.

//export goAudioCallback
func goAudioCallback(bufferData *C.float, numSamples C.size_t, channels C.uint16_t, sampleRate C.uint32_t, userData unsafe.Pointer) {
	if userData == nil {
		return
	}
	h := cgo.Handle(uintptr(userData))
	fn, ok := h.Value().(func(AudioBuffer))
	if !ok || fn == nil {
		return
	}

	// The C contract (rsac_audio_callback_t) hands us the raw interleaved f32
	// data directly — buffer_data/num_samples/channels/sample_rate — and the
	// pointer is valid only for the duration of this call. Copy into Go-managed
	// memory before invoking the user's callback.
	n := int(numSamples)
	ch := int(channels)
	rate := int(sampleRate)

	var data []float32
	if bufferData != nil && n > 0 {
		data = make([]float32, n)
		cSlice := unsafe.Slice((*float32)(unsafe.Pointer(bufferData)), n)
		copy(data, cSlice)
	}

	numFrames := 0
	if ch > 0 {
		numFrames = n / ch
	}

	buf := AudioBuffer{
		data:       data,
		numFrames:  numFrames,
		channels:   ch,
		sampleRate: rate,
	}
	fn(buf)
}

// SetCallback registers a Go callback function for push-based audio delivery.
// The callback is invoked on a background thread with each captured audio buffer.
// Must be called before [AudioCapture.Start].
//
// Pass nil to clear the callback.
//
// The callback receives copies of the audio data in Go-managed memory, so
// the buffer is safe to use after the callback returns.
func (c *AudioCapture) SetCallback(fn func(AudioBuffer)) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrClosed
	}

	if fn == nil {
		rc := C.rsac_capture_set_callback(c.handle, nil, nil)
		if rc != C.RSAC_OK {
			return newError(rc)
		}
		c.clearCallbackHandleLocked()
		return nil
	}

	h := cgo.NewHandle(fn)
	rc := C.rsac_go_set_callback(c.handle, C.uintptr_t(h))
	if rc != C.RSAC_OK {
		h.Delete()
		return newError(rc)
	}
	// Replace any previously registered callback for this capture.
	c.clearCallbackHandleLocked()
	c.callback = uintptr(h)
	return nil
}

// clearCallbackHandleLocked deletes the cgo.Handle backing the current callback
// (if any). Must be called with c.mu held. Safe to call repeatedly.
func (c *AudioCapture) clearCallbackHandleLocked() {
	if c.callback != 0 {
		cgo.Handle(c.callback).Delete()
		c.callback = 0
	}
}

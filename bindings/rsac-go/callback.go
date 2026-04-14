package rsac

/*
#include "rsac.h"
*/
import "C"
import (
	"sync"
	"unsafe"
)

// callbackRegistry maintains a global mapping from integer IDs to Go callback
// functions. This is necessary because CGo cannot pass Go function pointers
// directly to C — we pass an integer ID as the void* user_data instead.
var callbackRegistry = struct {
	mu    sync.RWMutex
	funcs map[uintptr]func(AudioBuffer)
	next  uintptr
}{
	funcs: make(map[uintptr]func(AudioBuffer)),
	next:  1,
}

// registerCallback stores a Go callback and returns an ID for it.
func registerCallback(fn func(AudioBuffer)) uintptr {
	callbackRegistry.mu.Lock()
	defer callbackRegistry.mu.Unlock()
	id := callbackRegistry.next
	callbackRegistry.next++
	callbackRegistry.funcs[id] = fn
	return id
}

// unregisterCallback removes a callback by ID.
func unregisterCallback(id uintptr) {
	callbackRegistry.mu.Lock()
	defer callbackRegistry.mu.Unlock()
	delete(callbackRegistry.funcs, id)
}

// lookupCallback retrieves a callback by ID.
func lookupCallback(id uintptr) (func(AudioBuffer), bool) {
	callbackRegistry.mu.RLock()
	defer callbackRegistry.mu.RUnlock()
	fn, ok := callbackRegistry.funcs[id]
	return fn, ok
}

//export goAudioCallback
func goAudioCallback(cbuf *C.rsac_audio_buffer_t, userData unsafe.Pointer) {
	id := uintptr(userData)
	fn, ok := lookupCallback(id)
	if !ok || fn == nil {
		return
	}

	// Build a Go AudioBuffer from the C buffer WITHOUT freeing it.
	// The C callback contract says the buffer is valid only for the callback duration.
	// We must copy the data.
	if cbuf == nil {
		return
	}

	numFrames := int(C.rsac_audio_buffer_num_frames(cbuf))
	channels := int(C.rsac_audio_buffer_channels(cbuf))
	sampleRate := int(C.rsac_audio_buffer_sample_rate(cbuf))
	numSamples := int(C.rsac_audio_buffer_num_samples(cbuf))

	cdata := C.rsac_audio_buffer_data(cbuf)
	var data []float32
	if cdata != nil && numSamples > 0 {
		data = make([]float32, numSamples)
		cSlice := unsafe.Slice((*float32)(unsafe.Pointer(cdata)), numSamples)
		copy(data, cSlice)
	}

	buf := AudioBuffer{
		data:       data,
		numFrames:  numFrames,
		channels:   channels,
		sampleRate: sampleRate,
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
		return newError(rc)
	}

	id := registerCallback(fn)
	rc := C.rsac_capture_set_callback(
		c.handle,
		C.rsac_audio_callback_t(C.goAudioCallback),
		unsafe.Pointer(id),
	)
	if rc != C.RSAC_OK {
		unregisterCallback(id)
		return newError(rc)
	}
	return nil
}

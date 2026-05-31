/*
 * bridge.h — small C shims that sit between the Go cgo layer and the rsac-ffi
 * C ABI. Compiled by cgo as part of the rsac Go package.
 *
 * These shims exist so that the integer <-> void* conversions required to carry
 * a runtime/cgo.Handle through rsac_capture_set_callback's user_data argument
 * happen in C, where Go's `go vet` unsafeptr analyzer does not apply. The handle
 * value never points to Go-managed memory, so this is safe and the cgo runtime
 * pointer checker accepts it.
 */

#ifndef RSAC_GO_BRIDGE_H
#define RSAC_GO_BRIDGE_H

#include <stdint.h>
#include "rsac.h"

/*
 * Installs the Go audio callback bridge (goAudioCallback) on the given capture,
 * carrying `handle` (a cgo.Handle, i.e. an opaque integer) through the C
 * user_data pointer. Pass the handle returned by cgo.NewHandle.
 */
rsac_error_t rsac_go_set_callback(RsacCapture* capture, uintptr_t handle);

#endif /* RSAC_GO_BRIDGE_H */

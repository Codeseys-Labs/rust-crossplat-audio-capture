#include "rsac_ring_atomics.h"

#include <stdatomic.h>

// The header exposes plain `uint64_t *` (Swift imports that cleanly as
// UnsafeMutablePointer<UInt64>); the implementations cast to
// `_Atomic uint64_t *`. On arm64/x86_64, _Atomic uint64_t has the same size
// and alignment as uint64_t, and the caller guarantees 8-byte alignment
// (RingLayout header offsets), so the cast is well-behaved in practice.
// This is the same pattern the Linux kernel and rtrb's FFI mirrors use.

uint64_t rsac_atomic_load_u64_acquire(const volatile uint64_t *ptr) {
    return atomic_load_explicit((const volatile _Atomic uint64_t *)ptr,
                                memory_order_acquire);
}

uint64_t rsac_atomic_load_u64_relaxed(const volatile uint64_t *ptr) {
    return atomic_load_explicit((const volatile _Atomic uint64_t *)ptr,
                                memory_order_relaxed);
}

void rsac_atomic_store_u64_release(volatile uint64_t *ptr, uint64_t value) {
    atomic_store_explicit((volatile _Atomic uint64_t *)ptr, value,
                          memory_order_release);
}

void rsac_atomic_store_u64_relaxed(volatile uint64_t *ptr, uint64_t value) {
    atomic_store_explicit((volatile _Atomic uint64_t *)ptr, value,
                          memory_order_relaxed);
}

uint64_t rsac_atomic_fetch_add_u64_relaxed(volatile uint64_t *ptr,
                                           uint64_t value) {
    return atomic_fetch_add_explicit((volatile _Atomic uint64_t *)ptr, value,
                                     memory_order_relaxed);
}

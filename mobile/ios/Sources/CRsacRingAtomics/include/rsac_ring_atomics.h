#ifndef RSAC_RING_ATOMICS_H
#define RSAC_RING_ATOMICS_H

// C11 stdatomic shim for RsacBroadcastKit.
//
// Why this exists: the cross-process mmap SPSC ring (RingLayout.swift) needs
// acquire/release atomic loads/stores on u64 header fields that live in a
// memory-mapped file shared between the broadcast extension (producer) and
// the host app / rsac Rust consumer (rsac-b3aa). Swift has no
// standard-library atomics that operate on raw shared memory before the
// iOS 18 `Synchronization` module, and the package targets iOS 14+, so the
// atomic operations are implemented here in C11 <stdatomic.h>.
//
// All pointers passed in MUST be 8-byte aligned (RingLayout guarantees this:
// every atomic field sits at an 8-byte-aligned header offset).
//
// The Rust consumer mirrors these with core::sync::atomic::AtomicU64 +
// Ordering::{Acquire,Release,Relaxed} on the same mapped bytes.

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Atomic load with acquire ordering.
uint64_t rsac_atomic_load_u64_acquire(const volatile uint64_t *ptr);

/// Atomic load with relaxed ordering.
uint64_t rsac_atomic_load_u64_relaxed(const volatile uint64_t *ptr);

/// Atomic store with release ordering.
void rsac_atomic_store_u64_release(volatile uint64_t *ptr, uint64_t value);

/// Atomic store with relaxed ordering.
void rsac_atomic_store_u64_relaxed(volatile uint64_t *ptr, uint64_t value);

/// Atomic fetch-add with relaxed ordering. Returns the PREVIOUS value.
uint64_t rsac_atomic_fetch_add_u64_relaxed(volatile uint64_t *ptr,
                                           uint64_t value);

#ifdef __cplusplus
}
#endif

#endif /* RSAC_RING_ATOMICS_H */

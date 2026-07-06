//! Android cdylib shim producing `librsac.so` (rsac-0aa9).
//!
//! This crate intentionally contains no logic. It exists so cargo-ndk can
//! build a `cdylib` named `rsac` for the Android ABIs without adding a
//! `cdylib` crate-type to the root rsac manifest (which would bolt an extra
//! artifact onto every desktop build). See the manifest for the full
//! decision record.
//!
//! The `pub use` below links the rsac rlib into this cdylib. `#[no_mangle]
//! extern "C"` symbols defined inside rsac — the JNI export surface arriving
//! with rsac-77f1 (`JNI_OnLoad`, `Java_ai_codeseys_rsac_*`) — are preserved
//! as exported dynamic symbols of the resulting `.so`, which is exactly what
//! `System.loadLibrary("rsac")` on the Kotlin side (mobile/android) resolves
//! against.
//!
//! Until rsac-77f1 lands, the `.so` exports no JNI symbols; the Kotlin glue
//! guards for that via `RsacProjection.isNativeAvailable()`. Building this
//! crate in CI still has real verification value: it is the first step that
//! actually LINKS the Android backend (`#[link(name = "aaudio")]`) against
//! the NDK, which `cargo check` never does.

// Link the whole rsac library (and thereby its future no_mangle JNI surface)
// into this cdylib.
pub use rsac;

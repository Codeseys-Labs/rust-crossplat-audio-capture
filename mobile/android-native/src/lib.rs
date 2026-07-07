//! Android cdylib shim producing `librsac.so` (rsac-0aa9).
//!
//! This crate intentionally contains no logic. It exists so cargo-ndk can
//! build a `cdylib` named `rsac` for the Android ABIs without adding a
//! `cdylib` crate-type to the root rsac manifest (which would bolt an extra
//! artifact onto every desktop build). See the manifest for the full
//! decision record.
//!
//! The `pub use` below links the rsac rlib into this cdylib. `#[no_mangle]
//! extern` symbols defined inside rsac — the JNI export surface (rsac-77f1)
//! — are preserved as exported dynamic symbols of the resulting `.so`,
//! which is exactly what `System.loadLibrary("rsac")` on the Kotlin side
//! (mobile/android) resolves against. The surface is deliberately **one
//! symbol**: `JNI_OnLoad` (src/audio/android/jni.rs), which registers the
//! `ai.codeseys.rsac` natives via `RegisterNatives` — there are no `Java_*`
//! name-resolved exports. The CI android mobile leg asserts the export with
//! llvm-nm after building the `.so`.
//!
//! Building this crate in CI has real verification value beyond the export:
//! it is the step that actually LINKS the Android backend
//! (`#[link(name = "aaudio")]`) against the NDK, which `cargo check` never
//! does.

// Link the whole rsac library (and thereby its future no_mangle JNI surface)
// into this cdylib.
pub use rsac;

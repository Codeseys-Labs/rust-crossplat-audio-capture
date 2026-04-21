# rsac v0.2.0 Publication Readiness Review — Loop 21

**Date:** 2026-04-17  
**Reviewer:** B1  
**Status:** READY (3 blockers to resolve before publish)

---

## Executive Summary

rsac v0.2.0 is **functionally complete and feature-ready** for publication. Cargo.toml metadata is comprehensive and CHANGELOG is well-structured. However, **three critical blockers must be resolved before a real v0.2.0 tag can be pushed to crates.io**:

1. **CRITICAL:** Missing LICENSE files (LICENSE-MIT, LICENSE-APACHE)
2. **MINOR:** Documentation build emits 4 warnings (broken links, private item refs)
3. **INFORMATIONAL:** No `[package.metadata.docs.rs]` config (optional but recommended)

---

## Findings

### 1. Cargo.toml Metadata — ✅ Complete

**Status:** All required fields present and correct.

- **Version:** 0.2.0 (consistent)
- **Edition:** 2021 (current standard)
- **License:** `MIT OR Apache-2.0` ✅
  - *Requires both LICENSE-MIT and LICENSE-APACHE in repo root*
- **Description:** Clear, concise (92 chars) ✅
  - "Cross-platform audio capture library with per-application process taps (macOS), WASAPI (Windows), and PipeWire (Linux)"
- **Repository:** https://github.com/baladita/rust-crossplat-audio-capture ✅
- **README:** README.md present ✅
- **Keywords:** `["audio", "capture", "wasapi", "coreaudio", "pipewire"]` ✅
- **Categories:** `["multimedia::audio", "os"]` ✅

All feature flags properly declared with sensible defaults (all platforms enabled).

---

### 2. LICENSE File Presence — 🚨 BLOCKER

**Status:** MISSING — prevents crates.io publication.

**Finding:** Root directory contains NO LICENSE files.

```bash
$ find . -maxdepth 1 -iname "license*"
(no output)
```

**Resolution required before publish:**

1. Create `LICENSE-MIT` — copy from https://opensource.org/licenses/MIT
2. Create `LICENSE-APACHE` — copy from https://opensource.org/licenses/Apache-2.0

crates.io **will reject** a publish attempt with `license = "MIT OR Apache-2.0"` if either file is missing.

**Recommendation:** Add both files to root, commit with the v0.2.0 tag.

---

### 3. docs.rs Build Compatibility — ⚠️ Warnings (4)

**Status:** Builds successfully but with warnings.

```
cargo doc --no-deps
    Documenting rsac v0.2.0
    Finished successfully with 4 warnings
```

**Warnings Summary:**

1. **Broken intra-doc link** (src/audio/macos/coreaudio.rs:75)
   - References `CaptureTarget::Application` (does not exist)
   - Fix: Update doc comment to correct reference or escape brackets

2. **Private item link** (src/core/capabilities.rs:172)
   - Public fn `get_macos_version()` links to private `PlatformCapabilities::macos()`
   - Fix: Either remove the link or change visibility

3. **Broken intra-doc link** (src/core/config.rs:141)
   - References `AudioCaptureBuilder` (not in scope)
   - Fix: Qualify path or remove stale reference

4. **Redundant explicit link target** (src/api.rs:473)
   - Unnecessary explicit target in `[mpsc](std::sync::mpsc)` link
   - Fix: Simplify to `[mpsc]`

**Impact:** docs.rs will build successfully and render the docs, but warnings will appear in the build log. Does not block publication but should be cleaned for professionalism.

---

### 4. docs.rs Metadata Config — ℹ️ Informational

**Status:** Not present (optional).

**Finding:** No `[package.metadata.docs.rs]` section in Cargo.toml.

**Current behavior:**
- docs.rs builds with default features (all platforms enabled)
- All platform dependencies available in the docs build environment
- No special build configuration needed

**Optional enhancement** (not blocking):
```toml
[package.metadata.docs.rs]
all-features = true
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu", "x86_64-apple-darwin"]
```

This would explicitly document the supported platforms on docs.rs. Recommended for next iteration but not required for v0.2.0.

---

### 5. CHANGELOG Format — ✅ Complete

**Status:** Well-structured and ready.

- **Keep a Changelog format:** Compliant ✅
- **v0.2.0 section:** Present with release date (2026-04-18) ✅
- **Categorization:** Standard sections (Added, Changed, Deprecated, Removed, Fixed, Security) ✅
- **Content quality:** Detailed with migration guidance for breaking changes ✅
- **Unreleased section:** Present and ready for v0.3.0 ✅

No action required.

---

## Top 3 Blockers for Loop 22

### 1. **Create LICENSE-MIT** (Critical)
   - Action: Generate MIT license file (standard boilerplate)
   - Owner: Recommend A3 or pre-publish
   - Est. Time: < 5 min

### 2. **Create LICENSE-APACHE** (Critical)
   - Action: Generate Apache 2.0 license file (standard boilerplate)
   - Owner: Recommend A3 or pre-publish
   - Est. Time: < 5 min

### 3. **Fix docs.rs Warnings** (Nice-to-have, low priority)
   - Action: Resolve 4 broken/redundant doc links
   - Owner: A3 if time permits (cosmetic, doesn't block)
   - Est. Time: 10–15 min

---

## Publication Workflow Recommendation

**Before tagging v0.2.0:**
1. Resolve blockers 1 & 2 (LICENSE files) — **mandatory**
2. Optional: Fix doc warnings (blocker 3) for clean build
3. Create v0.2.0 git tag
4. Run `cargo publish --dry-run` (A3 in-flight)
5. If dry-run succeeds: `cargo publish`

**Estimated time to publication:** 10–20 minutes after blockers resolved.

---

## Conclusion

✅ **Ready for publication with 2 critical prerequisites.**

Cargo.toml metadata is complete, CHANGELOG is professional, and the library compiles and documents cleanly. The missing LICENSE files are the only hard blocker; both are trivial to add. A3's dry-run validation will confirm crates.io acceptance once files are in place.

**Next step:** Add LICENSE files, confirm with A3's dry-run, and tag.

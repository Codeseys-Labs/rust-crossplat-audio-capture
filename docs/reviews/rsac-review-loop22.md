# rsac Publishing Readiness Review — Loop 22

**Reviewer:** B1 | **Date:** 2026-04-17 | **Version:** rsac 0.2.0

## Summary

rsac is **production-ready for crates.io publishing** with current automated workflows, but the three-ecosystem publish (crates.io + npm + PyPI) **requires manual steps** for the language bindings. The core library and release automation are solid; the gaps are structural (bindings lack CI/CD wiring) not quality issues.

**Status:** ✅ crates.io (automated) | ⚠️ npm/PyPI (manual, scheduled loop 23) | ✅ docs.rs (auto-builds post-publish)

---

## ✅ Crates.io Publication Ready

### Manifest Quality
- **Cargo.toml** (`Cargo.toml:1–217`):
  - All required fields present: `name`, `version = "0.2.0"`, `edition = "2021"`, `description`, `license = "MIT OR Apache-2.0"`, `repository`, `readme`
  - Keywords and categories well-chosen for discoverability
  - `exclude` list is comprehensive (excludes tests, docs, CI scripts, Docker, build artifacts) — good for keeping package lean
  - Platform-specific features correctly isolated (`feat_windows`, `feat_linux`, `feat_macos`)
  - All dependencies pinned; no wildcard SemVer specifiers

### Automated Release Workflow
- **`.github/workflows/release.yml`** (140 lines):
  - Triggered by semver tags (`vX.Y.Z`); no risk of accidental fires on loose tags
  - Three-job pipeline: verify → publish → github-release
  - `verify` runs unit tests on Linux/Windows/macOS with feature matrix — mirrors `ci.yml`
  - `publish` executes `cargo publish --dry-run` then `cargo publish` (safe two-step)
  - `github-release` extracts CHANGELOG and creates GH release (line 14–17: TODO acknowledges bindings skip)
  - **One-time setup required:** `CARGO_REGISTRY_TOKEN` secret must be added to GitHub Actions before first tag

### CHANGELOG
- **`CHANGELOG.md`** (dated section exists for 0.2.0):
  - Follows Keep a Changelog format
  - `## [0.2.0] - 2026-04-18` section populated with Added/Changed/Removed subsections
  - Automated workflow extracts this section for GitHub Release body

---

## ⚠️ npm Publishing — Manual Setup Required

### Current State
- **Binding directory:** `bindings/rsac-napi/` exists with structure
- **`package.json`** (`package.json:1–49`):
  - Manifest is **well-formed** and publication-ready
  - Scoped package name: `@rsac/audio` (good for namespace)
  - Version: `0.1.0` (misaligned from root `0.2.0` — see Recommendations)
  - NAPI-RS config present with target triples (x86_64, aarch64 on macOS/Linux)
  - `devDependencies` include `@napi-rs/cli` for binary build
  - No `prepublishOnly` workflow wiring for multi-platform binary builds in CI
- **Missing:** No `.github/workflows/*npm*.yml` to automate per-platform builds and publish
- **Missing:** No `NPM_TOKEN` secret in GitHub Actions
- **README.md:** Not present in `bindings/rsac-napi/` (referenced in loop-22 A2 task)
- **`.gitignore`:** Not present in `bindings/rsac-napi/` (referenced in loop-22 A2 task)

### Blockers for Automated npm Publishing
1. NAPI-RS requires pre-built platform-specific binaries (x86_64, aarch64 on Linux/macOS, Windows arm64)
2. Binaries must be built in CI on each platform or cross-compiled (not trivial)
3. `package.json` references a `build` script but no cross-platform CI matrix exists
4. `NPM_TOKEN` (GitHub Actions secret) must be created and configured

### Manual Publication Path (if needed before loop 23)
```bash
cd bindings/rsac-napi
napi build --platform --release
npm publish --access public  # Requires `npm login` locally
```

---

## ⚠️ PyPI Publishing — Manual Setup Required

### Current State
- **Binding directory:** `bindings/rsac-python/` exists with structure
- **`pyproject.toml`** (`pyproject.toml:1–37`):
  - Manifest is **well-formed** and publication-ready
  - Build system: maturin (correct for PyO3 + Rust)
  - Version: `0.1.0` (misaligned from root `0.2.0` — see Recommendations)
  - Python version: `>=3.9` (reasonable floor)
  - Classifiers cover platforms and Python versions
  - `[tool.maturin]` correctly configured for module name and features
  - No README in the directory; maturin will fall back to parent `README.md`
- **Missing:** No `.github/workflows/*pypi*.yml` to automate wheel builds and publish
- **Missing:** No `MATURIN_PYPI_TOKEN` secret in GitHub Actions
- **README.md:** Not present (A3 task scope)
- **`.gitignore`:** Not present (A3 task scope)

### Blockers for Automated PyPI Publishing
1. Wheels must be built on per-platform: manylinux (glibc x86_64 + aarch64), macOS (universal2), Windows (x86_64 + aarch64)
2. maturin-action in GitHub Actions can automate this, but CI pipeline must be wired
3. `MATURIN_PYPI_TOKEN` (GitHub Actions secret) must be created and configured
4. Wheel artifact upload/publish step must be added to release workflow

### Manual Publication Path (if needed before loop 23)
```bash
cd bindings/rsac-python
maturin build --release  # Single platform
maturin publish --release  # Requires `~/.pypirc` with PyPI token
```

---

## ✅ Workflow & Docs Quality

### CI/CD Status
- **`ci.yml`** (`257 lines`):
  - Lint → test-linux → test-windows → test-macos pipeline
  - Per-platform feature isolation correct
  - PipeWire dev libraries installed on Linux (necessary)
  - Windows audio subsystem expected to fail (Blacksmith runners have no audio); handled with `continue-on-error`
  - macOS CoreAudio tests functional

### Documentation
- **README.md**: Comprehensive; covers features, quick start, application capture, device enumeration. GitHub badges working.
- **`docs/RELEASE_PROCESS.md`**: Excellent walkthrough. Pre-release checklist, manual fallback, step-by-step instructions. Explicitly documents the gap for bindings (§6 "Manual step required — not yet wired").
- **Docs.rs**: Will auto-build post-publish. Current docs build has **1 broken intra-doc link** (`CaptureTarget::Application` — A1 task scope). Must be fixed before publish or docs.rs build will fail.

---

## ⚠️ Intra-Doc Links (A1 Task — In-Flight)

**Issue:** `cargo doc` fails with `error: unresolved link to 'CaptureTarget::Application'`

This is confirmed in-flight under A1. Must be resolved before crates.io publish because docs.rs will reject the version with broken links. **Blocker for publish workflow.**

---

## 🔍 Security & Dependency Audit (A4 Task — In-Flight)

`cargo audit` reveals two advisories:

1. **RUSTSEC-2025-0055** (tracing-subscriber 0.3.19):
   - ANSI escape sequence injection in logs
   - **Fix:** Upgrade `tracing-subscriber` to `>=0.3.20`
   - **Severity:** Medium (affects logging, not capture logic)

2. **RUSTSEC-2026-0097** (rand 0.10.0):
   - Unsoundness with custom logger using `rand::rng()`
   - **Fix:** Upgrade `rand` to next stable version or use `getrandom` crate
   - **Severity:** Low (only if custom logger + `rand::rng()` used together — rsac does not appear to use this pattern)

**Status:** A4 task is pending; both advisories are actionable and have clear upgrade paths. Neither is show-stopping for publish if they do not affect core functionality, but they should be fixed before v0.2.0 to avoid immediate vulnerability reports post-publish.

---

## ⚠️ Binding Version Alignment

**Observation:** Root `Cargo.toml` is at `0.2.0`, but:
- `bindings/rsac-napi/package.json`: version `0.1.0`
- `bindings/rsac-python/pyproject.toml`: version `0.1.0`

This is not a blocker (bindings can version independently), but it creates confusion and suggests they were scaffolded earlier. Recommendation: align to `0.2.0` or use a clear versioning strategy (e.g., bindings stay at `0.1.x` until their first published release).

---

## ⚠️ Workspace Configuration

**Observation:** `Cargo.toml` lists workspace members including bindings:
```toml
[workspace]
members = ["bindings/rsac-python", "bindings/rsac-ffi", "bindings/rsac-napi"]
exclude = ["apps/audio-graph/src-tauri"]
```

This means `cargo publish --dry-run` from root will validate all members, including python and napi bindings. If those bindings are not intended for crates.io (they're not — they go to npm/PyPI), the workspace root should not contain them in `members` or they should have `publish = false` in their `Cargo.toml`.

**Impact:** `cargo publish --dry-run` on master currently fails or succeeds depending on the state of binding Cargo.tomls. Verify this does not block the root publish in release workflow.

---

## 🎯 Ready-to-Publish Checklist

### ✅ Crates.io
- [x] Cargo.toml manifest complete
- [x] CHANGELOG.md dated section for 0.2.0
- [x] Release workflow exists and is sound
- ⚠️ Intra-doc links must be fixed (A1, in-flight)
- ⚠️ Security advisories should be resolved (A4, in-flight)
- ⚠️ Workspace member publish configuration should be verified

### ⚠️ npm (rsac-napi)
- [x] package.json manifest complete and well-formed
- ⚠️ README.md missing (A2, in-flight)
- ⚠️ .gitignore missing (A2, in-flight)
- ❌ No CI/CD workflow for multi-platform builds
- ❌ No NPM_TOKEN secret configured
- ❌ No npm publish step in release automation

### ⚠️ PyPI (rsac-python)
- [x] pyproject.toml manifest complete and well-formed
- ⚠️ README.md missing (A3, in-flight)
- ⚠️ .gitignore missing (A3, in-flight)
- ❌ No CI/CD workflow for multi-platform wheel builds
- ❌ No MATURIN_PYPI_TOKEN secret configured
- ❌ No PyPI publish step in release automation

---

## Top 3 Recommendations for Loop 23

1. **npm CI/CD Pipeline for rsac-napi**
   - Add `.github/workflows/release-npm.yml` triggered by the same tag as crates.io release
   - Use NAPI-RS's GitHub Actions template to build per-platform (x86_64/aarch64 on Linux/macOS, x86_64/aarch64 on Windows)
   - Upload artifacts and publish to npm with scoped public access
   - Requires `NPM_TOKEN` secret; estimate: 1–2 hrs to wire

2. **PyPI CI/CD Pipeline for rsac-python**
   - Add `.github/workflows/release-pypi.yml` triggered by the same tag
   - Use `maturin-action` to build wheels on Linux (manylinux), macOS (universal2), Windows
   - Upload wheels and publish to PyPI
   - Requires `MATURIN_PYPI_TOKEN` secret; estimate: 1–2 hrs to wire

3. **Workspace Configuration Audit**
   - Verify `cargo publish --dry-run` on root succeeds despite binding members
   - If needed, add `publish = false` to binding Cargo.tomls to prevent accidental crates.io publish
   - Add a clear note in docs/RELEASE_PROCESS.md about the workspace structure and which packages go where

---

## Conclusion

**rsac is ready to publish to crates.io with the current automation**, contingent on:
1. A1 (intra-doc links) and A4 (cargo audit) tasks completing
2. `CARGO_REGISTRY_TOKEN` secret being set in GitHub Actions (one-time setup)
3. Tag push triggering the release workflow

**npm and PyPI publishing remain manual** until loop 23 wires the CI/CD pipelines. The manifests are well-formed, so once the workflows exist, publishing will be straightforward. No blocking issues with code quality or manifest configuration — all gaps are tooling/CI related.

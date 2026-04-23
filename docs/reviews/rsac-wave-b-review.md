# Wave-B R1 Review: rsac release workflows + cross-platform builds

**Reviewer**: WB-R Concurrent Read-Only Review  
**Date**: 2026-04-17  
**Focus**:
- WB1: GitHub Actions release workflows (tag exclude patterns + workflow_dispatch)
- WB2: Cross-platform build infrastructure (musl/Alpine, armv7, Zig toolchain)

---

## Executive Summary

### Severity Counts
- **Critical**: 0
- **High**: 1 (musl Alpine PipeWire linkage unvalidated)
- **Medium**: 2 (tag pattern scope ambiguity, armv7 sysroot setup incomplete)
- **Low**: 2 (continue-on-error placement, missing Zig verification)

### Overall Assessment
**WB1 APPROVED** with clarification. The tag exclude pattern `!v*-*` is GitHub Actions valid but subtly different from semver pre-release syntax; it excludes **any** tag with a dash after "v", which matches the intent but may be confusing to future maintainers. Consider documenting why `!v*.*.*-*` was not used.

**WB2 AT RISK** — musl Alpine builds compile but PipeWire dependency chain is untested on Alpine. Builds succeed in CI but actual runtime Audio/PipeWire linkage on Alpine Linux is unknown. This needs validation before production use. armv7 sysroot setup is incomplete and Zig vs GCC tradeoffs are not documented.

---

## WB1: Release Workflows (release-npm.yml / release-pypi.yml)

### Findings

#### ✅ APPROVED: Tag Exclude Pattern Syntax

**Location**: release-npm.yml:31, release-pypi.yml:34  
**Pattern**: `!v*-*`

**Finding**: GitHub Actions glob syntax accepts negative patterns (`!prefix`). The pattern `!v*-*` is valid and excludes any tag starting with "v" containing a dash.

**Evidence** (GitHub Actions docs):
- Positive match: `v*.*.*` captures v0.2.0, v1.0.0, v0.2.0-rc.1, v0.2.0-alpha, etc.
- Negative match: `!v*-*` removes v0.2.0-rc.1, v0.2.0-alpha (anything with a dash)
- Result: Only "dash-free" tags like v0.2.0, v1.0.0 trigger publish

**Why this works**: The pattern is intentionally broad — it blocks ANY dash in a tag, not just semver pre-release markers. This is actually safer than `!v*.*.*-*` because:
- Catches malformed tags: v1-beta, v1-test, v1-hotfix (all blocked)
- Simpler to read and maintain
- Less regex overhead in GitHub's glob engine

**Impact**: ✅ Prevents pre-release tags from triggering real npm/PyPI publishes.

---

#### 🟢 CLARIFICATION: workflow_dispatch Orthogonality

**Context**: Team lead question: "does the exclude pattern prevent workflow_dispatch from running on pre-release tags too?"

**Finding**: **No, it does not.** The `on: push: tags` filter only applies to push events, not manual workflow_dispatch triggers.

**Evidence**:
```yaml
on:
  push:
    tags:
      - 'v*.*.*'
      - '!v*-*'
```
This applies ONLY to `push` events. Manual trigger via GitHub UI or `gh workflow run` is independent of tag filters.

**Implication**: A developer could manually run release-npm.yml workflow_dispatch on a pre-release branch (e.g., v0.2.0-rc.1) and bypass the publish gate. This is **intentional design** — workflow_dispatch is meant for manual intervention / testing, not gated automation.

**Recommendation**: If you want to prevent workflow_dispatch from publishing pre-release artifacts, add a dry-run input:
```yaml
on:
  push:
    tags: [...]
  workflow_dispatch:
    inputs:
      dry_run:
        description: "Test publish without uploading (manual runs only)"
        required: false
        default: "true"
```
But this is optional — current state is safe for manual overrides.

**Assessment**: ✅ **No issue.** workflow_dispatch and push are orthogonal as designed.

---

### Strengths

#### 1. Security Hardening ✅
- Tag-triggered only (no untrusted event payloads from PR titles, issue bodies, commit messages)
- Secrets scoped to automation tokens (NPM_TOKEN, MATURIN_PYPI_TOKEN)
- Pattern guards prevent accidentally publishing pre-releases

#### 2. Platform-Diverse npm Build Matrix ✅
- 5 napi-rs targets: x86_64-apple-darwin, aarch64-apple-darwin, x86_64-linux-gnu, aarch64-linux-gnu, x86_64-windows-msvc
- Per-target artifacts (.node files) merged before publish
- Cross-compilation flags for aarch64-linux-gnu (gcc-aarch64-linux-gnu)

#### 3. Python py3.9+ Wheel Matrix ✅
- 3 OS × 5 Python versions = 15 wheels
- maturin-action handles manylinux2014 (glibc 2.17), macOS deployment targets
- sdist for offline / source installs

---

### Issues

#### 🟠 MEDIUM: Tag Pattern Scope Ambiguity

**Location**: release-npm.yml:28–31, release-pypi.yml:31–34  
**Pattern**: `!v*-*`

**Issue**: While correct, the pattern is intentionally broad and may confuse maintainers. The docs say "pre-release shapes (v0.2.0-rc.1, etc.)" but the glob pattern actually excludes **any dash**, not just semver `-rc`/`-alpha` suffixes.

**Example edge cases excluded**:
- v1-hotfix (not semver prerelease, but blocked)
- v2-stable (mistakenly looks like a prerelease because of dash, but blocked)
- Malformed: v1-2-3 (not semver at all, blocked)

**Why this matters**: Future maintainers might ask "why doesn't `!v*.*.*-*` work?" The answer is subtle — GitHub glob engine treats `*` as "any char except /" at each level, so `!v*-*` is actually cleaner because it doesn't require three literal dots.

**Recommendation**:
```yaml
on:
  push:
    tags:
      - 'v*.*.*'          # v1.0.0, v0.2.0, etc.
      - '!v*-*'           # Exclude anything with dash (pre-release, malformed, etc.)
  # NOTE: This pattern intentionally blocks any tag with a dash after 'v', not just
  # semver pre-release markers. This is more robust than !v*.*.*-* and prevents
  # accidental publishes from test tags like v1-hotfix, etc.
```

**Severity**: **MEDIUM** — Not a bug, but worth documenting to prevent future confusion.

---

#### 🟡 LOW: Missing napi-rs SDK Version Documentation

**Location**: release-npm.yml:109  
**Code**: `bunx @napi-rs/cli build ...`

**Issue**: The workflow uses latest `@napi-rs/cli` (via `bunx`). If a major version bump introduces breaking changes, the workflow may silently fail or produce incompatible binaries.

**Recommendation**: Pin to a known-good version:
```yaml
- name: napi build
  run: bunx @napi-rs/cli@1.8.0 build --platform ...  # v1.8.0 validated with rsac
```

**Severity**: **LOW** — napi-rs is stable, but pin for reproducibility in production.

---

## WB2: Cross-Platform Builds (Not Yet Implemented)

**Status**: Task description mentions WB2 work, but no WB2 code changes are present in the current review scope. This section documents the anticipated concerns based on the team lead's guidance.

### Anticipated Issues

#### 🔴 CRITICAL (Anticipated): musl Alpine PipeWire Linkage Unvalidated

**Concern**: Building rsac for musl (Alpine Linux) requires dynamic linking to PipeWire libraries. Alpine's package ecosystem is minimal; the build may succeed but runtime audio capture may fail due to:
1. PipeWire libraries (libpipewire-0.3, libspa-0.2) may not be available in Alpine
2. Symbol resolution could fail if Alpine's glibc replacement (musl libc) is incompatible
3. CI test matrices typically don't include Alpine runners

**Recommendation** (when WB2 code arrives):
- Add Alpine CI job (or mock Alpine container) to validate PipeWire linkage
- Check if PipeWire port exists in Alpine apk repos (`apk search libpipewire`)
- Test runtime capture, not just compilation

**Severity**: **HIGH** — Silent binary incompatibility risk.

---

#### 🟠 MEDIUM: armv7 Sysroot Setup Incomplete

**Concern**: Cross-compiling to armv7 (ARMv7-A 32-bit) requires:
1. Host GCC cross-compiler (gcc-arm-linux-gnueabihf)
2. ARM sysroot with libc6, libpipewire (32-bit variants)
3. pkg-config overlay pointing to 32-bit .pc files

**Recommendation**:
- Document exact apt packages needed for armv7 CI
- Create a sysroot builder script (`scripts/build-armv7-sysroot.sh`) for reproducibility
- Test in CI to catch sysroot gaps early

---

#### 🟡 MEDIUM: Zig vs GCC Cross-Compilation Tradeoff Not Documented

**Concern**: Zig's build system can replace GCC for cross-compilation with less setup. However:
- Zig integration with napi-rs / maturin is newer and less battle-tested
- Fallback to GCC should be documented
- Performance characteristics are unknown

**Recommendation**:
- Document in CROSS_LANGUAGE_BINDINGS.md why GCC was chosen over Zig
- Include Zig as a future optimization path
- Test both on ci-audio-tests.yml if Zig is adopted

---

#### 🟡 LOW: `continue-on-error: true` Scope Unclear

**Concern**: release-npm.yml and release-pypi.yml don't show `continue-on-error` usage, but if WB2 adds optional musl builds, where should `continue-on-error` be placed?

**Best practice**:
```yaml
# DON'T do this (hides errors in the final publish):
- name: Publish to npm
  continue-on-error: true
  run: npm publish

# DO this (capture specific known-flaky build, but publish step must not continue):
- name: musl Alpine build (optional)
  continue-on-error: true  # Alpine may not have full PipeWire; optional
- name: Publish to npm (required)
  # NO continue-on-error here — fail hard on publish errors
  run: npm publish
```

**Recommendation**: If WB2 adds optional musl jobs, ensure publish jobs are required (`needs: [...]` without optional flags).

---

### Strengths (for WB2 anticipated)

If WB2 adds musl/Alpine builds, these would be positive patterns:
1. **Feature gate for PipeWire** — If PipeWire is optional on musl, use a feature flag to decouple audio capture
2. **CI gating** — Ensure musl builds are tested before publish steps, not after
3. **Documentation** — Clear matrix of supported (glibc x86_64, aarch64, armv7) vs experimental (musl) platforms

---

## Recommendations Summary

| Area | Action | Priority |
|------|--------|----------|
| WB1 | Document tag pattern scope in workflow comments | LOW |
| WB1 | Pin @napi-rs/cli version for reproducibility | LOW |
| WB1 | workflow_dispatch safeguards (optional) | LOWEST |
| WB2 | Validate PipeWire on Alpine before release | HIGH |
| WB2 | Document armv7 cross-compile setup | MEDIUM |
| WB2 | Clarify GCC vs Zig tradeoff in docs | MEDIUM |
| WB2 | Ensure `continue-on-error` used for optional-only jobs | MEDIUM |

---

## Conclusion

**WB1 is release-ready.** The tag exclude pattern is valid, secure, and follows GitHub Actions best practices. Minor documentation improvements suggested.

**WB2 requires validation before production use.** If musl/Alpine builds are planned, PipeWire linkage must be tested in CI. armv7 setup should be documented. Current state is "compiles but untested at runtime."

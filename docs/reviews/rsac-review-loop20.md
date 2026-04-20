# rsac Root Repo Review — Loop-20 (B1)

**Date:** 2026-04-17  
**Scope:** Read-only review focused on release automation safety and `cargo publish` blockers for v0.2.0.

---

## 1. Release Automation Status

### Current State
- **No `.github/workflows/release.yml`** exists in the rsac root repo.
- **All release steps are manual** per `docs/RELEASE_PROCESS.md` (§1–9):
  - Version bump in `Cargo.toml`
  - Tag creation (`git tag -a vX.Y.Z`)
  - Manual `cargo publish` invocation
  - Manual verification on crates.io
  - Binding crate publishes (not yet set up)

### In-Flight Work (Task A3)
The audio-graph team has a release workflow at `apps/audio-graph/.github/workflows/release.yml` (tag-triggered, produces platform-specific binaries). **rsac does not have an equivalent yet** — this is the gap A3 is addressing.

---

## 2. Cargo Publish Blockers for v0.2.0

### Registry Availability ✓ **CLEAR**
**All dependencies are on crates.io.** No git/path dependencies block publish.

**Root rsac dependencies:**
- Direct: futures-core, rtrb, atomic-waker (optional), thiserror, log, hound, clap, color-eyre, ctrlc, libc, env_logger, serde, serde_json
- Platform-specific (Windows): wasapi, windows, windows-core, widestring, sysinfo
- Platform-specific (Linux): pipewire, libspa, libspa-sys
- Platform-specific (macOS): coreaudio-rs, coreaudio-sys, objc2, objc2-foundation, objc2-app-kit, core-foundation, core-foundation-sys, sysinfo
- Build: pkg-config
- Dev: criterion, tempfile, rand, rand_pcg, tokio, futures-util

**All are public crates on crates.io.** ✓

**Binding crates** (workspace members `bindings/{rsac-ffi,rsac-napi,rsac-python}`):
- All depend on rsac via **path dependencies** (`path = "../../"`)
- These are **NOT published to crates.io** yet (per §6 of RELEASE_PROCESS.md: "binding crates exist as placeholders")
- Excludes them from `cargo publish` scope automatically

---

### Manifest Completeness ⚠ **WARNING**

**`cargo publish --dry-run` reports:**
```
warning: manifest has no description, license, license-file, documentation, homepage or repository
```

**Required fields missing in root `Cargo.toml`:**
- `license` or `license-file`
- `description`
- `documentation` (optional but recommended)
- `homepage` (optional but recommended)
- `repository` (optional but recommended — helps crates.io routing)

**Impact:** `cargo publish` will **fail outright**, not warn. This must be fixed before v0.2.0 can publish.

**Fix location:** `/Users/baladita/Documents/DevBox/rust-crossplat-audio-capture/Cargo.toml` lines 1–7

---

### Dirty Working Tree ⚠ **BLOCKER**

**Current state:**
```
10 uncommitted files in working directory:
.github/workflows/release.yml (A3 in-flight)
apps/audio-graph/docs/reviews/audio-graph-review-loop20.md (B2 in-flight)
apps/audio-graph/src/components/StorageBanner.tsx (A2 in-flight)
apps/audio-graph/src/components/TokenUsagePanel.tsx (A1 in-flight)
apps/audio-graph/src/components/TokenUsagePanel.test.tsx (A1 in-flight)
apps/audio-graph/src/hooks/useTauriEvents.ts (A1 in-flight)
apps/audio-graph/src/i18n/locales/en.json (A2, A1 in-flight)
apps/audio-graph/docs/RELEASE_PROCESS.md
docs/RELEASE_PROCESS.md (same file, pulled in by submodule)
```

**Impact:** `cargo publish` requires a clean working tree or `--allow-dirty` flag. Using `--allow-dirty` is **unsafe** for production releases — it can package uncommitted code into the published crate.

---

## 3. Release Workflow Safety Analysis (A3 Target)

### Key Requirements for A3's `.github/workflows/release.yml`

#### 3.1 Fail-Clean on Publish Failure
**Requirement:** If `cargo publish` fails, the workflow must not leave rsac in a partial state (tag pushed, crates.io partially updated, follow-up manual steps in doubt).

**Recommendations:**
- Use a multi-step job:
  1. Pre-flight checks: `cargo publish --dry-run` (must pass)
  2. Tag creation + push (only after dry-run succeeds)
  3. Publish to crates.io
  4. On publish failure: do NOT attempt recovery; let the job fail loudly so maintainer can investigate
- Do NOT commit/push tags until `--dry-run` passes
- Do NOT use auto-retry loops on publish — each attempt is idempotent on crates.io, but stale state is hard to debug

**Reference:** audio-graph's workflow uses `tauri-action` which is more complex (cross-platform builds); rsac can be simpler since it's publish-only.

---

#### 3.2 Manual Approval for First Publish
**Requirement:** v0.2.0 is the **first rsac release to crates.io** (previously unreleased). Auto-publishing on semver tags could slip through errors.

**Recommendations:**
- For the **v0.2.0 tag specifically**, require manual approval in GitHub Actions (use `environment` guard or require PR review for the release tag commit)
- After v0.2.0 lands, future semver tags can auto-publish if dry-run passes
- **Do NOT auto-publish pre-release tags** (see §3.3 below)

**Implementation:**
```yaml
jobs:
  publish:
    environment: production  # Requires manual approval
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      # ... publish steps
```

---

#### 3.3 Pre-Release Tag Safety
**Requirement:** Do NOT publish pre-release versions (e.g., `v0.2.0-rc.1`, `v0.2.0-alpha.2`) to crates.io by default.

**Current state:** RELEASE_PROCESS.md says to tag as `v<semver>` (e.g., `v0.2.0`), but does not guard against tagging pre-releases.

**Recommendations:**
- Workflow should detect pre-release tags (regex: `-alpha|-beta|-rc`) and either:
  1. Skip publish (log a notice, pass)
  2. Require manual approval (separate environment)
  3. Publish with `--allow-prerelease` flag (explicit signal)
- Default: skip publish, require manual decision

**Regex for detection:**
```bash
if [[ "$TAG" =~ -alpha|-beta|-rc|-pre ]]; then
  echo "Pre-release tag detected: $TAG — skipping publish. Manual override required."
  exit 0
fi
```

---

#### 3.4 Dual-Repo Submodule Relationship
**Requirement:** rsac root publishes to crates.io; audio-graph (in `apps/audio-graph/` as a git submodule) is a separate Tauri app that depends on rsac via path.

**Current structure:**
```
github.com/Codeseys-Labs/rust-crossplat-audio-capture (rsac root, publishes as crate)
  └─ apps/audio-graph (git submodule, path dep on rsac root)
```

**Implications for release workflow:**
- rsac's release workflow should **NOT** depend on audio-graph's state (rsac can release independently)
- audio-graph's release workflow (already in place) depends on rsac's tagged version
- **Correct order:** rsac v0.2.0 tag + publish to crates.io → then audio-graph can pull it

**Recommendations:**
- Keep rsac and audio-graph release workflows separate
- rsac workflow: tag + publish (no submodule involvement needed)
- audio-graph workflow: runs on its own tags, pulls latest rsac from crates.io (not path dep)
- Document this in RELEASE_PROCESS.md: "rsac publishes first; audio-graph must use published crate version, not path."

**Check:** Currently, audio-graph's `Cargo.toml` has `rsac = { path = "../../../" }`. For the **audio-graph release**, this should be changed to `rsac = "0.2.0"` (or use a build-time substitution). ⚠ **OUT OF SCOPE for A3 (rsac workflow), but noted for audio-graph maintainers.**

---

## 4. Gaps Before v0.2.0 Can Publish

### Blocking
1. **Add missing manifest fields to Cargo.toml:**
   - `description = "Cross-platform audio capture library…"`
   - `license = "MIT OR Apache-2.0"` (or chosen license; check LICENSE* file)
   - `repository = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture"`
   - Optional but recommended: `documentation` (or omit to auto-link to docs.rs)
   - Optional: `homepage`

   **Action:** Add to root `Cargo.toml` before tagging v0.2.0

2. **Ensure working tree is clean:**
   - Commit/merge all in-flight work (A1–A4, B2)
   - Or use a separate release branch (not recommended for a simple crate publish)
   - **Action:** Ensure master is clean before tagging

3. **Run `cargo publish --dry-run` locally:**
   - Must pass cleanly before tagging
   - Catches issues crates.io will reject
   - **Action:** Add this as a manual pre-flight step in RELEASE_PROCESS.md or automate in release workflow

---

### Non-Blocking (Post-v0.2.0)
- **Binding crate publishing:** rsac-napi (npm), rsac-python (PyPI), rsac-go (already external). Not needed for rsac v0.2.0 publish.
- **Changelog automation:** Currently manual edits to CHANGELOG.md. Future improvement.
- **GitHub release creation:** Optional; currently manual post-publish.

---

## 5. Recommended Checklist for A3

When building `.github/workflows/release.yml` for rsac:

- [ ] Trigger on semver tags matching `v*` (e.g., `v0.2.0`)
- [ ] Pre-flight: `cargo publish --dry-run` must pass before any git operations
- [ ] Skip pre-release tags (or require manual approval)
- [ ] Publish to crates.io with error handling (fail loud, don't retry)
- [ ] On success: log the publish time and crates.io URL
- [ ] **For v0.2.0 specifically:** Require manual approval (GitHub environment)
- [ ] Document in RELEASE_PROCESS.md how to trigger the workflow (push a tag from master)
- [ ] Add a "Rollback" section: how to yank if needed (manual `cargo yank --version X.Y.Z` today)

---

## 6. Top 3 for Loop-21

1. **A3 completion:** Finish `.github/workflows/release.yml` with the safety checks above; add missing Cargo.toml metadata; test dry-run locally.
2. **v0.2.0 tag + publish:** Once A3 is merged and Cargo.toml is complete, tag v0.2.0 and trigger the workflow; verify on crates.io.
3. **Audio-graph release unblock:** After rsac v0.2.0 is on crates.io, update audio-graph's Cargo.toml to depend on published rsac (not path), rebuild, and tag audio-graph v0.2.0.

---

## Appendix: Commands for Manual Verification

```bash
# Verify all deps are on crates.io
cargo tree --depth 1

# Dry-run publish (must pass before tagging)
cargo publish --dry-run

# Check for uncommitted changes
git status

# Docs.rs check (post-publish)
# Visit: https://docs.rs/rsac/0.2.0

# Smoke test (post-publish)
mkdir /tmp/rsac-smoke && cd /tmp/rsac-smoke
cargo init
cargo add rsac@0.2.0
cargo build
```

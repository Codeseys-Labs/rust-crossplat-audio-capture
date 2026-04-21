# rsac loop-23 Review: Multi-Ecosystem Release Readiness

**Date:** 2026-04-17  
**Scope:** Post-multi-ecosystem-workflow read-only review of rsac root  
**Focus:** Can we now tag + push and have all 3 registries (crates.io, npm, PyPI) publish automatically?

---

## Executive Summary

**Status: NOT READY FOR MULTI-ECOSYSTEM RELEASE**

The rsac root is **release-ready for crates.io only**. The npm (napi-rs) and PyPI (maturin) workflows are **in-flight** (task A3) and **not yet created**. Two critical blockers remain:

1. **Missing workflow files:**
   - `.github/workflows/release-npm.yml` — NOT YET CREATED
   - `.github/workflows/release-pypi.yml` — NOT YET CREATED

2. **Hardcoded __version__ in rsac-python:**
   - `bindings/rsac-python/src/lib.rs:844` still has `"0.1.0"` hardcoded
   - Should use `env!("CARGO_PKG_VERSION")` for automatic version tracking
   - Task A4 pending but not yet claimed

### Current Flow (Incomplete)

```
git tag v0.2.0 && git push origin v0.2.0
    ↓
.github/workflows/release.yml fires
    ├─ verify (tests on 3 platforms) ✅ WORKING
    ├─ publish → crates.io ✅ WORKING
    └─ github-release ✅ WORKING

[STOPS HERE — npm + PyPI NOT YET AUTOMATED]
```

---

## 1. Existing Release Infrastructure

### release.yml Status

**File:** `.github/workflows/release.yml`

| Component | Status | Details |
|-----------|--------|---------|
| Trigger | ✅ CORRECT | Semver tags (`v*.*.*` shape) properly filter via `on.push.tags` |
| Verify job | ✅ CORRECT | 3-platform matrix (Ubuntu, Windows, macOS), mirrors `ci.yml` tests |
| Publish job | ✅ CORRECT | `cargo publish --dry-run` → `cargo publish` with CARGO_REGISTRY_TOKEN |
| GitHub Release | ✅ CORRECT | Extracts CHANGELOG section, uses softprops/action-gh-release@v2 |

**Version:** Cargo.toml pinned to 0.2.0, matches rsac-napi + rsac-python manifest versions.

**Missing configuration:**
- ❌ No TODO documentation updated in release.yml (line 14-17 still references loop-20 A3 as future work)
- ❌ CARGO_REGISTRY_TOKEN must be set in GH Actions secrets (out of scope for review, but assumed done per RELEASE_PROCESS.md)

---

## 2. Language Bindings Status

### 2.1 rsac-napi (Node.js/TypeScript)

**Manifest:** `bindings/rsac-napi/package.json` (0.2.0)

```json
{
  "name": "@rsac/audio",
  "version": "0.2.0",
  "napi": {
    "name": "rsac-audio",
    "triples": {
      "defaults": true,
      "additional": [
        "aarch64-apple-darwin",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu"
      ]
    }
  }
}
```

**Release target:** npm registry with platform-specific sub-packages (standard NAPI-RS pattern)

**Status:**
- ✅ package.json exists with sensible defaults
- ✅ Cargo.toml (rsac-napi) correctly points to parent rsac via path dependency
- ✅ Build script uses `napi build --platform --release`
- ✅ prepublishOnly hook defined (`napi prepublish -t npm`)
- ❌ **release-npm.yml NOT YET CREATED**

### 2.2 rsac-python (Python/PyO3)

**Manifests:**
- `bindings/rsac-python/Cargo.toml` (0.2.0)
- `bindings/rsac-python/pyproject.toml` (0.2.0)

```toml
[project]
name = "rsac"
version = "0.2.0"
requires-python = ">=3.9"

[tool.maturin]
python-source = "."
module-name = "rsac._rsac"
```

**Release target:** PyPI with platform wheels (manylinux, macOS universal2, Windows)

**Status:**
- ✅ pyproject.toml exists, correctly configured for maturin
- ✅ Cargo.toml points to parent rsac via path dependency
- ✅ Python 3.9–3.13 support declared
- ❌ **release-pypi.yml NOT YET CREATED**
- ❌ **__version__ hardcoded in src/lib.rs (line 844 = "0.1.0")**

**Critical Issue:**
```rust
// bindings/rsac-python/src/lib.rs:844
m.add("__version__", "0.1.0")?;  // ← HARDCODED, STALE
```

Should be:
```rust
m.add("__version__", env!("CARGO_PKG_VERSION"))?;
```

This breaks the binding's version contract and will cause mismatch during release (wheel version 0.2.0, but `rsac.__version__` reports 0.1.0). **Blocks A4 (pending).**

---

## 3. Missing Workflow Files

### 3.1 release-npm.yml (Task A3 – In-Flight)

**Expected structure** (from A3 task description):

```yaml
name: Release (npm)
on:
  push:
    tags:
      - 'v*.*.*'

jobs:
  build-matrix:
    strategy:
      matrix:
        include:
          - os: macos-15
            target: x86_64-apple-darwin
          - os: macos-15
            target: aarch64-apple-darwin
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
          - os: ubuntu-latest
            target: aarch64-unknown-linux-gnu
          - os: windows-latest
            target: x86_64-pc-windows-msvc
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: '20'
          registry-url: 'https://registry.npmjs.org'
      - run: npm install -g bun
      - run: bun install
      - run: bunx @napi-rs/cli build --release --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: bindings/rsac-napi/*.node

  publish:
    needs: build-matrix
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          registry-url: 'https://registry.npmjs.org'
      - run: npm install -g bun
      - run: bun install
      - uses: actions/download-artifact@v4
      - run: bunx @napi-rs/cli prepublish -t npm
      - run: bunx npm publish --access public
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
```

**YAML validation:** NOT CHECKED (file does not exist)

**Potential issues to watch:**
- NAPI-RS action versions and compatibility
- Node version pinning (currently no spec in package.json)
- Registry URL typo in setup-node@v4
- NPM_TOKEN secret must be set in GH Actions

---

### 3.2 release-pypi.yml (Task A3 – In-Flight)

**Expected structure** (from A3 task description):

```yaml
name: Release (PyPI)
on:
  push:
    tags:
      - 'v*.*.*'

jobs:
  build-wheels:
    strategy:
      matrix:
        include:
          - os: ubuntu-24.04
            python-version: '3.9'
          - os: ubuntu-24.04
            python-version: '3.10'
          # ... 3.11–3.13
          - os: macos-15
            python-version: '3.9'
          # ... 3.10–3.13
          - os: windows-2025
            python-version: '3.9'
          # ... 3.10–3.13
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: PyO3/maturin-action@v1
        with:
          python-version: ${{ matrix.python-version }}
          args: --release --out dist --manifest-path bindings/rsac-python/Cargo.toml
      - uses: actions/upload-artifact@v4
        with:
          name: wheels-${{ matrix.os }}-py${{ matrix.python-version }}
          path: dist/*.whl

  publish:
    needs: build-wheels
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
      - run: |
          curl https://sh.rustup.rs -sSf | sh -s -- -y
          cargo install maturin
      - run: maturin upload dist/*.whl -u __token__ -p ${{ secrets.MATURIN_PYPI_TOKEN }}
```

**YAML validation:** NOT CHECKED (file does not exist)

**Potential issues to watch:**
- manylinux compatibility (may need explicit setup)
- PyO3/maturin-action version + configuration
- Python version matrix explosion (9 combinations × 2–3 OS types = ~20–30 jobs)
- MATURIN_PYPI_TOKEN secret must be set

---

## 4. Secret Configuration Gap

**Current state:**

| Secret | Purpose | Status | Location |
|--------|---------|--------|----------|
| `CARGO_REGISTRY_TOKEN` | crates.io publish | ✅ Referenced in release.yml | GH Actions secrets (assumed set) |
| `NPM_TOKEN` | npm publish | ❌ NOT YET NEEDED | N/A (release-npm.yml doesn't exist) |
| `MATURIN_PYPI_TOKEN` | PyPI publish | ❌ NOT YET NEEDED | N/A (release-pypi.yml doesn't exist) |

**Action items** (out of scope, but noted):
1. Generate NPM_TOKEN at npmjs.com (Settings → Tokens → Automation type)
2. Generate PyPI token at pypi.org (account settings → API tokens)
3. Add both to GH Actions repo secrets once A3 creates the workflows

---

## 5. Documentation Gaps

### 5.1 RELEASE_PROCESS.md

**Current status:** Partially stale

**Section 6 ("Publish language bindings")** notes:
```markdown
> **Manual step required — not yet wired:** neither `rsac-napi` 
> (Node.js bindings via NAPI-RS) nor `rsac-python` (Python bindings 
> via PyO3 / maturin) currently has a publish workflow.
```

**Should be updated to:**
1. Add `.github/workflows/release-npm.yml` to the automated flow (post-A3)
2. Add `.github/workflows/release-pypi.yml` to the automated flow (post-A3)
3. Document new secrets: NPM_TOKEN, MATURIN_PYPI_TOKEN
4. Update the TODO comment in release.yml once workflows exist

**Current issues:**
- ❌ Gap documentation exists but is stale
- ❌ No decision recorded on whether to use separate workflows vs. a single multi-job workflow

---

## 6. Downstream Consumer Check

### audio-graph (Tauri app)

**Status:** ✅ SAFE

- `ci.yml` includes `check-audio-graph` job (line 205–230)
- Compiles audio-graph against the current rsac (path dependency)
- This will immediately surface any rsac API breaking changes

**Note:** audio-graph submodule points to a separate repo, so rsac releases do not auto-update it. That's expected (explicit submodule bump required).

---

## 7. CI/CD Sanity Checks

### Existing ci.yml Pipeline

✅ **All green checks:**
1. Lint job (rustfmt + clippy)
2. Per-platform unit tests (Linux, Windows, macOS)
3. ARM64 cross-compile check
4. Downstream audio-graph compile check
5. Binding crates check (rsac-ffi, rsac-napi, rsac-python)

### release.yml Pipeline

✅ **Verify and publish functional for crates.io.**

---

## 8. Readiness Summary

### Blockers (Must fix before 0.2.0 release tag)

| Item | Status | Owner | Action |
|------|--------|-------|--------|
| **A3: release-npm.yml** | IN-FLIGHT | A3 | Create workflow file with napi-rs matrix |
| **A3: release-pypi.yml** | IN-FLIGHT | A3 | Create workflow file with maturin matrix |
| **A4: __version__ env! macro** | PENDING | A4 | Fix `bindings/rsac-python/src/lib.rs:844` |
| **NPM_TOKEN secret** | NOT SET | — | Configure after A3 (manual) |
| **MATURIN_PYPI_TOKEN secret** | NOT SET | — | Configure after A3 (manual) |

### Non-blockers (Nice-to-have, no gate)

| Item | Status | Impact |
|------|--------|--------|
| RELEASE_PROCESS.md update | TODO | Documentation only; doesn't affect automation |
| release.yml TODO comment removal | TODO | Code clarity only |

---

## 9. YAML Syntax Validation

### release.yml

✅ **Valid** — tested via `python3 -c 'import yaml; yaml.safe_load(...)'`

```bash
$ python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"
# No output = valid
```

### release-npm.yml & release-pypi.yml

⏳ **BLOCKED** — files don't exist yet; can't validate until A3 submits them.

**Validation checklist for when A3 submits:**
- [ ] YAML parses (no syntax errors)
- [ ] `on.push.tags` matches semver pattern `v*.*.*`
- [ ] Job `needs` dependencies form a DAG (no cycles)
- [ ] All `${{ secrets.* }}` references match configured secrets
- [ ] All GitHub action versions pinned (e.g., `@v4`, not `@latest`)
- [ ] Matrix strategy keys are valid (no typos in `os`, `target`, `python-version`)

---

## 10. Top 3 Recommendations for loop-24

1. **A3 → Complete release-npm.yml + release-pypi.yml**
   - Validate YAML syntax via Python
   - Test matrix coverage (all triples for napi, all py versions for maturin)
   - Ensure both workflows fire on semver tags and depend on `release.yml` verify job (to avoid re-testing)
   - Confirm NPM and MATURIN tokens will be injectable via GH Actions secrets

2. **A4 → Wire rsac-python __version__**
   - Change line 844 from `"0.1.0"` to `env!("CARGO_PKG_VERSION")`
   - `cargo check -p rsac-python` must pass
   - Bonus: verify wheel rebuild works locally with `uvx maturin` if environment available

3. **Manual GH Actions Secret Setup** (post-A3, pre-release)
   - Generate NPM_TOKEN, MATURIN_PYPI_TOKEN
   - Add to repo Settings → Secrets and variables → Actions
   - Document in RELEASE_PROCESS.md§1 (prerequisites section)
   - Test with a dry-run tag push (e.g., `v0.2.0-rc1`) to catch secret misconfigs before prod release

---

## Final Verdict

**NOT READY FOR MULTI-ECOSYSTEM RELEASE**

- ✅ crates.io infrastructure solid and tested
- ❌ npm + PyPI workflows missing (in-flight, expected end-of-loop-23)
- ❌ rsac-python __version__ stale (pending, expected end-of-loop-23)
- ⏳ Secrets not yet configured (expected post-workflows, pre-release)

**Next checkpoint:** Loop-24 gates on A3 + A4 completion. Once merged, a second read-only review should validate YAML, secrets, and end-to-end workflow chains before the v0.2.0 tag is pushed.

---

## Appendix: File Inventory

```
.github/workflows/
├── release.yml                  ✅ EXISTS (crates.io only)
├── release-npm.yml              ❌ MISSING (A3 in-flight)
├── release-pypi.yml             ❌ MISSING (A3 in-flight)
├── ci.yml                        ✅ UNRELATED (builds on all PRs)
└── ci-audio-tests.yml           ✅ UNRELATED

bindings/
├── rsac-napi/
│   ├── Cargo.toml               ✅ v0.2.0 (ready)
│   └── package.json             ✅ v0.2.0 (ready)
├── rsac-python/
│   ├── Cargo.toml               ✅ v0.2.0 (ready)
│   ├── pyproject.toml           ✅ v0.2.0 (ready)
│   └── src/lib.rs               ❌ __version__ hardcoded 0.1.0 (A4 pending)
└── rsac-ffi/
    ├── Cargo.toml               ✅ v0.1.0 (ffi-only, separate release cadence)

docs/
├── RELEASE_PROCESS.md           ⏳ TODO (update with 3-workflow architecture)
```


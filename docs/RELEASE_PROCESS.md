# Release Process

This document describes the end-to-end procedure for cutting a new release of
the `rsac` crate and (where applicable) its language bindings. It is written
so a maintainer without prior release context can follow it top-to-bottom.

The worked example throughout is the **0.2.0** release. Substitute your
target version where appropriate.

---

## Automated Release Flow

A single `git tag -a vX.Y.Z && git push --tags` fans out to **three
registry workflows**. They all key on the same `v*.*.*` tag push and run
in parallel — each publishes to one registry, and the GitHub Release is
created once the crates.io flow finishes.

| Workflow | Registry | Matrix | Key jobs | Required secret |
|---|---|---|---|---|
| `.github/workflows/release.yml` | crates.io | linux/win/mac | `verify` → `publish` → `github-release` | `CARGO_REGISTRY_TOKEN` |
| `.github/workflows/release-npm.yml` | npm (`@rsac/audio`) | 5 napi-rs targets | `verify-napi-build` (×5) → `publish-npm` | `NPM_TOKEN` |
| `.github/workflows/release-pypi.yml` | PyPI (`rsac`) | 3 OS × 5 Python (+ sdist) | `build-wheels` (×15) + `build-sdist` → `publish-pypi` | `MATURIN_PYPI_TOKEN` |

### `release.yml` (crates.io)

1. **`verify`** — matrix of `blacksmith-4vcpu-ubuntu-2404`,
   `blacksmith-4vcpu-windows-2025`, and `blacksmith-6vcpu-macos-15`, each
   running `cargo test --lib` against its platform feature. Mirrors the
   `test-*` jobs in `ci.yml` (including the Windows "no audio subsystem"
   exemption via `continue-on-error`).
2. **`publish`** — depends on `verify`; single Linux runner executes
   `cargo publish --dry-run` and then `cargo publish`. Uses the
   `CARGO_REGISTRY_TOKEN` repo secret.
3. **`github-release`** — depends on `publish`; extracts the CHANGELOG
   section matching the tag version and publishes a GitHub Release via
   `softprops/action-gh-release@v2`.

### `release-npm.yml` (npm)

1. **`verify-napi-build`** — matrix of five napi-rs standard targets:
   `x86_64-apple-darwin`, `aarch64-apple-darwin`,
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (cross-built
   with `gcc-aarch64-linux-gnu`), `x86_64-pc-windows-msvc`. Each job
   sets up Node 20 + Bun, installs `bindings/rsac-napi` deps with `bun
   install`, runs `bunx @napi-rs/cli build --platform --release --target
   <triple>`, and uploads the resulting `.node` as an artifact.
2. **`publish-npm`** — depends on all five matrix entries. Downloads
   every `.node` into `bindings/rsac-napi/artifacts/`, runs `bunx
   @napi-rs/cli artifacts --dir artifacts` to move them into place and
   `bunx @napi-rs/cli prepublish -t npm --skip-gh-release` to generate
   the per-platform sub-packages (`@rsac/audio-darwin-arm64`, etc.),
   then `bunx npm publish --access public --provenance` for the main
   package. Uses `NPM_TOKEN` as `NODE_AUTH_TOKEN` / `NPM_CONFIG_TOKEN`.

### `release-pypi.yml` (PyPI)

1. **`build-wheels`** — matrix of three runners
   (`blacksmith-4vcpu-ubuntu-2404`, `blacksmith-6vcpu-macos-15`,
   `blacksmith-4vcpu-windows-2025`) × five Python interpreters (3.9,
   3.10, 3.11, 3.12, 3.13) = 15 wheel builds. Uses
   `PyO3/maturin-action@v1` with `command: build`, `target: auto`,
   `manylinux: auto`, and
   `--manifest-path bindings/rsac-python/Cargo.toml`. Each wheel is
   uploaded as an artifact.
2. **`build-sdist`** — single Linux job, `PyO3/maturin-action@v1` with
   `command: sdist`. Uploads the `.tar.gz`.
3. **`publish-pypi`** — depends on both of the above. Downloads all
   artifacts into `dist-all/` with `merge-multiple: true`, then
   `PyO3/maturin-action@v1` with `command: upload` and
   `--skip-existing dist-all/*`. Uses `MATURIN_PYPI_TOKEN`.

   Known limitations:
   - `manylinux: auto` resolves to `manylinux2014` (glibc 2.17+) on
     x86_64, which is fine for Python 3.9+. If downstream users need
     older glibc, pin to `manylinux_2_17` explicitly.
   - macOS 15 runners produce wheels tagged with the Rust toolchain's
     default deployment target (macOS 11.0+ on `arm64`, 10.12+ on
     `x86_64`). Users on older macOS need to install from sdist.
   - Python 3.13 wheels require `maturin>=1.7` (already pinned in
     `bindings/rsac-python/pyproject.toml`).

### One-time setup

Before the **first** tag push, a maintainer must create repo secrets
for each registry they plan to publish to:

| Secret | Registry | Source |
|---|---|---|
| `CARGO_REGISTRY_TOKEN` | crates.io | <https://crates.io/me> → API Tokens, scope `publish-update` (+ `publish-new` if not yet published) |
| `NPM_TOKEN` | npm | <https://www.npmjs.com/settings/~/tokens> → new **Automation** token with publish rights on the `@rsac` scope |
| `MATURIN_PYPI_TOKEN` | PyPI | <https://pypi.org/manage/account/token/> → new token scoped to the `rsac` project |

Add each via **Settings → Secrets and variables → Actions → New
repository secret**.

Any secret that is missing causes its workflow's `publish-*` job to
fail — the other two flows continue independently. In that state, fall
back to the manual procedure below (§2–§6) for the affected registry.

### Using the automated flow

From a clean `master` with CI already green on the commit you intend to
release (see §2 for the pre-release checklist):

```bash
# Bump the version + promote CHANGELOG entries under a dated heading.
# See §2 "CHANGELOG promotion" and §3 "Version bump" for the details.
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "rsac X.Y.Z"
git push origin master

# Tag and push — this is the workflow trigger.
git tag -a vX.Y.Z -m "rsac X.Y.Z"
git push origin vX.Y.Z
```

Watch the Actions tab. If `verify` fails, delete the tag locally and
remotely (`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`), fix
the underlying issue, and re-tag. crates.io publishes are irrevocable,
so `publish` is gated behind a successful `verify` — but a failure
inside `publish` (e.g. transient network, token rotation) is not safe
to re-run blindly if `cargo publish` already succeeded. Inspect
<https://crates.io/crates/rsac> before re-running.

Bindings are published by the companion workflows described above.
The same tag push triggers `release-npm.yml` and `release-pypi.yml`
in parallel with `release.yml`, so a single `git push --tags`
publishes to all three registries. See §6 for manual fallbacks if one
of the automated flows fails.

---

## 1. Prerequisites

Before you start a release, confirm all of the following:

- **Push access to `master`** on `github.com/…/rust-crossplat-audio-capture`.
  Tag pushes are used as the release trigger, so you also need permission to
  push tags.
- **crates.io API token** exported in your shell:
  ```bash
  export CARGO_REGISTRY_TOKEN="cio_…"
  ```
  Obtain one at <https://crates.io/me> → "API Tokens" → scope: `publish-new`
  and `publish-update` for the `rsac` crate. Keep it out of shell history
  (use a password manager, not `.zshrc`).
- **Local toolchain ≥ 1.95.** The repo pins the toolchain via
  `rust-toolchain.toml` (`channel = "1.95.0"`); `rustup` will install it
  on first `cargo` invocation. Verify with `rustc --version`.
- **Clean working tree** on `master`, synced with `origin/master`:
  ```bash
  git checkout master && git pull --ff-only origin master && git status
  ```

> **Manual fallback procedure.** The sections below describe how to run
> the release by hand. When `.github/workflows/release.yml` is wired up
> (see "Automated Release Flow" above), the tag push does §4–§5 and §7
> step 4 for you; the manual steps here remain the fallback when the
> `CARGO_REGISTRY_TOKEN` secret is unset or a maintainer needs to
> override the automation. `scripts/bump-version.sh` is still absent —
> §3 is always manual today.

---

## 2. Pre-release checklist

Run these locally from the repo root. All must pass before tagging.

```bash
# 1. Tests green on the host platform.
cargo test --all-features

# 2. Clippy clean (pinned toolchain).
cargo clippy --all-targets --all-features -- -D warnings

# 3. Formatting clean.
cargo fmt --all -- --check

# 4. Docs build without broken intra-doc links.
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

Then confirm **per-platform CI is green** on the commit you intend to tag.
The workflows live in `.github/workflows/` (`ci.yml`,
`ci-audio-tests.yml`, and the platform-specific splits). Check the Actions
tab — do not rely solely on local results, since Linux PipeWire, Windows
WASAPI session, and macOS CoreAudio backends each have CI-only coverage.

### CHANGELOG promotion

Open `CHANGELOG.md` and verify that the release section already exists
with a dated heading, e.g.:

```markdown
## [0.2.0] - 2026-04-18
```

The `## [Unreleased]` section above it should either be empty (just the
Added/Changed/… subheadings) or contain only work targeted at the *next*
release. If unreleased entries were accidentally landed under `Unreleased`
that belong in this release, move them under the dated heading **before**
tagging.

---

## 3. Version bump

The crate version lives in `Cargo.toml` at the repo root:

```toml
[package]
name = "rsac"
version = "0.2.0"
```

Bump this to the target version. If the language bindings under
`rsac-napi/` or `rsac-python/` have their own manifests with independent
versions, bump those to match in the same commit. (As of this writing
both binding directories are stubs — no `package.json` / `pyproject.toml`
yet — so only the root `Cargo.toml` needs to change.)

**Worked example — loop-19 / task A2** bumped `Cargo.toml` from `0.1.0`
to `0.2.0` and fixed an unused-variable warning in
`examples/verify_audio.rs` in the same commit. Use that PR as a reference
for the shape of a version-bump change.

Commit:

```bash
git add Cargo.toml Cargo.lock
git commit -m "rsac 0.2.0"
```

Push and ensure CI is green on the bump commit before proceeding.

---

## 4. Tag the release

Annotated tags only — do not use lightweight tags.

```bash
git tag -a v0.2.0 -m "rsac 0.2.0"
git push origin v0.2.0
```

The tag name is `v<semver>`. If a release automation workflow is added
later, this is the event it should key on.

---

## 5. Publish to crates.io

Always dry-run first:

```bash
cargo publish --dry-run
```

The dry-run packages the crate and runs the same validation crates.io
will — missing `license`, `description`, or `repository` fields, files
excluded by `.gitignore` that the manifest references, etc. Fix any
errors, amend, and re-run. Do **not** proceed on warnings you do not
understand.

Once the dry-run is clean:

```bash
cargo publish
```

The upload is irrevocable. crates.io does not allow deleting a published
version (only yanking — see §8).

---

## 6. Publish language bindings

The automated path is `.github/workflows/release-npm.yml` and
`.github/workflows/release-pypi.yml` (see **Automated Release Flow**
above). Both fire on the same `v*.*.*` tag push as `release.yml`.

Before tagging, bump the binding manifests in lockstep with the root
`Cargo.toml`:

- `bindings/rsac-napi/package.json` — `"version": "X.Y.Z"`
- `bindings/rsac-python/pyproject.toml` — `version = "X.Y.Z"` under
  `[project]`. The Rust side's `bindings/rsac-python/Cargo.toml`
  typically tracks this too.

### Manual fallback — `rsac-napi` → npm

If `release-npm.yml` fails (missing `NPM_TOKEN`, transient npm outage,
build regression on one target), publish by hand:

```bash
cd bindings/rsac-napi
bun install
# Build every triple listed in package.json "napi.triples":
bunx @napi-rs/cli build --platform --release --target x86_64-apple-darwin
bunx @napi-rs/cli build --platform --release --target aarch64-apple-darwin
bunx @napi-rs/cli build --platform --release --target x86_64-unknown-linux-gnu
bunx @napi-rs/cli build --platform --release --target aarch64-unknown-linux-gnu
bunx @napi-rs/cli build --platform --release --target x86_64-pc-windows-msvc
bunx @napi-rs/cli prepublish -t npm --skip-gh-release
NODE_AUTH_TOKEN=<npm-token> bunx npm publish --access public
```

Cross-compiling all five targets from one host is impractical; in
practice the manual fallback means rerunning the failed matrix leg on
CI or building per-target on real hardware before running `prepublish`
+ `publish` on the aggregating machine.

### Manual fallback — `rsac-python` → PyPI

If `release-pypi.yml` fails, publish by hand from a Linux box (for
manylinux wheels) and optionally macOS / Windows (for those wheels):

```bash
cd bindings/rsac-python
maturin build --release --out dist --manifest-path Cargo.toml
# Repeat on macOS and Windows if cross-building is unavailable.
maturin upload --skip-existing dist/*
```

`maturin upload` reads `MATURIN_PYPI_TOKEN` from the environment (or
`~/.pypirc`). `--skip-existing` is safe to re-run if a partial upload
landed before the failure.

---

## 7. Verification

After `cargo publish` returns, confirm the release landed:

1. **crates.io page live.** Visit <https://crates.io/crates/rsac> and
   check that the new version appears in the version list and that the
   README renders. Propagation is usually under a minute.
2. **Downloadable from a fresh project.** In a scratch directory:
   ```bash
   mkdir /tmp/rsac-smoketest && cd /tmp/rsac-smoketest
   cargo init
   cargo add rsac@0.2.0
   cargo build
   ```
   This catches missing-file issues the dry-run can miss (e.g. a `build.rs`
   referencing a path excluded from the package).
3. **docs.rs build succeeded.** <https://docs.rs/rsac/0.2.0> — docs.rs
   builds automatically after publish; if it fails, the version page
   shows a build log. Common failure: missing system libs for
   feature-gated backends. Fix with `[package.metadata.docs.rs]` in
   `Cargo.toml`.
4. **GitHub release (optional).** Create a GitHub release against the
   `v0.2.0` tag with the CHANGELOG section pasted as the body. This
   gives non-Rust users a landing page.

---

## 8. Rollback

If a critical bug is discovered post-publish, **yank** the version. Yanking
prevents new projects from resolving it while leaving existing
`Cargo.lock` pins working:

```bash
cargo yank --version 0.2.0
```

To un-yank (if the yank was itself a mistake):

```bash
cargo yank --version 0.2.0 --undo
```

Yanking is not a substitute for a fix. Cut a patch release (e.g. `0.2.1`)
with the bug resolved and publish it using this same procedure.

---

## 9. Post-release

- Open `CHANGELOG.md` and restore the empty `## [Unreleased]` section at
  the top with the standard subsections (`Added`, `Changed`, `Deprecated`,
  `Removed`, `Fixed`, `Security`).
- Announce the release (repo discussions, team channel, or wherever the
  project coordinates).

---

## Gaps / manual steps summary

Tracked here so follow-up release-automation tasks can pick them up:

- All three registry workflows exist (`release.yml`, `release-npm.yml`,
  `release-pypi.yml`). Before the first tag push, the corresponding
  secrets (`CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`, `MATURIN_PYPI_TOKEN`)
  must be set under **Settings → Secrets and variables → Actions**.
  Missing secrets fail only the affected `publish-*` job; the other
  flows continue. Fall back to §2–§6 for the affected registry.
- No `scripts/bump-version.sh` — version strings are edited by hand in
  `Cargo.toml`, `bindings/rsac-napi/package.json`, and
  `bindings/rsac-python/pyproject.toml`. `apps/audio-graph/` has its
  own bump script; a cross-manifest version sync helper is still a
  nice-to-have.
- `release-npm.yml` builds five napi-rs triples; other targets
  (`linux-x64-musl`, `linux-arm-gnueabihf`, FreeBSD, Android) are not
  wired. Add them to the matrix if downstream users request them.
- `release-pypi.yml` relies on `manylinux: auto` — wheels are
  `manylinux2014`. Consuming environments on glibc < 2.17 must install
  from sdist.

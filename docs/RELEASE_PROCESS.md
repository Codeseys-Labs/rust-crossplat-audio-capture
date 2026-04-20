# Release Process

This document describes the end-to-end procedure for cutting a new release of
the `rsac` crate and (where applicable) its language bindings. It is written
so a maintainer without prior release context can follow it top-to-bottom.

The worked example throughout is the **0.2.0** release. Substitute your
target version where appropriate.

---

## Automated Release Flow

`.github/workflows/release.yml` automates the happy path for the crates.io
portion of this procedure. It is triggered by pushing a semver-shaped tag
(`vMAJOR.MINOR.PATCH`, e.g. `v0.2.0`) and runs three jobs in sequence:

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

### One-time setup

Before the **first** tag push, a maintainer must:

- Generate a crates.io API token at <https://crates.io/me> scoped to
  `publish-update` (and `publish-new` if `rsac` has not been published
  yet) for the `rsac` crate.
- Add it to GitHub repo secrets as **`CARGO_REGISTRY_TOKEN`**
  (Settings → Secrets and variables → Actions → New repository secret).

Without this secret the `publish` job will fail with a 401 from
crates.io, leaving `verify` green and the GH release uncreated. In that
state, fall back to the manual procedure below (§2–§5).

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

Bindings (`rsac-napi`, `rsac-python`) are **not** published by this
workflow — see §6 and the TODO comment in `release.yml`.

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

> **Manual step required — not yet wired:** neither `rsac-napi` (Node.js
> bindings via NAPI-RS) nor `rsac-python` (Python bindings via PyO3 /
> maturin) currently has a publish workflow. The binding directories
> exist as placeholders without manifests. Skip this section until they
> are set up; update this document when they are.

When the bindings are ready, the shape of the process will be:

- **`rsac-napi` → npm:**
  ```bash
  cd rsac-napi
  npm run build --release
  npm publish --access public
  ```
  Requires `NPM_TOKEN` / `npm login`. Platform-specific binaries should
  be built in CI (typically via NAPI-RS's GitHub Actions template) and
  published as scoped optional-dep packages.

- **`rsac-python` → PyPI:**
  ```bash
  cd rsac-python
  maturin publish --release
  ```
  Requires `MATURIN_PYPI_TOKEN` or `~/.pypirc`. Wheels must be built
  per-platform (manylinux, macOS universal2, Windows) — typically via
  `maturin-action` in a release workflow.

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

- `.github/workflows/release.yml` exists (see "Automated Release Flow"
  above), but `CARGO_REGISTRY_TOKEN` must be set in GH Actions secrets
  before the first tag push — until it is, the `publish` job will fail
  and the flow degrades to manual §2–§5.
- No `scripts/bump-version.sh` — version strings are edited by hand
  (compare to `apps/audio-graph/`, which does have a bump script).
- `rsac-napi` and `rsac-python` have no package manifests yet — neither
  npm nor PyPI publishing is set up, and `release.yml` intentionally
  does not touch them (a TODO comment in the workflow flags this for a
  future loop).

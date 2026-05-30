# Release Process

This document describes the end-to-end procedure for cutting a new release of
the `rsac` crate and (where applicable) its language bindings. It is written
so a maintainer without prior release context can follow it top-to-bottom.

The worked example throughout is the **0.2.0** release. Substitute your
target version where appropriate.

---

## Automated Release Flow

A single `vX.Y.Z` tag push fans out to **three registry workflows**.
They all key on the same `v*.*.*` tag push and run in parallel — each
publishes to one registry, and the GitHub Release is created once the
crates.io flow finishes.

There are **two ways** that `vX.Y.Z` tag gets created, and they share
this exact same publish fan-out:

1. **Automated, release-please style (recommended for minor/patch).** A
   maintainer runs the **Release Prepare** workflow from the Actions tab;
   it opens a `release: vX.Y.Z` PR (version bump + CHANGELOG rotation), a
   maintainer squash-merges it, and `release-tag.yml` then auto-creates
   and pushes the tag. See
   [§ Automated minor/patch release (release-please style)](#automated-minorpatch-release-release-please-style).
2. **Manual tag push.** A maintainer bumps the manifests + CHANGELOG by
   hand (via `scripts/bump-version.sh`), commits, and pushes an annotated
   `vX.Y.Z` tag directly. This is the **only** supported path for a
   **MAJOR** bump (the automated path refuses to change the major) and the
   fallback whenever the automation is unavailable. See §1–§9 below.

Either way, the three publish workflows below are what actually run once
the tag lands — they never trigger off anything but a `v*.*.*` tag push.

| Workflow | Registry | Matrix | Key jobs | Required secret |
|---|---|---|---|---|
| `.github/workflows/release.yml` | crates.io | linux/win/mac | `verify` → `publish` → `github-release` | `CARGO_REGISTRY_TOKEN` |
| `.github/workflows/release-npm.yml` | npm (`@rsac/audio`) | 8 napi-rs targets (5 required + 3 best-effort) | `verify-napi-build` (×8) → `publish-npm` | `NPM_TOKEN` |
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

1. **`verify-napi-build`** — matrix of eight napi-rs targets, split into
   two tiers:

   **Required (5 targets, must all pass):**
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`
   - `x86_64-unknown-linux-gnu`
   - `aarch64-unknown-linux-gnu` (cross-built with `gcc-aarch64-linux-gnu`)
   - `x86_64-pc-windows-msvc`

   **Best-effort (3 targets, `continue-on-error: true`):**
   - `x86_64-unknown-linux-musl` (via `cargo-zigbuild` + Zig 0.14.1)
   - `aarch64-unknown-linux-musl` (via `cargo-zigbuild` + Zig 0.14.1)
   - `armv7-unknown-linux-gnueabihf` (via `--use-napi-cross` +
     `gcc-arm-linux-gnueabihf`)

   Each job sets up Node 20 + Bun, installs `bindings/rsac-napi` deps
   with `bun install`, runs `bunx @napi-rs/cli build --platform
   --release --target <triple>` (plus `-x` for zigbuild or
   `--use-napi-cross` for napi-cross), and uploads the resulting
   `.node` as an artifact.

   The best-effort tier mirrors napi-rs/package-template's approach for
   musl and armv7. It is marked experimental because rsac links
   `libpipewire-0.3` + `libspa-0.2` via pkg-config on Linux, and neither
   `cargo-zigbuild` nor napi-cross ships a sysroot containing those
   headers. If one of these builds fails, it does not block the npm
   publish of the 5 required targets. The corresponding platform
   sub-package simply will not be produced for that release, and
   downstream users on that platform must build from source.

2. **`publish-npm`** — depends on all eight matrix entries (but the
   best-effort ones are gated by `continue-on-error`, so their failure
   does not block this job). Downloads every available `.node` into
   `bindings/rsac-napi/artifacts/`, runs `bunx @napi-rs/cli artifacts
   --dir artifacts` to move them into place and `bunx @napi-rs/cli
   prepublish -t npm --skip-gh-release` to generate the per-platform
   sub-packages (`@rsac/audio-darwin-arm64`, etc.), then `bunx npm
   publish --access public --provenance` for the main package. Uses
   `NPM_TOKEN` as `NODE_AUTH_TOKEN` / `NPM_CONFIG_TOKEN`.

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

## Automated minor/patch release (release-please style)

This is the **recommended** way to cut a `minor` or `patch` release. It
is a two-workflow, release-please-style flow: a maintainer kicks off a
*prepare* workflow that opens a reviewable release PR, and merging that
PR triggers a *tag* workflow that creates and pushes the `vX.Y.Z` tag.
**No bot ever pushes to `master` directly** — every byte that ships is
reviewed in a normal PR first.

```
 ┌──────────────────────────┐   human runs from Actions tab, picks bump=minor|patch
 │  Release Prepare          │   (.github/workflows/release-prepare.yml)
 │  (workflow_dispatch)      │
 └────────────┬─────────────┘
              │  computes next NON-major version, runs
              │  scripts/bump-version.sh, opens a PR
              ▼
 ┌──────────────────────────┐   human reviews + SQUASH-MERGES
 │  PR: "release: vX.Y.Z"    │   (CI version-lockstep gate must be green)
 └────────────┬─────────────┘
              │  squash commit subject == "release: vX.Y.Z" lands on master
              ▼
 ┌──────────────────────────┐   detects the release commit, creates +
 │  Release Tag on Merge     │   pushes annotated tag vX.Y.Z
 │  (push: master)           │   (.github/workflows/release-tag.yml)
 └────────────┬─────────────┘
              │  tag push
              ▼
   release.yml / release-npm.yml / release-pypi.yml   (the publish fan-out above)
```

### Step 1 — run the "Release Prepare" workflow

From **Actions → Release Prepare → Run workflow**, choose the `bump`
input:

- **`minor`** (the **default**) — `X.(Y+1).0`.
- **`patch`** — `X.Y.(Z+1)`.

There is deliberately **no `major` option**. `release-prepare.yml`
(`.github/workflows/release-prepare.yml`) then:

1. Reads the current `[package].version` from the root `Cargo.toml`,
   parses it as a strict `X.Y.Z`, and computes the next version from the
   `bump` choice. It **asserts the major stays the same** (belt-and-
   suspenders — `minor`/`patch` arithmetic can't change the major, but it
   refuses anyway if a future edit ever does) and **refuses if the tag
   `vX.Y.Z` already exists**.
2. Runs `bash scripts/bump-version.sh <computed-version>` (under
   `TZ=UTC`), which rewrites the five lockstep manifests
   (`Cargo.toml`, `bindings/rsac-napi/{Cargo.toml,package.json}`,
   `bindings/rsac-python/{Cargo.toml,pyproject.toml}`) and rotates
   `CHANGELOG.md` (`[Unreleased]` → `[X.Y.Z] - <UTC date>` plus a fresh
   `Unreleased` scaffold). See §"Versioning & ABI contract" for the
   `bindings/rsac-ffi/Cargo.toml` and `rsac-go` tag caveats the script
   does *not* handle.
3. Opens (via `peter-evans/create-pull-request`) a PR from a
   `release/vX.Y.Z` branch into `master`, titled **`release: vX.Y.Z`**,
   labeled `release`, whose body lists the bumped manifests and the merge
   instructions.

The job is guarded to the `Codeseys-Labs/rust-crossplat-audio-capture`
repo (a fork can't open a bogus release PR) and serialised via a
`concurrency` group so two dispatches can't race on the same branch.
Its `permissions:` are exactly `contents: write` (to push the
`release/*` branch) + `pull-requests: write` (to open the PR) — it never
writes to `master`.

### Step 2 — review and SQUASH-MERGE the release PR

Treat the `release: vX.Y.Z` PR like any other PR: review the manifest
diff and the CHANGELOG rotation. The CI **`version-lockstep` gate must be
green** on the PR before merging — it cross-checks that all **six** manifests
(root `Cargo.toml`, the rsac-ffi / rsac-napi / rsac-python `Cargo.toml`s, the
napi `package.json`, and the python `pyproject.toml`) carry the same version.
Because this gate runs on the PR/master push that subsequently gets tagged, the
tagged commit is already lockstep-verified before `release-tag.yml` tags it. (At
*tag* time, `release.yml` additionally re-checks the tag matches the root
`Cargo.toml`; the full six-manifest lockstep runs on push/PR, not the tag, since
`ci.yml` has no `tags:` trigger.)

> **Merge it as a SQUASH merge, and do NOT edit the commit subject.** The
> tag automation in step 3 keys on the squash-merge commit *subject* being
> exactly **`release: vX.Y.Z`** (which GitHub defaults to the PR title for
> a squash merge). If you reword the subject, or use a merge-commit /
> rebase strategy that changes the HEAD subject, the tag workflow will see
> a "normal" commit and **no tag will be created** — the release silently
> stalls. (Recoverable: fix it by pushing the tag manually per §4.)

### Step 3 — automatic tag creation (`release-tag.yml`)

`release-tag.yml` (`.github/workflows/release-tag.yml`, workflow name
**"Release Tag on Merge"**) runs on **every push to `master`**. On each
push it:

1. **Detects a release commit** — reads `git log -1 --format=%s` and
   proceeds only if the HEAD subject matches exactly
   `release: vX.Y.Z`. Any other commit is a no-op (the remaining steps are
   gated on this). Matching the squash-commit subject is the primary
   signal; it needs no API call and is independent of merge strategy.
2. **Manifest defense** — re-parses `[package].version` from `Cargo.toml`
   at that commit and **refuses to tag** if it disagrees with the version
   in the commit subject.
3. **Major guard** — refuses if the parsed major differs from the
   manifest major. The automated prepare path can never produce a major
   bump, but this defends against a hand-crafted `release:` commit that
   crosses a major boundary.
4. **Idempotency** — checks both the local tag set and `git ls-remote
   --tags origin`; if `vX.Y.Z` already exists it **skips with a notice and
   never re-tags**.
5. **Creates + pushes the annotated tag** — `git tag -a vX.Y.Z -m
   "Release X.Y.Z"` on HEAD (the reviewed, lockstep-passing squash commit)
   and `git push origin vX.Y.Z`. **That push is the sole action** of this
   workflow — it does **not** duplicate any publish logic; the tag push is
   what fans out to `release.yml` / `release-npm.yml` / `release-pypi.yml`
   (the three publish workflows documented under "Automated Release Flow").

This job is also repo-guarded to
`Codeseys-Labs/rust-crossplat-audio-capture` (a fork that merges a
`release:` commit must never tag + publish), serialised via a
`concurrency` group, and runs with least-privilege `permissions: contents:
write` (it only pushes a tag). Because the tagged commit already passed
`version-lockstep` as part of the merged PR, and `release.yml` re-runs
the tag↔manifest check on the tag, the version is verified at three
points.

### What the automated flow will NOT do (use the manual path instead)

- **MAJOR releases.** The `bump` input offers only `minor`/`patch`, the
  prepare job asserts the major is unchanged, and the tag job refuses a
  major-crossing release commit. A `X` → `X+1` (or pre-1.0 `0.x` →
  `0.(x+1)` per the ABI policy in §"Versioning & ABI contract") bump must
  be done **manually**: run `scripts/bump-version.sh <new-major.0.0>`,
  bring `bindings/rsac-ffi/Cargo.toml` to the same version, commit with a
  normal (non-`release:`) message, then tag and push by hand per §4.
- **`bindings/rsac-ffi/Cargo.toml` and the `rsac-go` tag.**
  `bump-version.sh` (and therefore the prepare workflow) does not touch
  the FFI manifest or push the `bindings/rsac-go/vX.Y.Z` Go module tag —
  see §"Versioning & ABI contract" (b) and (c). Reconcile the FFI manifest
  in the release PR before merging, and push the Go tag in lockstep after
  the crate tag lands.
- **A skipped or misnamed squash subject.** If step 3 never fires (subject
  reworded, non-squash merge), the manifests are still correctly bumped on
  `master`; just create the tag manually (§4) to trigger the publish
  fan-out.

### Recap: idempotency and safety properties

- **No direct push to `master`** — the bump only reaches `master` through
  a reviewed, squash-merged PR.
- **Idempotent tagging** — `release-tag.yml` never re-creates an existing
  local or remote tag.
- **`version-lockstep`** gates the release PR and is re-checked on the tag
  by `release.yml`; the five manifests must agree before anything ships.
- **Repo-guarded** — both workflows only run on
  `Codeseys-Labs/rust-crossplat-audio-capture`.
- **Major-proof** — three independent guards (no `major` input, prepare
  assertion, tag-job assertion) keep a major bump off this path.

---

## Versioning & ABI contract

This is the **normative** policy for how `rsac` and its four bindings are
versioned. The release tooling (`scripts/bump-version.sh`, the
`version-lockstep` CI job in `.github/workflows/ci.yml`, and the
tag↔manifest guard in `release.yml`) all enforce pieces of it.

### (a) Lockstep version bumps

`rsac` (root `Cargo.toml`) and the publishable bindings bump **in
lockstep** on every semver tag. A tag `vX.Y.Z` means *all* of these
carry version `X.Y.Z`:

| Manifest | Field | Notes |
|---|---|---|
| `Cargo.toml` | `[package].version` | the crates.io crate, source of truth |
| `bindings/rsac-ffi/Cargo.toml` | `[package].version` | C FFI crate (`publish = false`; versioned for ABI tracking, see (b)) |
| `bindings/rsac-napi/Cargo.toml` | `[package].version` | napi crate |
| `bindings/rsac-napi/package.json` | top-level `"version"` | npm package `@rsac/audio` |
| `bindings/rsac-python/Cargo.toml` | `[package].version` | pyo3 crate |
| `bindings/rsac-python/pyproject.toml` | `[project].version` | PyPI package `rsac` |

Run `bash scripts/bump-version.sh X.Y.Z` to rewrite the manifests it knows
about (root + napi + python, plus the CHANGELOG rotation); bring
`bindings/rsac-ffi/Cargo.toml` to the same value in the same commit. The
`version-lockstep` CI job re-checks all six values on every push/PR
(warning on a mid-cycle skew). Because the release PR's merge commit is a
push to `master`, this gate runs — and must be green — on the exact commit that
`release-tag.yml` then tags, so a mismatched manifest never reaches the tag.
At *tag* time, `release.yml`'s `verify` job independently re-checks the pushed
tag against the root `Cargo.toml` before publishing (a second, registry-side
gate); the full six-manifest lockstep itself runs on the push/PR, not the tag
(`ci.yml` has no `tags:` trigger). A mismatched manifest must never reach a
registry.

> Mid-cycle skew is tolerated by CI (warning only) so a binding can lag
> the root crate between releases, but it must be reconciled before
> tagging. As of this writing `rsac-ffi` trails at `0.1.0` while the
> others are at `0.2.0`; the next release must bring all six to the same
> version.

### (b) C ABI changes are MAJOR for `rsac-ffi`

The `rsac-ffi` crate exposes a C ABI: the exported `extern "C"` symbols
and the generated `rsac.h` header. **Any** of the following is a
**MAJOR** version change for the FFI surface and MUST be called out in
the CHANGELOG under a dedicated `### C ABI changes` subsection (see
[`CHANGELOG.md`](../CHANGELOG.md)):

- removing or renaming an exported symbol;
- changing the signature of an exported function (parameter/return types,
  arity, calling convention);
- changing the layout, size, or field order of any `#[repr(C)]` struct or
  enum crossing the boundary;
- changing the meaning of an existing return/error code.

Additive changes (new symbols, new `#[repr(C)]` types that don't alter
existing ones) are MINOR. Because `rsac` and the bindings bump in
lockstep (a), an ABI-MAJOR change forces the whole line to the next MAJOR
on a `0.x`→`0.(x+1)` (pre-1.0) or `x`→`x+1` (post-1.0) boundary; record
the rationale in the ABI subsection so consumers pinning the `.so`/`.dll`
know to recompile.

### (c) `rsac-go` tag convention

`bindings/rsac-go` is a Go module (`module
github.com/Codeseys-Labs/rsac-go`, see `bindings/rsac-go/go.mod`) and
carries **no in-manifest version** — Go derives versions from git tags.
Because the module lives in a subdirectory, its releases are tagged with
the **module-path-prefixed** form Go's module proxy expects:

```
bindings/rsac-go/vX.Y.Z
```

Push this tag in lockstep with the `vX.Y.Z` crate tag (same `X.Y.Z`).
Consumers then `go get
github.com/Codeseys-Labs/rsac-go@vX.Y.Z`. The `version-lockstep`
CI job notes rsac-go's absence of an in-tree version explicitly so the
gap is intentional, not an oversight.

---

## Pre-release Tags and Dry Runs

All three release workflows key on the `v*.*.*` tag pattern and also
exclude `v*-*` — so only stable semver tags (e.g. `v0.2.0`, `v1.3.7`)
trigger a publish. Pre-release shapes like `v0.2.0-rc.1`,
`v0.2.0-beta.2`, or `v1.0.0-alpha` are **not** published, because
crates.io and PyPI uploads are irrevocable and a mis-tagged RC should
never reach either registry.

This means you can safely push RC tags for internal sharing or CI
sanity checks without risking a real release:

```bash
git tag -a v0.2.0-rc.1 -m "rsac 0.2.0 release candidate 1"
git push origin v0.2.0-rc.1   # does NOT trigger publishes
```

### On-demand dry-run (crates.io)

`release.yml` also accepts a manual `workflow_dispatch` trigger with a
`dry_run` boolean input (defaulting to `true`). This lets a maintainer
rehearse the full `verify → publish` flow on any branch without
tagging. When invoked with `dry_run=true`, the `publish` job runs
`cargo publish --dry-run` and then **skips** the real `cargo publish`
step.

To use it:

1. Go to **Actions → Release → Run workflow**.
2. Pick the branch and leave `dry_run` checked (the default).
3. Start the run and watch `verify` + the dry-run packaging succeed.

Unchecking `dry_run` on `workflow_dispatch` would execute a real
`cargo publish` off a non-tagged commit — do not do that. Real
releases always go through a stable `vX.Y.Z` tag push.

The npm and PyPI workflows do not expose a `workflow_dispatch`: their
publishes are also irrevocable and there is no analogue to `cargo
publish --dry-run` that exercises the whole wheel/napi pipeline. For
those, rely on the tag-exclude guard above plus the existing per-PR CI
matrix.

### Promoting an RC to a stable release

Once the RC is validated, tag the stable version and push:

```bash
git tag -a v0.2.0 -m "rsac 0.2.0"
git push origin v0.2.0
```

That push triggers all three registry workflows in parallel.

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
> the release by hand. For a `minor`/`patch` release you normally do
> **not** run these by hand — use the automated release-please-style flow
> (§"Automated minor/patch release (release-please style)"), which runs
> `scripts/bump-version.sh` for you (§3), opens the bump PR, and pushes the
> tag (§4) on merge; the tag push then does §5 and §7 step 4. These manual
> steps remain the path for a **MAJOR** bump (the automation refuses one),
> when the `CARGO_REGISTRY_TOKEN` secret is unset, or when a maintainer
> needs to override the automation. `scripts/bump-version.sh` now exists
> and rewrites five of the six manifests + rotates the CHANGELOG — §3 is
> still a manual *invocation* of it (plus the `rsac-ffi` reconcile), not a
> hand-edit.

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

#### What `ci.yml` now gates (architecture-critique closures)

The per-OS unit jobs in `ci.yml` were tightened to close gaps flagged in
[`docs/reviews/rsac-architecture-critique-2026-05-30.md`](reviews/rsac-architecture-critique-2026-05-30.md);
each is a hard regression gate on a `vX.Y.Z` release commit:

- **`rt_alloc` runs per-OS (TC-01).** `cargo test --test rt_alloc … --
  --test-threads=1` runs on Linux, Windows, **and** macOS. This is the
  *sole* empirical proof of ADR-0001's alloc-free producer hot path
  (`tests/rt_alloc.rs` installs a process-wide counting `#[global_allocator]`,
  hence its own single-threaded binary). It is device-free, so it
  **hard-fails** on every platform — an allocation regression now blocks
  the release rather than slipping through the old `--lib`-only matrix.
- **`enumeration_matrix` runs per-OS (TC-02).** `cargo test --test
  enumeration_matrix` runs on all three platforms. It encodes the
  honest-failure enumeration contract (non-empty-or-classified-error) and
  gracefully skips the hardware assertions on a headless runner, so it is
  safe to gate everywhere.
- **Module-DAG reverse-edge guard (DAG-004).**
  `scripts/check-module-dag.sh` runs in CI and enforces the
  `core → bridge → audio → api (→ sink)` layering: it fails the build on
  any **new** upward edge (e.g. a fresh `core → audio` reference). It is
  allowlist-based — the known, documented `core/introspection.rs → audio`
  deviation (critique DAG-001/DAG-002, tracked in
  `docs/ARCHITECTURE.md` §1) is recorded as an explicit per-symbol
  exception, so the guard passes today but catches regressions.
- **Windows unit job hard-fails (TC-07).** The Windows `cargo test --lib`
  step is no longer `continue-on-error` on the whole suite. It now
  **partitions**: the platform-independent + non-audio tests hard-fail,
  and only the device-touching `create_audio_client*` subset (which needs
  a real WASAPI client, absent on Blacksmith Windows runners) is `--skip`ped
  into a separate tolerated step. `release.yml`'s `verify` job mirrors this
  exact partition.
- **macOS process-tap tests stay bounded (TC-03).** The unguarded
  CoreAudio process-tap tests are not executed (TCC grants are unavailable
  on Blacksmith macOS); instead CI asserts they are *wired up* via a
  `--list`/`--list --ignored` existence check, and the integration steps
  carry `timeout-minutes` caps so a stuck FFI test can't hang the job for
  10+ minutes.

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

Do **not** hand-edit the version anymore — drive it through
`scripts/bump-version.sh`, the same script the automated **Release
Prepare** workflow runs. It takes an **explicit** `X.Y.Z` (it does *not*
compute minor/patch — that arithmetic lives in `release-prepare.yml`) and
rewrites all five lockstep manifests plus rotates the CHANGELOG:

```bash
# Preview the edits without writing them:
bash scripts/bump-version.sh 0.3.0 --dry-run

# Apply them:
bash scripts/bump-version.sh 0.3.0
```

This rewrites, in one shot:

- `Cargo.toml` (root `rsac` crate)
- `bindings/rsac-napi/Cargo.toml` and `bindings/rsac-napi/package.json`
- `bindings/rsac-python/Cargo.toml` and `bindings/rsac-python/pyproject.toml`
- `CHANGELOG.md` — `## [Unreleased]` → `## [X.Y.Z] - <UTC date>` with a
  fresh `Unreleased` scaffold

Then reconcile the **sixth** manifest the script does not touch —
`bindings/rsac-ffi/Cargo.toml` — to the same `X.Y.Z` by hand (see
§"Versioning & ABI contract" (b)). The `version-lockstep` CI job checks
all six agree and hard-fails on a tag if any disagree.

Commit (use a normal subject for a manual/major release; reserve the
`release: vX.Y.Z` subject for the automated PR, since `release-tag.yml`
auto-tags any `release:` squash commit):

```bash
git add -A
git commit -m "chore: release 0.3.0"
```

Push and ensure CI — including `version-lockstep` — is green on the bump
commit before proceeding.

---

## 4. Tag the release

> For a `minor`/`patch` release prepared via the automated flow you do
> **not** run this step — `release-tag.yml` creates and pushes the
> annotated `vX.Y.Z` tag automatically once the `release: vX.Y.Z` PR is
> squash-merged (it is idempotent and won't double-tag). This manual step
> is for a **MAJOR** release or any time you bumped by hand in §3.

Annotated tags only — do not use lightweight tags. The tag name is
`v<semver>`; pushing it is the event all three publish workflows key on.

```bash
git tag -a v0.2.0 -m "rsac 0.2.0"
git push origin v0.2.0
```

If `release-tag.yml` already pushed the tag for you, this is a no-op (it
refuses to re-tag an existing `vX.Y.Z`); only run it by hand when the tag
does not yet exist.

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

## Post-Publish Verification (rsac#16)

After `cargo publish` returns and crates.io has had a minute or two to
propagate, run the one-command spot-check:

```bash
bash scripts/verify-docs-rs.sh              # uses version from Cargo.toml
bash scripts/verify-docs-rs.sh 0.2.0        # pin a specific version
```

The script is a focused probe — not a smoketest — and hits a handful of
`docs.rs` URLs to confirm the automated rustdoc build succeeded and the
rendered HTML contains the items rsac advertises.

What it checks:

1. `https://docs.rs/crate/rsac/<version>/builds.json` — parsed as JSON
   (via `jq` if available, else a portable `grep`), build status must
   report `success` or `succeeded`.
2. `https://docs.rs/rsac/<version>/rsac/` — HTTP 200 on the landing page.
3. The landing page HTML contains the names of core public items
   (`PlatformCapabilities`, `CaptureTarget`, `AudioDevice`) plus at
   least one feature-gated symbol (`feat_macos`) — the latter verifies
   docs.rs rendered with `all-features` (or that
   `[package.metadata.docs.rs]` was set correctly).
4. A few representative intra-doc link targets resolve (HTTP 200):
   `struct.AudioCaptureBuilder.html`, `struct.PlatformCapabilities.html`,
   `enum.CaptureTarget.html`, `struct.AudioDevice.html`. Loop 23 A1
   fixed four broken intra-doc links — this catches a regression.

Exits 0 on green with a human-readable summary. Exits non-zero on any
failed probe and prints the failing URLs, plus the `Cargo.toml` snippet
to add if the build itself failed:

```toml
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

Portable shell — BSD + GNU grep/curl, `shellcheck` clean. The script
reads its own default version from the root `Cargo.toml` so it needs no
arguments during a standard release.

### GitHub Actions secrets path

Before the first tag push, the publisher must add three repository
secrets via **Settings → Secrets and variables → Actions → New
repository secret**:

| Secret name              | Registry   | Token source |
|--------------------------|------------|--------------|
| `CARGO_REGISTRY_TOKEN`   | crates.io  | <https://crates.io/me> → API Tokens (scope `publish-update`, plus `publish-new` on first publish) |
| `NPM_TOKEN`              | npmjs      | <https://www.npmjs.com/settings/~/tokens> → new **Automation** token with publish rights on the `@rsac` scope |
| `MATURIN_PYPI_TOKEN`     | PyPI       | <https://pypi.org/manage/account/token/> → project-scoped token for `rsac` |

Each secret feeds exactly one `publish-*` job. A missing secret fails
that registry's workflow only — the other two proceed independently.

### Test-first-before-production flow

The three release workflows key on the `v*.*.*` tag pattern and
explicitly **exclude `v*-*`**, so pre-release tag shapes never publish.
Use that fact plus the `workflow_dispatch` dry-run to rehearse without
risking a real upload:

1. **Tag a release candidate.** `vX.Y.Z-rc.1` does not fire any publish
   workflow — it only exists for internal sharing / CI sanity.
   ```bash
   git tag -a v0.2.0-rc.1 -m "rsac 0.2.0 release candidate 1"
   git push origin v0.2.0-rc.1
   ```
2. **Dry-run the crates.io flow.** Go to **Actions → Release → Run
   workflow**, pick the RC branch or `master`, and leave
   `dry_run` checked (default `true`). `verify` + `publish` run end to
   end but `publish` stops at `cargo publish --dry-run` and never
   uploads. The npm and PyPI flows do not expose `workflow_dispatch`
   (no analogue to `--dry-run` exists for wheels/napi artifacts), so
   for those rely on the tag-exclude guard plus per-PR CI.
3. **Promote the RC to a real release.** Once the dry-run is green,
   tag the stable version — this push is the real trigger:
   ```bash
   git tag -a v0.2.0 -m "rsac 0.2.0"
   git push origin v0.2.0
   ```
   All three registry workflows fire in parallel against the same tag.
4. **Immediately after `publish` reports success**, run the
   verification script:
   ```bash
   bash scripts/verify-docs-rs.sh 0.2.0
   ```
   docs.rs usually starts the rustdoc build within a minute or two of
   the crates.io upload; if the script fails with `build_status not
   success`, re-run it in a few minutes before treating it as a real
   regression.

---

## Gaps / manual steps summary

Tracked here so follow-up release-automation tasks can pick them up:

- **Minor/patch releases are now fully automated** via the
  release-please-style two-workflow flow (`release-prepare.yml` +
  `release-tag.yml`) — see §"Automated minor/patch release (release-please
  style)". Run **Release Prepare** from the Actions tab, squash-merge the
  `release: vX.Y.Z` PR (keeping that exact subject), and the tag is pushed
  automatically. **MAJOR bumps stay manual** (the automation refuses to
  change the major): use `scripts/bump-version.sh` + a hand-pushed tag per
  §3–§4. The manual §1–§9 flow remains the fallback whenever the
  automation is unavailable or a maintainer needs to override it.
- All three registry workflows exist (`release.yml`, `release-npm.yml`,
  `release-pypi.yml`). Before the first tag push, the corresponding
  secrets (`CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`, `MATURIN_PYPI_TOKEN`)
  must be set under **Settings → Secrets and variables → Actions**.
  Missing secrets fail only the affected `publish-*` job; the other
  flows continue. Fall back to §2–§6 for the affected registry.
- `scripts/bump-version.sh X.Y.Z` rewrites the root `Cargo.toml`,
  `bindings/rsac-napi/{Cargo.toml,package.json}`, and
  `bindings/rsac-python/{Cargo.toml,pyproject.toml}` and rotates the
  CHANGELOG. It does **not** yet touch `bindings/rsac-ffi/Cargo.toml`
  (bring it to the target version by hand in the same commit) and cannot
  tag `rsac-go` (push `bindings/rsac-go/vX.Y.Z` separately — see
  §"Versioning & ABI contract" (c)). The `version-lockstep` CI job in
  `ci.yml` catches any manifest that drifts: it warns on push/PR and
  hard-fails on a release tag.
- `release-npm.yml` builds five napi-rs triples; other targets
  (`linux-x64-musl`, `linux-arm-gnueabihf`, FreeBSD, Android) are not
  wired. Add them to the matrix if downstream users request them.
- `release-pypi.yml` relies on `manylinux: auto` — wheels are
  `manylinux2014`. Consuming environments on glibc < 2.17 must install
  from sdist.

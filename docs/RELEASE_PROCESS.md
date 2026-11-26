# Release Process

This document describes the end-to-end procedure for cutting a new release of
the `rsac` crate and (where applicable) its language bindings. It is written
so a maintainer without prior release context can follow it top-to-bottom.

The worked example throughout is the **0.2.0** release. Substitute your
target version where appropriate.

---

## Automated Release Flow

Start with [Using the automated flow](#using-the-automated-flow) to prepare
a stable release and create its tags and GitHub Release. Registry publishing
is a separate operation; see [Registry publishing after tagging](#registry-publishing-after-tagging).
Immediately before the first upload, each registry publish job checks that
the remote release tag still resolves to the commit being published.

All three workflows now authenticate via **registry Trusted Publishing
(OIDC)** — no long-lived registry secrets. Each publish job requests
`id-token: write` and the registry accepts a short-lived, workflow-scoped
token. A one-time Trusted Publisher must be configured per registry (see
[§ Trusted Publishing setup (OIDC)](#trusted-publishing-setup-oidc)).

| Workflow | Registry | Matrix | Key jobs | Auth |
|---|---|---|---|---|
| `.github/workflows/release.yml` | crates.io | linux/win/mac | `verify` → `publish` → `github-release` | Trusted Publishing / OIDC (`rust-lang/crates-io-auth-action`) |
| `.github/workflows/release-npm.yml` | npm (`@rsac/audio`) | 8 napi-rs targets (5 required + 3 best-effort) | `verify-napi-build` (×8) → `publish-npm` | Trusted Publishing / OIDC (npm CLI ≥ 11.5.1 auto-detect) |
| `.github/workflows/release-pypi.yml` | PyPI (`rsac`) | 4 abi3 wheels (linux x86_64 + aarch64, macOS universal2, windows x64) + sdist | `build-wheels` (×4) + `build-sdist` → `publish-pypi` | Trusted Publishing / OIDC (`pypa/gh-action-pypi-publish`) |

### `release.yml` (crates.io)

1. **`verify`** — matrix of `blacksmith-4vcpu-ubuntu-2404`,
   `blacksmith-4vcpu-windows-2025`, and `blacksmith-6vcpu-macos-15`, each
   building all targets and running `cargo test --lib` against its platform
   feature **plus `compose`** (`feat_<os>,compose`). Mirrors the `test-*`
   jobs in `ci.yml`, including the Windows "no audio endpoint" handling:
   the Windows `--lib` suite is **partitioned**, not blanket-tolerated —
   the platform-independent + non-audio tests hard-fail, and only the
   device-touching WASAPI subset (which needs a real audio endpoint, absent
   on the Blacksmith Windows unit-test legs) is `--skip`ped into a separate
   `continue-on-error` step. See
   [§ What `ci.yml` now gates](#what-ciyml-now-gates-architecture-critique-closures)
   for the same partition on the CI side.
2. **`semver-checks`** — runs alongside `verify` on Linux, Windows and
   macOS. Each leg installs `cargo-semver-checks` and diffs the public API of the tagged
   commit against the previous stable release tag; skips with a warning on
   the first release (no baseline). See
   [§ Semver gate](#semver-gate-cargo-semver-checks) for the override
   procedure.
3. **`publish`** — depends on `verify` **and** `semver-checks`; single
   Linux runner first runs a version guard (on a tag push the tag must
   match `Cargo.toml`'s `[package].version`; on a `workflow_dispatch`
   with `dry_run=false` the `version` input is required and must match —
   dry runs may omit it), then runs the **full cross-manifest lockstep
   gate** (`scripts/check-version-lockstep.sh` — the same script the
   `version-lockstep` CI job uses; it hard-fails if any lockstep manifest
   or the rsac-ffi internal dep pin diverges) **before** any upload path,
   records build provenance (toolchain + OS package versions), then
   executes `cargo publish --locked --dry-run` and `cargo publish
   --locked` (`--locked` resolves against the committed `Cargo.lock` for a
   reproducible publish). Authentication is **crates.io Trusted Publishing
   (OIDC)** — the job requests `id-token: write` and exchanges the GitHub
   OIDC token for a short-lived crates.io API token via
   `rust-lang/crates-io-auth-action` (auto-revoked when the job ends); the
   previous `CARGO_REGISTRY_TOKEN` repo secret is no longer used. After the
   publish it generates a **CycloneDX SBOM** and uploads it plus the
   build-info file as a release artifact (both strictly advisory — SBOM
   tool failure warns, never fails the release).
4. **`github-release`** — depends on `publish`; extracts the CHANGELOG
   section matching the tag version and publishes a GitHub Release via
   `softprops/action-gh-release@v2`.

### Semver gate (`cargo-semver-checks`)

`release.yml`'s `semver-checks` job is a pre-publish API-compatibility
gate. On a full-history checkout (`fetch-depth: 0`) it resolves the
previous stable release tag —
`git describe --tags --abbrev=0 --match 'v*.*.*' --exclude 'v*-*' HEAD^`
— and runs:

```bash
cargo semver-checks check-release -p rsac --baseline-rev vPREV
```

- **What it catches:** any public-API change the version bump does not
  permit — e.g. removing/renaming a public item or changing a function
  signature on a minor/patch bump. Pre-1.0 the cargo convention applies:
  breaking changes require bumping the minor component
  (`0.x` → `0.(x+1)`).
- **First release:** with no previous stable `v*.*.*` tag there is no
  baseline; the job emits a `::warning::` and skips instead of failing.
- **Scope:** only the root `rsac` crate is checked (`-p rsac`) — this
  workflow publishes only that crate; the bindings ship via
  `release-npm.yml` / `release-pypi.yml`.

**Override procedure.** There is no skip input, and deleting or
commenting out the gate is not an accepted override. If the gate fails a
release, first follow the failed-`verify` procedure in
[Using the automated flow](#using-the-automated-flow) before changing any tag.

1. **Unintentional API change** — revert the offending change. Use the
   publication check's verdict to decide whether the version can be reused.
2. **Intentional API change** — the release is mis-versioned. Re-run
   the version bump (§3) with the **appropriate semver component** for
   the reported change category (breaking → next MAJOR,
   i.e. `0.x` → `0.(x+1)` pre-1.0, matching the ABI policy in
   §"Versioning & ABI contract"), update the CHANGELOG, and tag the new
   version.
3. **Tool false positive** (rare) — report it upstream to
   cargo-semver-checks, then take path 2 anyway: shipping under a bigger
   version bump is always semver-safe, while skipping the gate is not.

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
   does not block this job). Runs the **full cross-manifest lockstep
   gate** (`scripts/check-version-lockstep.sh`) before any upload,
   **upgrades the system npm to ≥ 11.5.1** (Node 20 ships npm 10.x, which
   predates OIDC trusted publishing), downloads every available `.node`
   into `bindings/rsac-napi/artifacts/`, runs `bunx @napi-rs/cli artifacts
   --dir artifacts` to move them into place, then `bunx @napi-rs/cli
   prepublish -t npm --skip-gh-release` — which itself **publishes** the
   per-platform sub-packages (`@rsac/audio-darwin-arm64`, etc.) to npm by
   shelling out to the (upgraded) system `npm` — and finally `npm publish
   --access public --ignore-scripts` for the main package
   (`--ignore-scripts` because package.json's `prepublishOnly` hook would
   otherwise re-run `napi prepublish` and double-publish the
   sub-packages). Authentication is **npm Trusted Publishing (OIDC)**: the
   job requests `id-token: write` and the npm CLI auto-detects the OIDC
   environment, authenticates with a short-lived token, and generates
   provenance automatically (so the explicit `--provenance` flag is no
   longer needed). No `NPM_TOKEN` secret. **Every** package published —
   the main `@rsac/audio` **and each** `@rsac/audio-<platform>`
   sub-package — must have its own Trusted Publisher configured on
   npmjs.com (see [§ Trusted Publishing setup](#trusted-publishing-setup-oidc)).

### `release-pypi.yml` (PyPI)

1. **`build-wheels`** — matrix of **four abi3 wheels**, one per
   (platform, arch):

   | Runner | `target` | Wheel |
   |---|---|---|
   | `blacksmith-4vcpu-ubuntu-2404` | `x86_64` | manylinux x86_64 (host-smoked) |
   | `blacksmith-4vcpu-ubuntu-2404` | `aarch64` | manylinux aarch64 (cross-built in the manylinux container; not host-smoked) |
   | `blacksmith-6vcpu-macos-15` | `universal2` | macOS x86_64 + arm64 |
   | `blacksmith-4vcpu-windows-2025` | `x64` | Windows x86_64 |

   The crate builds against the CPython **stable ABI** (pyo3
   `abi3-py39`), so a single `cp39-abi3` wheel per platform covers
   CPython 3.9–3.13 — there is **no per-interpreter matrix dimension**.
   Each job uses `PyO3/maturin-action@v1` (SHA-pinned) with
   `command: build`, an explicit `target:`, `manylinux: auto`,
   `--interpreter 3.9` (the abi3 floor), and
   `--manifest-path bindings/rsac-python/Cargo.toml`; a
   `before-script-linux` installs PipeWire + clang/llvm inside the
   manylinux container so the Linux wheels link `feat_linux`. Host-arch
   wheels are smoke-tested by installing the ONE built wheel into both
   Python 3.9 and 3.13 and importing it (proving abi3
   forward-compatibility). Each wheel is uploaded as an artifact.
2. **`build-sdist`** — single Linux job, `PyO3/maturin-action@v1` with
   `command: sdist`. Uploads the `.tar.gz`.
3. **`publish-pypi`** — depends on both of the above. Verifies the
   tag/requested version against `bindings/rsac-python/pyproject.toml`,
   downloads all artifacts into `dist-all/` with `merge-multiple: true`,
   then uploads via **`pypa/gh-action-pypi-publish`** with
   `skip-existing: true`. Authentication is **PyPI Trusted Publishing
   (OIDC)**: the job requests `id-token: write` and the action exchanges
   the short-lived OIDC token — **no long-lived PyPI secret exists or is
   used** (the previous `MATURIN_PYPI_TOKEN` flow is gone). A Trusted
   Publisher must be configured on the PyPI `rsac` project for this
   repo + `release-pypi.yml` (<https://docs.pypi.org/trusted-publishers/>).

   Known limitations:
   - `manylinux: auto` resolves to `manylinux2014` (glibc 2.17+) on
     x86_64, which is fine for Python 3.9+. If downstream users need
     older glibc, pin to `manylinux_2_17` explicitly.
   - macOS 15 runners produce wheels tagged with the Rust toolchain's
     default deployment target (macOS 11.0+ on `arm64`, 10.12+ on
     `x86_64`). Users on older macOS need to install from sdist.
   - The aarch64 Linux wheel is cross-built and cannot be import-smoked
     on the x86_64 runner; its first real load happens on a consumer's
     machine.

### One-time setup

All three registries now authenticate via **Trusted Publishing (OIDC)** —
there are **no long-lived registry secrets** to create. Instead, a
maintainer configures a Trusted Publisher on each registry's web UI once,
pointing it at this repository + the publishing workflow. The full
per-registry procedure is in
[§ Trusted Publishing setup (OIDC)](#trusted-publishing-setup-oidc).

Until a registry's Trusted Publisher is configured, that registry's
`publish-*` job fails (crates.io/npm reject the OIDC token exchange; PyPI
rejects the upload) — the other two flows continue independently. In that
state, fall back to the manual procedure below (§2–§6) for the affected
registry.

> **Migrating off the old token secrets.** The previous
> `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN` repo secrets (and the earlier
> `MATURIN_PYPI_TOKEN`) are no longer read by any workflow. After the first
> successful Trusted-Publishing release to each registry, delete the stale
> secrets from **Settings → Secrets and variables → Actions** so they
> cannot be misused.

### Trusted Publishing setup (OIDC)

One-time, web-UI-only configuration per registry. Each publisher binds a
registry package to this exact repository + workflow, so only a run of that
workflow in this repo can mint a publish token. No secret is stored in
GitHub. Owner: `Codeseys-Labs`; repository:
`rust-crossplat-audio-capture`.

**crates.io** (`rsac` crate) — replaces `CARGO_REGISTRY_TOKEN`.
1. Publish `rsac` once with a classic token if it does not yet exist
   (Trusted Publishing configures an *existing* crate).
2. On <https://crates.io> → the `rsac` crate → **Settings → Trusted
   Publishing → Add** a GitHub publisher: repository owner `Codeseys-Labs`,
   repository name `rust-crossplat-audio-capture`, workflow filename
   `release.yml`, environment blank.
3. `release.yml`'s `publish` job (`id-token: write`) then exchanges the
   OIDC token via `rust-lang/crates-io-auth-action` at publish time.

**npm** (`@rsac/audio` **and every** `@rsac/audio-<platform>` sub-package)
— replaces `NPM_TOKEN`.
1. Each package must exist on npm first (publish once with a token if new).
   The platform sub-packages are `@rsac/audio-darwin-x64`,
   `-darwin-arm64`, `-linux-x64-gnu`, `-linux-arm64-gnu`,
   `-win32-x64-msvc` (plus any best-effort ones that built:
   `-linux-x64-musl`, `-linux-arm64-musl`, `-linux-arm-gnueabihf`).
2. For **each** package on <https://www.npmjs.com> → package **Settings →
   Trusted publisher →** GitHub Actions: organization/owner
   `Codeseys-Labs`, repository `rust-crossplat-audio-capture`, workflow
   filename `release-npm.yml`, environment blank.
3. `release-npm.yml`'s `publish-npm` job (`id-token: write`) upgrades npm
   to ≥ 11.5.1, which auto-detects OIDC and publishes with automatic
   provenance.
   > Any sub-package **without** a Trusted Publisher will fail its publish
   > under OIDC. If you cannot configure all sub-packages at once, keep a
   > temporary `NPM_TOKEN` fallback for the missing ones (npm falls back to
   > a token when no OIDC publisher matches) and remove it once every
   > package has a publisher.

**PyPI** (`rsac` project) — already OIDC; no change needed.
1. On <https://pypi.org> → the `rsac` project → **Publishing → Add a new
   publisher** (GitHub): owner `Codeseys-Labs`, repository
   `rust-crossplat-audio-capture`, workflow `release-pypi.yml`, environment
   blank. For the very **first** publish use PyPI's *pending publisher*
   flow (<https://docs.pypi.org/trusted-publishers/>), which reserves the
   project name against this repo + workflow before the project exists.
2. `release-pypi.yml`'s `publish-pypi` job (`id-token: write`) uploads via
   `pypa/gh-action-pypi-publish` with no token input.

### Using the automated flow

**1. Prepare a pull request.** Start a branch from current `master`. Run
`bash scripts/bump-version.sh X.Y.Z`, review the manifests and dated
changelog, and refresh the workspace lockfile with `cargo update --workspace`.
Commit the bump and lockfile, push **the branch**, and open a PR into `master`.
For a minor or patch bump, `release-prepare.yml` can open the PR for you.
Review and squash-merge the PR; do not push the bump directly to `master`.
Record the merge commit's full SHA from the merged PR, not its branch head.

**2. Collect evidence on that SHA.** While `master` points at the merged
commit, dispatch all four workflows in the release repository:

```bash
repo=Codeseys-Labs/rust-crossplat-audio-capture
gh workflow run ci.yml --repo "$repo" --ref master -f full=true -f semver=true
gh workflow run ci-audio-tests.yml --repo "$repo" --ref master -f platform=all -f release_evidence=true
gh workflow run ci-android-emu.yml --repo "$repo" --ref master -f release_evidence=true
gh workflow run ci-ios-sim.yml --repo "$repo" --ref master -f release_evidence=true
```

Wait for each run and check its commit against the SHA from step 1, for
example with `gh run view RUN_ID --repo "$repo" --json headSha`. In order,
the required jobs are **Release Readiness**,
**Audio Release Evidence**, **Android Release Evidence** and **iOS Release
Evidence**. Each must succeed in a completed, successful manual run of its
own workflow on the exact release SHA. A green PR run is not evidence for
the squash commit. Later advances of `master` do not invalidate existing runs.

If `master` moved before a dispatch, create and push a branch at the recorded
merge SHA. Dispatch the missing evidence workflows with `--ref` pointing at
that branch and the same inputs as above. Record their run IDs for step 3.

**3. Dispatch the tag workflow on `master`.** Set the shell variable `sha`
to the full 40-character lower-case SHA recorded in step 1 and substitute
the stable version for `X.Y.Z`:

```bash
gh workflow run release-tag.yml --repo "$repo" --ref master -f version=X.Y.Z -f sha="$sha"
```

If you collected evidence on another branch, add the `run_ids` input to
that command, for example `-f run_ids='ci=123 audio=124 android-emu=125 ios-sim=126'`
(substitute the actual IDs; supply only the keys that need them). Dispatch
**`release-tag.yml` itself on `master`**, regardless of where evidence ran.

`release-tag.yml` runs its own revision's `scripts/tag-release.sh` from the
requested commit's checkout. It sets `RELEASE_GATE_DIR` to the separate,
trusted workflow checkout: the lockstep script and workflow names come from
there, but the manifests and other release evidence belong to the commit
being tagged. Locally, leaving `RELEASE_GATE_DIR` unset or empty reads the
gate files from the current checkout instead.

`RELEASE_REPO=owner/name` cannot override a different recognized GitHub
fetch or push target. For a test remote whose fetch or push URL cannot be
parsed, both `RELEASE_REPO` and `RELEASE_ALLOW_UNPARSED_PUSH_URL=1` must
be set. The gate requires one push URL and a clean tracked tree. It fetches
only the branch (no tags), checks `HEAD` is on that branch's history, and
rejects tags that already exist locally or on the named remote. If the test
seam points pushes elsewhere, a tag present only at that destination is not
seen in dry-run. A real atomic push rejects the conflict (exit 17): neither
remote tag changes, and the script removes the local tags it created.
It verifies manifest lockstep, the committed lockfile (`cargo metadata --locked`),
the dated changelog heading, and the Go module path for major versions ≥2
(see [the Go tag convention](#c-rsac-go-tag-convention)).

The release commit must contain `docs/evidence/windows-default-switch.md`;
a working-tree copy is not evidence. The committed note must have exactly one
`Tested-SHA: <40 lower-case hex digits>` line naming the tested ancestor.
Since that test, the API and core `recovery.rs` files,
`src/audio/windows/`, and `tests/ci_audio/hotplug_{common,windows}.rs` must
be unchanged. If not, rerun the workstation test and update the note.

Only `android-emu`, `ios-sim` and `windows-switch` can be waived. Give the
workflow a space-separated `waive` input, e.g. `-f waive='ios-sim=123'`;
each issue must be **open** in the release repository and carry the
`release-waiver` label. `ci` and `audio` have no waiver. The gate reports all
five results together. On success it pushes annotated `vX.Y.Z` and
`bindings/rsac-go/vX.Y.Z` atomically; their messages record the run IDs,
workstation SHA and any waiver issue numbers. The workflow creates a GitHub
Release from the changelog section and those evidence and waiver lines.
Set `dry_run=true` to preview the tag messages without tagging or releasing.

The workflow pushes with `GITHUB_TOKEN`, which does **not** start the
tag-triggered workflows. For emergencies, `bash scripts/tag-release.sh X.Y.Z`
from the release checkout runs the same gate. That local command accepts
`--dry-run`, `--no-go-tag`, repeatable `--waive key=issue`, and repeatable
`--run-id key=N` (`--run-id N` selects `ci`). Do not specify both a run ID and
a waiver for the same key. By default the gate inspects at most the 20 newest
listed runs per workflow. An explicit ID can select a dispatch on another
ref at this SHA. A tag pushed by a person **does** start
the tag-triggered workflows, including the registry publish jobs.

**If `verify` fails**, leave the tag in place. Wait for all publish workflows
for this version to finish, or cancel them and confirm they have stopped.
Check manual dispatches in Actions too: the script below sees only the latest
20 tag-push runs per workflow and the current remote Go tag.
Then run `bash scripts/check-release-published.sh X.Y.Z`. It checks the remote
Go tag, in-flight runs, crates.io, PyPI and npm (including every platform
package). Use its final line to decide:

- `safe-to-retag X.Y.Z` (exit 0): delete `vX.Y.Z` locally and remotely, fix
  the issue and follow the sequence above for the corrected commit. Just
  before re-tagging, run `bash scripts/check-release-published.sh X.Y.Z`
  again. Re-tag only if it still says `safe-to-retag X.Y.Z`.
- `published X.Y.Z` (exit 1): leave the tag untouched and ship a new version.
  A pushed Go tag counts as published because a Go proxy can retain it.
- `indeterminate X.Y.Z` (exit 3): treat as published. Do not move the tag.

GitHub can re-run a finished tag-triggered workflow from the same commit.
Do not start such a re-run for a tag that has been deleted or moved.

crates.io publishes are
irrevocable,
so `publish` is gated behind a successful `verify` + `semver-checks` — but a failure
inside `publish` (e.g. transient network, token rotation) is not safe
to re-run blindly if `cargo publish` already succeeded. Inspect
<https://crates.io/crates/rsac> before re-running.

See §6 for manual binding publish fallbacks.

---

## Registry publishing after tagging

The automated tag workflow does not start a registry publish
([#88](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/issues/88)).
When ready to publish, dispatch each registry workflow **at the release tag**:

Replace `X.Y.Z` below with the release version. For a real publish:

```sh
gh workflow run release.yml --ref vX.Y.Z -f version=X.Y.Z -f dry_run=false
gh workflow run release-npm.yml --ref vX.Y.Z -f version=X.Y.Z -f publish=true
gh workflow run release-pypi.yml --ref vX.Y.Z -f version=X.Y.Z -f publish=true
```

For a rehearsal without a registry upload:

```sh
gh workflow run release.yml --ref vX.Y.Z -f version=X.Y.Z -f dry_run=true
gh workflow run release-npm.yml --ref vX.Y.Z -f version=X.Y.Z -f publish=false
gh workflow run release-pypi.yml --ref vX.Y.Z -f version=X.Y.Z -f publish=false
```

Real publishes use Trusted Publishing (OIDC). Configure the crates.io and
PyPI publishers and the npm publisher for `@rsac/audio` **and** every
platform sub-package; see [Trusted Publishing setup](#trusted-publishing-setup-oidc).
The npm rehearsal builds and smokes artifacts; the PyPI rehearsal builds and
smokes wheels and the source distribution.

If a person pushed the release tags, monitor the resulting publish runs
instead of starting a second set.

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
| `mobile/android-native/Cargo.toml` | `[package].version` | Android `librsac.so` shim packaged into the AAR |

Run `bash scripts/bump-version.sh X.Y.Z` to rewrite **all nine** manifests
in one shot — including `bindings/rsac-ffi/Cargo.toml` and its internal
`rsac = { path = "../../", version = "…" }` dependency pin — plus the
CHANGELOG rotation. For the pre-tag gate on the merged release commit, see
[Using the automated flow](#using-the-automated-flow). On a registry
publish, `release.yml` independently verifies the version before uploading.

> Mid-cycle skew is tolerated by CI (warning only) so a binding can lag
> the root crate between releases, but it must be reconciled before
> tagging. As of this writing all nine lockstep manifests agree at `0.4.1`
> (`bump-version.sh` has kept them in lockstep since it grew the
> rsac-ffi rewrite).

### (b) C ABI changes are MAJOR for `rsac-ffi`

The `rsac-ffi` crate exposes a C ABI: the exported `extern "C"` symbols
and the C headers under `bindings/rsac-ffi/include/` — `rsac.h` is the
**curated** header consumers include; `rsac_generated.h` is the raw
cbindgen output that CI's header-drift check compares it against. **Any**
of the following is a
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
github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go`,
see `bindings/rsac-go/go.mod`) and
carries **no in-manifest version** — Go derives versions from git tags.
Because the module lives in a subdirectory of this repository, two
things must line up for `go get …@vX.Y.Z` to resolve: the **module path
is the repository path plus the subdirectory** (as above — a short
vanity path like `github.com/Codeseys-Labs/rsac-go` would need a
separate mirror repo and can never resolve from tags on this one), and
its releases are tagged with the **subdirectory-prefixed** form Go's
module proxy expects:

```
bindings/rsac-go/vX.Y.Z
```

This tag ships in lockstep with the `vX.Y.Z` crate tag (same `X.Y.Z`).
The [release gate](#using-the-automated-flow) pushes both tags together.
For `2.0.0` and later, the Go module path in `bindings/rsac-go/go.mod` must
end in `/vN`, where `N` is the release major (for example, `/v2` for
`2.0.0`). If the module has not made that change, use `--no-go-tag`.

Consumers use `go get <module-path>@vX.Y.Z` with the `module` path from
the tagged `go.mod`, including the major suffix when applicable. The `version-lockstep`
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

A real dispatch (`dry_run=false`) requires a `version` matching
`Cargo.toml`'s `[package].version` and a release tag pointing to the running
commit. Select the published tag, not a moving branch. Dry runs may omit
`version`.

`release-npm.yml` and `release-pypi.yml` also accept manual
`workflow_dispatch` runs. They are safe-by-default: `publish` defaults to
false, so the manual run builds/smokes artifacts but skips registry upload.
To publish manually, set `publish: true` and provide the expected `X.Y.Z`
version; the publish job verifies that value against the binding manifest
before uploading.

### Promoting an RC to a stable release

Follow [Using the automated flow](#using-the-automated-flow). If the RC's
stable manifests, dated changelog and updated lockfile have already landed
through a PR, use the merged SHA and start at the evidence dispatches.
The `release.yml` packaging dry-run is not release evidence.

---

## 1. Prerequisites

Before you start a release, confirm all of the following:

- **Repository access** to open and merge a release PR and dispatch workflows.
  Only the local emergency path requires permission to push tags yourself.
- **crates.io API token** exported in your shell — **only for a manual,
  local `cargo publish`** (the CI flow uses Trusted Publishing / OIDC and
  needs no token; OIDC is not available to a local shell):
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

> Use [Using the automated flow](#using-the-automated-flow) for all stable
> releases. The sections below cover preparation, registry fallbacks and
> post-publish verification.

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

For the required post-merge evidence see
[Using the automated flow](#using-the-automated-flow).
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
rewrites all nine lockstep manifests plus rotates the CHANGELOG:

```bash
# Preview the edits without writing them:
bash scripts/bump-version.sh 0.2.0 --dry-run    # or: mise run release:bump -- 0.2.0 --dry-run

# Apply them:
bash scripts/bump-version.sh 0.2.0              # or: mise run release:bump -- 0.2.0
```

This rewrites, in one shot:

- `Cargo.toml` (root `rsac` crate)
- `bindings/rsac-ffi/Cargo.toml` — both its `[package].version` and its
  internal `rsac = { path = "../../", version = "…" }` dependency pin
- `bindings/rsac-napi/Cargo.toml` and `bindings/rsac-napi/package.json`
- `bindings/rsac-python/Cargo.toml` and `bindings/rsac-python/pyproject.toml`
- `mobile/android-native/Cargo.toml`
- `CHANGELOG.md` — `## [Unreleased]` → `## [X.Y.Z] - <UTC date>` with a
  fresh `Unreleased` scaffold

That covers all nine lockstep manifests — there is no manual reconcile
step left. The `version-lockstep` CI job checks
they all agree and hard-fails on a tag if any disagree.

After reviewing the bump, continue with the lockfile and release PR in
[Using the automated flow](#using-the-automated-flow).

---

## 4. Tag the release

Use the tag workflow in [Using the automated flow](#using-the-automated-flow)
for the merged release commit (in this example, version `0.2.0`).

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

For the registry workflows, see
[Registry publishing after tagging](#registry-publishing-after-tagging).

Before tagging, bump the binding manifests in lockstep with the root
`Cargo.toml` — `bash scripts/bump-version.sh X.Y.Z` does this for you
(all nine manifests, including `bindings/rsac-napi/package.json`,
`bindings/rsac-python/pyproject.toml`, and `mobile/android-native/Cargo.toml`;
see §3).

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
# --ignore-scripts: package.json's prepublishOnly hook re-runs
# `napi prepublish`, which would double-publish the sub-packages the
# line above already pushed (E403). Same reasoning as release-npm.yml.
NODE_AUTH_TOKEN=<npm-token> bunx npm publish --access public --ignore-scripts
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

`maturin upload` needs a credential: CI uses Trusted Publishing (OIDC),
which is not available to a local shell, so create a **personal** PyPI
API token (scoped to the `rsac` project) and export it as
`MATURIN_PYPI_TOKEN` (or configure `~/.pypirc`) for the manual upload
only — no such repo secret exists in CI. `--skip-existing` is safe to
re-run if a partial upload landed before the failure.

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
4. **GitHub Release.** Check that the release points at `v0.2.0` and its notes
   include the version's changelog and the tag's evidence and waiver lines.

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
# (or: mise run release:verify-docs [-- X.Y.Z])
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

### GitHub Actions Trusted Publishing path

There are **no repository secrets to add** — all three registries
authenticate via Trusted Publishing (OIDC). Before the first tag push the
publisher configures a Trusted Publisher on each registry's web UI,
pointing it at this repo + the publishing workflow:

| Registry | Configure | Where |
|----------|-----------|-------|
| crates.io | Trusted Publisher → `release.yml` | <https://crates.io> → `rsac` crate → Settings → Trusted Publishing |
| npmjs     | Trusted Publisher → `release-npm.yml` on the main package **and every** `@rsac/audio-<platform>` sub-package | <https://www.npmjs.com> → package Settings → Trusted publisher |
| PyPI      | Trusted Publisher → `release-pypi.yml` | <https://pypi.org> → `rsac` project → Publishing (pending-publisher flow for the first upload) |

Full step-by-step: [§ Trusted Publishing setup (OIDC)](#trusted-publishing-setup-oidc).
Each publisher scopes exactly one `publish-*` job. A missing publisher
fails that registry's workflow only — the other two proceed independently.

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
   uploads. For npm/PyPI, run **Release npm** / **Release PyPI** with
   `publish` left false to build/smoke their artifacts without uploading.
3. **Promote the RC to a real release.** Follow
   [Using the automated flow](#using-the-automated-flow); see
   [Promoting an RC](#promoting-an-rc-to-a-stable-release) if the version is
   already stable.
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

- [Using the automated flow](#using-the-automated-flow) ends with a GitHub
  Release. See [Registry publishing after tagging](#registry-publishing-after-tagging)
  before starting an upload.
- All three registry workflows exist (`release.yml`, `release-npm.yml`,
  `release-pypi.yml`) and authenticate via **Trusted Publishing (OIDC)** —
  **no repo secrets**. Before the first tag push, configure a Trusted
  Publisher per registry on its web UI (crates.io crate Settings, npmjs
  package Settings for the main package **and every** platform sub-package,
  PyPI project Publishing) pointing at this repo + the publishing workflow
  — see §"Trusted Publishing setup (OIDC)". A missing publisher fails only
  the affected `publish-*` job; the other flows continue. Fall back to
  §2–§6 for the affected registry.
- `scripts/bump-version.sh X.Y.Z` rewrites all nine manifests — the root
  `Cargo.toml`, `bindings/rsac-ffi/Cargo.toml` (including its internal `rsac`
  dependency version pin), `bindings/rsac-napi/{Cargo.toml,package.json}`, and
  `bindings/rsac-python/{Cargo.toml,pyproject.toml}`, and
  `mobile/android-native/Cargo.toml` — and rotates the CHANGELOG. For the
  corresponding Go tag, see §"Versioning & ABI contract" (c). The `version-lockstep` CI job in
  `ci.yml` catches any manifest that drifts: it warns on a dispatched run and
  hard-fails on a release tag.
- `release-npm.yml` may omit a best-effort platform from a release. See the
  [npm build matrix](#release-npmyml-npm) for the required and best-effort targets.
- `release-pypi.yml` relies on `manylinux: auto` — wheels are
  `manylinux2014`. Consuming environments on glibc < 2.17 must install
  from sdist.

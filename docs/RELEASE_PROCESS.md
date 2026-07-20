# Release Process

This document describes the end-to-end procedure for cutting a new release of
the `rsac` crate and (where applicable) its language bindings. It is written
so a maintainer without prior release context can follow it top-to-bottom.

The worked example throughout is the **0.2.0** release. Substitute your
target version where appropriate.

---

## Automated Release Flow

Three registry workflows (`release.yml`, `release-npm.yml`, `release-pypi.yml`)
are *wired* to the same `v*.*.*` tag push and would run in parallel — each
publishing to one registry. **Whether they actually fire depends on HOW the tag
was pushed** (see the two paths below): a tag pushed with the default
`GITHUB_TOKEN` does **not** re-trigger them (GitHub's anti-recursion rule). The
automated path therefore pushes the tag with a **GitHub App installation
token** whenever the org secrets `RSAC_RELEASE_APP_ID` +
`RSAC_RELEASE_APP_PRIVATE_KEY` are configured — that push **does** trigger the
full fan-out. When the secrets are absent it gracefully falls back to
**GitHub-only** (tag + GitHub Release; registry publishes stay manual). A tag
pushed manually (local git, or a PAT) also triggers the full fan-out.

There are **two ways** that `vX.Y.Z` tag gets created, and they differ in
whether the publish fan-out fires:

1. **Automated, release-please style (recommended for minor/patch).** A
   maintainer runs the **Release Prepare** workflow from the Actions tab;
   it opens a `release: vX.Y.Z` PR (version bump + CHANGELOG rotation), a
   maintainer squash-merges it, and `release-tag.yml` then auto-creates and
   pushes the tag (plus the `bindings/rsac-go/vX.Y.Z` Go module tag). **With
   the release App secrets configured** the tag is pushed using an App
   installation token, so it **DOES** trigger the three registry workflows —
   full fan-out, no manual follow-up. **Without them** the tag is pushed with
   the default `GITHUB_TOKEN`, which GitHub's anti-recursion rule stops from
   triggering the registry workflows — the path degrades to **GitHub-only**
   (git tag + GitHub Release) and registry publishing is a manual follow-up
   (Step 4). See
   [§ Automated release (release-please style)](#automated-release-release-please-style).
2. **Manual tag push.** A maintainer bumps the manifests + CHANGELOG by
   hand (via `scripts/bump-version.sh`), commits, and pushes an annotated
   `vX.Y.Z` tag directly (local git or a PAT). This tag push is **not** from a
   workflow's `GITHUB_TOKEN`, so it **DOES** trigger the full registry fan-out
   below. It is also the **only** supported path for a **MAJOR** bump (the
   automated path refuses to change the major) and the fallback whenever the
   automation is unavailable. See §1–§9 below.

The three publish workflows below run when a tag is pushed by something OTHER
than a workflow's `GITHUB_TOKEN` (i.e. path 2, or path 1 with the release App
secrets configured) — they never trigger off anything but a `v*.*.*` tag push.

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
   jobs in `ci.yml`, including the Windows "no audio subsystem" handling:
   the Windows `--lib` suite is **partitioned**, not blanket-tolerated —
   the platform-independent + non-audio tests hard-fail, and only the
   device-touching WASAPI subset (which needs Audiosrv + a real endpoint,
   absent on Blacksmith Windows runners) is `--skip`ped into a separate
   `continue-on-error` step. See
   [§ What `ci.yml` now gates](#what-ciyml-now-gates-architecture-critique-closures)
   for the same partition on the CI side.
2. **`semver-checks`** — runs alongside `verify` on a single Linux runner;
   installs `cargo-semver-checks` and diffs the public API of the tagged
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
release:

1. **Unintentional API change** — delete the tag
   (`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`), revert the
   offending change, and re-tag.
2. **Intentional API change** — the release is mis-versioned. Delete the
   tag, re-run the version bump (§3) with the **appropriate semver
   component** for the reported change category (breaking → next MAJOR,
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

From a clean `master` with CI already green on the commit you intend to
release (see §2 for the pre-release checklist):

```bash
# Bump the version + promote CHANGELOG entries under a dated heading.
# scripts/bump-version.sh rewrites all nine lockstep manifests + rotates
# the CHANGELOG — see §2 "CHANGELOG promotion" and §3 "Version bump".
git add -A
git commit -m "rsac X.Y.Z"
git push origin master

# Tag and push — this is the workflow trigger.
git tag -a vX.Y.Z -m "rsac X.Y.Z"
git push origin vX.Y.Z
```

Watch the Actions tab. If `verify` fails, delete the tag locally and
remotely (`git tag -d vX.Y.Z && git push --delete origin vX.Y.Z`), fix
the underlying issue, and re-tag. crates.io publishes are irrevocable,
so `publish` is gated behind a successful `verify` + `semver-checks` — but a failure
inside `publish` (e.g. transient network, token rotation) is not safe
to re-run blindly if `cargo publish` already succeeded. Inspect
<https://crates.io/crates/rsac> before re-running.

Bindings are published by the companion workflows described above.
The same tag push triggers `release-npm.yml` and `release-pypi.yml`
in parallel with `release.yml`, so a single `git push --tags`
publishes to all three registries. See §6 for manual fallbacks if one
of the automated flows fails.

---

## Automated release (release-please style)

This is the **recommended** way to cut a `minor` or `patch` release. It
is a two-workflow, release-please-style flow: a maintainer kicks off a
*prepare* workflow that opens a reviewable release PR, and merging that
PR triggers a *tag* workflow that creates the `vX.Y.Z` tag, the
`bindings/rsac-go/vX.Y.Z` Go module tag, **and the GitHub Release**.
**No bot ever pushes to `master` directly** — every byte that ships is
reviewed in a normal PR first.

> ### ⚠️ Scope: registry fan-out needs the release App secrets
> Whether the flow ends at GitHub or fans out to the registries depends on
> the **release App secrets** (see
> [§ Release App setup](#release-app-setup-automatic-registry-fan-out)):
>
> - **Secrets configured** — `release-tag.yml` pushes the tag with a GitHub
>   App installation token. That push **does** trigger the
>   `on: push: tags:` publish workflows (`release.yml` / `release-npm.yml` /
>   `release-pypi.yml`), so crates.io / npm / PyPI publishing is automatic
>   and Step 4 becomes a verification step.
> - **Secrets absent (graceful fallback)** — the tag is pushed with the
>   default `GITHUB_TOKEN`, which GitHub's anti-recursion rule stops from
>   re-triggering the publish workflows. The flow produces the **git tag +
>   GitHub Release** only, the job summary explains what to configure, and
>   you **publish to the registries by hand** (see "Step 4" below).

```
 ┌──────────────────────────┐   human runs from Actions tab, picks bump=minor|patch
 │  Release Prepare          │   (.github/workflows/release-prepare.yml)
 │  (workflow_dispatch)      │
 └────────────┬─────────────┘
              │  computes next NON-major version, runs
              │  scripts/bump-version.sh, opens a PR
              ▼
 ┌──────────────────────────┐   human reviews + SQUASH-MERGES
 │  PR: "release: vX.Y.Z"    │   (CI version-lockstep gate must be green;
 └────────────┬─────────────┘    it now also runs at tag time)
              │  squash commit subject == "release: vX.Y.Z (#N)" lands on master
              ▼
 ┌──────────────────────────┐   detects the release commit, creates the
 │  Release Tag on Merge     │   annotated tags vX.Y.Z + bindings/rsac-go/vX.Y.Z
 │  (push: master)           │   AND a GitHub Release
 └────────────┬─────────────┘   (.github/workflows/release-tag.yml)
              │  GitHub Release published.
              │  App secrets set    → tag pushed with an App installation
              │                       token: release.yml / release-npm.yml /
              │                       release-pypi.yml fire automatically
              │                       (Step 4 = verify the runs).
              │  App secrets absent → GITHUB_TOKEN tag push does NOT trigger
              │                       them — do Step 4 manually.
              ▼
   Step 4: registry publishes (automatic with the App token; manual without)
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
   `TZ=UTC`), which rewrites **all nine** lockstep manifests
   (`Cargo.toml`, `bindings/rsac-ffi/Cargo.toml` — including its internal
   `rsac = { path = "../../", version = "…" }` dependency pin —
   `bindings/rsac-napi/{Cargo.toml,package.json}`,
   `bindings/rsac-python/{Cargo.toml,pyproject.toml}`, and
   `mobile/android-native/Cargo.toml`) and rotates
   `CHANGELOG.md` (`[Unreleased]` → `[X.Y.Z] - <UTC date>` plus a fresh
   `Unreleased` scaffold). The one thing the script does *not* handle is
   the `bindings/rsac-go/vX.Y.Z` tag — see §"Versioning & ABI contract"
   (c); on this automated path `release-tag.yml` pushes it for you.
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
green** on the PR before merging — it cross-checks that all **nine** manifests
(root `Cargo.toml`, the rsac-ffi / rsac-napi / rsac-python `Cargo.toml`s, the
napi `package.json`, the python `pyproject.toml`, and the Android native shim)
carry the same version.
Because this gate runs on the PR/master push that subsequently gets tagged, the
tagged commit is already lockstep-verified before `release-tag.yml` tags it. (At
*tag* time, `release.yml` additionally re-checks the tag matches the root
`Cargo.toml`, and `ci.yml` *also* re-runs on the `v*.*.*` tag push — its `on:`
block includes a `tags: ['v*.*.*', '!v*-*']` trigger — so the full nine-manifest
lockstep gate runs again at tag time as well as on push/PR.)

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
   "Release X.Y.Z"` on HEAD (the reviewed, lockstep-passing squash commit).
   The push credential is selected at runtime: a **GitHub App installation
   token** (minted via the SHA-pinned `actions/create-github-app-token`)
   when the `RSAC_RELEASE_APP_ID` + `RSAC_RELEASE_APP_PRIVATE_KEY` org
   secrets exist — that push re-triggers the registry publish workflows —
   or the default `GITHUB_TOKEN` otherwise (tag still created; publishes
   stay manual).
6. **Creates + pushes the Go module tag** — `bindings/rsac-go/vX.Y.Z`
   (same version, same selected credential), so Go consumers can
   `go get github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go@vX.Y.Z`
   — see §"Versioning &
   ABI contract" (c). Idempotent independently of the crate tag, so a
   rerun backfills a missed Go tag. No workflow triggers on this tag shape
   (every `tags:` filter in this repo is `v*.*.*`, and a single `*` in a
   tag glob never crosses `/`), so recursion is not a concern.
7. **Publishes the GitHub Release** — extracts the `## [X.Y.Z]` section
   from `CHANGELOG.md` as the release notes and creates the GitHub Release
   for the tag (via `softprops/action-gh-release`). The default
   `GITHUB_TOKEN` is allowed to create tags and releases.
8. **Reports the publish path** — with the App token, a `::notice::` plus
   a job summary confirming the registry publish workflows fired
   automatically; without it, a `::warning::` plus a job summary
   instructing the maintainer to run the registry publishes (Step 4) and
   documenting the App setup that would make them automatic.

This job is repo-guarded to
`Codeseys-Labs/rust-crossplat-audio-capture` (a fork that merges a
`release:` commit must never tag + release), serialised via a
`concurrency` group, and runs with least-privilege `permissions: contents:
write` (tag + release only). The version is verified at three points: the
merged PR's `version-lockstep` gate, the same gate re-running at tag time
(ci.yml now triggers on `v*.*.*` tags), and `release-tag.yml`'s own
manifest defense.

### Step 4 — registry publishes (automatic with the App token; manual fallback)

**With the release App secrets configured**, the tag push from Step 3
already triggered `release.yml`, `release-npm.yml`, and `release-pypi.yml`
— Step 4 is just verification: watch the three runs in the Actions tab and
confirm each registry publish succeeds.

**Without the secrets (graceful fallback)**, the automated flow stops at
the **git tag + GitHub Release**. To publish the crate and bindings to
crates.io / npm / PyPI, trigger the publish workflows by hand after the
GitHub Release appears:

- **crates.io** — Actions → **Release** (`release.yml`) → *Run workflow*
  (it has a `workflow_dispatch` with a `dry_run` toggle; set `dry_run:
  false` **and enter the expected `X.Y.Z` in the `version` input** to
  publish for real — the publish job refuses a real dispatch publish
  without a `version` that matches `Cargo.toml`). Authenticates via
  crates.io Trusted Publishing (OIDC) — requires the crates.io Trusted
  Publisher (see [§ Trusted Publishing setup](#trusted-publishing-setup-oidc)).
- **npm** (`@rsac/audio`) — Actions → **Release npm**
  (`release-npm.yml`) → *Run workflow*, enter `X.Y.Z`, and set `publish:
  true`. With `publish` left false, the workflow only builds/smokes the
  artifacts. Authenticates via npm Trusted Publishing (OIDC) — requires the
  npm Trusted Publisher on the main package **and** every platform
  sub-package.
- **PyPI** (`rsac`) — Actions → **Release PyPI** (`release-pypi.yml`) →
  *Run workflow*, enter `X.Y.Z`, and set `publish: true`. With `publish`
  left false, the workflow only builds/smokes wheels and sdist. Publishes
  via PyPI Trusted Publishing / OIDC.

**Why the fallback is manual:** a tag pushed with the default
`GITHUB_TOKEN` does not re-trigger `on: push: tags:` workflows (GitHub's
anti-recursion rule). Configuring the release App (next section) makes
`release-tag.yml` push the tag with an App installation token instead —
that token is not subject to the rule, so Step 4 then fires automatically.

### Release App setup (automatic registry fan-out)

One-time org-admin setup that upgrades Step 4 from manual to automatic:

1. **Create an org-owned GitHub App** (Organization Settings → Developer
   settings → GitHub Apps → New GitHub App). Only one permission is
   needed: **Repository permissions → Contents: Read and write** (what a
   tag push requires). No webhook, no user authorization.
2. **Install the App** on `Codeseys-Labs/rust-crossplat-audio-capture`
   (or org-wide).
3. **Generate a private key** for the App (App settings → Private keys)
   and note the App's numeric **App ID**.
4. **Add two org secrets** (Organization Settings → Secrets and variables
   → Actions), visible to this repository:

   | Secret | Value |
   |---|---|
   | `RSAC_RELEASE_APP_ID` | the App's numeric App ID |
   | `RSAC_RELEASE_APP_PRIVATE_KEY` | the App's PEM private key (full contents, including the `BEGIN`/`END` lines) |

`release-tag.yml` probes the pair at runtime (a step receives them via
`env` and tests emptiness — secrets are not readable in `if:` expressions
on every context) and mints an installation token with the SHA-pinned
`actions/create-github-app-token` only when both exist. Missing or partial
secrets are never an error: the workflow logs the fallback, pushes with
`GITHUB_TOKEN`, and the job summary spells out this exact setup.

### What the automated flow will NOT do (use the manual path instead)

- **MAJOR releases.** The `bump` input offers only `minor`/`patch`, the
  prepare job asserts the major is unchanged, and the tag job refuses a
  major-crossing release commit. A `X` → `X+1` (or pre-1.0 `0.x` →
  `0.(x+1)` per the ABI policy in §"Versioning & ABI contract") bump must
  be done **manually**: run `scripts/bump-version.sh <new-major.0.0>`
  (which rewrites all nine manifests, `bindings/rsac-ffi/Cargo.toml`
  included), commit with a normal (non-`release:`) message, then tag and
  push by hand per §4.
- **The `bindings/rsac-go/vX.Y.Z` Go module tag on a manual release.**
  On the automated path it is created + pushed by `release-tag.yml`
  right after the crate tag; on a manual release you push it yourself —
  see §"Versioning & ABI contract" (c).
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
  by `release.yml`; the nine manifests must agree before anything ships.
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
| `mobile/android-native/Cargo.toml` | `[package].version` | Android `librsac.so` shim packaged into the AAR |

Run `bash scripts/bump-version.sh X.Y.Z` to rewrite **all nine** manifests
in one shot — including `bindings/rsac-ffi/Cargo.toml` and its internal
`rsac = { path = "../../", version = "…" }` dependency pin — plus the
CHANGELOG rotation. The
`version-lockstep` CI job re-checks all nine values on every push/PR
(warning on a mid-cycle skew). Because the release PR's merge commit is a
push to `master`, this gate runs — and must be green — on the exact commit that
`release-tag.yml` then tags, so a mismatched manifest never reaches the tag.
At *tag* time, `release.yml`'s `verify` job independently re-checks the pushed
tag against the root `Cargo.toml` before publishing (a second, registry-side
gate), and `ci.yml` re-runs its full nine-manifest lockstep on the `v*.*.*` tag
push too (its `on:` block has a `tags: ['v*.*.*', '!v*-*']` trigger), so the
lockstep gate covers push/PR *and* tag time. A mismatched manifest must never
reach a registry.

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
On the automated path, `release-tag.yml` creates + pushes it right after
the crate tag (same selected credential, independently idempotent, and
recursion-safe — no workflow in this repo triggers on the
`bindings/rsac-go/…` tag shape, since every `tags:` filter is `v*.*.*`
and a single `*` in a tag glob never crosses `/`). On a manual release,
push it yourself alongside the crate tag:

```bash
git tag -a bindings/rsac-go/vX.Y.Z -m "rsac-go X.Y.Z"
git push origin bindings/rsac-go/vX.Y.Z
```

Consumers then `go get
github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go@vX.Y.Z`. The `version-lockstep`
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

Unchecking `dry_run` on `workflow_dispatch` executes a real
`cargo publish` off whatever ref you picked — the guard for that path is
the **`version` input**: a real dispatch publish is refused unless
`version` is provided and matches `Cargo.toml`'s `[package].version`
(dry runs stay flexible — `version` is optional and only checked when
given). Reserve real dispatch publishes for the Step 4 fallback on a
release commit; routine releases always go through a stable `vX.Y.Z`
tag push.

`release-npm.yml` and `release-pypi.yml` also accept manual
`workflow_dispatch` runs. They are safe-by-default: `publish` defaults to
false, so the manual run builds/smokes artifacts but skips registry upload.
To publish manually, set `publish: true` and provide the expected `X.Y.Z`
version; the publish job verifies that value against the binding manifest
before uploading.

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

> **Manual fallback procedure.** The sections below describe how to run
> the release by hand. For a `minor`/`patch` release you normally do
> **not** run these by hand — use the automated release-please-style flow
> (§"Automated release (release-please style)"), which runs
> `scripts/bump-version.sh` for you (§3), opens the bump PR, and pushes the
> tag (§4) on merge; the tag push then does §5 and §7 step 4. These manual
> steps remain the path for a **MAJOR** bump (the automation refuses one),
> when a registry's Trusted Publisher is not yet configured, or when a
> maintainer needs to override the automation. `scripts/bump-version.sh` now exists
> and rewrites all nine manifests + rotates the CHANGELOG — §3 is
> still a manual *invocation* of it, not a hand-edit.

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
rewrites all nine lockstep manifests plus rotates the CHANGELOG:

```bash
# Preview the edits without writing them:
bash scripts/bump-version.sh 0.3.0 --dry-run    # or: mise run release:bump -- 0.3.0 --dry-run

# Apply them:
bash scripts/bump-version.sh 0.3.0              # or: mise run release:bump -- 0.3.0
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
all seven agree and hard-fails on a tag if any disagree.

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

- **Minor/patch releases are automated** via the release-please-style
  two-workflow flow (`release-prepare.yml` + `release-tag.yml`) — see
  §"Automated release (release-please style)". Run **Release Prepare** from
  the Actions tab, squash-merge the `release: vX.Y.Z` PR (keeping that exact
  subject), and the crate tag, the `bindings/rsac-go/vX.Y.Z` Go module tag,
  and the GitHub Release are created automatically. **Registry publishing is
  automatic only when the release App secrets (`RSAC_RELEASE_APP_ID` +
  `RSAC_RELEASE_APP_PRIVATE_KEY`) are configured** — see §"Release App setup
  (automatic registry fan-out)". Without them, the `GITHUB_TOKEN`-pushed tag
  does not trigger the publish workflows and Step 4 stays a manual follow-up.
  **MAJOR bumps stay manual** (the automation refuses to
  change the major): use `scripts/bump-version.sh` + a hand-pushed tag per
  §3–§4. The manual §1–§9 flow remains the fallback whenever the
  automation is unavailable or a maintainer needs to override it.
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
  `mobile/android-native/Cargo.toml` — and rotates the CHANGELOG. It cannot tag
  `rsac-go` — on the automated path
  `release-tag.yml` pushes `bindings/rsac-go/vX.Y.Z` for you; on a manual
  release push it separately (see §"Versioning & ABI contract" (c)). The `version-lockstep` CI job in
  `ci.yml` catches any manifest that drifts: it warns on push/PR and
  hard-fails on a release tag.
- `release-npm.yml` builds five napi-rs triples; other targets
  (`linux-x64-musl`, `linux-arm-gnueabihf`, FreeBSD, Android) are not
  wired. Add them to the matrix if downstream users request them.
- `release-pypi.yml` relies on `manylinux: auto` — wheels are
  `manylinux2014`. Consuming environments on glibc < 2.17 must install
  from sdist.

# Contributing to rsac

Thanks for helping out. This doc is the short-and-honest version; for
architectural context read [`AGENTS.md`](../AGENTS.md) and
[`docs/ARCHITECTURE.md`](ARCHITECTURE.md) first.

## 1. Toolchain

The Rust toolchain is **pinned** via
[`rust-toolchain.toml`](../rust-toolchain.toml). `rustup` will pick it up
automatically; do not install against a different channel or the
pre-commit clippy gate will drift.

- Channel: see `rust-toolchain.toml` (currently `1.95.0`).
- Components: `rustfmt`, `clippy`.
- Bumping the toolchain is intentional — see the comment at the top of
  `rust-toolchain.toml` and the `clippy-toolchain-bump-ci-breakage`
  skill. Run `cargo clippy --all-targets -- -D warnings` locally before
  pushing the bump so new lints do not land cold in CI.

### Platform build dependencies

See [`docs/features.md`](features.md) for the feature matrix.

- **Linux:** `libpipewire-0.3-dev`, `libspa-0.2-dev`, `pkg-config`,
  `clang` / `libclang-dev`, `llvm-dev`.
- **Windows:** MSVC (WASAPI ships with the OS).
- **macOS:** Xcode Command Line Tools. Process Tap features require
  macOS 14.4+.

## 2. The local gate

Every commit must pass this gate. CI runs it too, so skipping it locally
just means you find out later:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib --no-default-features --features feat_linux   # on Linux
# … or feat_windows / feat_macos on the corresponding host
cargo doc --no-deps --all-features
```

`cargo doc` is part of the gate because `src/lib.rs` declares
`#![deny(rustdoc::broken_intra_doc_links)]` — a stale link becomes a
build error.

## 3. Running the test suite

### Unit tests (no audio hardware needed)

```bash
# Linux
cargo test --lib --no-default-features --features feat_linux

# Windows
cargo test --lib --no-default-features --features feat_windows

# macOS
cargo test --lib --no-default-features --features feat_macos
```

### Integration tests (require real audio infrastructure)

`tests/ci_audio/` drives the full capture pipeline end-to-end. The test
helpers use four gating macros (see
[`tests/ci_audio/helpers.rs`](../tests/ci_audio/helpers.rs)):

- `require_audio!()` — skips if audio infrastructure is absent.
- `require_system_capture!()` — adds a macOS TCC gate for `SystemDefault`.
- `require_app_capture!()` — adds the same TCC gate plus a capabilities
  check for application capture.
- `require_process_capture!()` — ditto for process-tree capture.

The TCC gate is controlled by `RSAC_CI_MACOS_TCC_GRANTED=1`. On headless
macOS runners without a TCC grant, Process Tap calls block for 10–18
minutes before erroring — leaving the env var unset lets those tests
skip early instead of hanging.

To run the integration tests locally (Linux example):

```bash
RSAC_CI_AUDIO_AVAILABLE=1 \
  cargo test --test ci_audio --no-default-features --features feat_linux \
             -- --test-threads=1
```

More detail in [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md).

## 4. CI matrix

Pull requests trigger three workflows:

- `.github/workflows/ci.yml` — lint + unit tests on Linux / Windows /
  macOS + bindings check + downstream `audio-graph` build.
- `.github/workflows/ci-audio-tests.yml` — 9-job (platforms × modes)
  audio integration matrix. See the "6 of 9 REAL" truth table in
  [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) for which cells are
  exercised end-to-end versus gated by macOS platform-security limits.
- Release workflows (`release.yml`, `release-npm.yml`,
  `release-pypi.yml`) only trigger on tags.

## 5. Docs

- Module-level `//!` on every public module. Every public type gets at
  least a one-sentence `///` purpose doc; non-trivial public items get
  `# Examples` and, where applicable, `# Errors`.
- `cargo doc --no-deps --all-features` must stay warning-free.
- Prefer updating existing docs over creating new files. See
  [`docs/audit/docs-queue.md`](audit/docs-queue.md) for the current
  documentation audit state.
- In-tree design docs live under `docs/architecture/` (design intent)
  and `docs/reviews/` (loop retrospectives). Don't confuse them: reviews
  snapshot a moment in time; architecture docs stay authoritative.

## 6. Commit style

Prefer imperative, scoped, factual messages:

```
<scope>: <short summary>

<longer body explaining "why" — what problem this change solves and
what alternative was rejected. Wrap at ~72 cols.>
```

Examples in `git log` are plentiful. Avoid editorialising; avoid emoji.

## 7. Release procedure

Releases are driven by `scripts/bump-version.sh`, which keeps five
version-bearing files in sync:

- `Cargo.toml` (root `rsac` crate)
- `bindings/rsac-napi/Cargo.toml`
- `bindings/rsac-napi/package.json`
- `bindings/rsac-python/Cargo.toml`
- `bindings/rsac-python/pyproject.toml`

Plus `CHANGELOG.md` (it rotates the `[Unreleased]` section into a dated
heading and re-seeds an empty `[Unreleased]`).

```bash
bash scripts/bump-version.sh 0.3.0 --dry-run   # preview
bash scripts/bump-version.sh 0.3.0             # apply
git diff                                        # review
git add -A && git commit -m "chore: release 0.3.0"
git tag -a v0.3.0 -m "Release 0.3.0"
git push origin master v0.3.0
```

Pushing the tag triggers `release.yml` (crates.io), `release-npm.yml`
(npm) and `release-pypi.yml` (PyPI). Full procedure including pre-flight
checks and post-publish verification is in
[`docs/RELEASE_PROCESS.md`](RELEASE_PROCESS.md).

## 8. Pull request checklist

- [ ] `cargo fmt --all -- --check` is clean.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- [ ] `cargo doc --no-deps --all-features` is warning-free.
- [ ] New public items have rustdoc (purpose + example where non-trivial +
      `# Errors` where applicable).
- [ ] Relevant CI matrix rows are green (or the PR explains why a row is
      skipped/`continue-on-error`).
- [ ] Commit messages are imperative and scoped.
- [ ] Any behaviour change is mentioned in `CHANGELOG.md` under
      `[Unreleased]`.

## 9. Reporting bugs

Open a GitHub issue with:

- OS and version (e.g., `macOS 14.5`, `Ubuntu 24.04`, `Windows 11 23H2`).
- Rust version (`rustc --version`).
- rsac version or commit SHA.
- A minimal reproduction.
- Captured error output (run with `RUST_LOG=debug` for more).

## 10. Where to learn more

- [`AGENTS.md`](../AGENTS.md) — definitive reference for AI agents and
  contributors. Current state, layering rules, gap closures.
- [`VISION.md`](../VISION.md) — scope, non-goals, design principles.
- [`docs/ARCHITECTURE.md`](ARCHITECTURE.md) — 3-layer overview + backend
  specifics.
- [`docs/architecture/`](architecture/) — design docs (per-topic).
- [`docs/reviews/`](reviews/) — loop-by-loop retrospectives.

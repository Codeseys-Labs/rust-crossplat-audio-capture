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
  `release-pypi.yml`) trigger on stable tags and also support guarded
  manual dispatch for release rehearsals or registry publish recovery.

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

### Code review dispositions

Before a PR merges, **every** review comment (human, CodeRabbit, or agent) must
be either *fixed in the PR* or *captured in a GitHub issue* — a finding must
never silently disappear into a merged PR thread. Triage each comment:

- **fix-now** — fix it; reply on the thread noting the fix.
- **already-addressed** — reply pointing at the code that handles it.
- **valid-defer** — open a tracking issue (label `deferred-review` + a domain
  label such as `bug`/`tech-debt`/`ci`), then reply `📌 Tracked in #N`.
- **invalid** / **wont-fix** — record the decision in an issue (one consolidated
  per-PR "review dispositions" issue is fine; label `invalid`/`wontfix`; close it
  as *not planned* — it's a searchable decision record, not open work) and reply
  with the rationale + link.

Always reply on the originating comment so the thread can be resolved. See
[`AGENTS.md` §6](../AGENTS.md) (Code review dispositions) for the full rule.

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
# Substitute the target version for X.Y.Z (e.g. the current release is 0.4.0).
bash scripts/bump-version.sh X.Y.Z --dry-run   # preview
bash scripts/bump-version.sh X.Y.Z             # apply
git diff                                        # review
git add -A && git commit -m "release: vX.Y.Z"   # subject triggers release-tag.yml
# (release-tag.yml then tags + creates the GitHub Release; see RELEASE_PROCESS.md)
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
- [ ] Every review comment is resolved: fixed in the PR, or captured in a
      tracking issue (`deferred-review`) / decision-record issue — none left to
      vanish on merge (see §6 → Code review dispositions).
- [ ] A large change that layers along the module DAG is split into a **stack of
      small PRs** (one layer per PR, merged bottom-up) rather than one
      hard-to-review mega-PR — see [`STACKED_PRS.md`](STACKED_PRS.md).

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

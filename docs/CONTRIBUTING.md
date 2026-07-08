# Contributing to rsac

Thanks for helping out. This doc is the short-and-honest version; for
architectural context read [`AGENTS.md`](../AGENTS.md) and
[`docs/ARCHITECTURE.md`](ARCHITECTURE.md) first.

## 1. Toolchain

**Rust** is **pinned** via
[`rust-toolchain.toml`](../rust-toolchain.toml). `rustup` picks it up
automatically; do not install against a different channel or your local
clippy results will drift from CI's.

- Channel: see `rust-toolchain.toml` (currently `1.95.0`).
- Components: `rustfmt`, `clippy`.
- Bumping the toolchain is intentional — see the comment at the top of
  `rust-toolchain.toml` for the rationale. Run
  `cargo clippy --all-targets -- -D warnings` locally with the new
  toolchain before pushing the bump so new lints do not land cold in CI.

**Everything else** (Bun, Node, Go, Python for the bindings; lefthook for
git hooks) is pinned via [`mise.toml`](../mise.toml). One-shot setup:

```bash
# install mise: https://mise.jdx.dev  (winget install jdx.mise / brew install mise)
mise install        # installs the pinned polyglot toolchain + lefthook
mise run setup      # installs the git hooks (lefthook install)
```

mise is a convenience, not a requirement — every task below also shows
the direct command. mise deliberately does **not** manage Rust.

### Platform build dependencies

See [`docs/features.md`](features.md) for the feature matrix.

- **Linux:** `libpipewire-0.3-dev`, `libspa-0.2-dev`, `pkg-config`,
  `clang` / `libclang-dev`, `llvm-dev`.
  On Windows/macOS, the fastest way to work on the Linux leg is the
  **devcontainer** ([`.devcontainer/`](../.devcontainer/devcontainer.json)) —
  it reuses `docker/linux/Dockerfile.test` (full PipeWire stack, session
  daemons booted at start) and runs
  `cargo check --features feat_linux` on create.
- **Windows:** MSVC (WASAPI ships with the OS). Git for Windows provides
  the `bash` used by the gate script and hooks.
- **macOS:** Xcode Command Line Tools. Process Tap features require
  macOS 14.4+.

## 2. The local gate

The gate is a **faithful replica of ci.yml's `lint` job** for your host
OS — same commands, same feature flags — so passing locally means the
lint leg passes in CI. It lives in one place,
[`scripts/gate.sh`](../scripts/gate.sh) (PowerShell wrapper:
`scripts/gate.ps1`), and is wired into the pre-push git hook.

```bash
mise run gate        # or: bash scripts/gate.sh
#   1. cargo fmt --all -- --check
#   2. cargo clippy --all-targets --no-default-features \
#        --features feat_<host>,compose,cli -- -D warnings
#   3. cargo build --no-default-features        (bare-build smoke)

mise run gate:full   # or: bash scripts/gate.sh --full
#   … adds: lib tests, doctests, cargo doc (docsrs, -D warnings),
#   and the module-DAG guard — the rest of the fast CI legs.
```

Other everyday tasks (`mise tasks` is the live list): `mise run test`
(CI test-job replica), `mise run test:audio` (the `ci_audio` integration
suite on this machine, dispatching to the host-OS script), and the
release pair `mise run release:bump -- X.Y.Z [--dry-run]` /
`mise run release:verify-docs` (§7).

`cargo doc` is part of `gate:full` because `src/lib.rs` declares
`#![deny(rustdoc::broken_intra_doc_links)]` — a stale link becomes a
build error.

### Git hooks (lefthook)

[`lefthook.yml`](../lefthook.yml) defines the hooks; `mise run setup`
(or `lefthook install`) activates them per-clone:

- **pre-commit** — `cargo fmt --all -- --check` (only when `.rs` files
  are staged).
- **commit-msg** — rejects `Co-Authored-By:` trailers and tool bylines
  (the AGENTS.md §6 commit conventions, enforced mechanically).
- **pre-push** — runs the gate.

Hooks are opt-in and skippable (`git push --no-verify`); CI is the
backstop either way.

### Editor setup

Several cargo features are off by default (`compose`, `cli`, `sink-wav`,
`async-stream`, `test-utils`), so without configuration rust-analyzer
shows **no diagnostics** inside `src/compose/`, `src/main.rs`, or the
gated examples. VS Code picks the checked-in
[`.vscode/settings.json`](../.vscode/settings.json) up automatically.
For other editors, set the equivalent of:

```json
{ "rust-analyzer.cargo.features": ["compose", "cli", "sink-wav", "async-stream", "test-utils"] }
```

(Zed: `languages.Rust.language_servers` → rust-analyzer
`initialization_options.cargo.features`; Neovim: pass the same table via
your LSP config's `settings`.)

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

On a machine with real, working audio, also export
`RSAC_CI_AUDIO_DETERMINISTIC=1` — it turns the capture tests' soft
non-silence warnings into hard assertions (see the workflow-knob list in
[`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md#5-workflow-knobs)).

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
- In-tree design docs live under `docs/architecture/` (original design
  intent — **historical**; each carries a divergence banner) and
  `docs/reviews/` (loop retrospectives — snapshots of a moment in time).
  Per [`AGENTS.md` §2](../AGENTS.md): **the code is the source of
  truth**. When a design doc and the code disagree, fix the doc (or note
  the divergence in its banner) — never bend the code to match a stale
  doc. Durable decisions are recorded as ADRs in
  [`docs/designs/`](designs/).

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

Releases are driven by `scripts/bump-version.sh`, which keeps six
version-bearing files in sync:

- `Cargo.toml` (root `rsac` crate)
- `bindings/rsac-ffi/Cargo.toml`
- `bindings/rsac-napi/Cargo.toml`
- `bindings/rsac-napi/package.json`
- `bindings/rsac-python/Cargo.toml`
- `bindings/rsac-python/pyproject.toml`

Plus `CHANGELOG.md` (it rotates the `[Unreleased]` section into a dated
heading and re-seeds an empty `[Unreleased]`).

```bash
# Substitute the target version for X.Y.Z (e.g. the current release is 0.4.0).
# `mise run release:bump -- X.Y.Z` is the same command via the task runner.
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

- [ ] `mise run gate` (or `bash scripts/gate.sh`) is clean — fmt,
      CI-replica clippy `-D warnings`, bare-build smoke.
- [ ] `mise run gate:full` extras are clean where relevant — lib tests,
      doctests, `cargo doc` (warning-free), module-DAG guard.
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

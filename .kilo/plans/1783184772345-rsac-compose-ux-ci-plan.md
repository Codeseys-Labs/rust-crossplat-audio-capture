# rsac Improvement Plan — Multi-Source Channel Compositor + UX/CI Hardening

> Tracking: all work items are filed as **seeds** (git-native issue tracker, `sd` CLI) with full
> descriptions, file paths, and a wired dependency graph. This plan is the index; the seeds are
> the detailed source of truth. Run `sd ready` to see unblocked work; `sd show <id>` for detail.
> The new `.seeds/` entries are **uncommitted** — commit them (e.g. `sd sync`) when starting work.

## Context (from the 2026-07-04 audit)

- Data plane today is strictly `1 target → 1 device → 1 ring → 1 stream` (src/api.rs:93). Multi-source = N independent `AudioCapture`s; no fan-in, no mixing, no resampler, no channel mapping.
- VISION.md:140-169 declares mixing **out of scope** — this plan deliberately amends that via ADR.
- Windows process loopback cannot autoconvert (src/audio/windows/wasapi.rs:152) → heterogeneous rates are a real, common case → rubato resampling is required, not optional.
- `AudioBuffer::timestamp` is never populated → v1 alignment is master-clock + FIFO based; timestamp drift-correction stays deferred (existing seed **rsac-ec25**).
- CLI-only deps (clap/color-eyre/ctrlc/env_logger, Cargo.toml:85-90) are unconditional library deps.
- CI is mature but lacks: MSRV job, feature powerset, semver-checks; has a stale probe workflow and fragile grep-based ARM64 gates.

## Resolved design decisions

1. **Placement:** in-crate, feature-gated (`compose` feature, `src/compose/` — new top DAG layer: `core → bridge → audio → api → compose`). Rejected: separate `rsac-mixer` crate; fan-in-only (backlog M3).
2. **Rate reconciliation:** rubato (optional dep, pulled only by `compose`) resamples any source ≠ session rate (default 48 kHz), on the compositor thread (non-RT).
3. **API shape:** `CompositionBuilder` → groups of `CaptureTarget`s; per group `mixdown(Mono|Stereo)` (per-source gain, plain summation, no auto-limiter, optional clamp default-off) or `keep_channels()` (v1: exactly one source); groups append in declaration order; `channel_map()` introspection. Output implements the existing `CapturingStream` contract → `drain_to`/`subscribe`/async/`WavFileSink` work unchanged.
4. **Pacing:** master-clock source (first system/device source) + per-source FIFOs; silence-pad behind, bounded-trim ahead; wall-clock fallback on master stall; per-source `padded_frames`/`trimmed_frames` stats.

## Epic 1 — Multi-source channel compositor · `rsac-e336` (P1)

Execution order (deps wired in seeds; ADR and scaffolding are the two ready entry points):

| Order | Seed | Item |
|---|---|---|
| 1 | `rsac-73a7` | ADR (docs/designs/) + VISION.md/AGENTS.md amendment — scope change |
| 1 | `rsac-07e3` | Scaffolding: `compose` feature, rubato dep (verify MSRV 1.87), `src/compose/mod.rs`, `scripts/check-module-dag.sh` update |
| 2 | `rsac-3b9a` | Types: `CompositionBuilder`, `Group`, `GroupLayout`, `ChannelMap`, preflight validation |
| 3 | `rsac-c65b` | Ingest + alignment engine: per-source FIFOs, master-clock pacing, silence-pad/trim, stats, teardown (mirrors CallbackPump/DrainHandle) |
| 3 | `rsac-0578` | Resampling: rubato per source → session rate (parallel with c65b) |
| 4 | `rsac-602a` | Mixdown math + channel composition + `CapturingStream` impl |
| 5 | `rsac-0f46` | Tests: mock-backend units (src/bridge/mock.rs 440 Hz), `tests/ci_audio/compose.rs` integration, `examples/composed_capture.rs` |
| 5 | `rsac-c276` | Docs: README section, rustdoc doctests, VISION/ARCHITECTURE updates |
| 6 | `rsac-0a1f` | CI wiring: compose in clippy/feature-combo/docs jobs + Linux audio tier |

## Epic 2 — Library UX + publish hygiene · `rsac-96e7` (P2, all independent)

| Seed | Item |
|---|---|
| `rsac-1ecd` | `cli` feature gating clap/color-eyre/ctrlc/env_logger; `required-features` on all `[[bin]]`s |
| `rsac-d26a` | repository URL fix (baladita→Codeseys-Labs), exclude `bindings/rsac-go` from tarball, README badge dedup |
| `rsac-d4bc` | `#![warn(missing_docs)]` + fill gaps (ErrorKind variants, error fields, `audio::linux` pub items) |

## Epic 3 — CI hardening · `rsac-b9df` (P2, all independent)

| Seed | Item |
|---|---|
| `rsac-c1d1` | MSRV (1.87) check job — also the tripwire for rubato's MSRV |
| `rsac-6ab9` | cargo-hack feature-powerset (exclude cross-OS `feat_*`; covers compose + cli) |
| `rsac-bc85` | cargo-semver-checks in release.yml verify stage (`--baseline-rev` until first crates.io publish) |
| `rsac-4dda` | Delete stale `blacksmith-audio-probe.yml`; replace grep-based ARM64 gates with real exit codes |

## Risks

- **rubato MSRV vs 1.87** — check before pinning (rsac-07e3); rsac-c1d1 is the CI tripwire.
- **Feature-matrix growth** — mitigated by rsac-6ab9 powerset job.
- **Compositor lifecycle/teardown** — must mirror the proven stop-flag+join+self-join-guard pattern (src/api.rs:1142, :843); single-consumer-per-ring rule: compositor owns inner captures' consumption.
- **Clock semantics** — v1 has no timestamp alignment; long-recording drift bounded by FIFO trim policy. Proper fix tracked in rsac-ec25.

## Validation

- `cargo test --lib` + new compose unit tests (mock backend, deterministic).
- `cargo clippy --all-targets -D warnings` per platform leg incl. `compose`.
- `bash scripts/check-module-dag.sh` (with new compose edges + self-test).
- Linux ci-audio tier runs the compose integration module (hard format asserts, soft content per existing policy).
- `cargo package --list` confirms rsac-go exclusion; `cargo tree` confirms CLI deps gone for library consumers.

## Explicitly deferred (not in this plan)

- Timestamp population + drift correction → existing seed **rsac-ec25**.
- Compose exposure through FFI/Python/Node bindings → future epic (noted in rsac-e336).
- tag→publish token handoff, dasp interop, coverage reporting → declined in scoping.

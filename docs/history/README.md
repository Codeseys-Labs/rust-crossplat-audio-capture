# docs/history — Archived Snapshots

These documents are **frozen snapshots** retained for historical context only.
They do **not** reflect the current state of the project — several reference
APIs, binaries, or workflows that were deleted (the Phase 0 legacy-API
removal, the old CI layout, the VLC docker experiments).

For current documentation, start at the [docs index](../README.md). Key
entry points:
- [README.md](../../README.md) — project overview
- [CHANGELOG.md](../../CHANGELOG.md) — version history
- [docs/ARCHITECTURE.md](../ARCHITECTURE.md) — current architecture overview
  (the `docs/architecture/` design docs are themselves historical — code wins)
- [docs/features.md](../features.md) — current feature matrix
- [docs/RELEASE_PROCESS.md](../RELEASE_PROCESS.md) — release procedure
- [docs/troubleshooting.md](../troubleshooting.md) — current troubleshooting
- [docs/CI_AUDIO_TESTING.md](../CI_AUDIO_TESTING.md) — current CI testing docs

## Contents

### Frozen March 14, 2026 (early development)

- `031326-survey.md` — initial project survey
- `PRODUCT_REQUIREMENTS.md` — original PRD
- `PROJECT_ANALYSIS.md` — early project-structure analysis
- `CARGO_TOML_FIXES.md` — one-off Cargo.toml fix log
- `CI_CD_STATUS_REPORT.md`, `CI_CD_IMPROVEMENTS_SUMMARY.md` — CI snapshots
- `CROSS_COMPILATION_STATUS.md`, `CROSS_COMPILATION_TOOLS_SUMMARY.md`,
  `DOCKER_CROSS_COMPILATION_OPTIONS.md` — cross-compile experiments
- `DEPLOYMENT_STATUS.md` — release readiness snapshot
- `MANUAL_WORKFLOW_SETUP.md` — pre-automation GH Actions guide

### Quarantined July 5, 2026 (docs-rot cleanup, rsac-5d88)

Stale/dead — they describe infrastructure that no longer exists:

- `LOCAL_CI.md` — `act`-based local CI runs against workflows/jobs/inputs
  that no longer exist (and a PulseAudio backend rsac never shipped)
- `TESTING.md` — old testing infrastructure (`src/audio/test_utils.rs`,
  `AudioBackendTests`, `test_runner` — none exist)
- `TESTING_REFACTORING.md` — test-refactor plan written against the deleted
  `get_audio_backend()` API
- `WINDOWS_AUDIO_DEBUG.md` — VLC + virtual-audio-driver CI experiment
  (already banner'd historical; its workflow and `dynamic_vlc_capture` bin
  are gone)
- `build_configuration_summary.md`, `ci_cd_setup.md`,
  `implementation_summary.md` — the pre-Phase-0 "application-specific
  capture" push: dependencies, workflow, and APIs described therein were
  all replaced

Completed plans (executed long ago; kept as decision context):

- `IMPLEMENTATION_PLAN.md` — the Phase 0–4 architecture-alignment backlog
  (fully executed; AGENTS.md §3 is the current state)
- `app_capture_implementation_plan.md` — early per-platform app-capture
  plan (superseded by the shipped G1–G6 gap closures)
- `app_specific_capture_research.md` — app-capture research notes
  (proposes extending the deleted `AudioCaptureBackend` trait)

If you're looking for the reasoning behind a specific decision, check the
corresponding commit history (`git log --follow docs/history/<file>`) rather
than relying on these frozen docs.

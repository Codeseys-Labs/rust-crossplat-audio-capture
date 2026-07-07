# scripts/

What each script is for and **who calls it**. Anything not listed here was
deleted in the 2026-07-05 rot cleanup (rsac-a3c4) — recover via
`git log --diff-filter=D -- scripts/` if you need the history.

## Developer entry points

| Script | Purpose | Called by |
|---|---|---|
| `gate.sh` | The local gate — replica of ci.yml's `lint` job for the host OS (`--full` adds tests/doc/DAG) | `mise run gate`, lefthook pre-push, humans |
| `gate.ps1` | PowerShell wrapper for `gate.sh` (finds Git bash, avoids WSL bash) | `mise run gate` on Windows, humans |
| `hooks/commit-msg.sh` | Rejects `Co-Authored-By:` trailers / tool bylines (AGENTS.md §6) | lefthook commit-msg hook |
| `test-audio-linux.sh` / `test-audio-macos.sh` / `test-audio-windows.ps1` | Run the `ci_audio` integration suite (all 3 capture tiers) on a physical machine | humans (see `docs/LOCAL_TESTING_GUIDE.md`) |
| `install-pipewire-deps.sh` | Distro-detecting install of the Linux PipeWire build deps | humans |
| `setup_env.sh` + `check_deps.sh` | Basic Linux env init + pkg-config dependency check | humans (`setup_env.sh` calls `check_deps.sh`) |
| `test-pipewire-setup.sh` | Diagnose a Linux PipeWire environment (daemons, tools, nodes) | humans |
| `debug-audio-system.sh` | PulseAudio/PipeWire/ALSA diagnostics dump | humans |

## CI / release plumbing

| Script | Purpose | Called by |
|---|---|---|
| `check-module-dag.sh` | Module-DAG reverse-edge guard (`core→bridge→audio→api`) | ci.yml `module-dag` job, `gate.sh --full` |
| `ci-linux-audio-route.sh` | Deterministic PipeWire routing gate: pins `ci_test_sink` as default, proves the tone→monitor route end-to-end (sox RMS + frequency), then exports `RSAC_CI_AUDIO_DETERMINISTIC=1` (rsac-b106/rsac-6efb) | ci-audio-tests.yml `linux-system`/`linux-device`/`linux-process`, humans on a Linux box |
| `ci-windows-audio-default.ps1` | Deterministic VB-CABLE endpoint gate: sets + hard-verifies the default playback endpoint, then exports `RSAC_CI_AUDIO_DETERMINISTIC=1` (rsac-0f33) | ci-audio-tests.yml `windows-system`/`windows-device`/`windows-process` |
| `bump-version.sh` | Bumps the six version-bearing manifests + rotates CHANGELOG | release-prepare.yml, humans (see CONTRIBUTING §7) |
| `verify-docs-rs.sh` | Post-publish docs.rs rendering spot-check | humans (see RELEASE_PROCESS.md) |

## Docker testing stack

| Script | Purpose | Called by |
|---|---|---|
| `docker-test-all.sh` | Unified docker test orchestrator (`docker-compose.unified.yml`) | `make docker-test-all` etc. |
| `aggregate-test-results.sh` | Aggregates `test-results/` from platform containers | `make docker-aggregate-results` etc. |
| `verify-platform-testing.sh` | Validates the docker testing stack's file inventory | humans |
| `cross-compile-check.sh` | `cross`-based check for the Linux targets (Darwin/MSVC legs removed — impossible with cross-rs) | `make cross-compile` |
| `download_test_audio.sh` | Fetches the `test_audio.mp3` fixture some docker builds COPY | humans, before docker matrix builds |

## ⚠️ Disabled (fail-fast stubs, kept only because live callers reference them)

| Script | Why |
|---|---|
| `run_audio_tests.sh` | Depended on the `run_tests` / `test-report-generator` bins removed in Phase 0. Referenced by `docker-compose.yml`, `docker-compose.unified.yml`, `docker/linux/Dockerfile.unified` — those matrix legs are non-functional anyway (missing `pulse-*.conf` COPY sources). Rebuild on `cargo test --test ci_audio` if the functionality is ever wanted again. |
| `run_linux_matrix_tests.sh` | Orchestrated the matrix above; disabled for the same reason. |

Known remaining docker-stack debt (not scripts): `docker/linux/Dockerfile.unified`
COPYs nonexistent `pulse-client.conf`/`pulse-daemon.conf`; several Dockerfiles
pin `rust:1.88.0` vs the repo toolchain 1.95.0; `docker-compose.yml` defines
`rsac-linux-pipewire` twice.

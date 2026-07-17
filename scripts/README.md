# scripts/

What each script is for and **who calls it**. Anything not listed here was
deleted in the 2026-07-05 rot cleanup (rsac-a3c4) — recover via
`git log --diff-filter=D -- scripts/` if you need the history.

## Developer entry points

| Script | Purpose | Called by |
|---|---|---|
| `gate.sh` | The local gate — replica of ci.yml's `lint` job for the host OS, including the docsrs `cargo doc` step (`--full` adds lib tests/doctests/module-DAG guard) | `mise run gate`, lefthook pre-push, humans |
| `gate.ps1` | PowerShell wrapper for `gate.sh` (delegates via `run-bash.ps1`) | `mise run gate` on Windows, lefthook pre-push |
| `gate-bindings.sh` | Local replica of ci.yml's `check-bindings` job — rsac-ffi/napi/python check+clippy+test, header drift, napi + python runtime smokes. Each leg skips gracefully if its toolchain is missing (`--strict` hard-fails instead) | `mise run gate:bindings`, humans |
| `run-bash.ps1` | Generic Windows wrapper: finds Git bash (avoids WSL bash) and runs any repo bash script with args | `gate.ps1`, the Windows legs of `mise run release:bump` / `release:verify-docs` |
| `hooks/commit-msg.sh` | Rejects `Co-Authored-By:` trailers / tool bylines (AGENTS.md §6) | lefthook commit-msg hook |
| `test-audio-linux.sh` / `test-audio-macos.sh` / `test-audio-windows.ps1` | Run the `ci_audio` integration suite (all 3 capture tiers) on a physical machine | `mise run test:audio` (host-OS dispatch), humans (see `docs/LOCAL_TESTING_GUIDE.md`) |
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
| `bump-version.sh` | Bumps the seven version-bearing manifests + rotates CHANGELOG | release-prepare.yml, `mise run release:bump -- X.Y.Z`, humans (see CONTRIBUTING §7) |
| `verify-docs-rs.sh` | Post-publish docs.rs rendering spot-check | `mise run release:verify-docs`, humans (see RELEASE_PROCESS.md) |

## Cross-compilation helpers

| Script | Purpose | Called by |
|---|---|---|
| `cross-compile-check.sh` | `cross`-based check for Linux targets (Darwin/MSVC legs are impossible with cross-rs) | `make cross-compile`, humans |

## Retired Docker matrix

The old Docker test matrix (`docker-compose*.yml`, `scripts/docker-test-all.sh`,
`scripts/run_audio_tests.sh`, and related report/fixture scripts) was removed in
the 0.4.1 docs cleanup. It depended on removed examples and helper binaries.
Use `mise run gate`, `mise run test`, `mise run test:audio`, and the CI audio
workflows instead. The only maintained Docker image is the devcontainer image at
`docker/linux/Dockerfile.test`; the `dockur` VM lab remains manual/experimental.

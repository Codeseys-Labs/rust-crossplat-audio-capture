# docker/ — Containerized Test & Cross-Compile Environments

Docker images for testing rsac on platforms you don't have locally, and for
cross-compiling to targets that need specific sysroots.

## Layout

| Subdir | Purpose |
|---|---|
| `linux/` | Build + test rsac on Debian + PipeWire. Includes `Dockerfile`, `Dockerfile.test`, `Dockerfile.unified`, and PipeWire entrypoint / verify scripts. |
| `macos/` | Cross-compile rsac from a Linux host to `x86_64-apple-darwin` / `aarch64-apple-darwin`. Includes `Dockerfile.cross` + `test-cross.sh`. |
| `windows/` | Cross-compile rsac from a Linux host to `x86_64-pc-windows-msvc` via `cargo-xwin`. Includes `Dockerfile.cargo-xwin` + `test-cargo-xwin.sh`. |
| `testing/` | Per-platform CI-style runners: `Dockerfile.linux-pipewire`, `Dockerfile.macos`, `Dockerfile.windows-cross`. |
| `unified/` | Single Dockerfile that builds rsac for all three platforms in one image. Useful for one-shot CI or release verification. |
| `dockur/` | Full Windows + macOS virtual machines running inside Docker via [dockur/windows](https://github.com/dockur/windows) + [dockur/macos](https://github.com/dockur/macos). Provides **native** WASAPI / CoreAudio testing from a Linux host via QEMU/KVM. |

## Quick Start

```bash
# Linux PipeWire build + test
docker build -f docker/linux/Dockerfile -t rsac:linux .
docker run --rm rsac:linux

# Windows cross-compile from Linux
docker build -f docker/windows/Dockerfile.cargo-xwin -t rsac:win-cross .
docker run --rm rsac:win-cross

# Unified build for all 3 platforms
docker build -f docker/unified/Dockerfile -t rsac:unified .
```

See also: [docs/DOCKER_TESTING.md](../docs/DOCKER_TESTING.md) and
[docs/LOCAL_CI.md](../docs/LOCAL_CI.md) for end-to-end workflows, and
[scripts/docker-test-all.sh](../scripts/docker-test-all.sh) for the
canonical "run every test in a container" entry point.

## Why Not `.docker/`?

The previous `.docker/` hidden directory was an early-development orphan
with no references from scripts, docs, or CI. It was removed in the
2026-04-24 repo reorg — all active containerization lives here under
`docker/`.

# Docker-based Cross-Platform Testing

This project targets Windows (WASAPI), macOS (CoreAudio) and Linux (PipeWire/PulseAudio). Each platform has its own Dockerfile under `docker/` used for running the example programs and basic tests.

## Linux
- **Base image:** `ubuntu:22.04`
- Installs PipeWire and PulseAudio with all development headers.
- Runs `pipewire` and `pipewire-pulse` inside the container together with `dbus` and `Xvfb` so that graphical audio applications can run.
- The Linux Dockerfile executes `docker/linux/test.sh` which builds the library and runs the example capture programs.

## Windows
- **Base image:** `mcr.microsoft.com/windows:ltsc2022` with PowerShell support.
- Configures a user account and enables the Windows Audio service.
- The test script `docker/windows/test.ps1` compiles the crate using the MSVC toolchain and runs the Windows WASAPI example.

## macOS
- **Base image:** [`sickcodes/docker-osx`](https://github.com/sickcodes/Docker-OSX) which provides a virtualized macOS environment.
- Installs Homebrew, Rust and the [BlackHole](https://github.com/ExistentialAudio/BlackHole) audio driver for loopback audio capture.
- The script `docker/macos/test.sh` builds the crate and runs the CoreAudio example inside the macOS VM.

## Running all containers
The simplest way to exercise all platforms is with docker-compose:

```bash
docker-compose up --build
```

Each container mounts the repository and executes its platform specific test script. Results are written to the `test_results/` directory.

Running these containers requires enough CPU/RAM and, for the macOS image, hardware virtualization support.

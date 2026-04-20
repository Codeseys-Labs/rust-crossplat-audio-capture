# rsac Troubleshooting

High-signal fixes for the errors users hit most often when building or running rsac. Each entry names the symptom, then the minimal fix. For the feature matrix referenced below, see [`features.md`](features.md).

## `cargo build` on Linux fails with `CoreAudio` or `objc2` errors

**Symptom:** Compiler errors mentioning `coreaudio-sys`, `objc2`, `NSWorkspace`, or missing Apple frameworks while building on Linux/Windows.

**Cause:** The macOS backend is being compiled on a non-macOS host, which should be impossible since `src/audio/mod.rs` gates `pub mod macos` on `target_os = "macos"`. If you see this, something in your dependency tree is forcing the `feat_macos` feature path.

**Fix:** Build with only the feature that matches your host, e.g. on Linux `cargo build --no-default-features --features feat_linux`. Do the same in downstream `Cargo.toml` entries that depend on `rsac`.

## `cargo build` on Linux fails: `pkg-config exited with status code 1` / `libpipewire-0.3 not found`

**Symptom:** Build breaks in `build.rs` or during link; `pkg-config` complains about `libpipewire-0.3` or `libspa-0.2`.

**Fix (Debian/Ubuntu):** `sudo apt install libpipewire-0.3-dev libspa-0.2-dev pkg-config libclang-dev llvm-dev clang build-essential`.
**Fix (Fedora):** `sudo dnf install pipewire-devel pkg-config clang-devel llvm-devel`.
**Fix (Arch):** `sudo pacman -S pipewire pkgconf clang llvm`.

After install, clear the build cache once: `cargo clean && cargo build`.

## Linux runtime: capture returns no data / `PipeWire daemon not running`

**Symptom:** `enumerate_devices()` succeeds but `read_buffer()` always returns `Ok(None)`, or the stream errors with `BackendError("Failed to connect to audio server")`.

**Fix:** Confirm PipeWire is the active audio server: `systemctl --user status pipewire pipewire-pulse wireplumber`. All three should be active. If PulseAudio is still running, rsac will not work — PipeWire-compat replaces it. On headless CI, start `pipewire` + a null-sink via the `docker/linux/` setup.

## Windows: `cargo build` links but `AUDCLNT_E_DEVICE_IN_USE` at runtime

**Symptom:** Another application holds the capture endpoint in exclusive mode, or the target app is not producing audio right now.

**Fix:** Close apps that may hold exclusive mode (DAWs, conferencing tools in some modes). For per-app capture, confirm the target PID is actually playing audio — WASAPI process loopback returns silence for idle sessions. Use `rsac list` (or `wasapi_session_test --features feat_windows`) to verify an active session exists.

## Windows CI: no audio device / headless runner has no sound card

**Symptom:** CI job on `windows-latest` fails device enumeration.

**Fix:** Install VB-CABLE as the default output device (the audio-tests workflow in this repo uses the LABSN install path and sets VB-CABLE as default — see `.github/workflows/ci-audio-tests.yml`). For local runs without speakers, same install path works.

## macOS: build fails with `xcrun: error: invalid active developer path`

**Symptom:** `cargo build` on macOS errors immediately during `coreaudio-sys` / `objc2` compile with a missing-SDK message.

**Fix:** Install Xcode Command Line Tools: `xcode-select --install`. Reboot the shell. If you have a full Xcode install and the error persists, run `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`.

## macOS runtime: `NotDetermined` permission / silent capture

**Symptom:** `PlatformCapabilities::query().check_audio_capture_permission()` returns `NotDetermined`, or capture returns silence on macOS.

**Fix:** rsac Process Tap needs **Screen Recording** permission (not Microphone). Open *System Settings → Privacy & Security → Screen & System Audio Recording*, enable your binary, and **fully quit and relaunch** the process — granted permissions are picked up only at process start. On first launch macOS prompts only after a capture attempt, not at enumeration time. Also verify you are on macOS 14.4+; Process Tap does not exist on earlier versions.

## macOS: `CATapDescription not found` or Process Tap returns empty data

**Symptom:** Per-app capture on 14.4+ builds cleanly but the aggregate device yields zero frames, or logs mention failed Tap installation.

**Fix:** `sudo killall coreaudiod` to restart the Core Audio daemon (clears stale Tap state). If the target app was launched before rsac, relaunch the target — Process Tap binds to the session, and apps that started before TCC approval may be ignored by the daemon. Sandboxed App Store apps cannot be tapped; this is an OS restriction, not an rsac bug.

## `cargo build` runs on the wrong platform by accident

**Symptom:** You intended to cross-compile but the build picks up host backends anyway, or `rust-analyzer` shows errors referencing a platform you are not targeting.

**Fix:** Cross-compilation of rsac between the three OSes is **not supported** — each backend links native system libraries (WASAPI, PipeWire, CoreAudio) that the cross-toolchain cannot provide. Build each platform on its own host (CI does this with three separate matrix jobs). For rust-analyzer, set the features it should analyze under in your editor config, e.g. `"rust-analyzer.cargo.features": ["feat_linux"]`.

## Ring buffer drops frames / `overrun_count()` grows / `is_under_backpressure()` is true

**Symptom:** Under load, frames are dropped faster than they are read.

**Fix:** Consumer is slower than the producer. Options: (1) process buffers off-thread via `subscribe()` and a dedicated worker, (2) reduce per-buffer work (move heavy DSP/ASR behind a queue), (3) raise the ring-buffer capacity at the backend level if the workload genuinely bursts. `overrun_count()` resets when the stream is torn down, so reset-on-stop is expected.

## Still stuck

Collect the following before filing an issue:

- OS + version (`sw_vers` / `ver` / `uname -a`)
- `rustc --version` and exact `cargo build` invocation (feature flags matter)
- Full error output with `RUST_LOG=debug` / `env_logger` enabled
- Output of `rsac info` and `rsac list`
- Whether the repro uses system capture, per-app capture, or process-tree capture

File at the repo's issue tracker. Linux repros run headlessly in `docker/linux/` — attach a minimal reproducer there if possible.

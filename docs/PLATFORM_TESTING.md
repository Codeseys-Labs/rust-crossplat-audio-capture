# Platform Testing

> **How rsac is verified across Windows (WASAPI), Linux (PipeWire), and macOS
> (CoreAudio).**
>
> This is a map of the three verification layers. For the authoritative audio
> integration test matrix (what each cell covers and why) see
> [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md); for hands-on QA on real
> hardware see [`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md).

---

## The core constraint: build each platform on its host

rsac cannot be cross-compiled between the three OSes — each backend links native
system libraries (WASAPI, PipeWire, CoreAudio) that a cross-toolchain cannot
provide. **Every platform is built and tested on its own host.** CI does this
with separate per-OS matrix jobs; local QA is done on physical machines. (The
only cross-*compile* checks that exist are ARM64 *compile-only* gates — no link,
no run. See [`docs/troubleshooting.md`](troubleshooting.md) § cross-compilation.)

---

## Three verification layers

### 1. Automated CI (the source of truth)

Two workflows run on every push/PR to `main`/`master`.

#### `ci.yml` — lint, unit tests, cross-compile checks

Runs on all three platforms plus bindings and supporting gates:

| Job | Runner | What it does |
|---|---|---|
| Lint & Format | `blacksmith-4vcpu-ubuntu-2404`, `blacksmith-4vcpu-windows-2025`, `blacksmith-6vcpu-macos-15` | `cargo fmt --check` (Linux), `cargo clippy --all-targets -D warnings` per feature |
| Linux Unit Tests | `blacksmith-4vcpu-ubuntu-2404` | `cargo test --lib` + `rt_alloc` + `enumeration_matrix` (`feat_linux`) |
| Windows Unit Tests | `blacksmith-4vcpu-windows-2025` | `cargo test --lib` (`feat_windows`); device-touching tests partitioned into a tolerated step |
| macOS Unit Tests | `blacksmith-6vcpu-macos-15` | `cargo test --lib` (`feat_macos`) + Process-Tap wiring existence checks |
| Linux ARM64 Cross-Compile | `blacksmith-4vcpu-ubuntu-2404` | `cargo check --target aarch64-unknown-linux-gnu` (compile-only) |
| Binding / Go / docs / supply-chain / DAG gates | `blacksmith-4vcpu-ubuntu-2404` (+ Go matrix on all 3) | FFI/Python/Node/Go bindings, `cargo doc`, `cargo-deny`, module-DAG guard, version lockstep |

Windows unit tests run on the **Blacksmith Windows** runner (compile + unit
logic only). That runner's Firecracker microVM has **no audio subsystem at all**
(`AudioSrv`, `AudioEndpointBuilder`, `AudioSes.dll` are absent — see AGENTS.md
§6), so device-touching WASAPI unit tests are split out via `--skip` and re-run
as a tolerated (`continue-on-error`) step; the pure-logic tests hard-fail.

#### `ci-audio-tests.yml` — audio integration tests

A platform × capture-tier matrix (system / device / process) that actually
routes a 440 Hz tone through the OS and captures it via the rsac public API:

| Platform | Runner | Virtual audio |
|---|---|---|
| **Linux** (PipeWire) — PRIMARY, hard-gated | `blacksmith-4vcpu-ubuntu-2404` | PipeWire + WirePlumber + `pipewire-pulse` launched **manually** (Firecracker VMs have no D-Bus user session, so `systemctl --user` won't start them), plus a `module-null-sink` virtual sink |
| **Windows** (WASAPI) — first-class, hard-gated | `windows-latest` (**GitHub-hosted**, not Blacksmith) | VB-CABLE via [`LABSN/sound-ci-helpers@v1`](https://github.com/LABSN/sound-ci-helpers), set as default endpoint with `AudioDeviceCmdlets` |
| **macOS** (CoreAudio) — advisory | `blacksmith-6vcpu-macos-15` | BlackHole 2ch via Homebrew |

Two nuances worth remembering:

- **Windows audio integration runs on GitHub-hosted `windows-latest`, not
  Blacksmith**, precisely because the Blacksmith Windows image lacks an audio
  stack. `ci.yml`'s Windows *unit* job still runs on Blacksmith.
- **macOS Process-Tap paths skip early** (`require_*_capture!()` gates on
  `RSAC_CI_MACOS_TCC_GRANTED=1`). `kTCCServiceAudioCapture` cannot be granted
  non-interactively on a managed runner, so those jobs are `continue-on-error`
  and excluded from the summary gate. macOS is verified on real hardware
  instead.

The summary job **fails on any Linux or Windows regression**; macOS stays
advisory. **Do not duplicate the truth table here** — it lives in
[`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md), which is the maintainer-facing
reference for exactly which cells are REAL vs. SKIP-EARLY and why.

### 2. Local manual QA on real hardware

CI cannot cover everything: macOS Process Tap (TCC-gated) and the full Windows
audio stack are only exercised end-to-end on physical machines. Before a
release, all three capture levels (system, application, process tree) are
validated by hand on real Windows, macOS (14.4+), and Linux hosts.

The step-by-step setup and validation procedure is in
[`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md). Quick smoke checks use
the real examples:

```bash
cargo run --example verify_audio       # end-to-end capture sanity check
cargo run --example list_devices       # enumerate devices via the public API
cargo run --example basic_capture      # minimal capture loop
cargo run --example record_to_file --features sink-wav
cargo run --example async_capture --features async-stream
```

Feature-gated platform diagnostics are also available as bins:
`cargo run --bin pipewire_diagnostics --features feat_linux` (Linux) and
`cargo run --bin wasapi_session_test --features feat_windows` (Windows).

### 3. `docker/` — legacy containerized harness (stale)

The `docker/` directory and the root `docker-compose*.yml` files contain an
older containerized test/cross-compile harness. **It is stale and not part of
the maintained verification path.** It is built around a `dynamic_vlc` example
that no longer exists in `examples/`, and it predates the Blacksmith CI above.
Prefer the two CI workflows and the local hardware guide; treat anything under
`docker/` as unmaintained until it is either updated or removed.

---

## Where to look

| I want to… | Go to |
|---|---|
| See exactly which audio test cells are REAL / skipped and why | [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) |
| Test on my own Windows / macOS / Linux machine | [`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md) |
| Understand the CI runner fleet & the Windows-no-audio finding | AGENTS.md §6 |
| Debug a cross-compilation / wrong-backend build | [`docs/troubleshooting.md`](troubleshooting.md) |
| Read the macOS TCC / version compatibility landscape | [`docs/MACOS_VERSION_COMPATIBILITY.md`](MACOS_VERSION_COMPATIBILITY.md) |

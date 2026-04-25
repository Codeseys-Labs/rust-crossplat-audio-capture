# Alpine musl + PipeWire linkage validation (rsac#19)

Manual, push-button validation of whether rsac's PipeWire backend
actually dlopens at runtime on Alpine when built against musl. This is
**deliberately not wired into regular CI** — it's a gate that runs
before promoting any of the experimental musl napi-rs targets to
"supported".

## When to run this

Run this before promoting any of the following napi-rs rows from
**experimental** to **supported**:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `armv7-unknown-linux-gnueabihf` (glibc, but same "dlopen works?" story
  on non-Debian distros)

The trigger is usually: a downstream user files an issue like
"`@rsac/napi-linux-x64-musl` installs fine but throws at
`getDeviceEnumerator()`". Before that happens, we don't spend CI minutes
on it.

Do NOT run this as part of every PR. It's slow (full musl cross-compile
in a container) and the answer doesn't change between unrelated commits.

## How to run it

From the repo root:

```bash
docker build \
    -f ci/alpine-musl-validation/Dockerfile \
    -t rsac-alpine-test \
    .

docker run --rm --privileged rsac-alpine-test
```

`--privileged` is required because PipeWire needs access to the
session's runtime dir and some kernel interfaces the default Docker
sandbox blocks. If you're running on a host where `--privileged` is not
acceptable, the narrower form is:

```bash
docker run --rm \
    --cap-add=SYS_NICE \
    --tmpfs /run/user/1000:exec,mode=0700,uid=1000 \
    rsac-alpine-test
```

The entrypoint is `smoke-test.sh`; it builds the smoke binary, launches
PipeWire + wireplumber + pipewire-pulse the same way `.github/workflows/
ci-audio-tests.yml` does in its Linux jobs, creates a null-sink, then
runs the binary.

## What success / failure means

- **Exit 0, log line `SUCCESS: rsac enumerated devices on Alpine musl`**

  `libpipewire-0.3.so.0` was found by the musl-linked binary, the
  PipeWire daemon accepted the connection, and `enumerate_devices()`
  returned without error. Safe to flip the relevant musl row from
  experimental to supported in `bindings/rsac-napi/package.json` and
  the release workflow matrices.

- **Exit non-zero on Phase 4**

  The build succeeded but runtime linkage or connection failed. The
  most common culprits, in order:

  1. Alpine ships `libpipewire-0.3.so.0` at a path the musl dynamic
     loader can't find. Inspect the image with
     `docker run --rm -it --entrypoint sh rsac-alpine-test` and check
     `apk info -L pipewire | grep -E '\.so'`.
  2. `pipewire-dev` metadata on Alpine emits pkg-config flags that
     assume glibc; rebuild with verbose linker output
     (`RUSTFLAGS="-C link-arg=-Wl,--verbose"`).
  3. `wireplumber` didn't come up in time; extend the `sleep` values in
     `smoke-test.sh` Phase 2.

  Do **not** promote the musl rows until Phase 4 is green.

- **Exit non-zero on Phase 1**

  The musl build itself failed under zigbuild. Usually a bindgen /
  libclang issue (check that `clang-dev` and `llvm-dev` are still in
  the Dockerfile) or a pkg-config-sys compatibility change on Alpine.
  Fix the build before worrying about runtime linkage.

## Why this isn't in regular CI

1. **Cost**: full Alpine + zigbuild run is ~4-6 minutes per invocation,
   with no caching between PRs because the base image is an ephemeral
   container.
2. **Signal-to-noise**: the thing this test guards against changes on
   Alpine's release cadence, not ours. Running it per-PR would be
   pure overhead almost every time.
3. **Scope**: the matrix also includes armv7 and aarch64 musl variants
   that require QEMU user-mode emulation to cover fully; adding those
   belongs in a future extension of this directory rather than the
   regular CI loop.

See issue rsac#19 for the original proposal and rationale.

## Files

- `Dockerfile` — Alpine 3.19 base with PipeWire runtime + dev deps,
  rustup at the channel pinned in `/rust-toolchain.toml`, and
  `cargo-zigbuild`.
- `smoke-test.sh` — build + daemon-launch + null-sink + run payload;
  this is the container entrypoint.
- `../../src/bin/smoke_alpine.rs` — the tiny Rust binary that calls
  `rsac::get_device_enumerator().enumerate_devices()`. Gated by
  `required-features = ["feat_linux"]` in `Cargo.toml`.

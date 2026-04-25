# Alpine musl + PipeWire runtime-linkage validation image for rsac.
#
# This image answers: "If we ship musl napi-rs wheels, does a statically-
# compiled musl rsac binary successfully connect to and enumerate PipeWire
# on Alpine at runtime?"
#
# The `pipewire` crate is LINKED (not dlopen'd) against libpipewire-0.3, so
# the build-time question is settled by pkg-config. The runtime question
# worth asking is whether the resulting musl binary can actually talk to
# Alpine's PipeWire daemon — Alpine's packaging uses different sonames and
# search paths than Debian/Ubuntu, and musl's dynamic-loader behavior is
# subtly different from glibc's. This image exercises that end-to-end
# runtime path.
#
# It is NOT wired into regular CI. Operators run it manually before
# promoting any of the experimental musl napi-rs targets:
#   - x86_64-unknown-linux-musl
#   - aarch64-unknown-linux-musl
#   - armv7-unknown-linux-gnueabihf  (gnu, but same fragile dlopen story)
# See ./README.md for the full runbook and ./smoke-test.sh for the payload.

# Pinned to 3.19 to match what downstream users on current Alpine releases
# are most likely to hit. Bump deliberately when a newer LTS is common.
FROM alpine:3.19

# ---- System deps --------------------------------------------------------
# Build tools: we need a real C toolchain because pipewire-sys / libspa-sys
# trigger bindgen against the system headers. `clang-dev` supplies the
# libclang.so that bindgen dlopens at build time; `musl-dev` gives us the
# musl headers cargo will link against.
#
# PipeWire split into "dev" (headers + .pc files for pkg-config at build
# time) and runtime packages (daemon + wireplumber + pipewire-pulse) so the
# smoke test can actually start a session bus and enumerate devices.
#
# `zig` is installed so we can use cargo-zigbuild for musl cross-compiles
# the same way the napi-rs release workflow does — keeps the smoke-test
# build path as close to the shipped artifact path as possible.
RUN apk add --no-cache \
      # Build toolchain
      build-base \
      musl-dev \
      clang-dev \
      llvm-dev \
      pkgconf \
      git \
      curl \
      bash \
      ca-certificates \
      # Zig for cargo-zigbuild
      zig \
      # PipeWire dev headers (pkg-config at build time)
      pipewire-dev \
      # PipeWire runtime stack (for the smoke test's dlopen path)
      pipewire \
      pipewire-tools \
      pipewire-pulse \
      wireplumber \
      # pactl, for creating the null-sink the smoke test listens on
      pulseaudio-utils

# ---- Rust toolchain -----------------------------------------------------
# Match the pinned channel in /rust-toolchain.toml (currently 1.95.0).
# Using rustup rather than apk's `rust` package so we get exact-version
# control and can add musl targets.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    RUST_VERSION=1.95.0

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal \
                  --default-toolchain "${RUST_VERSION}" \
                  --component clippy rustfmt \
 && rustup target add x86_64-unknown-linux-musl \
 && cargo install --locked cargo-zigbuild

# ---- Non-root runtime ---------------------------------------------------
# PipeWire refuses to start as root. Create an unprivileged user and a
# writable XDG_RUNTIME_DIR for the session socket.
RUN adduser -D -u 1000 rsac \
 && mkdir -p /run/user/1000 \
 && chown -R rsac:rsac /run/user/1000 \
 && chmod 700 /run/user/1000

# ---- Project source -----------------------------------------------------
# Expected to be run with the repo bind-mounted at /workspace, OR copied in
# by the build. We default to copy so `docker run --rm rsac-alpine-test`
# works standalone without a mount.
WORKDIR /workspace
COPY --chown=rsac:rsac . /workspace/

USER rsac
ENV XDG_RUNTIME_DIR=/run/user/1000 \
    CARGO_TERM_COLOR=always \
    RUST_BACKTRACE=1 \
    RUST_LOG=rsac=debug

# The smoke script handles: build -> daemon launch -> null-sink -> run.
ENTRYPOINT ["/workspace/ci/alpine-musl-validation/smoke-test.sh"]

#!/usr/bin/env bash
#
# record-build-info.sh — pragmatic build-provenance recorder (rsac-8d55).
#
# Release builds install OS packages (PipeWire dev libs, cross toolchains) from
# rolling apt repositories, so the exact versions that linked a given release
# artifact are otherwise lost. This script snapshots them — the toolchain, the
# host OS, and the installed version of every package we care about — into the
# job log AND an appendable build-info file, which the calling workflow uploads
# as a release artifact. It is provenance RECORDING, not apt pinning: it never
# fails a build (a missing package is simply noted "not installed"), it just
# makes "what versions built this?" answerable after the fact.
#
# Usage:
#   scripts/record-build-info.sh <build-info-file> [pkg ...]
#
#   <build-info-file>   file to append the snapshot to (created if absent).
#   [pkg ...]           extra dpkg package names to record on top of the
#                       always-recorded defaults (PipeWire/SPA/pkg-config).
#
# Always best-effort: exits 0 even if a tool or package is absent.

set -uo pipefail   # NOT -e: recording must never fail the release.

OUT="${1:-}"
if [ -z "$OUT" ]; then
    echo "usage: scripts/record-build-info.sh <build-info-file> [pkg ...]" >&2
    exit 2
fi
shift || true

# Default packages every Linux release leg installs; callers append cross
# toolchains (gcc-aarch64-linux-gnu, gcc-arm-linux-gnueabihf, …).
DEFAULT_PKGS="libpipewire-0.3-dev libspa-0.2-dev pkg-config"

{
    echo "==================================================================="
    echo "build-info snapshot: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "  job:      ${GITHUB_JOB:-<local>}"
    echo "  workflow: ${GITHUB_WORKFLOW:-<local>}"
    echo "  runner:   ${RUNNER_OS:-$(uname -s)} ${RUNNER_ARCH:-$(uname -m)}"
    echo "  ref:      ${GITHUB_REF:-<local>}  sha: ${GITHUB_SHA:-<local>}"

    # Host OS.
    if [ -r /etc/os-release ]; then
        # shellcheck disable=SC1091
        . /etc/os-release
        echo "  os:       ${PRETTY_NAME:-unknown}"
    else
        echo "  os:       $(uname -sr)"
    fi

    # Rust toolchain (the versions that actually compiled the crate/binding).
    if command -v rustc >/dev/null 2>&1; then echo "  rustc:    $(rustc --version)"; fi
    if command -v cargo >/dev/null 2>&1; then echo "  cargo:    $(cargo --version)"; fi

    # OS packages, via dpkg-query where available (Debian/Ubuntu runners).
    echo "  packages:"
    if command -v dpkg-query >/dev/null 2>&1; then
        for pkg in $DEFAULT_PKGS "$@"; do
            ver="$(dpkg-query -W -f='${Version}' "$pkg" 2>/dev/null || true)"
            if [ -n "$ver" ]; then
                echo "    ${pkg} = ${ver}"
            else
                echo "    ${pkg} = <not installed>"
            fi
        done
    else
        echo "    <dpkg-query unavailable — non-Debian runner; OS package versions not recorded>"
    fi
    echo
} | tee -a "$OUT"

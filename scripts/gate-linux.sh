#!/usr/bin/env bash
# scripts/gate-linux.sh — devcontainer-backed feat_linux lint-leg replica for
# non-Linux hosts (rsac-ef88).
#
# scripts/gate.sh only ever lints the HOST's platform feature (uname-derived),
# so a Windows/macOS contributor's pre-push gate is structurally blind to
# feat_linux-only breaks — ci.yml's `lint` matrix runs feat_linux/windows/macos
# as three independent legs, but locally only one of the three is reachable.
# This script closes that gap by running run_lint()'s fmt + clippy steps
# (NOT its bare-build or cargo-doc steps — those are target_os-independent and
# gate.sh already runs them on the host), forced to feat_linux, inside the
# existing docker/linux/Dockerfile.test image (the same image .devcontainer/
# builds). The clippy/fmt invocation is byte-identical to ci.yml's
# `lint (feat_linux)` leg.
#
# Usage:
#   bash scripts/gate-linux.sh            # build (if needed) + run; skips
#                                          # gracefully with an actionable
#                                          # message if Docker is unavailable
#   bash scripts/gate-linux.sh --rebuild  # force a fresh image build
#
# Keep the clippy/fmt invocation in lockstep with scripts/gate.sh's run_lint()
# and ci.yml's `lint` job's `feat_linux` leg. If you change one, change both.
#
# Named-volume caches (rsac-gate-linux-cargo-registry / -target) persist
# across runs on this host for fast warm re-runs; if disk is tight, remove
# them with `docker volume rm rsac-gate-linux-cargo-registry rsac-gate-linux-target`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

IMAGE_TAG="rsac-gate-linux"
REBUILD=0
case "${1:-}" in
  --rebuild) REBUILD=1 ;;
  "") ;;
  *) echo "gate-linux: unknown flag '$1' (expected --rebuild)" >&2; exit 1 ;;
esac

step() { printf '\n\033[1m── gate:linux: %s\033[0m\n' "$1"; }

# ── Graceful skip: no Docker, no gate:linux — never fail the caller ─────────
if ! command -v docker >/dev/null 2>&1; then
  cat >&2 <<'EOF'
gate-linux: [skip] no `docker` on PATH.

gate:linux replicates ci.yml's feat_linux lint leg on non-Linux hosts by
running clippy/fmt inside the same devcontainer image (docker/linux/Dockerfile.test).
Install Docker (or OrbStack/Docker Desktop) to run it locally, or rely on CI's
`lint (feat_linux)` job to catch feat_linux-only breaks.
EOF
  exit 0
fi
if ! docker info >/dev/null 2>&1; then
  echo "gate-linux: [skip] docker CLI present but the daemon is not running (start Docker Desktop/OrbStack)." >&2
  exit 0
fi

# ── Build-or-reuse the image ─────────────────────────────────────────────────
step "docker build (${IMAGE_TAG})"
if [ "${REBUILD}" -eq 1 ] || ! docker image inspect "${IMAGE_TAG}" >/dev/null 2>&1; then
  docker build -f docker/linux/Dockerfile.test -t "${IMAGE_TAG}" .
else
  echo "gate-linux: reusing existing ${IMAGE_TAG} image (pass --rebuild to force a fresh build)"
fi

# ── Run the lint replica in-container ────────────────────────────────────────
# Named volumes (not bind mounts) for the cargo registry + target dir: they
# persist across runs on the SAME host (fast warm re-runs) without polluting
# the host's own ~/.cargo or target/ (this container's rustc/glibc triple
# differs from the host's). The workspace itself IS bind-mounted read-write
# so clippy sees uncommitted changes and `cargo fmt --check` sees the same
# tree the caller is about to push.
step "clippy -D warnings + fmt --check (feat_linux,compose,cli) inside ${IMAGE_TAG}"
docker run --rm \
  -v "${REPO_ROOT}:/workspace" \
  -v rsac-gate-linux-cargo-registry:/root/.cargo/registry \
  -v rsac-gate-linux-target:/workspace/target \
  -w /workspace \
  "${IMAGE_TAG}" \
  bash -lc '
    set -euo pipefail
    source "$HOME/.cargo/env"
    rustup component add clippy rustfmt >/dev/null 2>&1 || true
    cargo fmt --all -- --check
    cargo clippy --all-targets --no-default-features --features feat_linux,compose,cli -- -D warnings
  '

printf '\n\033[1;32mgate-linux: OK (feat_linux)\033[0m\n'

#!/usr/bin/env bash
# scripts/gate.sh — the local gate: a faithful replica of ci.yml's `lint` job
# for the host OS, with opt-in extensions that replay the heavier jobs.
#
# CI is the backstop; this is the fast local feedback loop (rsac-7e19).
# Invoked by: `mise run gate`, lefthook's pre-push hook, or directly.
# Works on Linux, macOS, and Windows (via Git for Windows' bash).
#
# Usage:
#   bash scripts/gate.sh              # lint-job replica: fmt + clippy -D warnings + bare build
#   bash scripts/gate.sh --full       # + lib tests, doctests, cargo doc, module-DAG guard
#   bash scripts/gate.sh --tests-only # just the test-job replica for the host OS
#
# Keep the commands here in lockstep with .github/workflows/ci.yml — the
# lint job (fmt / clippy / bare build) and the per-OS test jobs. If you
# change one, change the other.
set -euo pipefail

# ── Host backend feature (mirrors ci.yml's lint matrix) ────────────────
case "$(uname -s)" in
  Linux*)                    FEAT="feat_linux" ;;
  Darwin*)                   FEAT="feat_macos" ;;
  MINGW*|MSYS*|CYGWIN*)      FEAT="feat_windows" ;;
  *) echo "gate: unknown host OS '$(uname -s)' — cannot pick a backend feature" >&2; exit 1 ;;
esac

MODE="lint"
case "${1:-}" in
  --full)       MODE="full" ;;
  --tests-only) MODE="tests" ;;
  "")           ;;
  *) echo "gate: unknown flag '$1' (expected --full or --tests-only)" >&2; exit 1 ;;
esac

step() { printf '\n\033[1m── gate: %s\033[0m\n' "$1"; }

run_lint() {
  # ci.yml lint job, verbatim commands (fmt runs on the Linux leg in CI;
  # rustfmt output is OS-independent, so locally we always run it).
  step "cargo fmt --all -- --check"
  cargo fmt --all -- --check

  step "clippy -D warnings (CI replica: ${FEAT},compose,cli)"
  cargo clippy --all-targets --no-default-features --features "${FEAT},compose,cli" -- -D warnings

  step "bare-build smoke (cargo build --no-default-features)"
  cargo build --no-default-features
}

run_tests() {
  # Per-OS test-job replica (ci.yml test-linux/-windows/-macos core commands).
  step "lib tests (${FEAT},compose)"
  cargo test --lib --no-default-features --features "${FEAT},compose"

  step "doctests (${FEAT},compose,sink-wav,cli)"
  cargo test --doc --no-default-features --features "${FEAT},compose,sink-wav,cli"
}

run_extras() {
  # docs job replica.
  step "cargo doc (docsrs cfg, -D warnings)"
  RUSTDOCFLAGS="--cfg docsrs -D warnings" cargo doc --no-deps --all-features

  # module-dag job replica.
  step "module-DAG reverse-edge guard"
  bash scripts/check-module-dag.sh
}

case "$MODE" in
  lint)  run_lint ;;
  tests) run_tests ;;
  full)  run_lint; run_tests; run_extras ;;
esac

printf '\n\033[1;32mgate: OK (%s, %s)\033[0m\n' "$MODE" "$FEAT"

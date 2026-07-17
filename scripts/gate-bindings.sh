#!/usr/bin/env bash
# scripts/gate-bindings.sh — local replica of ci.yml's `check-bindings`
# ("Binding Crates") job, for the host OS (rsac-f9c1).
#
# scripts/gate.sh never touches bindings/ — its lint-job replica only covers
# the root `rsac` crate's default workspace member. That left the FFI/napi/
# python legs with no local feedback loop at all (the CI failure post-mortem
# for run 28762700151: a napi smoke unhandled-rejection break had no possible
# local catch). This script closes that gap without folding into gate.sh,
# because it needs a polyglot toolchain (Python venv + maturin, Node/Bun +
# napi-rs) gate.sh's pure-Rust legs do not.
#
# Usage:
#   bash scripts/gate-bindings.sh          # every leg; missing toolchains skip
#   bash scripts/gate-bindings.sh --strict # missing toolchains FAIL instead of skip
#
# Each leg is independent and best-effort: a leg whose toolchain is missing
# on this host prints a "skip:" line and moves on (unless --strict), so this
# script degrades gracefully on a Rust-only dev box while still giving full
# coverage on a fully-provisioned one. `mise install` provisions Bun/Node/Go/
# Python; maturin and the napi-rs CLI are project-local (pip/npm), not mise
# tools, so they get their own presence checks.
#
# Keep the commands here in lockstep with ci.yml's `check-bindings` job. If
# you change one, change the other.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

STRICT=0
case "${1:-}" in
  --strict) STRICT=1 ;;
  "") ;;
  *) echo "gate-bindings: unknown flag '$1' (expected --strict)" >&2; exit 1 ;;
esac

FAILED=0

step()  { printf '\n\033[1m── gate:bindings: %s\033[0m\n' "$1"; }
skip()  {
  if [ "$STRICT" -eq 1 ]; then
    echo "gate-bindings: [FAIL, --strict] $1" >&2
    FAILED=1
  else
    echo "gate-bindings: [skip] $1"
  fi
}

# ── rsac-ffi: check + clippy, bare AND --features compose ──────────────
step "cargo check -p rsac-ffi (bare)"
cargo check -p rsac-ffi

step "cargo check -p rsac-ffi (--features compose)"
cargo check -p rsac-ffi --features compose

step "clippy binding crates (-D warnings)"
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo clippy -p rsac-ffi --all-targets -- -D warnings
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo clippy -p rsac-ffi --all-targets --features compose -- -D warnings
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo clippy -p rsac-napi --all-targets -- -D warnings
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo clippy -p rsac-python --all-targets -- -D warnings

step "cargo test -p rsac-ffi (bare)"
cargo test -p rsac-ffi

step "cargo test -p rsac-ffi (--features compose)"
cargo test -p rsac-ffi --features compose

step "cargo check -p rsac-napi"
cargo check -p rsac-napi

step "cargo check -p rsac-python"
PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 cargo check -p rsac-python

# ── Header regen byte-determinism diff (ci.yml: "Header drift check") ──
# Mirrors the CI job's two builds + diff exactly (see ci.yml's comment for
# why this is a SYMBOL SET diff for curated-vs-generated but a byte diff for
# bare-vs-compose determinism).
step "header regen byte-determinism (bare vs --features compose)"
# `mktemp -t <prefix>` is not portable: BSD/macOS treat the arg as a filename
# prefix and append random chars, but GNU/Linux treat it as a template that
# MUST contain X's ("too few X's in template" → exit 1, aborting under set -e).
# Use an explicit XXXXXX template so both implementations agree.
BARE_HEADER="$(mktemp "${TMPDIR:-/tmp}/rsac_generated_bare.XXXXXX")"
cargo build -p rsac-ffi
cp bindings/rsac-ffi/include/rsac_generated.h "$BARE_HEADER"
cargo build -p rsac-ffi --features compose
if ! diff -u "$BARE_HEADER" bindings/rsac-ffi/include/rsac_generated.h; then
  echo "gate-bindings: include/rsac_generated.h differs between the bare and --features compose builds (must be feature-deterministic, see cbindgen.toml [defines])." >&2
  FAILED=1
fi
rm -f "$BARE_HEADER"

step "header symbol-set diff (curated rsac.h vs generated rsac_generated.h)"
(
  cd bindings/rsac-ffi/include
  CURATED="rsac.h"
  GENERATED="rsac_generated.h"
  symbols() {
    local f="$1"
    {
      grep -oE '^\}[[:space:]]*[A-Za-z_][A-Za-z0-9_]*[[:space:]]*;' "$f" \
        | sed -E 's/^\}[[:space:]]*//; s/[[:space:]]*;//'
      grep -oE 'typedef[[:space:]]+struct[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*;' "$f" \
        | sed -E 's/.*[[:space:]]([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*;/\1/'
      grep -oE '\(\*[A-Za-z_][A-Za-z0-9_]*\)' "$f" | sed -E 's/\(\*//; s/\)//'
      grep -oE '\brsac_[A-Za-z0-9_]*\(' "$f" | sed -E 's/\($//'
    } | sort -u
  }
  symbols "$CURATED" > /tmp/rsac-curated-symbols.txt
  symbols "$GENERATED" > /tmp/rsac-generated-symbols.txt
  # Guard against an extractor that silently matched nothing (mirrors ci.yml):
  # `diff -u` of two empty files exits 0, so without this a header-format
  # change that breaks the regexes would pass this leg locally while CI fails.
  if [ ! -s /tmp/rsac-curated-symbols.txt ] || [ ! -s /tmp/rsac-generated-symbols.txt ]; then
    echo "gate-bindings: symbol extraction yielded an empty set — the header format may have changed; fix the extractor here and in ci.yml." >&2
    exit 1
  fi
  diff -u /tmp/rsac-curated-symbols.txt /tmp/rsac-generated-symbols.txt
) || { echo "gate-bindings: curated rsac.h has drifted from the generated header's symbol set." >&2; FAILED=1; }
rm -f /tmp/rsac-curated-symbols.txt /tmp/rsac-generated-symbols.txt

# ── napi leg: build + node --test (needs Bun or npm, from mise) ────────
step "napi build + node --test"
if command -v bun >/dev/null 2>&1; then
  NAPI_PKG_MGR=(bun)
elif command -v npm >/dev/null 2>&1; then
  NAPI_PKG_MGR=(npm)
else
  skip "napi leg — neither bun nor npm on PATH (mise install provisions bun)"
  NAPI_PKG_MGR=()
fi
if [ "${#NAPI_PKG_MGR[@]}" -gt 0 ]; then
  (
    cd bindings/rsac-napi
    "${NAPI_PKG_MGR[@]}" install --no-fund --no-audit 2>/dev/null || "${NAPI_PKG_MGR[@]}" install
    "${NAPI_PKG_MGR[@]}" run build
    # `run test` (not bare `test`): bun's bare `bun test` invokes bun's OWN
    # native test runner and ignores package.json's "test" script entirely,
    # which would silently skip the `node --test` invocation ci.yml's `npm
    # test` actually runs. `run test` resolves the package.json script on
    # both npm and bun, keeping this leg honest to what CI executes.
    "${NAPI_PKG_MGR[@]}" run test
  ) || FAILED=1
  # napi build regenerates index.d.ts/index.js from the compiled addon's
  # symbol table; the checked-in copies are hand-curated (JSDoc prose differs
  # from napi-rs's auto-generated comments byte-for-byte). Never let a local
  # gate run silently leave the working tree dirty with regenerated bindings.
  git -C bindings/rsac-napi checkout -- index.d.ts index.js 2>/dev/null || true
fi

# ── python leg: maturin develop + import smoke (needs a venv + maturin) ─
step "python binding smoke (maturin develop + import smoke)"
if command -v python3 >/dev/null 2>&1; then
  VENV_DIR="$REPO_ROOT/.venv-smoke"
  if ! python3 -m venv "$VENV_DIR"; then
    skip "python leg — 'python3 -m venv' failed (missing venv/ensurepip module?)"
  else
    # shellcheck disable=SC1091
    source "$VENV_DIR/bin/activate"
    if ! python -m pip install --quiet maturin; then
      skip "python leg — could not install maturin into $VENV_DIR (offline / no PyPI access?)"
    else
      PYO3_USE_ABI3_FORWARD_COMPATIBILITY=1 maturin develop -m bindings/rsac-python/Cargo.toml \
        && python bindings/rsac-python/tests/smoke.py \
        || FAILED=1
    fi
    deactivate 2>/dev/null || true
  fi
else
  skip "python leg — no python3 on PATH (mise install provisions python 3.12)"
fi

if [ "$FAILED" -ne 0 ]; then
  printf '\n\033[1;31mgate:bindings: FAILED\033[0m\n'
  exit 1
fi
printf '\n\033[1;32mgate:bindings: OK\033[0m\n'

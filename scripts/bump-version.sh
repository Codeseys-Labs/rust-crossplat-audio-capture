#!/usr/bin/env bash
#
# Bump the rsac workspace version across the five files that must agree:
#   - Cargo.toml                              (root `rsac` crate)
#   - bindings/rsac-napi/Cargo.toml           (napi crate)
#   - bindings/rsac-napi/package.json         (npm package)
#   - bindings/rsac-python/Cargo.toml         (pyo3 crate)
#   - bindings/rsac-python/pyproject.toml     (python package)
#
# Also rotates CHANGELOG.md: the current "## [Unreleased]" section becomes
# "## [X.Y.Z] - YYYY-MM-DD" and a fresh Unreleased scaffold is inserted.
#
# Usage:
#   bash scripts/bump-version.sh <X.Y.Z> [--dry-run]
#
# Intentionally dumb — no git automation, no tagging. Caller reviews the
# diff, commits, and tags by hand:
#
#   bash scripts/bump-version.sh 0.3.0
#   git diff
#   git add -A && git commit -m "chore: release 0.3.0"
#   git tag -a v0.3.0 -m "Release 0.3.0"
#   git push origin master v0.3.0

set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────
if [ -t 1 ] && [ "${NO_COLOR:-}" = "" ]; then
    RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLW=$'\033[0;33m'
    BLU=$'\033[0;34m'; DIM=$'\033[0;90m'; RST=$'\033[0m'
else
    RED=""; GRN=""; YLW=""; BLU=""; DIM=""; RST=""
fi
info()  { printf '%s[info]%s %s\n'  "$BLU" "$RST" "$*"; }
ok()    { printf '%s[ok]%s %s\n'    "$GRN" "$RST" "$*"; }
warn()  { printf '%s[warn]%s %s\n'  "$YLW" "$RST" "$*" >&2; }
err()   { printf '%s[err]%s %s\n'   "$RED" "$RST" "$*" >&2; }
plan()  { printf '%s  would change:%s %s\n' "$DIM" "$RST" "$*"; }

# ── args ─────────────────────────────────────────────────────────────
DRY_RUN=0
NEW_VERSION=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        -*)
            err "unknown flag: $arg"
            exit 1
            ;;
        *)
            if [ -n "$NEW_VERSION" ]; then
                err "unexpected extra argument: $arg"
                exit 1
            fi
            NEW_VERSION="$arg"
            ;;
    esac
done

if [ -z "$NEW_VERSION" ]; then
    err "usage: bash scripts/bump-version.sh <X.Y.Z> [--dry-run]"
    exit 1
fi

# Semver shape check — major.minor.patch with optional pre-release.
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    err "version '$NEW_VERSION' doesn't match X.Y.Z[-prerelease]"
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ROOT_CARGO="Cargo.toml"
NAPI_CARGO="bindings/rsac-napi/Cargo.toml"
NAPI_PKG="bindings/rsac-napi/package.json"
PY_CARGO="bindings/rsac-python/Cargo.toml"
PY_PYPROJ="bindings/rsac-python/pyproject.toml"
CHANGELOG="CHANGELOG.md"

for f in "$ROOT_CARGO" "$NAPI_CARGO" "$NAPI_PKG" "$PY_CARGO" "$PY_PYPROJ" "$CHANGELOG"; do
    [ -f "$f" ] || { err "missing required file: $f"; exit 1; }
done

# ── helpers ──────────────────────────────────────────────────────────
# Read the current [package].version from a Cargo.toml / pyproject.toml.
# Only matches the first `version = "..."` that appears *inside* the
# `[package]` or `[project]` section so nested deps don't confuse it.
cargo_package_version() {
    awk '
        /^\[package\]/ || /^\[project\]/ { in_sec = 1; next }
        /^\[/ && !/^\[package\]/ && !/^\[project\]/ { in_sec = 0 }
        in_sec && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
            match($0, /"[^"]+"/)
            v = substr($0, RSTART + 1, RLENGTH - 2)
            print v
            exit
        }
    ' "$1"
}

# Read the top-level "version" from package.json. Assumes the canonical
# npm-written format with "version" as a top-level key.
json_version() {
    awk '
        /^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"[^"]+"/ {
            match($0, /"[^"]+"[[:space:]]*,?[[:space:]]*$/)
            v = substr($0, RSTART + 1, RLENGTH - 2)
            # strip trailing quote/comma/whitespace
            sub(/["[:space:],]+$/, "", v)
            print v
            exit
        }
    ' "$1"
}

# In-place rewrite of [package]/[project] version in a TOML file. Only the
# first match inside the section is touched — transitive `version = "..."`
# on dep entries is left alone.
rewrite_cargo_version() {
    local file="$1" new="$2"
    awk -v new="$new" '
        BEGIN { in_sec = 0; done = 0 }
        /^\[package\]/ || /^\[project\]/ { in_sec = 1; print; next }
        /^\[/ && !/^\[package\]/ && !/^\[project\]/ { in_sec = 0 }
        in_sec && !done && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
            sub(/"[^"]+"/, "\"" new "\"")
            done = 1
        }
        { print }
    ' "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
}

# In-place rewrite of the top-level JSON "version" key. package.json has
# a nested "scripts"."version" key ("napi version") that shares the same
# line shape, so we can't just sed-replace every match — instead we rewrite
# only the first `"version"` line whose indentation is exactly 2 spaces
# (the top-level depth that npm/bun emit).
rewrite_json_version() {
    local file="$1" new="$2"
    awk -v new="$new" '
        BEGIN { done = 0 }
        !done && /^  "version"[[:space:]]*:[[:space:]]*"[^"]+"/ {
            # Rewrite only the value (second quoted string on the line),
            # preserving any trailing comma and whitespace.
            colon = index($0, ":")
            prefix = substr($0, 1, colon)
            rest = substr($0, colon + 1)
            sub(/"[^"]+"/, "\"" new "\"", rest)
            $0 = prefix rest
            done = 1
        }
        { print }
    ' "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
}

# ── plan ─────────────────────────────────────────────────────────────
CUR_ROOT=$(cargo_package_version "$ROOT_CARGO")
CUR_NAPI_CARGO=$(cargo_package_version "$NAPI_CARGO")
CUR_NAPI_PKG=$(json_version "$NAPI_PKG")
CUR_PY_CARGO=$(cargo_package_version "$PY_CARGO")
CUR_PY_PYPROJ=$(cargo_package_version "$PY_PYPROJ")

info "target version:  $NEW_VERSION"
info "current versions:"
printf '  %-42s %s\n' "$ROOT_CARGO"  "$CUR_ROOT"
printf '  %-42s %s\n' "$NAPI_CARGO"  "$CUR_NAPI_CARGO"
printf '  %-42s %s\n' "$NAPI_PKG"    "$CUR_NAPI_PKG"
printf '  %-42s %s\n' "$PY_CARGO"    "$CUR_PY_CARGO"
printf '  %-42s %s\n' "$PY_PYPROJ"   "$CUR_PY_PYPROJ"

# Idempotency guard: if every target already matches, exit cleanly without
# touching the changelog either (rotating a changelog that's already been
# rotated would create a duplicate ## [X.Y.Z] header).
if [ "$CUR_ROOT" = "$NEW_VERSION" ] && \
   [ "$CUR_NAPI_CARGO" = "$NEW_VERSION" ] && \
   [ "$CUR_NAPI_PKG" = "$NEW_VERSION" ] && \
   [ "$CUR_PY_CARGO" = "$NEW_VERSION" ] && \
   [ "$CUR_PY_PYPROJ" = "$NEW_VERSION" ]; then
    ok "already at version $NEW_VERSION — nothing to do"
    exit 0
fi

# Plan per-file changes.
CHANGES=()
[ "$CUR_ROOT" != "$NEW_VERSION" ]       && CHANGES+=("$ROOT_CARGO")
[ "$CUR_NAPI_CARGO" != "$NEW_VERSION" ] && CHANGES+=("$NAPI_CARGO")
[ "$CUR_NAPI_PKG" != "$NEW_VERSION" ]   && CHANGES+=("$NAPI_PKG")
[ "$CUR_PY_CARGO" != "$NEW_VERSION" ]   && CHANGES+=("$PY_CARGO")
[ "$CUR_PY_PYPROJ" != "$NEW_VERSION" ]  && CHANGES+=("$PY_PYPROJ")

# Changelog rotation is planned if there's an "## [Unreleased]" section
# *and* no "## [$NEW_VERSION]" header already exists (idempotent).
CHANGELOG_ROTATE=0
if grep -q '^## \[Unreleased\]' "$CHANGELOG" && \
   ! grep -qE "^## \[$(printf '%s' "$NEW_VERSION" | sed 's/[.]/\\./g')\]" "$CHANGELOG"; then
    CHANGELOG_ROTATE=1
    CHANGES+=("$CHANGELOG (rotate Unreleased → [$NEW_VERSION])")
fi

info "planned changes (${#CHANGES[@]}):"
for c in "${CHANGES[@]}"; do
    plan "$c"
done

if [ "$DRY_RUN" -eq 1 ]; then
    warn "dry-run: no files written"
    exit 0
fi

# ── apply ────────────────────────────────────────────────────────────
[ "$CUR_ROOT"       != "$NEW_VERSION" ] && rewrite_cargo_version "$ROOT_CARGO"  "$NEW_VERSION"
[ "$CUR_NAPI_CARGO" != "$NEW_VERSION" ] && rewrite_cargo_version "$NAPI_CARGO"  "$NEW_VERSION"
[ "$CUR_NAPI_PKG"   != "$NEW_VERSION" ] && rewrite_json_version  "$NAPI_PKG"    "$NEW_VERSION"
[ "$CUR_PY_CARGO"   != "$NEW_VERSION" ] && rewrite_cargo_version "$PY_CARGO"    "$NEW_VERSION"
[ "$CUR_PY_PYPROJ"  != "$NEW_VERSION" ] && rewrite_cargo_version "$PY_PYPROJ"   "$NEW_VERSION"

# ── changelog rotation ───────────────────────────────────────────────
if [ "$CHANGELOG_ROTATE" -eq 1 ]; then
    # Portable UTC date across BSD (macOS) and GNU (Linux).
    TODAY=$(date -u +%Y-%m-%d)

    # awk-only rotation: find "## [Unreleased]", capture body until the
    # next "## [" header (or EOF), and emit:
    #   ## [Unreleased]
    #   <empty scaffold>
    #
    #   ## [X.Y.Z] - YYYY-MM-DD
    #   <original unreleased body>
    awk -v ver="$NEW_VERSION" -v today="$TODAY" '
        BEGIN { state = "pre"; body = "" }

        # pre  : before ## [Unreleased]            — pass through
        # body : between unreleased and next ##    — accumulate
        # post : after we have emitted replacement — pass through

        state == "pre" {
            if ($0 ~ /^## \[Unreleased\]/) {
                state = "body"
                next
            }
            print
            next
        }

        state == "body" {
            if ($0 ~ /^## \[/) {
                # Finalise: strip Added/Changed/Deprecated/Removed/Fixed/Security
                # scaffolding that had no content. If nothing real remains we
                # still emit a minimal ### Added so the dated section is valid.
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", body)
                # Strip only-header scaffolds like "### Added\n### Changed\n..."
                stripped = body
                gsub(/###[[:space:]]+(Added|Changed|Deprecated|Removed|Fixed|Security)[[:space:]]*\n?/, "", stripped)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", stripped)
                printf "## [Unreleased]\n\n"
                printf "### Added\n\n"
                printf "### Changed\n\n"
                printf "### Deprecated\n\n"
                printf "### Removed\n\n"
                printf "### Fixed\n\n"
                printf "### Security\n\n"
                printf "## [%s] - %s\n\n", ver, today
                if (stripped == "") {
                    printf "### Added\n\n"
                } else {
                    printf "%s\n\n", body
                }
                state = "post"
                print
                next
            }
            body = body $0 "\n"
            next
        }

        state == "post" { print }

        END {
            # File ended while still inside Unreleased — emit anyway.
            if (state == "body") {
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", body)
                stripped = body
                gsub(/###[[:space:]]+(Added|Changed|Deprecated|Removed|Fixed|Security)[[:space:]]*\n?/, "", stripped)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", stripped)
                printf "## [Unreleased]\n\n"
                printf "### Added\n\n"
                printf "### Changed\n\n"
                printf "### Deprecated\n\n"
                printf "### Removed\n\n"
                printf "### Fixed\n\n"
                printf "### Security\n\n"
                printf "## [%s] - %s\n\n", ver, today
                if (stripped == "") {
                    printf "### Added\n\n"
                } else {
                    printf "%s\n\n", body
                }
            }
        }
    ' "$CHANGELOG" > "$CHANGELOG.tmp"
    mv "$CHANGELOG.tmp" "$CHANGELOG"
fi

# ── summary ──────────────────────────────────────────────────────────
ok "bumped to $NEW_VERSION"
echo "files changed:"
for c in "${CHANGES[@]}"; do
    printf '  - %s\n' "$c"
done
echo
echo "next steps:"
echo "  git diff"
echo "  git add -A && git commit -m \"chore: release $NEW_VERSION\""
echo "  git tag -a v$NEW_VERSION -m \"Release $NEW_VERSION\""
echo "  git push origin master v$NEW_VERSION"

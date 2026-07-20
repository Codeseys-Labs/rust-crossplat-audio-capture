#!/usr/bin/env bash
#
# check-version-lockstep.sh — the single source of truth for the rsac
# version-lockstep gate (docs/RELEASE_PROCESS.md §"Versioning & ABI contract").
#
# rsac (root Cargo.toml) and every publishable binding + integration manifest
# bump in lockstep on a semver tag. This script extracts the [package]/[project]
# version of the root crate and every lockstep manifest, PLUS the internal
# `rsac = { path = "../../", version = "…" }` dependency pin that
# scripts/bump-version.sh rewrites, and asserts they all agree.
#
# It is deliberately the ONE place that logic lives: `.github/workflows/ci.yml`'s
# `version-lockstep` job calls it (warn-only on push/PR, hard-fail on a tag), and
# the three registry publish workflows (release.yml, release-npm.yml,
# release-pypi.yml) call it with `--expect X.Y.Z` BEFORE any publish or dry-run
# upload path, so a mismatched manifest can never reach a registry. Keeping the
# extraction + comparison here means the CI job and the release workflows share
# identical semantics with zero duplication.
#
# ── Manifests checked (must match scripts/bump-version.sh) ─────────────
#   Cargo.toml                                             (root rsac crate)
#   bindings/rsac-ffi/Cargo.toml                           [package].version
#   bindings/rsac-napi/Cargo.toml                          [package].version
#   bindings/rsac-napi/package.json                        top-level "version"
#   bindings/rsac-python/Cargo.toml                        [package].version
#   bindings/rsac-python/pyproject.toml                    [project].version
#   mobile/android-native/Cargo.toml                       [package].version
#   integrations/tauri-plugin-rsac/Cargo.toml              [package].version
#   integrations/tauri-plugin-rsac/guest-js/package.json   top-level "version"
# ── Plus the internal rsac dep pin in ─────────────────────────────────
#   any of the binding/integration manifests that carry a versioned internal
#   `rsac = { path = "../..", version = "…" }` dep (today rsac-ffi AND
#   integrations/tauri-plugin-rsac — both detected dynamically below). A
#   stale pin — the bug rsac-0d58 fixed in bump-version.sh — is a divergence
#   too, so we track it here as well.
#
# ── Usage ──────────────────────────────────────────────────────────────
#   scripts/check-version-lockstep.sh [--expect X.Y.Z] [--warn-only]
#
#   (default)         every manifest + internal pin must equal the root
#                     Cargo.toml [package].version; exit 1 on any divergence.
#   --expect X.Y.Z    ALSO assert every version equals X.Y.Z (tag / release
#                     time). Implies a hard fail on any mismatch. Overrides
#                     --warn-only (a release must never merely warn).
#   --warn-only       on divergence from the root, emit a `::warning::` and exit
#                     0 instead of failing — the push/PR mid-cycle-skew tolerance
#                     (a binding may lag the root crate between releases). A
#                     manifest whose version cannot be extracted at all is ALWAYS
#                     a hard failure (fail-closed), regardless of this flag.
#   -h | --help       print this header and exit 0.
#
# Exit status: 0 = lockstep OK (or warn-only divergence); 1 = divergence /
#              tag mismatch / extraction failure; 2 = usage error.
#
# Portability: written for bash 3.2 (macOS /bin/bash) — NO associative arrays.
# Parallel indexed arrays + `IFS='=' read` style comparisons only, so it runs
# identically on a developer's Mac and the Linux CI runner.

set -euo pipefail

# ── colours (TTY only) ─────────────────────────────────────────────────
if [ -t 1 ] && [ "${NO_COLOR:-}" = "" ]; then
    RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLW=$'\033[0;33m'; RST=$'\033[0m'
else
    RED=""; GRN=""; YLW=""; RST=""
fi

# GitHub Actions annotation helpers degrade to plain prefixes off-CI.
gha_error() { printf '::error::%s\n' "$*"; printf '%s[err]%s %s\n' "$RED" "$RST" "$*" >&2; }
gha_warn()  { printf '::warning::%s\n' "$*"; printf '%s[warn]%s %s\n' "$YLW" "$RST" "$*" >&2; }

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; }

# ── args ────────────────────────────────────────────────────────────────
EXPECT=""
WARN_ONLY=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --expect)
            [ "$#" -ge 2 ] || { gha_error "--expect requires a X.Y.Z argument"; exit 2; }
            EXPECT="$2"; shift 2 ;;
        --expect=*) EXPECT="${1#--expect=}"; shift ;;
        --warn-only) WARN_ONLY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) gha_error "unknown argument: $1"; usage >&2; exit 2 ;;
    esac
done

if [ -n "$EXPECT" ] && ! printf '%s' "$EXPECT" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    gha_error "--expect value '$EXPECT' is not a stable X.Y.Z semver version"
    exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ── extractors (mirror scripts/bump-version.sh + ci.yml) ────────────────
# First `version = "X.Y.Z"` inside the [package] / [project] section of a TOML.
toml_pkg_version() {
    awk '
        /^\[package\][[:space:]]*$/ || /^\[project\][[:space:]]*$/ { in_sec = 1; next }
        /^\[/ { if ($0 !~ /^\[(package|project)\][[:space:]]*$/) in_sec = 0 }
        in_sec && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
            match($0, /"[^"]+"/); print substr($0, RSTART + 1, RLENGTH - 2); exit
        }
    ' "$1"
}
# Top-level (2-space indent) "version" key of a package.json.
json_top_version() {
    awk '
        /^  "version"[[:space:]]*:[[:space:]]*"[^"]+"[[:space:]]*,?[[:space:]]*$/ {
            colon = index($0, ":"); rest = substr($0, colon + 1)
            match(rest, /"[^"]+"/); print substr(rest, RSTART + 1, RLENGTH - 2); exit
        }
    ' "$1"
}
# Version pin on an INTERNAL rsac dep (restricted to a relative `path = "../..`
# so a registry dep or an unrelated `rsac-foo` crate can never be picked up).
# Mirrors internal_rsac_dep_version() in scripts/bump-version.sh; handles both
# the single-line inline-table form (rsac-ffi) and the multi-line
# [*.dependencies.rsac] table form. Emits nothing for a path-only dep.
internal_rsac_dep_version() {
    awk '
        /^[[:space:]]*rsac[[:space:]]*=[[:space:]]*\{/ && /path[[:space:]]*=[[:space:]]*"\.\.\// {
            if (match($0, /(^|[^[:alnum:]_])version[[:space:]]*=[[:space:]]*"[^"]+"/)) {
                kv = substr($0, RSTART, RLENGTH)
                match(kv, /"[^"]+"/); print substr(kv, RSTART + 1, RLENGTH - 2)
            }
            exit
        }
        /^[[:space:]]*\[.*\.?dependencies\.rsac\][[:space:]]*$/ { in_tbl = 1; tbl_path = 0; tbl_ver = ""; next }
        in_tbl && /^[[:space:]]*\[/ { in_tbl = 0 }
        in_tbl {
            if ($0 ~ /^[[:space:]]*path[[:space:]]*=[[:space:]]*"\.\.\//) tbl_path = 1
            if (match($0, /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/)) {
                kv = substr($0, RSTART, RLENGTH)
                match(kv, /"[^"]+"/); tbl_ver = substr(kv, RSTART + 1, RLENGTH - 2)
            }
            if (tbl_path && tbl_ver != "") { print tbl_ver; exit }
        }
    ' "$1"
}

# ── gather versions (parallel indexed arrays — bash 3.2 safe) ───────────
# LABELS[i] is the human label; VERS[i] is the extracted version.
LABELS=(); VERS=()
record() { LABELS[${#LABELS[@]}]="$1"; VERS[${#VERS[@]}]="$2"; }

FAIL_EXTRACT=0
add_toml() {
    local file="$1" v; v="$(toml_pkg_version "$file")"
    if [ -z "$v" ]; then gha_error "could not extract [package]/[project].version from $file"; FAIL_EXTRACT=1; return; fi
    record "$file" "$v"
}
add_json() {
    local file="$1" v; v="$(json_top_version "$file")"
    if [ -z "$v" ]; then gha_error "could not extract top-level \"version\" from $file"; FAIL_EXTRACT=1; return; fi
    record "$file" "$v"
}

add_toml "Cargo.toml"
add_toml "bindings/rsac-ffi/Cargo.toml"
add_toml "bindings/rsac-napi/Cargo.toml"
add_json "bindings/rsac-napi/package.json"
add_toml "bindings/rsac-python/Cargo.toml"
add_toml "bindings/rsac-python/pyproject.toml"
add_toml "mobile/android-native/Cargo.toml"
add_toml "integrations/tauri-plugin-rsac/Cargo.toml"
add_json "integrations/tauri-plugin-rsac/guest-js/package.json"

# Internal rsac dep pins — only the manifests bump-version.sh treats as
# INTERNAL_DEP_MANIFESTS; a path-only dep (no version key) is skipped, not an
# error (matches bump-version.sh). Today only rsac-ffi carries a pin.
for m in \
    "bindings/rsac-ffi/Cargo.toml" \
    "bindings/rsac-napi/Cargo.toml" \
    "bindings/rsac-python/Cargo.toml" \
    "integrations/tauri-plugin-rsac/Cargo.toml"; do
    pin="$(internal_rsac_dep_version "$m" || true)"
    [ -z "$pin" ] && continue
    record "$m (internal rsac dep pin)" "$pin"
done

# ── report ──────────────────────────────────────────────────────────────
# Fail-closed FIRST: if any extraction failed, VERS[0] may not even be the
# root Cargo.toml (record() appends to the next free index), so binding ROOT
# or printing divergences before this check would mislead whoever is
# debugging a blocked release (CodeRabbit PR #69).
if [ "$FAIL_EXTRACT" -ne 0 ]; then
    gha_error "one or more manifest versions could not be extracted — refusing to proceed (fail-closed)"
    exit 1
fi

ROOT="${VERS[0]}"   # Cargo.toml is recorded first — the source of truth.
echo "Discovered versions:"
i=0
while [ "$i" -lt "${#LABELS[@]}" ]; do
    printf '  %-52s %s\n' "${LABELS[$i]}" "${VERS[$i]:-<none>}"
    i=$((i + 1))
done

# rsac-go carries no in-tree version (tagged out-of-band as
# bindings/rsac-go/vX.Y.Z — docs/RELEASE_PROCESS.md). Recorded so the gap is
# intentional, not an oversight.
echo "note: rsac-go carries no manifest version (tagged out-of-band as bindings/rsac-go/vX.Y.Z)"

# Divergence from the root source of truth.
DIVERGED=0
i=0
while [ "$i" -lt "${#LABELS[@]}" ]; do
    if [ "${VERS[$i]}" != "$ROOT" ]; then
        DIVERGED=1
        echo "DIVERGENCE: ${LABELS[$i]} is ${VERS[$i]}, root Cargo.toml is $ROOT"
    fi
    i=$((i + 1))
done

# ── verdict ──────────────────────────────────────────────────────────────
if [ -n "$EXPECT" ]; then
    # Tag / release time: every version must equal the expected release version.
    # This subsumes the divergence check (all == EXPECT ⇒ all agree).
    FAIL=0
    i=0
    while [ "$i" -lt "${#LABELS[@]}" ]; do
        if [ "${VERS[$i]}" != "$EXPECT" ]; then
            FAIL=1
            gha_error "${LABELS[$i]} version ${VERS[$i]} does not match expected release version ${EXPECT}"
        fi
        i=$((i + 1))
    done
    if [ "$FAIL" -ne 0 ]; then
        gha_error "version lockstep violated vs expected ${EXPECT} — refusing to proceed"
        exit 1
    fi
    printf '%s[ok]%s all manifests + internal pins match expected release version %s\n' "$GRN" "$RST" "$EXPECT"
    exit 0
fi

if [ "$DIVERGED" -ne 0 ]; then
    if [ "$WARN_ONLY" -eq 1 ]; then
        gha_warn "rsac + binding versions are not in lockstep (see above). This only HARD-FAILS on a release tag / release publish; bump every manifest to match before tagging (scripts/bump-version.sh)."
        exit 0
    fi
    gha_error "rsac + binding versions are not in lockstep (see above) — refusing to proceed"
    exit 1
fi

printf '%s[ok]%s rsac + all bindings + internal pins agree at %s\n' "$GRN" "$RST" "$ROOT"
exit 0

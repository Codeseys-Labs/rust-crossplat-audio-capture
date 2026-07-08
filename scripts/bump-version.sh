#!/usr/bin/env bash
#
# Bump the rsac workspace version across the seven files that must agree:
#   - Cargo.toml                              (root `rsac` crate)
#   - bindings/rsac-ffi/Cargo.toml            (C FFI crate)
#   - bindings/rsac-napi/Cargo.toml           (napi crate)
#   - bindings/rsac-napi/package.json         (npm package)
#   - bindings/rsac-python/Cargo.toml         (pyo3 crate)
#   - bindings/rsac-python/pyproject.toml     (python package)
#   - mobile/android-native/Cargo.toml        (Android cdylib shim)
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
FFI_CARGO="bindings/rsac-ffi/Cargo.toml"
NAPI_CARGO="bindings/rsac-napi/Cargo.toml"
NAPI_PKG="bindings/rsac-napi/package.json"
PY_CARGO="bindings/rsac-python/Cargo.toml"
PY_PYPROJ="bindings/rsac-python/pyproject.toml"
ANDROID_NATIVE_CARGO="mobile/android-native/Cargo.toml"
CHANGELOG="CHANGELOG.md"

for f in "$ROOT_CARGO" "$FFI_CARGO" "$NAPI_CARGO" "$NAPI_PKG" "$PY_CARGO" "$PY_PYPROJ" "$ANDROID_NATIVE_CARGO" "$CHANGELOG"; do
    [ -f "$f" ] || { err "missing required file: $f"; exit 1; }
done

# ── helpers ──────────────────────────────────────────────────────────
# Read the current [package].version from a Cargo.toml / pyproject.toml.
# Only matches the first `version = "..."` that appears *inside* the
# `[package]` or `[project]` section so nested deps don't confuse it.
cargo_package_version() {
    awk '
        /^\[package\][[:space:]]*$/ || /^\[project\][[:space:]]*$/ { in_sec = 1; next }
        /^\[/ { if ($0 !~ /^\[(package|project)\][[:space:]]*$/) in_sec = 0 }
        in_sec && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
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
        /^  "version"[[:space:]]*:[[:space:]]*"[^"]+"[[:space:]]*,?[[:space:]]*$/ {
            # Capture the value literal — the second quoted string on the
            # line — by skipping past the colon first so we do not match
            # the key name.
            colon = index($0, ":")
            rest = substr($0, colon + 1)
            match(rest, /"[^"]+"/)
            v = substr(rest, RSTART + 1, RLENGTH - 2)
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
        /^\[package\][[:space:]]*$/ || /^\[project\][[:space:]]*$/ {
            in_sec = 1; print; next
        }
        /^\[/ {
            if ($0 !~ /^\[(package|project)\][[:space:]]*$/) in_sec = 0
        }
        in_sec && !done && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
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
        !done && /^  "version"[[:space:]]*:[[:space:]]*"[^"]+"[[:space:]]*,?[[:space:]]*$/ {
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

# Read the version pin from an internal rsac dependency in a binding manifest.
# Two manifest forms are recognised, both restricted to the *internal* rsac dep
# (the entry must carry a relative `path = "../..` to the workspace root, so an
# unrelated crate named `rsac-foo` or a registry dep can never be picked up):
#
#   1. Single-line inline table:
#        rsac = { path = "../../", version = "X.Y.Z", ... }     (rsac-ffi today)
#   2. Multi-line dependency table (rsac-python pins its backend deps this way):
#        [target.'cfg(windows)'.dependencies.rsac]
#        path = "../.."
#        version = "X.Y.Z"           # path-only today; future-proofed here
#
# Emits nothing if the manifest has no versioned internal rsac dep (path-only
# deps — rsac-napi, rsac-python — legitimately omit the version requirement).
# The `version` key is matched with a left word boundary so a longer key like
# `min_version =` can never be mistaken for it.
internal_rsac_dep_version() {
    awk '
        # ── form 1: single-line inline table ──────────────────────────
        /^[[:space:]]*rsac[[:space:]]*=[[:space:]]*\{/ && /path[[:space:]]*=[[:space:]]*"\.\.\// {
            # Pull the value out of the first `version = "..."` key on the line.
            # The `(^|[^[:alnum:]_])` left boundary keeps `min_version` etc. out.
            if (match($0, /(^|[^[:alnum:]_])version[[:space:]]*=[[:space:]]*"[^"]+"/)) {
                kv = substr($0, RSTART, RLENGTH)
                match(kv, /"[^"]+"/)
                print substr(kv, RSTART + 1, RLENGTH - 2)
            }
            exit
        }

        # ── form 2: multi-line [*.dependencies.rsac] table ────────────
        # A section header ending in `.dependencies.rsac` (e.g.
        # [dependencies.rsac] or [target.cfg(...).dependencies.rsac])
        # opens a block we scan for a `path = "../..` + `version = "..."` pair.
        /^[[:space:]]*\[.*\.?dependencies\.rsac\][[:space:]]*$/ {
            in_tbl = 1; tbl_path = 0; tbl_ver = ""
            next
        }
        in_tbl && /^[[:space:]]*\[/ {           # next section closes the block
            in_tbl = 0
        }
        in_tbl {
            if ($0 ~ /^[[:space:]]*path[[:space:]]*=[[:space:]]*"\.\.\//) tbl_path = 1
            # `^[[:space:]]*version` anchors the key to the start of its own
            # line, so `min_version = ...` (different start) can never match.
            if (match($0, /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/)) {
                kv = substr($0, RSTART, RLENGTH)
                match(kv, /"[^"]+"/)
                tbl_ver = substr(kv, RSTART + 1, RLENGTH - 2)
            }
            # Emit as soon as a versioned internal table is fully identified.
            if (tbl_path && tbl_ver != "") {
                print tbl_ver
                exit
            }
        }
    ' "$1"
}

# In-place rewrite of the version pin on an internal rsac dependency so a binding
# crate that records a version requirement (today only rsac-ffi) stays in lockstep
# with the workspace `[package].version`. Without this, a bump rewrites rsac-ffi's
# own `[package].version` but leaves its `rsac = { path = "../../", version =
# "<old>" }` pin stale, which breaks the workspace build until hand-fixed
# (seed rsac-0d58).
#
# Handles both manifest forms (mirroring internal_rsac_dep_version):
#   1. Single-line inline table — `rsac = { path = "../..", version = "X" }`
#   2. Multi-line `[*.dependencies.rsac]` table with a `version = "X"` line
# In both cases only the value of the existing `version = "..."` key is replaced;
# every other key, the table form, ordering and spacing are preserved. A
# path-only internal dep (no `version` key) is left untouched — there is nothing
# to keep in lockstep. The `version` key is matched with a left word boundary so
# a longer key (`min_version =`) is never rewritten by mistake.
rewrite_internal_rsac_dep_version() {
    local file="$1" new="$2"
    awk -v new="$new" '
        # Splice a new value into the FIRST standalone `version = "..."` key on
        # `line`, preserving everything else. The left boundary ((^|[^[:alnum:]_]))
        # keeps a longer key such as `min_version =` from matching. awk sub() has
        # no \1 backreference, so we locate the value with match()+substr() and
        # rebuild the string by hand. Returns the rewritten line (unchanged if no
        # standalone version key is present).
        function splice_version(line,   ks, kl, pre, post, hit, qpre) {
            if (!match(line, /(^|[^[:alnum:]_])version[[:space:]]*=[[:space:]]*"[^"]+"/))
                return line
            # Snapshot the key-match bounds BEFORE the inner match() clobbers
            # RSTART/RLENGTH.
            ks = RSTART; kl = RLENGTH
            pre  = substr(line, 1, ks - 1)        # text before the matched key
            post = substr(line, ks + kl)          # text after the matched value
            hit  = substr(line, ks, kl)           # the matched `[b]version = "old"`
            # Replace only the quoted value inside the matched span.
            match(hit, /"[^"]+"/)
            qpre = substr(hit, 1, RSTART - 1)     # `[b]version = ` (key + separator)
            return pre qpre "\"" new "\"" post
        }

        # Emit the buffered multi-line table, rewriting its version line only when
        # the table is the internal rsac dep (carries the `../..` path pin) and we
        # have not already rewritten a pin elsewhere.
        function flush_tbl(   i) {
            if (tbl_path && tbl_ver_idx > 0 && !done) {
                buf[tbl_ver_idx] = splice_version(buf[tbl_ver_idx])
                done = 1
            }
            for (i = 1; i <= nbuf; i++) print buf[i]
            nbuf = 0; tbl_path = 0; tbl_ver_idx = 0
        }

        BEGIN { done = 0; in_tbl = 0; nbuf = 0; tbl_path = 0; tbl_ver_idx = 0 }

        # A section header closes any open multi-line table first.
        /^[[:space:]]*\[/ {
            if (in_tbl) { flush_tbl(); in_tbl = 0 }
        }

        # ── form 2: open a multi-line [*.dependencies.rsac] table ─────
        /^[[:space:]]*\[.*\.?dependencies\.rsac\][[:space:]]*$/ {
            in_tbl = 1
            print
            next
        }

        # Buffer lines inside a multi-line table so we can decide (path + version)
        # before rewriting, regardless of key ordering.
        in_tbl {
            buf[++nbuf] = $0
            if ($0 ~ /^[[:space:]]*path[[:space:]]*=[[:space:]]*"\.\.\//) tbl_path = 1
            # `^[[:space:]]*version` anchors the key to the line start, so a
            # `min_version = ...` line (different start) is never targeted.
            if (tbl_ver_idx == 0 \
                && $0 ~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"/) {
                tbl_ver_idx = nbuf
            }
            next
        }

        # ── form 1: single-line inline table ──────────────────────────
        !done \
            && /^[[:space:]]*rsac[[:space:]]*=[[:space:]]*\{/ \
            && /path[[:space:]]*=[[:space:]]*"\.\.\// {
            new_line = splice_version($0)
            if (new_line != $0) { $0 = new_line; done = 1 }
            print
            next
        }

        { print }

        END { if (in_tbl) flush_tbl() }
    ' "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
}

# ── plan ─────────────────────────────────────────────────────────────
# Wraps an extractor call so an empty result becomes a fatal error rather
# than silently propagating an empty string into later comparisons (which
# would mis-plan or mis-rewrite).
extract_or_die() {
    local label="$1" file="$2" extractor="$3"
    local v
    v=$("$extractor" "$file")
    if [ -z "$v" ]; then
        err "could not extract version from $label ($file)"
        err "  — check that the file has a [package]/[project] section"
        err "    with a \`version = \"X.Y.Z\"\` line (or top-level"
        err "    \"version\" key for JSON)"
        exit 1
    fi
    printf '%s\n' "$v"
}

CUR_ROOT=$(extract_or_die       "root crate"   "$ROOT_CARGO" cargo_package_version)
CUR_FFI_CARGO=$(extract_or_die  "rsac-ffi"     "$FFI_CARGO"  cargo_package_version)
CUR_NAPI_CARGO=$(extract_or_die "rsac-napi"    "$NAPI_CARGO" cargo_package_version)
CUR_NAPI_PKG=$(extract_or_die   "rsac-napi pkg" "$NAPI_PKG"   json_version)
CUR_PY_CARGO=$(extract_or_die   "rsac-python"  "$PY_CARGO"   cargo_package_version)
CUR_PY_PYPROJ=$(extract_or_die  "rsac-python pyproject" "$PY_PYPROJ" cargo_package_version)
CUR_ANDROID_NATIVE=$(extract_or_die "rsac-android-native" "$ANDROID_NATIVE_CARGO" cargo_package_version)

# Internal `rsac = { ..., version = "X.Y.Z" }` dep pins in the binding manifests
# (seed rsac-0d58). These must track the root crate version so a published
# binding resolves the matching rsac release and the workspace build stays
# self-consistent after a bump. We scan every binding Cargo.toml and remember
# the ones that actually carry a versioned internal rsac dep — path-only deps
# (no version requirement) are skipped, not an error. Today only rsac-ffi pins a
# version, but driving this off detection means a new versioned pin in any
# binding manifest is picked up automatically.
INTERNAL_DEP_MANIFESTS=("$FFI_CARGO" "$NAPI_CARGO" "$PY_CARGO")
INTERNAL_DEP_FILES=()    # manifests with a stale internal pin to rewrite
for m in "${INTERNAL_DEP_MANIFESTS[@]}"; do
    cur_pin=$(internal_rsac_dep_version "$m")
    [ -z "$cur_pin" ] && continue          # path-only internal dep — nothing to pin
    if [ "$cur_pin" != "$NEW_VERSION" ]; then
        INTERNAL_DEP_FILES+=("$m")
    fi
done

info "target version:  $NEW_VERSION"
info "current versions:"
printf '  %-42s %s\n' "$ROOT_CARGO"  "$CUR_ROOT"
printf '  %-42s %s\n' "$FFI_CARGO"   "$CUR_FFI_CARGO"
printf '  %-42s %s\n' "$NAPI_CARGO"  "$CUR_NAPI_CARGO"
printf '  %-42s %s\n' "$NAPI_PKG"    "$CUR_NAPI_PKG"
printf '  %-42s %s\n' "$PY_CARGO"    "$CUR_PY_CARGO"
printf '  %-42s %s\n' "$PY_PYPROJ"   "$CUR_PY_PYPROJ"
printf '  %-42s %s\n' "$ANDROID_NATIVE_CARGO" "$CUR_ANDROID_NATIVE"

# Idempotency guard: if every target already matches — including the internal
# rsac dep pins — exit cleanly without touching the changelog either (rotating a
# changelog that's already been rotated would create a duplicate ## [X.Y.Z]
# header). A stale internal pin (a non-empty INTERNAL_DEP_FILES) still counts as
# work to do, so this stays correct after the rsac-0d58 fix.
if [ "$CUR_ROOT" = "$NEW_VERSION" ] && \
   [ "$CUR_FFI_CARGO" = "$NEW_VERSION" ] && \
   [ "$CUR_NAPI_CARGO" = "$NEW_VERSION" ] && \
   [ "$CUR_NAPI_PKG" = "$NEW_VERSION" ] && \
   [ "$CUR_PY_CARGO" = "$NEW_VERSION" ] && \
   [ "$CUR_PY_PYPROJ" = "$NEW_VERSION" ] && \
   [ "$CUR_ANDROID_NATIVE" = "$NEW_VERSION" ] && \
   [ "${#INTERNAL_DEP_FILES[@]}" -eq 0 ]; then
    ok "already at version $NEW_VERSION — nothing to do"
    exit 0
fi

# Plan per-file changes.
CHANGES=()
[ "$CUR_ROOT" != "$NEW_VERSION" ]       && CHANGES+=("$ROOT_CARGO")
[ "$CUR_FFI_CARGO" != "$NEW_VERSION" ]  && CHANGES+=("$FFI_CARGO")
[ "$CUR_NAPI_CARGO" != "$NEW_VERSION" ] && CHANGES+=("$NAPI_CARGO")
[ "$CUR_NAPI_PKG" != "$NEW_VERSION" ]   && CHANGES+=("$NAPI_PKG")
[ "$CUR_PY_CARGO" != "$NEW_VERSION" ]   && CHANGES+=("$PY_CARGO")
[ "$CUR_PY_PYPROJ" != "$NEW_VERSION" ]  && CHANGES+=("$PY_PYPROJ")
[ "$CUR_ANDROID_NATIVE" != "$NEW_VERSION" ] && CHANGES+=("$ANDROID_NATIVE_CARGO")

# Internal rsac dep-pin rewrites (rsac-0d58). Listed distinctly so the plan is
# honest even when the same manifest also gets a [package].version bump (FFI).
for m in "${INTERNAL_DEP_FILES[@]}"; do
    CHANGES+=("$m (internal rsac dep pin → $NEW_VERSION)")
done

# Changelog rotation is planned if there's an "## [Unreleased]" section
# *and* no "## [$NEW_VERSION]" header already exists (idempotent).
#
# Use `grep -F` (fixed string) for the version header check rather than
# trying to escape regex metacharacters in $NEW_VERSION. Today the semver
# gate above ensures $NEW_VERSION contains only digits/dots/hyphens/
# letters — none of which are grep-BRE metacharacters — but using -F
# removes the regex-injection surface entirely, so this stays safe if the
# semver regex is ever relaxed.
CHANGELOG_ROTATE=0
if grep -qF '## [Unreleased]' "$CHANGELOG" && \
   ! grep -qF "## [$NEW_VERSION]" "$CHANGELOG"; then
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
[ "$CUR_FFI_CARGO"  != "$NEW_VERSION" ] && rewrite_cargo_version "$FFI_CARGO"   "$NEW_VERSION"
[ "$CUR_NAPI_CARGO" != "$NEW_VERSION" ] && rewrite_cargo_version "$NAPI_CARGO"  "$NEW_VERSION"
[ "$CUR_NAPI_PKG"   != "$NEW_VERSION" ] && rewrite_json_version  "$NAPI_PKG"    "$NEW_VERSION"
[ "$CUR_PY_CARGO"   != "$NEW_VERSION" ] && rewrite_cargo_version "$PY_CARGO"    "$NEW_VERSION"
[ "$CUR_PY_PYPROJ"  != "$NEW_VERSION" ] && rewrite_cargo_version "$PY_PYPROJ"   "$NEW_VERSION"
[ "$CUR_ANDROID_NATIVE" != "$NEW_VERSION" ] && rewrite_cargo_version "$ANDROID_NATIVE_CARGO" "$NEW_VERSION"

# Internal rsac dep pins (rsac-0d58) — rewrite after the [package].version pass
# so a manifest that gets both (FFI) ends up fully self-consistent.
for m in "${INTERNAL_DEP_FILES[@]}"; do
    rewrite_internal_rsac_dep_version "$m" "$NEW_VERSION"
done

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

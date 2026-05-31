#!/usr/bin/env bash
#
# verify-docs-rs.sh — post-publish docs.rs rendering spot-check for rsac#16.
#
# Exercises https://docs.rs/ shortly after `cargo publish` lands a new
# version: checks the build status, the rendered landing page, and a few
# representative public items + feature-gated items. Intended as the
# one-command verification referenced by docs/RELEASE_PROCESS.md.
#
# Usage:
#   bash scripts/verify-docs-rs.sh [VERSION]
#
# If VERSION is omitted, the script reads it from the root Cargo.toml.
# Exits 0 on success, non-zero on any failed probe.
#
# Portable shell only — BSD + GNU grep/curl. No "\s" class (uses
# "[[:space:]]" instead). shellcheck clean.

set -euo pipefail

# ── colours ──────────────────────────────────────────────────────────
if [ -t 1 ] && [ "${NO_COLOR:-}" = "" ]; then
    RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLW=$'\033[0;33m'
    BLU=$'\033[0;34m'; DIM=$'\033[0;90m'; RST=$'\033[0m'
else
    RED=""; GRN=""; YLW=""; BLU=""; DIM=""; RST=""
fi
info()  { printf '%s[info]%s %s\n'  "$BLU" "$RST" "$*"; }
ok()    { printf '%s[ok]%s   %s\n'  "$GRN" "$RST" "$*"; }
warn()  { printf '%s[warn]%s %s\n'  "$YLW" "$RST" "$*" >&2; }
err()   { printf '%s[err]%s  %s\n'  "$RED" "$RST" "$*" >&2; }

# ── repo root (so we can read Cargo.toml regardless of cwd) ──────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── args: version defaults to Cargo.toml [package] version ───────────
VERSION="${1:-}"
if [ -z "$VERSION" ]; then
    if [ ! -f "$REPO_ROOT/Cargo.toml" ]; then
        err "Cargo.toml not found at $REPO_ROOT/Cargo.toml and no version passed"
        exit 2
    fi
    # Extract the first "version = \"X.Y.Z\"" line in the [package] table.
    # BSD/GNU-portable: awk scan, no lookbehind, no \s.
    VERSION="$(awk '
        /^\[package\]/            { in_pkg = 1; next }
        /^\[/ && !/^\[package\]/  { in_pkg = 0 }
        in_pkg && /^version[[:space:]]*=/ {
            # line looks like: version = "0.2.0"
            match($0, /"[^"]+"/)
            if (RSTART > 0) {
                v = substr($0, RSTART + 1, RLENGTH - 2)
                print v
                exit
            }
        }
    ' "$REPO_ROOT/Cargo.toml")"
    if [ -z "$VERSION" ]; then
        err "could not parse version from $REPO_ROOT/Cargo.toml"
        exit 2
    fi
    info "version not supplied; using Cargo.toml version: $VERSION"
fi

CRATE="rsac"
DOCS_BASE="https://docs.rs/${CRATE}/${VERSION}/${CRATE}"
BUILDS_URL="https://docs.rs/crate/${CRATE}/${VERSION}/builds.json"

PASS=0
FAIL=0
FAILURES=()

probe_http_200() {
    # $1 = label, $2 = url
    local label="$1"
    local url="$2"
    local code
    code="$(curl -sS -o /dev/null -w '%{http_code}' -L "$url" || printf '000')"
    if [ "$code" = "200" ]; then
        ok "HTTP 200  $label  ($url)"
        PASS=$((PASS + 1))
    else
        err "HTTP $code  $label  ($url)"
        FAIL=$((FAIL + 1))
        FAILURES+=("$label -> HTTP $code at $url")
    fi
}

probe_html_contains() {
    # $1 = label, $2 = url, $3 = needle (fixed string)
    local label="$1"
    local url="$2"
    local needle="$3"
    local body
    body="$(curl -sS -L "$url" || printf '')"
    if [ -z "$body" ]; then
        err "empty body  $label  ($url)"
        FAIL=$((FAIL + 1))
        FAILURES+=("$label -> empty body at $url")
        return
    fi
    # -F: fixed string. -q: quiet. Works on BSD + GNU grep.
    if printf '%s' "$body" | grep -qF -- "$needle"; then
        ok "contains  '$needle'  in  $label"
        PASS=$((PASS + 1))
    else
        err "missing   '$needle'  in  $label  ($url)"
        FAIL=$((FAIL + 1))
        FAILURES+=("$label missing '$needle' at $url")
    fi
}

info "rsac docs.rs spot-check for v${VERSION}"
info "base: ${DOCS_BASE}/"
echo

# ── 1. build status (JSON) ───────────────────────────────────────────
info "checking build status: $BUILDS_URL"
BUILDS_JSON="$(curl -sS -L "$BUILDS_URL" || printf '')"
if [ -z "$BUILDS_JSON" ]; then
    err "could not fetch builds.json (network? version not yet indexed?)"
    FAIL=$((FAIL + 1))
    FAILURES+=("builds.json fetch failed")
else
    # Prefer jq if available; else fall back to a portable grep.
    BUILD_OK=0
    if command -v jq >/dev/null 2>&1; then
        # builds.json is a JSON array. Any entry with build_status == "success"
        # (older) or "succeeded" (newer docs.rs schema) passes.
        if printf '%s' "$BUILDS_JSON" \
            | jq -e 'any(.[]; (.build_status // .status) as $s
                              | $s == "success" or $s == "succeeded")' \
            >/dev/null 2>&1; then
            BUILD_OK=1
        fi
    else
        warn "jq not installed; falling back to grep on builds.json"
        if printf '%s' "$BUILDS_JSON" \
            | grep -qE '"(build_status|status)"[[:space:]]*:[[:space:]]*"(success|succeeded)"'; then
            BUILD_OK=1
        fi
    fi
    if [ "$BUILD_OK" = "1" ]; then
        ok "docs.rs build succeeded for ${CRATE} v${VERSION}"
        PASS=$((PASS + 1))
    else
        err "docs.rs build did NOT report success for ${CRATE} v${VERSION}"
        FAIL=$((FAIL + 1))
        FAILURES+=("build_status not success in builds.json")
    fi
fi
echo

# ── 2. landing page HTTP 200 ─────────────────────────────────────────
probe_http_200 "crate landing page" "${DOCS_BASE}/"
echo

# ── 3. core public items rendered on landing page ────────────────────
info "checking core public item rendering on landing page"
LANDING_URL="${DOCS_BASE}/"
probe_html_contains "landing page" "$LANDING_URL" "PlatformCapabilities"
probe_html_contains "landing page" "$LANDING_URL" "CaptureTarget"
probe_html_contains "landing page" "$LANDING_URL" "AudioDevice"
# Feature-gated item shows up because docs.rs renders with all-features
# (or is explicitly configured via [package.metadata.docs.rs]).
probe_html_contains "landing page" "$LANDING_URL" "feat_macos"
echo

# ── 4. intra-doc link targets (representative pages) ─────────────────
info "checking intra-doc link targets resolve"
probe_http_200 "AudioCaptureBuilder struct page" \
    "${DOCS_BASE}/struct.AudioCaptureBuilder.html"
probe_http_200 "PlatformCapabilities struct page" \
    "${DOCS_BASE}/struct.PlatformCapabilities.html"
probe_http_200 "CaptureTarget enum page" \
    "${DOCS_BASE}/enum.CaptureTarget.html"
probe_http_200 "AudioDevice struct page" \
    "${DOCS_BASE}/struct.AudioDevice.html"
echo

# ── summary ──────────────────────────────────────────────────────────
TOTAL=$((PASS + FAIL))
echo "${DIM}────────────────────────────────────────────────────────────${RST}"
printf 'verify-docs-rs: %s %d/%d checks passed%s\n' \
    "$GRN" "$PASS" "$TOTAL" "$RST"
if [ "$FAIL" -eq 0 ]; then
    ok "docs.rs rendering for ${CRATE} v${VERSION} looks good"
    exit 0
fi

err "docs.rs rendering for ${CRATE} v${VERSION} has ${FAIL} failure(s):"
for f in "${FAILURES[@]}"; do
    printf '  %s- %s%s\n' "$RED" "$f" "$RST"
done
echo
warn "if the build itself failed, add [package.metadata.docs.rs] to Cargo.toml:"
cat <<'EOF'
    [package.metadata.docs.rs]
    all-features = true
    rustdoc-args = ["--cfg", "docsrs"]
EOF
exit 1

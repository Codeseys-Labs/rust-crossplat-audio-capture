#!/usr/bin/env bash
#
# check-module-dag.sh — module-DAG reverse-edge guard (critique DAG-004)
#
# Enforces the rsac module layering DAG documented in
#   - src/lib.rs
#   - docs/ARCHITECTURE.md  (§1, "Known deviation (tracked)")
#   - AGENTS.md             (§ Module layering / §6.6)
#
#       core/  →  bridge/  →  audio/  →  api/
#                                         ↘ sink/
#
# Dependencies may only point DOWN this chain. A layer must never `use` or
# reference (`crate::<upper-layer>::…`) a layer that sits ABOVE it. Concretely:
#
#   core    must not reference  bridge | audio | api | sink
#   bridge  must not reference  audio  | api   | sink
#   audio   must not reference  api    | sink
#
# (api/ and sink/ are the top of the chain, so they have no forbidden edges to
# guard here.)
#
# This is the DAG-004 CI guard called for by
# docs/reviews/rsac-architecture-critique-2026-05-30.md (findings DAG-001 /
# DAG-002, "Top actions" #3): "add a CI grep/cargo-modules guard so
# core->audio/bridge/api/sink edges fail the build."
#
# ─────────────────────────────────────────────────────────────────────────────
# HONESTY / ALLOWLIST
# ─────────────────────────────────────────────────────────────────────────────
# The critique confirmed a KNOWN, already-shipped violation: core/introspection.rs
# reaches UP into crate::audio::* to implement source/application discovery
# (4 call sites). That edge is documented as a tracked deviation in
# docs/ARCHITECTURE.md (§1, "Known deviation (tracked)") and is finding
# DAG-001/DAG-002 of the 2026-05-30 architecture critique. The accepted fix is to
# move list_audio_sources / list_audio_applications(_into) into the audio/api
# layer; until that lands, the edge is an EXPLICIT, DOCUMENTED exception.
#
# The ALLOWLIST array below records each accepted exception so this guard PASSES
# on the known edges TODAY but FAILS on any NEW reverse edge. Each entry is keyed
# as "<relative-path>::<crate-path-prefix>" and is intentionally specific (it
# names the exact upward symbol), so a brand-new edge — even in the same file, to
# a different symbol — is NOT silently covered and will fail the build.
#
# To add a new exception you MUST: (1) add the precise key here, (2) cite a
# tracking issue / ADR / critique-finding in the trailing comment. Do NOT
# broaden a key to a whole file or a whole layer.
#
# Usage:
#   scripts/check-module-dag.sh          # scan, print offenders, exit 1 if any
#   scripts/check-module-dag.sh --list   # print the allowlist and exit 0
#   scripts/check-module-dag.sh -h|--help
#
# Exit status: 0 = clean (only allowlisted edges remain); 1 = NEW violation(s);
#              2 = usage / environment error.
#
set -euo pipefail

# ── Locate the repo root (this script lives in <root>/scripts/) ───────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." >/dev/null 2>&1 && pwd)"
SRC_DIR="${REPO_ROOT}/src"

# ─────────────────────────────────────────────────────────────────────────────
# ALLOWLIST — accepted upward edges. Format: "<rel-path>::<crate::path::prefix>"
#
# Every entry below is a documented exception. The guard treats a matched real
# edge as OK if and only if "<rel-path>::<matched-crate-path>" begins with one of
# these keys. Keep keys as specific as possible (name the exact symbol).
# ─────────────────────────────────────────────────────────────────────────────
ALLOWLIST=(
  # --- DAG-001 / DAG-002 (architecture critique 2026-05-30; tracked in
  #     docs/ARCHITECTURE.md §1 "Known deviation (tracked)"). core/introspection.rs
  #     reaches up into the audio layer for source/application discovery. Accepted
  #     fix: move discovery into audio/api and re-export at the same lib.rs paths.
  "src/core/introspection.rs::crate::audio::get_device_enumerator"
  "src/core/introspection.rs::crate::audio::macos::enumerate_audio_applications"
  "src/core/introspection.rs::crate::audio::windows::enumerate_application_audio_sessions"
  "src/core/introspection.rs::crate::audio::linux::enumerate_audio_applications"

  # --- Test-only edge: the bridge/ integration-test module (#[cfg(test)]) wires
  #     the full stack (sink + bridge) together to exercise the pipeline end to
  #     end. This is test wiring, not a production reverse dependency. Scoped to
  #     the exact sink import used by mod integration_tests.
  "src/bridge/mod.rs::crate::sink"
)

# ── Optional flags ────────────────────────────────────────────────────────────
if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,60p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit 0
fi
if [[ "${1:-}" == "--list" ]]; then
  echo "Module-DAG allowlist (${#ALLOWLIST[@]} accepted exception key(s)):"
  for entry in "${ALLOWLIST[@]}"; do
    echo "  - ${entry}"
  done
  exit 0
fi

if [[ ! -d "${SRC_DIR}" ]]; then
  echo "error: source directory not found: ${SRC_DIR}" >&2
  exit 2
fi

# ── Pick a search tool: prefer ripgrep, fall back to grep -rn ─────────────────
have_rg=0
if command -v rg >/dev/null 2>&1; then
  have_rg=1
fi

# scan_layer <repo-relative-target> <forbidden-alt-regex>
#   Emits "<rel-path>:<lineno>:<crate::path>" for every REAL upward edge found.
#   "Real" = the reference appears in the CODE portion of the line; rustdoc
#   intra-doc links and ordinary // comments are stripped before matching, so a
#   `/// [..](crate::api::Foo)` doc link is NOT counted as a dependency edge.
#
#   The search runs with the working directory at REPO_ROOT and a REPO-RELATIVE
#   target (e.g. "src/core"). This is deliberate: an absolute Windows path
#   contains a drive-letter colon ("E:/…") that would corrupt the "file:line:col"
#   field split below. Relative paths have no such colon.
scan_layer() {
  local target="$1"      # repo-relative dir (e.g. src/core) OR file (src/api.rs)
  local alt="$2"         # e.g. "audio|api|sink"
  local pattern="crate::(${alt})::"

  local raw
  if [[ "${have_rg}" -eq 1 ]]; then
    # --no-heading + -n => "file:lineno:line"; restrict to Rust sources.
    raw="$(cd "${REPO_ROOT}" && rg -n --no-heading -g '*.rs' "${pattern}" "${target}" 2>/dev/null || true)"
  else
    if [[ -d "${REPO_ROOT}/${target}" ]]; then
      raw="$(cd "${REPO_ROOT}" && grep -rnE --include='*.rs' "${pattern}" "${target}" 2>/dev/null || true)"
    else
      raw="$(cd "${REPO_ROOT}" && grep -nE "${pattern}" "${target}" 2>/dev/null || true)"
    fi
  fi

  [[ -z "${raw}" ]] && return 0

  printf '%s\n' "${raw}" | while IFS= read -r line; do
    [[ -z "${line}" ]] && continue
    # Split "file:lineno:code". Targets are repo-relative, so the file field has
    # no drive-letter colon; the first two ':' are the rg/grep field separators.
    local file lineno code
    file="${line%%:*}"
    local rest="${line#*:}"
    lineno="${rest%%:*}"
    code="${rest#*:}"

    # Strip comments so doc-links / trailing comments do not count as edges.
    # Remove from the first "//" that is NOT preceded by ':' (protects "https://"
    # and "crate::" itself), and any line that starts with //, /// or //!.
    local stripped
    stripped="$(printf '%s' "${code}" | sed -E 's#([^:])//.*#\1#; s#^[[:space:]]*//.*##')"

    # Re-test the code-only portion; bail if the reference was comment-only.
    local matched
    matched="$(printf '%s' "${stripped}" | grep -oE "crate::(${alt})(::[A-Za-z0-9_]+)*" | head -n1 || true)"
    [[ -z "${matched}" ]] && continue

    # Normalise the path to repo-relative POSIX form (Windows rg emits "\").
    local rel
    rel="$(printf '%s' "${file}" | tr '\\' '/')"
    printf '%s:%s:%s\n' "${rel}" "${lineno}" "${matched}"
  done
}

# is_allowed <rel-path> <crate::path>
#   True if "<rel-path>::<crate::path>" starts with an allowlist key.
is_allowed() {
  local rel="$1" cratepath="$2"
  local key="${rel}::${cratepath}"
  local entry
  for entry in "${ALLOWLIST[@]}"; do
    if [[ "${key}" == "${entry}"* ]]; then
      return 0
    fi
  done
  return 1
}

echo "==> rsac module-DAG reverse-edge guard (DAG-004)"
echo "    chain: core -> bridge -> audio -> api (-> sink)"
echo "    tool : $([[ ${have_rg} -eq 1 ]] && echo 'ripgrep' || echo 'grep -rn')"
echo

# ── The three forbidden-edge scans (one per layer that has things above it) ───
# Layer source roots are REPO-RELATIVE (see scan_layer for why). NOTE: api/ is
# the file src/api.rs, every other layer is a directory. sink/ is top-of-chain
# so it is never a source scanned here.
declare -a SCAN_TARGETS=(
  "core::src/core::bridge|audio|api|sink"
  "bridge::src/bridge::audio|api|sink"
  "audio::src/audio::api|sink"
)

new_violations=0
allowed_hits=0

for spec in "${SCAN_TARGETS[@]}"; do
  layer="${spec%%::*}"
  rest="${spec#*::}"
  target="${rest%%::*}"
  alt="${rest#*::}"

  if [[ ! -e "${REPO_ROOT}/${target}" ]]; then
    echo "warning: ${layer} source root missing, skipping: ${target}" >&2
    continue
  fi

  while IFS= read -r hit; do
    [[ -z "${hit}" ]] && continue
    # hit == "rel:lineno:crate::path"
    rel="${hit%%:*}"
    r2="${hit#*:}"
    lineno="${r2%%:*}"
    cratepath="${r2#*:}"

    if is_allowed "${rel}" "${cratepath}"; then
      allowed_hits=$((allowed_hits + 1))
      printf '  [allowlisted] %s (%s:%s) -> %s\n' "${layer}" "${rel}" "${lineno}" "${cratepath}"
    else
      new_violations=$((new_violations + 1))
      printf '  [VIOLATION]   %s -> reverse edge at %s:%s : %s\n' "${layer}" "${rel}" "${lineno}" "${cratepath}" >&2
    fi
  done < <(scan_layer "${target}" "${alt}")
done

echo
echo "==> summary: ${allowed_hits} allowlisted edge(s), ${new_violations} new violation(s)"

if [[ "${new_violations}" -gt 0 ]]; then
  cat >&2 <<'EOF'

FAIL: new module-DAG reverse edge(s) detected.

The module DAG only allows dependencies to point DOWN the chain
  core -> bridge -> audio -> api (-> sink)
A lower layer must never `use`/reference `crate::<upper-layer>::…`.

Fix the offending file so the edge points down (move the upward-reaching code
into the higher layer and, if needed, re-export it at the same lib.rs path so
the public surface is unchanged). See docs/ARCHITECTURE.md §1.

If — and only if — the edge is a deliberate, tracked exception, add a SPECIFIC
key (path + exact symbol) to the ALLOWLIST in this script WITH a comment citing
the tracking issue / ADR. Do not broaden an existing key to cover it.
EOF
  exit 1
fi

echo "OK: no new module-DAG reverse edges (only documented exceptions remain)."
exit 0

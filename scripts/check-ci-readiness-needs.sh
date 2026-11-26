#!/usr/bin/env bash
#
# check-ci-readiness-needs.sh — guard release-readiness against CI job drift.
#
# Usage: bash scripts/check-ci-readiness-needs.sh [--self-test] [workflow.yml]
# Exit status: 0 = matching sets (or successful self-test); 1 = drift or
# self-test failure; 2 = usage or workflow extraction error.
# The optional workflow path defaults to .github/workflows/ci.yml.
#
set -euo pipefail
export LC_ALL=C

self_test=0
if [[ "${1:-}" == "--self-test" ]]; then
  self_test=1
  shift
fi
if [[ "$#" -gt 1 || "${1:-}" == --* ]]; then
  echo 'usage: bash scripts/check-ci-readiness-needs.sh [--self-test] [workflow.yml]' >&2
  exit 2
fi
workflow="${1:-.github/workflows/ci.yml}"
if [[ ! -f "$workflow" ]]; then
  echo "check-ci-readiness-needs: workflow not found: $workflow" >&2
  exit 2
fi

# The workflow uses two-space job keys and a four-space needs key. Fail closed
# if either section is absent or the needs list is not one of the supported forms.
extract() {
  awk -v wanted="$1" '
    function emit_list(s, n, a, i) {
      sub(/^[[:space:]]*\[/, "", s)
      sub(/\][[:space:]]*(#.*)?$/, "", s)
      n = split(s, a, ",")
      for (i = 1; i <= n; i++) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", a[i])
        if (a[i] !~ /^[A-Za-z_][A-Za-z0-9_-]*$/) exit 2
        print a[i]
        count++
      }
    }
    { sub(/\r$/, "") }
    /^jobs:[[:space:]]*(#.*)?$/ { in_jobs = 1; found_jobs = 1; next }
    in_jobs && /^[^ #[:space:]]/ { in_jobs = 0 }
    in_jobs && /^  [A-Za-z_][A-Za-z0-9_-]*:[[:space:]]*(#.*)?$/ {
      id = $0; sub(/^  /, "", id); sub(/:.*/, "", id)
      in_readiness = (id == "release-readiness")
      if (in_readiness) found_readiness++
      if (wanted == "jobs" && !in_readiness) { print id; count++ }
      in_list = 0
      next
    }
    in_jobs && in_readiness && /^    needs:[[:space:]]*/ {
      found_needs++
      value = $0; sub(/^    needs:[[:space:]]*/, "", value)
      if (value ~ /^\[[^]]*\][[:space:]]*(#.*)?$/) {
        if (wanted == "needs") emit_list(value)
      } else if (value ~ /^(#.*)?$/) {
        in_list = 1
      } else exit 2
      next
    }
    in_list && /^[[:space:]]*(#.*)?$/ { next }
    in_list && /^      -[[:space:]]+[A-Za-z_][A-Za-z0-9_-]*[[:space:]]*(#.*)?$/ {
      id = $0; sub(/^      -[[:space:]]+/, "", id); sub(/[[:space:]]*(#.*)?$/, "", id)
      if (wanted == "needs") { print id; count++ }
      next
    }
    in_list { in_list = 0 }
    END {
      if (!found_jobs || found_readiness != 1 || found_needs != 1 || !count) exit 2
    }
  ' "$workflow" | sort -u
}

if ! jobs="$(extract jobs)" || ! needs="$(extract needs)"; then
  echo 'check-ci-readiness-needs: could not extract jobs or release-readiness needs' >&2
  exit 2
fi
missing="$(comm -23 <(printf '%s\n' "$jobs") <(printf '%s\n' "$needs"))"
extra="$(comm -13 <(printf '%s\n' "$jobs") <(printf '%s\n' "$needs"))"
if [[ -n "$missing" || -n "$extra" ]]; then
  while IFS= read -r id; do
    [[ -z "$id" ]] || echo "check-ci-readiness-needs: missing job: $id" >&2
  done <<< "$missing"
  while IFS= read -r id; do
    [[ -z "$id" ]] || echo "check-ci-readiness-needs: extra job: $id" >&2
  done <<< "$extra"
  exit 1
fi

if [[ "$self_test" -eq 1 ]]; then
  # Rewrite the needs list in a temporary copy, removing exactly one job.
  removed="${needs%%$'\n'*}"
  remaining=""
  while IFS= read -r id; do
    if [[ "$id" != "$removed" ]]; then
      remaining="${remaining}${remaining:+, }${id}"
    fi
  done <<< "$needs"
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  awk -v remaining="$remaining" '
    { sub(/\r$/, "") }
    /^  release-readiness:[[:space:]]*(#.*)?$/ { in_readiness = 1 }
    in_readiness && /^    needs:[[:space:]]*/ {
      block = ($0 !~ /\[/)
      print "    needs: [" remaining "]"
      next
    }
    block && /^[[:space:]]*(#.*)?$/ { next }
    block && /^      -[[:space:]]+/ { next }
    block { block = 0 }
    { print }
  ' "$workflow" > "$tmpdir/workflow.yml"
  rc=0
  output="$(bash "${BASH_SOURCE[0]}" "$tmpdir/workflow.yml" 2>&1)" || rc=$?
  if [[ "$rc" -ne 1 || "$output" != *"missing job: $removed"* ]]; then
    echo 'check-ci-readiness-needs: self-test failed to flag the removed job' >&2
    echo "$output" >&2
    exit 1
  fi
  echo "check-ci-readiness-needs: self-test OK (flagged missing job: $removed)" >&2
else
  echo 'check-ci-readiness-needs: all CI jobs are listed' >&2
fi

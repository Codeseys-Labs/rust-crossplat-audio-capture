#!/usr/bin/env bash
#
# check-release-published.sh — decide whether a failed release can be re-tagged.
# Run from the repository root. This only inspects; it never deletes a tag.
#
# Usage: bash scripts/check-release-published.sh X.Y.Z [--remote NAME]
#        bash scripts/check-release-published.sh --self-test
# Default remote: origin. GH_BIN and CURL_BIN override gh and curl.
# RELEASE_REPO overrides the GitHub owner/name derived from --remote. An
# invalid override or unknown repository makes all three workflow probes indeterminate.
# --self-test checks the npm platform list against the napi manifest offline.
#
# Exit: 0 safe to re-tag; 1 published; 2 usage; 3 indeterminate (do not
# re-tag). For --self-test: 0 covered, 1 invalid or uncovered manifest.
set -euo pipefail

log() { printf 'check-release-published: %s\n' "$*" >&2; }
usage() { log 'usage: check-release-published.sh X.Y.Z [--remote NAME] | --self-test'; exit 2; }

# gh and curl return one value per probe. Preserve boundaries so malformed
# output (e.g. a zero followed by a blank line) cannot certify an unused version.
probe_output() {
    "$@" 2>/dev/null | {
        local value
        if IFS= read -r -d '' value; then return 1; fi
        if [[ -n $value ]]; then
            case $value in
                *$'\r\n') value=${value%$'\r\n'} ;;
                *) value=${value%$'\n'} ;;
            esac
            [[ -n $value && $value != *$'\n'* ]] || return 1
        fi
        printf '%s' "$value"
    }
}

NPM_PLATFORMS=(win32-x64-msvc darwin-x64 linux-x64-gnu darwin-arm64 linux-arm64-gnu
               linux-x64-musl linux-arm64-musl linux-arm-gnueabihf)

check_triple() {
    local platform
    case $1 in
        x86_64-pc-windows-msvc) platform=win32-x64-msvc ;;
        x86_64-apple-darwin) platform=darwin-x64 ;;
        x86_64-unknown-linux-gnu) platform=linux-x64-gnu ;;
        aarch64-apple-darwin) platform=darwin-arm64 ;;
        aarch64-unknown-linux-gnu) platform=linux-arm64-gnu ;;
        x86_64-unknown-linux-musl) platform=linux-x64-musl ;;
        aarch64-unknown-linux-musl) platform=linux-arm64-musl ;;
        armv7-unknown-linux-gnueabihf) platform=linux-arm-gnueabihf ;;
        *) log "no npm platform mapped for $1"; return 1 ;;
    esac
    [[ " ${NPM_PLATFORMS[*]} " == *" $platform "* ]] || { log "platform $platform for $1 missing from NPM_PLATFORMS"; return 1; }
}

self_test() {
    local path=bindings/rsac-napi/package.json manifest triples triple
    local array_re='"napi"[[:space:]]*:[[:space:]]*\{[^{}]*"triples"[[:space:]]*:[[:space:]]*\{[^{}]*"additional"[[:space:]]*:[[:space:]]*\[([^][]*)\]'
    local item_re='^[[:space:]]*"([[:alnum:]_-]+)"[[:space:]]*(,|$)'
    [[ -f $path && -r $path ]] || { log "cannot read $path"; return 1; }
    manifest=$(<"$path")
    # Read the manifest's flat string array, including inline or reflowed rows.
    # Fail closed if its layout or any element is no longer recognizable, or if
    # a key repeats: a JSON parser takes the last duplicate, this reads the first.
    local rest key key_re
    for key in napi additional; do
        key_re="\"$key\"[[:space:]]*:"
        [[ $manifest =~ $key_re ]] || continue
        rest=${manifest#*"${BASH_REMATCH[0]}"}
        [[ ! $rest =~ $key_re ]] || { log "key \"$key\" occurs more than once in $path"; return 1; }
    done
    [[ $manifest =~ $array_re ]] || { log "cannot find napi.triples.additional in $path"; return 1; }
    triples=${BASH_REMATCH[1]}
    for triple in x86_64-pc-windows-msvc x86_64-apple-darwin x86_64-unknown-linux-gnu; do
        check_triple "$triple" || return 1
    done
    while [[ ! $triples =~ ^[[:space:]]*$ ]]; do
        [[ $triples =~ $item_re ]] || { log "invalid napi.triples.additional entry in $path"; return 1; }
        triple=${BASH_REMATCH[1]}
        triples=${triples#"${BASH_REMATCH[0]}"}
        check_triple "$triple" || return 1
    done
    log 'self-test passed: every napi target has an npm platform check'
}
if [[ $# == 1 && $1 == --self-test ]]; then self_test; exit 0; fi

version=${1:-}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
shift
remote=origin
if (($#)); then
    [[ $# == 2 && $1 == --remote && -n $2 && $2 != -* ]] || usage
    remote=$2
fi
GH_BIN=${GH_BIN:-gh}
CURL_BIN=${CURL_BIN:-curl}

# Keep going after every probe: a definite publication trumps uncertainty.
verdict=safe-to-retag
report() {
    log "$1 $2"
    case $2 in
        published) verdict=published ;;
        indeterminate) if [[ $verdict != published ]]; then verdict=indeterminate; fi ;;
    esac
}
if ! ref=$(probe_output git ls-remote --tags "$remote" "refs/tags/bindings/rsac-go/v$version"); then state=indeterminate
elif [[ -n $ref ]]; then state=published
else state=clean
fi
report go-tag "$state"

REPO=${RELEASE_REPO:-}
repo_pattern='^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
if [[ -n $REPO ]]; then
    [[ $REPO =~ $repo_pattern ]] || REPO=''
elif url=$(probe_output git remote get-url "$remote"); then
    github_url='^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)$'
    url=${url%/}
    if [[ ${url%.git} =~ $github_url ]]; then REPO=${BASH_REMATCH[2]}; fi
fi
for workflow in release.yml release-npm.yml release-pypi.yml; do
    if [[ -n $REPO ]] &&
        count=$(probe_output "$GH_BIN" run list --repo "$REPO" --workflow "$workflow" --event push --branch "v$version" --limit 20 --json status --jq '[.[] | select(.status != "completed")] | length') &&
        [[ $count =~ ^0+$ ]]; then
        report "$workflow" clean
    else
        report "$workflow" indeterminate
    fi
done

probe_registry() {
    local code state
    if code=$(probe_output "$CURL_BIN" -sS -o /dev/null -w '%{http_code}' --max-time 20 -A 'rsac-release-check (https://github.com/Codeseys-Labs/rust-crossplat-audio-capture)' "$2"); then
        case $code in
            200) state=published ;;
            404) state=clean ;;
            *) state=indeterminate ;;
        esac
    else
        state=indeterminate
    fi
    report "$1" "$state"
}
probe_registry crates.io "https://crates.io/api/v1/crates/rsac/$version"
probe_registry PyPI "https://pypi.org/pypi/rsac/$version/json"
probe_registry @rsac/audio "https://registry.npmjs.org/@rsac%2Faudio/$version"
for platform in "${NPM_PLATFORMS[@]}"; do
    probe_registry "@rsac/audio-$platform" "https://registry.npmjs.org/@rsac%2Faudio-$platform/$version"
done

printf '%s %s\n' "$verdict" "$version"
case $verdict in
    safe-to-retag) exit 0 ;;
    published) exit 1 ;;
    indeterminate) exit 3 ;;
esac

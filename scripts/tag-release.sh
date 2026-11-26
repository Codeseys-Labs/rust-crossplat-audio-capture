#!/usr/bin/env bash
#
# tag-release.sh — gate a stable release, then push its annotated tags in one
# transaction. Run from the repository root on the commit to release.
#
# Usage: bash scripts/tag-release.sh X.Y.Z [--remote NAME] [--branch NAME]
#            [--run-id [KEY=]N]... [--waive KEY=[#]N]... [--no-go-tag] [--dry-run]
# Run keys: ci (default for a bare id), audio, android-emu, ios-sim.
# Waiver keys: android-emu, ios-sim, windows-switch; N is an open release-waiver issue.
# A key cannot have both a run ID and a waiver. The Windows switch note must be in HEAD.
# Defaults: origin, master, first qualifying run among the newest 20 per workflow, both tags.
# --dry-run performs every pre-tag check without creating or pushing the tags.
# GH_BIN and CARGO_BIN override the gh and cargo executables respectively.
# RELEASE_REPO overrides GitHub owner/name discovery, not a known push target.
# An unparsed push URL also requires RELEASE_ALLOW_UNPARSED_PUSH_URL=1 (test seam).
#
# Exit: 0 success; 2 usage; 10 dirty tree; 11 repository/fetch/ancestry;
# 12 tag conflict or lookup failure; 13 manifest lockstep; 14 lockfile;
# 15 changelog; 16 missing workflow evidence; 17 tag creation or atomic push
# failed; 18 incompatible Go module path; 20 missing Windows workstation evidence.
set -euo pipefail

log() { printf 'tag-release: %s\n' "$*" >&2; }
die() { log "$2"; exit "$1"; }
usage() { die 2 'usage: tag-release.sh X.Y.Z [--remote NAME] [--branch NAME] [--run-id [KEY=]N]... [--waive KEY=[#]N]... [--no-go-tag] [--dry-run]'; }

# Parallel indexed arrays keep the report order and work on macOS Bash 3.2.
keys=(ci audio android-emu ios-sim windows-switch)
files=(ci.yml ci-audio-tests.yml ci-android-emu.yml ci-ios-sim.yml)
jobs=('Release Readiness' 'Audio Release Evidence' 'Android Release Evidence' 'iOS Release Evidence')
inputs=('-f full=true -f semver=true' '-f platform=all -f release_evidence=true' '-f release_evidence=true' '-f release_evidence=true')
run_ids=('' '' '' '')
waivers=('' '' '' '' '')
WINDOWS_SWITCH_PATHS=(src/api/recovery.rs src/core/recovery.rs src/audio/windows
                      tests/ci_audio/hotplug_common.rs tests/ci_audio/hotplug_windows.rs)

key_index() {
    local i
    for i in "${!keys[@]}"; do
        if [[ $1 == "${keys[i]}" ]]; then printf '%s\n' "$i"; return 0; fi
    done
    return 1
}

version=${1:-}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
shift
remote=origin branch=master go_tag=1 dry_run=0
while (($#)); do
    case $1 in
        --remote|--branch|--run-id|--waive)
            [[ -n ${2:-} && $2 != -* ]] || usage
            case $1 in
                --remote) remote=$2 ;;
                --branch) branch=$2 ;;
                --run-id)
                    key=ci id=$2
                    if [[ $2 == *=* ]]; then key=${2%%=*}; id=${2#*=}; fi
                    i=$(key_index "$key") || usage
                    [[ $i != 4 && $id =~ ^[0-9]+$ && -z ${run_ids[i]:-} ]] || usage
                    run_ids[i]=$id ;;
                --waive)
                    [[ $2 == *=* ]] || usage
                    i=$(key_index "${2%%=*}") || usage
                    issue=${2#*=}; issue=${issue#\#}
                    [[ $i -ge 2 && $issue =~ ^[0-9]+$ && -z ${waivers[i]} ]] || usage
                    waivers[i]=$issue ;;
            esac
            shift 2 ;;
        --no-go-tag) go_tag=0; shift ;;
        --dry-run) dry_run=1; shift ;;
        *) usage ;;
    esac
done
for i in "${!run_ids[@]}"; do
    [[ -z ${run_ids[i]} || -z ${waivers[i]} ]] || die 2 "cannot specify both --run-id and --waive for ${keys[i]}"
done
GH_BIN=${GH_BIN:-gh}
CARGO_BIN=${CARGO_BIN:-cargo}
tag=v$version
tags=("$tag")
if ((go_tag)); then tags+=("bindings/rsac-go/$tag"); fi

git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null ||
    die 10 'tracked files must be clean (including the index)'
# Preserve line boundaries: command substitution normally drops *all* final
# newlines, hiding an extra URL or malformed evidence. Reject embedded NULs.
capture_output() {
    output=$("$@" 2>/dev/null | {
        local text
        if IFS= read -r -d '' text; then exit 1; fi
        printf '%s.' "$text"
    }) || return 1
    output=${output%.}
    case $output in
        $'\n'|$'\r\n') return 1 ;; # An empty line is not empty output.
        *$'\r\n') output=${output%$'\r\n'} ;;
        *) output=${output%$'\n'} ;;
    esac
}
github_repo() {
    local url=${1%/}
    local pattern='^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)$'
    [[ ${url%.git} =~ $pattern ]] || return 1
    printf '%s\n' "${BASH_REMATCH[2]}"
}
same_repo() (shopt -s nocasematch; [[ $1 == "$2" ]])

REPO=${RELEASE_REPO:-}
repo_pattern='^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
[[ -z $REPO || $REPO =~ $repo_pattern ]] || die 11 'invalid RELEASE_REPO; expected owner/name'
capture_output git remote get-url --push --all "$remote" &&
    [[ -n $output && $output != *$'\n'* ]] || die 11 "cannot determine the GitHub repository: expected exactly one push URL for $remote"
push_url=$output
if [[ -z $REPO ]]; then
    capture_output git remote get-url "$remote" && REPO=$(github_repo "$output") ||
        die 11 "cannot determine the GitHub repository for $remote; set RELEASE_REPO=owner/name"
fi
if push_repo=$(github_repo "$push_url"); then
    same_repo "$REPO" "$push_repo" || die 11 "push URL names a different repository: $push_repo (expected $REPO)"
elif [[ -z ${RELEASE_REPO:-} || ${RELEASE_ALLOW_UNPARSED_PUSH_URL:-} != 1 ]]; then
    die 11 "cannot determine the GitHub repository for the push URL of $remote"
fi
log "repository $REPO (remote $remote)"
# Fetch into the fully qualified tracking ref and test against that ref: the
# short name `<remote>/<branch>` can be shadowed by a local branch or tag, and
# a remote without a fetch refspec would leave the tracking ref stale.
tracking=refs/remotes/$remote/$branch
# Fetch follows symbolic destinations, even one pointing into refs/tags/.
if git symbolic-ref -q --no-recurse "$tracking" >/dev/null 2>&1; then
    die 11 "refusing to fetch into symbolic tracking ref $tracking"
elif [[ $? != 1 ]]; then
    die 11 "cannot inspect tracking ref $tracking"
fi
# Ignore configured ref mappings, which can send even a branch to refs/tags/.
git fetch --no-tags --refmap= "$remote" "+refs/heads/$branch:$tracking" >/dev/null 2>&1 || die 11 "cannot fetch $branch from $remote"
git merge-base --is-ancestor HEAD "$tracking" 2>/dev/null || die 11 "HEAD is not on $remote/$branch"
sha=$(git rev-parse HEAD 2>/dev/null) || die 11 'cannot resolve HEAD'
tip=$(git rev-parse --verify "$tracking" 2>/dev/null) || die 11 "cannot resolve $tracking"
for name in "${tags[@]}"; do
    if git rev-parse -q --verify "refs/tags/$name" >/dev/null 2>&1; then die 12 "tag $name exists locally"
    elif [[ $? != 1 ]]; then die 12 "cannot check local tag $name"
    fi
    capture_output git ls-remote --tags "$remote" "refs/tags/$name" || die 12 "cannot check $remote for $name"
    [[ -z $output ]] || die 12 "tag $name already exists on $remote"
done

has_go_major() {
    local file=bindings/rsac-go/go.mod line
    [[ -f $file && -r $file ]] || return 1
    while IFS= read -r line || [[ -n $line ]]; do
        if [[ $line =~ ^[[:space:]]*module[[:space:]]+ ]]; then
            [[ $line =~ ^[[:space:]]*module[[:space:]]+[^[:space:]]+/v"$major"[[:space:]]*$ ]]
            return
        fi
    done < "$file"
    return 1
}
major=${version%%.*}
if ((go_tag)) && [[ ! $major =~ ^0*[01]$ ]] && ! has_go_major; then
    die 18 "bindings/rsac-go/go.mod needs a module path ending in /v$major; use --no-go-tag to omit the Go tag"
fi
bash scripts/check-version-lockstep.sh --expect "$version" >/dev/null 2>&1 || die 13 "manifest lockstep check failed for $version"
"$CARGO_BIN" metadata --locked --format-version 1 >/dev/null 2>&1 || die 14 'cargo metadata --locked failed; check the committed Cargo.lock'

has_changelog_heading() {
    local line
    [[ -f CHANGELOG.md && -r CHANGELOG.md ]] || return 1
    while IFS= read -r line || [[ -n $line ]]; do
        if [[ ${line%$'\r'} =~ ^"## [$version] - "[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then return 0; fi
    done < CHANGELOG.md
    return 1
}
has_changelog_heading || die 15 "missing dated heading for [$version] in CHANGELOG.md"

workflow_name() {
    local file=.github/workflows/$1 line name
    [[ -f $file && -r $file ]] || return 1
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line == name:* ]] || continue
        name=${line#name:}
        name=${name#"${name%%[![:space:]]*}"}
        name=${name%"${name##*[![:space:]]}"}
        case $name in
            \"*\"|\'*\') name=${name:1:${#name}-2} ;;
        esac
        [[ $name == *[![:space:]]* ]] || return 1
        printf '%s\n' "$name"
        return 0
    done < "$file"
    return 1
}

find_run() {
    local i=$1 file=${files[$1]} job=${jobs[$1]} ids=${run_ids[$1]}
    local expected filter id head status conclusion event readiness workflow newest=''
    local limit=20
    local -a candidates=()
    local fields='^(([^[:space:]]+ ){4}[^[:space:]]+) (.*[^[:space:]].*)$'
    expected=$(workflow_name "$file") || { reason='cannot determine workflow name'; return 1; }
    if [[ -z $ids ]]; then
        capture_output "$GH_BIN" run list --repo "$REPO" --workflow "$file" --branch "$branch" --event workflow_dispatch --commit "$sha" --limit "$limit" --json databaseId --jq '.[].databaseId' ||
            { reason='could not list runs'; return 1; }
        ids=$output
        [[ -n $ids ]] || { reason='no run found'; return 1; }
    fi
    while IFS= read -r id; do
        id=${id%$'\r'}
        [[ $id =~ ^[0-9]+$ ]] || { reason='unexpected run id from gh run list'; return 1; }
        candidates+=("$id")
        if ((${#candidates[@]} == limit)); then break; fi
    done <<< "$ids"
    filter='def result: if . == null or . == "" then "none" else . end;
        [.headSha, .status, (.conclusion | result), .event,
         ([(.jobs // [])[] | select(.name == "'"$job"'")] |
          if length == 0 then "missing" else (.[0].conclusion | result) end), .workflowName] | join(" ")'
    for id in "${candidates[@]}"; do
        capture_output "$GH_BIN" run view "$id" --repo "$REPO" --json headSha,status,conclusion,event,jobs,workflowName --jq "$filter" ||
            { reason="could not inspect run $id"; return 1; }
        if [[ $output == *$'\n'* || ! $output =~ $fields ]]; then
            reason="run $id: expected one line with six fields"; return 1
        fi
        workflow=${BASH_REMATCH[3]}
        read -r head status conclusion event readiness <<< "${BASH_REMATCH[1]}"
        if [[ $head != "$sha" ]]; then reason='wrong commit'
        elif [[ $status != completed ]]; then reason='still running'
        elif [[ $conclusion != success ]]; then reason='failed'
        elif [[ $event != workflow_dispatch ]]; then reason='not dispatched'
        elif [[ $workflow != "$expected" ]]; then reason="not the $file workflow"
        elif [[ $readiness == missing ]]; then reason="no $job job"
        elif [[ $readiness != success ]]; then reason="$job failed"
        else run_ids[i]=$id; return 0
        fi
        if [[ -z $newest ]]; then newest=$reason; fi
    done
    reason=$newest
    return 1
}

check_windows_switch() {
    local note=docs/evidence/windows-default-switch.md
    local stamp='^Tested-SHA: ([0-9a-f]{40})$'
    reason="expected exactly one Tested-SHA in $note"
    # Match the exact committed path: a directory of that name is not a note.
    capture_output git grep --no-color --no-line-number --no-column -IHE '^Tested-SHA: [0-9a-f]{40}'$'\r?$' "$sha" -- "$note" &&
        [[ $output == "$sha:$note:"* && ${output#"$sha:$note:"} =~ $stamp ]] || return 1
    tested_sha=${BASH_REMATCH[1]}
    reason="$tested_sha is not an ancestor commit of HEAD"
    capture_output git cat-file -t "$tested_sha" && [[ $output == commit ]] &&
        git merge-base --is-ancestor "$tested_sha" HEAD 2>/dev/null || return 1
    if git diff --quiet "$tested_sha" HEAD -- "${WINDOWS_SWITCH_PATHS[@]}" 2>/dev/null; then return 0; fi
    reason="cannot verify Windows switch paths since $tested_sha"
    capture_output git diff --name-only "$tested_sha" HEAD -- "${WINDOWS_SWITCH_PATHS[@]}" && [[ -n $output ]] || return 1
    reason="changed: ${output//$'\n'/, }; rerun the workstation test and update the note, or waive with --waive windows-switch=<issue>"
    return 1
}

evaluate() {
    local i=$1 issue=${waivers[$1]} reason
    if [[ -n $issue ]]; then
        if capture_output "$GH_BIN" issue view "$issue" --repo "$REPO" --json state,labels --jq '[.state, ([.labels[].name] | index("release-waiver") != null)] | join(" ")' && [[ $output == 'OPEN true' ]]; then
            reports[i]="waived #$issue"; return
        fi
        reason="waiver issue #$issue is not an open release-waiver issue"
    elif [[ $i == 4 ]]; then
        if check_windows_switch; then reports[i]="ok $tested_sha"; return; fi
    elif find_run "$i"; then
        reports[i]="ok run ${run_ids[i]}"; return
    fi
    reports[i]="missing: $reason"
}

tested_sha=''
evaluate 4
for i in "${!files[@]}"; do evaluate "$i"; done
for i in "${!keys[@]}"; do log "evidence ${keys[i]} ${reports[i]}"; done

missing=0
for i in "${!files[@]}"; do
    [[ ${reports[i]} == missing:* ]] || continue
    missing=1
    if [[ $sha == "$tip" ]]; then
        log "dispatch: gh workflow run ${files[i]} --repo $REPO --ref $branch ${inputs[i]}"
    else
        log "a dispatch on $branch tests the branch tip, not this commit; tag the tip, or dispatch ${files[i]} on a ref at this commit with ${inputs[i]} and pass --run-id ${keys[i]}=<N>"
    fi
done
if ((missing)); then exit 16; fi
if [[ ${reports[4]} == missing:* ]]; then exit 20; fi

body=''
for i in "${!files[@]}"; do
    if [[ -z ${waivers[i]} ]]; then body+=$'\n'"Evidence: ${keys[i]}=${run_ids[i]}"; fi
done
if [[ -z ${waivers[4]} ]]; then body+=$'\n'"Evidence: windows-switch=$tested_sha"; fi
for i in 2 3 4; do
    if [[ -n ${waivers[i]} ]]; then
        body+=$'\n'"Waived: ${keys[i]}=#${waivers[i]}"
        log "WAIVED ${keys[i]} (#${waivers[i]})"
    fi
done
crate_message="rsac $version"$'\n'"$body"
go_message="rsac-go $version"$'\n'"$body"

if ((dry_run)); then
    log "would tag $sha and push ${tags[*]} to $remote atomically"
    log "$crate_message"
    if ((go_tag)); then log "$go_message"; fi
    printf 'dry-run ok %s at %s\n' "$tag" "$sha"
    exit 0
fi

remove_tags() {
    git tag -d "$@" >/dev/null 2>&1 || log "could not delete local tags: $*; inspect them before retrying"
}
# A local core.commentChar must not strip evidence or waiver lines.
git tag -a "$tag" --cleanup=verbatim -m "$crate_message" "$sha" >/dev/null 2>&1 || die 17 "cannot create $tag"
if ((go_tag)) && ! git tag -a "${tags[1]}" --cleanup=verbatim -m "$go_message" "$sha" >/dev/null 2>&1; then
    remove_tags "$tag"
    die 17 "cannot create ${tags[1]}"
fi
if ! git push --atomic --no-follow-tags "$remote" "${tags[@]}" >/dev/null 2>&1; then
    remove_tags "${tags[@]}"
    die 17 "atomic push to $remote failed"
fi
printf 'tagged %s at %s\n' "$tag" "$sha"

#!/usr/bin/env bash
#
# tag-release.sh — gate a manual stable release, then push its annotated tags
# in one transaction. Run from the repository root on the commit to release.
#
# Usage: bash scripts/tag-release.sh X.Y.Z [--remote NAME] [--branch NAME]
#            [--run-id N] [--no-go-tag] [--dry-run]
# Defaults: origin, master, newest qualifying CI run, both tags.
# --dry-run performs every pre-tag check without creating or pushing the tags.
# GH_BIN and CARGO_BIN override the gh and cargo executables respectively.
# RELEASE_REPO overrides the GitHub owner/name derived from --remote.
#
# Exit: 0 success; 2 usage; 10 dirty tree; 11 repository/fetch/ancestry;
# 12 tag conflict or lookup failure; 13 manifest lockstep; 14 lockfile;
# 15 changelog; 16 no qualifying CI evidence; 17 tag creation or atomic
# push failed; 18 incompatible Go module path for the requested major.
set -euo pipefail

log() { printf 'tag-release: %s\n' "$*" >&2; }
die() { log "$2"; exit "$1"; }
usage() { die 2 'usage: tag-release.sh X.Y.Z [--remote NAME] [--branch NAME] [--run-id N] [--no-go-tag] [--dry-run]'; }

version=${1:-}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
shift
remote=origin branch=master run_id='' go_tag=1 dry_run=0
while (($#)); do
    case $1 in
        --remote|--branch|--run-id)
            [[ -n ${2:-} && $2 != -* ]] || usage
            case $1 in
                --remote) remote=$2 ;;
                --branch) branch=$2 ;;
                --run-id) [[ $2 =~ ^[0-9]+$ ]] || usage; run_id=$2 ;;
            esac
            shift 2 ;;
        --no-go-tag) go_tag=0; shift ;;
        --dry-run) dry_run=1; shift ;;
        *) usage ;;
    esac
done
GH_BIN=${GH_BIN:-gh}
CARGO_BIN=${CARGO_BIN:-cargo}
tag=v$version
tags=("$tag")
if ((go_tag)); then tags+=("bindings/rsac-go/$tag"); fi

git diff --quiet 2>/dev/null && git diff --cached --quiet 2>/dev/null ||
    die 10 'tracked files must be clean (including the index)'
REPO=${RELEASE_REPO:-}
if [[ -z $REPO ]] && url=$(git remote get-url "$remote" 2>/dev/null); then
    github_url='^(https://github\.com/|git@github\.com:|ssh://git@github\.com/)([A-Za-z0-9_-]+/[A-Za-z0-9_.-]+)$'
    url=${url%/}
    if [[ ${url%.git} =~ $github_url ]]; then REPO=${BASH_REMATCH[2]}; fi
fi
[[ -n $REPO ]] || die 11 "cannot determine the GitHub repository for $remote; set RELEASE_REPO=owner/name"
# Fetch into the fully qualified tracking ref and test against that ref: the
# short name `<remote>/<branch>` can be shadowed by a local branch or tag, and
# a remote without a fetch refspec would leave the tracking ref stale.
tracking=refs/remotes/$remote/$branch
git fetch --tags "$remote" "+refs/heads/$branch:$tracking" >/dev/null 2>&1 || die 11 "cannot fetch $branch and tags from $remote"
git merge-base --is-ancestor HEAD "$tracking" 2>/dev/null || die 11 "HEAD is not on $remote/$branch"
sha=$(git rev-parse HEAD 2>/dev/null) || die 11 'cannot resolve HEAD'
tip=$(git rev-parse --verify "$tracking" 2>/dev/null) || die 11 "cannot resolve $tracking"
for name in "${tags[@]}"; do
    if git rev-parse -q --verify "refs/tags/$name" >/dev/null 2>&1; then die 12 "local tag $name already exists"; fi
    ref=$(git ls-remote --tags "$remote" "refs/tags/$name" 2>/dev/null) || die 12 "cannot check $remote for $name"
    [[ -z $ref ]] || die 12 "tag $name already exists on $remote"
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

ci_failure() {
    log "$1"
    if [[ $sha != "$tip" ]]; then
        die 16 "a dispatch on $branch tests the branch tip, not this commit; tag the tip, or dispatch ci.yml on a ref at this commit with full=true and semver=true and pass --run-id"
    fi
    die 16 "dispatch: gh workflow run ci.yml --ref $branch -f full=true -f semver=true"
}
ci_workflow_name() {
    local file=.github/workflows/ci.yml line name
    [[ -f $file && -r $file ]] || return 1
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line == name:* ]] || continue
        name=${line#name:}
        name=${name#"${name%%[![:space:]]*}"}
        name=${name%"${name##*[![:space:]]}"}
        case $name in
            \"*\"|\'*\') name=${name:1:${#name}-2} ;;
        esac
        [[ -n $name ]] || return 1
        printf '%s\n' "$name"
        return 0
    done < "$file"
    return 1
}
ci_name=$(ci_workflow_name) || ci_failure 'cannot determine workflow name from .github/workflows/ci.yml'
if [[ -n $run_id ]]; then
    ids=$run_id
else
    ids=$("$GH_BIN" run list --repo "$REPO" --workflow ci.yml --branch "$branch" --event workflow_dispatch --commit "$sha" --limit 20 --json databaseId --jq '.[].databaseId' 2>/dev/null) || ci_failure 'could not list CI runs'
fi
view_filter='def result: if . == null or . == "" then "none" else . end;
    [.headSha, .status, (.conclusion | result), .event,
     ([(.jobs // [])[] | select(.name == "Release Readiness")] |
      if length == 0 then "missing" else (.[0].conclusion | result) end), .workflowName] | join(" ")'
# Keep the sixth field verbatim: workflow names can contain spaces.
view_fields='^(([^[:space:]]+ ){4}[^[:space:]]+) (.*[^[:space:]].*)$'
qualified=0 inspected=0
while IFS= read -r id; do
    id=${id%$'\r'}
    [[ -n $id ]] || continue
    [[ $id =~ ^[0-9]+$ ]] || ci_failure 'unexpected run id from gh run list'
    inspected=$((inspected + 1))
    # Preserve line boundaries, including a spurious second empty line.
    view=$("$GH_BIN" run view "$id" --repo "$REPO" --json headSha,status,conclusion,event,jobs,workflowName --jq "$view_filter" 2>/dev/null && printf '.') || ci_failure "could not inspect run $id"
    view=${view%.}; view=${view%$'\n'}; view=${view%$'\r'}
    if [[ $view == *$'\n'* || ! $view =~ $view_fields ]]; then ci_failure "run $id: expected one line with six fields"; fi
    workflow=${BASH_REMATCH[3]}
    read -r head status conclusion event readiness <<< "${BASH_REMATCH[1]}"
    if [[ $head != "$sha" ]]; then reason='wrong commit'
    elif [[ $status != completed ]]; then reason="still running (status: $status)"
    elif [[ $conclusion != success ]]; then reason="failed (conclusion: $conclusion)"
    elif [[ $event != workflow_dispatch ]]; then reason="not dispatched (event: $event)"
    elif [[ $workflow != "$ci_name" ]]; then reason='not the ci.yml workflow'
    elif [[ $readiness == missing ]]; then reason='no Release Readiness job'
    elif [[ $readiness != success ]]; then reason="Release Readiness failed (conclusion: $readiness)"
    else qualified=1; break
    fi
    log "run $id: $reason"
done <<< "$ids"
if ((!qualified)); then
    if ((inspected)); then ci_failure "no qualifying run among $inspected for $sha"; fi
    ci_failure "no run found for $sha on $branch"
fi

if ((dry_run)); then
    log "would tag $sha and push ${tags[*]} to $remote atomically"
    printf 'dry-run ok %s at %s\n' "$tag" "$sha"
    exit 0
fi

remove_tags() {
    git tag -d "$@" >/dev/null 2>&1 || log "could not delete local tags: $*; inspect them before retrying"
}
git tag -a "$tag" -m "rsac $version" "$sha" >/dev/null 2>&1 || die 17 "cannot create $tag"
if ((go_tag)) && ! git tag -a "${tags[1]}" -m "rsac-go $version" "$sha" >/dev/null 2>&1; then
    remove_tags "$tag"
    die 17 "cannot create ${tags[1]}"
fi
if ! git push --atomic --no-follow-tags "$remote" "${tags[@]}" >/dev/null 2>&1; then
    remove_tags "${tags[@]}"
    die 17 "atomic push to $remote failed"
fi
printf 'tagged %s at %s\n' "$tag" "$sha"

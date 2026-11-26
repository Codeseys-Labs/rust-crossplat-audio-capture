#!/usr/bin/env bash
#
# tag-release.test.sh — contract tests for scripts/tag-release.sh
#
# Written from the release-tooling contract ONLY: section 4 describes the script, section 7 these
# tests, the revision notes r2 and r3 amend section 4, and the release-gate contract amends it
# again: revision 4 (clauses R4.1 to R4.6; R4.11 describes these tests), its Revision 5
# (R5.1 to R5.5, R5.7) and its Revision 6 (R6.1 to R6.4; R6.4 describes these tests), each of
# which overrides the one before where it differs. The script under test was
# not read while writing them. Every expectation names the contract clause it comes from. A
# failing case stays failing: never weaken an expectation to match observed output; fix the script
# or report the contract problem. A case whose contract text can be read two ways carries an
# `AMBIGUOUS(<clause>)` comment naming both readings (the chosen one is the most literal).
#
# ── What a case gets ────────────────────────────────────────────────────────
#   * a throwaway git repository (branch master, two commits pushed) whose remote `origin` is a
#     bare repository, inside a `mktemp -d` workspace removed on exit. The first commit holds the
#     sources and workflows, the second (HEAD) adds docs/evidence/windows-default-switch.md whose
#     Tested-SHA names the first, so R4.5 passes unless a case breaks it;
#   * a committed stub scripts/check-version-lockstep.sh whose exit status the case controls
#     ($FAKE_DIR/lockstep.rc) and which logs its arguments;
#   * a committed CHANGELOG.md, bindings/rsac-go/go.mod and the four evidence workflow files
#     (ci.yml, ci-audio-tests.yml, ci-android-emu.yml, ci-ios-sim.yml, each with a `name:` line);
#   * fake `gh` and `cargo` executables reached only through GH_BIN / CARGO_BIN. Real
#     gh/cargo/curl are shadowed on PATH by "poison" executables, so a script that ignores a seam
#     is caught (and can never reach a real GitHub or registry).
#   * git is isolated from the user's configuration (HOME, system config) and the throwaway
#     repository sets identity and gpgsign=false locally.
#
# ── The fake gh and the "world" of a case ────────────────────────────────────
#   The fake gh answers three call forms and rejects (exit 97, and the case fails) everything else
#   and any call whose `--repo` is not right after the subcommand words or names another
#   repository than $FAKE_EXPECT_REPO (R3.1; the harness exports it as RELEASE_REPO because the
#   throwaway remote is a local bare repository, not a GitHub URL):
#     run list --repo R --workflow <file> ...   stdout of gh-list-<key>.out, status gh-list-<key>.rc
#     run view <id> --repo R ...                stdout of gh-view-<id>.out,   status gh-view-<id>.rc
#     issue view <N> --repo R ...               stdout of gh-issue-<N>.out,   status gh-issue-<N>.rc
#   <file> is one of the four workflows of the R4.3 table; <key> is ci, audio, android-emu or
#   ios-sim. The --jq expression is ignored (the case supplies what it would print). Every call
#   is logged, one line, to $FAKE_DIR/gh.log so a case can assert the exact call shape.
#   A case states its world with one `world` call, a table in the order of R4.3:
#
#       world  ci=<spec>  audio=<spec>  android-emu=<spec>  ios-sim=<spec>
#
#   Every key lists one run (its default id: ci 4242, audio 4343, android-emu 4444, ios-sim 4545)
#   whose `gh run view` line follows the spec; `none` lists no run. The specs are defined once, at
#   spec_line below (green, wrong-commit, queued, failed, event-push, job-missing, ...), each with
#   the reason phrase R4.3 prescribes (spec_phrase). `arrange_green` is `world` with every key
#   green. Waiver issues are arranged with issue_set / honour_waiver (default: no such issue).
#
# ── Giving a remote a GitHub URL without network (R3.1 / R4.1 cases) ─────────
#   The script must derive <owner>/<name> from `git remote get-url`, while fetch, ls-remote and
#   push must keep working against the local bare repository.
#   `url.<local path>.insteadOf` cannot do that: `git remote get-url` EXPANDS insteadOf, so the
#   script would see the local path (verified). Instead the harness sets `remote.<name>.url` to
#   the GitHub-form URL and `remote.<name>.vcs=rsacproxy`; the tiny remote helper
#   `git-remote-rsacproxy` (on PATH, created below) then serves every fetch, ls-remote and push
#   from the bare repository named by $RSAC_PROXY_REPO, whatever URL (fetch or push) git was
#   given. `git remote get-url` reports the GitHub form, and git can never open a network
#   connection for that remote.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   bash scripts/tests/tag-release.test.sh
#
# Running ONE case (or a few): set RSAC_TEST_FILTER to a substring of the case name.
#   RSAC_TEST_FILTER=tree-dirty-staged bash scripts/tests/tag-release.test.sh
#   List the names (loop-generated ones show as "$k"/"$spec" in the file; run once to see them):
#     grep -o '^run_case [^ ]*' scripts/tests/tag-release.test.sh
# A filter that selects no case prints `FAIL no case name contains
# RSAC_TEST_FILTER=<filter>` and the file exits non-zero (R3.8).
#
# Output: one line per case (`ok <name>` / `FAIL <name>: <why>`), then a summary
# line. Exit status: 0 = every case passed, 1 = at least one case failed. A case
# whose script under test does not exist FAILs (it is never skipped).
#
# Portability: bash 3.2 compatible; no jq, python or node; runs under Git Bash.

set -uo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_NAME="tag-release.sh"
SCRIPT="$REPO_ROOT/scripts/$SCRIPT_NAME"

# The version under test; reset to the default after every case (use_version changes it).
DEFAULT_VER="1.2.3"
use_version() { VER="$1"; CRATE_TAG="v$VER"; GO_TAG="bindings/rsac-go/v$VER"; }
use_version "$DEFAULT_VER"

# The repository every fake-gh call must name (R3.1); the harness exports it as RELEASE_REPO.
TEST_REPO="rsac-test-owner/rsac-test-name"
GO_MODULE="github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go"
OTHER_SHA="0123456789abcdef0123456789abcdef01234567" # a full sha that is not HEAD
NO_SUCH_SHA="1234567890abcdef1234567890abcdef12345678" # a full sha that names no object

# ── the table of R4.3 ───────────────────────────────────────────────────────
# Key | workflow file | required job name | dispatch inputs. (The workflow `name:` values below are the
# throwaway repository's own: R4.3 reads the expected name from the working tree.)
KEYS="ci audio android-emu ios-sim"
key_file() {
  case "$1" in
    ci) echo ci.yml ;;
    audio) echo ci-audio-tests.yml ;;
    android-emu) echo ci-android-emu.yml ;;
    ios-sim) echo ci-ios-sim.yml ;;
  esac
}
key_job() {
  case "$1" in
    ci) echo "Release Readiness" ;;
    audio) echo "Audio Release Evidence" ;;
    android-emu) echo "Android Release Evidence" ;;
    ios-sim) echo "iOS Release Evidence" ;;
  esac
}
key_inputs() {
  case "$1" in
    ci) echo "-f full=true -f semver=true" ;;
    audio) echo "-f platform=all -f release_evidence=true" ;;
    android-emu | ios-sim) echo "-f release_evidence=true" ;;
  esac
}
key_wfname() {
  case "$1" in
    ci) echo "CI" ;;
    audio) echo "CI Audio Tests" ;;
    android-emu) echo "CI Android Emulator" ;;
    ios-sim) echo "CI iOS Simulator" ;;
  esac
}
key_id() { # the default run id of a key's world
  case "$1" in
    ci) echo 4242 ;;
    audio) echo 4343 ;;
    android-emu) echo 4444 ;;
    ios-sim) echo 4545 ;;
  esac
}
key_var() { echo "${1//-/_}"; }

# R4.5: the script's WINDOWS_SWITCH_PATHS
WINDOWS_SWITCH_PATHS="src/api/recovery.rs src/core/recovery.rs src/audio/windows tests/ci_audio/hotplug_common.rs tests/ci_audio/hotplug_windows.rs"
NOTE_PATH="docs/evidence/windows-default-switch.md"
TESTED_SHA="" # set by build_template: the first commit, which the committed note names

# What a success run must report and write (R4.3 report lines, R4.6 tag message). Reset per case.
# EV_<key>: the run id (windows-switch: the Tested-SHA) of a satisfied requirement; WV_<key>: the issue
# number of an honoured waiver. A key has one or the other.
ev_get() { local n="EV_$(key_var "$1")"; printf '%s' "${!n}"; }
ev_set() { printf -v "EV_$(key_var "$1")" '%s' "$2"; }
wv_get() { local n="WV_$(key_var "$1")"; printf '%s' "${!n}"; }
wv_set() { printf -v "WV_$(key_var "$1")" '%s' "$2"; }
reset_expectations() {
  local k
  for k in $KEYS; do
    ev_set "$k" "$(key_id "$k")"
    wv_set "$k" ""
  done
  ev_set windows-switch "$TESTED_SHA"
  wv_set windows-switch ""
  EXP_REPO="$TEST_REPO"
  EXP_REMOTE=origin
  EXP_BRANCH=master
}

# git must behave identically everywhere: no inherited repository, no user/system
# configuration, no prompts, byte-wise sorting.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR GIT_OBJECT_DIRECTORY \
      GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_CEILING_DIRECTORIES
export LC_ALL=C GIT_TERMINAL_PROMPT=0 GIT_CONFIG_NOSYSTEM=1
# Defence in depth: whatever a script (or a misconfigured case) does, git can reach neither an ssh host nor an http(s)
# proxy target. The cases that use GitHub-form URLs serve them through the local remote helper below.
export GIT_SSH_COMMAND=false https_proxy=http://127.0.0.1:9 http_proxy=http://127.0.0.1:9 HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9
unset ALL_PROXY all_proxy NO_PROXY no_proxy

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config"
mkdir -p "$HOME"

# ── fake and poison executables ─────────────────────────────────────────────
mkdir -p "$WORK/bin" "$WORK/poison" "$WORK/helper"

# Fake GitHub CLI: see the header. Missing --repo, --repo not right after the subcommand words, or another
# repository: exit 97 and a line in $FAKE_DIR/gh.unexpected (post_checks then fails the case). An issue or
# a run with no file answers exit 1.
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
want_repo="${FAKE_EXPECT_REPO:?FAKE_EXPECT_REPO not set}"
a="$*"
a="${a//$'\n'/ }"
printf '%s\n' "$a" >> "$d/gh.log"
unexpected() { # <why>
  printf '%s (%s)\n' "$a" "$1" >> "$d/gh.unexpected"
  echo "fake gh: rejected ($1): $a" >&2
  exit 97
}
same_repo() { # <repository as given>
  [ "$1" = "$want_repo" ]
}
reply() { # <name>: stdout from gh-<name>.out, exit status from gh-<name>.rc (default 0)
  local rc=0
  [ -f "$d/gh-$1.out" ] && cat "$d/gh-$1.out"
  [ -f "$d/gh-$1.rc" ] && rc="$(cat "$d/gh-$1.rc")"
  [ "$rc" -ne 0 ] && echo "fake gh: failing on request ($1)" >&2
  exit "$rc"
}
if [ "${1:-}" = run ] && [ "${2:-}" = list ]; then
  [ "${3:-}" = --repo ] || unexpected "no --repo right after 'run list'"
  same_repo "${4:-}" || unexpected "--repo is '${4:-}', expected '$want_repo'"
  wf=""
  prev=""
  for x in "$@"; do
    [ "$prev" = --workflow ] && wf="$x"
    prev="$x"
  done
  case "$wf" in
    ci.yml) k=ci ;;
    ci-audio-tests.yml) k=audio ;;
    ci-android-emu.yml) k=android-emu ;;
    ci-ios-sim.yml) k=ios-sim ;;
    *) unexpected "workflow '$wf' is not one of the four evidence workflows" ;;
  esac
  reply "list-$k"
fi
if [ "${1:-}" = run ] && [ "${2:-}" = view ] && [ -n "${3:-}" ]; then
  [ "${4:-}" = --repo ] || unexpected "no --repo right after 'run view <id>'"
  same_repo "${5:-}" || unexpected "--repo is '${5:-}', expected '$want_repo'"
  if [ ! -f "$d/gh-view-$3.out" ] && [ ! -f "$d/gh-view-$3.rc" ]; then
    echo "fake gh: no such run: $3" >&2
    exit 1
  fi
  reply "view-$3"
fi
if [ "${1:-}" = issue ] && [ "${2:-}" = view ] && [ -n "${3:-}" ]; then
  [ "${4:-}" = --repo ] || unexpected "no --repo right after 'issue view <N>'"
  same_repo "${5:-}" || unexpected "--repo is '${5:-}', expected '$want_repo'"
  if [ ! -f "$d/gh-issue-$3.out" ] && [ ! -f "$d/gh-issue-$3.rc" ]; then
    echo "fake gh: no such issue: $3" >&2
    exit 1
  fi
  reply "issue-$3"
fi
unexpected "not one of the contract forms"
EOF

# Fake cargo: logs arguments, prints `{}`, exits with $FAKE_DIR/cargo.rc (default 0).
cat > "$WORK/bin/cargo" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
printf '%s\n' "$*" >> "$d/cargo.log"
rc=0
[ -f "$d/cargo.rc" ] && rc="$(cat "$d/cargo.rc")"
if [ "$rc" -ne 0 ]; then
  echo "fake cargo: failing on request" >&2
  exit "$rc"
fi
echo '{}'
EOF

# Stub for the committed scripts/check-version-lockstep.sh.
cat > "$WORK/bin/lockstep-stub.sh" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
printf '%s\n' "$*" >> "$d/lockstep.log"
rc=0
[ -f "$d/lockstep.rc" ] && rc="$(cat "$d/lockstep.rc")"
exit "$rc"
EOF

# Gate stub for the lockstep script inside a RELEASE_GATE_DIR (R6.1): logs its arguments AND its
# working directory (tab-separated; R6.1a: the gate's script runs with the commit's checkout as
# cwd), and exits with $FAKE_DIR/gate-lockstep.rc (default 0).
cat > "$WORK/bin/gate-lockstep-stub.sh" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
printf '%s\t%s\n' "$PWD" "$*" >> "$d/gate-lockstep.log"
rc=0
[ -f "$d/gate-lockstep.rc" ] && rc="$(cat "$d/gate-lockstep.rc")"
exit "$rc"
EOF

# gpg replacement that always fails (used to make `git tag -a` fail).
cat > "$WORK/bin/failing-gpg" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF

# Remote helper: serves every transport request of a remote that has `remote.<name>.vcs=rsacproxy`
# from the bare repository $RSAC_PROXY_REPO (see the header comment).
cat > "$WORK/helper/git-remote-rsacproxy" <<'EOF'
#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    capabilities) printf 'connect\n\n' ;;
    connect\ *)
      svc="${line#connect git-}"
      printf '\n'
      exec git "$svc" "$RSAC_PROXY_REPO"
      ;;
    '') exit 0 ;;
  esac
done
EOF

# Poison: stand-ins for the real tools on PATH. Reached only if a script ignores a seam.
for tool in gh cargo curl; do
  cat > "$WORK/poison/$tool" <<'EOF'
#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "${FAKE_DIR:?FAKE_DIR not set}/poison.log"
echo "poison: a real tool was invoked instead of the test seam" >&2
exit 99
EOF
done
chmod +x "$WORK"/bin/* "$WORK"/poison/* "$WORK"/helper/*

case "$PATH" in *\;*) PATH_SEP=';' ;; *) PATH_SEP=':' ;; esac
PATH="$WORK/poison$PATH_SEP$WORK/helper$PATH_SEP$PATH"
export GH_BIN="$WORK/bin/gh" CARGO_BIN="$WORK/bin/cargo"

# ── result bookkeeping ──────────────────────────────────────────────────────
PASSED=0
FAILED=0
SELECTED=0
CASE_N=0
CASE_NAME=""
FAILS=""
CASE_ABORT=0

fail() { # <why>: record a failure for the running case (muted once the case is aborted)
  [ "$CASE_ABORT" = 1 ] && return 0
  FAILS="${FAILS:+$FAILS;} $*"
  return 0
}

harness_die() { # unrecoverable harness problem outside any case
  echo "harness error: $*" >&2
  exit 2
}

must() { # <cmd...>: harness setup step; a failure aborts the running case
  local out
  if ! out="$("$@" 2>&1)"; then
    fail "harness setup failed: $* -> $out"
    CASE_ABORT=1
    return 1
  fi
  return 0
}

# ── template repository (built once, copied per case) ───────────────────────
TEMPLATE="$WORK/template"
TEMPLATE_SOURCES="src/lib.rs src/api/recovery.rs src/core/recovery.rs src/audio/windows/mod.rs src/audio/macos/mod.rs tests/ci_audio/hotplug_common.rs tests/ci_audio/hotplug_windows.rs"
build_template() {
  local t="$TEMPLATE" f k
  mkdir -p "$t/repo/scripts" "$t/repo/.github/workflows" "$t/repo/bindings/rsac-go" "$t/repo/docs/evidence" || harness_die "mkdir"
  git init -q --bare "$t/remote.git" || harness_die "git init --bare"
  git -C "$t/remote.git" symbolic-ref HEAD refs/heads/master || harness_die "bare HEAD"
  git init -q "$t/repo" || harness_die "git init"
  git -C "$t/repo" symbolic-ref HEAD refs/heads/master || harness_die "repo HEAD"
  local kv
  for kv in user.name=rsac-test user.email=rsac-test@example.invalid commit.gpgsign=false \
            tag.gpgsign=false tag.forceSignAnnotated=false core.autocrlf=false; do
    git -C "$t/repo" config "${kv%%=*}" "${kv#*=}" || harness_die "git config $kv"
  done
  cp "$WORK/bin/lockstep-stub.sh" "$t/repo/scripts/check-version-lockstep.sh" || harness_die "stub"
  printf '# rsac test repository\n' > "$t/repo/README.md"
  for f in $TEMPLATE_SOURCES; do
    mkdir -p "$(dirname "$t/repo/$f")" || harness_die "mkdir $f"
    printf '// %s\n' "$f" > "$t/repo/$f" || harness_die "write $f"
  done
  cat > "$t/repo/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

## [10.0.0] - 2026-06-01

## [3.1.4] - 2026-05-01

## [3.0.0] - 2026-04-01

## [2.0.0] - 2026-03-01

## [1.9.0] - 2026-02-01

## [1.2.3] - 2026-01-02

- A change.

## [1.2.2] - 2025-12-01

- An older change.

## [0.5.0] - 2025-06-01
EOF
  # R3.2 / R4.3: the run's workflow name must equal the `name:` of the workflow file in the working tree.
  for k in $KEYS; do
    printf 'name: %s\n\non:\n  workflow_dispatch:\n\njobs:\n  evidence:\n    name: %s\n    runs-on: ubuntu-latest\n' \
      "$(key_wfname "$k")" "$(key_job "$k")" > "$t/repo/.github/workflows/$(key_file "$k")" || harness_die "workflow $k"
  done
  # R3.4: a module path without a major-version suffix (fine for majors 0 and 1).
  printf 'module %s\n\ngo 1.21\n' "$GO_MODULE" > "$t/repo/bindings/rsac-go/go.mod"
  git -C "$t/repo" add -A || harness_die "git add"
  git -C "$t/repo" commit -q -m "initial" || harness_die "git commit"
  TESTED_SHA="$(git -C "$t/repo" rev-parse HEAD)"
  # R4.5: the note names the commit above (an ancestor of HEAD) and no path of WINDOWS_SWITCH_PATHS changes after it.
  printf '# Windows default-device switch evidence\n\nTested-SHA: %s\n' "$TESTED_SHA" > "$t/repo/$NOTE_PATH"
  git -C "$t/repo" add -A || harness_die "git add note"
  git -C "$t/repo" commit -q -m "evidence note" || harness_die "git commit note"
  git -C "$t/repo" remote add origin "$t/remote.git" || harness_die "git remote add"
  git -C "$t/repo" push -q origin master || harness_die "git push"
}
build_template

new_sandbox() {
  CASE_N=$((CASE_N + 1))
  SB="$WORK/c$CASE_N"
  REPO="$SB/repo"
  REMOTE="$SB/remote.git"
  UPSTREAM="$SB/upstream.git"
  FAKE_DIR="$SB/fake"
  export FAKE_DIR
  mkdir -p "$FAKE_DIR" || harness_die "mkdir sandbox"
  cp -R "$TEMPLATE/remote.git" "$REMOTE" || harness_die "copy remote"
  cp -R "$TEMPLATE/repo" "$REPO" || harness_die "copy repo"
  git -C "$REPO" remote set-url origin "$REMOTE" || harness_die "set-url"
  SHA="$(git -C "$REPO" rev-parse HEAD)"
  RC=""
  unset RELEASE_GATE_DIR # R6.1: the trusted gate directory; a case that wants one calls use_gate_dir
  GATE_DIR=""
  RSAC_PROXY_REPO="$REMOTE"
  FAKE_EXPECT_REPO="$TEST_REPO"
  RELEASE_REPO="$TEST_REPO" # R3.1 seam: the throwaway remote is a local path, not a GitHub URL
  # R5.3 seam: RELEASE_REPO is set and the push URL (the local-path fetch URL) does not parse as a GitHub
  # URL, which R5.3 accepts only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1. Every case inherits it here;
  # case_release_repo_unparsed_push_url_no_seam unsets it to test the rule.
  RELEASE_ALLOW_UNPARSED_PUSH_URL=1
  export RSAC_PROXY_REPO FAKE_EXPECT_REPO RELEASE_REPO RELEASE_ALLOW_UNPARSED_PUSH_URL
  reset_expectations
}

# ── arranging a case ────────────────────────────────────────────────────────
commit_local() { # <message>: commit everything (git add -A) locally only
  must git -C "$REPO" add -A || return 1
  must git -C "$REPO" commit -q -m "$1" || return 1
  SHA="$(git -C "$REPO" rev-parse HEAD)"
}
push_master() { must git -C "$REPO" push -q origin master; }
touch_commit() { # <text>: change a tracked file and commit it locally
  printf '%s\n' "$1" >> "$REPO/README.md"
  commit_local "$1"
}
commit_if_changed() { # <message>: commit and push only when the tracked tree differs (a rewrite may be a no-op)
  if git -C "$REPO" diff --quiet && git -C "$REPO" diff --cached --quiet; then return 0; fi
  commit_local "$1" && push_master
}
set_changelog() { # <content>: replace CHANGELOG.md, commit and push it
  printf '%s' "$1" > "$REPO/CHANGELOG.md"
  commit_if_changed "changelog"
}
remove_changelog() {
  must git -C "$REPO" rm -q CHANGELOG.md || return 1
  commit_local "remove changelog" && push_master
}
set_gomod() { # <module path>: replace bindings/rsac-go/go.mod, commit and push it
  printf 'module %s\n\ngo 1.21\n' "$1" > "$REPO/bindings/rsac-go/go.mod"
  commit_if_changed "go.mod"
}
set_workflow_file() { # <workflow file name> <full content>: replace .github/workflows/<file>, commit and push it
  printf '%s\n' "$2" > "$REPO/.github/workflows/$1"
  commit_if_changed "$1"
}
remove_workflow_file() { # <workflow file name>
  must git -C "$REPO" rm -q ".github/workflows/$1" || return 1
  commit_local "remove $1" && push_master
}
workflow_file_with() { # <`name:` line>: a workflow file whose top-level name line is the argument
  printf '%s\n\non:\n  workflow_dispatch:\n\njobs:\n  lint:\n    name: Lint\n    runs-on: ubuntu-latest' "$1"
}
set_note_raw() { # <exact content of the note>: replace the R4.5 note, commit and push it
  printf '%s' "$1" > "$REPO/$NOTE_PATH"
  commit_if_changed "note"
}
set_note_body() { # <body lines>: the note is a heading, a blank line and the body
  set_note_raw "$(printf '# Windows default-device switch evidence\n\n%s\n' "$1")"
}
remove_note() {
  must git -C "$REPO" rm -q "$NOTE_PATH" || return 1
  commit_local "remove note" && push_master
}
change_path() { # <path>: append to (or create) a file, commit and push
  mkdir -p "$(dirname "$REPO/$1")" || return 1
  printf 'changed after the tested commit\n' >> "$REPO/$1"
  commit_local "change $1" && push_master
}
delete_path() { # <path>: delete a tracked file, commit and push
  must git -C "$REPO" rm -q "$1" || return 1
  commit_local "delete $1" && push_master
}
make_tag() { # <local|remote> <tag>: a lightweight tag at HEAD, on one side only
  must git -C "$REPO" tag "$2" || return 1
  if [ "$1" = remote ]; then
    must git -C "$REPO" push -q origin "refs/tags/$2" || return 1
    must git -C "$REPO" tag -d "$2" || return 1
  fi
}
make_head_behind_tip() { # <branch>: remote <branch> is one commit ahead of HEAD (HEAD stays on an ancestor)
  touch_commit "newer"
  must git -C "$REPO" push -q origin "HEAD:refs/heads/$1" || return 1
  must git -C "$REPO" reset -q --hard HEAD~1 || return 1
  SHA="$(git -C "$REPO" rev-parse HEAD)"
}
add_upstream() { # a second bare remote named `upstream` that carries master
  must git init -q --bare "$UPSTREAM" || return 1
  must git -C "$REPO" remote add upstream "$UPSTREAM" || return 1
  must git -C "$REPO" push -q upstream master:master
}
use_remote_url() { # <remote> <url> <bare repository serving it>: GitHub-form URL, transport still local
  must git -C "$REPO" remote set-url "$1" "$2" || return 1
  must git -C "$REPO" config "remote.$1.vcs" rsacproxy || return 1
  RSAC_PROXY_REPO="$3"
  export RSAC_PROXY_REPO
}
set_rc() { printf '%s\n' "$2" > "$FAKE_DIR/$1.rc"; } # <lockstep|cargo|gh-list-<key>|gh-view-<id>|gh-issue-<N>> <code>
install_hook() { # <name> <script-body>: hook in the bare origin
  mkdir -p "$REMOTE/hooks"
  printf '#!/bin/sh\n%s\n' "$2" > "$REMOTE/hooks/$1"
  chmod +x "$REMOTE/hooks/$1"
}

# ── the world of the fake gh (header comment) ───────────────────────────────
arrange_list() { printf '%s' "$2" > "$FAKE_DIR/gh-list-$1.out"; } # <key> <stdout of the run listing>
arrange_run() { # <id> <line>: output of `gh run view <id>`; an empty line argument means no output at all
  if [ -n "$2" ]; then printf '%s\n' "$2" > "$FAKE_DIR/gh-view-$1.out"; else : > "$FAKE_DIR/gh-view-$1.out"; fi
}
issue_set() { printf '%s\n' "$2" > "$FAKE_DIR/gh-issue-$1.out"; } # <N> <stdout line of `gh issue view <N>`>
honour_waiver() { # <key> <N>: issue N is an open release-waiver issue; the case expects the waiver to be honoured
  issue_set "$2" "OPEN true"
  ev_set "$1" ""
  wv_set "$1" "$2"
}

# The `gh run view` line of a key's single listed run, per spec. Six fields (R3.2): <headSha> <status>
# <conclusion> <event> <readiness> <workflow name, the rest of the line>. Fails for an unknown spec.
spec_line() { # <key> <spec>
  local wf
  wf="$(key_wfname "$1")"
  case "$2" in
    green | list-fails | view-fails) echo "$SHA completed success workflow_dispatch success $wf" ;;
    wrong-commit) echo "$OTHER_SHA completed success workflow_dispatch success $wf" ;;
    abbreviated-sha) echo "${SHA:0:7} completed success workflow_dispatch success $wf" ;;
    queued) echo "$SHA queued none workflow_dispatch none $wf" ;;
    in-progress) echo "$SHA in_progress none workflow_dispatch none $wf" ;;
    failed) echo "$SHA completed failure workflow_dispatch success $wf" ;;
    cancelled) echo "$SHA completed cancelled workflow_dispatch success $wf" ;;
    conclusion-none) echo "$SHA completed none workflow_dispatch success $wf" ;;
    event-push) echo "$SHA completed success push success $wf" ;;
    event-pr) echo "$SHA completed success pull_request success $wf" ;;
    job-missing) echo "$SHA completed success workflow_dispatch missing $wf" ;;
    job-failed) echo "$SHA completed success workflow_dispatch failure $wf" ;;
    job-skipped) echo "$SHA completed success workflow_dispatch skipped $wf" ;;
    job-none) echo "$SHA completed success workflow_dispatch none $wf" ;;
    other-workflow) echo "$SHA completed success workflow_dispatch success Some Other Workflow" ;;
    malformed-4) echo "$SHA completed success workflow_dispatch" ;;
    malformed-5) echo "$SHA completed success workflow_dispatch success" ;;
    malformed-empty) echo "" ;;
    *) return 1 ;;
  esac
}
# The reason phrase of R4.3 (r2, R3.2) for a spec, with the job name substituted; empty where the contract
# names none (a failing GitHub CLI call, a malformed line: "missing for that key").
spec_phrase() { # <key> <spec>
  case "$2" in
    none) echo "no run found" ;;
    wrong-commit | abbreviated-sha) echo "wrong commit" ;;
    queued | in-progress) echo "still running" ;;
    failed | cancelled | conclusion-none) echo "failed" ;;
    event-push | event-pr) echo "not dispatched" ;;
    other-workflow) echo "not the $(key_file "$1") workflow" ;;
    job-missing) echo "no $(key_job "$1") job" ;;
    job-failed | job-skipped | job-none) echo "$(key_job "$1") failed" ;;
    *) echo "" ;;
  esac
}
world_key() { # <key> <spec>
  local k="$1" s="$2" id line
  id="$(key_id "$k")"
  ev_set "$k" ""
  if [ "$s" = none ]; then
    arrange_list "$k" ""
    return 0
  fi
  if ! line="$(spec_line "$k" "$s")"; then
    fail "harness: unknown world spec '$s' for $k"
    return 1
  fi
  arrange_list "$k" "$id"$'\n'
  arrange_run "$id" "$line"
  case "$s" in
    green) ev_set "$k" "$id" ;;
    list-fails) set_rc "gh-list-$k" 1 ;;
    view-fails) set_rc "gh-view-$id" 1 ;;
  esac
}
world() { # <ci=spec> <audio=spec> <android-emu=spec> <ios-sim=spec>: the four keys, in the order of R4.3
  local kv names=""
  for kv in "$@"; do
    names="$names ${kv%%=*}"
    world_key "${kv%%=*}" "${kv#*=}"
  done
  [ "$names" = " ci audio android-emu ios-sim" ] || fail "harness: world must name ci audio android-emu ios-sim in this order, got:$names"
}
world_with() { # <key=spec>...: these keys as given, every other key green
  local k s kv
  local -a args=()
  for k in $KEYS; do
    s=green
    for kv in "$@"; do
      if [ "${kv%%=*}" = "$k" ]; then s="${kv#*=}"; fi
    done
    args+=("$k=$s")
  done
  world "${args[@]}"
}
# World table of arrange_green (the world most cases use; every requirement is satisfied by one run):
#   key          list   gh run view line
#   ci           4242   <HEAD sha> completed success workflow_dispatch success CI
#   audio        4343   <HEAD sha> completed success workflow_dispatch success CI Audio Tests
#   android-emu  4444   <HEAD sha> completed success workflow_dispatch success CI Android Emulator
#   ios-sim      4545   <HEAD sha> completed success workflow_dispatch success CI iOS Simulator
#   issues: none
arrange_green() { world ci=green audio=green android-emu=green ios-sim=green; }

# R6.1: a RELEASE_GATE_DIR holds the gate's own scripts/check-version-lockstep.sh and .github/workflows/.
# <key>=<name> names that key's workflow name in the gate directory; NONE = the gate's file has no
# `name:` line; MISSING = the gate directory has no such file at all. Without arguments the gate
# names the same workflows as the commit (the template's own names).
use_gate_dir() { # [<key>=<name | NONE | MISSING>]...
  local k name kv
  GATE_DIR="$SB/gate"
  mkdir -p "$GATE_DIR/scripts" "$GATE_DIR/.github/workflows" || { fail "harness: mkdir $GATE_DIR"; return 1; }
  cp "$WORK/bin/gate-lockstep-stub.sh" "$GATE_DIR/scripts/check-version-lockstep.sh" || return 1
  for k in $KEYS; do
    name="$(key_wfname "$k")"
    for kv in "$@"; do
      if [ "${kv%%=*}" = "$k" ]; then name="${kv#*=}"; fi
    done
    [ "$name" = MISSING ] && continue
    if [ "$name" = NONE ]; then
      printf 'on:\n  workflow_dispatch:\n' > "$GATE_DIR/.github/workflows/$(key_file "$k")" || return 1
    else
      printf 'name: %s\n\non:\n  workflow_dispatch:\n' "$name" > "$GATE_DIR/.github/workflows/$(key_file "$k")" || return 1
    fi
  done
  RELEASE_GATE_DIR="$GATE_DIR"
  export RELEASE_GATE_DIR
}

# ── running the script under test ───────────────────────────────────────────
snapshot_tags() { # <before|after>: "<refname> <object>" per tag, local and per bare remote
  git -C "$REPO" for-each-ref --format='%(refname) %(objectname)' refs/tags | sort > "$SB/tags.$1.local"
  : > "$SB/tags.$1.remote"
  local r
  for r in "$REMOTE" "$UPSTREAM"; do
    [ -d "$r" ] || continue
    git -C "$r" for-each-ref --format='%(refname) %(objectname)' refs/tags \
      | sed "s|^|$(basename "$r") |" | sort >> "$SB/tags.$1.remote"
  done
}

run_tag() { # <args...>: run scripts/tag-release.sh from inside the throwaway repository
  snapshot_tags before
  ( cd "$REPO" && bash "$SCRIPT" "$@" ) > "$SB/stdout" 2> "$SB/stderr" < /dev/null
  RC=$?
  snapshot_tags after
}

# ── assertions ──────────────────────────────────────────────────────────────
assert_rc() { # <expected> <clause>
  [ "$RC" = "$1" ] || fail "exit $RC, expected $1 [$2]"
}
assert_last_stdout() { # <expected> <clause>
  local last
  last="$(tail -n 1 "$SB/stdout")"
  [ "$last" = "$1" ] || fail "last stdout line is '$last', expected '$1' [$2]"
}
assert_stderr_has() { # <fixed string> <clause>
  grep -qF -- "$1" "$SB/stderr" || fail "stderr lacks '$1' [$2]"
}
assert_stderr_lacks() { # <fixed string> <clause>
  if grep -qF -- "$1" "$SB/stderr"; then fail "stderr contains '$1' [$2]"; fi
}
assert_stderr_has_ci() { # <fixed string, case-insensitive; quote characters in the message are ignored> <clause>
  # The contract writes the reason "no `Release Readiness` job" with markdown backticks; the wording is
  # literal, the quoting around the job name is not part of the wording.
  tr -d "\`'\"" < "$SB/stderr" | grep -qiF -- "$1" || fail "stderr lacks '$1' (any case, quotes ignored) [$2]"
}
assert_output_has() { # <fixed string> <clause>: stdout or stderr
  cat "$SB/stdout" "$SB/stderr" | grep -qF -- "$1" || fail "output lacks '$1' [$2]"
}
assert_stderr_prefixed() { # <clause>
  grep -q '^tag-release: ' "$SB/stderr" || fail "no stderr line starts with 'tag-release: ' [$1]"
}
assert_stdout_unprefixed() { # <clause>
  if grep -q '^tag-release: ' "$SB/stdout"; then fail "a 'tag-release: ' message went to stdout [$1]"; fi
}
assert_no_success_line() { # <clause>
  local last
  last="$(tail -n 1 "$SB/stdout")"
  case "$last" in
    "tagged "* | "dry-run ok "*) fail "failing run printed a success line '$last' [$1]" ;;
  esac
}
assert_eq() { # <actual> <expected> <what>
  [ "$1" = "$2" ] || fail "$3: got '$1', expected '$2'"
}
assert_not_called() { # <clause> <lockstep|cargo|gh>...: a failing check stops the run
  local clause="$1" t
  shift
  for t in "$@"; do
    if [ -s "$FAKE_DIR/$t.log" ]; then
      fail "$t was called after the failing check: $(tr '\n' '|' < "$FAKE_DIR/$t.log") [$clause]"
    fi
  done
}
log_text() { # <tool>: log content with lines joined by '|' (empty when never called)
  if [ -f "$FAKE_DIR/$1.log" ]; then tr '\n' '|' < "$FAKE_DIR/$1.log"; fi
}
gh_count() { # <fixed string>: number of gh calls (log lines) containing it
  if [ -f "$FAKE_DIR/gh.log" ]; then grep -cF -- "$1" "$FAKE_DIR/gh.log"; else echo 0; fi
}
gh_logged() { # <exact log line>: the gh call was made, exactly like this
  [ -f "$FAKE_DIR/gh.log" ] && grep -qxF -- "$1" "$FAKE_DIR/gh.log"
}
stderr_lf() { tr -d '\r' < "$SB/stderr"; } # stderr without carriage returns

assert_nothing_created() { # <clause>: a failure creates or pushes nothing
  cmp -s "$SB/tags.before.remote" "$SB/tags.after.remote" \
    || fail "a tag was pushed to or removed from a remote [$1]"
  # R4.2 (changed from the earlier version, which allowed local tags that a `git fetch --tags` mirrored from
  # the remote): "No code path ... creates or updates a local tag other than the two the script tags in
  # step 8", so after a failing run the local tags are exactly what they were, names and objects.
  cmp -s "$SB/tags.before.local" "$SB/tags.after.local" \
    || fail "the local tags changed during a failing run: before [$(tr '\n' '|' < "$SB/tags.before.local")] after [$(tr '\n' '|' < "$SB/tags.after.local")] [$1]"
}
assert_no_tag_lost() { # <clause>
  local lost
  lost="$(comm -23 "$SB/tags.before.local" "$SB/tags.after.local" | tr '\n' '|')"
  [ -z "$lost" ] || fail "pre-existing local tag(s) removed: $lost [$1]"
}
assert_tag_unchanged() { # <tag> <clause>: no push, no overwrite, no new object on either side
  assert_eq "$(grep -F "refs/tags/$1 " "$SB/tags.after.remote")" "$(grep -F "refs/tags/$1 " "$SB/tags.before.remote")" \
    "remote tag $1 changed [$2]"
  # R4.2: the fetch imports no tag, so locally the tag is exactly what it was.
  assert_eq "$(grep -F "refs/tags/$1 " "$SB/tags.after.local")" "$(grep -F "refs/tags/$1 " "$SB/tags.before.local")" \
    "local tag $1 changed [$2]"
}
assert_local_tags_added() { # <clause> <tag>...: the local tags afterwards are exactly the ones before plus these (R4.2)
  local clause="$1" t
  shift
  { sed 's/ .*//' "$SB/tags.before.local"; for t in "$@"; do echo "refs/tags/$t"; done; } | sort > "$SB/tags.want.names"
  sed 's/ .*//' "$SB/tags.after.local" | sort > "$SB/tags.got.names"
  cmp -s "$SB/tags.want.names" "$SB/tags.got.names" \
    || fail "local tags afterwards [$(tr '\n' '|' < "$SB/tags.got.names")], expected [$(tr '\n' '|' < "$SB/tags.want.names")] [$clause]"
}

tag_dir() { # <local|remote|upstream>
  case "$1" in
    local) echo "$REPO" ;;
    remote) echo "$REMOTE" ;;
    upstream) echo "$UPSTREAM" ;;
  esac
}
assert_tag_present() { # <local|remote|upstream> <tag> <subject> <clause>: annotated, on full HEAD sha
  local dir type peeled subject
  dir="$(tag_dir "$1")"
  type="$(git -C "$dir" cat-file -t "refs/tags/$2" 2>/dev/null)"
  peeled="$(git -C "$dir" rev-parse "refs/tags/$2^{commit}" 2>/dev/null)"
  subject="$(git -C "$dir" for-each-ref --format='%(contents:subject)' "refs/tags/$2" 2>/dev/null)"
  assert_eq "$type" tag "$1 tag $2 object type (annotated) [$4]"
  assert_eq "$peeled" "$SHA" "$1 tag $2 target [$4]"
  assert_eq "$subject" "$3" "$1 tag $2 message [$4]"
}
assert_tag_message() { # <local|remote|upstream> <tag> <expected full message> <clause>: read back from the annotated tag
  local dir got
  dir="$(tag_dir "$1")"
  got="$(git -C "$dir" for-each-ref --format='%(contents)' "refs/tags/$2" 2>/dev/null)"
  assert_eq "$got" "$3" "$1 tag $2 full message [$4]"
}
assert_tag_absent() { # <local|remote|upstream> <tag> <clause>
  local dir
  dir="$(tag_dir "$1")"
  if git -C "$dir" rev-parse -q --verify "refs/tags/$2" > /dev/null 2>&1; then
    fail "$1 has tag $2 [$3]"
  fi
}
remote_tag_names() { # names of every tag on the default remote, sorted, space-terminated
  git -C "$REMOTE" for-each-ref --format='%(refname:short)' refs/tags | sort | tr '\n' ' '
}

# R4.6: the message of a tag, from the case's expectations. First line, a blank line, `Evidence: <key>=<run id>`
# per satisfied requirement in table order, `Evidence: windows-switch=<Tested-SHA>` when R4.5 passed, then
# `Waived: <key>=#<N>` per honoured waiver in the order android-emu, ios-sim, windows-switch.
tag_message() { # <first line>
  local m="$1" k v
  m="$m"$'\n'
  for k in $KEYS; do
    v="$(ev_get "$k")"
    [ -z "$v" ] || m="$m"$'\n'"Evidence: $k=$v"
  done
  v="$(ev_get windows-switch)"
  [ -z "$v" ] || m="$m"$'\n'"Evidence: windows-switch=$v"
  for k in android-emu ios-sim windows-switch; do
    v="$(wv_get "$k")"
    [ -z "$v" ] || m="$m"$'\n'"Waived: $k=#$v"
  done
  printf '%s' "$m"
}

# R4.3 / R4.5 report: the stderr lines `tag-release: evidence <key> ...`, in order, one per requirement.
# A spec is <key>=ok:<run id | Tested-SHA for windows-switch>, <key>=waived:<N> or <key>=missing:<phrase>
# (the phrase is looked for in the reason, any case, quotes ignored; an empty phrase only asks for a reason).
assert_evidence_lines() { # <clause> <spec>...
  local clause="$1" spec key rest kind arg i=0 line want reason n
  shift
  stderr_lf | grep '^tag-release: evidence ' > "$SB/evidence.got"
  n="$(wc -l < "$SB/evidence.got" | tr -d ' ')"
  if [ "$n" != "$#" ]; then
    fail "$n 'tag-release: evidence' lines on stderr, expected $#: $(tr '\n' '|' < "$SB/evidence.got") [$clause]"
    return 0
  fi
  for spec in "$@"; do
    i=$((i + 1))
    key="${spec%%=*}"
    rest="${spec#*=}"
    kind="${rest%%:*}"
    arg="${rest#*:}"
    line="$(sed -n "${i}p" "$SB/evidence.got")"
    case "$kind" in
      ok)
        if [ "$key" = windows-switch ]; then
          want="tag-release: evidence windows-switch ok $arg"
        else
          want="tag-release: evidence $key ok run $arg"
        fi
        [ "$line" = "$want" ] || fail "evidence line $i is '$line', expected '$want' [$clause]"
        ;;
      waived)
        want="tag-release: evidence $key waived #$arg"
        [ "$line" = "$want" ] || fail "evidence line $i is '$line', expected '$want' [$clause]"
        ;;
      missing)
        case "$line" in
          "tag-release: evidence $key missing: "*)
            reason="${line#"tag-release: evidence $key missing: "}"
            [ -n "$reason" ] || fail "evidence line $i has no reason after 'missing: ' [$clause]"
            if [ -n "$arg" ]; then
              printf '%s' "$reason" | tr -d "\`'\"" | grep -qiF -- "$arg" \
                || fail "evidence line $i reason '$reason' lacks '$arg' [$clause]"
            fi
            ;;
          *) fail "evidence line $i is '$line', expected 'tag-release: evidence $key missing: <reason>' [$clause]" ;;
        esac
        ;;
      *) fail "harness: bad evidence spec '$spec'" ;;
    esac
  done
}
assert_evidence_state() { # <clause>: the report implied by the case's EV_*/WV_* expectations
  local clause="$1" k v w
  local -a specs=()
  for k in $KEYS windows-switch; do
    v="$(ev_get "$k")"
    w="$(wv_get "$k")"
    if [ -n "$v" ]; then
      specs+=("$k=ok:$v")
    elif [ -n "$w" ]; then
      specs+=("$k=waived:$w")
    else
      fail "harness: no expectation for $k"
    fi
  done
  assert_evidence_lines "$clause" "${specs[@]}"
}
assert_only_missing() { # <clause> <key> <phrase>: every other requirement is ok (default ids), <key> is missing with <phrase>
  local clause="$1" miss="$2" phrase="$3" k
  local -a specs=()
  for k in $KEYS windows-switch; do
    if [ "$k" = "$miss" ]; then
      specs+=("$k=missing:$phrase")
    else
      specs+=("$k=ok:$(ev_get "$k")")
    fi
  done
  assert_evidence_lines "$clause" "${specs[@]}"
}
assert_no_evidence_lines() { # <clause>: nothing was reported, the run failed before the evidence stage
  if stderr_lf | grep -q '^tag-release: evidence '; then fail "evidence lines were printed by a run that failed before check 7 [$1]"; fi
}
# R4.1: `tag-release: repository <REPO> (remote <name>)`, once, before the evidence lines.
assert_repository_line() { # <clause>
  local clause="$1" want n ln first
  want="tag-release: repository $EXP_REPO (remote $EXP_REMOTE)"
  stderr_lf > "$SB/stderr.lf"
  n="$(grep -cFx -- "$want" "$SB/stderr.lf")"
  ln="$(grep -nFx -- "$want" "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
  [ "$n" = 1 ] || fail "$n stderr lines equal '$want', expected exactly one [$clause]"
  first="$(grep -n '^tag-release: evidence ' "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
  if [ -n "$ln" ] && [ -n "$first" ] && [ "$ln" -ge "$first" ]; then
    fail "the repository line (stderr line $ln) does not come before the first evidence line ($first) [$clause]"
  fi
}
# R4.3: one hint line per missing key, `gh workflow run <file> --repo <REPO> --ref <branch> <inputs>`, in table
# order, after the four evidence lines. With no key, no line carries a dispatch command at all.
assert_hints() { # <clause> <key>...
  local clause="$1" k want ln prev=0 ios total
  shift
  stderr_lf > "$SB/stderr.lf"
  ios="$(grep -n '^tag-release: evidence ios-sim ' "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
  total="$(grep -c 'gh workflow run ' "$SB/stderr.lf")"
  [ "$total" = "$#" ] || fail "$total stderr lines carry a 'gh workflow run' hint, expected $# [$clause]"
  for k in "$@"; do
    want="gh workflow run $(key_file "$k") --repo $EXP_REPO --ref $EXP_BRANCH $(key_inputs "$k")"
    ln="$(grep -nF -- "$want" "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
    if [ -z "$ln" ]; then
      fail "no hint line with '$want' [$clause]"
      continue
    fi
    sed -n "${ln}p" "$SB/stderr.lf" | grep -q '^tag-release: ' || fail "the hint line for $k lacks the 'tag-release: ' prefix [$clause]"
    [ "$ln" -gt "$prev" ] || fail "the hint for $k is not after the hint before it (table order) [$clause]"
    if [ -n "$ios" ] && [ "$ln" -le "$ios" ]; then fail "the hint for $k precedes the last of the four evidence lines [$clause]"; fi
    prev="$ln"
  done
}
assert_stderr_after_evidence() { # <fixed string> <clause>: some stderr line after the four evidence lines holds it
  local ios ln
  stderr_lf > "$SB/stderr.lf"
  ios="$(grep -n '^tag-release: evidence ios-sim ' "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
  ln="$(awk -v s="${ios:-0}" -v pat="$1" 'NR > s && index($0, pat) { print NR; exit }' "$SB/stderr.lf")"
  [ -n "$ln" ] || fail "no stderr line after the evidence lines contains '$1' [$2]"
}
# R4.4: `tag-release: WAIVED <key> (#<N>)` per honoured waiver, immediately before tagging (and in --dry-run).
assert_waived_lines() { # <clause> <key=N>...: exactly these lines, each after the last evidence line
  local clause="$1" kv want ln last total
  shift
  stderr_lf > "$SB/stderr.lf"
  last="$(grep -n '^tag-release: evidence ' "$SB/stderr.lf" | tail -n 1 | cut -d: -f1)"
  total="$(grep -c 'WAIVED' "$SB/stderr.lf")"
  [ "$total" = "$#" ] || fail "$total stderr lines contain WAIVED, expected $# [$clause]"
  for kv in "$@"; do
    want="tag-release: WAIVED ${kv%%=*} (#${kv#*=})"
    ln="$(grep -nFx -- "$want" "$SB/stderr.lf" | head -n 1 | cut -d: -f1)"
    if [ -z "$ln" ]; then
      fail "no stderr line '$want' [$clause]"
    elif [ -n "$last" ] && [ "$ln" -le "$last" ]; then
      fail "'$want' is not after the last evidence line [$clause]"
    fi
  done
}
assert_no_waived_lines() { # <clause>
  if grep -q 'WAIVED' "$SB/stderr"; then fail "a WAIVED line was printed: $(grep 'WAIVED' "$SB/stderr" | tr '\n' '|') [$1]"; fi
}
# R4.6 / R6.4: the message `--dry-run` would write is on stderr: the first line and then the body lines,
# in order, each compared as a WHOLE line (an `Evidence: ci=4242` expectation must not accept
# `Evidence: ci=42420`).
assert_message_printed() { # <clause> <first line>
  local clause="$1" want ln last=0 k v
  local -a wants=("$2")
  for k in $KEYS; do
    v="$(ev_get "$k")"
    [ -z "$v" ] || wants+=("Evidence: $k=$v")
  done
  v="$(ev_get windows-switch)"
  [ -z "$v" ] || wants+=("Evidence: windows-switch=$v")
  for k in android-emu ios-sim windows-switch; do
    v="$(wv_get "$k")"
    [ -z "$v" ] || wants+=("Waived: $k=#$v")
  done
  stderr_lf > "$SB/stderr.lf"
  for want in "${wants[@]}"; do
    ln="$(grep -nFx -- "$want" "$SB/stderr.lf" | awk -F: -v l="$last" '$1 > l { print $1; exit }')"
    if [ -z "$ln" ]; then
      fail "stderr lacks the whole line '$want' after line $last (the message --dry-run would write, in order) [$clause]"
      return 0
    fi
    last="$ln"
  done
}
# R6.4: the two-tag dry run asserts the complete Go annotation block as well as the crate block. Both
# blocks carry the same body (R4.6), so each block's lines are checked whole-line and in order; which of
# the two messages comes first is not pinned (the contract does not order them).
assert_dry_run_messages() { # <clause>
  assert_message_printed "$1" "rsac $VER"
  assert_message_printed "$1" "rsac-go $VER"
}
# R6.4: --no-go-tag prints no Go annotation block: no whole line 'rsac-go X.Y.Z', and each body line of
# the crate message exactly once (a second block would double it).
assert_no_go_block_printed() { # <clause>
  local clause="$1" k v n
  stderr_lf > "$SB/stderr.lf"
  if grep -qFx -- "rsac-go $VER" "$SB/stderr.lf"; then
    fail "a whole line 'rsac-go $VER' was printed although --no-go-tag [$clause]"
  fi
  for k in $KEYS windows-switch; do
    v="$(ev_get "$k")"
    [ -z "$v" ] && continue
    n="$(grep -cFx -- "Evidence: $k=$v" "$SB/stderr.lf")"
    [ "$n" = 1 ] || fail "$n whole lines 'Evidence: $k=$v': the crate block prints it once, a Go block would double it [$clause]"
  done
  for k in android-emu ios-sim windows-switch; do
    v="$(wv_get "$k")"
    [ -z "$v" ] && continue
    n="$(grep -cFx -- "Waived: $k=#$v" "$SB/stderr.lf")"
    [ "$n" = 1 ] || fail "$n whole lines 'Waived: $k=#$v' [$clause]"
  done
}

gh_list_form() { # <key>: the listing call as logged (shell quoting removed), R3.1 --repo right after 'run list'
  echo "run list --repo $EXP_REPO --workflow $(key_file "$1") --branch $EXP_BRANCH --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId"
}
gh_view_prefix() { # <id>: the start of the inspection call as logged (R3.1, R3.2)
  echo "run view $1 --repo $EXP_REPO --json headSha,status,conclusion,event,jobs,workflowName --jq "
}
gh_issue_form() { # <N>: the waiver verification call of R4.4 as logged
  echo "issue view $1 --repo $EXP_REPO --json state,labels --jq [.state, ([.labels[].name] | index(\"release-waiver\") != null)] | join(\" \")"
}

realdir() { ( cd "$1" 2>/dev/null && pwd ) || echo missing; } # <path>: one canonical spelling
# R6.1a: in RELEASE_GATE_DIR mode the gate's lockstep script is invoked as
#   bash "$RELEASE_GATE_DIR/scripts/check-version-lockstep.sh" --root <absolute working directory> --expect X.Y.Z
# (arguments in exactly that order). The gate stub observes its arguments and its working directory; both
# must be the commit's checkout (R6.1: the gate's script still reads the commit's manifests).
assert_gate_lockstep_call() { # <clause>
  local clause="$1" line stub_pwd args root
  if [ ! -s "$FAKE_DIR/gate-lockstep.log" ]; then
    fail "the gate's lockstep stub was never called [$clause]"
    return 0
  fi
  line="$(head -n 1 "$FAKE_DIR/gate-lockstep.log")"
  stub_pwd="${line%%$'\t'*}"
  args="${line#*$'\t'}"
  set -- $args
  if [ "${1:-}" != --root ]; then
    fail "the gate's lockstep call is '$args', expected it to start with --root [R6.1a, $clause]"
    return 0
  fi
  root="$2"
  if [ "${3:-}" != --expect ]; then
    fail "the gate's lockstep call is '$args', expected --expect right after --root <dir> [R6.1a, $clause]"
    return 0
  fi
  assert_eq "$4" "$VER" "the gate's lockstep --expect value [R6.1a, $clause]"
  [ "$#" = 4 ] || fail "the gate's lockstep call is '$args', expected exactly '--root <dir> --expect X.Y.Z' [R6.1a, $clause]"
  assert_eq "$(realdir "$stub_pwd")" "$(realdir "$REPO")" "the working directory of the gate's script [R6.1: the commit's checkout]"
  assert_eq "$(realdir "${root:-}")" "$(realdir "$REPO")" "the --root value [R6.1a: the commit's checkout, absolute]"
}
assert_gh_calls_name_repo() { # <repo> <clause>: every logged gh call has `--repo <repo>` (R3.1)
  local total=0 named=0
  if [ -f "$FAKE_DIR/gh.log" ]; then
    total="$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')"
    named="$(grep -cF -- " --repo $1 " "$FAKE_DIR/gh.log")"
  fi
  [ "$total" -ge 1 ] || fail "gh was never called [$2]"
  assert_eq "$named" "$total" "gh calls naming --repo $1 out of all gh calls [$2]"
}
assert_lists_and_views() { # <clause> <missing key>...: all four requirements were evaluated (R4.3: one failure stops nothing)
  local clause="$1" k m skip
  shift
  for k in $KEYS; do
    gh_logged "$(gh_list_form "$k")" || fail "no listing call for $k: $(gh_list_form "$k") [$clause]"
    skip=0
    for m in "$@"; do
      if [ "$m" = "$k" ]; then skip=1; fi
    done
    # a requirement that lists no run (or whose listing fails) has no run to inspect
    if [ "$skip" = 0 ] && [ "$(gh_count "run view $(key_id "$k") ")" = 0 ]; then
      fail "the run of $k ($(key_id "$k")) was never inspected [$clause]"
    fi
  done
}

expect_failure() { # <exit code> <clause>: the shape every failing check shares
  assert_rc "$1" "$2"
  assert_stderr_prefixed "$2"
  assert_no_success_line "$2"
  assert_nothing_created "$2"
}
expect_both_tagged() { # <clause>
  assert_rc 0 "$1"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "$1"
  assert_tag_present local "$CRATE_TAG" "rsac $VER" "$1"
  assert_tag_present local "$GO_TAG" "rsac-go $VER" "$1"
  assert_tag_present remote "$CRATE_TAG" "rsac $VER" "$1"
  assert_tag_present remote "$GO_TAG" "rsac-go $VER" "$1"
}
expect_crate_tagged_only() { # <clause>
  assert_rc 0 "$1"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "$1"
  assert_tag_present local "$CRATE_TAG" "rsac $VER" "$1"
  assert_tag_present remote "$CRATE_TAG" "rsac $VER" "$1"
}
# The r4 view of a success run: the full tag messages read back from the tags (R4.6), the report (R4.3,
# R4.5), the repository line (R4.1) and no local tag but the two the script created (R4.2).
expect_both_tagged_full() { # <clause>
  expect_both_tagged "$1"
  assert_tag_message local "$CRATE_TAG" "$(tag_message "rsac $VER")" "$1"
  assert_tag_message remote "$CRATE_TAG" "$(tag_message "rsac $VER")" "$1"
  assert_tag_message local "$GO_TAG" "$(tag_message "rsac-go $VER")" "$1"
  assert_tag_message remote "$GO_TAG" "$(tag_message "rsac-go $VER")" "$1"
  assert_evidence_state "$1"
  assert_repository_line "$1"
  assert_local_tags_added "$1" "$CRATE_TAG" "$GO_TAG"
}
expect_crate_tagged_only_full() { # <clause>
  expect_crate_tagged_only "$1"
  assert_tag_message local "$CRATE_TAG" "$(tag_message "rsac $VER")" "$1"
  assert_tag_message remote "$CRATE_TAG" "$(tag_message "rsac $VER")" "$1"
  assert_evidence_state "$1"
  assert_repository_line "$1"
  assert_local_tags_added "$1" "$CRATE_TAG"
}

post_checks() { # run after every case: no seam bypassed, gh stayed inside the contract forms
  if [ -s "$FAKE_DIR/poison.log" ]; then
    fail "the script ran a real tool instead of the seam: $(tr '\n' '|' < "$FAKE_DIR/poison.log") [§4 GH_BIN/CARGO_BIN seams, §7 no real gh]"
  fi
  if [ -s "$FAKE_DIR/gh.unexpected" ]; then
    fail "gh invoked outside the forms of §4 check 7 / R3.1 / R4.3 / R4.4: $(tr '\n' '|' < "$FAKE_DIR/gh.unexpected")"
  fi
}

select_case() { # <name>: honours RSAC_TEST_FILTER (R3.8) and counts the selected cases
  if [ -n "${RSAC_TEST_FILTER:-}" ]; then
    case "$1" in *"$RSAC_TEST_FILTER"*) ;; *) return 1 ;; esac
  fi
  SELECTED=$((SELECTED + 1))
  return 0
}
report_case() { # <name>: print the result line of the case that just ran
  if [ -z "$FAILS" ]; then
    echo "ok $1"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL $1:$FAILS"
    FAILED=$((FAILED + 1))
  fi
  use_version "$DEFAULT_VER"
}

run_case() { # <name> <function> [args...]
  local name="$1"
  shift
  select_case "$name" || return 0
  CASE_NAME="$name"
  FAILS=""
  CASE_ABORT=0
  if [ ! -f "$SCRIPT" ]; then
    echo "FAIL $name: scripts/$SCRIPT_NAME does not exist (the script under test is missing)"
    FAILED=$((FAILED + 1))
    return 0
  fi
  new_sandbox
  "$@"
  post_checks
  report_case "$name"
}
run_harness_case() { # <name> <function>: tests of the harness itself; needs no script under test
  local name="$1"
  shift
  select_case "$name" || return 0
  CASE_NAME="$name"
  FAILS=""
  CASE_ABORT=0
  "$@"
  report_case "$name"
}

# ═══════════════════════════════════════════════════════════════════════════
# Cases
# ═══════════════════════════════════════════════════════════════════════════

# ── §4 check 0, R4.3, R4.4: arguments → exit 2 ─────────────────────────────
case_args_bad() { # <clause> <args...>
  local clause="$1"
  shift
  arrange_green
  run_tag "$@"
  expect_failure 2 "$clause"
  # Argument errors precede every other check, so no tool has been used yet.
  assert_not_called "$clause" lockstep cargo gh
  assert_no_evidence_lines "$clause"
}
case_args_error_precedes_dirty_tree() { # <args...>
  printf 'edit\n' >> "$REPO/README.md" # a dirty tree would be exit 10 (check 1)
  arrange_green
  run_tag "$@"
  # §4 check order: check 0 (arguments, exit 2) runs before check 1 (tree, exit 10)
  expect_failure 2 "§4 table: check 0 before check 1"
  assert_not_called "§4 check order" lockstep cargo gh
}

# ── §4 check 1: tracked tree clean → exit 10 ───────────────────────────────
case_dirty_unstaged() {
  printf 'edit\n' >> "$REPO/README.md"
  arrange_green
  run_tag "$VER"
  expect_failure 10 "§4 check 1: git diff --quiet fails on an unstaged tracked change"
  assert_not_called "§4 check order: nothing after check 1 runs" lockstep cargo gh
}
case_dirty_staged() {
  printf 'edit\n' >> "$REPO/README.md"
  must git -C "$REPO" add README.md
  arrange_green
  run_tag "$VER"
  expect_failure 10 "§4 check 1: git diff --cached --quiet fails on a staged change"
  assert_not_called "§4 check order: nothing after check 1 runs" lockstep cargo gh
}
case_untracked_file_allowed() {
  printf 'stray\n' > "$REPO/stray.txt"
  arrange_green
  run_tag "$VER"
  # §4 check 1: "Untracked files are allowed"
  expect_both_tagged "§4 check 1: untracked files are allowed"
}
case_dirty_precedes_fetch() {
  printf 'edit\n' >> "$REPO/README.md"
  arrange_green
  run_tag "$VER" --remote no-such-remote # an unknown remote would be exit 11 (check 2)
  expect_failure 10 "§4 table: check 1 before check 2"
}

# ── §4 check 2: fetch and ancestry → exit 11 ───────────────────────────────
case_fetch_fails_unknown_remote() {
  arrange_green
  run_tag "$VER" --remote no-such-remote
  expect_failure 11 "§4 check 2: the fetch of <remote> must succeed"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_fetch_fails_unknown_branch() {
  arrange_green
  run_tag "$VER" --branch no-such-branch
  expect_failure 11 "§4 check 2: fetching a branch the remote lacks fails"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_head_ahead_of_remote() {
  touch_commit "unpushed"
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2: HEAD ahead of <remote>/<branch> is not an ancestor of it"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_head_diverged_from_remote() {
  touch_commit "on remote"
  push_master
  must git -C "$REPO" reset -q --hard HEAD~1
  touch_commit "diverged, local only"
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2: HEAD on a commit the remote branch does not contain"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_head_behind_remote_allowed() {
  touch_commit "newer"
  push_master
  must git -C "$REPO" reset -q --hard HEAD~1
  SHA="$(git -C "$REPO" rev-parse HEAD)" # the older commit: an ancestor of origin/master
  arrange_green
  run_tag "$VER"
  # §4 check 2: HEAD may be an ancestor of <remote>/<branch>; §4 check 8: tags go on the full HEAD sha
  expect_both_tagged "§4 checks 2 and 8: an ancestor HEAD is tagged at HEAD, not at the remote tip"
}
case_stale_tracking_ref_refreshed() {
  touch_commit "pushed"
  push_master
  # Make the local remote-tracking ref stale; only the fetch of check 2 can repair it.
  must git -C "$REPO" update-ref refs/remotes/origin/master HEAD~1
  arrange_green
  run_tag "$VER"
  expect_both_tagged "§4 check 2: the script fetches before comparing against <remote>/<branch>"
  # R4.2: the fetch is `git fetch --no-tags <remote> "+refs/heads/<branch>:<tracking ref>"`: it writes the tracking ref.
  assert_eq "$(git -C "$REPO" rev-parse refs/remotes/origin/master)" "$(git -C "$REMOTE" rev-parse refs/heads/master)" \
    "tracking ref after the run [R4.2: the explicit refspec updates refs/remotes/<remote>/<branch>]"
}
case_remote_and_branch_flags() {
  must git init -q --bare "$UPSTREAM"
  must git -C "$REPO" remote add upstream "$UPSTREAM"
  must git -C "$REPO" push -q upstream master:release
  EXP_REMOTE=upstream
  EXP_BRANCH=release
  arrange_green
  run_tag "$VER" --remote upstream --branch release
  # §4 usage: --remote NAME and --branch NAME; the tags go to the chosen remote, the listing uses the chosen branch
  assert_rc 0 "§4 --remote/--branch"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "§4 --remote/--branch"
  assert_tag_present upstream "$CRATE_TAG" "rsac $VER" "§4 --remote/--branch: pushed to the chosen remote"
  assert_tag_present upstream "$GO_TAG" "rsac-go $VER" "§4 --remote/--branch: pushed to the chosen remote"
  assert_tag_absent remote "$CRATE_TAG" "§4 --remote: the default remote is not used"
  assert_tag_absent remote "$GO_TAG" "§4 --remote: the default remote is not used"
  # R4.3 (changed: four listings instead of one): every requirement is listed on the chosen branch; the first call is
  # the ci listing (§4 check 7, R3.1)
  assert_eq "$(head -n 1 "$FAKE_DIR/gh.log")" \
    "run list --repo $TEST_REPO --workflow ci.yml --branch release --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId" \
    "§4 check 7 listing form with --branch release (R3.1: --repo right after 'run list')"
  assert_eq "$(gh_count ' --branch release ')" 4 "listings on branch release [R4.3: four requirements]"
  # R4.1: the repository line names the chosen remote
  assert_repository_line "R4.1: 'repository <REPO> (remote upstream)'"
}
# Adversarial-review regressions for check 2 (§4: HEAD must be <remote>/<branch> or an ancestor of the
# branch as it stands on the remote). A name that merely LOOKS like the tracking ref must not decide it.
case_head_shadowed_by_local_branch() {
  touch_commit "unpushed" # HEAD is ahead of the remote branch
  # refs/heads/origin/master wins name resolution of `origin/master` over refs/remotes/origin/master
  must git -C "$REPO" branch origin/master HEAD
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2: a local branch named origin/master at an unpushed commit must not pass for the remote branch"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_head_shadowed_by_local_tag() {
  touch_commit "unpushed"
  must git -C "$REPO" tag origin/master HEAD # refs/tags/origin/master also precedes refs/remotes/ in name resolution
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2: a local tag named origin/master at an unpushed commit must not pass for the remote branch"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_no_fetch_refspec_remote_rewritten() {
  touch_commit "head"
  push_master # origin/master == HEAD, and the tracking ref says so
  local root other
  root="$(git -C "$REPO" rev-parse HEAD~1)"
  other="$(git -C "$REPO" commit-tree "$root^{tree}" -p "$root" -m "rewritten tip")"
  must git -C "$REPO" push -q origin "$other:refs/heads/other"
  must git -C "$REMOTE" update-ref refs/heads/master "$other" # the remote branch no longer contains HEAD
  # Without a fetch refspec `git fetch origin master` could not refresh refs/remotes/origin/master; R4.2's explicit
  # "+refs/heads/<branch>:<tracking ref>" refspec does, so a stale tracking ref cannot vouch for the branch.
  must git -C "$REPO" config --unset-all remote.origin.fetch
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2 / R4.2: a stale tracking ref must not vouch for a remote branch that no longer contains HEAD"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_no_fetch_refspec_remote_contains_head() {
  # Control for the case above. Changed by R4.2: the explicit refspec of the fetch makes the literal reading and the
  # intent of §4 check 2 coincide (the remote branch as just fetched contains HEAD, so the check passes although the
  # remote has no fetch refspec and the tracking ref was stale).
  touch_commit "pushed"
  push_master
  must git -C "$REPO" update-ref refs/remotes/origin/master HEAD~1
  must git -C "$REPO" config --unset-all remote.origin.fetch
  arrange_green
  run_tag "$VER"
  expect_both_tagged "§4 check 2 / R4.2: the remote branch contains HEAD although the tracking ref was stale"
}

# ── R4.2: the fetch brings in no tag ────────────────────────────────────────
case_fetch_imports_no_tags() {
  make_tag remote v0.0.9 # a tag only the remote has
  arrange_green
  run_tag "$VER"
  expect_both_tagged "R4.2: success despite a remote-only tag"
  assert_tag_absent local v0.0.9 "R4.2: git fetch --no-tags: the remote's tag is not imported"
  assert_local_tags_added "R4.2: local tags afterwards are the two the script created" "$CRATE_TAG" "$GO_TAG"
}
case_fetch_imports_no_tags_dry_run() {
  make_tag remote v0.0.9
  arrange_green
  run_tag "$VER" --dry-run
  assert_rc 0 "R4.2 / §4 --dry-run"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4 --dry-run"
  # R4.2: "No code path, --dry-run included, creates or updates a local tag other than the two the script tags in step 8"
  assert_tag_absent local v0.0.9 "R4.2: --dry-run imports no tag"
  assert_eq "$(wc -l < "$SB/tags.after.local" | tr -d ' ')" 0 "local tags after --dry-run [R4.2]"
}
case_fetch_imports_no_tags_on_failure() {
  make_tag remote v0.0.9
  world ci=none audio=green android-emu=green ios-sim=green # check 7 fails
  run_tag "$VER"
  expect_failure 16 "R4.3: no run for ci"
  assert_tag_absent local v0.0.9 "R4.2: a failing run imports no tag either"
}
case_fetch_keeps_diverged_local_tag() {
  # A local tag with the name of a remote tag that points elsewhere: `git fetch --tags` would refuse to update it (and fail);
  # R4.2's `--no-tags` fetch leaves both alone and the run succeeds.
  local behind
  behind="$(git -C "$REPO" rev-parse HEAD~1)"
  must git -C "$REPO" tag v0.0.9 HEAD
  must git -C "$REPO" push -q origin "$behind:refs/tags/v0.0.9"
  arrange_green
  run_tag "$VER"
  expect_both_tagged "R4.2: a diverged same-name tag does not fail check 2"
  assert_eq "$(git -C "$REPO" rev-parse refs/tags/v0.0.9)" "$SHA" "local tag v0.0.9 [R4.2: no code path updates a local tag]"
  assert_eq "$(git -C "$REMOTE" rev-parse refs/tags/v0.0.9)" "$behind" "remote tag v0.0.9 [R4.2: nothing is pushed but the two tags]"
}

# ── R3.1, R4.1: repository binding (RELEASE_REPO, or parsed from the remote URLs) ──
case_repo_unset_local_remote() { # <RELEASE_REPO value or "unset">
  if [ "$1" = unset ]; then unset RELEASE_REPO; else RELEASE_REPO=""; export RELEASE_REPO; fi
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R3.1: RELEASE_REPO $1 and a local-path remote URL: check 2 fails"
  assert_stderr_has "cannot determine the GitHub repository" "R3.1: the message of exit 11"
  assert_not_called "R3.1: check 2 fails before anything else runs" lockstep cargo gh
}
case_repo_unset_precedes_tags_check() {
  unset RELEASE_REPO
  make_tag local "$CRATE_TAG" # check 3 would be exit 12
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R3.1: the repository is determined in check 2, before check 3"
  assert_stderr_has "cannot determine the GitHub repository" "R3.1: the message of exit 11"
}
case_repo_from_url() { # <remote url> <expected owner/name>
  unset RELEASE_REPO # R3.1: otherwise REPO comes from `git remote get-url <remote>`
  FAKE_EXPECT_REPO="$2"
  EXP_REPO="$2"
  export FAKE_EXPECT_REPO
  use_remote_url origin "$1" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R3.1: $1 yields --repo $2 (R4.1: the push URL, the same URL, names the same repository)"
  assert_gh_calls_name_repo "$2" "R3.1: every GitHub CLI call passes --repo $2 (from $1)"
}
case_repo_url_rejected() { # <remote url>
  unset RELEASE_REPO
  FAKE_EXPECT_REPO="o/n"
  export FAKE_EXPECT_REPO
  use_remote_url origin "$1" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R3.1: '$1' is not one of the accepted GitHub URL forms"
  assert_stderr_has "cannot determine the GitHub repository" "R3.1: the message of exit 11"
  assert_not_called "R3.1: check 2 fails before anything else runs" lockstep cargo gh
}
case_repo_env_overrides_url() {
  # Changed by R6.2 (this combination passed under R3.1/R4.1): RELEASE_REPO is x/y and the push URL names
  # x/y, but the FETCH URL names o/n — with RELEASE_REPO set a fetch URL that parses as a GitHub URL must
  # name REPO too: exit 11.
  RELEASE_REPO="x/y"
  FAKE_EXPECT_REPO="x/y"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  must git -C "$REPO" config remote.origin.pushurl "https://github.com/x/y.git" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R6.2: fetch o/n, push x/y, RELEASE_REPO=x/y: the fetch URL names a different repository"
  assert_stderr_has "fetch URL names a different repository" "R6.2: the message of exit 11"
  assert_not_called "R6.2: check 2 fails before anything else runs" lockstep cargo gh
  assert_no_evidence_lines "R6.2"
}
case_repo_env_matching_fetch_url() {
  # R6.2: the same world with the fetch URL naming RELEASE_REPO passes; RELEASE_REPO still decides the
  # repository (R3.1), and the push URL (the same GitHub-form URL) names it (R4.1).
  RELEASE_REPO="x/y"
  FAKE_EXPECT_REPO="x/y"
  EXP_REPO="x/y"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/x/y.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R6.2: RELEASE_REPO=x/y and the fetch URL names x/y"
  assert_gh_calls_name_repo "x/y" "R3.1: --repo is RELEASE_REPO, not anything parsed from the URL"
}
case_fetch_url_unparsed_with_repo() { # <seam|no-seam>
  # R6.2: a fetch URL that does not parse (the default world: RELEASE_REPO set, local-path remote URL) is
  # accepted only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1 — the same seam as R5.3, which new_sandbox sets
  # for exactly this shape; here the variable is the subject.
  if [ "$1" = no-seam ]; then
    unset RELEASE_ALLOW_UNPARSED_PUSH_URL
  else
    RELEASE_ALLOW_UNPARSED_PUSH_URL=1
    export RELEASE_ALLOW_UNPARSED_PUSH_URL
  fi
  arrange_green
  run_tag "$VER"
  if [ "$1" = seam ]; then
    expect_both_tagged_full "R6.2: an unparsed fetch URL with RELEASE_REPO is accepted under the seam variable"
  else
    expect_failure 11 "R6.2: an unparsed fetch URL with RELEASE_REPO and no seam variable"
    assert_stderr_has "cannot determine the GitHub repository" "R6.2: the message of exit 11"
    assert_not_called "R6.2: check 2 fails before anything else runs" lockstep cargo gh
    assert_no_evidence_lines "R6.2"
  fi
}
case_repo_env_with_push_url_of_other_repo() {
  # R4.1 (replaces the old reading of this setup as a success): RELEASE_REPO is x/y and the push URL parses as a
  # GitHub URL naming o/n, so it "must name REPO": exit 11, 'push URL names a different repository'.
  RELEASE_REPO="x/y"
  FAKE_EXPECT_REPO="x/y"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R4.1: RELEASE_REPO set, push URL names another repository"
  assert_stderr_has "push URL names a different repository" "R4.1: the message of exit 11"
  assert_not_called "R4.1: check 2 fails before anything else runs" lockstep cargo gh
}
case_repo_empty_env_uses_url() {
  RELEASE_REPO="" # R3.1: "set and non-empty": an empty value is ignored
  FAKE_EXPECT_REPO="o/n"
  EXP_REPO="o/n"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R3.1: an empty RELEASE_REPO falls back to the remote URL"
  assert_gh_calls_name_repo "o/n" "R3.1: --repo comes from the URL"
}
case_repo_from_chosen_remote() {
  unset RELEASE_REPO
  FAKE_EXPECT_REPO="up/stream"
  EXP_REPO="up/stream"
  EXP_REMOTE=upstream
  export FAKE_EXPECT_REPO
  must git init -q --bare "$UPSTREAM"
  must git -C "$REPO" remote add upstream "$UPSTREAM"
  must git -C "$REPO" push -q upstream master:master
  use_remote_url upstream "git@github.com:up/stream.git" "$UPSTREAM" || return 0
  arrange_green
  run_tag "$VER" --remote upstream # origin keeps its local-path URL, which would not parse
  assert_rc 0 "R3.1: the URL of the remote chosen with --remote is the one parsed"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "R3.1"
  assert_tag_present upstream "$CRATE_TAG" "rsac $VER" "R3.1"
  assert_gh_calls_name_repo "up/stream" "R3.1: --repo is parsed from the chosen remote"
  assert_repository_line "R4.1: 'repository up/stream (remote upstream)'"
}

# R4.1: RELEASE_REPO, when set and non-empty, must match ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$; otherwise exit 11,
# message contains `invalid RELEASE_REPO`.
case_release_repo_invalid() { # <value>
  RELEASE_REPO="$1"
  export RELEASE_REPO
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R4.1: RELEASE_REPO '$1' does not match ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\$"
  assert_stderr_has "invalid RELEASE_REPO" "R4.1: the message of exit 11"
  assert_not_called "R4.1: check 2 fails before anything else runs" lockstep cargo gh
  assert_no_evidence_lines "R4.1"
}
case_release_repo_invalid_no_url_fallback() {
  # R4.1: an invalid RELEASE_REPO is not replaced by the repository parsed from a perfectly good remote URL.
  RELEASE_REPO="not a repository"
  FAKE_EXPECT_REPO="o/n"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R4.1: invalid RELEASE_REPO, parseable remote URL"
  assert_stderr_has "invalid RELEASE_REPO" "R4.1: the message of exit 11"
  assert_not_called "R4.1: check 2 fails before anything else runs" lockstep cargo gh
}
case_release_repo_invalid_precedes_tags_check() {
  RELEASE_REPO="not a repository"
  export RELEASE_REPO
  make_tag local "$CRATE_TAG" # check 3 would be exit 12
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R4.1 / R3.1: the repository is settled in check 2, before check 3"
  assert_stderr_has "invalid RELEASE_REPO" "R4.1"
}
case_release_repo_invalid_after_tree_check() {
  RELEASE_REPO="not a repository"
  export RELEASE_REPO
  printf 'edit\n' >> "$REPO/README.md"
  arrange_green
  run_tag "$VER"
  expect_failure 10 "§4 table: check 1 (tree) before check 2 (repository, exit 11)"
}
case_release_repo_valid() { # <value>
  RELEASE_REPO="$1"
  FAKE_EXPECT_REPO="$1"
  EXP_REPO="$1"
  export RELEASE_REPO FAKE_EXPECT_REPO
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R4.1: RELEASE_REPO '$1' matches the pattern"
  assert_gh_calls_name_repo "$1" "R4.1/R3.1: --repo is the RELEASE_REPO value"
}

# R4.1: `git remote get-url --push --all <remote>` must print exactly one line; with RELEASE_REPO unset the fetch URL
# and the push URL must each parse and name the same repository (case-insensitively); with RELEASE_REPO set a push
# URL that parses must name REPO (case-insensitively) and one that does not parse is accepted (the test seam).
# Placeholders in URLs: @SB@ = the case's sandbox directory, @REMOTE@ = the bare origin.
expand_url() { local u="$1"; u="${u//@SB@/$SB}"; u="${u//@REMOTE@/$REMOTE}"; printf '%s' "$u"; }
case_push_url() { # <RELEASE_REPO or unset> <expected --repo> <fetch url> <push url or -> <ok|exit code> <phrase> <clause>
  local rr="$1" repo="$2" url purl="$4" want="$5" phrase="$6" clause="$7"
  url="$(expand_url "$3")"
  purl="$(expand_url "$purl")"
  if [ "$rr" = unset ]; then unset RELEASE_REPO; else RELEASE_REPO="$rr"; export RELEASE_REPO; fi
  FAKE_EXPECT_REPO="$repo"
  EXP_REPO="$repo"
  export FAKE_EXPECT_REPO
  use_remote_url origin "$url" "$REMOTE" || return 0
  if [ "$purl" != - ]; then must git -C "$REPO" config remote.origin.pushurl "$purl" || return 0; fi
  arrange_green
  run_tag "$VER"
  if [ "$want" = ok ]; then
    expect_both_tagged_full "$clause"
    assert_gh_calls_name_repo "$repo" "$clause"
  else
    expect_failure "$want" "$clause"
    assert_stderr_has "$phrase" "$clause"
    assert_not_called "$clause (check 2 fails before anything else runs)" lockstep cargo gh
    assert_no_evidence_lines "$clause"
  fi
}
case_push_url_count() { # <RELEASE_REPO or unset> <two-urls|two-pushurls|one-pushurl-two-urls> <ok|exit code>
  local rr="$1" setup="$2" want="$3" u1 u2
  if [ "$rr" = unset ]; then
    unset RELEASE_REPO
    EXP_REPO="o/n"
  else
    RELEASE_REPO="$rr"
    export RELEASE_REPO
    EXP_REPO="$rr"
  fi
  FAKE_EXPECT_REPO="$EXP_REPO"
  export FAKE_EXPECT_REPO
  u1="https://github.com/$EXP_REPO.git"
  u2="git@github.com:$EXP_REPO"
  use_remote_url origin "$u1" "$REMOTE" || return 0
  case "$setup" in
    two-urls) must git -C "$REPO" config --add remote.origin.url "https://github.com/other/other.git" || return 0 ;;
    two-pushurls)
      must git -C "$REPO" config --add remote.origin.pushurl "$u1" || return 0
      must git -C "$REPO" config --add remote.origin.pushurl "$u2" || return 0
      ;;
    one-pushurl-two-urls)
      must git -C "$REPO" config --add remote.origin.url "https://github.com/other/other.git" || return 0
      must git -C "$REPO" config remote.origin.pushurl "$u2" || return 0
      ;;
  esac
  arrange_green
  run_tag "$VER"
  if [ "$want" = ok ]; then
    expect_both_tagged_full "R4.1: exactly one push URL ($setup)"
  else
    expect_failure "$want" "R4.1: $setup: more than one push URL"
    assert_stderr_has "exactly one push URL" "R4.1: the message of exit 11"
    assert_not_called "R4.1: check 2 fails before anything else runs" lockstep cargo gh
    assert_no_evidence_lines "R4.1"
  fi
}
case_push_insteadof() { # <RELEASE_REPO or unset> <pushInsteadOf replacement base> <ok|exit code> <phrase> <clause>
  # The fetch URL names the repository; `url.<base>.pushInsteadOf <fetch URL prefix>` rewrites ONLY the push URL, which
  # `git remote get-url --push` reports (and the plain `git remote get-url` does not).
  local rr="$1" base want="$3" phrase="$4" clause="$5" owner got
  if [ "$rr" = unset ]; then
    unset RELEASE_REPO
    EXP_REPO="o/n"
  else
    RELEASE_REPO="$rr"
    export RELEASE_REPO
    EXP_REPO="$rr"
  fi
  FAKE_EXPECT_REPO="$EXP_REPO"
  export FAKE_EXPECT_REPO
  owner="${EXP_REPO%%/*}"
  base="$(expand_url "$2")"
  base="${base//@OWNER@/$owner}"
  use_remote_url origin "https://github.com/$EXP_REPO.git" "$REMOTE" || return 0
  must git -C "$REPO" config "url.$base.pushInsteadOf" "https://github.com/$owner/" || return 0
  got="$(git -C "$REPO" remote get-url --push origin)"
  assert_eq "$got" "${base}${EXP_REPO#*/}.git" "harness: the push URL git reports after pushInsteadOf"
  assert_eq "$(git -C "$REPO" remote get-url origin)" "https://github.com/$EXP_REPO.git" "harness: the fetch URL is untouched by pushInsteadOf"
  arrange_green
  run_tag "$VER"
  if [ "$want" = ok ]; then
    expect_both_tagged_full "$clause"
  else
    expect_failure "$want" "$clause"
    assert_stderr_has "$phrase" "$clause"
    assert_not_called "$clause (check 2 fails before anything else runs)" lockstep cargo gh
  fi
}
case_pushurl_second_repository() {
  # R4.1 (issue #117: the gate must see where the push goes): RELEASE_REPO is set and the push URL, a plain
  # path, does not parse as a GitHub URL, which R5.3 accepts only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1
  # (set by new_sandbox for exactly this shape); the tags then go to the push URL, not to the fetch URL.
  must git init -q --bare "$UPSTREAM" || return 0
  must git -C "$REPO" config remote.origin.pushurl "$UPSTREAM" || return 0
  arrange_green
  run_tag "$VER"
  assert_rc 0 "R4.1: an unparseable push URL is accepted when RELEASE_REPO is set"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "R4.1"
  assert_tag_present upstream "$CRATE_TAG" "rsac $VER" "R3.3/R4.1: step 9 pushes to the push URL"
  assert_tag_present upstream "$GO_TAG" "rsac-go $VER" "R3.3/R4.1: step 9 pushes to the push URL"
  assert_tag_message upstream "$CRATE_TAG" "$(tag_message "rsac $VER")" "R4.6: message as pushed to the push URL"
  assert_tag_absent remote "$CRATE_TAG" "R4.1: nothing goes to the fetch URL"
  assert_tag_absent remote "$GO_TAG" "R4.1: nothing goes to the fetch URL"
}
case_release_repo_unparsed_push_url_no_seam() {
  # R5.3: with RELEASE_REPO set, a push URL that does not parse (here the local-path fetch URL of the
  # throwaway remote) is accepted only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1; without the seam: exit 11.
  unset RELEASE_ALLOW_UNPARSED_PUSH_URL
  arrange_green
  run_tag "$VER"
  expect_failure 11 "R5.3: RELEASE_REPO set and the push URL does not parse as a GitHub URL"
  assert_stderr_has "cannot determine the GitHub repository" "R5.3: the message of exit 11"
  assert_not_called "R5.3: check 2 fails before anything else runs" lockstep cargo gh
  assert_no_evidence_lines "R5.3"
}
case_release_repo_unparsed_push_url_seam() {
  # R5.3: the same world under RELEASE_ALLOW_UNPARSED_PUSH_URL=1 (the harness default, set in new_sandbox) passes.
  RELEASE_ALLOW_UNPARSED_PUSH_URL=1
  export RELEASE_ALLOW_UNPARSED_PUSH_URL
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R5.3: the seam accepts an unparsed push URL when RELEASE_REPO is set"
}

# ── R3.2, R4.3: workflow identity (six fields, workflow name last) ──────────
case_wf_name_file() { # <key> <full workflow file> <run's workflow name> <qualifies|fail> <clause> [reason phrase, @FILE@ = the file name]
  local k="$1" phrase="${6:-}"
  set_workflow_file "$(key_file "$k")" "$2" || return 0
  arrange_green # the run of <k> then reports the workflow name given as $3
  arrange_run "$(key_id "$k")" "$SHA completed success workflow_dispatch success $3"
  run_tag "$VER"
  if [ "$4" = qualifies ]; then
    expect_both_tagged "$5"
    assert_evidence_state "$5"
  else
    expect_failure 16 "$5"
    phrase="${phrase//@FILE@/$(key_file "$k")}"
    assert_only_missing "$5" "$k" "$phrase"
  fi
}
case_wf_name_line() { # <key> <`name:` line of the workflow file> <run's workflow name> <qualifies|fail> <clause> [phrase]
  case_wf_name_file "$1" "$(workflow_file_with "$2")" "$3" "$4" "$5" "${6:-}"
}
case_wf_name_unusable() { # <key> <no-name-line|empty-name|file-missing> <clause>
  local k="$1" file
  file="$(key_file "$k")"
  case "$2" in
    no-name-line) set_workflow_file "$file" $'on:\n  workflow_dispatch:\n\njobs:\n  lint:\n    name: Lint\n    runs-on: ubuntu-latest' || return 0 ;;
    empty-name) set_workflow_file "$file" $'name:\n\non:\n  workflow_dispatch:\n' || return 0 ;;
    quoted-empty-name) set_workflow_file "$file" $'name: ""\n\non:\n  workflow_dispatch:\n' || return 0 ;;
    file-missing) remove_workflow_file "$file" || return 0 ;;
  esac
  arrange_green # a qualifying run exists; only the expected name cannot be read
  run_tag "$VER"
  expect_failure 16 "$3"
  assert_only_missing "$3" "$k" "cannot determine workflow name"
  if [ "$2" != file-missing ]; then assert_hints "$3" "$k"; fi
}

# ── R6.1: the gate's own files come from a trusted directory (RELEASE_GATE_DIR) ──
case_gate_dir_same_names() {
  # World: arrange_green; the gate directory names the same workflows as the commit and holds a passing
  # lockstep stub. R6.1: gate mode is otherwise as before.
  use_gate_dir || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R6.1: a gate directory with the commit's own names passes"
  assert_gate_lockstep_call "R6.1a: the gate's lockstep call"
  assert_not_called "R6.1: the commit's own lockstep stub is not called in gate mode" lockstep
}
case_gate_dir_empty() {
  RELEASE_GATE_DIR="" # R6.1: "When unset or empty, both come from the working directory as before"
  export RELEASE_GATE_DIR
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R6.1: an empty RELEASE_GATE_DIR behaves as unset"
  assert_eq "$(log_text lockstep)" "--expect $VER|" \
    "R6.1a: without RELEASE_GATE_DIR the call stays 'bash scripts/check-version-lockstep.sh --expect X.Y.Z'"
  assert_not_called "R6.1: no gate script ran" gate-lockstep
}
case_gate_dir_bad() { # <missing|no-lockstep|a-file>
  case "$1" in
    missing) RELEASE_GATE_DIR="$SB/does-not-exist" ;;
    no-lockstep)
      RELEASE_GATE_DIR="$SB/gate-incomplete"
      mkdir -p "$RELEASE_GATE_DIR/.github/workflows" # a directory that lacks the lockstep script
      ;;
    a-file) RELEASE_GATE_DIR="$SB/a-file"; : > "$RELEASE_GATE_DIR" ;;
  esac
  export RELEASE_GATE_DIR
  arrange_green
  run_tag "$VER"
  expect_failure 2 "R6.1: RELEASE_GATE_DIR $1: exit 2"
  assert_stderr_has "RELEASE_GATE_DIR" "R6.1: the message of exit 2 names the variable"
  assert_not_called "R6.1: exit 2 precedes every check" lockstep gate-lockstep cargo gh
  assert_no_evidence_lines "R6.1"
}
case_gate_wf_name() { # <key> <gate name | NONE | MISSING> <commit's own name> <run's workflow name> <qualifies|fail> <clause> [phrase]
  # World: arrange_green, the run of <key> reporting <4>; the gate directory names <2> for <key> and the
  # commit's own workflow file says <3>, so only the gate's file can decide.
  local k="$1" phrase="${7:-}"
  use_gate_dir "$k=$2" || return 0
  set_workflow_file "$(key_file "$k")" "$(workflow_file_with "name: $3")" || return 0
  arrange_green
  arrange_run "$(key_id "$k")" "$SHA completed success workflow_dispatch success $4"
  run_tag "$VER"
  if [ "$5" = qualifies ]; then
    expect_both_tagged_full "$6"
    assert_gate_lockstep_call "R6.1a"
  else
    expect_failure 16 "$6"
    phrase="${phrase//@FILE@/$(key_file "$k")}"
    assert_only_missing "$6" "$k" "$phrase"
    assert_hints "$6" "$k"
  fi
}
case_gate_lockstep() { # <gate rc> <commit rc> <pass|fail> <clause>
  # World: arrange_green; the gate directory's lockstep stub exits <1>, the commit's own stub <2>.
  use_gate_dir || return 0
  set_rc gate-lockstep "$1"
  set_rc lockstep "$2"
  arrange_green
  run_tag "$VER"
  assert_gate_lockstep_call "R6.1a: --root <absolute working directory> --expect X.Y.Z, in that order"
  assert_not_called "R6.1: the commit's own lockstep stub is not called in gate mode" lockstep
  if [ "$3" = pass ]; then
    expect_both_tagged_full "$4"
  else
    expect_failure 13 "$4"
    assert_not_called "§4 check order: nothing after check 4 runs" cargo gh
  fi
}
case_gate_changelog_of_commit() {
  # R6.1: "Everything that describes the COMMIT ... is still read from the commit being tagged": the gate
  # directory has no CHANGELOG.md; the commit's entry decides.
  use_gate_dir || return 0
  set_changelog $'# Changelog\n\n## [Unreleased]\n'
  arrange_green
  run_tag "$VER"
  expect_failure 15 "R6.1 / §4 check 6: the CHANGELOG.md of the commit decides in gate mode"
  assert_not_called "§4 check order: nothing after check 6 runs" gh
  assert_no_evidence_lines "R6.1"
}
case_gate_note_of_commit() {
  use_gate_dir || return 0
  remove_note || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R6.1 / R4.5: the evidence note of the commit decides in gate mode"
  assert_stderr_has "Tested-SHA" "R4.5 / R5.1: the message of exit 20"
  assert_evidence_lines "R6.1 / R4.5" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
}

# ── §4 check 3 (R4.2): tags must not exist → exit 12 ───────────────────────
case_tag_exists() { # <local|remote> <tag> <clause> [extra args...]
  make_tag "$1" "$2"
  arrange_green
  run_tag "$VER" "${@:4}"
  expect_failure 12 "$3"
  assert_not_called "§4 check order: nothing after check 3 runs" lockstep cargo gh
  if [ "$1" = local ]; then
    assert_stderr_has "exists locally" "R4.2: a tag found by git rev-parse is reported as 'exists locally'"
  else
    assert_stderr_has "exists on origin" "R4.2: a tag only git ls-remote prints is reported as 'exists on <remote>'"
    assert_stderr_lacks "exists locally" "R4.2: the fetch imported nothing, so the remote-only tag is not local"
  fi
}
case_tag_exists_local_and_remote() { # <tag>
  must git -C "$REPO" tag "$1" || return 0
  must git -C "$REPO" push -q origin "refs/tags/$1" || return 0 # and it stays local
  arrange_green
  run_tag "$VER"
  expect_failure 12 "§4 check 3: the tag exists on both sides"
  assert_stderr_has "exists locally" "R4.2: 'exists locally' when git rev-parse succeeds"
}
case_tag_exists_on_chosen_remote() { # <tag>
  add_upstream || return 0
  must git -C "$REPO" tag "$1" || return 0
  must git -C "$REPO" push -q upstream "refs/tags/$1" || return 0
  must git -C "$REPO" tag -d "$1" || return 0
  arrange_green
  run_tag "$VER" --remote upstream
  expect_failure 12 "§4 check 3: the tag exists on the chosen remote"
  assert_stderr_has "exists on upstream" "R4.2: the message names the remote that has the tag"
  assert_stderr_lacks "exists locally" "R4.2: nothing was imported locally"
}

# R6.3 WITHDRAWN: the lookup stays `git ls-remote --tags <remote>` (R4.2), which sees the fetch side; by
# R4.1 and R6.2 the fetch and push URLs name the same GitHub repository, so a separate push destination
# exists only under the R5.3 seam (RELEASE_ALLOW_UNPARSED_PUSH_URL=1, set by new_sandbox). Recorded
# behaviour under the seam, named here because the clause is withdrawn: a tag that exists only at the
# separate push destination is invisible to check 3 -- `--dry-run` passes; a real run's atomic push is
# refused (the destination already has that tag; a tag update without --force is rejected), the local
# tags are removed, nothing is tagged at either place, exit 17, and the pre-existing tag at the push
# destination is unchanged.
case_tag_at_push_destination() { # <pushurl> <tag> [extra args...]
  # (Only `remote.<name>.pushurl` routes the script's by-name push to a separate destination: a URL
  # rewritten by `url.<base>.pushInsteadOf` is bypassed by the harness's vcs helper, which serves
  # every by-name operation from the fetch side. The rewrite itself is covered by the
  # `pushinsteadof-*` cases of the R4.1 push-URL section.)
  local dry=0 a before
  for a in "${@:3}"; do [ "$a" = --dry-run ] && dry=1; done
  must git init -q --bare "$UPSTREAM" || return 0
  must git -C "$REPO" config remote.origin.pushurl "$UPSTREAM" || return 0
  must git -C "$REPO" tag "$2" || return 0
  must git -C "$REPO" push -q "$UPSTREAM" "refs/tags/$2" || return 0 # straight to the push destination's bare repo ...
  must git -C "$REPO" tag -d "$2" || return 0                 # ... the fetch side never does, and it is not local
  before="$(git -C "$UPSTREAM" rev-parse "refs/tags/$2")"
  arrange_green
  run_tag "$VER" "${@:3}"
  if [ "$dry" = 1 ]; then
    # R6.3 WITHDRAWN: --dry-run cannot see a tag at a separate push destination and passes.
    assert_rc 0 "R6.3 WITHDRAWN: --dry-run passes although $2 exists at the push destination ($1)"
    assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4: last stdout line for --dry-run"
    assert_nothing_created "R6.3 WITHDRAWN: --dry-run creates and pushes nothing"
  else
    # R6.3 WITHDRAWN: a real run tags, is refused, cleans up after itself and exits 17.
    expect_failure 17 "R6.3 WITHDRAWN: a real run is refused because $2 exists at the push destination ($1)"
    assert_tag_absent local "$CRATE_TAG" "R6.3 WITHDRAWN: the local tags created in step 8 are removed"
    assert_tag_absent local "$GO_TAG" "R6.3 WITHDRAWN: the local tags created in step 8 are removed"
  fi
  # Nothing is tagged at either place, and the pre-existing tag at the push destination is unchanged.
  assert_eq "$(git -C "$UPSTREAM" rev-parse "refs/tags/$2" 2>/dev/null)" "$before" \
    "the pre-existing tag at the push destination [R6.3 WITHDRAWN: unchanged]"
  assert_eq "$(git -C "$UPSTREAM" for-each-ref --format='%(refname:short)' refs/tags | sort | tr '\n' ' ')" "$2 " \
    "tags at the push destination [R6.3 WITHDRAWN: only the pre-existing one; no new tag lands]"
  assert_eq "$(git -C "$REMOTE" for-each-ref --format='%(refname:short)' refs/tags)" "" \
    "tags on the fetch side [R6.3 WITHDRAWN: nothing is tagged there either]"
}
case_tag_on_fetch_side_found() {
  # R4.2 (the lookup restored by the withdrawn R6.3): `git ls-remote --tags <remote>` sees the FETCH side,
  # so a tag that exists only there is found: exit 12, `exists on <remote>`, and nothing after check 3 runs
  # (the push destination, a separate bare repository under the R5.3 seam, does not have it).
  must git init -q --bare "$UPSTREAM" || return 0
  must git -C "$REPO" config remote.origin.pushurl "$UPSTREAM" || return 0
  must git -C "$REPO" tag "$CRATE_TAG" || return 0
  must git -C "$REPO" push -q "$REMOTE" "refs/tags/$CRATE_TAG" || return 0 # straight to the fetch-side bare repo
  must git -C "$REPO" tag -d "$CRATE_TAG" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 12 "R4.2: $CRATE_TAG exists on the fetch side, which the by-remote-name lookup sees"
  assert_stderr_has "exists on origin" "R4.2: the message names the remote"
  assert_stderr_lacks "exists locally" "R4.2: the fetch imported nothing, so it is not local"
  assert_not_called "§4 check order: nothing after check 3 runs" lockstep cargo gh
  assert_eq "$(git -C "$UPSTREAM" for-each-ref --format='%(refname:short)' refs/tags)" "" \
    "the push destination stays empty [R4.2: the tag is on the fetch side only]"
}
case_no_go_tag_ignores_go_tag() { # <local|remote>
  make_tag "$1" "$GO_TAG"
  arrange_green
  run_tag "$VER" --no-go-tag
  # §4 check 3: the Go tag is only checked "unless --no-go-tag"
  expect_crate_tagged_only "§4 check 3: --no-go-tag ignores an existing Go tag ($1)"
  assert_tag_unchanged "$GO_TAG" "§4 --no-go-tag creates and pushes no Go tag"
}
case_similar_tags_do_not_block() { # <local|remote>
  local t
  for t in "v$VER-rc.1" "v${VER}0" "v1.2.2" "bindings/rsac-go/v${VER}0" "bindings/rsac-go/v1.2.2"; do
    make_tag "$1" "$t"
  done
  arrange_green
  run_tag "$VER"
  # §4 check 3: the refs are exact (refs/tags/vX.Y.Z); other versions do not collide
  expect_both_tagged "§4 check 3: only the exact tags vX.Y.Z block ($1)"
  assert_no_tag_lost "§4 check 3: other tags survive"
}
case_tags_before_lockstep() {
  make_tag local "$CRATE_TAG"
  set_rc lockstep 1
  arrange_green
  run_tag "$VER"
  expect_failure 12 "§4 table: check 3 before check 4"
  assert_not_called "§4 table: check 3 before check 4" lockstep cargo gh
}
case_fetch_before_tags() {
  touch_commit "unpushed"
  make_tag local "$CRATE_TAG"
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 table: check 2 before check 3"
  assert_not_called "§4 table: check 2 before check 3" lockstep cargo gh
}

# ── R3.4: Go module major version (check 3b, exit 18) ──────────────────────
case_go_major() { # <version> <module path suffix> <pass|crate-only|fail> <clause> [extra args...]
  use_version "$1"
  set_gomod "$GO_MODULE$2" || return 0
  arrange_green
  run_tag "$VER" "${@:5}"
  case "$3" in
    pass) expect_both_tagged "$4" ;;
    crate-only) expect_crate_tagged_only "$4" ;;
    fail)
      expect_failure 18 "$4"
      assert_stderr_has "--no-go-tag" "R3.4: the message of exit 18 names --no-go-tag"
      assert_not_called "R3.4: nothing after check 3b runs" lockstep cargo gh
      ;;
  esac
}
case_go_major_after_tag_check() {
  use_version 2.0.0
  set_gomod "$GO_MODULE" || return 0 # no /v2: would be exit 18
  make_tag local "$CRATE_TAG"        # check 3 is exit 12
  arrange_green
  run_tag "$VER"
  expect_failure 12 "R3.4: check 3b comes after check 3"
}
case_go_major_before_lockstep() {
  use_version 2.0.0
  set_gomod "$GO_MODULE" || return 0
  set_rc lockstep 1 # check 4 would be exit 13
  arrange_green
  run_tag "$VER"
  expect_failure 18 "R3.4: check 3b comes before check 4"
  assert_not_called "R3.4: nothing after check 3b runs" lockstep cargo gh
}
case_dry_run_checks_go_module() {
  use_version 2.0.0
  set_gomod "$GO_MODULE" || return 0
  arrange_green
  run_tag "$VER" --dry-run
  expect_failure 18 "R3.4 / §4 --dry-run: check 3b is among checks 0 to 7"
}

# ── §4 check 4: version lockstep → exit 13 ─────────────────────────────────
case_lockstep_fails() { # <stub exit code>
  set_rc lockstep "$1"
  arrange_green
  run_tag "$VER"
  expect_failure 13 "§4 check 4: scripts/check-version-lockstep.sh --expect X.Y.Z exits non-zero (stub exit $1)"
  assert_eq "$(log_text lockstep)" "--expect $VER|" "§4 check 4: stub arguments"
  assert_not_called "§4 check order: nothing after check 4 runs" cargo gh
}
case_lockstep_before_cargo() {
  set_rc lockstep 1
  set_rc cargo 1
  arrange_green
  run_tag "$VER"
  expect_failure 13 "§4 table: check 4 before check 5"
  assert_not_called "§4 table: check 4 before check 5" cargo gh
}

# ── §4 check 5: cargo metadata --locked → exit 14 ──────────────────────────
case_cargo_fails() {
  set_rc cargo 1
  arrange_green
  run_tag "$VER"
  expect_failure 14 "§4 check 5: cargo metadata --locked --format-version 1 exits non-zero"
  assert_eq "$(log_text cargo)" "metadata --locked --format-version 1|" "§4 check 5: cargo arguments"
  assert_not_called "§4 check order: nothing after check 5 runs" gh
}
case_cargo_before_changelog() {
  set_rc cargo 1
  set_changelog $'# Changelog\n\n## [Unreleased]\n'
  arrange_green
  run_tag "$VER"
  expect_failure 14 "§4 table: check 5 before check 6"
  assert_not_called "§4 table: check 5 before check 6" gh
}

# ── §4 check 6: CHANGELOG.md entry → exit 15 ───────────────────────────────
case_changelog_bad() { # <clause> <content>
  set_changelog "$2"
  arrange_green
  run_tag "$VER"
  expect_failure 15 "$1"
  assert_not_called "§4 check order: nothing after check 6 runs" gh
  assert_no_evidence_lines "$1"
}
case_changelog_missing_file() {
  remove_changelog
  arrange_green
  run_tag "$VER"
  expect_failure 15 "§4 check 6: no CHANGELOG.md means no matching line"
  assert_not_called "§4 check order: nothing after check 6 runs" gh
}
case_changelog_entry_anywhere_ok() {
  set_changelog $'# Changelog\n\nintro text\n\n## [Unreleased]\n\n### Added\n\n- x\n\n## [1.2.4] - 2026-02-01\n\n## [1.2.3] - 2026-01-02\n\n### Fixed\n\n- y\n\n## [1.2.2] - 2025-12-01\n'
  arrange_green
  run_tag "$VER"
  expect_both_tagged "§4 check 6: a line anywhere in the file matches"
}
case_changelog_before_ci() {
  set_changelog $'# Changelog\n\n## [Unreleased]\n'
  run_tag "$VER" # no run is arranged either: a missing run would be exit 16 (check 7)
  expect_failure 15 "§4 table: check 6 before check 7"
  assert_not_called "§4 table: check 6 before check 7" gh
}
case_changelog_missing_before_gh_calls() {
  set_changelog $'# Changelog\n'
  arrange_green
  run_tag "$VER"
  expect_failure 15 "§4 table: check 6 before check 7"
  assert_not_called "§4 table: check 6 before check 7" gh
}

# ── R4.5: the workstation evidence note (check 6b, exit 20) ────────────────
case_ws_note_content() { # <exact note content, a newline is added unless it starts with @NONL@> <ok|20> <phrase> <clause>
  local content="$1"$'\n'
  case "$1" in @NONL@*) content="${1#@NONL@}" ;; esac
  set_note_raw "$content" || return 0
  arrange_green
  run_tag "$VER"
  if [ "$2" = ok ]; then
    expect_both_tagged_full "$4"
  else
    expect_failure 20 "$4"
    assert_stderr_has "$3" "$4"
    # R4.5: the failure exits 20 after all five lines are printed; the four R4.3 requirements are satisfied here
    assert_evidence_lines "$4" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
    assert_hints "$4"
  fi
}
case_ws_note_missing() {
  remove_note || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R4.5: no docs/evidence/windows-default-switch.md"
  assert_stderr_has "Tested-SHA" "R4.5: the message of exit 20"
  assert_evidence_lines "R4.5: five lines, the fifth missing" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R4.5: no dispatch hint for the note"
  # R4.5: the four R4.3 requirements were still evaluated before the exit
  assert_eq "$(gh_count 'run list ')" 4 "listings [R4.3: all four evaluated, then 20]"
  assert_eq "$(gh_count 'run view ')" 4 "inspections [R4.3: all four evaluated, then 20]"
}
case_ws_sha_not_ancestor() { # <unknown|side-branch>
  local sha
  case "$1" in
    unknown) sha="$NO_SUCH_SHA" ;;
    side-branch) sha="$(git -C "$REPO" commit-tree "HEAD~1^{tree}" -p "HEAD~1" -m side)" ;; # exists, not an ancestor of HEAD
  esac
  set_note_body "Tested-SHA: $sha" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R4.5: Tested-SHA ($1) is not HEAD or an ancestor of HEAD"
  assert_stderr_has "not an ancestor" "R4.5: the message of exit 20"
  assert_evidence_lines "R4.5" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
}
case_ws_sha_is_head() {
  # R5.1 (settles the AMBIGUOUS(R4.5) marker of the first version): check 6b reads the note from HEAD's
  # tree, never from the working tree, and a committed note can never name HEAD itself (the note is
  # committed after the test ran). A note naming HEAD therefore exists only in the working tree:
  # untracked (check 1 allows untracked files), not in HEAD's tree → missing → exit 20.
  remove_note || return 0
  printf '# note\n\nTested-SHA: %s\n' "$SHA" > "$REPO/$NOTE_PATH" # untracked: present in the working tree only
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R5.1: a note naming HEAD is not in HEAD's tree, so it is missing"
  assert_stderr_has "Tested-SHA" "R4.5/R5.1: the message of exit 20"
  assert_evidence_lines "R5.1: the fifth line is missing, the four requirements are ok" \
    ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R5.1: no dispatch hint for the note"
}
case_ws_stale_note_fresh_untracked_copy() {
  # R5.1: the note of HEAD is stale (a path of WINDOWS_SWITCH_PATHS changed after Tested-SHA); a fresh
  # copy sitting UNTRACKED in the working tree (check 1 allows untracked files) cannot vouch for the commit.
  change_path src/api/recovery.rs || return 0
  printf '# fresh\n\nTested-SHA: %s\n' "$SHA" > "$REPO/docs/evidence/windows-default-switch-fresh.md" # untracked
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R5.1: a fresh untracked copy does not save the stale note of HEAD"
  # R4.5: the staleness here is a path difference, so the message lists the changed path (not 'Tested-SHA')
  assert_stderr_has "src/api/recovery.rs" "R4.5/R5.1: the message lists the changed path"
  assert_stderr_has "--waive windows-switch=" "R4.5: the message names the waiver"
  assert_evidence_lines "R5.1" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R5.1: no dispatch hint for the note"
}
case_ws_stale_note_fresh_modified_copy() {
  # R5.1: a fresh MODIFIED copy of the tracked note leaves the tracked tree dirty, and check 1 (exit 10)
  # fires before check 6b can read anything; asserted here, as the instruction for this revision says.
  change_path src/api/recovery.rs || return 0
  printf '# fresh\n\nTested-SHA: %s\n' "$SHA" > "$REPO/$NOTE_PATH" # modified tracked file
  arrange_green
  run_tag "$VER"
  expect_failure 10 "R5.1 / §4 check 1: a modified tracked note fires check 1 (exit 10) before check 6b"
  assert_not_called "§4 check order: nothing after check 1 runs" lockstep cargo gh
  assert_no_evidence_lines "R5.1"
}
case_ws_note_only_in_working_tree() {
  # R5.1: a note that is not in HEAD's tree is missing, whatever the working tree contains.
  remove_note || return 0
  printf '# fresh\n\nTested-SHA: %s\n' "$SHA" > "$REPO/$NOTE_PATH" # untracked (check 1 allows it)
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R5.1: a note present only in the working tree is missing"
  assert_stderr_has "Tested-SHA" "R4.5/R5.1: the message of exit 20"
  assert_evidence_lines "R5.1" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R5.1"
}
case_ws_committed_note_working_copy_deleted() {
  # R5.1: the committed note is fresh, but deleting the working-tree copy is a tracked change, and check 1
  # (exit 10) does not allow that state; asserted here, as the instruction for this revision says.
  rm "$REPO/$NOTE_PATH" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 10 "R5.1 / §4 check 1: deleting the tracked note fires check 1 (exit 10) before check 6b"
  assert_not_called "§4 check order: nothing after check 1 runs" lockstep cargo gh
}
case_ws_path_changed() { # <path> [<second path>]
  change_path "$1" || return 0
  if [ -n "${2:-}" ]; then change_path "$2" || return 0; fi
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R4.5: $1 differs between Tested-SHA and HEAD"
  assert_stderr_has "$1" "R4.5: the message lists the changed path (git diff --name-only)"
  if [ -n "${2:-}" ]; then assert_stderr_has "$2" "R4.5: the message lists every changed path"; fi
  assert_stderr_has "--waive windows-switch=" "R4.5: the message names the waiver"
  # AMBIGUOUS(R4.5): the contract says the message says "to rerun the workstation test and update the note"; the wording is
  # not fixed, the words 'workstation' and 'note' are what is looked for (any case).
  assert_stderr_has_ci "workstation" "R4.5: rerun the workstation test"
  assert_stderr_has_ci "note" "R4.5: update the note"
  assert_evidence_lines "R4.5" ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R4.5"
}
case_ws_path_deleted() { # <path>
  delete_path "$1" || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 20 "R4.5: $1 was deleted after Tested-SHA (a difference)"
  assert_stderr_has "$1" "R4.5: the message lists the deleted path"
}
case_ws_path_changed_then_reverted() {
  change_path src/api/recovery.rs || return 0
  must git -C "$REPO" checkout "$TESTED_SHA" -- src/api/recovery.rs || return 0
  commit_local "revert src/api/recovery.rs" && push_master || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R4.5: 'git diff --quiet <Tested-SHA> HEAD' compares the two commits, so a change that was reverted is no difference"
}
case_ws_tested_sha_after_path_change() {
  local c
  change_path src/api/recovery.rs || return 0
  c="$SHA"
  set_note_body "Tested-SHA: $c" || return 0
  arrange_green
  EV_windows_switch="$c"
  run_tag "$VER"
  expect_both_tagged_full "R4.5: the note names the commit after the change; no difference from it to HEAD"
}
case_ws_unrelated_change() { # <path>
  change_path "$1" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "R4.5: $1 is not one of WINDOWS_SWITCH_PATHS"
}
case_ws_before_changelog_precedes() {
  remove_note || return 0
  set_changelog $'# Changelog\n\n## [Unreleased]\n' || return 0
  arrange_green
  run_tag "$VER"
  expect_failure 15 "R4.5: check 6b comes after the changelog check (6)"
  assert_no_evidence_lines "R4.5"
}
case_ws_and_ci_missing() {
  remove_note || return 0
  world ci=none audio=green android-emu=green ios-sim=green
  run_tag "$VER"
  expect_failure 16 "R4.5: when an R4.3 requirement is missing too, 16 wins over 20"
  assert_evidence_lines "R4.5: five lines" ci=missing:"no run found" audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 windows-switch=missing:
  assert_hints "R4.3: a dispatch hint for ci only" ci
}
case_ws_checked_by_dry_run() {
  remove_note || return 0
  arrange_green
  run_tag "$VER" --dry-run
  expect_failure 20 "R4.5 / §4 --dry-run: check 6b is among the checks --dry-run runs"
  assert_stderr_has "Tested-SHA" "R4.5"
}
case_ws_waived() { # <note-missing|path-changed|note-ok>
  case "$1" in
    note-missing) remove_note || return 0 ;;
    path-changed) change_path src/core/recovery.rs || return 0 ;;
    note-ok) ;;
  esac
  arrange_green
  honour_waiver windows-switch 55
  run_tag "$VER" --waive windows-switch=55
  expect_both_tagged_full "R4.5/R4.4: an honoured windows-switch waiver skips the note check ($1)"
  assert_waived_lines "R4.4" windows-switch=55
}

# ── §4 check 7, R4.3: evidence → exit 16 ───────────────────────────────────
# One requirement fails (or several: <key=spec>...), every other is satisfied; the report lines, the reason phrases and
# the hints are those of R4.3.
case_evidence_missing() { # <clause> <key=spec>...
  local clause="$1" k s kv first=""
  shift
  local -a specs=() hints=()
  world_with "$@"
  run_tag "$VER"
  expect_failure 16 "$clause"
  for k in $KEYS; do
    s=""
    for kv in "$@"; do
      if [ "${kv%%=*}" = "$k" ]; then s="${kv#*=}"; fi
    done
    if [ -n "$s" ]; then
      specs+=("$k=missing:$(spec_phrase "$k" "$s")")
      hints+=("$k")
      [ -n "$first" ] || first="$k"
      # r2: a conclusion that is not success is 'failed'; 'Release Readiness failed' (job name substituted) is another reason
      case "$s" in
        failed | cancelled | conclusion-none)
          if stderr_lf | grep "^tag-release: evidence $k missing: " | tr -d "\`'\"" | grep -qiF -- "$(key_job "$k") failed"; then
            fail "the reason for a run whose conclusion is '$s' names the job ('$(key_job "$k") failed') [R4.3: reason 'failed']"
          fi
          ;;
      esac
    else
      specs+=("$k=ok:$(ev_get "$k")")
    fi
  done
  specs+=("windows-switch=ok:$TESTED_SHA")
  assert_repository_line "$clause"
  assert_evidence_lines "$clause" "${specs[@]}"
  assert_hints "$clause" "${hints[@]}"
  assert_lists_and_views "$clause" "${hints[@]}"
}
case_evidence_one() { # <key> <spec> <clause>
  # World: <key>=<spec>, the other three keys green; issues: none; note: ok
  case_evidence_missing "$3" "$1=$2"
}
case_evidence_all_ok() {
  # World: ci=green audio=green android-emu=green ios-sim=green; issues: none; note: ok
  world ci=green audio=green android-emu=green ios-sim=green
  run_tag "$VER"
  expect_both_tagged_full "R4.3/R4.5/R4.6: all four requirements and the note are satisfied"
  assert_evidence_lines "R4.3 / R4.5: the five report lines, in order" \
    ci=ok:4242 audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  assert_tag_message remote "$CRATE_TAG" $'rsac 1.2.3\n\nEvidence: ci=4242\nEvidence: audio=4343\nEvidence: android-emu=4444\nEvidence: ios-sim=4545\nEvidence: windows-switch='"$TESTED_SHA" \
    "R4.6: the crate tag message, spelled out"
  assert_tag_message remote "$GO_TAG" $'rsac-go 1.2.3\n\nEvidence: ci=4242\nEvidence: audio=4343\nEvidence: android-emu=4444\nEvidence: ios-sim=4545\nEvidence: windows-switch='"$TESTED_SHA" \
    "R4.6: the Go tag message: the same body, first line 'rsac-go X.Y.Z'"
  assert_no_waived_lines "R4.4: nothing was waived"
  assert_eq "$(gh_count 'issue view ')" 0 "issue lookups [R4.4: only a requested waiver is verified]"
  assert_hints "R4.3: no hint when nothing is missing"
}
case_evidence_two_missing_report() {
  # World: ci=wrong-commit audio=green android-emu=job-failed ios-sim=green; issues: none; note: ok
  case_evidence_missing "R4.3: two requirements missing at once, each with its own reason" ci=wrong-commit android-emu=job-failed
}
case_evidence_cli_failure_others_evaluated() {
  # World: ci=view-fails audio=none android-emu=green ios-sim=green; issues: none; note: ok
  world ci=view-fails audio=none android-emu=green ios-sim=green
  run_tag "$VER"
  expect_failure 16 "R4.3: any failure of the GitHub CLI is missing for that key, and the other keys are still evaluated"
  assert_evidence_lines "R4.3" ci=missing: audio=missing:"no run found" android-emu=ok:4444 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  assert_hints "R4.3: a hint per missing key" ci audio
  assert_lists_and_views "R4.3" audio
}
case_evidence_first_view_fails_second_would_qualify() { # <key> <first id> <second id>
  # R5.7 (settles the AMBIGUITY of the first version): a CLI failure while inspecting a listed run makes
  # the key missing at once; the next listed run is not tried.
  local k="$1"
  arrange_green
  arrange_list "$k" "$2"$'\n'"$3"$'\n'
  arrange_run "$2" "$(spec_line "$k" green)"
  set_rc "gh-view-$2" 1
  arrange_run "$3" "$(spec_line "$k" green)"
  run_tag "$VER"
  expect_failure 16 "R4.3: a failing CLI call is 'missing' for the key (reading A)"
  assert_only_missing "R4.3" "$k" ""
}
case_evidence_later_run_used() { # <key> <id1> <id2> <id3>
  # World: <key> lists <id1> (failed) <id2> (in progress) <id3> (green); the other keys green; issues: none; note: ok
  local k="$1"
  arrange_green
  arrange_list "$k" "$2"$'\n'"$3"$'\n'"$4"$'\n'
  arrange_run "$2" "$(spec_line "$k" failed)"
  arrange_run "$3" "$(spec_line "$k" in-progress)"
  arrange_run "$4" "$(spec_line "$k" green)"
  ev_set "$k" "$4"
  run_tag "$VER"
  # §4 check 7 (per requirement, R4.3): runs are inspected in order; the first that qualifies is used (and named in the tag message, R4.6)
  expect_both_tagged_full "R4.3: a later listed run is used when earlier ones do not qualify ($k)"
  assert_eq "$(grep -E "^run view ($2|$3|$4) " "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "$2 $3 $4 " \
    "inspection order for $k [§4 check 7: in listed order]"
}
case_evidence_no_listed_run_qualifies() {
  # World: ci lists 101 (failed) and 102 (in progress); the other keys green; issues: none; note: ok
  arrange_green
  arrange_list ci $'101\n102\n'
  arrange_run 101 "$(spec_line ci failed)"
  arrange_run 102 "$(spec_line ci in-progress)"
  run_tag "$VER"
  expect_failure 16 "§4 check 7 / R4.3: no listed run qualifies"
  # R5.5: several listed runs are inspected and none qualifies: the reason is that of the first listed run
  assert_evidence_lines "R4.3/R5.5" ci=missing:failed audio=ok:4343 android-emu=ok:4444 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  assert_hints "R4.3" ci
}
case_evidence_run_20_qualifies() { # <key>
  # R5.4: "At most twenty runs are inspected per key": the run in position 20 of the listing is inspected
  # and satisfies the key (World: <key> lists nineteen failed runs and one green as the twentieth; the
  # other keys green; issues: none; note: ok).
  local k="$1" i id list=""
  arrange_green
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19; do
    id=$((2000 + i))
    list+="$id"$'\n'
    arrange_run "$id" "$(spec_line "$k" failed)"
  done
  arrange_run 2200 "$(spec_line "$k" green)"
  list+="2200"$'\n'
  arrange_list "$k" "$list"
  ev_set "$k" 2200
  run_tag "$VER"
  expect_both_tagged_full "R5.4: the twentieth listed run is inspected and satisfies $k"
  assert_eq "$(gh_count 'run view 2200 ')" 1 "inspections of the twentieth run [R5.4]"
}
case_evidence_run_21_ignored() { # <key>
  # R5.4: ids beyond the twentieth listed are ignored, so a qualifying run in position 21 does not satisfy
  # the key; R5.5: the reason is that of the first listed run (World: <key> lists twenty failed runs and
  # one green as the twenty-first; the other keys green; issues: none; note: ok).
  local k="$1" i id list=""
  arrange_green
  for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    id=$((2000 + i))
    list+="$id"$'\n'
    arrange_run "$id" "$(spec_line "$k" failed)"
  done
  arrange_run 2221 "$(spec_line "$k" green)" # position 21: green, but never inspected
  list+="2221"$'\n'
  arrange_list "$k" "$list"
  run_tag "$VER"
  expect_failure 16 "R5.4: a qualifying run in position 21 does not satisfy $k"
  assert_eq "$(gh_count 'run view 2221 ')" 0 "inspections of the twenty-first run [R5.4: ignored]"
  assert_only_missing "R5.4/R5.5: the reason is that of the first listed run (failed)" "$k" "failed"
  assert_hints "R5.4" "$k"
}
case_evidence_first_listed_reason() { # <key>
  # R5.5: when several listed runs are inspected and none qualifies, the reported reason is that of the
  # first (newest) listed run (World: <key> lists a queued, a failed and a pushed run; the others green).
  local k="$1"
  arrange_green
  arrange_list "$k" $'2101\n2102\n2103\n'
  arrange_run 2101 "$(spec_line "$k" queued)"
  arrange_run 2102 "$(spec_line "$k" failed)"
  arrange_run 2103 "$(spec_line "$k" event-push)"
  run_tag "$VER"
  expect_failure 16 "R5.5: none of the listed runs of $k qualifies"
  assert_only_missing "R5.5: the reason of the FIRST listed run (queued → 'still running'), not 'failed' or 'not dispatched'" "$k" "still running"
  assert_hints "R5.5" "$k"
}
case_evidence_no_run_custom_branch() {
  must git -C "$REPO" push -q origin master:release
  EXP_BRANCH=release
  world ci=none audio=green android-emu=none ios-sim=green
  run_tag "$VER" --branch release
  expect_failure 16 "R4.3: no run on branch release for ci and android-emu"
  assert_hints "R4.3: the hint carries --ref <branch>" ci android-emu
  assert_eq "$(gh_count ' --branch release ')" 4 "listings on branch release"
}
case_evidence_all_four_missing() {
  case_evidence_missing "R4.3: all four requirements missing: every one is reported, with its own hint" \
    ci=none audio=wrong-commit android-emu=queued ios-sim=event-push
}
case_evidence_failure_creates_no_tags() {
  world ci=none audio=none android-emu=none ios-sim=none
  run_tag "$VER"
  expect_failure 16 "R4.3: checks 8-9 only run after the evidence passes"
  assert_eq "$(wc -l < "$SB/tags.after.local" | tr -d ' ')" 0 "local tags after exit 16 [§4 check 8 comes after check 7]"
}
case_dry_run_fails_on_check7() {
  world ci=none audio=green android-emu=green ios-sim=green
  run_tag "$VER" --dry-run
  expect_failure 16 "§4 --dry-run: runs checks 0 to 7, so a missing run still fails"
}
case_dry_run_fails_on_dirty_tree() {
  printf 'edit\n' >> "$REPO/README.md"
  arrange_green
  run_tag "$VER" --dry-run
  expect_failure 10 "§4 --dry-run: runs checks 0 to 7, so a dirty tree still fails"
}

# ── R3.5, R4.3: the exit-16 hint depends on whether HEAD is the branch tip ──
case_hint_behind_tip() { # <branch> <key> <spec> [extra args...]
  local branch="$1" k="$2"
  make_head_behind_tip "$branch" || return 0
  EXP_BRANCH="$branch"
  world_with "$k=$3"
  run_tag "$VER" "${@:4}"
  expect_failure 16 "§4 check 7: no qualifying run for $k at HEAD"
  # R3.5: "the message says that a dispatch on <branch> tests the branch tip, not this commit, and names the two ways out: tag the
  # tip, or dispatch ci.yml on a ref at this commit and pass --run-id". R4.3: this text replaces the hint when HEAD is not the tip.
  # AMBIGUITY: the wording is not fixed; chosen reading: the message mentions the branch name, 'dispatch', 'tip' and '--run-id',
  # after the four evidence lines. (Whether the exact dispatch command must be absent, and whether the text is printed once or
  # per missing key, is not asserted.)
  assert_stderr_has_ci "$branch" "R3.5: the message names the branch"
  assert_stderr_has_ci "dispatch" "R3.5: the message talks about a dispatch"
  assert_stderr_has_ci "tip" "R3.5: the message says a dispatch tests the branch tip"
  assert_stderr_has "--run-id" "R3.5: the message names the --run-id way out"
  assert_stderr_after_evidence "--run-id" "R4.3: the two-ways-out text replaces the hint line, which follows the four evidence lines"
  assert_only_missing "R4.3" "$k" "$(spec_phrase "$k" "$3")"
}
case_hint_behind_tip_reason() {
  make_head_behind_tip master || return 0
  world ci=none audio=green android-emu=green ios-sim=green
  run_tag "$VER"
  expect_failure 16 "§4 check 7"
  assert_stderr_has_ci "no run found" "§4 check 7 / r2: the reason is still named when HEAD is behind the tip"
}
case_hint_at_tip() {
  world ci=none audio=none android-emu=none ios-sim=none
  run_tag "$VER"
  expect_failure 16 "§4 check 7"
  # R3.5: "When HEAD is the tip, the hint is the dispatch command as before." R4.3: one line per missing key, with --repo
  assert_hints "R3.5 / R4.3: HEAD is the tip of the fetched master, so every missing key gets its dispatch command" ci audio android-emu ios-sim
}

# ── the call forms ─────────────────────────────────────────────────────────
case_gh_call_forms() {
  arrange_green
  run_tag "$VER"
  assert_rc 0 "R4.3"
  local k view prefix expr o
  for k in $KEYS; do
    # §4 check 7 listing form (R3.1: --repo right after 'run list'; R4.3: --workflow <file> of the row)
    gh_logged "$(gh_list_form "$k")" || fail "no call '$(gh_list_form "$k")' [§4 check 7 listing form, R3.1, R4.3 for $k]"
    prefix="$(gh_view_prefix "$(key_id "$k")")"
    view="$(grep -F "run view $(key_id "$k") " "$FAKE_DIR/gh.log" | head -n 1)"
    case "$view" in
      "$prefix"*) ;;
      *) fail "the inspection call for $k is '$view', expected it to start with '$prefix' [§4 check 7 inspection form, R3.1, R3.2]"; continue ;;
    esac
    expr="${view#"$prefix"}"
    [ -n "$expr" ] || fail "inspection form for $k has an empty --jq expression [§4 check 7]"
    # R4.3: "the row's job name in the inspection filter"; the other rows' job names must not be there
    case "$expr" in *"$(key_job "$k")"*) ;; *) fail "--jq expression for $k never names the job '$(key_job "$k")' [R4.3]" ;; esac
    for o in $KEYS; do
      if [ "$o" != "$k" ]; then
        case "$expr" in *"$(key_job "$o")"*) fail "--jq expression for $k names the job '$(key_job "$o")' of $o [R4.3]" ;; esac
      fi
    done
    case "$expr" in *missing*) ;; *) fail "--jq expression for $k cannot print 'missing' [§4 check 7 readiness]" ;; esac
    case "$expr" in *none*) ;; *) fail "--jq expression for $k cannot print 'none' [§4 check 7 conclusion/readiness]" ;; esac
    case "$expr" in *workflowName*) ;; *) fail "--jq expression for $k never reads workflowName [R3.2 sixth field]" ;; esac
  done
  # R4.3: two calls per requirement, and "checked in this order"
  assert_eq "$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')" 8 "number of gh calls [R4.3: one listing and one inspection per requirement]"
  assert_eq "$(grep '^run list ' "$FAKE_DIR/gh.log" | sed -E 's/.*--workflow ([^ ]+) .*/\1/' | tr '\n' ' ')" \
    "ci.yml ci-audio-tests.yml ci-android-emu.yml ci-ios-sim.yml " "workflows listed, in order [R4.3 table order]"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "4242 4343 4444 4545 " \
    "runs inspected, in order [R4.3 table order]"
}
case_check_tool_forms() {
  arrange_green
  run_tag "$VER"
  assert_rc 0 "§4 checks 4 and 5"
  # R6.1a: without RELEASE_GATE_DIR the call stays `bash scripts/check-version-lockstep.sh --expect X.Y.Z`
  # (the relative path means the commit's checkout is the working directory).
  assert_eq "$(log_text lockstep)" "--expect $VER|" "§4 check 4 / R6.1a: bash scripts/check-version-lockstep.sh --expect X.Y.Z"
  assert_eq "$(log_text cargo)" "metadata --locked --format-version 1|" "§4 check 5: cargo metadata --locked --format-version 1"
  assert_not_called "R6.1: no gate script runs without RELEASE_GATE_DIR" gate-lockstep
}

# ── R4.3: --run-id <key>=<N> | <N> ─────────────────────────────────────────
case_run_id_key() { # <flag value> <key> <id>
  # World: arrange_green, plus run <id> green for <key>; the listing of <key> would offer its default run (a decoy)
  local k="$2" o
  arrange_green
  arrange_run "$3" "$(spec_line "$k" green)"
  ev_set "$k" "$3"
  run_tag "$VER" --run-id "$1"
  expect_both_tagged_full "R4.3: --run-id $1 inspects only run $3 for $k"
  for o in $KEYS; do
    if [ "$o" = "$k" ]; then
      assert_eq "$(gh_count "--workflow $(key_file "$o") ")" 0 "listings of $o [R4.3: with a run id only that run is inspected]"
      assert_eq "$(gh_count "run view $(key_id "$o") ")" 0 "inspections of the decoy $(key_id "$o") [R4.3]"
      assert_eq "$(gh_count "run view $3 ")" 1 "inspections of run $3 [R4.3]"
    else
      gh_logged "$(gh_list_form "$o")" || fail "the other requirement $o was not listed [R4.3: --run-id is per key]"
    fi
  done
}
case_run_id_all_four() {
  # World: runs 11 22 33 44 green for ci audio android-emu ios-sim; the listings offer decoys that must not be consulted
  local k i=0 id
  arrange_green
  for k in $KEYS; do
    i=$((i + 1))
    id="$i$i"
    arrange_run "$id" "$(spec_line "$k" green)"
    ev_set "$k" "$id"
  done
  run_tag "$VER" --run-id ci=11 --run-id audio=22 --run-id android-emu=33 --run-id ios-sim=44
  expect_both_tagged_full "R4.3: --run-id is repeatable"
  assert_eq "$(gh_count 'run list ')" 0 "listings [R4.3: all four keys have a run id]"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "11 22 33 44 " "runs inspected [R4.3]"
}
case_run_id_bare_and_key() {
  # `--run-id 5` means ci=5; audio=6 in the same invocation
  arrange_green
  arrange_run 5 "$(spec_line ci green)"
  arrange_run 6 "$(spec_line audio green)"
  ev_set ci 5
  ev_set audio 6
  run_tag "$VER" --run-id 5 --run-id audio=6
  expect_both_tagged_full "R4.3: a bare <N> means ci=<N>; forms mix"
  assert_eq "$(gh_count '--workflow ci.yml ')" 0 "ci listings"
  assert_eq "$(gh_count '--workflow ci-audio-tests.yml ')" 0 "audio listings"
  assert_eq "$(gh_count '--workflow ci-android-emu.yml ')" 1 "android-emu listings"
  assert_eq "$(gh_count '--workflow ci-ios-sim.yml ')" 1 "ios-sim listings"
}
case_run_id_order_independent() {
  arrange_green
  arrange_run 44 "$(spec_line ios-sim green)"
  arrange_run 11 "$(spec_line ci green)"
  ev_set ios-sim 44
  ev_set ci 11
  run_tag "$VER" --run-id ios-sim=44 --run-id ci=11
  # R4.3: the report is one line per requirement "in table order", whatever the order of the arguments
  expect_both_tagged_full "R4.3: the report and the message are in table order"
}
case_run_id_not_qualifying_no_fallback() { # <key> <spec> <phrase>
  # World: arrange_green; run 777 of <key> is <spec>; the listing of <key> would offer a qualifying run
  local k="$1"
  arrange_green
  arrange_run 777 "$(spec_line "$k" "$2")"
  run_tag "$VER" --run-id "$k=777"
  expect_failure 16 "R4.3: with --run-id only that run is inspected for $k, no fallback to a listing"
  assert_eq "$(gh_count "--workflow $(key_file "$k") ")" 0 "listings of $k [R4.3]"
  assert_only_missing "R4.3" "$k" "$3"
  assert_hints "R4.3" "$k"
}
case_run_id_unknown_run() { # <key>
  # World: arrange_green; the fake gh knows no run 999 and exits non-zero
  local k="$1"
  arrange_green
  run_tag "$VER" --run-id "$k=999"
  expect_failure 16 "R4.3: gh run view failing for --run-id is missing for $k"
  assert_only_missing "R4.3" "$k" ""
  assert_hints "R4.3" "$k"
}
case_run_id_skips_listing() {
  arrange_green
  arrange_run 777 "$(spec_line ci green)"
  ev_set ci 777
  run_tag "$VER" --run-id 777
  expect_both_tagged_full "§4 check 7: --run-id N inspects only run N"
  assert_eq "$(gh_count "run list --repo $EXP_REPO --workflow ci.yml ")" 0 "ci listings [§4 check 7: listing only when --run-id is absent]"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "777 4343 4444 4545 " \
    "runs inspected [§4 check 7: only run N for ci; the other requirements as before]"
}

# ── R4.4: waivers ──────────────────────────────────────────────────────────
case_waive_honoured() { # <key> <flag value> <N>
  # World: <key>=none (no run exists at all), the others green; issue <N> = OPEN true; note: ok
  local k="$1"
  world_with "$k=none"
  honour_waiver "$k" "$3"
  run_tag "$VER" --waive "$2"
  expect_both_tagged_full "R4.4: --waive $2 with an open release-waiver issue is honoured"
  assert_waived_lines "R4.4: 'tag-release: WAIVED <key> (#<N>)'" "$k=$3"
  # R4.4: the verification call, exactly (the argument of the form `=#N` is `N` in the call)
  gh_logged "$(gh_issue_form "$3")" || fail "no call '$(gh_issue_form "$3")'; gh calls: $(log_text gh) [R4.4 verification call]"
  assert_eq "$(gh_count 'issue view ')" 1 "issue lookups [R4.4: one per waiver]"
}
case_waive_runs_not_queried() { # <key>
  # World: <key>=green (a qualifying run exists), the others green; issue 77 = OPEN true. The waiver means the runs are not queried.
  local k="$1"
  arrange_green
  honour_waiver "$k" 77
  run_tag "$VER" --waive "$k=77"
  expect_both_tagged_full "R4.4: a waived requirement is reported waived, not ok, although a run exists"
  assert_eq "$(gh_count "--workflow $(key_file "$k") ")" 0 "listings of the waived $k [R4.4: a waived requirement's runs are not queried]"
  assert_eq "$(gh_count "run view $(key_id "$k") ")" 0 "inspections of the waived $k's run [R4.4]"
  assert_eq "$(gh_count 'run list ')" 3 "listings of the other requirements [R4.3]"
  assert_tag_message remote "$CRATE_TAG" "$(tag_message "rsac $VER")" "R4.6: Waived: instead of Evidence: for $k"
}
case_waive_both_android_and_ios() { # <first flag> <second flag>
  # World: android-emu=none ios-sim=none (no runs), ci and audio green; issues 11 and 22 = OPEN true
  world ci=green audio=green android-emu=none ios-sim=none
  honour_waiver android-emu 11
  honour_waiver ios-sim 22
  run_tag "$VER" "--waive" "$1" "--waive" "$2"
  expect_both_tagged_full "R4.4/R4.6: both mobile requirements waived (arguments '$1' '$2'); Waived: lines in the order android-emu, ios-sim"
  assert_waived_lines "R4.4" android-emu=11 ios-sim=22
  assert_eq "$(gh_count 'issue view ')" 2 "issue lookups [R4.4: one per waiver]"
  gh_logged "$(gh_issue_form 11)" || fail "no verification call for issue 11 [R4.4]"
  gh_logged "$(gh_issue_form 22)" || fail "no verification call for issue 22 [R4.4]"
}
case_waive_all_three() {
  # World: ci=green audio=green android-emu=none ios-sim=none; note: missing (waived); issues 11, 22, 33 = OPEN true
  remove_note || return 0
  world ci=green audio=green android-emu=none ios-sim=none
  honour_waiver android-emu 11
  honour_waiver ios-sim 22
  honour_waiver windows-switch 33
  run_tag "$VER" --waive windows-switch=33 --waive ios-sim=#22 --waive android-emu=11
  expect_both_tagged_full "R4.4/R4.6: three waivers; Waived: lines in the order android-emu, ios-sim, windows-switch whatever the argument order"
  assert_waived_lines "R4.4" android-emu=11 ios-sim=22 windows-switch=33
  assert_tag_message remote "$CRATE_TAG" $'rsac 1.2.3\n\nEvidence: ci=4242\nEvidence: audio=4343\nWaived: android-emu=#11\nWaived: ios-sim=#22\nWaived: windows-switch=#33' \
    "R4.6: the crate tag message, spelled out"
}
case_waive_rejected() { # <key> <label> <clause>
  # World: arrange_green; issue 77 answers per <label> (a stdout other than exactly 'OPEN true', a failing command, or no such issue)
  local key="$1" want_rc=16
  [ "$key" != windows-switch ] || want_rc=20
  arrange_green
  case "$2" in
    open-false) issue_set 77 "OPEN false" ;;
    closed-true) issue_set 77 "CLOSED true" ;;
    closed-false) issue_set 77 "CLOSED false" ;;
    blank) issue_set 77 "" ;;
    silent) : > "$FAKE_DIR/gh-issue-77.out" ;;
    lowercase) issue_set 77 "open true" ;;
    extra-word) issue_set 77 "OPEN true extra" ;;
    state-only) issue_set 77 "OPEN" ;;
    reversed) issue_set 77 "true OPEN" ;;
    failing)
      issue_set 77 "OPEN true"
      set_rc gh-issue-77 1
      ;;
    absent) ;;
  esac
  run_tag "$VER" --waive "$key=77"
  expect_failure "$want_rc" "$3"
  assert_stderr_has_ci "waiver issue #77 is not an open release-waiver issue" "R4.4: the reason"
  # R5.7 (settles the AMBIGUOUS(R4.4) marker of the first version): a rejected waiver makes its key
  # missing even though a qualifying run (or a good note) exists; a waived key's runs are not queried.
  assert_only_missing "$3" "$key" "waiver issue #77 is not an open release-waiver issue"
  assert_no_waived_lines "R4.4: a rejected waiver is not honoured"
  gh_logged "$(gh_issue_form 77)" || fail "no verification call '$(gh_issue_form 77)' [R4.4]"
}
case_waive_rejected_ci_missing_too() { # <key>
  # World: ci=none audio=green android-emu=green ios-sim=green; issue 77 = OPEN false; note: ok (or missing for windows-switch)
  local k="$1"
  world ci=none audio=green android-emu=green ios-sim=green
  issue_set 77 "OPEN false"
  run_tag "$VER" --waive "$k=77"
  expect_failure 16 "R4.4/R4.5: a rejected waiver and a missing R4.3 requirement: exit 16 (20 wins only when no R4.3 requirement is missing)"
  assert_stderr_has_ci "waiver issue #77 is not an open release-waiver issue" "R4.4"
  assert_stderr_has_ci "no run found" "R4.3: ci is missing too"
}
case_waive_honoured_but_ci_missing() {
  # World: ci=none audio=green android-emu=none ios-sim=green; issue 77 = OPEN true (android-emu waived)
  world ci=none audio=green android-emu=none ios-sim=green
  honour_waiver android-emu 77
  run_tag "$VER" --waive android-emu=77
  expect_failure 16 "R4.3: ci is missing; the honoured android-emu waiver does not rescue it"
  assert_evidence_lines "R4.3/R4.4: all four evaluated; the waived one reported as such" \
    ci=missing:"no run found" audio=ok:4343 android-emu=waived:77 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  assert_hints "R4.3: a hint for ci only" ci
  # R5.7 (settles the AMBIGUOUS(R4.4) marker of the first version): the WAIVED lines belong to the moment
  # just before tagging (and to --dry-run); a run that exits 16 never prints one.
  assert_no_waived_lines "R4.4: nothing is tagged, so nothing is announced"
}
case_waive_dry_run() {
  # World: android-emu=none ios-sim=green ...; android-emu waived (issue 11), windows-switch waived (issue 33, note removed)
  remove_note || return 0
  world ci=green audio=green android-emu=none ios-sim=green
  honour_waiver android-emu 11
  honour_waiver windows-switch 33
  run_tag "$VER" --dry-run --waive android-emu=11 --waive windows-switch=33
  assert_rc 0 "R4.4 / §4 --dry-run"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4 --dry-run"
  assert_nothing_created "R4.6: --dry-run creates nothing"
  assert_evidence_state "R4.3/R4.4"
  assert_waived_lines "R4.4: the WAIVED lines are printed in --dry-run too" android-emu=11 windows-switch=33
  assert_dry_run_messages "R6.4 / R4.6: --dry-run prints the complete crate and Go messages"
}
case_waive_with_run_id() {
  # World: ios-sim=none (waived, issue 5); ci run 1001 given with --run-id; the others green
  world ci=green audio=green android-emu=green ios-sim=none
  arrange_run 1001 "$(spec_line ci green)"
  ev_set ci 1001
  honour_waiver ios-sim 5
  run_tag "$VER" --run-id ci=1001 --waive ios-sim=5
  expect_both_tagged_full "R4.3/R4.4: --run-id and --waive combine"
  assert_eq "$(gh_count '--workflow ci.yml ')" 0 "ci listings [R4.3: --run-id ci=1001]"
}

# ── R4.6: dry-run prints the message and leaves nothing ────────────────────
case_dry_run_leaves_no_tag() { # [extra args...]
  local no_go=0 a
  for a in "$@"; do [ "$a" = --no-go-tag ] && no_go=1; done
  arrange_green
  run_tag "$VER" --dry-run "$@"
  assert_rc 0 "§4 --dry-run"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4: last stdout line for --dry-run"
  assert_nothing_created "R4.6: --dry-run creates and pushes nothing"
  assert_tag_absent local "$CRATE_TAG" "R4.6 / §4: --dry-run creates no local tag"
  assert_tag_absent local "$GO_TAG" "R4.6 / §4: --dry-run creates no local tag"
  assert_tag_absent remote "$CRATE_TAG" "R4.6 / §4: --dry-run pushes no tag"
  assert_tag_absent remote "$GO_TAG" "R4.6 / §4: --dry-run pushes no tag"
  assert_eq "$(wc -l < "$SB/tags.after.local" | tr -d ' ')" 0 "local tags after --dry-run"
  assert_eq "$(wc -l < "$SB/tags.after.remote" | tr -d ' ')" 0 "remote tags after --dry-run"
  assert_evidence_state "R4.3: --dry-run reports the evidence"
  assert_repository_line "R4.1"
  if [ "$no_go" = 1 ]; then
    # R6.4: --no-go-tag prints no Go annotation block; the crate message is printed, whole lines.
    assert_message_printed "R6.4: with --no-go-tag the crate message is printed" "rsac $VER"
    assert_no_go_block_printed "R6.4: --no-go-tag prints no Go annotation block"
  else
    assert_dry_run_messages "R6.4 / R4.6: --dry-run prints the complete crate AND Go messages, whole lines"
  fi
  assert_stdout_unprefixed "§4: every message goes to stderr"
}

# ── §4 steps 8 and 9: create and push (exit 17) ────────────────────────────
case_success_both_tags() {
  touch_commit "tagged commit is the HEAD sha, not an abbreviation"
  push_master
  install_hook post-receive 'echo "--push--" >> "$(dirname "$0")/../hooklog"; cat >> "$(dirname "$0")/../hooklog"'
  arrange_green
  run_tag "$VER"
  expect_both_tagged_full "§4 steps 8-9: annotated tags on the full HEAD sha, pushed"
  assert_eq "${#SHA}" 40 "length of the full sha"
  assert_stdout_unprefixed "§4: every message goes to stderr"
  # §4 step 9: one `git push --atomic <remote> <tags...>` carries both tags
  assert_eq "$(grep -c -e '--push--' "$REMOTE/hooklog" 2>/dev/null)" 1 "number of pushes received by the remote [§4 step 9]"
  grep -q "refs/tags/$CRATE_TAG\$" "$REMOTE/hooklog" 2>/dev/null || fail "push did not carry $CRATE_TAG [§4 step 9]"
  grep -q "refs/tags/$GO_TAG\$" "$REMOTE/hooklog" 2>/dev/null || fail "push did not carry $GO_TAG [§4 step 9]"
}
case_success_no_go_tag() {
  arrange_green
  run_tag "$VER" --no-go-tag
  expect_crate_tagged_only_full "§4 --no-go-tag: only the crate tag is created and pushed"
  assert_tag_absent local "$GO_TAG" "§4 --no-go-tag: no Go tag is created"
  assert_tag_absent remote "$GO_TAG" "§4 --no-go-tag: no Go tag is pushed"
}
case_success_dry_run() {
  arrange_green
  run_tag "$VER" --dry-run
  assert_rc 0 "§4 --dry-run"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4: last stdout line for --dry-run"
  assert_nothing_created "§4 --dry-run creates and pushes nothing"
  assert_eq "$(wc -l < "$SB/tags.after.local" | tr -d ' ')" 0 "local tags after --dry-run [§4 --dry-run]"
  assert_eq "$(wc -l < "$SB/tags.after.remote" | tr -d ' ')" 0 "remote tags after --dry-run [§4 --dry-run]"
  # R6.4 / R4.6: --dry-run prints both complete messages (whole lines, in order; assert_dry_run_messages
  # checks the crate block and the Go block).
  assert_dry_run_messages "R6.4: --dry-run prints what it would tag: both messages"
  assert_stdout_unprefixed "§4: every message goes to stderr"
  # §4 --dry-run runs checks 0 to 7: the evidence was inspected (R4.3: one inspection per requirement)
  assert_eq "$(gh_count 'run view ')" 4 "gh run view calls [§4 --dry-run runs check 7; R4.3: four requirements]"
}
case_success_dry_run_no_go_tag() {
  arrange_green
  run_tag "$VER" --dry-run --no-go-tag
  assert_rc 0 "§4 --dry-run with --no-go-tag"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4: last stdout line for --dry-run"
  assert_nothing_created "§4 --dry-run creates and pushes nothing"
  assert_message_printed "R6.4: the crate message is printed, whole lines" "rsac $VER"
  assert_no_go_block_printed "R6.4: --no-go-tag prints no Go annotation block"
}
case_push_rejected_by_remote() {
  install_hook pre-receive 'cat >> "$(dirname "$0")/../hooklog"; echo "rejected by test hook" >&2; exit 1'
  make_tag local v0.0.1 # an unrelated tag must survive the clean-up
  arrange_green
  run_tag "$VER"
  expect_failure 17 "§4 step 9: a rejected push exits 17 and removes the local tags created in step 8"
  # The remote saw one push carrying both tags: proves step 8 created both and step 9 pushed them together.
  grep -q "refs/tags/$CRATE_TAG\$" "$REMOTE/hooklog" 2>/dev/null || fail "remote never received $CRATE_TAG [§4 steps 8-9]"
  grep -q "refs/tags/$GO_TAG\$" "$REMOTE/hooklog" 2>/dev/null || fail "remote never received $GO_TAG [§4 steps 8-9]"
  assert_tag_absent local "$CRATE_TAG" "§4 step 9: local tags removed after a failed push"
  assert_tag_absent local "$GO_TAG" "§4 step 9: local tags removed after a failed push"
  assert_tag_absent remote "$CRATE_TAG" "§4 step 9: nothing pushed"
}
case_push_unreachable_remote() { # [extra args...]
  # R4.1/R5.3: RELEASE_REPO is set and the push URL, a path that is not there, does not parse as a GitHub
  # URL — accepted only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1 (set by new_sandbox); the push then fails.
  must git -C "$REPO" config remote.origin.pushurl "$SB/does-not-exist.git" # fetch and ls-remote still work
  arrange_green
  run_tag "$VER" "$@"
  expect_failure 17 "§4 step 9: a failing push exits 17 and removes the local tags created in step 8"
  assert_tag_absent local "$CRATE_TAG" "§4 step 9: local tags removed after a failed push"
  assert_tag_absent local "$GO_TAG" "§4 step 9: local tags removed after a failed push"
}
case_push_is_atomic() {
  # The remote refuses only the Go tag. With `git push --atomic` the crate tag must not land either.
  install_hook update 'case "$1" in refs/tags/bindings/rsac-go/*) echo "rejected by test hook" >&2; exit 1;; esac; exit 0'
  arrange_green
  run_tag "$VER"
  expect_failure 17 "§4 step 9: git push --atomic: one refused ref fails the whole push"
  assert_tag_absent remote "$CRATE_TAG" "§4 step 9: --atomic keeps the crate tag off the remote"
  assert_tag_absent remote "$GO_TAG" "§4 step 9: --atomic"
  assert_tag_absent local "$CRATE_TAG" "§4 step 9: local tags removed after a failed push"
  assert_tag_absent local "$GO_TAG" "§4 step 9: local tags removed after a failed push"
}
case_push_no_follow_tags() { # <expected remote tags, sorted, space-terminated> [extra args...]
  local want="$1"
  shift
  touch_commit "second commit"
  push_master
  must git -C "$REPO" tag -a v0.0.1 -m "old release" HEAD~1 # unrelated, annotated, local only, on an ancestor of HEAD
  must git -C "$REPO" tag -a v0.0.2 -m "same commit" HEAD   # unrelated, annotated, local only, on HEAD itself
  must git -C "$REPO" config push.followTags true            # would make a plain push carry both of them
  arrange_green
  run_tag "$VER" "$@"
  assert_rc 0 "R3.3: the push succeeds"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "R3.3"
  # R3.3: step 9 is `git push --atomic --no-follow-tags <remote> <tags...>`: the remote ends up with only the requested tags
  assert_eq "$(remote_tag_names)" "$want" "tags on the remote after the push [R3.3: --no-follow-tags]"
  assert_no_tag_lost "R3.3: the unrelated local tags are untouched"
}
case_tag_creation_fails() {
  # tag.gpgsign with a failing gpg makes `git tag -a` fail
  must git -C "$REPO" config tag.gpgsign true
  must git -C "$REPO" config gpg.program "$WORK/bin/failing-gpg"
  install_hook pre-receive 'echo push >> "$(dirname "$0")/../hooklog"'
  arrange_green
  run_tag "$VER"
  expect_failure 17 "§4 step 8: failing to create the tags exits 17"
  [ ! -f "$REMOTE/hooklog" ] || fail "a push was attempted although tag creation failed [§4 steps 8-9]"
}

# ── harness self-tests (no script under test) ──────────────────────────────
expect_assertion_fails() { # <function> <args...>: the assertion must record a failure
  local saved="$FAILS" got
  FAILS=""
  "$@"
  got="$FAILS"
  FAILS="$saved"
  [ -n "$got" ] || fail "harness: '$*' did not fail"
}
expect_assertion_passes() { # <function> <args...>: the assertion must record no failure
  local saved="$FAILS" got
  FAILS=""
  "$@"
  got="$FAILS"
  FAILS="$saved"
  [ -z "$got" ] || fail "harness: '$*' failed: $got"
}
case_harness_fake_gh_demands_repo() {
  # R3.1 / §7: the fake gh must reject a call without --repo, with --repo in the wrong place, or with another repository,
  # reject a workflow outside the four of R4.3 and every other form, and accept the contract forms with the right one.
  local d="$WORK/harness-fake" k
  rm -rf "$d"
  mkdir -p "$d"
  FAKE_DIR="$d"
  FAKE_EXPECT_REPO="$TEST_REPO"
  export FAKE_DIR FAKE_EXPECT_REPO
  for k in $KEYS; do printf '4242\n' > "$d/gh-list-$k.out"; done
  printf 'line\n' > "$d/gh-view-7.out"
  printf 'OPEN true\n' > "$d/gh-issue-7.out"
  gh_status() { "$GH_BIN" "$@" > /dev/null 2>&1; echo $?; }
  for k in $KEYS; do
    assert_eq "$(gh_status run list --repo "$TEST_REPO" --workflow "$(key_file "$k")")" 0 "fake gh accepts 'run list --repo R' for $(key_file "$k")"
  done
  assert_eq "$(gh_status run list --workflow ci.yml)" 97 "fake gh rejects 'run list' without --repo"
  assert_eq "$(gh_status run list --workflow ci.yml --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'run list'"
  assert_eq "$(gh_status run list --repo other/repo --workflow ci.yml)" 97 "fake gh rejects another repository (list)"
  assert_eq "$(gh_status run list --repo "$TEST_REPO" --workflow release.yml)" 97 "fake gh rejects a workflow that is not one of the four"
  assert_eq "$(gh_status run view 7 --repo "$TEST_REPO" --json x)" 0 "fake gh accepts 'run view <id> --repo R'"
  assert_eq "$(gh_status run view 7 --json x)" 97 "fake gh rejects 'run view' without --repo"
  assert_eq "$(gh_status run view 7 --json x --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'run view <id>'"
  assert_eq "$(gh_status run view 7 --repo other/repo --json x)" 97 "fake gh rejects another repository (view)"
  assert_eq "$(gh_status run view 8 --repo "$TEST_REPO" --json x)" 1 "fake gh answers exit 1 for a run it does not know"
  assert_eq "$(gh_status issue view 7 --repo "$TEST_REPO" --json state,labels)" 0 "fake gh accepts 'issue view <N> --repo R'"
  assert_eq "$(gh_status issue view 7 --json state,labels)" 97 "fake gh rejects 'issue view' without --repo"
  assert_eq "$(gh_status issue view 7 --json state,labels --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'issue view <N>'"
  assert_eq "$(gh_status issue view 7 --repo other/repo --json state,labels)" 97 "fake gh rejects another repository (issue)"
  assert_eq "$(gh_status issue view 8 --repo "$TEST_REPO" --json state,labels)" 1 "fake gh answers exit 1 for an issue it does not know"
  assert_eq "$(gh_status release list --repo "$TEST_REPO")" 97 "fake gh rejects every other form"
  assert_eq "$(wc -l < "$d/gh.unexpected" | tr -d ' ')" 11 "rejected calls recorded in gh.unexpected"
}
case_harness_remote_helper() {
  # A remote with a GitHub-form URL and remote.<name>.vcs=rsacproxy reports that URL and still
  # fetches and pushes through the local bare repository (no network), also with a push URL and pushInsteadOf in play.
  new_sandbox
  use_remote_url origin "git@github.com:o/n.git" "$REMOTE" || return 0
  assert_eq "$(git -C "$REPO" remote get-url origin)" "git@github.com:o/n.git" "git remote get-url reports the GitHub form"
  must git -C "$REPO" fetch --no-tags origin master
  must git -C "$REPO" tag -a harness-probe -m probe
  must git -C "$REPO" push origin refs/tags/harness-probe
  assert_eq "$(git -C "$REMOTE" for-each-ref --format='%(refname:short)' refs/tags)" harness-probe "tag pushed through the helper"
  must git -C "$REPO" config remote.origin.pushurl "https://github.com/o/fork.git"
  must git -C "$REPO" tag -a harness-probe-2 -m probe
  must git -C "$REPO" push origin refs/tags/harness-probe-2
  assert_eq "$(git -C "$REPO" remote get-url --push --all origin)" "https://github.com/o/fork.git" "git remote get-url --push --all reports the push URL"
  assert_eq "$(git -C "$REMOTE" for-each-ref --format='%(refname:short)' refs/tags | tr '\n' ' ')" "harness-probe harness-probe-2 " "a push to a GitHub-form push URL still lands in the bare repository"
}
case_harness_unmatched_filter() {
  # R3.8: a filter that selects no case prints a line naming it and the file exits non-zero.
  local out rc
  out="$(RSAC_TEST_FILTER=zz-no-such-case-zz bash "$SELF" 2>&1)"
  rc=$?
  [ "$rc" != 0 ] || fail "exit 0 although the filter selected no case [R3.8]"
  case "$out" in *zz-no-such-case-zz*) ;; *) fail "no output line names the unmatched filter; output was: $out [R3.8]" ;; esac
}
case_harness_world_and_state() {
  new_sandbox
  world ci=green audio=none android-emu=failed ios-sim=list-fails
  assert_eq "$(cat "$FAKE_DIR/gh-list-ci.out")" 4242 "ci listing"
  assert_eq "$(cat "$FAKE_DIR/gh-view-4242.out")" "$SHA completed success workflow_dispatch success CI" "ci run line"
  assert_eq "$(wc -c < "$FAKE_DIR/gh-list-audio.out" | tr -d ' ')" 0 "audio lists no run"
  assert_eq "$(cat "$FAKE_DIR/gh-view-4444.out")" "$SHA completed failure workflow_dispatch success CI Android Emulator" "android-emu run line"
  assert_eq "$(cat "$FAKE_DIR/gh-list-ios-sim.rc")" 1 "ios-sim listing fails"
  assert_eq "$(ev_get ci)" 4242 "ci is expected ok"
  assert_eq "$(ev_get audio)" "" "audio is not expected ok"
  expect_assertion_fails world audio=green ci=green android-emu=green ios-sim=green
  expect_assertion_fails world_key ci no-such-spec
  assert_eq "$(spec_phrase ios-sim job-missing)" "no iOS Release Evidence job" "job name substituted in the reason"
  assert_eq "$(spec_phrase audio other-workflow)" "not the ci-audio-tests.yml workflow" "file substituted in the reason"
  reset_expectations
  assert_eq "$(tag_message "rsac 1.2.3")" $'rsac 1.2.3\n\nEvidence: ci=4242\nEvidence: audio=4343\nEvidence: android-emu=4444\nEvidence: ios-sim=4545\nEvidence: windows-switch='"$TESTED_SHA" \
    "tag_message of the default expectations"
  honour_waiver android-emu 7
  honour_waiver windows-switch 9
  assert_eq "$(tag_message "rsac-go 1.2.3")" $'rsac-go 1.2.3\n\nEvidence: ci=4242\nEvidence: audio=4343\nEvidence: ios-sim=4545\nWaived: android-emu=#7\nWaived: windows-switch=#9' \
    "tag_message with waivers"
}
case_harness_assertions() {
  new_sandbox
  reset_expectations
  cat > "$SB/stderr" <<EOF
tag-release: repository $TEST_REPO (remote origin)
tag-release: evidence ci ok run 4242
tag-release: evidence audio missing: no run found
tag-release: evidence android-emu waived #7
tag-release: evidence ios-sim ok run 4545
tag-release: evidence windows-switch ok $TESTED_SHA
tag-release: hint: gh workflow run ci-audio-tests.yml --repo $TEST_REPO --ref master -f platform=all -f release_evidence=true
tag-release: WAIVED android-emu (#7)
EOF
  expect_assertion_passes assert_evidence_lines "c" ci=ok:4242 audio=missing:"no run" android-emu=waived:7 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  expect_assertion_fails assert_evidence_lines "c" ci=ok:4243 audio=missing:"no run" android-emu=waived:7 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  expect_assertion_fails assert_evidence_lines "c" ci=ok:4242 audio=missing:"wrong commit" android-emu=waived:7 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  expect_assertion_fails assert_evidence_lines "c" ci=ok:4242 audio=ok:4343 android-emu=waived:7 ios-sim=ok:4545 "windows-switch=ok:$TESTED_SHA"
  expect_assertion_fails assert_evidence_lines "c" ci=ok:4242 audio=missing: android-emu=waived:7 ios-sim=ok:4545
  expect_assertion_passes assert_repository_line "c"
  expect_assertion_passes assert_hints "c" audio
  expect_assertion_fails assert_hints "c" ci
  expect_assertion_fails assert_hints "c" audio ci
  expect_assertion_fails assert_hints "c"
  expect_assertion_passes assert_waived_lines "c" android-emu=7
  expect_assertion_fails assert_waived_lines "c" android-emu=8
  expect_assertion_fails assert_waived_lines "c"
  expect_assertion_fails assert_no_waived_lines "c"
  expect_assertion_passes assert_stderr_after_evidence "gh workflow run" "c"
  expect_assertion_fails assert_stderr_after_evidence "repository" "c"
  # R6.4: whole-line comparison — an `Evidence: ci=4242` expectation must not accept `Evidence: ci=42420`.
  cat > "$SB/stderr" <<EOF
rsac $VER

Evidence: ci=42420
Evidence: audio=4343
Evidence: android-emu=4444
Evidence: ios-sim=4545
Evidence: windows-switch=$TESTED_SHA
EOF
  expect_assertion_fails assert_message_printed "c" "rsac $VER"
  cat > "$SB/stderr" <<EOF
rsac $VER

Evidence: ci=4242
Evidence: audio=4343
Evidence: android-emu=4444
Evidence: ios-sim=4545
Evidence: windows-switch=$TESTED_SHA
EOF
  expect_assertion_passes assert_message_printed "c" "rsac $VER"
  expect_assertion_passes assert_no_go_block_printed "c"
  cat >> "$SB/stderr" <<EOF
rsac-go $VER

Evidence: ci=4242
EOF
  expect_assertion_fails assert_no_go_block_printed "c"

  EXP_REPO="other/repo"
  expect_assertion_fails assert_repository_line "c"
}

# ═══════════════════════════════════════════════════════════════════════════
# Case list
# ═══════════════════════════════════════════════════════════════════════════
C0="§4 check 0: arguments"
run_case args-no-version case_args_bad "$C0: exactly one version is required"
run_case args-empty-version case_args_bad "$C0: an empty version does not match ^[0-9]+.[0-9]+.[0-9]+\$" ""
run_case args-version-two-parts case_args_bad "$C0: 1.2 does not match" "1.2"
run_case args-version-v-prefix case_args_bad "$C0: v1.2.3 does not match" "v$VER"
run_case args-version-prerelease case_args_bad "$C0: 1.2.3-rc.1 does not match" "$VER-rc.1"
run_case args-version-four-parts case_args_bad "$C0: 1.2.3.4 does not match" "$VER.4"
run_case args-version-non-numeric case_args_bad "$C0: a.b.c does not match" "a.b.c"
run_case args-two-versions case_args_bad "$C0: exactly one version" "$VER" "1.2.4"
run_case args-unknown-flag case_args_bad "$C0: unknown flag" "$VER" --bogus
run_case args-remote-missing-value case_args_bad "$C0: --remote needs a value" "$VER" --remote
run_case args-branch-missing-value case_args_bad "$C0: --branch needs a value" "$VER" --branch
run_case args-run-id-missing-value case_args_bad "$C0: --run-id needs a value" "$VER" --run-id
run_case args-waive-missing-value case_args_bad "$C0 / R4.4: --waive needs a value" "$VER" --waive
run_case args-error-precedes-dirty-tree case_args_error_precedes_dirty_tree "1.2"

# R4.3: --run-id takes <key>=<N> (key in the table: ci, audio, android-emu, ios-sim) or a bare <N>
RI="R4.3: --run-id"
run_case args-run-id-unknown-key case_args_bad "$RI: a key outside the table is exit 2" "$VER" --run-id bogus=5
run_case args-run-id-windows-switch-key case_args_bad "$RI: windows-switch is not a row of the R4.3 table" "$VER" --run-id windows-switch=5
run_case args-run-id-empty-key case_args_bad "$RI: an empty key is outside the table" "$VER" --run-id =5
run_case args-run-id-repeated-ci case_args_bad "$RI: a repeated key is exit 2" "$VER" --run-id ci=1 --run-id ci=2
run_case args-run-id-bare-then-ci-repeats case_args_bad "$RI: a bare <N> is ci=<N>, so ci=6 repeats the key" "$VER" --run-id 5 --run-id ci=6
run_case args-run-id-ci-then-bare-repeats case_args_bad "$RI: ci=6 then a bare <N> repeats the key" "$VER" --run-id ci=6 --run-id 5
run_case args-run-id-two-bare-repeat case_args_bad "$RI: two bare ids both mean ci" "$VER" --run-id 5 --run-id 6
run_case args-run-id-repeated-audio case_args_bad "$RI: a repeated key is exit 2 (audio)" "$VER" --run-id audio=1 --run-id audio=2
run_case args-run-id-repeated-android-emu case_args_bad "$RI: a repeated key is exit 2 (android-emu)" "$VER" --run-id android-emu=1 --run-id android-emu=2
run_case args-run-id-repeated-ios-sim case_args_bad "$RI: a repeated key is exit 2 (ios-sim)" "$VER" --run-id ios-sim=1 --run-id ios-sim=2
run_case args-run-id-non-numeric case_args_bad "$RI: a non-numeric id is exit 2" "$VER" --run-id abc
run_case args-run-id-key-non-numeric case_args_bad "$RI: ci=abc is a non-numeric id" "$VER" --run-id ci=abc
run_case args-run-id-empty-number case_args_bad "$RI: ci= has no id" "$VER" --run-id ci=
run_case args-run-id-negative case_args_bad "$RI: a negative id is not numeric" "$VER" --run-id ci=-5
run_case args-run-id-hash-form case_args_bad "$RI: the '#' form belongs to --waive; a run id is not numeric with it" "$VER" --run-id 'ci=#5'
run_case args-run-id-double-equals case_args_bad "$RI: ci=5=6 is a non-numeric id" "$VER" --run-id ci=5=6
run_case args-run-id-valid-then-invalid case_args_bad "$RI: one invalid among valid items fails the whole call" "$VER" --run-id audio=5 --run-id bogus=6
# R5.2: a run id and a waiver for the same key is a usage error, in both argument orders
run_case args-run-id-then-waive-same-key case_args_bad "R5.2: --run-id <key>=<N> with --waive <key>=<M>: exit 2 (run id first)" "$VER" --run-id android-emu=5 --waive android-emu=7
run_case args-waive-then-run-id-same-key case_args_bad "R5.2: exit 2 in the other argument order (waiver first)" "$VER" --waive android-emu=7 --run-id android-emu=5
run_case args-waive-then-run-id-ios-sim case_args_bad "R5.2: exit 2 (ios-sim)" "$VER" --waive ios-sim=7 --run-id ios-sim=5

# R4.4: --waive <key>=<N> | <key>=#<N>; keys android-emu, ios-sim, windows-switch
WV="R4.4: --waive"
run_case args-waive-ci case_args_bad "$WV: ci is not waivable" "$VER" --waive ci=5
run_case args-waive-audio case_args_bad "$WV: audio is not waivable" "$VER" --waive audio=5
run_case args-waive-unknown-key case_args_bad "$WV: an unknown key is exit 2" "$VER" --waive bogus=5
run_case args-waive-repeated-android-emu case_args_bad "$WV: a repeated key is exit 2" "$VER" --waive android-emu=1 --waive android-emu=2
run_case args-waive-repeated-same-issue case_args_bad "$WV: a repeated key is exit 2 even for the same issue" "$VER" --waive ios-sim=1 --waive ios-sim=1
run_case args-waive-repeated-hash-form case_args_bad "$WV: a repeated key is exit 2 whatever the form" "$VER" --waive android-emu=1 --waive 'android-emu=#1'
run_case args-waive-repeated-windows-switch case_args_bad "$WV: a repeated key is exit 2 (windows-switch)" "$VER" --waive windows-switch=1 --waive windows-switch=2
run_case args-waive-non-numeric case_args_bad "$WV: a non-numeric issue is exit 2" "$VER" --waive android-emu=abc
run_case args-waive-hash-non-numeric case_args_bad "$WV: =#abc is a non-numeric issue" "$VER" --waive 'android-emu=#abc'
run_case args-waive-hash-only case_args_bad "$WV: =# has no issue number" "$VER" --waive 'android-emu=#'
run_case args-waive-double-hash case_args_bad "$WV: =##5 is not numeric" "$VER" --waive 'ios-sim=##5'
run_case args-waive-no-equals case_args_bad "$WV: a key without an issue" "$VER" --waive android-emu
run_case args-waive-empty-number case_args_bad "$WV: android-emu= has no issue" "$VER" --waive android-emu=
run_case args-waive-negative case_args_bad "$WV: a negative issue is not numeric" "$VER" --waive android-emu=-5
run_case args-waive-trailing-garbage case_args_bad "$WV: 12x is not numeric" "$VER" --waive ios-sim=12x
run_case args-waive-windows-switch-non-numeric case_args_bad "$WV: windows-switch=abc is a non-numeric issue" "$VER" --waive windows-switch=abc
run_case args-waive-valid-then-invalid case_args_bad "$WV: one invalid among valid items fails the whole call" "$VER" --waive android-emu=5 --waive ci=6
run_case args-waive-error-precedes-dirty-tree case_args_error_precedes_dirty_tree "$VER" --waive ci=5
run_case args-run-id-error-precedes-dirty-tree case_args_error_precedes_dirty_tree "$VER" --run-id bogus=5

run_case tree-dirty-unstaged case_dirty_unstaged
run_case tree-dirty-staged case_dirty_staged
run_case tree-untracked-file-allowed case_untracked_file_allowed
run_case tree-dirty-precedes-fetch case_dirty_precedes_fetch

run_case fetch-fails-unknown-remote case_fetch_fails_unknown_remote
run_case fetch-fails-unknown-branch case_fetch_fails_unknown_branch
run_case head-ahead-of-remote case_head_ahead_of_remote
run_case head-diverged-from-remote case_head_diverged_from_remote
run_case head-behind-remote-allowed case_head_behind_remote_allowed
run_case fetch-refreshes-stale-tracking-ref case_stale_tracking_ref_refreshed
run_case remote-and-branch-flags case_remote_and_branch_flags
run_case head-shadowed-by-local-branch case_head_shadowed_by_local_branch
run_case head-shadowed-by-local-tag case_head_shadowed_by_local_tag
run_case no-fetch-refspec-remote-rewritten case_no_fetch_refspec_remote_rewritten
run_case no-fetch-refspec-remote-contains-head case_no_fetch_refspec_remote_contains_head

# R4.2: the fetch imports no tag
run_case fetch-imports-no-tags case_fetch_imports_no_tags
run_case fetch-imports-no-tags-dry-run case_fetch_imports_no_tags_dry_run
run_case fetch-imports-no-tags-on-failure case_fetch_imports_no_tags_on_failure
run_case fetch-keeps-diverged-local-tag case_fetch_keeps_diverged_local_tag

# R3.1
run_case repo-unset-local-path-remote case_repo_unset_local_remote unset
run_case repo-empty-local-path-remote case_repo_unset_local_remote empty
run_case repo-unset-precedes-tags-check case_repo_unset_precedes_tags_check
run_case repo-url-https case_repo_from_url "https://github.com/o/n" "o/n"
run_case repo-url-https-dot-git case_repo_from_url "https://github.com/o/n.git" "o/n"
run_case repo-url-https-trailing-slash case_repo_from_url "https://github.com/o/n/" "o/n"
run_case repo-url-https-dot-git-trailing-slash case_repo_from_url "https://github.com/o/n.git/" "o/n"
run_case repo-url-scp-dot-git case_repo_from_url "git@github.com:o/n.git" "o/n"
run_case repo-url-scp case_repo_from_url "git@github.com:o/n" "o/n"
run_case repo-url-scp-trailing-slash case_repo_from_url "git@github.com:o/n/" "o/n"
run_case repo-url-ssh-dot-git case_repo_from_url "ssh://git@github.com/o/n.git" "o/n"
run_case repo-url-ssh case_repo_from_url "ssh://git@github.com/o/n" "o/n"
run_case repo-url-ssh-trailing-slash case_repo_from_url "ssh://git@github.com/o/n/" "o/n"
run_case repo-url-dotted-name case_repo_from_url "https://github.com/o/n.js.git" "o/n.js"
run_case repo-url-hyphenated-names case_repo_from_url \
  "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture.git" "Codeseys-Labs/rust-crossplat-audio-capture"
run_case repo-url-other-host-rejected case_repo_url_rejected "https://gitlab.com/o/n.git"
run_case repo-url-lookalike-host-rejected case_repo_url_rejected "https://github.com.evil.example/o/n"
run_case repo-url-http-rejected case_repo_url_rejected "http://github.com/o/n"
run_case repo-url-missing-name-rejected case_repo_url_rejected "https://github.com/o"
run_case repo-url-extra-path-rejected case_repo_url_rejected "https://github.com/o/n/extra"
run_case repo-url-ssh-without-git-user-rejected case_repo_url_rejected "ssh://github.com/o/n.git"
run_case repo-url-scp-other-host-rejected case_repo_url_rejected "git@gitlab.com:o/n.git"
run_case repo-env-fetch-names-other-repository case_repo_env_overrides_url
run_case repo-env-matching-fetch-url-passes case_repo_env_matching_fetch_url
run_case repo-fetch-url-unparsed-seam-passes case_fetch_url_unparsed_with_repo seam
run_case repo-fetch-url-unparsed-no-seam-fails case_fetch_url_unparsed_with_repo no-seam
run_case repo-empty-env-uses-url case_repo_empty_env_uses_url
run_case repo-from-chosen-remote case_repo_from_chosen_remote

# R4.1: RELEASE_REPO validation
run_case release-repo-valid-all-allowed-characters case_release_repo_valid "A_b.c-d/E.f_g-h"
run_case release-repo-valid-digits-and-single-characters case_release_repo_valid "0/9"
run_case release-repo-invalid-no-slash case_release_repo_invalid "owner"
run_case release-repo-invalid-two-slashes case_release_repo_invalid "o/n/x"
run_case release-repo-invalid-empty-owner case_release_repo_invalid "/n"
run_case release-repo-invalid-empty-name case_release_repo_invalid "o/"
run_case release-repo-invalid-space-in-owner case_release_repo_invalid "o w/n"
run_case release-repo-invalid-space-in-name case_release_repo_invalid "o/n x"
run_case release-repo-invalid-leading-space case_release_repo_invalid " o/n"
run_case release-repo-invalid-trailing-space case_release_repo_invalid "o/n "
run_case release-repo-invalid-semicolon case_release_repo_invalid "o/n;id"
run_case release-repo-invalid-command-substitution case_release_repo_invalid 'o/$(id)'
run_case release-repo-invalid-backtick case_release_repo_invalid 'o/`id`'
run_case release-repo-invalid-url-form case_release_repo_invalid "https://github.com/o/n"
run_case release-repo-invalid-at-sign case_release_repo_invalid "o/n@x"
run_case release-repo-invalid-colon case_release_repo_invalid "o:x/n"
run_case release-repo-invalid-backslash case_release_repo_invalid 'o\x/n'
run_case release-repo-invalid-non-ascii case_release_repo_invalid $'\xc3\xb6/n'
run_case release-repo-invalid-trailing-newline case_release_repo_invalid $'o/n\n'
run_case release-repo-invalid-embedded-newline case_release_repo_invalid $'o/n\nx/y'
run_case release-repo-invalid-no-url-fallback case_release_repo_invalid_no_url_fallback
run_case release-repo-invalid-precedes-tags-check case_release_repo_invalid_precedes_tags_check
run_case release-repo-invalid-after-tree-check case_release_repo_invalid_after_tree_check

# R4.1: push URLs (`git remote get-url --push --all <remote>`)
PU="R4.1 push URL"
run_case pushurl-same-repo-other-form case_push_url unset o/n "https://github.com/o/n.git" "git@github.com:o/n" ok "" \
  "$PU: the same repository in another accepted form passes"
run_case pushurl-same-repo-other-case case_push_url unset O/N "https://github.com/O/N.git" "git@github.com:o/n.git" ok "" \
  "$PU / R5.7: the repositories are compared case-insensitively; REPO is the fetch-side spelling (O/N)"
run_case pushurl-different-name case_push_url unset o/n "https://github.com/o/n.git" "https://github.com/o/fork.git" 11 \
  "push URL names a different repository" "$PU: another repository name (issue #117: push goes elsewhere than the fetch URL)"
run_case pushurl-different-owner case_push_url unset o/n "https://github.com/o/n.git" "https://github.com/fork/n.git" 11 \
  "push URL names a different repository" "$PU: another owner"
run_case pushurl-different-repository-ssh-form case_push_url unset o/n "https://github.com/o/n.git" "ssh://git@github.com/other/n" 11 \
  "push URL names a different repository" "$PU: another repository, ssh form"
run_case pushurl-other-host case_push_url unset o/n "https://github.com/o/n.git" "https://gitlab.com/o/n.git" 11 \
  "cannot determine the GitHub repository" "$PU: a push URL that does not parse (RELEASE_REPO unset)"
run_case pushurl-local-path case_push_url unset o/n "https://github.com/o/n.git" "@SB@/elsewhere.git" 11 \
  "cannot determine the GitHub repository" "$PU: a local path does not parse (RELEASE_REPO unset)"
run_case pushurl-parses-but-fetch-url-does-not case_push_url unset o/n "@REMOTE@" "https://github.com/o/n.git" 11 \
  "cannot determine the GitHub repository" "$PU: the fetch URL must parse as well (RELEASE_REPO unset)"
run_case release-repo-pushurl-names-repo-other-case case_push_url "$TEST_REPO" "$TEST_REPO" "https://github.com/$TEST_REPO.git" \
  "https://github.com/RSAC-TEST-OWNER/RSAC-TEST-NAME.git" ok "" "$PU / R6.2: RELEASE_REPO set, a parsing push URL names REPO case-insensitively (the fetch URL names REPO too, as R6.2 requires)"
run_case release-repo-pushurl-other-repository case_push_url "$TEST_REPO" "$TEST_REPO" "https://github.com/$TEST_REPO.git" \
  "https://github.com/o/n.git" 11 "push URL names a different repository" "$PU / R5.3: the seam variable is set (new_sandbox) and a parsing push URL naming another repository still exits 11"
run_case release-repo-pushurl-unparseable-host-accepted case_push_url "$TEST_REPO" "$TEST_REPO" "https://github.com/$TEST_REPO.git" \
  "https://gitlab.com/o/n.git" ok "" "$PU / R5.3: RELEASE_REPO set: a push URL that does not parse is accepted only under RELEASE_ALLOW_UNPARSED_PUSH_URL=1 (set by new_sandbox)"
run_case release-repo-unparsed-push-url-no-seam case_release_repo_unparsed_push_url_no_seam
run_case release-repo-unparsed-push-url-seam-passes case_release_repo_unparsed_push_url_seam
run_case release-repo-pushurl-second-repository case_pushurl_second_repository
run_case pushurl-two-pushurls-release-repo-unset case_push_url_count unset two-pushurls 11
run_case pushurl-two-pushurls-release-repo-set case_push_url_count "$TEST_REPO" two-pushurls 11
run_case pushurl-two-url-values-release-repo-unset case_push_url_count unset two-urls 11
run_case pushurl-two-url-values-release-repo-set case_push_url_count "$TEST_REPO" two-urls 11
run_case pushurl-one-pushurl-with-two-urls-passes case_push_url_count unset one-pushurl-two-urls ok
run_case pushinsteadof-different-repository case_push_insteadof unset "https://github.com/evil/" 11 \
  "push URL names a different repository" "$PU: pushInsteadOf rewrites the push URL to another repository (RELEASE_REPO unset)"
run_case pushinsteadof-different-repository-release-repo-set case_push_insteadof "$TEST_REPO" "https://github.com/evil/" 11 \
  "push URL names a different repository" "$PU: pushInsteadOf rewrites the push URL to another repository (RELEASE_REPO set)"
run_case pushinsteadof-local-path case_push_insteadof unset "@SB@/elsewhere/" 11 \
  "cannot determine the GitHub repository" "$PU: pushInsteadOf rewrites the push URL to a path (RELEASE_REPO unset)"
run_case pushinsteadof-same-repository-other-form case_push_insteadof unset "git@github.com:@OWNER@/" ok "" \
  "$PU: pushInsteadOf rewrites https to ssh, same repository"

# R3.2, R4.3: the run's workflow name (sixth field, the rest of the line) must equal the `name:` of the workflow file
N="R3.2 / R4.3"
run_case ci-name-matching-qualifies case_wf_name_line ci "name: CI" "CI" qualifies "$N: equal names qualify"
run_case ci-name-mismatch case_wf_name_line ci "name: CI" "Other" fail \
  "$N: a different workflow name fails the requirement" "not the @FILE@ workflow"
run_case ci-name-mismatch-with-spaces case_wf_name_line ci "name: CI" "Some Other Workflow" fail \
  "$N: a different workflow name, with spaces, fails the requirement" "not the @FILE@ workflow"
run_case ci-name-is-compared-exactly case_wf_name_line ci "name: CI" "ci" fail \
  "$N: 'equals' is exact, so ci is not CI" "not the @FILE@ workflow"
run_case ci-name-prefix-is-not-equal case_wf_name_line ci "name: CI" "CI Extra" fail \
  "$N: the rest of the line is the name, so 'CI Extra' is not CI" "not the @FILE@ workflow"
run_case ci-name-with-spaces-qualifies case_wf_name_line ci "name: Release Gate CI" "Release Gate CI" qualifies \
  "$N: a name containing spaces, in the file and in the line, qualifies"
run_case ci-name-quoted-double-qualifies case_wf_name_line ci 'name: "CI"' "CI" qualifies \
  "$N: surrounding double quotes of the file's name: are removed"
run_case ci-name-quoted-single-qualifies case_wf_name_line ci "name: 'CI'" "CI" qualifies \
  "$N: surrounding single quotes of the file's name: are removed"
run_case ci-name-quoted-with-spaces-qualifies case_wf_name_line ci 'name: "Release Gate CI"' "Release Gate CI" qualifies \
  "$N: a quoted name containing spaces matches the unquoted run name"
run_case ci-name-surrounding-whitespace-qualifies case_wf_name_line ci "name:    CI   " "CI" qualifies \
  "$N: surrounding whitespace of the file's name: is removed"
run_case ci-name-ignores-indented-and-commented-name-lines case_wf_name_file ci \
  $'# name: Commented Out\non:\n  workflow_dispatch:\njobs:\n  lint:\n    name: Lint\nname: CI' "CI" qualifies \
  "$N: only a line matching ^name: counts; comments and indented name: lines are skipped"
run_case ci-name-of-an-indented-line-is-not-the-workflow case_wf_name_file ci \
  $'# name: Commented Out\non:\n  workflow_dispatch:\njobs:\n  lint:\n    name: Lint\nname: CI' "Lint" fail \
  "$N: an indented name: line is not the workflow name" "not the @FILE@ workflow"
for k in audio android-emu ios-sim; do
  wfname="$(key_wfname "$k")"
  run_case "$k-name-matching-qualifies" case_wf_name_line "$k" "name: $wfname" "$wfname" qualifies "$N: equal names qualify ($k)"
  run_case "$k-name-mismatch" case_wf_name_line "$k" "name: $wfname" "CI" fail \
    "$N: the run of another workflow (the ci.yml name) does not satisfy $k" "not the @FILE@ workflow"
  run_case "$k-name-prefix-is-not-equal" case_wf_name_line "$k" "name: $wfname" "$wfname Extra" fail \
    "$N: 'equals' is exact ($k)" "not the @FILE@ workflow"
  run_case "$k-name-quoted-qualifies" case_wf_name_line "$k" "name: \"$wfname\"" "$wfname" qualifies \
    "$N: surrounding quotes of the file's name: are removed ($k)"
done
for k in $KEYS; do
  U="$N: a file without a usable name is 'cannot determine workflow name' ($k)"
  run_case "$k-name-unusable-no-name-line" case_wf_name_unusable "$k" no-name-line "$U: no ^name: line"
  run_case "$k-name-unusable-empty-name" case_wf_name_unusable "$k" empty-name "$U: an empty name"
  run_case "$k-name-unusable-quoted-empty-name" case_wf_name_unusable "$k" quoted-empty-name "$U: a name of two quote characters"
done
# R5.7 (settles the AMBIGUOUS(R4.3) marker of the first version): a workflow file that does not exist is
# "a file without a usable name", reason `cannot determine workflow name`.
run_case ci-name-unusable-file-missing case_wf_name_unusable ci file-missing "$N / R5.7: a missing workflow file is 'cannot determine workflow name'"
run_case audio-name-unusable-file-missing case_wf_name_unusable audio file-missing "$N / R5.7: a missing workflow file is 'cannot determine workflow name'"

# R6.1: the gate's own files come from a trusted directory (RELEASE_GATE_DIR)
run_case gate-dir-same-names-passes case_gate_dir_same_names
run_case gate-dir-empty-behaves-as-unset case_gate_dir_empty
run_case gate-dir-missing case_gate_dir_bad missing
run_case gate-dir-without-lockstep-script case_gate_dir_bad no-lockstep
run_case gate-dir-is-a-file case_gate_dir_bad a-file
for k in $KEYS; do
  run_case "gate-name-$k-run-named-like-gate-qualifies" case_gate_wf_name "$k" "Gate $(key_wfname "$k")" \
    "Commit $(key_wfname "$k")" "Gate $(key_wfname "$k")" qualifies \
    "R6.1: the run is judged by the GATE directory's name, not the commit's ($k)"
  run_case "gate-name-$k-run-named-like-commit-fails" case_gate_wf_name "$k" "Gate $(key_wfname "$k")" \
    "Commit $(key_wfname "$k")" "Commit $(key_wfname "$k")" fail \
    "R6.1: a run named like the COMMIT's workflow does not qualify ($k)" "not the @FILE@ workflow"
  run_case "gate-name-$k-gate-file-no-name-line" case_gate_wf_name "$k" NONE "Whatever $(key_wfname "$k")" \
    "Whatever" fail "R6.1: the gate directory's file has no usable name ($k)" "cannot determine workflow name"
  run_case "gate-name-$k-gate-file-missing" case_gate_wf_name "$k" MISSING "Whatever $(key_wfname "$k")" \
    "Whatever" fail "R6.1 / R5.7: the gate directory has no $(key_file "$k") at all ($k)" "cannot determine workflow name"
done
run_case gate-name-commit-file-unusable-passes case_gate_wf_name ci "Gate CI" "" "Gate CI" qualifies \
  "R6.1: the commit's own file (empty name here) does not matter; the gate directory's name decides"
run_case gate-lockstep-gate-fails-commit-passes-refused case_gate_lockstep 1 0 fail \
  "R6.1: the commit's own always-passing lockstep script does not save it: the GATE directory's script decides (exit 13)"
run_case gate-lockstep-commit-fails-gate-passes case_gate_lockstep 0 1 pass \
  "R6.1: the commit's failing lockstep script does not block: the GATE directory's script decides"
run_case gate-changelog-of-commit-decides case_gate_changelog_of_commit
run_case gate-note-of-commit-decides case_gate_note_of_commit

# §4 check 3, R4.2: where the tag was found
run_case tag-exists-local-crate case_tag_exists local "$CRATE_TAG" "§4 check 3 / R4.2: vX.Y.Z exists locally"
run_case tag-exists-local-go case_tag_exists local "$GO_TAG" "§4 check 3 / R4.2: bindings/rsac-go/vX.Y.Z exists locally"
run_case tag-exists-remote-crate case_tag_exists remote "$CRATE_TAG" "§4 check 3 / R4.2: vX.Y.Z exists on the remote only"
run_case tag-exists-remote-go case_tag_exists remote "$GO_TAG" "§4 check 3 / R4.2: bindings/rsac-go/vX.Y.Z exists on the remote only"
run_case tag-exists-crate-with-no-go-tag case_tag_exists local "$CRATE_TAG" "§4 check 3: --no-go-tag only exempts the Go tag" --no-go-tag
run_case tag-exists-remote-crate-with-no-go-tag case_tag_exists remote "$CRATE_TAG" "§4 check 3: --no-go-tag only exempts the Go tag (remote)" --no-go-tag
run_case tag-exists-local-and-remote-crate case_tag_exists_local_and_remote "$CRATE_TAG"
run_case tag-exists-local-and-remote-go case_tag_exists_local_and_remote "$GO_TAG"
run_case tag-exists-on-chosen-remote-crate case_tag_exists_on_chosen_remote "$CRATE_TAG"
run_case tag-exists-on-chosen-remote-go case_tag_exists_on_chosen_remote "$GO_TAG"
run_case no-go-tag-ignores-local-go-tag case_no_go_tag_ignores_go_tag local
run_case no-go-tag-ignores-remote-go-tag case_no_go_tag_ignores_go_tag remote
run_case similar-tags-local-do-not-block case_similar_tags_do_not_block local
run_case similar-tags-remote-do-not-block case_similar_tags_do_not_block remote
run_case tags-check-precedes-lockstep case_tags_before_lockstep
run_case fetch-check-precedes-tags case_fetch_before_tags

# R6.3 WITHDRAWN: the lookup stays on the remote name (R4.2); the cases record the behaviour under the
# R5.3 seam (a separate push destination): --dry-run passes, a real run is refused and exits 17.
run_case tag-at-pushurl-destination-crate case_tag_at_push_destination pushurl "$CRATE_TAG"
run_case tag-at-pushurl-destination-go case_tag_at_push_destination pushurl "$GO_TAG"
run_case tag-at-pushurl-destination-crate-dry-run case_tag_at_push_destination pushurl "$CRATE_TAG" --dry-run
run_case tag-on-fetch-side-found-exit-12 case_tag_on_fetch_side_found

# R3.4: the Go module path must carry /vN for a major of 2 or more
G="R3.4"
run_case go-major-2-without-suffix-fails case_go_major 2.0.0 "" fail "$G: major 2 needs a module path ending /v2"
run_case go-major-2-with-suffix-passes case_go_major 2.0.0 "/v2" pass "$G: a module path ending /v2 satisfies major 2"
run_case go-major-2-no-go-tag-skips-check case_go_major 2.0.0 "" crate-only "$G: --no-go-tag skips the check" --no-go-tag
run_case go-major-3-with-v2-suffix-fails case_go_major 3.1.4 "/v2" fail "$G: major 3 needs /v3, not /v2"
run_case go-major-3-with-suffix-passes case_go_major 3.0.0 "/v3" pass "$G: a module path ending /v3 satisfies major 3"
run_case go-major-10-with-v1-suffix-fails case_go_major 10.0.0 "/v1" fail "$G: major 10 needs /v10; /v1 is not a match"
run_case go-major-2-with-v12-suffix-fails case_go_major 2.0.0 "/v12" fail "$G: /v12 does not end with /v2"
run_case go-major-1-not-checked case_go_major 1.9.0 "" pass "$G: major 1 is not checked"
run_case go-major-0-not-checked case_go_major 0.5.0 "" pass "$G: major 0 is not checked"
run_case go-major-checked-by-dry-run case_dry_run_checks_go_module
run_case go-major-check-follows-tag-check case_go_major_after_tag_check
run_case go-major-check-precedes-lockstep case_go_major_before_lockstep

run_case lockstep-fails-exit-1 case_lockstep_fails 1
run_case lockstep-fails-exit-2 case_lockstep_fails 2
run_case lockstep-precedes-cargo case_lockstep_before_cargo
run_case cargo-metadata-fails case_cargo_fails
run_case cargo-precedes-changelog case_cargo_before_changelog

CL="§4 check 6: CHANGELOG.md line ^## \\[X\\.Y\\.Z\\] - [0-9]{4}-[0-9]{2}-[0-9]{2}\$"
run_case changelog-missing-file case_changelog_missing_file
run_case changelog-no-entry-for-version case_changelog_bad "$CL: no entry for the version" \
  $'# Changelog\n\n## [Unreleased]\n\n## [1.2.2] - 2025-12-01\n'
run_case changelog-entry-without-date case_changelog_bad "$CL: the date is required" \
  $'# Changelog\n\n## [1.2.3]\n'
run_case changelog-date-not-zero-padded case_changelog_bad "$CL: [0-9]{2} month and day" \
  $'# Changelog\n\n## [1.2.3] - 2026-1-2\n'
run_case changelog-trailing-text case_changelog_bad "$CL: the line ends after the date" \
  $'# Changelog\n\n## [1.2.3] - 2026-01-02 (yanked)\n'
run_case changelog-version-dots-literal case_changelog_bad "$CL: the dots of X.Y.Z are literal" \
  $'# Changelog\n\n## [1x2y3] - 2026-01-02\n'
run_case changelog-other-version-with-prefix case_changelog_bad "$CL: [1.2.30] is not [1.2.3]" \
  $'# Changelog\n\n## [1.2.30] - 2026-01-02\n'
run_case changelog-indented-heading case_changelog_bad "$CL: anchored at the line start" \
  $'# Changelog\n\n ## [1.2.3] - 2026-01-02\n'
run_case changelog-wrong-heading-level case_changelog_bad "$CL: exactly two #" \
  $'# Changelog\n\n### [1.2.3] - 2026-01-02\n'
run_case changelog-entry-anywhere-in-file case_changelog_entry_anywhere_ok
run_case changelog-precedes-ci-run case_changelog_before_ci
run_case changelog-missing-precedes-gh case_changelog_missing_before_gh_calls

# R4.5: check 6b, the workstation evidence note (exit 20)
W5="R4.5"
run_case windows-switch-note-missing case_ws_note_missing
run_case windows-switch-note-without-tested-sha-line case_ws_note_content \
  "$(printf '# note\n\nNo evidence line here.\n')" 20 "Tested-SHA" "$W5: no line matching ^Tested-SHA: [0-9a-f]{40}\$"
run_case windows-switch-note-two-tested-sha-lines case_ws_note_content \
  "$(printf '# note\n\nTested-SHA: %s\nTested-SHA: %s\n' "$TESTED_SHA" "$TESTED_SHA")" 20 "Tested-SHA" "$W5: more than one such line"
run_case windows-switch-note-two-different-tested-sha-lines case_ws_note_content \
  "$(printf '# note\n\nTested-SHA: %s\nTested-SHA: %s\n' "$TESTED_SHA" "$OTHER_SHA")" 20 "Tested-SHA" "$W5: more than one such line (different shas)"
run_case windows-switch-note-sha-uppercase case_ws_note_content \
  "$(printf 'Tested-SHA: %s\n' "$(printf '%s' "$TESTED_SHA" | tr 'a-f' 'A-F')")" 20 "Tested-SHA" "$W5: lower-case hex only"
run_case windows-switch-note-sha-39-chars case_ws_note_content \
  "$(printf 'Tested-SHA: %s\n' "${TESTED_SHA:0:39}")" 20 "Tested-SHA" "$W5: exactly 40 hex digits (39)"
run_case windows-switch-note-sha-41-chars case_ws_note_content \
  "$(printf 'Tested-SHA: %s0\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: exactly 40 hex digits (41)"
run_case windows-switch-note-sha-abbreviated case_ws_note_content \
  "$(printf 'Tested-SHA: %s\n' "${TESTED_SHA:0:7}")" 20 "Tested-SHA" "$W5: a full sha, not an abbreviation"
run_case windows-switch-note-sha-not-hex case_ws_note_content \
  "$(printf 'Tested-SHA: %sg\n' "${TESTED_SHA:0:39}")" 20 "Tested-SHA" "$W5: hex digits only"
run_case windows-switch-note-sha-empty case_ws_note_content \
  "$(printf 'Tested-SHA: \n')" 20 "Tested-SHA" "$W5: an empty value"
run_case windows-switch-note-trailing-text case_ws_note_content \
  "$(printf 'Tested-SHA: %s (ok)\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: the line ends after the sha"
run_case windows-switch-note-leading-space case_ws_note_content \
  "$(printf ' Tested-SHA: %s\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: anchored at the line start"
run_case windows-switch-note-no-space-after-colon case_ws_note_content \
  "$(printf 'Tested-SHA:%s\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: exactly one space after the colon (none)"
run_case windows-switch-note-two-spaces-after-colon case_ws_note_content \
  "$(printf 'Tested-SHA:  %s\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: exactly one space after the colon (two)"
run_case windows-switch-note-lowercase-key case_ws_note_content \
  "$(printf 'tested-sha: %s\n' "$TESTED_SHA")" 20 "Tested-SHA" "$W5: the key is spelled Tested-SHA"
run_case windows-switch-note-crlf-tolerated case_ws_note_content \
  "$(printf '# note\r\n\r\nTested-SHA: %s\r\n' "$TESTED_SHA")" ok "" "$W5: a trailing carriage return is tolerated"
run_case windows-switch-note-prose-mention-ok case_ws_note_content \
  "$(printf 'The key below is Tested-SHA: a note about it.\n\nTested-SHA: %s\n' "$TESTED_SHA")" ok "" \
  "$W5: only lines that match count; a mention in prose does not make a second line"
run_case windows-switch-note-no-trailing-newline-ok case_ws_note_content \
  "@NONL@$(printf 'Tested-SHA: %s' "$TESTED_SHA")" ok "" "$W5: the last line needs no newline"
run_case windows-switch-sha-unknown-commit case_ws_sha_not_ancestor unknown
run_case windows-switch-sha-on-side-branch case_ws_sha_not_ancestor side-branch
run_case windows-switch-sha-is-head case_ws_sha_is_head
run_case windows-switch-stale-note-fresh-untracked-copy case_ws_stale_note_fresh_untracked_copy
run_case windows-switch-stale-note-fresh-modified-copy case_ws_stale_note_fresh_modified_copy
run_case windows-switch-note-only-in-working-tree case_ws_note_only_in_working_tree
run_case windows-switch-committed-note-working-copy-deleted case_ws_committed_note_working_copy_deleted
for p in $WINDOWS_SWITCH_PATHS; do
  case "$p" in
    src/audio/windows) changed="src/audio/windows/mod.rs" ;;
    *) changed="$p" ;;
  esac
  run_case "windows-switch-path-changed-$(printf '%s' "$p" | tr '/_.' '---')" case_ws_path_changed "$changed"
done
run_case windows-switch-path-new-file-under-directory case_ws_path_changed src/audio/windows/capture.rs
run_case windows-switch-two-paths-changed case_ws_path_changed src/api/recovery.rs tests/ci_audio/hotplug_windows.rs
run_case windows-switch-path-deleted-file case_ws_path_deleted src/core/recovery.rs
run_case windows-switch-path-changed-then-reverted case_ws_path_changed_then_reverted
run_case windows-switch-tested-sha-after-the-change case_ws_tested_sha_after_path_change
run_case windows-switch-unrelated-readme-change case_ws_unrelated_change README.md
run_case windows-switch-unrelated-source-change case_ws_unrelated_change src/lib.rs
run_case windows-switch-unrelated-macos-change case_ws_unrelated_change src/audio/macos/mod.rs
run_case windows-switch-sibling-directory-with-shared-prefix case_ws_unrelated_change src/audio/windows-extra/mod.rs
run_case windows-switch-sibling-test-file case_ws_unrelated_change tests/ci_audio/hotplug_linux.rs
run_case windows-switch-check-follows-changelog-check case_ws_before_changelog_precedes
run_case windows-switch-and-ci-missing-exit-16-wins case_ws_and_ci_missing
run_case windows-switch-checked-by-dry-run case_ws_checked_by_dry_run
run_case windows-switch-waived-note-missing case_ws_waived note-missing
run_case windows-switch-waived-path-changed case_ws_waived path-changed
run_case windows-switch-waived-note-ok case_ws_waived note-ok

# R4.3: one requirement missing alone (every spec, ci in full; the other rows the core set), then several at once
E3="R4.3"
FULL_SPECS="none wrong-commit abbreviated-sha queued in-progress failed cancelled conclusion-none event-push event-pr job-missing job-failed job-skipped job-none other-workflow malformed-4 malformed-5 malformed-empty list-fails view-fails"
CORE_SPECS="none wrong-commit queued failed event-push job-missing job-failed other-workflow malformed-5 malformed-empty list-fails view-fails"
run_case evidence-all-ok case_evidence_all_ok
for k in $KEYS; do
  if [ "$k" = ci ]; then specs="$FULL_SPECS"; else specs="$CORE_SPECS"; fi
  for spec in $specs; do
    run_case "evidence-$k-$spec" case_evidence_one "$k" "$spec" "$E3: $k alone is '$spec', the other requirements are satisfied"
  done
done
run_case evidence-two-missing-ci-and-android-emu case_evidence_two_missing_report
run_case evidence-two-missing-ci-and-audio case_evidence_missing "$E3: ci and audio missing" ci=none audio=none
run_case evidence-two-missing-audio-and-ios-sim case_evidence_missing "$E3: audio and ios-sim missing" audio=queued ios-sim=job-missing
run_case evidence-two-missing-android-emu-and-ios-sim case_evidence_missing "$E3: android-emu and ios-sim missing" android-emu=none ios-sim=event-push
run_case evidence-two-missing-ci-and-ios-sim case_evidence_missing "$E3: ci and ios-sim missing" ci=failed ios-sim=other-workflow
run_case evidence-all-four-missing case_evidence_all_four_missing
run_case evidence-cli-failure-others-evaluated case_evidence_cli_failure_others_evaluated
run_case evidence-ci-first-view-cli-fails-aborts-key case_evidence_first_view_fails_second_would_qualify ci 101 102
run_case evidence-audio-first-view-cli-fails-aborts-key case_evidence_first_view_fails_second_would_qualify audio 201 202
run_case evidence-ci-later-listed-run-used case_evidence_later_run_used ci 101 102 103
run_case evidence-audio-later-listed-run-used case_evidence_later_run_used audio 201 202 203
run_case evidence-android-emu-later-listed-run-used case_evidence_later_run_used android-emu 301 302 303
run_case evidence-ios-sim-later-listed-run-used case_evidence_later_run_used ios-sim 401 402 403
run_case evidence-ci-no-listed-run-qualifies case_evidence_no_listed_run_qualifies
run_case evidence-run-20-qualifies case_evidence_run_20_qualifies ci
run_case evidence-run-21-ignored case_evidence_run_21_ignored ci
run_case evidence-audio-run-21-ignored case_evidence_run_21_ignored audio
run_case evidence-first-listed-reason case_evidence_first_listed_reason ci
run_case evidence-audio-first-listed-reason case_evidence_first_listed_reason audio
run_case evidence-no-run-on-custom-branch case_evidence_no_run_custom_branch
run_case evidence-failure-creates-no-tags case_evidence_failure_creates_no_tags
run_case evidence-missing-with-dry-run case_dry_run_fails_on_check7
run_case dirty-tree-with-dry-run case_dry_run_fails_on_dirty_tree

# R3.5, R4.3: the exit-16 hint
run_case hint-behind-tip-no-run case_hint_behind_tip master ci none
run_case hint-behind-tip-failed-run case_hint_behind_tip master ci failed
run_case hint-behind-tip-audio-missing case_hint_behind_tip master audio none
run_case hint-behind-tip-ios-sim-wrong-commit case_hint_behind_tip master ios-sim wrong-commit
run_case hint-behind-tip-custom-branch case_hint_behind_tip release ci none --branch release
run_case hint-behind-tip-names-reason case_hint_behind_tip_reason
run_case hint-at-tip-dispatch-commands case_hint_at_tip

# the call forms
run_case gh-call-forms case_gh_call_forms
run_case lockstep-and-cargo-call-forms case_check_tool_forms

# R4.3: --run-id
run_case run-id-bare-is-ci case_run_id_key 777 ci 777
run_case run-id-ci-key case_run_id_key ci=777 ci 777
run_case run-id-audio-key case_run_id_key audio=888 audio 888
run_case run-id-android-emu-key case_run_id_key android-emu=999 android-emu 999
run_case run-id-ios-sim-key case_run_id_key ios-sim=555 ios-sim 555
run_case run-id-all-four-no-listing case_run_id_all_four
run_case run-id-bare-and-key-mix case_run_id_bare_and_key
run_case run-id-argument-order-does-not-matter case_run_id_order_independent
run_case run-id-skips-listing case_run_id_skips_listing
run_case run-id-ci-not-qualifying-no-fallback case_run_id_not_qualifying_no_fallback ci wrong-commit "wrong commit"
run_case run-id-audio-not-qualifying-no-fallback case_run_id_not_qualifying_no_fallback audio wrong-commit "wrong commit"
run_case run-id-android-emu-job-failed-no-fallback case_run_id_not_qualifying_no_fallback android-emu job-failed "Android Release Evidence failed"
run_case run-id-ios-sim-other-workflow-no-fallback case_run_id_not_qualifying_no_fallback ios-sim other-workflow "not the ci-ios-sim.yml workflow"
for k in $KEYS; do
  run_case "run-id-$k-unknown-run" case_run_id_unknown_run "$k"
done

# R4.4: waivers
run_case waive-android-emu-honoured case_waive_honoured android-emu android-emu=77 77
run_case waive-ios-sim-honoured case_waive_honoured ios-sim ios-sim=88 88
run_case waive-hash-form-honoured case_waive_honoured android-emu 'android-emu=#77' 77
run_case waive-android-emu-runs-not-queried case_waive_runs_not_queried android-emu
run_case waive-ios-sim-runs-not-queried case_waive_runs_not_queried ios-sim
run_case waive-both-mobile-requirements case_waive_both_android_and_ios android-emu=11 ios-sim=22
run_case waive-both-mobile-requirements-reversed case_waive_both_android_and_ios ios-sim=22 android-emu=11
run_case waive-both-mobile-requirements-hash-forms case_waive_both_android_and_ios 'android-emu=#11' 'ios-sim=#22'
run_case waive-all-three case_waive_all_three
run_case waive-with-run-id case_waive_with_run_id
run_case waive-honoured-but-ci-missing case_waive_honoured_but_ci_missing
run_case waive-in-dry-run case_waive_dry_run
for k in android-emu ios-sim windows-switch; do
  if [ "$k" = android-emu ]; then labels="open-false closed-true closed-false blank silent lowercase extra-word state-only reversed failing absent"; else labels="open-false closed-true failing absent"; fi
  for label in $labels; do
    run_case "waive-rejected-$k-$label" case_waive_rejected "$k" "$label" "R4.4: the issue lookup answers '$label': not an open release-waiver issue"
  done
done
for k in android-emu ios-sim windows-switch; do
  run_case "waive-rejected-$k-with-ci-missing" case_waive_rejected_ci_missing_too "$k"
done

# R4.6: --dry-run prints the message and leaves no tag
run_case dry-run-leaves-no-tag case_dry_run_leaves_no_tag
run_case dry-run-leaves-no-tag-with-no-go-tag case_dry_run_leaves_no_tag --no-go-tag

run_case success-both-tags-on-full-head-sha case_success_both_tags
run_case success-no-go-tag case_success_no_go_tag
run_case success-dry-run case_success_dry_run
run_case success-dry-run-no-go-tag case_success_dry_run_no_go_tag
run_case push-rejected-by-remote case_push_rejected_by_remote
run_case push-fails-unreachable-remote case_push_unreachable_remote
run_case push-fails-unreachable-remote-no-go-tag case_push_unreachable_remote --no-go-tag
run_case push-is-atomic case_push_is_atomic
# R3.3: push.followTags=true must not drag unrelated local annotated tags along
run_case push-does-not-follow-tags case_push_no_follow_tags "bindings/rsac-go/v$VER v$VER "
run_case push-does-not-follow-tags-no-go-tag case_push_no_follow_tags "v$VER " --no-go-tag
run_case tag-creation-fails case_tag_creation_fails

run_harness_case harness-fake-gh-demands-repo case_harness_fake_gh_demands_repo
run_harness_case harness-remote-helper-serves-github-url case_harness_remote_helper
run_harness_case harness-unmatched-filter-fails case_harness_unmatched_filter
run_harness_case harness-world-and-expectations case_harness_world_and_state
run_harness_case harness-assertions-detect-what-they-claim case_harness_assertions

if [ -n "${RSAC_TEST_FILTER:-}" ] && [ "$SELECTED" -eq 0 ]; then
  echo "FAIL no case name contains RSAC_TEST_FILTER=$RSAC_TEST_FILTER"
  FAILED=$((FAILED + 1))
fi
echo "tag-release tests: $PASSED passed, $FAILED failed"
rm -rf "$WORK" # also cleaned by the EXIT trap; explicit so interpreters that skip EXIT traps leave nothing behind
[ "$FAILED" -eq 0 ]

#!/usr/bin/env bash
#
# tag-release.test.sh — contract tests for scripts/tag-release.sh
#
# Written from the release-tooling contract ONLY (section 4 describes the script,
# section 7 describes these tests, revision notes r2 and r3 amend section 4); the
# script under test was not read while writing them. Every expectation names the
# contract clause it comes from. A failing case stays failing: never weaken an
# expectation to match observed output; fix the script or report the contract problem.
#
# ── What a case gets ────────────────────────────────────────────────────────
#   * a throwaway git repository (branch master, one commit pushed) whose remote
#     `origin` is a bare repository, inside a `mktemp -d` workspace removed on exit;
#   * a committed stub scripts/check-version-lockstep.sh whose exit status the case
#     controls ($FAKE_DIR/lockstep.rc) and which logs its arguments;
#   * a committed CHANGELOG.md, .github/workflows/ci.yml (with a `name:` line) and
#     bindings/rsac-go/go.mod the case may replace (committed and pushed, so the
#     tracked tree stays clean and HEAD stays on the remote);
#   * fake `gh` and `cargo` executables reached only through GH_BIN / CARGO_BIN.
#     The fake gh answers the two call forms of contract section 4 check 7 from
#     files the case writes and logs every invocation. Per R3.1 it REJECTS (exit 97,
#     and the case fails) any call that lacks `--repo <owner>/<name>` right after
#     `run list` / `run view <id>`, or that names another repository than
#     $FAKE_EXPECT_REPO. The harness sets RELEASE_REPO to that repository, because
#     the throwaway remote is a local bare repository, not a GitHub URL.
#     Real gh/cargo/curl are shadowed on PATH by "poison" executables, so a script
#     that ignores a seam is caught (and can never reach a real GitHub or registry).
#   * git is isolated from the user's configuration (HOME, system config) and the
#     throwaway repository sets identity and gpgsign=false locally.
#
# ── Giving a remote a GitHub URL without network (R3.1 cases) ────────────────
#   The script must derive <owner>/<name> from `git remote get-url <remote>`, while
#   fetch, ls-remote and push must keep working against the local bare repository.
#   `url.<local path>.insteadOf` cannot do that: `git remote get-url` EXPANDS
#   insteadOf, so the script would see the local path (verified). Instead the
#   harness sets `remote.<name>.url` to the GitHub-form URL and
#   `remote.<name>.vcs=rsacproxy`; the tiny remote helper `git-remote-rsacproxy`
#   (on PATH, created below) then serves every fetch, ls-remote and push from the
#   bare repository named by $RSAC_PROXY_REPO. `git remote get-url` reports the
#   GitHub form, and git can never open a network connection for that remote.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   bash scripts/tests/tag-release.test.sh
#
# Running ONE case (or a few): set RSAC_TEST_FILTER to a substring of the case name.
#   RSAC_TEST_FILTER=tree-dirty-staged bash scripts/tests/tag-release.test.sh
#   List the names:  grep -o '^run_case [^ ]*' scripts/tests/tag-release.test.sh
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

# git must behave identically everywhere: no inherited repository, no user/system
# configuration, no prompts, byte-wise sorting.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX GIT_COMMON_DIR GIT_OBJECT_DIRECTORY \
      GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_CEILING_DIRECTORIES
export LC_ALL=C GIT_TERMINAL_PROMPT=0 GIT_CONFIG_NOSYSTEM=1

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config"
mkdir -p "$HOME"

# ── fake and poison executables ─────────────────────────────────────────────
mkdir -p "$WORK/bin" "$WORK/poison" "$WORK/helper"

# Fake GitHub CLI. Answers ONLY the two forms of contract section 4 check 7, as amended by R3.1:
#   run list --repo R --workflow ci.yml ...  → stdout of $FAKE_DIR/gh-list.out, status gh-list.rc
#   run view <id> --repo R ...               → stdout of $FAKE_DIR/gh-view-<id>.out, status gh-view-<id>.rc
# R is $FAKE_EXPECT_REPO. Missing --repo, --repo not right after the subcommand words, or another
# repository: exit 97 and a line in $FAKE_DIR/gh.unexpected (post_checks then fails the case).
# (the --jq expression is ignored: the case supplies the output the expression would print).
# Every call is appended to $FAKE_DIR/gh.log.
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
reply() { # <key>: stdout from gh-<key>.out, exit status from gh-<key>.rc (default 0)
  local rc=0
  [ -f "$d/gh-$1.out" ] && cat "$d/gh-$1.out"
  [ -f "$d/gh-$1.rc" ] && rc="$(cat "$d/gh-$1.rc")"
  [ "$rc" -ne 0 ] && echo "fake gh: failing on request ($1)" >&2
  exit "$rc"
}
if [ "${1:-}" = run ] && [ "${2:-}" = list ]; then
  [ "${3:-}" = --repo ] || unexpected "no --repo right after 'run list'"
  [ "${4:-}" = "$want_repo" ] || unexpected "--repo is '${4:-}', expected '$want_repo'"
  wf=""
  prev=""
  for x in "$@"; do
    [ "$prev" = --workflow ] && wf="$x"
    prev="$x"
  done
  [ "$wf" = ci.yml ] || unexpected "workflow '$wf' is not ci.yml"
  reply list
fi
if [ "${1:-}" = run ] && [ "${2:-}" = view ] && [ -n "${3:-}" ]; then
  [ "${4:-}" = --repo ] || unexpected "no --repo right after 'run view <id>'"
  [ "${5:-}" = "$want_repo" ] || unexpected "--repo is '${5:-}', expected '$want_repo'"
  if [ ! -f "$d/gh-view-$3.out" ] && [ ! -f "$d/gh-view-$3.rc" ]; then
    echo "fake gh: no such run: $3" >&2
    exit 1
  fi
  reply "view-$3"
fi
unexpected "not one of the two contract forms"
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
build_template() {
  local t="$TEMPLATE"
  mkdir -p "$t/repo/scripts" "$t/repo/.github/workflows" "$t/repo/bindings/rsac-go" || harness_die "mkdir"
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
  # R3.2: the run's workflow name must equal the `name:` of this file.
  cat > "$t/repo/.github/workflows/ci.yml" <<'EOF'
name: CI

on:
  workflow_dispatch:

jobs:
  lint:
    name: Lint
    runs-on: ubuntu-latest
EOF
  # R3.4: a module path without a major-version suffix (fine for majors 0 and 1).
  printf 'module %s\n\ngo 1.21\n' "$GO_MODULE" > "$t/repo/bindings/rsac-go/go.mod"
  git -C "$t/repo" add -A || harness_die "git add"
  git -C "$t/repo" commit -q -m "initial" || harness_die "git commit"
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
  RSAC_PROXY_REPO="$REMOTE"
  FAKE_EXPECT_REPO="$TEST_REPO"
  RELEASE_REPO="$TEST_REPO" # R3.1 seam: the throwaway remote is a local path, not a GitHub URL
  export RSAC_PROXY_REPO FAKE_EXPECT_REPO RELEASE_REPO
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
set_workflow_file() { # <full content>: replace .github/workflows/ci.yml, commit and push it
  printf '%s\n' "$1" > "$REPO/.github/workflows/ci.yml"
  commit_if_changed "ci.yml"
}
workflow_file_with() { # <`name:` line>: a ci.yml whose top-level name line is the argument
  printf '%s\n\non:\n  workflow_dispatch:\n\njobs:\n  lint:\n    name: Lint\n    runs-on: ubuntu-latest' "$1"
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
use_remote_url() { # <remote> <url> <bare repository serving it>: GitHub-form URL, transport still local
  must git -C "$REPO" remote set-url "$1" "$2" || return 1
  must git -C "$REPO" config "remote.$1.vcs" rsacproxy || return 1
  RSAC_PROXY_REPO="$3"
  export RSAC_PROXY_REPO
}
arrange_list() { printf '%s' "$1" > "$FAKE_DIR/gh-list.out"; }  # stdout of the run listing
arrange_run() { # <id> <line>: output of `gh run view <id>`; an empty line argument means no output at all
  if [ -n "$2" ]; then printf '%s\n' "$2" > "$FAKE_DIR/gh-view-$1.out"; else : > "$FAKE_DIR/gh-view-$1.out"; fi
}
arrange_green() { # one listed run (4242) that qualifies for the current HEAD (R3.2: six fields, workflow name last)
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success CI"
}
set_rc() { printf '%s\n' "$2" > "$FAKE_DIR/$1.rc"; } # <lockstep|cargo|gh-list|gh-view-N> <code>
install_hook() { # <name> <script-body>: hook in the bare origin
  mkdir -p "$REMOTE/hooks"
  printf '#!/bin/sh\n%s\n' "$2" > "$REMOTE/hooks/$1"
  chmod +x "$REMOTE/hooks/$1"
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

assert_nothing_created() { # <clause>: a failure creates or pushes nothing
  cmp -s "$SB/tags.before.remote" "$SB/tags.after.remote" \
    || fail "a tag was pushed to or removed from a remote [$1]"
  # Allowed local tags afterwards: those present before, plus those a `git fetch --tags`
  # legitimately mirrors from a remote (contract check 2 fetches tags).
  { cat "$SB/tags.before.local"; sed 's/^[^ ]* //' "$SB/tags.before.remote"; } | sort -u > "$SB/tags.allowed"
  local extra lost
  extra="$(comm -13 "$SB/tags.allowed" "$SB/tags.after.local" | tr '\n' '|')"
  [ -z "$extra" ] || fail "local tag(s) created by a failing run: $extra [$1]"
  lost="$(comm -23 "$SB/tags.before.local" "$SB/tags.after.local" | tr '\n' '|')"
  [ -z "$lost" ] || fail "pre-existing local tag(s) removed: $lost [$1]"
}
assert_no_tag_lost() { # <clause>
  local lost
  lost="$(comm -23 "$SB/tags.before.local" "$SB/tags.after.local" | tr '\n' '|')"
  [ -z "$lost" ] || fail "pre-existing local tag(s) removed: $lost [$1]"
}
assert_tag_unchanged() { # <tag> <clause>: no push, no overwrite, no new object on either side
  local pre post
  assert_eq "$(grep -F "refs/tags/$1 " "$SB/tags.after.remote")" "$(grep -F "refs/tags/$1 " "$SB/tags.before.remote")" \
    "remote tag $1 changed [$2]"
  # Locally the tag may only be the pre-existing object (a fetch may mirror a remote-only tag).
  pre="$({ cat "$SB/tags.before.local"; sed 's/^[^ ]* //' "$SB/tags.before.remote"; } | grep -F "refs/tags/$1 " | sort -u)"
  post="$(grep -F "refs/tags/$1 " "$SB/tags.after.local")"
  [ -z "$post" ] || assert_eq "$post" "$pre" "local tag $1 changed [$2]"
}

tag_dir() { # <local|remote|upstream>
  case "$1" in
    local) echo "$REPO" ;;
    remote) echo "$REMOTE" ;;
    upstream) echo "$UPSTREAM" ;;
  esac
}
assert_tag_present() { # <local|remote|upstream> <tag> <message> <clause>: annotated, on full HEAD sha
  local dir type peeled subject
  dir="$(tag_dir "$1")"
  type="$(git -C "$dir" cat-file -t "refs/tags/$2" 2>/dev/null)"
  peeled="$(git -C "$dir" rev-parse "refs/tags/$2^{commit}" 2>/dev/null)"
  subject="$(git -C "$dir" for-each-ref --format='%(contents:subject)' "refs/tags/$2" 2>/dev/null)"
  assert_eq "$type" tag "$1 tag $2 object type (annotated) [$4]"
  assert_eq "$peeled" "$SHA" "$1 tag $2 target [$4]"
  assert_eq "$subject" "$3" "$1 tag $2 message [$4]"
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
assert_gh_calls_name_repo() { # <repo> <clause>: every logged gh call has `--repo <repo>` (R3.1)
  local total=0 named=0
  if [ -f "$FAKE_DIR/gh.log" ]; then
    total="$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')"
    named="$(grep -cF -- " --repo $1 " "$FAKE_DIR/gh.log")"
  fi
  [ "$total" -ge 1 ] || fail "gh was never called [$2]"
  assert_eq "$named" "$total" "gh calls naming --repo $1 out of all gh calls [$2]"
}

post_checks() { # run after every case: no seam bypassed, gh stayed inside the two forms
  if [ -s "$FAKE_DIR/poison.log" ]; then
    fail "the script ran a real tool instead of the seam: $(tr '\n' '|' < "$FAKE_DIR/poison.log") [§4 GH_BIN/CARGO_BIN seams, §7 no real gh]"
  fi
  if [ -s "$FAKE_DIR/gh.unexpected" ]; then
    fail "gh invoked outside the two forms of §4 check 7 / R3.1: $(tr '\n' '|' < "$FAKE_DIR/gh.unexpected")"
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

# ── §4 check 0: arguments → exit 2 ─────────────────────────────────────────
case_args_bad() { # <clause> <args...>
  local clause="$1"
  shift
  arrange_green
  run_tag "$@"
  expect_failure 2 "$clause"
  # Argument errors precede every other check, so no tool has been used yet.
  assert_not_called "$clause" lockstep cargo gh
}
case_args_error_precedes_dirty_tree() {
  printf 'edit\n' >> "$REPO/README.md" # a dirty tree would be exit 10 (check 1)
  arrange_green
  run_tag "1.2"
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
  expect_failure 11 "§4 check 2: git fetch --tags <remote> <branch> must succeed"
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
}
case_remote_and_branch_flags() {
  must git init -q --bare "$UPSTREAM"
  must git -C "$REPO" remote add upstream "$UPSTREAM"
  must git -C "$REPO" push -q upstream master:release
  arrange_green
  run_tag "$VER" --remote upstream --branch release
  # §4 usage: --remote NAME and --branch NAME; the tags go to the chosen remote, the listing uses the chosen branch
  assert_rc 0 "§4 --remote/--branch"
  assert_last_stdout "tagged $CRATE_TAG at $SHA" "§4 --remote/--branch"
  assert_tag_present upstream "$CRATE_TAG" "rsac $VER" "§4 --remote/--branch: pushed to the chosen remote"
  assert_tag_present upstream "$GO_TAG" "rsac-go $VER" "§4 --remote/--branch: pushed to the chosen remote"
  assert_tag_absent remote "$CRATE_TAG" "§4 --remote: the default remote is not used"
  assert_tag_absent remote "$GO_TAG" "§4 --remote: the default remote is not used"
  assert_eq "$(head -n 1 "$FAKE_DIR/gh.log")" \
    "run list --repo $TEST_REPO --workflow ci.yml --branch release --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId" \
    "§4 check 7 listing form with --branch release (R3.1: --repo right after 'run list')"
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
  # Without a fetch refspec `git fetch origin master` cannot refresh refs/remotes/origin/master.
  must git -C "$REPO" config --unset-all remote.origin.fetch
  arrange_green
  run_tag "$VER"
  expect_failure 11 "§4 check 2: a stale tracking ref must not vouch for a remote branch that no longer contains HEAD"
  assert_not_called "§4 check order: nothing after check 2 runs" lockstep cargo gh
}
case_no_fetch_refspec_remote_contains_head() {
  # Control for the case above. INTENT reading of §4 check 2 ("HEAD is <remote>/<branch> or an ancestor of
  # it"): the remote branch as just fetched contains HEAD, so the check passes even though a remote without
  # a fetch refspec leaves refs/remotes/origin/master stale. (Literal reading: the stale ref decides and the
  # script exits 11. The case follows the intent; see the report's Contract issues.)
  touch_commit "pushed"
  push_master
  must git -C "$REPO" update-ref refs/remotes/origin/master HEAD~1
  must git -C "$REPO" config --unset-all remote.origin.fetch
  arrange_green
  run_tag "$VER"
  expect_both_tagged "§4 check 2 (intent): the remote branch contains HEAD although the tracking ref is stale"
}

# ── R3.1: repository binding (RELEASE_REPO, or parsed from the remote URL) ───
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
  export FAKE_EXPECT_REPO
  use_remote_url origin "$1" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged "R3.1: $1 yields --repo $2"
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
  RELEASE_REPO="x/y" # R3.1: a non-empty RELEASE_REPO is used as REPO
  FAKE_EXPECT_REPO="x/y"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged "R3.1: RELEASE_REPO wins over the remote URL"
  assert_gh_calls_name_repo "x/y" "R3.1: --repo is RELEASE_REPO, not the URL's o/n"
}
case_repo_empty_env_uses_url() {
  RELEASE_REPO="" # R3.1: "set and non-empty": an empty value is ignored
  FAKE_EXPECT_REPO="o/n"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  arrange_green
  run_tag "$VER"
  expect_both_tagged "R3.1: an empty RELEASE_REPO falls back to the remote URL"
  assert_gh_calls_name_repo "o/n" "R3.1: --repo comes from the URL"
}
case_repo_from_chosen_remote() {
  unset RELEASE_REPO
  FAKE_EXPECT_REPO="up/stream"
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
}

# ── R3.2: workflow identity (six fields, workflow name last) ───────────────
case_ci_name_file() { # <full ci.yml> <run's workflow name> <qualifies|fail> <clause> [reason phrase]
  set_workflow_file "$1" || return 0
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success $2"
  run_tag "$VER"
  if [ "$3" = qualifies ]; then
    expect_both_tagged "$4"
  else
    expect_failure 16 "$4"
    [ -z "${5:-}" ] || assert_stderr_has_ci "$5" "$4"
  fi
}
case_ci_name_line() { # <`name:` line of ci.yml> <run's workflow name> <qualifies|fail> <clause> [reason phrase]
  case_ci_name_file "$(workflow_file_with "$1")" "$2" "$3" "$4" "${5:-}"
}

# ── §4 check 3: tags must not exist → exit 12 ──────────────────────────────
case_tag_exists() { # <local|remote> <tag> <clause> [extra args...]
  make_tag "$1" "$2"
  arrange_green
  run_tag "$VER" "${@:4}"
  expect_failure 12 "$3"
  assert_not_called "§4 check order: nothing after check 3 runs" lockstep cargo gh
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

# ── §4 check 7: a qualifying CI run → exit 16 ──────────────────────────────
DISPATCH_HINT_MASTER="gh workflow run ci.yml --ref master -f full=true -f semver=true"
case_ci_single_run() { # <view line, @SHA@ = HEAD> <phrase or ""> <clause>
  arrange_list $'4242\n'
  arrange_run 4242 "${1//@SHA@/$SHA}"
  run_tag "$VER"
  expect_failure 16 "$3"
  # §4 check 7: the message names why ...
  [ -z "$2" ] || assert_stderr_has_ci "$2" "$3"
  # ... and (HEAD is the tip of master here, R3.5) prints the dispatch command
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7 / R3.5: dispatch command on exit 16 when HEAD is the tip"
}
case_ci_no_run_listed() {
  arrange_list ""
  run_tag "$VER"
  expect_failure 16 "§4 check 7: zero listed runs is no qualifying run"
  assert_stderr_has_ci "no run found" "§4 check 7 / r2: reason 'no run found'"
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7 / R3.5: dispatch command on exit 16 when HEAD is the tip"
}
case_ci_no_run_listed_custom_branch() {
  must git -C "$REPO" push -q origin master:release
  arrange_list ""
  run_tag "$VER" --branch release
  expect_failure 16 "§4 check 7: no run on branch release"
  assert_stderr_has "gh workflow run ci.yml --ref release -f full=true -f semver=true" \
    "§4 check 7: the dispatch command carries <branch>"
}
case_ci_list_cli_fails() {
  set_rc gh-list 1
  run_tag "$VER"
  expect_failure 16 "§4 check 7: any non-zero exit of the GitHub CLI is a failure of check 7, never a pass"
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7 / R3.5: dispatch command on exit 16 when HEAD is the tip"
}
case_ci_view_cli_fails() {
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success CI" # output is fine, the exit is not
  set_rc gh-view-4242 1
  run_tag "$VER"
  expect_failure 16 "§4 check 7: any non-zero exit of the GitHub CLI is a failure of check 7, never a pass"
}
case_ci_first_view_cli_fails_second_would_qualify() {
  # AMBIGUITY: "Any non-zero exit of the GitHub CLI ... is a failure of check 7 (exit 16)".
  # Reading A (chosen, most literal): the CLI failing while inspecting ANY run fails check 7 at once.
  # Reading B: that one run merely does not qualify and the next listed run is tried.
  arrange_list $'101\n102\n'
  arrange_run 101 "$SHA completed success workflow_dispatch success CI"
  set_rc gh-view-101 1
  arrange_run 102 "$SHA completed success workflow_dispatch success CI"
  run_tag "$VER"
  expect_failure 16 "§4 check 7: a failing CLI call is a failure of check 7 (reading A)"
}
case_ci_run_id_view_unknown_run() {
  run_tag "$VER" --run-id 999 # the fake gh knows no run 999 and exits non-zero
  expect_failure 16 "§4 check 7: gh run view failing for --run-id is a failure of check 7"
}
case_ci_failure_creates_no_tags() { # failure of check 7 must not leave tags behind (no tag step ran)
  arrange_list ""
  run_tag "$VER"
  expect_failure 16 "§4 table: checks 8-9 only run after check 7 passes"
  assert_eq "$(wc -l < "$SB/tags.after.local" | tr -d ' ')" 0 "local tags after exit 16 [§4 check 8 comes after check 7]"
}
case_dry_run_fails_on_check7() {
  arrange_list ""
  run_tag "$VER" --dry-run
  expect_failure 16 "§4 --dry-run: runs checks 0 to 7, so a missing run still fails"
}
case_dry_run_fails_on_dirty_tree() {
  printf 'edit\n' >> "$REPO/README.md"
  arrange_green
  run_tag "$VER" --dry-run
  expect_failure 10 "§4 --dry-run: runs checks 0 to 7, so a dirty tree still fails"
}
case_dry_run_checks_go_module() {
  use_version 2.0.0
  set_gomod "$GO_MODULE" || return 0
  arrange_green
  run_tag "$VER" --dry-run
  expect_failure 18 "R3.4 / §4 --dry-run: check 3b is among checks 0 to 7"
}

# ── R3.5: the exit-16 hint depends on whether HEAD is the branch tip ───────
case_ci_hint_behind_tip() { # <branch> <setup: no-run|failed-run> [extra args...]
  local branch="$1"
  make_head_behind_tip "$branch" || return 0
  case "$2" in
    no-run) arrange_list "" ;;
    failed-run)
      arrange_list $'4242\n'
      arrange_run 4242 "$SHA completed failure workflow_dispatch success CI"
      ;;
  esac
  run_tag "$VER" "${@:3}"
  expect_failure 16 "§4 check 7: no qualifying run for HEAD"
  # R3.5: "the message says that a dispatch on <branch> tests the branch tip, not this commit, and names
  # the two ways out: tag the tip, or dispatch ci.yml on a ref at this commit and pass --run-id".
  # AMBIGUITY: the wording is not fixed; chosen reading: the message mentions the branch name, 'dispatch',
  # 'tip' and '--run-id'. (Whether the exact `--ref <branch>` dispatch command must be absent is not asserted.)
  assert_stderr_has_ci "$branch" "R3.5: the message names the branch"
  assert_stderr_has_ci "dispatch" "R3.5: the message talks about a dispatch"
  assert_stderr_has_ci "tip" "R3.5: the message says a dispatch tests the branch tip"
  assert_stderr_has "--run-id" "R3.5: the message names the --run-id way out"
}
case_ci_hint_behind_tip_reason() {
  make_head_behind_tip master || return 0
  arrange_list ""
  run_tag "$VER"
  expect_failure 16 "§4 check 7"
  assert_stderr_has_ci "no run found" "§4 check 7 / r2: the reason is still named when HEAD is behind the tip"
}
case_ci_hint_at_tip() {
  arrange_list ""
  run_tag "$VER"
  expect_failure 16 "§4 check 7"
  # R3.5: "When HEAD is the tip, the hint is the dispatch command as before."
  assert_stderr_has "$DISPATCH_HINT_MASTER" "R3.5: HEAD is the tip of the fetched master, so the hint is the dispatch command"
}

# ── §4 check 7: the two gh call forms ──────────────────────────────────────
case_gh_call_forms() {
  arrange_green
  run_tag "$VER"
  assert_rc 0 "§4 check 7"
  # §4 check 7 listing form, exactly (shell quoting removed in the log); R3.1: --repo right after 'run list'
  assert_eq "$(head -n 1 "$FAKE_DIR/gh.log")" \
    "run list --repo $TEST_REPO --workflow ci.yml --branch master --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId" \
    "§4 check 7 listing form with R3.1 --repo"
  # §4 check 7: exactly two calls for one listed run (one listing, one inspection)
  assert_eq "$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')" 2 "number of gh calls [§4 check 7: the two forms]"
  local view prefix expr
  view="$(sed -n '2p' "$FAKE_DIR/gh.log")"
  # R3.1: --repo right after 'run view <id>'; R3.2: workflowName is requested too
  prefix="run view 4242 --repo $TEST_REPO --json headSha,status,conclusion,event,jobs,workflowName --jq "
  case "$view" in
    "$prefix"*) ;;
    *) fail "second gh call is '$view', expected it to start with '$prefix' [§4 check 7 inspection form, R3.1, R3.2]" ;;
  esac
  expr="${view#"$prefix"}"
  [ -n "$expr" ] || fail "inspection form has an empty --jq expression [§4 check 7]"
  # The expression must be able to print the literals the contract defines for the readiness field.
  case "$expr" in *"Release Readiness"*) ;; *) fail "--jq expression never names the job 'Release Readiness' [§4 check 7 readiness]" ;; esac
  case "$expr" in *missing*) ;; *) fail "--jq expression cannot print 'missing' [§4 check 7 readiness]" ;; esac
  case "$expr" in *none*) ;; *) fail "--jq expression cannot print 'none' [§4 check 7 conclusion/readiness]" ;; esac
  case "$expr" in *workflowName*) ;; *) fail "--jq expression never reads workflowName [R3.2 sixth field]" ;; esac
}
case_check_tool_forms() {
  arrange_green
  run_tag "$VER"
  assert_rc 0 "§4 checks 4 and 5"
  assert_eq "$(log_text lockstep)" "--expect $VER|" "§4 check 4: bash scripts/check-version-lockstep.sh --expect X.Y.Z"
  assert_eq "$(log_text cargo)" "metadata --locked --format-version 1|" "§4 check 5: cargo metadata --locked --format-version 1"
}

# ── §4 check 7: the run must qualify; run selection ────────────────────────
case_later_run_used() {
  arrange_list $'101\n102\n103\n'
  arrange_run 101 "$SHA completed failure workflow_dispatch failure CI"
  arrange_run 102 "$SHA in_progress none workflow_dispatch none CI"
  arrange_run 103 "$SHA completed success workflow_dispatch success CI"
  run_tag "$VER"
  # §4 check 7: runs are inspected in order; the first that qualifies is used
  expect_both_tagged "§4 check 7: a later listed run is used when earlier ones do not qualify"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "101 102 103 " \
    "inspection order [§4 check 7: in listed order]"
}
case_no_listed_run_qualifies() {
  arrange_list $'101\n102\n'
  arrange_run 101 "$SHA completed failure workflow_dispatch failure CI"
  arrange_run 102 "$SHA in_progress none workflow_dispatch none CI"
  run_tag "$VER"
  expect_failure 16 "§4 check 7: no listed run qualifies"
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7 / R3.5: dispatch command on exit 16 when HEAD is the tip"
}
case_run_id_skips_listing() {
  arrange_list $'4242\n' # a listing would offer 4242; --run-id must not consult it
  arrange_run 4242 "$SHA completed success workflow_dispatch success CI"
  arrange_run 777 "$SHA completed success workflow_dispatch success CI"
  run_tag "$VER" --run-id 777
  expect_both_tagged "§4 check 7: --run-id N inspects only run N"
  assert_eq "$(grep -c '^run list ' "$FAKE_DIR/gh.log")" 0 "number of 'run list' calls [§4 check 7: listing only when --run-id is absent]"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "777 " \
    "runs inspected [§4 check 7: only run N]"
}
case_run_id_not_qualifying_does_not_fall_back() {
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success CI" # would qualify, but is not N
  arrange_run 777 "0123456789abcdef0123456789abcdef01234567 completed success workflow_dispatch success CI"
  run_tag "$VER" --run-id 777
  expect_failure 16 "§4 check 7: with --run-id N only run N is inspected, no fallback to a listing"
  assert_eq "$(grep -c '^run list ' "$FAKE_DIR/gh.log")" 0 "number of 'run list' calls [§4 check 7]"
  assert_stderr_has_ci "wrong commit" "§4 check 7 / r2: reason 'wrong commit'"
}

# ── §4 steps 8 and 9: create and push (exit 17) ────────────────────────────
case_success_both_tags() {
  touch_commit "tagged commit is the HEAD sha, not an abbreviation"
  push_master
  install_hook post-receive 'echo "--push--" >> "$(dirname "$0")/../hooklog"; cat >> "$(dirname "$0")/../hooklog"'
  arrange_green
  run_tag "$VER"
  expect_both_tagged "§4 steps 8-9: annotated tags on the full HEAD sha, pushed"
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
  expect_crate_tagged_only "§4 --no-go-tag: only the crate tag is created and pushed"
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
  # §4 --dry-run "prints what it would tag". AMBIGUITY: stream and wording are unspecified;
  # chosen reading: the Go tag name appears somewhere in stdout or stderr.
  assert_output_has "$GO_TAG" "§4 --dry-run prints what it would tag"
  assert_stdout_unprefixed "§4: every message goes to stderr"
  # §4 --dry-run runs checks 0 to 7: the CI run was inspected
  assert_eq "$(grep -c '^run view ' "$FAKE_DIR/gh.log")" 1 "gh run view calls [§4 --dry-run runs check 7]"
}
case_success_dry_run_no_go_tag() {
  arrange_green
  run_tag "$VER" --dry-run --no-go-tag
  assert_rc 0 "§4 --dry-run with --no-go-tag"
  assert_last_stdout "dry-run ok $CRATE_TAG at $SHA" "§4: last stdout line for --dry-run"
  assert_nothing_created "§4 --dry-run creates and pushes nothing"
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

# ── §4 precedence of the remaining checks ──────────────────────────────────
case_changelog_missing_before_gh_calls() {
  set_changelog $'# Changelog\n'
  arrange_green
  run_tag "$VER"
  expect_failure 15 "§4 table: check 6 before check 7"
  assert_not_called "§4 table: check 6 before check 7" gh
}

# ── harness self-tests (no script under test) ──────────────────────────────
case_harness_fake_gh_demands_repo() {
  # R3.1 / §7: the fake gh must reject a call without --repo, with --repo in the wrong place, or with
  # another repository, and accept the contract forms with the right one.
  local d="$WORK/harness-fake"
  rm -rf "$d"
  mkdir -p "$d"
  FAKE_DIR="$d"
  FAKE_EXPECT_REPO="$TEST_REPO"
  export FAKE_DIR FAKE_EXPECT_REPO
  printf '4242\n' > "$d/gh-list.out"
  printf 'line\n' > "$d/gh-view-7.out"
  gh_status() { "$GH_BIN" "$@" > /dev/null 2>&1; echo $?; }
  assert_eq "$(gh_status run list --repo "$TEST_REPO" --workflow ci.yml)" 0 "fake gh accepts 'run list --repo R'"
  assert_eq "$(gh_status run list --workflow ci.yml)" 97 "fake gh rejects 'run list' without --repo"
  assert_eq "$(gh_status run list --workflow ci.yml --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'run list'"
  assert_eq "$(gh_status run list --repo other/repo --workflow ci.yml)" 97 "fake gh rejects another repository (list)"
  assert_eq "$(gh_status run view 7 --repo "$TEST_REPO" --json x)" 0 "fake gh accepts 'run view <id> --repo R'"
  assert_eq "$(gh_status run view 7 --json x)" 97 "fake gh rejects 'run view' without --repo"
  assert_eq "$(gh_status run view 7 --json x --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'run view <id>'"
  assert_eq "$(gh_status run view 7 --repo other/repo --json x)" 97 "fake gh rejects another repository (view)"
  assert_eq "$(wc -l < "$d/gh.unexpected" | tr -d ' ')" 6 "rejected calls recorded in gh.unexpected"
}
case_harness_remote_helper() {
  # A remote with a GitHub-form URL and remote.<name>.vcs=rsacproxy reports that URL and still
  # fetches and pushes through the local bare repository (no network).
  new_sandbox
  use_remote_url origin "git@github.com:o/n.git" "$REMOTE" || return 0
  assert_eq "$(git -C "$REPO" remote get-url origin)" "git@github.com:o/n.git" "git remote get-url reports the GitHub form"
  must git -C "$REPO" fetch --tags origin master
  must git -C "$REPO" tag -a harness-probe -m probe
  must git -C "$REPO" push origin refs/tags/harness-probe
  assert_eq "$(git -C "$REMOTE" for-each-ref --format='%(refname:short)' refs/tags)" harness-probe "tag pushed through the helper"
}
case_harness_unmatched_filter() {
  # R3.8: a filter that selects no case prints a line naming it and the file exits non-zero.
  local out rc
  out="$(RSAC_TEST_FILTER=zz-no-such-case-zz bash "$SELF" 2>&1)"
  rc=$?
  [ "$rc" != 0 ] || fail "exit 0 although the filter selected no case [R3.8]"
  case "$out" in *zz-no-such-case-zz*) ;; *) fail "no output line names the unmatched filter; output was: $out [R3.8]" ;; esac
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
run_case args-error-precedes-dirty-tree case_args_error_precedes_dirty_tree

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
run_case repo-env-overrides-url case_repo_env_overrides_url
run_case repo-empty-env-uses-url case_repo_empty_env_uses_url
run_case repo-from-chosen-remote case_repo_from_chosen_remote

run_case tag-exists-local-crate case_tag_exists local "$CRATE_TAG" "§4 check 3: vX.Y.Z exists locally"
run_case tag-exists-local-go case_tag_exists local "$GO_TAG" "§4 check 3: bindings/rsac-go/vX.Y.Z exists locally"
run_case tag-exists-remote-crate case_tag_exists remote "$CRATE_TAG" "§4 check 3: vX.Y.Z exists on the remote"
run_case tag-exists-remote-go case_tag_exists remote "$GO_TAG" "§4 check 3: bindings/rsac-go/vX.Y.Z exists on the remote"
run_case tag-exists-crate-with-no-go-tag case_tag_exists local "$CRATE_TAG" "§4 check 3: --no-go-tag only exempts the Go tag" --no-go-tag
run_case no-go-tag-ignores-local-go-tag case_no_go_tag_ignores_go_tag local
run_case no-go-tag-ignores-remote-go-tag case_no_go_tag_ignores_go_tag remote
run_case similar-tags-local-do-not-block case_similar_tags_do_not_block local
run_case similar-tags-remote-do-not-block case_similar_tags_do_not_block remote
run_case tags-check-precedes-lockstep case_tags_before_lockstep
run_case fetch-check-precedes-tags case_fetch_before_tags

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

CI7="§4 check 7"
run_case ci-no-run-listed case_ci_no_run_listed
run_case ci-no-run-listed-custom-branch case_ci_no_run_listed_custom_branch
run_case ci-run-wrong-commit case_ci_single_run \
  "0123456789abcdef0123456789abcdef01234567 completed success workflow_dispatch success CI" "wrong commit" \
  "$CI7: headSha must equal the full HEAD sha"
run_case ci-run-abbreviated-head-sha case_ci_single_run \
  "abcdef0 completed success workflow_dispatch success CI" "wrong commit" \
  "$CI7: an abbreviated sha is not equal to the full HEAD sha"
run_case ci-run-queued case_ci_single_run "@SHA@ queued none workflow_dispatch none CI" "still running" \
  "$CI7: status must be completed (queued)"
run_case ci-run-in-progress case_ci_single_run "@SHA@ in_progress none workflow_dispatch none CI" "still running" \
  "$CI7: status must be completed (in_progress)"
run_case ci-run-conclusion-failure case_ci_single_run "@SHA@ completed failure workflow_dispatch success CI" "failed" \
  "$CI7: conclusion must be success (failure)"
run_case ci-run-conclusion-cancelled case_ci_single_run "@SHA@ completed cancelled workflow_dispatch success CI" "failed" \
  "$CI7: conclusion must be success (cancelled)"
run_case ci-run-conclusion-none-when-completed case_ci_single_run "@SHA@ completed none workflow_dispatch success CI" "failed" \
  "$CI7 / r2: an empty or null conclusion prints as none, which is not success: reason 'failed'"
run_case ci-run-event-push case_ci_single_run "@SHA@ completed success push success CI" "not dispatched" \
  "$CI7: event must be workflow_dispatch (push)"
run_case ci-run-event-pull-request case_ci_single_run "@SHA@ completed success pull_request success CI" "not dispatched" \
  "$CI7: event must be workflow_dispatch (pull_request)"
run_case ci-run-readiness-missing case_ci_single_run "@SHA@ completed success workflow_dispatch missing CI" "no Release Readiness job" \
  "$CI7: readiness missing means the run has no Release Readiness job"
run_case ci-run-readiness-failure case_ci_single_run "@SHA@ completed success workflow_dispatch failure CI" "Release Readiness failed" \
  "$CI7 / r2: readiness must be success (failure): reason 'Release Readiness failed'"
run_case ci-run-readiness-skipped case_ci_single_run "@SHA@ completed success workflow_dispatch skipped CI" "Release Readiness failed" \
  "$CI7 / r2: readiness must be success (skipped): reason 'Release Readiness failed'"
run_case ci-run-readiness-none case_ci_single_run "@SHA@ completed success workflow_dispatch none CI" "Release Readiness failed" \
  "$CI7 / r2: readiness none (job without a conclusion) is not success: reason 'Release Readiness failed'"
run_case ci-view-malformed-four-fields case_ci_single_run "@SHA@ completed success workflow_dispatch" "" \
  "$CI7 / R3.2: a line with fewer than six fields is a failure of check 7, never a pass"
run_case ci-view-malformed-five-fields case_ci_single_run "@SHA@ completed success workflow_dispatch success" "" \
  "R3.2: the old five-field line (no workflow name) is malformed: exit 16"
run_case ci-view-malformed-empty-output case_ci_single_run "" "" \
  "$CI7: an empty line has fewer than six fields, so it is a failure of check 7"
run_case ci-list-cli-fails case_ci_list_cli_fails
run_case ci-view-cli-fails case_ci_view_cli_fails
run_case ci-first-view-cli-fails-aborts-check case_ci_first_view_cli_fails_second_would_qualify
run_case ci-run-id-unknown-run case_ci_run_id_view_unknown_run
run_case ci-failure-creates-no-tags case_ci_failure_creates_no_tags
run_case ci-no-run-listed-with-dry-run case_dry_run_fails_on_check7
run_case dirty-tree-with-dry-run case_dry_run_fails_on_dirty_tree
run_case changelog-missing-precedes-gh case_changelog_missing_before_gh_calls
run_case ci-no-listed-run-qualifies case_no_listed_run_qualifies
run_case ci-later-listed-run-used case_later_run_used
run_case ci-run-id-skips-listing case_run_id_skips_listing
run_case ci-run-id-not-qualifying-no-fallback case_run_id_not_qualifying_does_not_fall_back
run_case gh-call-forms case_gh_call_forms
run_case lockstep-and-cargo-call-forms case_check_tool_forms

# R3.2: the run's workflow name (sixth field, the rest of the line) must equal the `name:` of ci.yml
N="R3.2"
run_case ci-name-matching-qualifies case_ci_name_line "name: CI" "CI" qualifies "$N: equal names qualify"
run_case ci-name-mismatch case_ci_name_line "name: CI" "Other" fail \
  "$N: a different workflow name fails check 7" "not the ci.yml workflow"
run_case ci-name-mismatch-with-spaces case_ci_name_line "name: CI" "Some Other Workflow" fail \
  "$N: a different workflow name, with spaces, fails check 7" "not the ci.yml workflow"
run_case ci-name-is-compared-exactly case_ci_name_line "name: CI" "ci" fail \
  "$N: 'equals' is exact, so ci is not CI" "not the ci.yml workflow"
run_case ci-name-prefix-is-not-equal case_ci_name_line "name: CI" "CI Extra" fail \
  "$N: the rest of the line is the name, so 'CI Extra' is not CI" "not the ci.yml workflow"
run_case ci-name-with-spaces-qualifies case_ci_name_line "name: Release Gate CI" "Release Gate CI" qualifies \
  "$N: a name containing spaces, in the file and in the line, qualifies"
run_case ci-name-quoted-double-qualifies case_ci_name_line 'name: "CI"' "CI" qualifies \
  "$N: surrounding double quotes of the file's name: are removed"
run_case ci-name-quoted-single-qualifies case_ci_name_line "name: 'CI'" "CI" qualifies \
  "$N: surrounding single quotes of the file's name: are removed"
run_case ci-name-quoted-with-spaces-qualifies case_ci_name_line 'name: "Release Gate CI"' "Release Gate CI" qualifies \
  "$N: a quoted name containing spaces matches the unquoted run name"
run_case ci-name-surrounding-whitespace-qualifies case_ci_name_line "name:    CI   " "CI" qualifies \
  "$N: surrounding whitespace of the file's name: is removed"
run_case ci-name-ignores-indented-and-commented-name-lines case_ci_name_file \
  $'# name: Commented Out\non:\n  workflow_dispatch:\njobs:\n  lint:\n    name: Lint\nname: CI' "CI" qualifies \
  "$N: only a line matching ^name: counts; comments and indented name: lines are skipped"
run_case ci-name-of-an-indented-line-is-not-the-workflow case_ci_name_file \
  $'# name: Commented Out\non:\n  workflow_dispatch:\njobs:\n  lint:\n    name: Lint\nname: CI' "Lint" fail \
  "$N: an indented name: line is not the workflow name" "not the ci.yml workflow"
run_case ci-view-six-fields-name-mismatch case_ci_single_run "@SHA@ completed success workflow_dispatch success extra" "not the ci.yml workflow" \
  "$N: a sixth field is the workflow name; 'extra' is not CI"

# R3.5: the exit-16 hint
run_case ci-hint-behind-tip-no-run case_ci_hint_behind_tip master no-run
run_case ci-hint-behind-tip-failed-run case_ci_hint_behind_tip master failed-run
run_case ci-hint-behind-tip-custom-branch case_ci_hint_behind_tip release no-run --branch release
run_case ci-hint-behind-tip-names-reason case_ci_hint_behind_tip_reason
run_case ci-hint-at-tip-dispatch-command case_ci_hint_at_tip

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

if [ -n "${RSAC_TEST_FILTER:-}" ] && [ "$SELECTED" -eq 0 ]; then
  echo "FAIL no case name contains RSAC_TEST_FILTER=$RSAC_TEST_FILTER"
  FAILED=$((FAILED + 1))
fi
echo "tag-release tests: $PASSED passed, $FAILED failed"
rm -rf "$WORK" # also cleaned by the EXIT trap; explicit so interpreters that skip EXIT traps leave nothing behind
[ "$FAILED" -eq 0 ]

#!/usr/bin/env bash
#
# tag-release.test.sh — contract tests for scripts/tag-release.sh
#
# Written from the release-tooling contract ONLY (section 4 describes the script,
# section 7 describes these tests); the script under test was not read while writing
# them. Every expectation names the contract clause it comes from. A failing case
# stays failing: never weaken an expectation to match observed output; fix the script
# or report the contract problem.
#
# ── What a case gets ────────────────────────────────────────────────────────
#   * a throwaway git repository (branch master, one commit pushed) whose remote
#     `origin` is a bare repository, inside a `mktemp -d` workspace removed on exit;
#   * a committed stub scripts/check-version-lockstep.sh whose exit status the case
#     controls ($FAKE_DIR/lockstep.rc) and which logs its arguments;
#   * a CHANGELOG.md the case may replace (committed and pushed, so the tracked
#     tree stays clean and HEAD stays on the remote);
#   * fake `gh` and `cargo` executables reached only through GH_BIN / CARGO_BIN.
#     The fake gh answers the two call forms of contract section 4 check 7 from
#     files the case writes and logs every invocation. Real gh/cargo/curl are
#     shadowed on PATH by "poison" executables, so a script that ignores a seam
#     is caught (and can never reach a real GitHub or registry).
#   * git is isolated from the user's configuration (HOME, system config) and the
#     throwaway repository sets identity and gpgsign=false locally.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   bash scripts/tests/tag-release.test.sh
#
# Output: one line per case (`ok <name>` / `FAIL <name>: <why>`), then a summary
# line. Exit status: 0 = every case passed, 1 = at least one case failed. A case
# whose script under test does not exist FAILs (it is never skipped).
# RSAC_TEST_FILTER=<substring> runs only the cases whose name contains it.
#
# Portability: bash 3.2 compatible; no jq, python or node; runs under Git Bash.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT_NAME="tag-release.sh"
SCRIPT="$REPO_ROOT/scripts/$SCRIPT_NAME"

VER="1.2.3"
CRATE_TAG="v$VER"
GO_TAG="bindings/rsac-go/v$VER"

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
mkdir -p "$WORK/bin" "$WORK/poison"

# Fake GitHub CLI. Answers ONLY the two forms of contract section 4 check 7:
#   run list --workflow ci.yml ...   → stdout of $FAKE_DIR/gh-list.out, status gh-list.rc
#   run view <id> ...                → stdout of $FAKE_DIR/gh-view-<id>.out, status gh-view-<id>.rc
# (the --jq expression is ignored: the case supplies the output the expression would print).
# Every call is appended to $FAKE_DIR/gh.log; anything else is logged to gh.unexpected.
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
a="$*"
a="${a//$'\n'/ }"
printf '%s\n' "$a" >> "$d/gh.log"
unexpected() {
  printf '%s\n' "$a" >> "$d/gh.unexpected"
  echo "fake gh: invocation outside the contract forms: $a" >&2
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
  wf=""
  prev=""
  for x in "$@"; do
    [ "$prev" = --workflow ] && wf="$x"
    prev="$x"
  done
  [ "$wf" = ci.yml ] || unexpected
  reply list
fi
if [ "${1:-}" = run ] && [ "${2:-}" = view ] && [ -n "${3:-}" ]; then
  if [ ! -f "$d/gh-view-$3.out" ] && [ ! -f "$d/gh-view-$3.rc" ]; then
    echo "fake gh: no such run: $3" >&2
    exit 1
  fi
  reply "view-$3"
fi
unexpected
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

# Poison: stand-ins for the real tools on PATH. Reached only if a script ignores a seam.
for tool in gh cargo curl; do
  cat > "$WORK/poison/$tool" <<'EOF'
#!/usr/bin/env bash
printf '%s %s\n' "$(basename "$0")" "$*" >> "${FAKE_DIR:?FAKE_DIR not set}/poison.log"
echo "poison: a real tool was invoked instead of the test seam" >&2
exit 99
EOF
done
chmod +x "$WORK"/bin/* "$WORK"/poison/*

case "$PATH" in *\;*) PATH_SEP=';' ;; *) PATH_SEP=':' ;; esac
PATH="$WORK/poison$PATH_SEP$PATH"
export GH_BIN="$WORK/bin/gh" CARGO_BIN="$WORK/bin/cargo"

# ── result bookkeeping ──────────────────────────────────────────────────────
PASSED=0
FAILED=0
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
  mkdir -p "$t/repo/scripts" || harness_die "mkdir"
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

## [1.2.3] - 2026-01-02

- A change.

## [1.2.2] - 2025-12-01

- An older change.
EOF
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
set_changelog() { # <content>: replace CHANGELOG.md, commit and push it
  printf '%s' "$1" > "$REPO/CHANGELOG.md"
  commit_local "changelog" && push_master
}
remove_changelog() {
  must git -C "$REPO" rm -q CHANGELOG.md || return 1
  commit_local "remove changelog" && push_master
}
make_tag() { # <local|remote> <tag>: a lightweight tag at HEAD, on one side only
  must git -C "$REPO" tag "$2" || return 1
  if [ "$1" = remote ]; then
    must git -C "$REPO" push -q origin "refs/tags/$2" || return 1
    must git -C "$REPO" tag -d "$2" || return 1
  fi
}
arrange_list() { printf '%s' "$1" > "$FAKE_DIR/gh-list.out"; }  # stdout of the run listing
arrange_run() { # <id> <line>: output of `gh run view <id>`; an empty line argument means no output at all
  if [ -n "$2" ]; then printf '%s\n' "$2" > "$FAKE_DIR/gh-view-$1.out"; else : > "$FAKE_DIR/gh-view-$1.out"; fi
}
arrange_green() { # one listed run (4242) that qualifies for the current HEAD
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success"
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

post_checks() { # run after every case: no seam bypassed, gh stayed inside the two forms
  if [ -s "$FAKE_DIR/poison.log" ]; then
    fail "the script ran a real tool instead of the seam: $(tr '\n' '|' < "$FAKE_DIR/poison.log") [§4 GH_BIN/CARGO_BIN seams, §7 no real gh]"
  fi
  if [ -s "$FAKE_DIR/gh.unexpected" ]; then
    fail "gh invoked outside the two forms of §4 check 7: $(tr '\n' '|' < "$FAKE_DIR/gh.unexpected")"
  fi
}

run_case() { # <name> <function> [args...]
  local name="$1"
  shift
  if [ -n "${RSAC_TEST_FILTER:-}" ]; then
    case "$name" in *"$RSAC_TEST_FILTER"*) ;; *) return 0 ;; esac
  fi
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
  if [ -z "$FAILS" ]; then
    echo "ok $name"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL $name:$FAILS"
    FAILED=$((FAILED + 1))
  fi
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
    "run list --workflow ci.yml --branch release --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId" \
    "§4 check 7 listing form with --branch release"
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
  # ... and prints the dispatch command
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7: dispatch command on exit 16"
}
case_ci_no_run_listed() {
  arrange_list ""
  run_tag "$VER"
  expect_failure 16 "§4 check 7: zero listed runs is no qualifying run"
  assert_stderr_has_ci "no run found" "§4 check 7: reason 'no run found'"
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7: dispatch command on exit 16"
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
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7: dispatch command on exit 16"
}
case_ci_view_cli_fails() {
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success" # output is fine, the exit is not
  set_rc gh-view-4242 1
  run_tag "$VER"
  expect_failure 16 "§4 check 7: any non-zero exit of the GitHub CLI is a failure of check 7, never a pass"
}
case_ci_first_view_cli_fails_second_would_qualify() {
  # AMBIGUITY: "Any non-zero exit of the GitHub CLI ... is a failure of check 7 (exit 16)".
  # Reading A (chosen, most literal): the CLI failing while inspecting ANY run fails check 7 at once.
  # Reading B: that one run merely does not qualify and the next listed run is tried.
  arrange_list $'101\n102\n'
  arrange_run 101 "$SHA completed success workflow_dispatch success"
  set_rc gh-view-101 1
  arrange_run 102 "$SHA completed success workflow_dispatch success"
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

# ── §4 check 7: the two gh call forms ──────────────────────────────────────
case_gh_call_forms() {
  arrange_green
  run_tag "$VER"
  assert_rc 0 "§4 check 7"
  # §4 check 7 listing form, exactly (shell quoting removed in the log)
  assert_eq "$(head -n 1 "$FAKE_DIR/gh.log")" \
    "run list --workflow ci.yml --branch master --event workflow_dispatch --commit $SHA --limit 20 --json databaseId --jq .[].databaseId" \
    "§4 check 7 listing form"
  # §4 check 7: exactly two calls for one listed run (one listing, one inspection)
  assert_eq "$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')" 2 "number of gh calls [§4 check 7: the two forms]"
  local view prefix expr
  view="$(sed -n '2p' "$FAKE_DIR/gh.log")"
  prefix="run view 4242 --json headSha,status,conclusion,event,jobs --jq "
  case "$view" in
    "$prefix"*) ;;
    *) fail "second gh call is '$view', expected it to start with '$prefix' [§4 check 7 inspection form]" ;;
  esac
  expr="${view#"$prefix"}"
  [ -n "$expr" ] || fail "inspection form has an empty --jq expression [§4 check 7]"
  # The expression must be able to print the literals the contract defines for the readiness field.
  case "$expr" in *"Release Readiness"*) ;; *) fail "--jq expression never names the job 'Release Readiness' [§4 check 7 readiness]" ;; esac
  case "$expr" in *missing*) ;; *) fail "--jq expression cannot print 'missing' [§4 check 7 readiness]" ;; esac
  case "$expr" in *none*) ;; *) fail "--jq expression cannot print 'none' [§4 check 7 conclusion/readiness]" ;; esac
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
  arrange_run 101 "$SHA completed failure workflow_dispatch failure"
  arrange_run 102 "$SHA in_progress none workflow_dispatch none"
  arrange_run 103 "$SHA completed success workflow_dispatch success"
  run_tag "$VER"
  # §4 check 7: runs are inspected in order; the first that qualifies is used
  expect_both_tagged "§4 check 7: a later listed run is used when earlier ones do not qualify"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "101 102 103 " \
    "inspection order [§4 check 7: in listed order]"
}
case_no_listed_run_qualifies() {
  arrange_list $'101\n102\n'
  arrange_run 101 "$SHA completed failure workflow_dispatch failure"
  arrange_run 102 "$SHA in_progress none workflow_dispatch none"
  run_tag "$VER"
  expect_failure 16 "§4 check 7: no listed run qualifies"
  assert_stderr_has "$DISPATCH_HINT_MASTER" "§4 check 7: dispatch command on exit 16"
}
case_run_id_skips_listing() {
  arrange_list $'4242\n' # a listing would offer 4242; --run-id must not consult it
  arrange_run 4242 "$SHA completed success workflow_dispatch success"
  arrange_run 777 "$SHA completed success workflow_dispatch success"
  run_tag "$VER" --run-id 777
  expect_both_tagged "§4 check 7: --run-id N inspects only run N"
  assert_eq "$(grep -c '^run list ' "$FAKE_DIR/gh.log")" 0 "number of 'run list' calls [§4 check 7: listing only when --run-id is absent]"
  assert_eq "$(grep '^run view ' "$FAKE_DIR/gh.log" | awk '{print $3}' | tr '\n' ' ')" "777 " \
    "runs inspected [§4 check 7: only run N]"
}
case_run_id_not_qualifying_does_not_fall_back() {
  arrange_list $'4242\n'
  arrange_run 4242 "$SHA completed success workflow_dispatch success" # would qualify, but is not N
  arrange_run 777 "0123456789abcdef0123456789abcdef01234567 completed success workflow_dispatch success"
  run_tag "$VER" --run-id 777
  expect_failure 16 "§4 check 7: with --run-id N only run N is inspected, no fallback to a listing"
  assert_eq "$(grep -c '^run list ' "$FAKE_DIR/gh.log")" 0 "number of 'run list' calls [§4 check 7]"
  assert_stderr_has_ci "wrong commit" "§4 check 7: reason 'wrong commit'"
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
  "0123456789abcdef0123456789abcdef01234567 completed success workflow_dispatch success" "wrong commit" \
  "$CI7: headSha must equal the full HEAD sha"
run_case ci-run-abbreviated-head-sha case_ci_single_run \
  "abcdef0 completed success workflow_dispatch success" "wrong commit" \
  "$CI7: an abbreviated sha is not equal to the full HEAD sha"
run_case ci-run-queued case_ci_single_run "@SHA@ queued none workflow_dispatch none" "still running" \
  "$CI7: status must be completed (queued)"
run_case ci-run-in-progress case_ci_single_run "@SHA@ in_progress none workflow_dispatch none" "still running" \
  "$CI7: status must be completed (in_progress)"
run_case ci-run-conclusion-failure case_ci_single_run "@SHA@ completed failure workflow_dispatch success" "failed" \
  "$CI7: conclusion must be success (failure)"
run_case ci-run-conclusion-cancelled case_ci_single_run "@SHA@ completed cancelled workflow_dispatch success" "failed" \
  "$CI7: conclusion must be success (cancelled)"
run_case ci-run-conclusion-none-when-completed case_ci_single_run "@SHA@ completed none workflow_dispatch success" "" \
  "$CI7: an empty or null conclusion prints as none, which is not success"
run_case ci-run-event-push case_ci_single_run "@SHA@ completed success push success" "not dispatched" \
  "$CI7: event must be workflow_dispatch (push)"
run_case ci-run-event-pull-request case_ci_single_run "@SHA@ completed success pull_request success" "not dispatched" \
  "$CI7: event must be workflow_dispatch (pull_request)"
run_case ci-run-readiness-missing case_ci_single_run "@SHA@ completed success workflow_dispatch missing" "no Release Readiness job" \
  "$CI7: readiness missing means the run has no Release Readiness job"
run_case ci-run-readiness-failure case_ci_single_run "@SHA@ completed success workflow_dispatch failure" "failed" \
  "$CI7: readiness must be success (failure)"
run_case ci-run-readiness-skipped case_ci_single_run "@SHA@ completed success workflow_dispatch skipped" "" \
  "$CI7: readiness must be success (skipped)"
run_case ci-run-readiness-none case_ci_single_run "@SHA@ completed success workflow_dispatch none" "" \
  "$CI7: readiness none (job without a conclusion) is not success"
run_case ci-view-malformed-four-fields case_ci_single_run "@SHA@ completed success workflow_dispatch" "" \
  "$CI7: a line without exactly five fields is a failure of check 7, never a pass"
run_case ci-view-malformed-six-fields case_ci_single_run "@SHA@ completed success workflow_dispatch success extra" "" \
  "$CI7: a line without exactly five fields is a failure of check 7, never a pass"
run_case ci-view-malformed-empty-output case_ci_single_run "" "" \
  "$CI7: an empty line is not five fields, so it is a failure of check 7"
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

run_case success-both-tags-on-full-head-sha case_success_both_tags
run_case success-no-go-tag case_success_no_go_tag
run_case success-dry-run case_success_dry_run
run_case success-dry-run-no-go-tag case_success_dry_run_no_go_tag
run_case push-rejected-by-remote case_push_rejected_by_remote
run_case push-fails-unreachable-remote case_push_unreachable_remote
run_case push-fails-unreachable-remote-no-go-tag case_push_unreachable_remote --no-go-tag
run_case push-is-atomic case_push_is_atomic
run_case tag-creation-fails case_tag_creation_fails

echo "tag-release tests: $PASSED passed, $FAILED failed"
rm -rf "$WORK" # also cleaned by the EXIT trap; explicit so interpreters that skip EXIT traps leave nothing behind
[ "$FAILED" -eq 0 ]

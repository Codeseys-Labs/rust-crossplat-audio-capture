#!/usr/bin/env bash
#
# check-release-published.test.sh — contract tests for scripts/check-release-published.sh
#
# Written from the release-tooling contract ONLY (section 5 describes the script,
# section 7 describes these tests, revision note r3 amends section 5); the script
# under test was not read while writing them. Every expectation names the contract
# clause it comes from. A failing case stays failing: never weaken an expectation to
# match observed output; fix the script or report the contract problem.
#
# ── What a case gets ────────────────────────────────────────────────────────
#   * a throwaway git repository (branch master, one commit pushed) whose remote
#     `origin` is a bare repository, inside a `mktemp -d` workspace removed on exit
#     (the Go-tag check and the repository lookup are the only parts that talk to git);
#   * fake `gh` and `curl` executables reached only through GH_BIN / CURL_BIN. The
#     fake gh answers the "runs in flight" form of contract section 5 check 2 from
#     files the case writes. Per R3.1 it REJECTS (exit 97, and the case fails) any call
#     that lacks `--repo <owner>/<name>` right after `run list`, or that names another
#     repository than $FAKE_EXPECT_REPO. The harness sets RELEASE_REPO to that
#     repository, because the throwaway remote is a local bare repository, not a
#     GitHub URL. The fake curl prints an HTTP status chosen per URL (default 404).
#     Both log every invocation. Real gh/cargo/curl are shadowed on PATH by "poison"
#     executables, so a script that ignores a seam is caught (and can never reach a
#     real GitHub or registry).
#   * git is isolated from the user's configuration (HOME, system config).
#
# ── Giving a remote a GitHub URL without network (R3.1 cases) ────────────────
#   The script must derive <owner>/<name> from `git remote get-url <remote>`, while
#   ls-remote must keep working against the local bare repository.
#   `url.<local path>.insteadOf` cannot do that: `git remote get-url` EXPANDS
#   insteadOf, so the script would see the local path (verified). Instead the
#   harness sets `remote.<name>.url` to the GitHub-form URL and
#   `remote.<name>.vcs=rsacproxy`; the tiny remote helper `git-remote-rsacproxy`
#   (on PATH, created below) then serves every git transport request from the bare
#   repository named by $RSAC_PROXY_REPO. `git remote get-url` reports the GitHub
#   form, and git can never open a network connection for that remote.
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   bash scripts/tests/check-release-published.test.sh
#
# Running ONE case (or a few): set RSAC_TEST_FILTER to a substring of the case name.
#   RSAC_TEST_FILTER=go-tag-published bash scripts/tests/check-release-published.test.sh
#   List the names (loop-generated ones show as "$wf"/"$s" in the file; run once to see them):
#     grep -o '^run_case [^ ]*' scripts/tests/check-release-published.test.sh
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
SCRIPT_NAME="check-release-published.sh"
SCRIPT="$REPO_ROOT/scripts/$SCRIPT_NAME"

VER="1.2.3"
GO_TAG="bindings/rsac-go/v$VER"
UA="rsac-release-check (https://github.com/Codeseys-Labs/rust-crossplat-audio-capture)"
WORKFLOWS="release.yml release-npm.yml release-pypi.yml"
# §5 check 3: the NPM_PLATFORMS list
NPM_PLATFORMS="win32-x64-msvc darwin-x64 linux-x64-gnu darwin-arm64 linux-arm64-gnu linux-x64-musl linux-arm64-musl linux-arm-gnueabihf"
# Every registry source, named for use in case names: crates, pypi, npm-main, npm-<platform>
SOURCES="crates pypi npm-main"
for p in $NPM_PLATFORMS; do SOURCES="$SOURCES npm-$p"; done
# R3.7: the probe names, in the order of the report lines (15 in total)
PROBE_NAMES="go-tag release.yml release-npm.yml release-pypi.yml crates.io PyPI @rsac/audio"
for p in $NPM_PLATFORMS; do PROBE_NAMES="$PROBE_NAMES @rsac/audio-$p"; done

# The repository every fake-gh call must name (R3.1); the harness exports it as RELEASE_REPO.
TEST_REPO="rsac-test-owner/rsac-test-name"

source_url() { # <source>: the URL contract §5 check 3 names for it
  case "$1" in
    crates) echo "https://crates.io/api/v1/crates/rsac/$VER" ;;
    pypi) echo "https://pypi.org/pypi/rsac/$VER/json" ;;
    npm-main) echo "https://registry.npmjs.org/@rsac%2Faudio/$VER" ;;
    npm-*) echo "https://registry.npmjs.org/@rsac%2Faudio-${1#npm-}/$VER" ;;
  esac
}
source_probe() { # <source>: the R3.7 report name of its probe
  case "$1" in
    crates) echo "crates.io" ;;
    pypi) echo "PyPI" ;;
    npm-main) echo "@rsac/audio" ;;
    npm-*) echo "@rsac/audio-${1#npm-}" ;;
  esac
}

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

# Fake GitHub CLI. Answers ONLY the "runs in flight" form of contract section 5 check 2, as amended by R3.1:
#   run list --repo R --workflow <release.yml|release-npm.yml|release-pypi.yml> --event push ...
# R is $FAKE_EXPECT_REPO. Missing --repo, --repo not right after `run list`, or another repository:
# exit 97 and a line in $FAKE_DIR/gh.unexpected (post_checks then fails the case).
# stdout from $FAKE_DIR/gh-inflight-<workflow>.out (default `0`), status from
# $FAKE_DIR/gh-inflight-<workflow>.rc (default 0). The --jq expression is ignored: the
# case supplies the output the expression would print. Every call is appended to
# $FAKE_DIR/gh.log.
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
if [ "${1:-}" = run ] && [ "${2:-}" = list ]; then
  [ "${3:-}" = --repo ] || unexpected "no --repo right after 'run list'"
  [ "${4:-}" = "$want_repo" ] || unexpected "--repo is '${4:-}', expected '$want_repo'"
  wf=""
  prev=""
  for x in "$@"; do
    [ "$prev" = --workflow ] && wf="$x"
    prev="$x"
  done
  case "$wf" in
    release.yml | release-npm.yml | release-pypi.yml) ;;
    *) unexpected "workflow '$wf' is not one of the three release workflows" ;;
  esac
  if [ ! -f "$d/gh-inflight-$wf.out" ] && [ ! -f "$d/gh-inflight-$wf.rc" ]; then
    echo 0
    exit 0
  fi
  rc=0
  [ -f "$d/gh-inflight-$wf.out" ] && cat "$d/gh-inflight-$wf.out"
  [ -f "$d/gh-inflight-$wf.rc" ] && rc="$(cat "$d/gh-inflight-$wf.rc")"
  [ "$rc" -ne 0 ] && echo "fake gh: failing on request ($wf)" >&2
  exit "$rc"
fi
unexpected "not the contract form"
EOF

# Fake curl. The URL is the last argument. $FAKE_DIR/curl.map holds `<url> <code>` lines:
#   <code>  prints the code (no newline, like curl -w '%{http_code}') and exits 0
#   FAIL    prints 000 and exits 7 (connection failure)
#   NOOUT   prints nothing and exits 0
# URLs absent from the map answer 404. Every call is appended to $FAKE_DIR/curl.log.
cat > "$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
a="$*"
printf '%s\n' "$a" >> "$d/curl.log"
url="${!#}"
code=""
if [ -f "$d/curl.map" ]; then
  while read -r u c; do
    if [ "$u" = "$url" ]; then
      code="$c"
      break
    fi
  done < "$d/curl.map"
fi
[ -n "$code" ] || code=404
case "$code" in
  FAIL)
    echo "curl: (7) fake connection failure" >&2
    printf '000'
    exit 7
    ;;
  NOOUT) exit 0 ;;
  *)
    printf '%s' "$code"
    exit 0
    ;;
esac
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
export GH_BIN="$WORK/bin/gh" CURL_BIN="$WORK/bin/curl"

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
  local t="$TEMPLATE" kv
  mkdir -p "$t/repo" || harness_die "mkdir"
  git init -q --bare "$t/remote.git" || harness_die "git init --bare"
  git -C "$t/remote.git" symbolic-ref HEAD refs/heads/master || harness_die "bare HEAD"
  git init -q "$t/repo" || harness_die "git init"
  git -C "$t/repo" symbolic-ref HEAD refs/heads/master || harness_die "repo HEAD"
  for kv in user.name=rsac-test user.email=rsac-test@example.invalid commit.gpgsign=false \
            tag.gpgsign=false tag.forceSignAnnotated=false core.autocrlf=false; do
    git -C "$t/repo" config "${kv%%=*}" "${kv#*=}" || harness_die "git config $kv"
  done
  printf '# rsac test repository\n' > "$t/repo/README.md"
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
  RC=""
  RSAC_PROXY_REPO="$REMOTE"
  FAKE_EXPECT_REPO="$TEST_REPO"
  RELEASE_REPO="$TEST_REPO" # R3.1 seam: the throwaway remote is a local path, not a GitHub URL
  export RSAC_PROXY_REPO FAKE_EXPECT_REPO RELEASE_REPO
}

# ── arranging a case ────────────────────────────────────────────────────────
curl_set() { printf '%s %s\n' "$(source_url "$1")" "$2" >> "$FAKE_DIR/curl.map"; } # <source> <code|FAIL|NOOUT>
inflight_set() { printf '%s\n' "$2" > "$FAKE_DIR/gh-inflight-$1.out"; }          # <workflow> <stdout line>
inflight_empty() { : > "$FAKE_DIR/gh-inflight-$1.out"; }                          # <workflow>
inflight_fail() { printf '1\n' > "$FAKE_DIR/gh-inflight-$1.rc"; }                 # <workflow>
add_upstream() { # a second bare remote named `upstream`
  must git init -q --bare "$UPSTREAM" || return 1
  must git -C "$REPO" remote add upstream "$UPSTREAM"
}
push_tag() { # <remote> <tag>: an annotated tag at HEAD, pushed to <remote>, then removed locally
  must git -C "$REPO" tag -a "$2" -m "test tag" || return 1
  must git -C "$REPO" push -q "$1" "refs/tags/$2" || return 1
  must git -C "$REPO" tag -d "$2"
}
use_remote_url() { # <remote> <url> <bare repository serving it>: GitHub-form URL, transport still local
  must git -C "$REPO" remote set-url "$1" "$2" || return 1
  must git -C "$REPO" config "remote.$1.vcs" rsacproxy || return 1
  RSAC_PROXY_REPO="$3"
  export RSAC_PROXY_REPO
}

# ── running the script under test ───────────────────────────────────────────
run_crp() { # <args...>: run scripts/check-release-published.sh from inside the throwaway repository
  ( cd "$REPO" && bash "$SCRIPT" "$@" ) > "$SB/stdout" 2> "$SB/stderr" < /dev/null
  RC=$?
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
assert_eq() { # <actual> <expected> <what>
  [ "$1" = "$2" ] || fail "$3: got '$1', expected '$2'"
}
assert_stderr_prefixed() { # <clause>
  grep -q '^check-release-published: ' "$SB/stderr" || fail "no stderr line starts with 'check-release-published: ' [$1]"
}
assert_stdout_unprefixed() { # <clause>
  if grep -q '^check-release-published: ' "$SB/stdout"; then fail "a 'check-release-published: ' message went to stdout [$1]"; fi
}
assert_no_verdict() { # <clause>: no verdict line on a usage error
  if grep -qE '^(safe-to-retag|published|indeterminate) ' "$SB/stdout"; then
    fail "a verdict line was printed although the arguments were rejected [$1]"
  fi
}
assert_no_calls() { # <clause> <gh|curl>...: the tool was never invoked
  local clause="$1" t
  shift
  for t in "$@"; do
    if [ -s "$FAKE_DIR/$t.log" ]; then
      fail "$t was invoked: $(tr '\n' '|' < "$FAKE_DIR/$t.log") [$clause]"
    fi
  done
}

# R3.7: "Exactly one stderr line per probe, `check-release-published: <name> <state>`, with these names in this
# order ... (15 lines in total). <state> is `clean`, `published` or `indeterminate`."
# AMBIGUITY: what counts as a report line. Chosen reading: a stderr line of exactly the shape
# `check-release-published: <one word> <state word>`; so ANY such line besides the 15 probe lines (a summary
# line in that shape, a duplicate) fails the case, and other messages (longer, or not ending in a state word) are free.
assert_report() { # <default state> [<probe>=<state>]...: the 15 probe lines, in order, with these states
  local default="$1" name state kv got want
  shift
  : > "$SB/report.want"
  for name in $PROBE_NAMES; do
    state="$default"
    for kv in "$@"; do
      [ "${kv%%=*}" = "$name" ] && state="${kv#*=}"
    done
    printf '%s %s\n' "$name" "$state" >> "$SB/report.want"
  done
  tr -d '\r' < "$SB/stderr" | grep -E '^check-release-published: [^ ]+ (clean|published|indeterminate)$' \
    | sed 's/^check-release-published: //' > "$SB/report.got"
  if ! cmp -s "$SB/report.want" "$SB/report.got"; then
    got="$(wc -l < "$SB/report.got" | tr -d ' ')"
    want="$(diff "$SB/report.want" "$SB/report.got" | grep '^[<>]' | head -n 8 | tr '\n' '|')"
    fail "probe report lines differ (expected 15, saw $got) [R3.7]; diff expected(<) vs actual(>): $want"
  fi
}

expect_verdict() { # <exit code> <verdict word> <clause>
  assert_rc "$1" "$3"
  assert_last_stdout "$2 $VER" "$3"
  assert_stderr_prefixed "$3"
  assert_stdout_unprefixed "$3"
}
expect_clean() { expect_verdict 0 safe-to-retag "$1"; }         # §5: exit 0, last stdout line safe-to-retag X.Y.Z
expect_published() { expect_verdict 1 published "$1"; }         # §5: exit 1, last stdout line published X.Y.Z
expect_indeterminate() { expect_verdict 3 indeterminate "$1"; } # §5: exit 3, last stdout line indeterminate X.Y.Z

assert_all_urls_requested() { # <clause>: every URL of §5 check 3 was the target of a curl call
  local s url
  for s in $SOURCES; do
    url="$(source_url "$s")"
    if ! awk -v u="$url" '$NF == u { found = 1 } END { exit !found }' "$FAKE_DIR/curl.log" 2>/dev/null; then
      fail "$url was never requested [$1]"
    fi
  done
}
inflight_form() { # <workflow> [repo]: the §5 check 2 invocation as logged (shell quoting removed), with R3.1 --repo
  echo "run list --repo ${2:-$TEST_REPO} --workflow $1 --event push --branch v$VER --limit 20 --json status --jq [.[] | select(.status != \"completed\")] | length"
}
assert_all_workflows_queried() { # <clause> [repo]
  local wf
  for wf in $WORKFLOWS; do
    grep -qxF -- "$(inflight_form "$wf" "${2:-}")" "$FAKE_DIR/gh.log" 2>/dev/null \
      || fail "gh was never asked about $wf in the §5 check 2 form with --repo ${2:-$TEST_REPO} [$1]"
  done
}
assert_all_checks_ran() { # <clause>: §5 "all of them are run and reported"
  assert_all_urls_requested "$1"
  assert_all_workflows_queried "$1"
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

post_checks() { # run after every case: no seam bypassed, gh stayed inside the contract form
  if [ -s "$FAKE_DIR/poison.log" ]; then
    fail "the script ran a real tool instead of the seam: $(tr '\n' '|' < "$FAKE_DIR/poison.log") [§5 GH_BIN/CURL_BIN seams, §7 no network]"
  fi
  if [ -s "$FAKE_DIR/gh.unexpected" ]; then
    fail "gh invoked outside the form of §5 check 2 / R3.1: $(tr '\n' '|' < "$FAKE_DIR/gh.unexpected")"
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

# ── exit 0: nothing published, nothing in flight ───────────────────────────
case_clean() {
  local wf
  for wf in $WORKFLOWS; do inflight_set "$wf" 0; done
  run_crp "$VER"
  expect_clean "§5 exit 0: no Go tag, no run in flight (count 0), every registry 404"
  # R3.7: exactly 15 probe lines, names and order as listed, every state `clean` (404 = clean, count 0 = clean)
  assert_report clean
  assert_all_checks_ran "§5 all checks are run (clean)"
}
case_crate_tag_alone_is_not_published() {
  # §5 check 1 looks only at refs/tags/bindings/rsac-go/vX.Y.Z; the crate tag is the one being recreated.
  push_tag origin "v$VER"
  run_crp "$VER"
  expect_clean "§5 check 1: only the Go tag counts; the crate tag vX.Y.Z is irrelevant"
  assert_report clean
}
case_go_tag_other_versions_are_clean() {
  push_tag origin "bindings/rsac-go/v${VER}0"
  push_tag origin "bindings/rsac-go/v1.2.2"
  run_crp "$VER"
  expect_clean "§5 check 1: the ref is exact (refs/tags/bindings/rsac-go/vX.Y.Z)"
  assert_report clean
}

# ── check 1: Go tag ────────────────────────────────────────────────────────
case_go_tag_published() {
  push_tag origin "$GO_TAG"
  run_crp "$VER"
  expect_published "§5 check 1: ls-remote prints a ref → published; §5 exit 1"
  assert_report clean go-tag=published # R3.7: the state is on the go-tag line
}
case_go_tag_remote_flag_published() {
  add_upstream
  push_tag upstream "$GO_TAG"
  run_crp "$VER" --remote upstream
  expect_published "§5 usage: --remote NAME selects the remote asked for the Go tag"
  assert_report clean go-tag=published
}
case_go_tag_other_remote_ignored() {
  add_upstream
  push_tag origin "$GO_TAG"
  run_crp "$VER" --remote upstream
  expect_clean "§5 usage: with --remote upstream the Go tag on origin is not consulted"
  assert_report clean
}
case_go_tag_ls_remote_fails() {
  run_crp "$VER" --remote no-such-remote
  expect_indeterminate "§5 check 1: git ls-remote failing → indeterminate; §5 exit 3"
  assert_report clean go-tag=indeterminate # R3.7: RELEASE_REPO is set, so the other probes are unaffected
  assert_all_checks_ran "§5 all checks are run even when check 1 is indeterminate"
}

# ── check 2: runs in flight ────────────────────────────────────────────────
case_inflight() { # <workflow> <stdout line> <clause>
  inflight_set "$1" "$2"
  run_crp "$VER"
  expect_indeterminate "$3"
  assert_report clean "$1=indeterminate" # R3.7: the state is on that workflow's own line
}
case_inflight_empty_stdout() { # <workflow>
  inflight_empty "$1"
  run_crp "$VER"
  expect_indeterminate "§5 check 2: empty stdout is not one integer → indeterminate ($1)"
  assert_report clean "$1=indeterminate"
}
case_inflight_cli_fails() { # <workflow>
  inflight_fail "$1"
  run_crp "$VER"
  expect_indeterminate "§5 check 2: a failing gh command → indeterminate ($1)"
  assert_report clean "$1=indeterminate"
}

# ── check 3: registries ────────────────────────────────────────────────────
case_registry() { # <source> <code|FAIL|NOOUT> <verdict: published|indeterminate> <clause>
  curl_set "$1" "$2"
  run_crp "$VER"
  if [ "$3" = published ]; then expect_published "$4"; else expect_indeterminate "$4"; fi
  assert_report clean "$(source_probe "$1")=$3" # R3.7: the state is on that registry's own line
}

# ── worst result decides ───────────────────────────────────────────────────
case_published_beats_indeterminate_go_vs_curl() {
  push_tag origin "$GO_TAG"
  curl_set crates 500
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (Go tag published, crates.io 500)"
  assert_report clean go-tag=published crates.io=indeterminate
}
case_published_beats_indeterminate_registry_vs_gh() {
  curl_set npm-main 200
  inflight_fail release.yml
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (npm main 200, gh failing)"
  assert_report clean @rsac/audio=published release.yml=indeterminate
}
case_published_beats_indeterminate_platform_vs_curl() {
  curl_set npm-linux-arm64-musl 200
  curl_set pypi FAIL
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (npm platform 200, PyPI curl failing)"
  assert_report clean @rsac/audio-linux-arm64-musl=published PyPI=indeterminate
}
case_published_beats_indeterminate_registry_vs_ls_remote() {
  curl_set crates 200
  run_crp "$VER" --remote no-such-remote
  expect_published "§5: published beats indeterminate (crates.io 200, ls-remote failing)"
  assert_report clean crates.io=published go-tag=indeterminate
}
case_indeterminate_beats_clean() {
  inflight_set release-npm.yml 1
  curl_set pypi 404
  run_crp "$VER"
  expect_indeterminate "§5: indeterminate beats clean (one run in flight, the rest clean)"
  assert_report clean release-npm.yml=indeterminate
}
case_all_checks_run_when_published() {
  push_tag origin "$GO_TAG"
  run_crp "$VER"
  expect_published "§5 exit 1"
  assert_all_checks_ran "§5: all checks are run and reported even after one says published"
  assert_report clean go-tag=published
}
case_report_states_on_their_probe_lines() {
  push_tag origin "$GO_TAG"                  # go-tag published
  inflight_set release-npm.yml 2             # release-npm.yml indeterminate
  curl_set pypi 200                          # PyPI published
  curl_set npm-darwin-arm64 FAIL             # @rsac/audio-darwin-arm64 indeterminate
  curl_set npm-win32-x64-msvc 200            # @rsac/audio-win32-x64-msvc published
  run_crp "$VER"
  expect_published "§5 exit 1"
  # R3.7: every state sits on its own probe's line; everything else stays clean
  assert_report clean go-tag=published release-npm.yml=indeterminate PyPI=published \
    @rsac/audio-darwin-arm64=indeterminate @rsac/audio-win32-x64-msvc=published
}
case_report_has_exactly_15_lines() {
  run_crp "$VER"
  expect_clean "§5 exit 0"
  # R3.7: "(15 lines in total)"
  assert_eq "$(tr -d '\r' < "$SB/stderr" | grep -cE '^check-release-published: [^ ]+ (clean|published|indeterminate)$')" 15 \
    "number of '<name> <state>' report lines on stderr [R3.7]"
  assert_report clean
}

# ── invocation forms ───────────────────────────────────────────────────────
case_curl_invocation_form() {
  run_crp "$VER"
  expect_clean "§5 check 3"
  local s url prefix="-sS -o /dev/null -w %{http_code} --max-time 20 -A $UA"
  for s in $SOURCES; do
    url="$(source_url "$s")"
    # §5 check 3 as amended by R3.6: "$CURL_BIN" -sS -o /dev/null -w '%{http_code}' --max-time 20 -A '<agent>' <url>
    grep -qxF -- "$prefix $url" "$FAKE_DIR/curl.log" 2>/dev/null \
      || fail "curl was not invoked as '$prefix $url' [§5 check 3 invocation form, R3.6]"
  done
}
case_curl_every_call_has_max_time() {
  curl_set crates 500
  curl_set pypi FAIL
  curl_set npm-main 200
  run_crp "$VER"
  assert_rc 1 "§5 exit 1 (npm main published)"
  # R3.6: every curl call carries `--max-time 20`, whatever the answer was
  assert_eq "$(wc -l < "$FAKE_DIR/curl.log" | tr -d ' ')" 11 "number of curl calls [§5 check 3: 11 URLs]"
  assert_eq "$(grep -cF -- ' --max-time 20 ' "$FAKE_DIR/curl.log")" 11 "curl calls carrying '--max-time 20' [R3.6]"
}
case_gh_invocation_form() {
  run_crp "$VER"
  expect_clean "§5 check 2"
  assert_all_workflows_queried "§5 check 2 invocation form (R3.1 --repo right after 'run list') for release.yml, release-npm.yml and release-pypi.yml"
  assert_eq "$(wc -l < "$FAKE_DIR/gh.log" | tr -d ' ')" 3 "number of gh calls [§5 check 2: one per workflow]"
}

# ── R3.1: repository binding (RELEASE_REPO, or parsed from the remote URL) ───
case_repo_unset_local_remote() { # <unset|empty>
  if [ "$1" = unset ]; then unset RELEASE_REPO; else RELEASE_REPO=""; export RELEASE_REPO; fi
  run_crp "$VER"
  # R3.1: a URL that is not an accepted GitHub form: the three workflow probes are indeterminate
  expect_indeterminate "R3.1: RELEASE_REPO $1 and a local-path remote URL: the workflow probes are indeterminate"
  assert_report clean release.yml=indeterminate release-npm.yml=indeterminate release-pypi.yml=indeterminate
  # (No gh call is possible here without a repository; the fake gh rejects one, and post_checks fails the case.)
}
case_repo_from_url() { # <remote url> <expected owner/name>
  unset RELEASE_REPO # R3.1: otherwise REPO comes from `git remote get-url <remote>`
  FAKE_EXPECT_REPO="$2"
  export FAKE_EXPECT_REPO
  use_remote_url origin "$1" "$REMOTE" || return 0
  run_crp "$VER"
  expect_clean "R3.1: $1 yields --repo $2"
  assert_report clean
  assert_gh_calls_name_repo "$2" "R3.1: every GitHub CLI call passes --repo $2 (from $1)"
  assert_all_workflows_queried "R3.1: the three probes ask about $2" "$2"
}
case_repo_url_rejected() { # <remote url>
  unset RELEASE_REPO
  FAKE_EXPECT_REPO="o/n"
  export FAKE_EXPECT_REPO
  use_remote_url origin "$1" "$REMOTE" || return 0
  run_crp "$VER"
  expect_indeterminate "R3.1: '$1' is not one of the accepted GitHub URL forms: the workflow probes are indeterminate"
  assert_report clean release.yml=indeterminate release-npm.yml=indeterminate release-pypi.yml=indeterminate
}
case_repo_env_overrides_url() {
  RELEASE_REPO="x/y" # R3.1: a non-empty RELEASE_REPO is used as REPO
  FAKE_EXPECT_REPO="x/y"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  run_crp "$VER"
  expect_clean "R3.1: RELEASE_REPO wins over the remote URL"
  assert_report clean
  assert_gh_calls_name_repo "x/y" "R3.1: --repo is RELEASE_REPO, not the URL's o/n"
}
case_repo_empty_env_uses_url() {
  RELEASE_REPO="" # R3.1: "set and non-empty": an empty value is ignored
  FAKE_EXPECT_REPO="o/n"
  export RELEASE_REPO FAKE_EXPECT_REPO
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  run_crp "$VER"
  expect_clean "R3.1: an empty RELEASE_REPO falls back to the remote URL"
  assert_report clean
  assert_gh_calls_name_repo "o/n" "R3.1: --repo comes from the URL"
}
case_repo_from_chosen_remote() {
  unset RELEASE_REPO
  FAKE_EXPECT_REPO="up/stream"
  export FAKE_EXPECT_REPO
  add_upstream || return 0
  push_tag upstream "$GO_TAG" || return 0
  use_remote_url upstream "git@github.com:up/stream.git" "$UPSTREAM" || return 0
  run_crp "$VER" --remote upstream # origin keeps its local-path URL, which would not parse
  expect_published "R3.1: the Go tag is looked up on the chosen remote"
  assert_report clean go-tag=published
  assert_gh_calls_name_repo "up/stream" "R3.1: --repo is parsed from the URL of the remote chosen with --remote"
}

# ── usage: exit 2 ──────────────────────────────────────────────────────────
case_usage() { # <clause> <args...>
  local clause="$1"
  shift
  run_crp "$@"
  assert_rc 2 "$clause"
  assert_stderr_prefixed "$clause"
  assert_no_verdict "$clause"
  assert_no_calls "$clause (rejected arguments precede every check)" gh curl
}

# ── --self-test ────────────────────────────────────────────────────────────
case_self_test_real_package_json() {
  # §5 check 4 / §7: run from the real repository root against the real bindings/rsac-napi/package.json
  ( cd "$REPO_ROOT" && bash "$SCRIPT" --self-test ) > "$SB/stdout" 2> "$SB/stderr" < /dev/null
  RC=$?
  assert_rc 0 "§5 --self-test passes on the real bindings/rsac-napi/package.json"
  assert_no_calls "§5 --self-test needs no network" gh curl
}
stage_self_test() { # <package.json source>: a throwaway repository root with a copy of the script
  mkdir -p "$REPO/scripts" "$REPO/bindings/rsac-napi" || return 1
  cp "$SCRIPT" "$REPO/scripts/$SCRIPT_NAME" || return 1
  cp "$1" "$REPO/bindings/rsac-napi/package.json"
}
run_self_test() {
  ( cd "$REPO" && bash "scripts/$SCRIPT_NAME" --self-test ) > "$SB/stdout" 2> "$SB/stderr" < /dev/null
  RC=$?
}
case_self_test_copy_baseline() {
  # Control for the cases below: the same staging with the unmodified manifest passes.
  must stage_self_test "$REPO_ROOT/bindings/rsac-napi/package.json" || return 0
  run_self_test
  assert_rc 0 "§5 --self-test passes on an unmodified copy of bindings/rsac-napi/package.json"
  assert_no_calls "§5 --self-test needs no network" gh curl
}
case_self_test_unknown_triple_fails() {
  # Add one triple that cannot have a row in any triple-to-platform table.
  awk '{ print } /"additional": \[/ { print "        \"riscv64gc-unknown-linux-gnu\"," }' \
    "$REPO_ROOT/bindings/rsac-napi/package.json" > "$SB/package.json"
  must stage_self_test "$SB/package.json" || return 0
  run_self_test
  # §5 check 4: "A triple with no row is a failure". AMBIGUITY: the failing exit code is unspecified;
  # chosen reading: any non-zero status.
  [ "$RC" != 0 ] || fail "--self-test exited 0 although package.json names riscv64gc-unknown-linux-gnu [§5 check 4: a triple with no row is a failure]"
  assert_stderr_prefixed "§5 messages on stderr prefixed"
  assert_no_calls "§5 --self-test needs no network" gh curl
}
case_self_test_subset_of_triples_passes() {
  # napi.triples.additional reduced to one entry (the array layout is kept): the three napi-rs defaults
  # plus aarch64-apple-darwin. Fewer triples than NPM_PLATFORMS is fine: each named triple has a row (§5 check 4).
  awk '/"additional": \[/ { print; print "        \"aarch64-apple-darwin\""; skip = 1; next }
       skip { if ($0 ~ /\]/) { print; skip = 0 } ; next }
       { print }' "$REPO_ROOT/bindings/rsac-napi/package.json" > "$SB/package.json"
  must stage_self_test "$SB/package.json" || return 0
  run_self_test
  assert_rc 0 "§5 check 4: every triple named in package.json has a row in a listed platform (a subset passes)"
  assert_no_calls "§5 --self-test needs no network" gh curl
}
case_self_test_second_napi_block() { # <second|first>: where the triple without a row sits
  # Adversarial-review regression. A manifest with a SECOND "napi": block must fail --self-test. The unknown
  # triple sits in the second block (a parser that stops at the first block misses it) or, in the other case,
  # in the first block while the second block names only known triples (a parser that keeps only the last
  # block misses it). Under both readings of the contract (every triple named in package.json, or a
  # duplicate napi block is itself an error) the self-test must fail.
  local second_block first_extra
  # awk -v values use \n escapes (a literal newline in -v is not portable to every awk)
  if [ "$1" = second ]; then
    first_extra=""
    second_block='  "napi": {\n    "triples": {\n      "additional": [\n        "riscv64gc-unknown-linux-gnu"\n      ]\n    }\n  },'
  else
    first_extra='        "riscv64gc-unknown-linux-gnu",'
    second_block='  "napi": {\n    "triples": {\n      "additional": [\n        "aarch64-apple-darwin"\n      ]\n    }\n  },'
  fi
  awk -v block="$second_block" -v extra="$first_extra" '
    /"additional": \[/ { print; if (extra != "") print extra; next }
    /^  "license"/ { print block }
    { print }' "$REPO_ROOT/bindings/rsac-napi/package.json" > "$SB/package.json"
  [ "$(grep -c '"napi":' "$SB/package.json")" = 2 ] || fail "harness: expected two napi blocks in the staged package.json"
  must stage_self_test "$SB/package.json" || return 0
  run_self_test
  [ "$RC" != 0 ] || fail "--self-test exited 0 on a manifest with a second \"napi\": block [§5 check 4 / adversarial review]"
  assert_stderr_prefixed "§5 messages on stderr prefixed"
  assert_no_calls "§5 --self-test needs no network" gh curl
}

# ── harness self-tests (no script under test) ──────────────────────────────
case_harness_fake_gh_demands_repo() {
  # R3.1 / §7: the fake gh must reject a call without --repo, with --repo in the wrong place, or with
  # another repository, and accept the contract form with the right one.
  local d="$WORK/harness-fake"
  rm -rf "$d"
  mkdir -p "$d"
  FAKE_DIR="$d"
  FAKE_EXPECT_REPO="$TEST_REPO"
  export FAKE_DIR FAKE_EXPECT_REPO
  gh_status() { "$GH_BIN" "$@" > /dev/null 2>&1; echo $?; }
  assert_eq "$(gh_status run list --repo "$TEST_REPO" --workflow release.yml)" 0 "fake gh accepts 'run list --repo R'"
  assert_eq "$(gh_status run list --workflow release.yml)" 97 "fake gh rejects 'run list' without --repo"
  assert_eq "$(gh_status run list --workflow release.yml --repo "$TEST_REPO")" 97 "fake gh rejects --repo not right after 'run list'"
  assert_eq "$(gh_status run list --repo other/repo --workflow release.yml)" 97 "fake gh rejects another repository"
  assert_eq "$(wc -l < "$d/gh.unexpected" | tr -d ' ')" 3 "rejected calls recorded in gh.unexpected"
}
case_harness_remote_helper() {
  # A remote with a GitHub-form URL and remote.<name>.vcs=rsacproxy reports that URL and still
  # answers ls-remote from the local bare repository (no network).
  new_sandbox
  push_tag origin "$GO_TAG" || return 0
  use_remote_url origin "https://github.com/o/n.git" "$REMOTE" || return 0
  assert_eq "$(git -C "$REPO" remote get-url origin)" "https://github.com/o/n.git" "git remote get-url reports the GitHub form"
  assert_eq "$(git -C "$REPO" ls-remote --tags origin "refs/tags/$GO_TAG" | wc -l | tr -d ' ')" 1 "tag listed through the helper"
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
run_case clean-nothing-published case_clean
run_case clean-crate-tag-alone-is-not-published case_crate_tag_alone_is_not_published
run_case clean-go-tag-of-other-versions case_go_tag_other_versions_are_clean

run_case go-tag-published case_go_tag_published
run_case go-tag-remote-flag-published case_go_tag_remote_flag_published
run_case go-tag-other-remote-ignored case_go_tag_other_remote_ignored
run_case go-tag-ls-remote-fails case_go_tag_ls_remote_fails

for wf in $WORKFLOWS; do
  run_case "inflight-run-$wf" case_inflight "$wf" 1 "§5 check 2: count 1 > 0 → indeterminate ($wf)"
  run_case "inflight-non-integer-$wf" case_inflight "$wf" abc "§5 check 2: a non-integer count → indeterminate ($wf)"
  run_case "inflight-cli-fails-$wf" case_inflight_cli_fails "$wf"
done
run_case inflight-count-multi-digit case_inflight release.yml 12 "§5 check 2: 12 is greater than zero → indeterminate"
run_case inflight-count-decimal case_inflight release-pypi.yml 1.5 "§5 check 2: 1.5 is not an integer → indeterminate"
run_case inflight-empty-stdout case_inflight_empty_stdout release-npm.yml

for s in $SOURCES; do
  run_case "published-$s" case_registry "$s" 200 published "§5 check 3: HTTP 200 from the $s URL = published"
done
for s in $SOURCES; do
  run_case "status-500-$s" case_registry "$s" 500 indeterminate "§5 check 3: any status other than 200/404 = indeterminate (500, $s)"
done
for s in $SOURCES; do
  run_case "curl-fails-$s" case_registry "$s" FAIL indeterminate "§5 check 3: a failing curl command = indeterminate ($s)"
done
run_case status-201-is-not-published case_registry crates 201 indeterminate "§5 check 3: only 200 is published; 201 is 'anything else'"
run_case status-301-is-indeterminate case_registry pypi 301 indeterminate "§5 check 3: 301 is 'anything else'"
run_case status-403-is-indeterminate case_registry npm-main 403 indeterminate "§5 check 3: 403 (rate limit, block) is 'anything else'"
run_case status-000-is-indeterminate case_registry crates 000 indeterminate "§5 check 3: 000 is 'anything else'"
run_case curl-empty-stdout-is-indeterminate case_registry pypi NOOUT indeterminate "§5 check 3: no status printed is 'anything else'"

run_case published-beats-indeterminate-go-tag-vs-curl case_published_beats_indeterminate_go_vs_curl
run_case published-beats-indeterminate-registry-vs-gh case_published_beats_indeterminate_registry_vs_gh
run_case published-beats-indeterminate-platform-vs-curl case_published_beats_indeterminate_platform_vs_curl
run_case published-beats-indeterminate-registry-vs-ls-remote case_published_beats_indeterminate_registry_vs_ls_remote
run_case indeterminate-beats-clean case_indeterminate_beats_clean
run_case all-checks-run-when-published case_all_checks_run_when_published

# R3.7
run_case report-has-exactly-15-lines case_report_has_exactly_15_lines
run_case report-states-on-their-probe-lines case_report_states_on_their_probe_lines

# R3.6 and the invocation forms
run_case curl-invocation-form case_curl_invocation_form
run_case curl-every-call-has-max-time case_curl_every_call_has_max_time
run_case gh-invocation-form case_gh_invocation_form

# R3.1
run_case repo-unset-local-path-remote case_repo_unset_local_remote unset
run_case repo-empty-local-path-remote case_repo_unset_local_remote empty
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

U="§5 usage: exit 2"
run_case usage-no-version case_usage "$U (a version is required)"
run_case usage-version-two-parts case_usage "$U (1.2 is not X.Y.Z)" "1.2"
run_case usage-version-v-prefix case_usage "$U (v1.2.3 is not X.Y.Z)" "v$VER"
run_case usage-two-versions case_usage "$U (exactly one version)" "$VER" "1.2.4"
run_case usage-unknown-flag case_usage "$U (unknown flag)" "$VER" --bogus
run_case usage-remote-missing-value case_usage "$U (--remote needs a value)" "$VER" --remote

run_case self-test-real-package-json case_self_test_real_package_json
run_case self-test-copy-baseline case_self_test_copy_baseline
run_case self-test-unknown-triple-fails case_self_test_unknown_triple_fails
run_case self-test-subset-of-triples-passes case_self_test_subset_of_triples_passes
run_case self-test-second-napi-block-fails case_self_test_second_napi_block second
run_case self-test-second-napi-block-masks-first-fails case_self_test_second_napi_block first

run_harness_case harness-fake-gh-demands-repo case_harness_fake_gh_demands_repo
run_harness_case harness-remote-helper-serves-github-url case_harness_remote_helper
run_harness_case harness-unmatched-filter-fails case_harness_unmatched_filter

if [ -n "${RSAC_TEST_FILTER:-}" ] && [ "$SELECTED" -eq 0 ]; then
  echo "FAIL no case name contains RSAC_TEST_FILTER=$RSAC_TEST_FILTER"
  FAILED=$((FAILED + 1))
fi
echo "check-release-published tests: $PASSED passed, $FAILED failed"
rm -rf "$WORK" # also cleaned by the EXIT trap; explicit so interpreters that skip EXIT traps leave nothing behind
[ "$FAILED" -eq 0 ]

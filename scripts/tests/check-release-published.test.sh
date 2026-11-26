#!/usr/bin/env bash
#
# check-release-published.test.sh — contract tests for scripts/check-release-published.sh
#
# Written from the release-tooling contract ONLY (section 5 describes the script,
# section 7 describes these tests); the script under test was not read while writing
# them. Every expectation names the contract clause it comes from. A failing case
# stays failing: never weaken an expectation to match observed output; fix the script
# or report the contract problem.
#
# ── What a case gets ────────────────────────────────────────────────────────
#   * a throwaway git repository (branch master, one commit pushed) whose remote
#     `origin` is a bare repository, inside a `mktemp -d` workspace removed on exit
#     (the Go-tag check is the only one that talks to git);
#   * fake `gh` and `curl` executables reached only through GH_BIN / CURL_BIN. The
#     fake gh answers the "runs in flight" form of contract section 5 check 2 from
#     files the case writes; the fake curl prints an HTTP status chosen per URL
#     (default 404). Both log every invocation. Real gh/cargo/curl are shadowed on
#     PATH by "poison" executables, so a script that ignores a seam is caught (and
#     can never reach a real GitHub or registry).
#   * git is isolated from the user's configuration (HOME, system config).
#
# ── Usage ────────────────────────────────────────────────────────────────────
#   bash scripts/tests/check-release-published.test.sh
#
# Output: one line per case (`ok <name>` / `FAIL <name>: <why>`), then a summary
# line. Exit status: 0 = every case passed, 1 = at least one case failed. A case
# whose script under test does not exist FAILs (it is never skipped).
# RSAC_TEST_FILTER=<substring> runs only the cases whose name contains it.
#
# Portability: bash 3.2 compatible; no jq, python or node; runs under Git Bash.

set -uo pipefail

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

source_url() { # <source>: the URL contract §5 check 3 names for it
  case "$1" in
    crates) echo "https://crates.io/api/v1/crates/rsac/$VER" ;;
    pypi) echo "https://pypi.org/pypi/rsac/$VER/json" ;;
    npm-main) echo "https://registry.npmjs.org/@rsac%2Faudio/$VER" ;;
    npm-*) echo "https://registry.npmjs.org/@rsac%2Faudio-${1#npm-}/$VER" ;;
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
mkdir -p "$WORK/bin" "$WORK/poison"

# Fake GitHub CLI. Answers ONLY the "runs in flight" form of contract section 5 check 2:
#   run list --workflow <release.yml|release-npm.yml|release-pypi.yml> --event push ...
# stdout from $FAKE_DIR/gh-inflight-<workflow>.out (default `0`), status from
# $FAKE_DIR/gh-inflight-<workflow>.rc (default 0). The --jq expression is ignored: the
# case supplies the output the expression would print. Every call is appended to
# $FAKE_DIR/gh.log; anything else is logged to gh.unexpected.
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
d="${FAKE_DIR:?FAKE_DIR not set}"
a="$*"
a="${a//$'\n'/ }"
printf '%s\n' "$a" >> "$d/gh.log"
unexpected() {
  printf '%s\n' "$a" >> "$d/gh.unexpected"
  echo "fake gh: invocation outside the contract form: $a" >&2
  exit 97
}
if [ "${1:-}" = run ] && [ "${2:-}" = list ]; then
  wf=""
  prev=""
  for x in "$@"; do
    [ "$prev" = --workflow ] && wf="$x"
    prev="$x"
  done
  case "$wf" in
    release.yml | release-npm.yml | release-pypi.yml) ;;
    *) unexpected ;;
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
unexpected
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
export GH_BIN="$WORK/bin/gh" CURL_BIN="$WORK/bin/curl"

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
state_lines() { # <state>: stderr lines `<prefix> <name> <state>` ending in the state word
  grep -c "^check-release-published: .* $1\$" "$SB/stderr"
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
inflight_form() { # <workflow>: the §5 check 2 invocation as logged (shell quoting removed)
  echo "run list --workflow $1 --event push --branch v$VER --limit 20 --json status --jq [.[] | select(.status != \"completed\")] | length"
}
assert_all_workflows_queried() { # <clause>
  local wf
  for wf in $WORKFLOWS; do
    grep -qxF -- "$(inflight_form "$wf")" "$FAKE_DIR/gh.log" 2>/dev/null \
      || fail "gh was never asked about $wf in the §5 check 2 form [$1]"
  done
}
assert_all_checks_ran() { # <clause>: §5 "all of them are run and reported"
  assert_all_urls_requested "$1"
  assert_all_workflows_queried "$1"
}

post_checks() { # run after every case: no seam bypassed, gh stayed inside the contract form
  if [ -s "$FAKE_DIR/poison.log" ]; then
    fail "the script ran a real tool instead of the seam: $(tr '\n' '|' < "$FAKE_DIR/poison.log") [§5 GH_BIN/CURL_BIN seams, §7 no network]"
  fi
  if [ -s "$FAKE_DIR/gh.unexpected" ]; then
    fail "gh invoked outside the form of §5 check 2: $(tr '\n' '|' < "$FAKE_DIR/gh.unexpected")"
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

# ── exit 0: nothing published, nothing in flight ───────────────────────────
case_clean() {
  local wf
  for wf in $WORKFLOWS; do inflight_set "$wf" 0; done
  run_crp "$VER"
  expect_clean "§5 exit 0: no Go tag, no run in flight (count 0), every registry 404"
  # §5 "all of them are run and reported (one stderr line each: <name> <state>)"; 404 = clean.
  # AMBIGUITY: the check names and their granularity (per source vs per URL) are unspecified, so
  # only the state word is asserted: at least one `clean` line and no `published`/`indeterminate` one.
  [ "$(state_lines clean)" -ge 1 ] || fail "no stderr line ends in 'clean' [§5: one line '<name> <state>' per check]"
  assert_eq "$(state_lines published)" 0 "stderr lines ending in 'published' [§5 per-check states]"
  assert_eq "$(state_lines indeterminate)" 0 "stderr lines ending in 'indeterminate' [§5 per-check states]"
  assert_all_checks_ran "§5 all checks are run (clean)"
}
case_crate_tag_alone_is_not_published() {
  # §5 check 1 looks only at refs/tags/bindings/rsac-go/vX.Y.Z; the crate tag is the one being recreated.
  push_tag origin "v$VER"
  run_crp "$VER"
  expect_clean "§5 check 1: only the Go tag counts; the crate tag vX.Y.Z is irrelevant"
}
case_go_tag_other_versions_are_clean() {
  push_tag origin "bindings/rsac-go/v${VER}0"
  push_tag origin "bindings/rsac-go/v1.2.2"
  run_crp "$VER"
  expect_clean "§5 check 1: the ref is exact (refs/tags/bindings/rsac-go/vX.Y.Z)"
}

# ── check 1: Go tag ────────────────────────────────────────────────────────
case_go_tag_published() {
  push_tag origin "$GO_TAG"
  run_crp "$VER"
  expect_published "§5 check 1: ls-remote prints a ref → published; §5 exit 1"
  [ "$(state_lines published)" -ge 1 ] || fail "no stderr line ends in 'published' [§5: one line '<name> <state>' per check]"
}
case_go_tag_remote_flag_published() {
  add_upstream
  push_tag upstream "$GO_TAG"
  run_crp "$VER" --remote upstream
  expect_published "§5 usage: --remote NAME selects the remote asked for the Go tag"
}
case_go_tag_other_remote_ignored() {
  add_upstream
  push_tag origin "$GO_TAG"
  run_crp "$VER" --remote upstream
  expect_clean "§5 usage: with --remote upstream the Go tag on origin is not consulted"
}
case_go_tag_ls_remote_fails() {
  run_crp "$VER" --remote no-such-remote
  expect_indeterminate "§5 check 1: git ls-remote failing → indeterminate; §5 exit 3"
  assert_all_checks_ran "§5 all checks are run even when check 1 is indeterminate"
}

# ── check 2: runs in flight ────────────────────────────────────────────────
case_inflight() { # <workflow> <stdout line> <clause>
  inflight_set "$1" "$2"
  run_crp "$VER"
  expect_indeterminate "$3"
  [ "$(state_lines indeterminate)" -ge 1 ] || fail "no stderr line ends in 'indeterminate' [§5: one line '<name> <state>' per check]"
}
case_inflight_empty_stdout() { # <workflow>
  inflight_empty "$1"
  run_crp "$VER"
  expect_indeterminate "§5 check 2: empty stdout is not one integer → indeterminate ($1)"
}
case_inflight_cli_fails() { # <workflow>
  inflight_fail "$1"
  run_crp "$VER"
  expect_indeterminate "§5 check 2: a failing gh command → indeterminate ($1)"
}

# ── check 3: registries ────────────────────────────────────────────────────
case_registry() { # <source> <code|FAIL|NOOUT> <verdict: published|indeterminate> <clause>
  curl_set "$1" "$2"
  run_crp "$VER"
  if [ "$3" = published ]; then expect_published "$4"; else expect_indeterminate "$4"; fi
}

# ── worst result decides ───────────────────────────────────────────────────
case_published_beats_indeterminate_go_vs_curl() {
  push_tag origin "$GO_TAG"
  curl_set crates 500
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (Go tag published, crates.io 500)"
}
case_published_beats_indeterminate_registry_vs_gh() {
  curl_set npm-main 200
  inflight_fail release.yml
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (npm main 200, gh failing)"
}
case_published_beats_indeterminate_platform_vs_curl() {
  curl_set npm-linux-arm64-musl 200
  curl_set pypi FAIL
  run_crp "$VER"
  expect_published "§5: published beats indeterminate (npm platform 200, PyPI curl failing)"
}
case_published_beats_indeterminate_registry_vs_ls_remote() {
  curl_set crates 200
  run_crp "$VER" --remote no-such-remote
  expect_published "§5: published beats indeterminate (crates.io 200, ls-remote failing)"
}
case_indeterminate_beats_clean() {
  inflight_set release-npm.yml 1
  curl_set pypi 404
  run_crp "$VER"
  expect_indeterminate "§5: indeterminate beats clean (one run in flight, the rest clean)"
}
case_all_checks_run_when_published() {
  push_tag origin "$GO_TAG"
  run_crp "$VER"
  expect_published "§5 exit 1"
  assert_all_checks_ran "§5: all checks are run and reported even after one says published"
}
case_stderr_reports_each_state() {
  push_tag origin "$GO_TAG" # published
  curl_set crates 500       # indeterminate
  run_crp "$VER"            # everything else: clean (404 / 0 in flight)
  expect_published "§5 exit 1"
  # §5: one stderr line `<name> <state>` per check, states published / indeterminate / clean
  [ "$(state_lines published)" -ge 1 ] || fail "no stderr line ends in 'published' [§5 per-check states]"
  [ "$(state_lines indeterminate)" -ge 1 ] || fail "no stderr line ends in 'indeterminate' [§5 per-check states]"
  [ "$(state_lines clean)" -ge 1 ] || fail "no stderr line ends in 'clean' [§5 per-check states]"
}

# ── invocation forms ───────────────────────────────────────────────────────
case_curl_invocation_form() {
  run_crp "$VER"
  expect_clean "§5 check 3"
  local s url prefix="-sS -o /dev/null -w %{http_code} -A $UA"
  for s in $SOURCES; do
    url="$(source_url "$s")"
    # §5 check 3: "$CURL_BIN" -sS -o /dev/null -w '%{http_code}' -A '<user agent>' <url>
    grep -qxF -- "$prefix $url" "$FAKE_DIR/curl.log" 2>/dev/null \
      || fail "curl was not invoked as '$prefix $url' [§5 check 3 invocation form]"
  done
}
case_gh_invocation_form() {
  run_crp "$VER"
  expect_clean "§5 check 2"
  assert_all_workflows_queried "§5 check 2 invocation form for release.yml, release-npm.yml and release-pypi.yml"
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
  # Control for the two cases below: the same staging with the unmodified manifest passes.
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
run_case stderr-reports-state-per-check case_stderr_reports_each_state

run_case curl-invocation-form case_curl_invocation_form
run_case gh-invocation-form case_gh_invocation_form

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

echo "check-release-published tests: $PASSED passed, $FAILED failed"
rm -rf "$WORK" # also cleaned by the EXIT trap; explicit so interpreters that skip EXIT traps leave nothing behind
[ "$FAILED" -eq 0 ]

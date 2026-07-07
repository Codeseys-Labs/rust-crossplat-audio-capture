<!-- Keep the summary line imperative and scoped; explain the "why" in the body.
     Full contributor reference: docs/CONTRIBUTING.md -->

## What & why

<!-- What does this PR change, and what problem does it solve? Link issues/seeds. -->

## Checklist (from CONTRIBUTING §8)

- [ ] `mise run gate` (or `bash scripts/gate.sh`) is clean — fmt, CI-replica clippy `-D warnings`, bare-build smoke.
- [ ] `mise run gate:full` extras are clean where relevant — lib tests, doctests, `cargo doc` (warning-free), module-DAG guard.
- [ ] New public items have rustdoc (purpose + example where non-trivial + `# Errors` where applicable).
- [ ] Relevant CI matrix rows are green (or the PR explains why a row is skipped/`continue-on-error`).
- [ ] Commit messages are imperative and scoped — **no `Co-Authored-By:` trailers or tool bylines** (AGENTS.md §6).
- [ ] Any behaviour change is mentioned in `CHANGELOG.md` under `[Unreleased]`.
- [ ] A large change layering along the module DAG is split into a stack of small PRs ([`docs/STACKED_PRS.md`](../docs/STACKED_PRS.md)).

## Review dispositions (before merge)

Every review comment (human, CodeRabbit, or agent) must end in one of two
terminal states **before this PR merges** — *fixed in the PR* or *captured in
a tracking issue* (label `deferred-review`; invalid/wont-fix → a closed
decision-record issue). A finding must never silently disappear into a merged
thread. See AGENTS.md §6.

- [ ] All review threads are resolved with an explicit disposition.

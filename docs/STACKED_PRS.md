# Stacked Pull Requests

> **TL;DR.** Big changes that layer along the module DAG
> (`core → bridge → audio → api → lib`) should ship as a **stack of small PRs**,
> one layer per PR, merged **bottom-up**. We do this **gh-native** (plain `git` +
> `gh` — no Graphite/spr/ghstack). The one hazard is our **squash-merge** culture:
> after the bottom PR squash-merges, recover each child with
> `git rebase --onto origin/master <old-parent> <child>`, never a plain rebase.

This document exists because PR #27 grew to **167 files / 52 commits** — CodeRabbit
auto-paused ("Too many files!") and couldn't review it, and squash-merging it
atomically was risky. Stacked PRs keep each layer small, independently reviewable,
and independently green.

## 1. Why gh-native (and not Graphite/spr/ghstack)

The recommendation is to **stay 100% gh-native** and add no tooling, for reasons
specific to this repo:

- **The module DAG is a natural stack.** Most large changes layer cleanly along
  `core → bridge → audio → api → lib`, with disjoint files per layer — exactly the
  condition under which upward rebases replay without conflict.
- **We squash-merge**, and that is the one true hazard (see §4). Commit-per-PR
  tools (`spr`, `ghstack`) actively fight squash-merge culture — `ghstack` can't
  even use the merge button. The only thing that would *remove* the squash-rebase
  dance is GitHub's server-side `gh stack`, which is private-preview/waitlist-gated
  and not usable today.
- **Small team.** SaaS lock-in + per-seat cost (Graphite) buys little at this size.
- **CodeRabbit size limits are solved structurally** by stacking: each child PR
  diffs against its **parent branch**, not `master`, so each layer's diff is small
  enough to review.

**Verified repo settings** (these shape the workflow below): default branch
`master`; `deleteBranchOnMerge = false`; squash + merge + rebase all allowed;
`gh` 2.92.0 with **no extensions**; `git` 2.54 (so `rebase.updateRefs` works).

**When to reconsider tooling:** adopt the official `gh stack` the moment it reaches
GA / you're off the waitlist (it's gh-native and removes the `--onto` dance); or, if
the team grows past ~3–4 active contributors **or** stacks routinely exceed 5 PRs,
evaluate Graphite (accept SaaS for a stack-aware merge queue + auto-restack). Until
one of those fires, do not add tooling.

## 2. When to stack (and when not to)

**Stack when:**

- The change layers along the DAG — e.g. a `core` type/trait change that `bridge`
  consumes, that `audio` then surfaces. This is the canonical rsac trigger.
- A single change would exceed **~200–250 LOC** or **~25 files** (the size that
  auto-paused CodeRabbit on #27) but decomposes into sequentially-dependent,
  individually-reviewable layers.
- Lower layers are ready for review before the upper layers are finished
  (reviewers pipeline: review `core` while you write `api`).
- Each layer touches **mostly disjoint files** → clean upward rebases.
- Depth stays **3–5 PRs** (the DAG has 5 layers — a natural cap).

**Do NOT stack when:**

- The changes are **genuinely independent** (e.g. a `bridge` fix and an unrelated
  `api` fix) → open **parallel PRs against `master`**; stacking would needlessly
  block the upper PR whenever the lower one is blocked.
- The whole change already fits in **one <250-LOC / <25-file PR** → stacking
  overhead (rebase cascades, N CI runs) isn't justified.
- You'd be forced to put genuinely-unrelated work in one stack just to share a
  branch — that's a smell; split into independent PRs.

## 3. The workflow (end to end)

**One-time setup** (run once, globally):

```bash
# git 2.38+ then repositions every intermediate stack branch pointer in a single
# rebase instead of orphaning them. rsac is on git 2.54.
git config --global rebase.updateRefs true
```

**Decompose** the change into DAG layers (`core → bridge → audio → api → lib`),
one layer per PR, each touching disjoint files where possible. Target **<250 LOC /
<25 files** per PR; split further if a layer is still too big.

**Build the stack bottom-up off `master`:**

```bash
# Layer A (e.g. core): branch off master.
git switch -c stack/core master
#   …commit the core layer…
git push -u origin stack/core
gh pr create --base master --head stack/core --fill --draft

# Layer B (e.g. bridge): branch off A, and set the PR base to A so the diff is
# scoped to just the bridge layer (NOT master).
git switch -c stack/bridge stack/core
#   …commit the bridge layer…
git push -u origin stack/bridge
gh pr create --base stack/core --head stack/bridge --fill --draft
# …repeat for audio / api / lib, each --base = the branch below it.
```

**Map the stack in every PR body** so humans + CodeRabbit understand ordering
(`gh` has no stack visualization). Use a fixed block:

```
Stack (merge bottom-up): #A core <- #B bridge <- #C audio <- #D api <- #E lib.
This PR = the bridge layer, stacked on #A.
```

> ⚠️ **Never start an intermediate PR title with `release:`** — that subject
> triggers `release-tag.yml` on squash-merge (see §4).

**Review downward first:** keep upper PRs `--draft`; flip the bottom to ready
(`gh pr ready stack/core`) so review focuses on the layer that merges next.

**Merge bottom-up.** When the bottom PR is approved + green:

```bash
# --delete-branch is REQUIRED (deleteBranchOnMerge=false) — it's what triggers
# GitHub to auto-retarget the child PR's base to master. Add --auto to wait for
# required checks.
gh pr merge stack/core --squash --delete-branch
```

**Recover the next child** (the critical step — squash made a NEW SHA on master):

```bash
git fetch origin
git switch stack/bridge
# Transplant ONLY bridge's own commits onto the squashed master tip, dropping the
# already-merged core commits. The 3-arg --onto form is mandatory here.
git rebase --onto origin/master stack/core stack/bridge
git push --force-with-lease
git branch -D stack/core          # squash means it won't show as "merged"; force-delete
gh pr view stack/bridge --json baseRefName -q .baseRefName   # confirm == master
```

**Repeat** up the stack: merge `stack/bridge --squash --delete-branch`, then
`git rebase --onto origin/master stack/bridge stack/audio`, and so on until `lib`
lands.

**Mid-stack review fix** (a comment lands on a *lower* PR): amend that branch, then
cascade **upward** (parent identity didn't change, so plain `rebase`, not `--onto`):

```bash
git switch stack/bridge && git commit --fixup HEAD && git push --force-with-lease
git switch stack/audio  && git rebase stack/bridge && git push --force-with-lease
# rebase.updateRefs keeps all intermediate refs consistent in one pass.
```

**Disposition every review comment** per repo policy (fix-in-PR + reply, or open a
`deferred-review`-labelled issue and reply `Tracked in #N`) — see
[`AGENTS.md` §6](../AGENTS.md). A finding must not vanish into a squash-merged PR.

## 4. Pitfalls (read before your first stack)

| Pitfall | Mitigation |
|---|---|
| **Squash-divergence (#1 failure mode).** After squashing the bottom PR, a plain `git rebase origin/master` on the child re-replays the already-merged commits → phantom conflicts / a diff that re-includes the parent's work. | **Never** plain-rebase a child after a parent squash. Always `git rebase --onto origin/master <old-parent> <child>`, then `--force-with-lease`. Delete the merged parent with `git branch -D`. |
| **No auto-retarget.** `deleteBranchOnMerge=false` means GitHub does NOT retarget the child's base unless you delete the parent branch. | Always pass `--delete-branch` to `gh pr merge`. If forgotten, `gh pr edit <child> --base master`. Verify with `gh pr view <child> --json baseRefName`. |
| **`git push --force` clobbers** a teammate's/bot's concurrent push. | Use `--force-with-lease` everywhere — it aborts if the remote moved. |
| **A `release:`-titled intermediate PR** would cut a spurious tag + Release on squash-merge. | Reserve `release: vX.Y.Z` titles exclusively for the dedicated (never-stacked) release-prepare PR. |
| **Merging out of order.** GitHub only cleanly merges a contiguous group from the lowest unmerged PR. | Always merge strictly bottom-up. To re-prioritize, rebase the wanted layer down to base — don't merge mid-stack. |
| **Deep-stack rebase cascade.** A fix at the bottom of a deep stack re-runs CI per layer and can conflict at each. | Keep stacks shallow (3–5) and layers' files disjoint. `rebase.updateRefs` fixes all intermediate refs in one pass when you rebase from the top. |
| **CI base confusion.** A stacked PR's CI runs against its *base* (the layer below), not `master`. | Each stacked branch contains its parent's commits, so its tree is complete and CI is meaningful — but ensure workflows trigger on `pull_request` to any base, not hard-pinned to `master`. |
| **Native merge queue is not stack-aware.** | Don't put a stack in the merge queue; merge bottom-up manually with `gh pr merge --squash --delete-branch`. |

## 5. Quick reference

```bash
# one-time
git config --global rebase.updateRefs true

# build (per layer N stacked on N-1)
git switch -c stack/<layer> <parent> && git push -u origin stack/<layer>
gh pr create --base <parent> --head stack/<layer> --fill --draft

# merge bottom-up, then recover each child
gh pr merge stack/<bottom> --squash --delete-branch
git fetch origin && git switch stack/<child>
git rebase --onto origin/master stack/<bottom> stack/<child> && git push --force-with-lease
git branch -D stack/<bottom>

# mid-stack fix: amend lower, cascade up with plain rebase + --force-with-lease
```

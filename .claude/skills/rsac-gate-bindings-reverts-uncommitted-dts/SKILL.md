---
name: rsac-gate-bindings-reverts-uncommitted-dts
description: |
  rsac trap: gate:bindings (scripts/gate-bindings.sh) git-checkouts
  bindings/rsac-napi/index.d.ts and index.js after the napi build to undo
  NAPI-RS auto-regeneration — which ALSO silently reverts any UNCOMMITTED
  hand edits to those files. Use when: (1) a hand edit to index.d.ts
  "disappears" after running mise run gate:bindings, (2) editing the
  hand-maintained napi type stubs as part of a change that also needs the
  bindings gate, (3) a lane agent reports "reverted index.d.ts with git
  checkout" after a napi build.
author: Claude Code
version: 1.0.0
date: 2026-07-18
---

# rsac: gate:bindings reverts uncommitted index.d.ts hand edits

## Problem

`bindings/rsac-napi/index.d.ts` and `index.js` are HAND-MAINTAINED, but the
napi build (`napi build`, run inside `scripts/gate-bindings.sh`) regenerates
both from the compiled addon. To keep the tree clean, the gate script
`git checkout`s them after the build — restoring the **committed** version.

Consequence: any *uncommitted* hand edit to `index.d.ts` made before running
`mise run gate:bindings` is silently wiped. The gate exits green; the edit is
just gone. In PR #63 this happened twice independently: the lane-A fixer had
to re-revert regenerated drift, and a review-fix hand edit to the `gain()`
doc was silently lost and had to be re-applied after the gate run.

## Context / Trigger Conditions

- `git status` no longer shows `bindings/rsac-napi/index.d.ts` after a
  gate:bindings run that should not have touched it
- A doc/type change you made to `index.d.ts` is absent from the diff you are
  about to commit
- Working on a change that touches both Rust napi code and the `.d.ts` stubs

## Solution

Order of operations when a change involves `index.d.ts`:

1. Make the Rust-side changes.
2. Run `mise run gate:bindings` FIRST (it builds, tests, and restores the
   committed `.d.ts`).
3. THEN hand-edit `index.d.ts` / `index.js`.
4. Commit. (A follow-up gate run is safe once the edit is committed — the
   restore checks out the committed version, which now includes your edit.)

Alternative: commit the hand edit before gating.

## Verification

After the gate run, `grep` for your edited text in `index.d.ts` before
committing — if it's missing, the gate reverted it.

## Notes

- `bindings/rsac-python/rsac/__init__.pyi` is also hand-maintained but is
  NOT regenerated/reverted by the gate — only the napi pair is affected.
- The same discipline applies to any agent prompt: tell implementers to
  edit `.d.ts` after their final gate run, or commit first.
- See also: `rsac-buffer-size-is-ring-slot-count`,
  `rsac-post-stop-read-contract` (sibling project-contract traps).

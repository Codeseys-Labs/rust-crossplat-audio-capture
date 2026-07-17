# Workflow Recursion Guard Design

## Goal

Prevent workflows from recursively creating additional workflows while preserving phase-local agent expansion and concurrent top-level workflows.

## Policy

- Only the main Claude Code session may invoke `Workflow`.
- Agents running inside a workflow must not invoke `Workflow`.
- Workflow agents may invoke `Agent` to expand work within their assigned workflow and phase.
- Multiple workflows may run concurrently when the main session starts them.
- `TaskCreate` and other task-management tools remain available; this guard does not restrict them.

## Implementation

Add a short orchestration rule to the repository's shared Claude instructions. Add a project-level `PreToolUse` hook matched only to `Workflow`. The hook permits the call when no subagent identity is present and denies it when Claude Code reports a subagent identity.

The denial message directs the worker to use `Agent` for phase-local expansion or return a request to the main session when a separate workflow is genuinely needed.

The hook must use a documented, stable Claude Code hook-input field. If the installed Claude Code version does not expose a reliable subagent identity to `PreToolUse`, keep the written policy and do not install a heuristic hook based on transcript paths, process ancestry, or naming conventions.

## Validation

- Validate project settings with the installed Claude Code configuration checker.
- Feed representative main-session and subagent hook payloads to the hook command.
- Verify a main-session `Workflow` call is allowed.
- Verify a subagent `Workflow` call is denied with a useful message.
- Verify `Agent` is not matched or denied.

## Non-goals

- Limiting the number of top-level workflows.
- Limiting how many agents a workflow phase may use.
- Preventing agents from creating tasks.
- Building a cross-process workflow counter or custom orchestration service.

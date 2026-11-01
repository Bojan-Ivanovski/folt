## When it applies

An L is always a Question. Split it into M/S/XS child tickets under it.

## The ticket must contain

Use the five-section [work-item body](../issue-format.md) with Summary and Scope filled at creation.
Set type Question and the milestone, Priority, Size, Status and ownership from the [shared steps](README.md#shared-steps).

## Analysis and review

L needs analysis and review. The orchestrator records findings and solution attachments using [Issue format](../issue-format.md).
Use a read-only Codex [review](../review.md).

## Solution

The developer selects the answer. Mark its attachment [Selected Solution] and summarize changes, Affected areas and Why this one.
Link findings and two to four key points in Details. Derive the child tickets from the answer.

## Statuses

Suggested path: `S0: To Do` -> `S1: In Analysis` -> `S3: Ready For Implementation` -> `S4: In Implementation` -> `S5: Done`.
Project workflows set Done on close and on merge. An answered Question may be Done with Answered while its children remain open.

## Who does what

The orchestrator analyses; the developer selects the answer. Codex implements and fixes in dedicated child worktrees and does not analyse.
The orchestrator runs the [gate](../testing.md#gate-before-push), commits, pushes and opens PRs; only the developer merges.
Give every PR a milestone, an assignee and the project.

## Components and parents

L may name several Components describing what can be done; represent each one by its own child ticket.
Each child keeps its size's Component limit. Use native parents and the [shared repository rules](README.md#shared-steps).

## Closing

Close with `Disposition: Answered` only after all children are closed.
It may be `S5: Done` with its Disposition before then; do not rely on "Auto-close issue" for parents.

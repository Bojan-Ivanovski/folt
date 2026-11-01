## When it applies

Use M for work that needs analysis and review; the ticket may leave several solutions open.
Split it into M/S items when warranted.

## The ticket must contain

Use the five-section [work-item body](../issue-format.md) with Summary and Scope filled at creation.
Set the milestone, Priority, Size, Status and ownership from the [shared steps](README.md#shared-steps).

## Analysis and review

M needs analysis AND review. The orchestrator records findings and solution attachments using [Issue format](../issue-format.md).
Use a read-only Codex [review](../review.md).

## Solution

The developer selects the solution before implementation. Mark its attachment [Selected Solution].
Summarize changes, Affected areas and Why this one in Solution; link findings and two to four key points in Details.

## Statuses

Suggested path: `S0: To Do` -> `S1: In Analysis` -> `S3: Ready For Implementation` -> `S4: In Implementation` -> `S5: Done`.
Project workflows set Done on close and on merge.

## Who does what

Codex implements and fixes in a dedicated worktree and does not analyse; the orchestrator analyses.
The orchestrator runs the [gate](../testing.md#gate-before-push), commits, pushes and opens the PR; only the developer merges.
Give every PR a milestone, an assignee and the project. Apply the [type row](README.md#by-issue-type).

## Components and parents

Set up to two Component issue fields, for example Source Code and Tests for unit tests.
Use the [shared parent and repository rules](README.md#shared-steps).

## Closing

Record Disposition and PR verification. A parent cannot close until all children are closed.
It may be `S5: Done` with its Disposition before then; do not rely on "Auto-close issue" for parents.

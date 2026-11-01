## When it applies

Use XS for the smallest work item; the ticket is self-descriptive.

## The ticket must contain

Use the five-section [work-item body](../issue-format.md) with Summary and Scope filled at creation.
Set the milestone, Priority, Size, Status and ownership from the [shared steps](README.md#shared-steps).

## Analysis and review

XS needs no analysis and no review.

## Solution

Make the ticket self-descriptive; follow the applicable [type row](README.md#by-issue-type).

## Statuses

The ticket is ready when created. Suggested path: `S0: To Do` -> `S4: In Implementation` -> `S5: Done`.
Project workflows set Done on close and on merge.

## Who does what

Codex implements and fixes in a dedicated worktree and does not analyse.
The orchestrator runs the [gate](../testing.md#gate-before-push), commits, pushes and opens the PR; only the developer merges.
Give every PR a milestone, an assignee and the project.

## Components and parents

Set exactly one Component issue field. Use the [shared parent and repository rules](README.md#shared-steps).

## Closing

Record Disposition and PR verification. A parent cannot close until all children are closed.
It may be `S5: Done` with its Disposition before then; do not rely on "Auto-close issue" for parents.

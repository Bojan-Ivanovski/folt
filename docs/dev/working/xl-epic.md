## When it applies

XL is a release; it only groups the work and is always an Epic.

## The ticket must contain

Title it `<version> -- Folt`. The body contains only `## Objective` and a short release explanation.
Set type Epic and the milestone, Priority, Size, Status and ownership from the [shared steps](README.md#shared-steps).

## Analysis and review

XL needs no analysis and no review. Follow each child's size for its analysis and review.

## Solution

Keep detailed work in child tickets; the Epic only groups it.

## Statuses

The Status path of an XL is not decided; see [Open points](README.md#open-points).
Project workflows set `S5: Done` on close.

## Who does what

Follow [Testing roles](../testing.md#roles) for children: Codex implements and fixes in a dedicated worktree and does not analyse.
The orchestrator analyses, runs the gate, commits, pushes and opens PRs; only the developer merges.
Give every PR a milestone, an assignee and the project.

## Components and parents

Group work with native child relationships; follow each child's size for Component limits.
Use the [shared repository rules](README.md#shared-steps); the XL Component count is an [open point](README.md#open-points).

## Closing

Close with `Disposition: Released` only after all children are closed.
A parent may be `S5: Done` with its Disposition before closure; do not rely on "Auto-close issue" for parents.

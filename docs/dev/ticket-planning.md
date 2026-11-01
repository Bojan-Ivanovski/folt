# Ticket planning and lifecycle

Use this guide whenever creating, analysing, scheduling, implementing or closing
a GitHub issue. GitHub Issues hold the work record; the Folt project
(Folt-CLI organization project #1) holds current planning status, size and
iteration. Priority is an issue field.

## At issue creation

Terms are defined in [GLOSSARY.md](../../GLOSSARY.md).

1. Choose the issue type from the initial request. See the
   [issue types](working/README.md#by-issue-type) for their uses.
2. Assign a responsible person. For tickets created at the developer's request,
   assign `Bojan-Ivanovski` unless the developer names another owner. Change the
   assignee when responsibility is handed off; do not leave an active issue
   unassigned.
3. Add the issue to the Folt project and set its metadata using the
   [shared steps](working/README.md#shared-steps). Every ticket has a milestone
   (the milestone of the release it is for, currently `0.1.0`; otherwise
   `Backlog`), a Priority, a Size and a Status.
4. Set its native parent relationship at creation: put a ticket derived from a
   decision under its Question. Use an existing delivery group when one fits;
   otherwise use release epic #1 unless the developer gives another parent.
   Follow the [size rules](working/README.md) when grouping work. Do not repeat
   the parent in the issue body.
5. Add a native `blocked by` relationship only for a real prerequisite that
   prevents implementation or a dependent decision. GitHub represents the
   reverse `blocking` relationship on the prerequisite. Do not use parent links
   as dependency links or add dependencies just because work is related.

Follow the [shared steps](working/README.md#shared-steps) for repository
ownership and required fields.

## Priority

Set the Priority issue field when creating the issue:

- **Urgent:** An exceptional immediate risk or blocker, such as a security gap
  or a prerequisite that must be resolved before the next core work can safely
  proceed. Reserve Urgent for the few items that truly stop other work.
- **High:** Important product or foundation work on the near-term path.
- **Medium:** Later, non-blocking work.
- **Low:** Optional work, including polish and extensions.

Priority may change as dependencies or product decisions change. Update the
issue field when new evidence warrants it.

## Size

Follow the [size table](working/README.md) and the individual templates for
[XS](working/xs.md), [S](working/s.md), [M](working/m.md),
[L Questions](working/l-question.md) and [XL Epics](working/xl-epic.md).
They define analysis, splitting, review and Component limits.
Follow the [shared steps](working/README.md#shared-steps) for refining Size
after analysis and applying the new review and Component limits.

The Folt project's `Estimate` field has no agreed unit or scale. Leave it blank
until the team defines one. Do not convert sizes into invented point or time
values.

## Fields after decisions

Set the Component issue field after the developer selects a solution, using
Build, Dev Docs, Dev Tools, Source Code, Tests or User Docs for its actual
affected areas. Follow the [size template](working/README.md) for limits.
Set the Disposition issue field when ready to close, using Abandoned, Answered,
Duplicate, Invalid, Merged, Obsolete, Out of Scope, Released, Superseded,
Withdrawn or Won't Do. Follow the [closing rules](working/README.md#shared-steps)
and [type rules](working/README.md#by-issue-type) for the outcome.
The [shared steps](working/README.md#shared-steps) also govern `Merged`, reasons
for other closures before `S5: Done`, and superseded child tickets.

## Project status through the workflow

Use the [Status list and shared steps](working/README.md#shared-steps), keeping
status aligned with the ticket's stage. Follow the size template's path. When
blocked, keep the stage status, add the native blocked-by relationship and
record the blocker and next action in the issue. Remove the dependency when it
no longer blocks work.

## Milestones and iterations

Use the release milestone for scheduled release work and `Backlog` otherwise,
following the [required metadata](#at-issue-creation). Update it when the
intended release changes.

Assign an iteration when the work is deliberately scheduled into a real project
cycle, taking priority, dependencies and capacity into account. Leave unscheduled
backlog work without an iteration; do not put every issue into the next cycle.
Closed work does not belong to a future iteration.

Before closure, follow the [closing rules](working/README.md#shared-steps) and
record delivery and performed verification in the PR.

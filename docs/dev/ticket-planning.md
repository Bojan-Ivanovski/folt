# Ticket planning and lifecycle

Use this guide whenever creating, analysing, scheduling, implementing or closing
a GitHub issue. GitHub Issues hold the work record; Project Foltivo holds the
current planning status, priority, size and iteration.

## At issue creation

1. Choose exactly one existing `Type:` label from the issue's initial request:
   `Bug`, `Documentation`, `Enhancement`, `Epic`, `Feature`, `Maintenance`,
   `Other` or `Question`. The type describes the work requested, not its
   implementation details. Release epics use `Type: Epic`.
2. Assign a responsible person. For tickets created at the developer's request,
   assign `Bojan-Ivanovski` unless the developer names another owner. Change the
   assignee when responsibility is handed off; do not leave an active issue
   unassigned.
3. Add the issue to Project Foltivo. Set `Status` to `Backlog`, and set the
   `Priority` and `Size` fields using the scales below. Revisit those estimates
   when analysis clarifies the scope.
4. Set its native parent relationship at creation: use the existing delivery
   group when one fits; otherwise use release epic #1 unless the developer gives
   another parent. Do not repeat the parent in the issue body.
5. Add a native `blocked by` relationship only for a real prerequisite that
   prevents implementation or a dependent decision. GitHub represents the
   reverse `blocking` relationship on the prerequisite. Do not use parent links
   as dependency links or add dependencies just because work is related.

`Priority` and `Size` are Project Foltivo fields, not issue labels. The issue
labels configured in both repositories are `Type:`, `Component:` and
`Disposition:` labels.

## Priority

Set priority when creating the issue. Use the Project Foltivo values:

- **P0:** An exceptional immediate risk or blocker, such as a security gap or a
  prerequisite that must be resolved before the next core work can safely
  proceed. Reserve P0 for the few items that truly stop other work.
- **P1:** Important product or foundation work on the near-term path.
- **P2:** Later, optional or non-blocking work, including polish and extensions.

Priority may change as dependencies or product decisions change. Update the
project field when new evidence warrants it.

## Size and splitting

Set the Project Foltivo `Size` field at creation and refine it during analysis.
Use this scale as a rough scope guide, not a promise of duration:

| Size | Use for |
| --- | --- |
| **XS** | A tiny maintenance change, such as deleting one forgotten unused file. |
| **S** | A small feature update or other narrow, reviewable change. |
| **M** | One cohesive feature or bug fix with a bounded outcome. |
| **L** | A related group of features or bugs that forms one delivery area. Use a parent issue and child tickets when the pieces have distinct outcomes or review boundaries. |
| **XL** | A release or epic containing a tree of changes. Create an epic and decompose its work into bounded child issues; do not implement the whole tree in one ticket. |

Resolve the hierarchy during analysis before an L or XL issue is marked
Ready:

- If an L or XL scope contains distinct outcomes, decisions or review boundaries,
  create native child issues for those parts. Keep the parent as the delivery
  group and link real prerequisites with native blocked-by relationships.
- If analysis finds one cohesive outcome that should ship and be reviewed as one
  change, reduce its size to M (or smaller) instead of keeping it as a standalone
  L/XL implementation ticket.
- XL is release/epic scale: use a release or epic parent and decompose its tree
  into bounded delivery groups and work items. Do not implement the tree in one
  ticket.
- A decision-only analysis issue may be M even when the implementation it
  informs will later need an L/XL parent and child tickets.

Analysis is not complete until the size and ticket hierarchy agree. Follow
[ticket hierarchy](ticket-hierarchy.md) for parent selection and group structure.

The Project Foltivo `Estimate` field has no agreed unit or scale. Leave it blank
until the team defines one. Do not convert sizes into invented point or time
values.

## Labels after decisions

- **`Type:`** Apply exactly one at issue creation, based on the requested work.
- **`Component:`** Wait until the developer selects a solution. Then apply the
  existing component labels for the selected solution's actual affected areas;
  add more than one when appropriate.
- **`Disposition:`** Apply one existing disposition label only when the issue is
  ready to close and its outcome is known. Use `Merged` for work delivered by a
  merged PR; use the matching existing value for work closed for another reason.

Do not guess future component labels before the solution is selected or label an
open issue with a final disposition.

## Project status through the workflow

Keep the issue's Project Foltivo status aligned with its current stage:

| Status | When to use |
| --- | --- |
| **Backlog** | Created but not actively being analysed or implemented. |
| **In Analysis** | Investigation, solution comparison or developer decision is underway. |
| **Ready** | Scope and selected solution are clear enough to implement. |
| **In progress** | Implementation has started. |
| **In review** | A pull request is open for the work. |
| **Done** | The issue is closed with its disposition recorded. |

Move an issue to `In Analysis` when refinement starts, and update status as it
moves stages. The current project has no `Blocked` status: keep the stage status,
add the native blocked-by relationship, and record the blocker and next action in
the issue when work cannot proceed. Remove the dependency when it no longer
blocks the work.

During analysis, keep findings and solution attachments current. After the
developer selects a solution, mark its attachment `[Selected Solution]`, fill
the Solution section, apply `Component:` labels and move the issue to `Ready` if
implementation can start. If scope or priority changes, revise the Project
Foltivo fields and explain the decision in the issue or a short comment.

## Iterations and completion

Assign an iteration when the work is deliberately scheduled into a real project
cycle, taking priority, dependencies and capacity into account. Leave unscheduled
backlog work without an iteration; do not put every issue into the next cycle.
Closed work does not belong to a future iteration.

Before closing an issue, choose its final `Disposition:` label. On merge, confirm
the PR closes the right issue, apply `Disposition: Merged`, and set the project
status to `Done`. For any other closure, apply the matching disposition and
record the reason before setting `Done`.

# Working templates

Use these templates to work a ticket by size and issue type. They record the
rules decided in [Question #188](https://github.com/Folt-CLI/folt/issues/188).
These rules override the older labels, priorities, statuses and size rules in
the linked guides; [#191](https://github.com/Folt-CLI/folt/issues/191) updates
those guides. Follow the guides for procedures these rules do not change.

Reading order: size file first, then the type row.

| Size | Analysis | Review | Components | Typically |
| --- | --- | --- | --- | --- |
| [XS](xs.md) | No | No | Exactly one | Smallest, self-descriptive work item |
| [S](s.md) | No | Yes | Exactly one | Straightforward ticket with its solution already decided |
| [M](m.md) | Yes | Yes | Up to two | Work that may leave several solutions open; split into M/S items when warranted |
| [L](l-question.md) | Yes | Yes | Several; each represented by its own child | Always a Question, split into M/S/XS children |
| [XL](xl-epic.md) | No | No | Not specified; see Open points | Always an Epic; a release that only groups work |

## Shared steps

- Ticket work in the repository where it happens. Reference other repositories
  by full name. Use native parent links for grouping and dependency links for
  real prerequisites. Derived tickets belong under the Question whose answer they come from;
  otherwise use an existing delivery group or release epic #1 unless directed otherwise.
- Give every ticket a milestone (the milestone of the release it is for, currently `0.1.0`; otherwise `Backlog`),
  a Priority (`Urgent`, `High`, `Medium`, `Low`), a Size and a Status. Follow
  [ticket planning](../ticket-planning.md) for assignment and project membership.
  Give every pull request a milestone, an assignee and the project.
- Use issue types: Task, Bug, Feature, Question, Epic, Maintenance, Documentation,
  Incident, Test. Issue types replace Type labels; Component and Disposition
  are issue fields replacing those labels. Set Components after solution selection.
- Use exactly Summary, Scope, Analysis, Solution and Details for work-item bodies
  as in [Issue format](../issue-format.md); fill Summary and Scope at creation.
  Follow the size file for analysis and solution readiness, and the Incident
  and Epic rows below for their report and Objective formats.
- Use these Status values: `S0: To Do`, `S1: In Analysis`,
  `S3: Ready For Implementation`, `S4: In Implementation`, `S5: Done`.
  XS and S are ready when created; their suggested path is S0 -> S4 -> S5.
  M and L have the suggested path S0 -> S1 -> S3 -> S4 -> S5.
  Project workflows set Done on close and on merge.
- Codex implements and fixes in a dedicated worktree and does not analyse code,
  reviews or CI. The orchestrator analyses, runs the gate, commits, pushes and
  opens the PR. Only the developer merges. Follow [Testing](../testing.md) for
  roles, tests and the gate, and [Review](../review.md) for read-only Codex
  review where the size requires review.
- Copy the issue's type into commit subjects: `<Issue Type>[#N]: ...`.
  Follow [Workflow](../workflow.md) for commit bodies and PR delivery, and
  [Stacked pull requests](../stacked-prs.md) for dependent branches and PRs.
- Record Disposition before closing. A parent cannot close until all its
  children are closed. It may reach `S5: Done` with its Disposition beforehand,
  such as an answered Question with open children. Do not rely on the project
  workflow "Auto-close issue" for parents. Record delivery and verification in the PR.

## By issue type

| Type | What changes in the steps | Commit subject |
| --- | --- | --- |
| Task | Describe the bounded work in Summary and Scope; follow the size steps. | `Task[#N]: ...` |
| Bug | Record reproduction steps, actual and expected behaviour; add a regression test that fails before the fix. | `Bug[#N]: ...` |
| Feature | Describe the user need and intended result; follow the size steps. | `Feature[#N]: ...` |
| Maintenance | Describe the maintenance outcome and scope; follow the size steps. | `Maintenance[#N]: ...` |
| Documentation | Describe the documentation change; see [Skipping PR builds](../testing.md#skipping-pr-builds) for when CI skips. | `Documentation[#N]: ...` |
| Test | The whole outcome is test cases; see [Testing](../testing.md#test-tickets). Tests belonging to another change stay in that change's ticket. | `Test[#N]: ...` |
| Question | Record a decision selected by the developer. Derive children from the answer. Close with `Disposition: Answered` only after all children are closed; Done with Answered may precede closure. | `Question[#N]: ...` |
| Epic | Use only `## Objective` in the body. Follow XL: title `<version> -- Folt`, group release work, close with `Disposition: Released` after all children close. | `Epic[#N]: ...` |
| Incident | Follow [Incidents](../incidents.md) for evidence, impact, linked response tickets and resolution; the report is not itself the fix. | `Incident[#N]: ...` |

## Existing guides

Use [Process](../process.md) and [AGENTS.md](../../../AGENTS.md) as entry points.
Consult [Workflow](../workflow.md), [Ticket planning](../ticket-planning.md),
[Issue format](../issue-format.md), [Testing](../testing.md),
[Review](../review.md), [Incidents](../incidents.md) and
[Stacked pull requests](../stacked-prs.md) for their unchanged procedures.

## Open points

- XL has no decided Status path; confirm which statuses an Epic visits before it is Done.
- XL has no Component count specified in the supplied decisions; confirm whether any limit applies.

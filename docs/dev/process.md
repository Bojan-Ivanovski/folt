# Folt development process

Staged issue workflow shared by the Foltivo repositories. Keep process docs
under docs/dev and issue forms under .github/ISSUE_TEMPLATE. GitHub issues are
the durable work record.

Work items contain exactly Summary, Scope, Analysis, Solution and Details.
Fill Summary and Scope at creation. Codex investigates and attaches findings and
separate proposals; the developer selects the solution before implementation.
Details contains the findings attachment and two to four key points. Do not add
Acceptance criteria, Implementation plan or Closeout sections. Record delivery
and performed verification in the PR.

Use native parent links for grouping and blocked-by links for prerequisites.
Independent release work defaults to epic #1; use an existing delivery group or
an explicit developer instruction when applicable. Release epic titles use
`<version> -- Folt`; their body contains only `## Objective` and a
short release explanation. Do not repeat a ticket's parent in its description.

| Guide | Covers |
| --- | --- |
| [Workflow](workflow.md) | Issue stages, branches, commits, labels and PRs |
| [Issue format](issue-format.md) | Work-item and release-epic format and analysis attachments |
| [Ticket planning](ticket-planning.md) | Labels, assignment, size, priority, relationships, and Project status |
| [Incidents](incidents.md) | Reports, evidence, linked response work and resolution |
| [GitHub CLI](github-cli.md) | Issues, native relationships, Markdown uploads and PR commands |
| [Ticket hierarchy](ticket-hierarchy.md) | Starting order and cross-repository boundaries |
| [Stack](stack.md) | Confirmed technical choices and pending decisions |
| [Verification](verification.md) | Checks actually performed and their status |

[AGENTS.md](../../AGENTS.md) points future sessions to this process.
[CONTRIBUTING.md](../../CONTRIBUTING.md) is the contributor entry point.
[Label definitions](../../.github/labels.json) record the shared Type/Component
labels configured on GitHub.

# Folt development process

Staged issue workflow shared by the Foltivo repositories. Keep process docs
under docs/dev and issue forms under .github/ISSUE_TEMPLATE. GitHub issues are
the durable work record.

Work items contain exactly Summary, Scope, Analysis, Solution and Details.
Fill Summary and Scope at creation. The orchestrator investigates and attaches
findings and separate proposals; the developer selects the solution before
implementation where analysis applies; see the working templates.
Details contains the findings attachment and two to four key points. Do not add
Acceptance criteria, Implementation plan or Closeout sections. Record delivery
and performed verification in the PR.

Use native parent links for grouping and blocked-by links for prerequisites.
Independent release work defaults to epic #1; use an existing delivery group or
Question, or an explicit developer instruction when applicable. See
[Issue format](issue-format.md#epics) for release Epics. Do not repeat a ticket's
parent in its description.

| Guide | Covers |
| --- | --- |
| [Glossary](../../GLOSSARY.md) | The terms of Folt and of the process, defined once |
| [Working templates](working/README.md) | How to work a ticket by size and issue type |
| [Workflow](workflow.md) | Issue stages, branches, commits, issue types and fields, and PRs |
| [Stacked pull requests](stacked-prs.md) | Creating, updating and merging native PR stacks |
| [Review](review.md) | Read-only agent reviews and posting findings |
| [Testing](testing.md) | Test rule, push gate, CI review, roles |
| [Issue format](issue-format.md) | Work-item and release-epic format and analysis attachments |
| [Ticket planning](ticket-planning.md) | Issue types and fields, assignment, priority, relationships, milestones and iterations |
| [Incidents](incidents.md) | Reports, evidence, linked response work and resolution |
| [GitHub CLI](github-cli.md) | Issues, native relationships, Markdown uploads and PR commands |
| [Stack](stack.md) | Confirmed technical choices and pending decisions |
| [Repository settings](repository.md) | Recorded GitHub settings, drift checks and application |
| [Verification](verification.md) | Checks actually performed and their status |

[AGENTS.md](../../AGENTS.md) points future sessions to this process.
[CONTRIBUTING.md](../../CONTRIBUTING.md) is the contributor entry point.
The [working templates](working/README.md) hold the size, status and closing
rules. Use the other guides for their specific procedures.

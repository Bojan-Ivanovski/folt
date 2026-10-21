# Folt repository instructions

Read [the development process](docs/dev/process.md) before creating or updating
issues, analysing work, implementing changes or committing. Keep process docs
under docs/dev and GitHub issue forms under .github/ISSUE_TEMPLATE.

## Scope and decisions

- Record technical decisions in [the stack](docs/dev/stack.md) only after the
  developer selects them; do not infer one decision from another.
- Use `gh` with `--repo Bojan-Ivanovski/folt`.
- Use native parent and dependency relationships. Release work defaults to epic
  #1 unless a delivery group, Question or developer instruction specifies
  another parent.
- Cross-repository references must include the repository name or full URL. Do
  not assume issue numbers identify issues in another repository.

## Issues and analysis

- Follow [ticket planning](docs/dev/ticket-planning.md) for every issue: set its
  `Type:` label, assignee, Project Foltivo priority and size at creation; add
  native parent/dependency relationships; apply `Component:` only after solution
  selection and `Disposition:` when ready to close; update project status through
  analysis and delivery.
- Work items have exactly Summary, Scope, Analysis, Solution and Details.
- Fill Summary/Scope at creation; leave other sections pending until their stage.
- No Acceptance criteria, Implementation plan or Closeout sections.
- Codex investigates; the developer selects the solution before implementation.
- Attach findings as <N>-code-findings.md and each solution as
  <N>-solution-<k>.md using gh-attach. Verify uploads and use real attachment links.
- After selection, tag the attachment [Selected Solution] and summarize changes,
  Affected areas and Why this one in Solution.
- Details contains the findings attachment and two to four key points.

## Commits and delivery

- When implementation finishes, the orchestrator reviews the diff, runs local
  checks, commits in the issue format, pushes and opens the PR without another
  request. Only the developer merges; an agent merges only when instructed for
  that PR. Follow [Review](docs/dev/review.md) and [Stacked pull
  requests](docs/dev/stacked-prs.md).
- All agents, including Codex and delegated agents, must do implementation work
  in a dedicated Git worktree for the issue branch. Do not implement in the
  primary checkout or share one worktree between concurrent issue work. Fetch
  and fast-forward the base branch before creating the worktree.
- First commit: Initial[#1]: <short explanation>.
- Later commits: <Issue Type>[#<issue-number>]: <short explanation>, copying the
  exact value of the issue's Type: label.
- Keep commits small and focused; group a multi-part ticket into several commits.
- Every commit body has **Summary** (why the commit exists) and, when it touches
  more than one thing or the reason is not obvious, **Changes**: bullets of
  "what changed: why it changed". See docs/dev/workflow.md.
- Start issue branches from `develop`, or from the lower branch of a stack, and
  target PRs to `develop`, or to the branch below in a stack. Promote
  releases from `develop` to `main` through a PR. After implementation, the
  orchestrator reviews the diff, runs local checks, commits in the issue's
  commit format, pushes and opens the PR without a further request. Only the
  developer merges; an agent merges only when instructed for that PR. Record
  requested changes on the ticket. Use the PR title and description format in
  docs/dev/workflow.md and record delivery and verification in the PR.
- Do not add/run tests unless the developer requests testing or verification.

## Repository conventions

- Use exactly one appropriate existing Type: label. Copy its value into commit
  subjects; apply Component: labels after selecting the solution's affected areas.
- Label definitions are recorded in .github/labels.json. Preserve existing labels
  and issue assignments when synchronizing definitions.
- Follow .editorconfig. Keep build outputs, local configuration and credentials
  out of Git.
- Use the PR template to record changes, owning issues, performed verification
  and material risks. Do not treat template placeholders as performed checks.

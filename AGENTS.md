# Folt repository instructions

Read [the development process](docs/dev/process.md) before creating or updating
issues, analysing work, implementing changes or committing. Keep process docs
under docs/dev and GitHub issue forms under .github/ISSUE_TEMPLATE.

## Scope and decisions

- Read [GLOSSARY.md](GLOSSARY.md) before analysis or implementation. Use its
  terms in tickets, code names, analysis and commit messages. Add a new term in
  the same change that introduces it; never use a defined word in another sense.
- Record technical decisions in [the stack](docs/dev/stack.md) only after the
  developer selects them; do not infer one decision from another.
- Use `gh` with `--repo Folt-CLI/folt`.
- Use native parent and dependency relationships. Release work defaults to epic
  #1 unless a delivery group, Question or developer instruction specifies
  another parent.
- Cross-repository references use `owner/repo#N` or a full URL. Do not assume
  issue numbers identify issues in another repository.

## Issues and analysis

- Work items have exactly Summary, Scope, Analysis, Solution and Details. Fill
  Summary/Scope at creation; follow [Issue format](docs/dev/issue-format.md)
  for the remaining sections.
- See [working templates](docs/dev/working/README.md) for how a ticket is worked
  by its size and issue type.
- Implementation agents implement and fix only; they do not analyse code, reviews or CI. The orchestrator investigates. For M and L tickets the developer selects the solution before implementation; XS and S carry their solution. Follow [Testing](docs/dev/testing.md) for roles and handoffs.
- Attach findings as <N>-code-findings.md and each solution as
  <N>-solution-<k>.md using gh-attach. Verify uploads and use real attachment links.
- Parents wait for closed children; see [closing rules](docs/dev/working/README.md#shared-steps).
- Set ticket and PR metadata; see [ticket planning](docs/dev/ticket-planning.md#at-issue-creation) and [PRs](docs/dev/workflow.md#pull-requests).
- Ticket work in its repository; see [repository rules](docs/dev/working/README.md#shared-steps).
- Use the release Epic format and closure; see [Epics](docs/dev/issue-format.md#epics).

## Commits and delivery

- When implementation finishes, follow the size's review rule in
  [working templates](docs/dev/working/README.md). The orchestrator runs the
  gate in [Testing](docs/dev/testing.md), then commits in the issue format,
  pushes and opens or updates the PR without another request. Only the developer
  merges; an agent merges only when instructed for that PR. Follow
  [Review](docs/dev/review.md) and [Stacked pull requests](docs/dev/stacked-prs.md).
- All agents, including Codex and delegated agents, must do implementation work
  in a dedicated Git worktree for the issue branch. Do not implement in the
  primary checkout or share one worktree between concurrent issue work. Fetch
  and fast-forward the base branch before creating the worktree.
- First commit: Initial[#1]: <short explanation>.
- Later commits: <Issue Type>[#<issue-number>]: <short explanation>, copying the
  exact issue type.
- Dependabot uses `Dependabot[#0]: <Dependabot's description>`, the only subject
  type that is not an issue type; `#0` needs no ticket, epic or issue. Set it
  through `commit-message.prefix` in `.github/dependabot.yml`; squash
  nonconforming PR commits into one with that subject. Always run a read-only Codex review of
  every Dependabot PR using the diff, CI result and pasted dependency release
  notes to check workflows and code. Post exactly `No issues.` when clean;
  otherwise post findings as in [Review](docs/dev/review.md).
- Keep commits small and focused; group a multi-part ticket into several commits.
- Every commit body has **Summary** (why the commit exists) and, when it touches
  more than one thing or the reason is not obvious, **Changes**: bullets of
  "what changed: why it changed". See docs/dev/workflow.md.
- Start issue branches from `develop`, or from the lower branch of a stack, and
  target PRs to `develop`, or to the branch below in a stack. Promote
  releases from `develop` to `main` through a PR. Record requested changes on
  the ticket. Use the PR title and description format in
  docs/dev/workflow.md and record delivery and verification in the PR.
- Tests are part of every ticket; follow [Testing](docs/dev/testing.md).

## Repository conventions

- Use the issue type and fields described in
  [ticket planning](docs/dev/ticket-planning.md).
- Follow .editorconfig. Keep build outputs, local configuration and credentials
  out of Git.
- Use the PR template to record changes, owning issues, performed verification
  and material risks. Do not treat template placeholders as performed checks.

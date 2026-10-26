# Work workflow

Use GitHub Issues as the durable record of a change. Keep the issue body current
as the work becomes clearer; use comments for short decisions and handoffs, and
link substantial design notes or evidence instead of copying long material into
comments.

## Stages

1. **Propose.** Create a work item or epic from its issue form. Agree on the
   outcome and scope before implementation starts.
2. **Analyse and discuss.** The orchestrator inspects the code and records
   factual findings, constraints, and compatibility concerns. Each possible
   solution is a separate Markdown attachment linked from Analysis. The
   orchestrator explains the trade-offs and may recommend an option; the
   developer chooses. Leave the Solution section pending until that choice is
   made. Then mark the chosen attachment **[Selected Solution]** and fill
   Solution with the changes, affected areas, and reason for choosing it. Split
   work that cannot be reviewed as one bounded change into linked issues.
3. **Implement.** All agents implement in a dedicated Git worktree for the
   issue branch, never in the primary checkout. Use one worktree per ticket and
   do not share a worktree between concurrent implementation work. Dependent
   tickets may use a [stacked pull request](stacked-prs.md), with each upper
   branch based on the lower branch. Keep changes within the agreed scope.
   Implementation agents implement and fix only, following the roles and
   standard prompt in [Testing](testing.md). Update the issue when a decision
   changes the plan or scope.
4. **Review.** When implementation finishes, the orchestrator reviews the diff,
   runs the gate in [Testing](testing.md), then commits in the issue's commit
   format, pushes and opens the pull request without a further request. Follow
   the [review workflow](review.md) and link the issue (for example, `Closes
   #2`). Only the developer merges; an agent merges only when the developer
   tells it to for that pull request. Record requested changes on the ticket
   and address them on the same branch before merging.
5. **Complete.** Record the delivered change and verification in the pull request.
   Link follow-up work where needed. A linked closing keyword may close the work
   item when its pull request merges.

## Branches and commits

The first repository commit is the one exception to the issue-branch workflow:
it establishes the project scaffold and its development process. Stage the
intended files explicitly, inspect the staged changes, and use a title in this
format:

```text
Initial[#<issue-number>]: <short explanation>
```

Always include a body with a `**Summary**` heading and a short explanation of
why the change was made; the initial commit may use the Summary only. The first
commit uses the active release epic's number. For example:

```markdown
**Summary**

Establish the initial Folt scaffold and development workflow so release work can
proceed from a consistent, documented baseline.
```

After the initial commit, each work item gets its own branch and dedicated Git
worktree. Before creating the worktree, fetch and fast-forward its base so the
branch starts from the current remote state. For an independent ticket, the
base is `develop`; for a dependent ticket in an open stack, use the lower
branch. All implementation agents use that worktree; the primary checkout is
not an implementation workspace. The bottom pull request targets `develop`;
upper pull requests target the preceding stack branch. See [Stacked pull
requests](stacked-prs.md). Use `<type>/<issue-number>-<short-slug>` (for example,
`feature/2-schema-migrations`). Keep one issue's work on its branch and use focused
commits with this format:

```text
<Issue Type>[#<issue-number>]: <short explanation>
```

`<Issue Type>` must exactly match the value of the issue's `Type:` label,
including capitalization and wording. For example, an issue labeled `Type: Feature`
uses `Feature[#2]: ...`; `Type: Bug` uses `Bug[#3]: ...`; and
`Type: Documentation` uses `Documentation[#4]: ...`.

### Commit size

Keep commits small and focused. When a ticket covers several distinct changes,
group them into separate commits (for example code, CI and documentation)
instead of one large commit. Where practical, each commit should build on its
own.

### Commit body

Every commit body has a `**Summary**` heading followed by one to three sentences
on why the commit exists. When a commit touches more than one thing, or the
reason is not obvious from the diff, add a `**Changes**` heading with one bullet
per notable change in the form "what changed: why it changed". Each bullet gives
the reason, not the lines changed. A one-line fix may use the Summary only. Do
not repeat the issue number (the subject has it) and do not add attribution
lines. The subject is imperative and capitalised, has no trailing period, and
stays at about 72 characters or fewer.

```text
Maintenance[#8]: Add the Rust workspace

**Summary**

Later work needs a buildable base, so the repository gets a minimal workspace
before any feature is written.

**Changes**
- Add a library crate and a CLI crate: the split keeps the core logic reusable
  by other programs.
- Mark both crates unpublishable: the API is not stable yet.
```

Stage only files belonging to the change and review the staged diff before
committing. When implementation finishes, the orchestrator reviews the diff,
runs the gate in [Testing](testing.md), then commits, pushes and opens or
updates the pull request without a further request. After pushing a commit, do
not rewrite it except for unmerged branches of a stack: `gh stack` may rebase
them and push with `--force-with-lease`. Stop and report if the local and
remote stacks have diverged. Only the developer merges; an agent merges only
when the developer instructs it to for that pull request.

## Labels and Project Foltivo

Follow [Ticket planning](ticket-planning.md) for the required issue labels,
assignee, parent and dependency relationships, Project Foltivo fields, and status
transitions. Set `Type:` at creation, `Component:` after the developer selects a
solution, and `Disposition:` when the issue is ready to close. Set Project
Foltivo `Priority` and `Size` at creation; use its `Status` field throughout
analysis and delivery. The configured project values and size-based splitting
rules are defined in that guide.

## Dependencies and grouped work

Place work tickets under an existing delivery group or Question where
applicable; independent work defaults to epic #1 unless the developer specifies
another parent. See [ticket hierarchy and starting order](ticket-hierarchy.md).
Pass the chosen parent with `--parent` when creating or editing a ticket. Do
not repeat the parent in the description. Epics contain only an Objective and
use the title `<version> -- Folt`. Track detailed work in the child issues.

A `Type: Question` ticket is the parent of tickets that derive from its answer:
work items that implement the decision, documentation that records it, and
sub-decisions that refine it. Create a derived ticket under its Question. For
L/XL derived pieces that need a parent of their own, create a delivery group
under the Question and put those pieces under the group. Answer a Question and
close it with `Disposition: Answered`; our rule is that closing a Question does
not close its children, which stay open under it. GitHub documents that closed
sub-issues count as completed in the parent's progress, and supports up to
eight nesting levels and 100 sub-issues per parent. A child that was
superseded keeps its parent and is closed with `Disposition: Superseded`. A
parent groups tickets and does not order them: add a blocked-by link for each
real prerequisite, including a sub-decision that cannot start before its
parent's answer. Keep nesting shallow: two or three levels, well below GitHub's
limits.

For example, this tree shows how a Question can group different kinds of
derived work:

```text
v0.1.0 -- Folt                         (epic, default parent)
|- Question: choose the home layout    (answered and closed)
|   |- Feature: implement the layout   (work item)
|   |- Documentation: record the layout in the stack
|   `- Question: portability of the layout   (sub-decision)
`- Question: choose the history model  (open)
```

The Question stays the parent after it closes. A sub-decision that needs its
parent's answer is also blocked by it.

Delivery-group parents are ordinary work items using the five-section template,
not additional release epics. They coordinate an outcome; implementation and
verification belong to bounded children. Preserve existing analysis and selected
solutions when regrouping. Add native blocked-by links for actual implementation
prerequisites; a shared parent does not define execution order. Analysis and UI
exploration may proceed alongside dependency work. Keep optional enhancements
from blocking the first usable slice merely because they share a parent.

## Pull requests

When implementation finishes, the orchestrator reviews the diff and runs the
gate in [Testing](testing.md), then commits in the issue format, pushes and
opens or updates the pull request without a further request. Follow
[Review](review.md) for read-only agent review, posting findings, recording
requested changes on the ticket and fixing accepted findings on the same
branch. Repeat review until a review has no findings. Only the developer
merges; an agent merges only when the developer tells it to for that pull
request.

For a stacked pull request, state its position in the stack and identify the
pull request below it. Review the pull request using [Review](review.md).

The title uses the commit subject format,
`<Issue Type>[#<issue-number>]: <short explanation>`. The description follows
[the pull request template](../../.github/PULL_REQUEST_TEMPLATE.md), in this
order:

1. **Summary:** why the pull request exists, in one to three sentences.
2. **Changes:** one bullet per notable change as "what changed: why it changed",
   grouped by commit when there are several.
3. **Related work:** the owning issue, with `Closes #N` only when the pull
   request completes it. Use a full link for another repository.
4. **Verification:** record suites and CI results as defined in
   [Testing](testing.md#verification-in-the-pr).
5. **Risks and follow-up:** material compatibility, configuration or migration
   implications, and links to remaining work.

Do not add attribution lines.

- Use a focused title and explain the change, rationale, and verification.
- Link the work issue with a closing keyword when merging should close it.
- Keep unrelated changes out of the pull request; create another issue when
  needed.
- Do not claim verification that was not run. List relevant checks and manual
  review plainly.
- Merge only when the developer merges or instructs the agent to merge, and after
  the required checks and review are complete.

## Blocked work

When work is blocked, update the issue with what is blocked, why, and the
specific next action or decision needed. Link the dependency or related issue.
Do not leave a vague `blocked` label without context. Resume by updating the
status and issue plan.

## Issue-specific developer instructions

Explicit developer instructions override the default workflow for that work.
Record any such override and its limits in the issue and the pull request. An
instruction for one issue does not authorize merges.

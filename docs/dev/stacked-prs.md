# Stacked pull requests

Use a stack when a ticket depends on implementation in another ticket whose
pull request is still open. Create a native GitHub stack with the `gh stack`
extension. A stack is a chain of pull requests in one repository: the bottom
pull request targets `develop`, and each later pull request targets the branch
below it. Each pull request shows its own layer's diff. Cross-fork stacks and
GitHub Desktop are not supported. See GitHub's [about stacked pull
requests](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs).

Keep one worktree per ticket. Stacks may span linked worktrees, but `gh stack
rebase` updates only clean worktrees. Stack state is shared
by worktrees in the clone; do not run other Git tools concurrently with stack
mutations.

## Prerequisites

Install `gh` 2.90.0 or newer, Git 2.36 or newer, and the extension:

```sh
gh extension install github/gh-stack
```

Agents may optionally install its skill with `gh skill install
github/gh-stack`. Check that stacked pull requests are enabled for this
repository. `gh stack` exit code 9 means the feature is not enabled.

## Create and submit a stack

Start the bottom branch from `develop` and add each dependent ticket's branch
above it. Use the normal issue branch name `<type>/<issue-number>-<short-slug>`.
For example:

```sh
gh stack init --base develop feature/22-foundation
gh stack add feature/23-dependent
gh stack submit
```

`gh stack init` can also adopt existing branches, listed bottom to top. Use
`gh stack link` to link existing branches or pull requests into a stack. Use
`gh stack view` to inspect its order and pull request status. See the [GitHub
CLI reference](github-cli.md#stacked-pull-requests) for the supported commands.

The bottom pull request targets `develop`; `gh stack submit` sets the chained
bases for the upper pull requests. State the stack position in each pull
request using the [pull request template](../../.github/PULL_REQUEST_TEMPLATE.md).

## Keep the stack current

Fetch remote changes, then use `gh stack rebase` to rebase the stack's branches.
Run the local suite gate in [Testing](testing.md) on every affected branch, then
use `gh stack push` only when all pass. It uses per-branch `--force-with-lease`
checks and is not atomic. Stop and report if
the local and remote stacks have diverged, a push is rejected, or a rebase
conflict cannot be resolved without discarding remote work. Otherwise, resolve
rebase conflicts, stage the resolution with `git add`, and continue with
`gh stack rebase --continue`.

CI and branch protection on the bottom pull request's base apply to every
layer. Check every pull request's status as work advances.

## Merge and ticket status

Only the developer merges a pull request. `gh stack merge` merges every stack
member up to the selected PR in one atomic, all-or-nothing operation; with no
argument it uses the current stack, or pass a PR number to merge up to that PR.
Through the GitHub UI, merge one layer at a time from the bottom up. Do not run
`gh stack merge` or a merge API unless the developer instructs you to merge
that stack.
The synchronous pull request merge API does not support stacks; the stack merge
uses GitHub's asynchronous merge endpoint. GitHub's documentation describes
automatic retargeting of upper pull requests after a bottom pull request merges
with merge commit, squash or rebase methods; the about-stacked-prs page lists all
three methods as supported. The merge-commit case has not been observed in this
repository.

Update Project Foltivo status separately for each ticket, following [Ticket
planning](ticket-planning.md). A shared stack does not change ticket ownership
or status.

## Unverified

Enablement for this repository has not been confirmed. The `line` and `side`
review-comment parameters and requesting changes on an agent-authored pull
request have not been verified here; see [Review](review.md).

# GitHub and `gh` reference

Use `gh` for repeatable issue and pull request operations. Always pass the
repository explicitly when running from a checkout that may have another
`origin`:

```sh
--repo Bojan-Ivanovski/folt
```

The GitHub issue body is the durable record. Before editing it, read the current
issue; when replacing the body, provide the complete updated text so existing
sections are preserved.

Use `Bojan-Ivanovski/folt` in every `gh --repo` argument and
maintained GitHub URL. The initial local origin is
`https://github.com/Bojan-Ivanovski/folt.git`; inspect `git remote -v`
before changing any remote configuration.

## Issues

```sh
# List open issues
gh issue list --repo Bojan-Ivanovski/folt

# Read the issue before changing it
gh issue view 2 --repo Bojan-Ivanovski/folt

# Create from a prepared issue body
gh issue create \
  --repo Bojan-Ivanovski/folt \
  --title "<focused outcome>" \
  --body-file /path/to/issue.md \
  --label "Type: Feature" \
  --parent 1

# Replace title/body or add an existing label
gh issue edit 2 \
  --repo Bojan-Ivanovski/folt \
  --title "<updated title>" \
  --body-file /path/to/complete-updated-issue.md

# Add a short decision or handoff
gh issue comment 2 \
  --repo Bojan-Ivanovski/folt \
  --body "<short update and links>"

# Close after the work is completed and verification is recorded in the PR
gh issue close 2 --repo Bojan-Ivanovski/folt
```

Use the repository's GitHub issue forms when creating a work item, epic, or
incident in the web interface. `gh issue create` accepts a title/body but does
not fill out the form interactively; use a prepared Markdown body that follows
[issue-format.md](issue-format.md).

Use the parent specified by the developer or an existing delivery group or
Question in [workflow](workflow.md#dependencies-and-grouped-work); independent
release work defaults to #1. Use the appropriate existing `Type:` label, not
necessarily Feature. Verify the native parent and labels after creation with
`gh issue view <number> --repo
Bojan-Ivanovski/folt --json parent,labels`. Do not add a parent
reference to the body.

## Markdown attachments

The built-in `--attach` flag supports images and videos. For Markdown documents,
use [sudosubin/gh-attach](https://github.com/sudosubin/gh-attach). Install it once:

```sh
gh extension install sudosubin/gh-attach
```

Document uploads require a signed-in GitHub browser session matching the `gh`
account. The extension uses that session automatically. Do not copy session
cookies into commands, logs, or issue text.

```sh
gh attach /tmp/2-code-findings.md /tmp/2-solution-1.md /tmp/2-solution-2.md \
  --repo Bojan-Ivanovski/folt --json href,name
```

Use the returned `href` values as normal Markdown links in the issue body.
Read the latest issue body first and preserve other sections. Verify every
uploaded file by downloading it and comparing it with the original:

```sh
gh attach download '<returned-url>' --output /tmp/downloaded-solution.md
cmp /tmp/2-solution-1.md /tmp/downloaded-solution.md
```

Keep uploads linked from the issue so reviewers can access them. Do not mark a
solution selected until the developer chooses it.

## Labels

Only apply labels that exist in the repository. `gh issue edit --add-label
"<label>"` adds a label without replacing the current set; `--remove-label` or
`--remove-all-labels` removes labels. Read the issue first and preserve unrelated
labels when changing metadata. The intended label groups and lifecycle are in
[workflow.md](workflow.md#labels-and-project-foltivo).

## Pull requests

```sh
# Open a PR and link the issue so GitHub can close it on merge
gh pr create \
  --repo Bojan-Ivanovski/folt \
  --title "<change>" \
  --body-file /path/to/pr-description.md

# Inspect review state before merge
gh pr view <number> --repo Bojan-Ivanovski/folt
```

The PR body should include `Closes #<issue-number>` when merging it completes
the linked work item. Do not use a closing keyword for an epic until its release
objective is delivered. A merge is a human review decision; do not merge
unless the repository's required checks and review are satisfied.

### Inspect checks

Follow [Testing](testing.md#ci-result-in-the-review) for CI analysis and the
review handoff:

```sh
gh pr checks <number> --repo Bojan-Ivanovski/folt --watch
gh run view <run-id> --repo Bojan-Ivanovski/folt --log-failed
```

### Posting a code review

Post a review with `gh api` and a JSON input file. Set `commit_id` to the head
commit being reviewed, `event` to `COMMENT`, and include a summary `body`. For
each inline comment, provide the changed `path`, `line`, `side` and comment
`body`:

```sh
gh api repos/{owner}/{repo}/pulls/{n}/reviews --input review.json
```

See [Review](review.md) for the review payload format and review cycle. Resolve
fixed threads through GraphQL `resolveReviewThread`; reply with the reason when
a finding is rejected.

### Stacked pull requests

Use `gh stack` from the [stacked pull request guide](stacked-prs.md). These
commands require the `github/gh-stack` extension:

| Command | Purpose |
| --- | --- |
| `gh stack init --base develop <branch>...` | Start a stack, optionally adopting branches bottom to top |
| `gh stack add <branch>` | Add a branch above the current stack position |
| `gh stack link <branch-or-pr>...` | Link existing branches or pull requests bottom to top |
| `gh stack view` | View stack order and pull request status |
| `gh stack rebase` | Cascade-rebase stack branches |
| `gh stack sync` | Fetch, rebase, push and sync remote stack state |
| `gh stack push` | Push stack branches with per-branch `--force-with-lease` |
| `gh stack submit` | Push branches and create or update pull requests |
| `gh stack merge` | Merge a stack; developer only |

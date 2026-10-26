# Pull request review

After a pull request is opened or updated, run a read-only Codex review of its
diff against the pull request's base. For a stack, review each pull request
against its own base. CI and branch protection on the bottom base apply to
every layer; see [Stacked pull requests](stacked-prs.md).

## Review agent

Run headless Codex with `codex exec`. The `-p` option means `--profile`; it does
not supply the prompt. `codex exec review --uncommitted` does not accept a
custom prompt. For a custom prompt, run:

```sh
codex exec -s read-only -C <worktree> -o <file> - < prompt
```

The prompt should tell the agent to review read-only, compare the PR diff with
its base, and report only actionable findings. Include an out-of-scope list.
Do not ask for new tests or dependencies; do review the tests in the diff.
Return findings with severity (High, Medium, or Low), what goes wrong, a concrete
trigger, the fix when clear, and `file:line`. Keep findings tied to the changed diff where possible.

The read-only sandbox has no network or `gh` access and cannot create the Cargo
build lock. The orchestrator posts reviews and runs the gate in
[Testing](testing.md#gate-before-push). Implementation runs use
`-s workspace-write` in the issue's worktree and leave an uncommitted diff for
the orchestrator to review and deliver through [Workflow](workflow.md#stages).

## CI result

Follow [Testing](testing.md#ci-result-in-the-review): the orchestrator fetches
and analyses CI, includes the result and failing excerpts under **CI result**
in the review prompt, and waits for CI to finish before posting the review.

## Post findings

The orchestrator posts findings as one GitHub code review using
`gh api repos/{owner}/{repo}/pulls/{n}/reviews --input <file>`. Set `commit_id`
to the reviewed head commit and `event` to `COMMENT`. Agents act as the PR
author and cannot approve their own pull request. Include a summary body with
finding counts and categories with no findings. Add one inline comment per
finding on a changed line, with `path`, `line` and `side`. Put findings in the
diff that cannot be attached to a changed line in the summary. The orchestrator
records findings about something outside the pull request's diff as incidents
using the **Incident report** form (see [Incident reports](incidents.md)),
links them from the review summary, and leaves them out of the inline comments.
Findings about changed lines stay in the pull request.

For example, `review.json` may contain:

```json
{
  "commit_id": "0123456789abcdef0123456789abcdef01234567",
  "event": "COMMENT",
  "body": "Review: 1 High, 0 Medium, 0 Low. No findings in performance or security.",
  "comments": [
    {
      "path": "src/example.rs",
      "line": 42,
      "side": "RIGHT",
      "body": "High: This can panic when the input is empty. Trigger: pass an empty list. Return an error instead."
    }
  ]
}
```

Use `gh pr review` for body-only reviews. Record requested changes on the
ticket. An implementation agent fixes accepted findings on the same branch as
further commits, and the review cycle repeats until a review has no findings.
The orchestrator resolves fixed review threads. If the orchestrator rejects a
finding, reply on its thread with the reason and do not fix it. Resolve threads
through GraphQL `resolveReviewThread`. Give the review agent rejected findings
and their reasons as context for the next round, such as in the prompt. Do not
report a rejected finding again unless the agent can show that the reason does
not hold.

## Unverified

The exact `line` and `side` parameters for inline review comments, whether an
agent-author can request changes on its own pull request, and retargeting after
a merge-commit merge have not been verified. Use `COMMENT` for agent reviews.

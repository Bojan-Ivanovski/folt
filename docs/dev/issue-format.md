# Issue format

The issue body is the shared record of why work exists, what it includes, and
how completion will be judged. Keep it short enough to scan. Put larger research
or design material in a linked document; comments should remain brief.

## Work items

Terms are defined in [GLOSSARY.md](../../GLOSSARY.md).

Use the **Work item** form for one bounded change. Follow the
[working templates](working/README.md) for what each size contains; the table
below describes sections when their stage applies.

| Section | When to fill it | Purpose |
| --- | --- | --- |
| Summary | At creation | State the current situation or user need, expected outcome, and who benefits. |
| Scope | At creation | Name affected area and what is in and out. |
| Analysis | During refinement | Record current behavior, cause when relevant, constraints, compatibility, and viable options. |
| Solution | After the developer chooses | Summarize the selected solution, affected areas, and reason for choosing it. |
| Details | After investigation | Link the code findings attachment and summarize two to four key facts. |

For a defect, describe what happens and what should happen without presuming a
cause. For new work, describe the user need and the result. Keep `In scope` and
`Out of scope` explicit. Avoid unresolved questions in an issue marked ready to
implement; resolve them during refinement or create a discovery issue.

## Analysis and solution selection

Details holds a link to the full code findings and two to four key points the
reader must not miss. State the findings and their implications; do not use this
section as a list of file paths or routes. Exact code references belong in the
attachment. Leave Details pending until investigation has produced findings.

```markdown
## Details

**Code findings:** [<N>-code-findings.md](<attachment-url>)

**Key points**
- <important finding>
- <important finding>
```

The orchestrator investigates and proposes solutions from code findings. The
developer selects the solution; a recommendation is not a selection.

1. Record facts with exact `file:line` references in `<N>-code-findings.md`.
2. Write each viable solution as `<N>-solution-<k>.md`. Offer alternatives only
   when there is a real trade-off.
3. In Analysis, summarize the current state, constraints, compatibility, and links
   to the solution attachments. Discuss the options with the developer.
4. Wait for the developer's choice before implementation. Leave Solution pending.
   If no option fits, revise the proposals.
5. Mark the chosen attachment **[Selected Solution]**, record the decision in the
   issue, and fill Solution from it.

Solution contains the selected result, not another plan document:

```markdown
## Solution

> More details are in the attachment tagged **[Selected Solution]** above.

- <what changes, one short bullet each>

**Affected areas:** <pages, routes, or components covered by the chosen solution>

**Why this one:** <one sentence>
```

Match the affected-area list to Scope: one page for a page change, a list for a
feature group, or a table for a change across the application. Copy the affected
areas from the selected solution attachment. Unselected proposals keep their
affected-area lists only in their own attachments.

Each solution attachment contains: Description, What changes, Applies to,
Outcome, Limitations, Possible issues, and Components. Findings contain facts;
solutions contain proposals. Long material stays in attachments and issue comments
stay short. Name files with the issue number so they remain identifiable.

Upload Markdown files using the `gh-attach` extension, then link their returned
URLs from the issue. See [the upload procedure](github-cli.md#markdown-attachments).
Verify the uploaded contents and issue links. Do not substitute public gists,
repository commits, or local filesystem paths for GitHub attachments. If no
signed-in browser session is available, keep upload pending and report that
specific blocker.

## Epics

An epic contains only `## Objective` followed by a quick explanation of what we
want from the release. Its title is `<version> -- Folt`. Close it with the
Disposition issue field set to Released only after all its children are closed;
see the [XL template](working/xl-epic.md).
Place release work under its existing delivery group or Question where
applicable; otherwise default to #1 unless the developer specifies another
parent. See [workflow](workflow.md#dependencies-and-grouped-work). Use native
parent relationships; the description does not repeat them.

## Notes, attachments, and links

- Use comments for concise decisions, questions, and handoffs; update the body
  when the issue's agreed scope or plan changes.
- Link long design notes and investigation findings instead of pasting them in
  full. Keep sensitive data, credentials, and private user information out of
  issues and attachments.
- Link dependencies and related work with `#<issue-number>` references.
- Pull requests should state what changed, how it was checked, and any follow-up
  work, with links to the relevant issues.

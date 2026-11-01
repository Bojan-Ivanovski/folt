# Repository settings

Issue [#108](https://github.com/Folt-CLI/folt/issues/108) records the
settings selected by the developer on 2026-10-26. The files are the desired
configuration, not evidence that GitHub has been changed. The orchestrator
applies them only after this pull request is merged and records the actual
result and verification in the delivery record. Nothing was applied during
implementation.

## Files and decisions

| File | Owns | Reason |
| --- | --- | --- |
| [repository.json](../../.github/settings/repository.json) | Description, empty homepage, topics, merge methods and repository features | Make the repository discoverable and keep merging consistent with the process |
| [protect-integration-branches.json](../../.github/settings/rulesets/protect-integration-branches.json) | One active branch ruleset for `develop` and `main` | Enforce review through a PR and the aggregate CI gate |
| [security.json](../../.github/settings/security.json) | Security features, CodeQL default setup and Actions policies | Enable private reporting and automated detection; limit workflow permissions |
| [labels.json](../../.github/labels.json) | Label names, colours and descriptions | Reuse the existing definitions without deleting extra labels or changing issue assignments |
| [repo-settings.sh](../../scripts/repo-settings.sh) | Read-only comparison and explicit application | Review changes and detect drift without giving an external app administration access |

Repository settings allow merge commits only, disable auto-merge and retain
merged branches. Stacked PRs need deliberate retargeting and branch cleanup;
see [Stacked pull requests](stacked-prs.md). Issues and Projects remain on;
Wiki and Discussions are off. Topics describe the Rust CLI and its document
workflow. The description follows the README; the homepage is empty until one
is selected.

The ruleset requires a pull request with zero required approvals. This allows
the maintainer's own PRs while keeping the PR and CI record. The `CI` check is
bound to the GitHub Actions app (integration ID `15368` on github.com), with
no requirement to update the branch before merging. The aggregate `CI` job
already waits for the matrix and MSRV jobs. Force pushes and branch deletion
are blocked on both integration branches. There are no bypass actors, including
for administrators. Agents still merge only when explicitly instructed for that
PR. The script never changes any other ruleset or classic protection on another
branch.

Security settings enable private vulnerability reporting, Dependabot alerts
and security updates, and CodeQL default setup for `actions` using
the default query suite. Secret scanning and push protection remain enabled.
CodeQL default setup does not support Rust; revisit when it does.
[SECURITY.md](../../SECURITY.md) provides the private reporting route and
support policy. CodeQL uses GitHub's default setup rather than an additional
workflow file. Setup can finish asynchronously; inspect its result on GitHub
and rerun `check` after it finishes.

Actions require full commit SHA pinning. The default `GITHUB_TOKEN` permission
is read-only and workflows cannot approve pull requests by default. Existing
workflow-level permissions still govern explicit grants; these defaults do not
replace review of workflow code. The script leaves Actions enablement and
allowed-action selections alone.

[dependabot.yml](../../.github/dependabot.yml) checks `github-actions` monthly
and groups version updates. Cargo version updates remain deferred. Security
updates are a separate repository feature; the monthly group does not control
their timing. `commit-message.prefix` sets the commit subject to
`Dependabot[#0]: <Dependabot's description>`. `Dependabot` is the only subject
type that is not a `Type:` label; `#0` means there is no ticket and none is
needed, including an epic or issue. Squash nonconforming PR commits into one
commit with that subject.

Always run a read-only Codex review on every Dependabot PR to verify that the
bump will not break workflows or code, using the diff, CI result and release
notes of the bumped dependencies. Codex has no network, so paste the CI result
and release notes into the prompt. Leave a PR comment that is exactly
`No issues.` when clean; otherwise post findings as for any
[review](review.md). No bot bypass or automatic merge is introduced.

[LICENSE-MIT](../../LICENSE-MIT) and [LICENSE-APACHE](../../LICENSE-APACHE)
provide the `MIT OR Apache-2.0` choice declared by Cargo; the README links both.
The issue chooser disables blank issues and links to the security policy.
Existing issue forms and the PR template remain the entry points.

[CODE_OF_CONDUCT.md](../../CODE_OF_CONDUCT.md) contains Contributor Covenant
version 2.1 with the developer-selected enforcement contact
0ivanovskibojan@gmail.com. The contributor guide links to this community policy.

## Check for drift

Requirements: Bash, `gh`, `jq`, and standard shell utilities including `diff`.
Run on the maintainer's machine with GitHub authentication able to read every
managed setting. Some security and administration endpoints require repository
administration privileges even for reads. The script uses github.com explicitly
and never stores credentials. Authentication is managed by `gh`; do not add a
token to the repository or CI secrets for this script.

The script is executable and can be run directly:

```sh
./scripts/repo-settings.sh check
./scripts/repo-settings.sh check owner/repository
```

Exit codes: `0` means the declared settings match, `1` means drift or pending
CodeQL provisioning, and `2` means a usage, dependency or explicitly handled
read/verification error. Other
nonzero statuses can come from `gh`, `jq` or a failed write; always treat any
nonzero exit as unsuccessful. Output shows unified differences from live
settings to the files. Response-only fields and array order are ignored.
Every declared field is compared, including false booleans.
Rules are matched by type and compared using the fields declared in the file,
including their parameter keys. Server-added defaults are ignored; changed or
missing values, missing or extra rules, enforcement, branch targets and bypass
actors still cause drift. The same comparison verifies protection migration.
Other settings already ignore undeclared object fields.
CodeQL with state `configured` and an empty language list reports `Pending`
when its other declared settings match. It is not reconfigured while pending;
rerun `check` after provisioning finishes.
Labels are read with pagination; extra labels are retained and never deleted.
A live label under `Type: `, `Component: ` or `Disposition: ` that the file lacks
is reported as drift by `check` (report only). Other extra labels, such as
Dependabot's, are ignored. The script does not rename labels: a changed name
creates another label and leaves the old one intact.

A 404 means disabled/missing only for endpoints that use it that way: alerts,
CodeQL default setup and classic `main` protection. Other failures stop the
run; a failed read never triggers a write. GitHub may mask inaccessible
resources with 404, so use an account with the required rights and investigate
unexpected missing-resource output before applying.

Run `check` as part of maintainer verification after a settings change, and
periodically to find manual drift. It is deliberately outside CI because it
would need privileged credentials. Read-only checks may be used during review;
applying settings waits until the files are merged.

## Apply after merge

Edit the JSON in a PR, have the developer select any new decisions, merge it,
and use the merged checkout. The target repository must already exist, with
`develop` and `main` and the expected `CI` workflow. An alternate repository
argument selects the API target; content such as policy links and description
still belongs to Folt and must be reviewed when adapting a fork.

First show the diff (this command writes nothing and exits nonzero without
`--yes`):

```sh
./scripts/repo-settings.sh apply
```

Then explicitly apply the reviewed settings:

```sh
./scripts/repo-settings.sh apply --yes
./scripts/repo-settings.sh check
```

The script reads all managed endpoints before its first write and writes only
settings that drift. After all other pending settings and labels, it creates
the named ruleset if absent or updates that repository ruleset by ID. Duplicate
names or an inherited ruleset with that name require manual resolution. Rulesets
with other names remain untouched.
An HTTP or write failure stops the run; there is no transaction or automatic
rollback. Fix the error, check the partial state, then rerun. Unknown live
settings are not overwritten. Changes to the repository's API shapes or feature
availability may require updating the script; no weaker fallback is applied.

### Remove classic main protection separately

The initial application retains classic protection. Confirm in GitHub that the
active `protect-integration-branches` ruleset covers both branches and requires
the expected Actions `CI` check. Then run this explicit migration step:

```sh
./scripts/repo-settings.sh apply --yes --remove-main-protection
./scripts/repo-settings.sh check
```

The command shows the current diff, applies any pending declared settings, then
re-reads the ruleset and compares its active enforcement, branch conditions,
rules and empty bypass list before deleting classic protection on `main`.
If that verification fails, it retains classic protection. A normal `apply`
never removes it; `check` reports remaining classic protection as migration
drift. No classic protection on any other branch is removed. Record the actual
application, migration and follow-up check results in the delivery record; do
not record them as complete merely because the files exist.

## Offline verification

```sh
bash -n scripts/repo-settings.sh
python3 scripts/tests/test_repo_settings.py
# When installed:
shellcheck scripts/repo-settings.sh
```

The Python tests use a fake `gh` executable and make no network requests. They
cover matching settings, drift, write confirmation, read failures, Actions
payload preservation, ruleset write order and protection migration safeguards.
Label tests cover the reverse check for extra labels under the three managed
prefixes, ignored extras and report-only behaviour during apply.
They cannot verify GitHub's current API contract, token permissions, CodeQL
provisioning, or the effect of the ruleset on real merges.
Windows shells are not verified; this script targets the maintainer's Bash
shell. The orchestrator runs the repository's normal gate before delivery.

## Not covered

- No `CODEOWNERS`, `FUNDING.yml`, `SUPPORT.md`, social preview or README badges
  are introduced. No homepage is selected.
- Cargo version updates, tag rulesets, releases and crates.io publishing belong
  to later work. Merge queue is not configured.
- Labels are synchronized from the file, which now defines
  `Disposition: Answered`. The report-only reverse check covers the `Type:`,
  `Component:` and `Disposition:` prefixes.
- Project contents, issue assignments, other repository settings and the
  developer's merge authorization are outside the script's ownership.

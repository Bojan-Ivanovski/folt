# Testing

## Rule

Every ticket adds or updates the tests for what it changes, in the same pull
request. A bug fix adds a regression case that fails before the fix. The
implementing agent writes the tests. The local suite must pass before anything
is pushed.

## Test tickets

The Test issue type is for a ticket whose whole outcome is adding test cases, such as
cases for behaviour that already exists. Work around the test cases is a
Feature: a framework, helpers, a runner or a CI job. Tests that belong to a
change stay in that change's ticket, whatever its type. Commits of a Test ticket
use the subject `Test[#N]:`.

## Roles

Implementation agents (headless Codex in the ticket's worktree) implement and
fix. They do not analyse code, reviews or CI. The orchestrator analyses: it runs
the gate, reads CI, and hands findings and failing excerpts to the agent. The
review agent reviews the diff, the tests in it and the CI result; it does not
run anything.

## Gate before push

The orchestrator runs, in the worktree:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --test '*' -- -D warnings
cargo build --locked --workspace
cargo test --locked --workspace
cargo test-pr --locked
cargo test-full --locked
```

Commit and push only when all commands pass. A failure goes back to the
implementation agent with the output. Never push a failing suite. The second
Clippy command and the aliases also cover suites excluded from the local run.
For offline work, export `CARGO_NET_OFFLINE=true` and add `--offline` to each
Cargo command that resolves dependencies.

The `*` is unquoted in the Cargo alias strings because Cargo splits arguments
without a shell. Quote it when invoking Cargo directly from a shell.

## Skipping PR builds

The CI workflow always starts and reports the required `CI` check. Its `plan`
job runs `scripts/ci-plan.sh`: only a `pull_request` event may skip `Verify` and
`MSRV`. Pushes to `develop` and `main` and manual runs always run in full. The
scheduled Performance workflow is separate and unaffected.

A PR skips only when every commit in `base..head` has a subject starting with
`Documentation[#N]:` (a numeric issue number), or its last non-empty message
line is exactly `[SKIP BUILD]`. Both are case-sensitive; carriage returns are
ignored. Only the orchestrator adds the marker, on every commit of an eligible
PR; Codex agents never add it. See [Commit body](workflow.md#commit-body).

The code-path guard overrides both rules. Any change in `crates/`, root `tests/`,
`Cargo.toml`, `Cargo.lock`, `.cargo/`, `rust-toolchain*` or `.github/workflows/`
runs in full. The script checks `git diff base...head`, including deletions and
old rename paths. In a stack, the PR's actual base and head define the range;
every commit above that base must qualify. An unmarked follow-up runs in full.
`scripts/` and `.github/settings/` are eligible today; extend the guard if their
contents later become part of CI.

The plan's decision and reason appear in its job summary. The `CI` gate requires
a successful plan and successful checks, accepting skipped checks only when
the plan says to skip. Failed or cancelled dependencies fail the gate. Do not
use GitHub's built-in `[skip ci]` syntax: skipping the workflow leaves the
required check pending.

Run the decision script's offline tests with `bash scripts/ci-plan-test.sh`.
They use temporary Git repositories and need no GitHub access.

## CI result in the review

After the pull request is opened or updated, the orchestrator waits for CI:

```sh
gh pr checks <n> --repo Folt-CLI/folt --watch
```

For every failed job it runs:

```sh
gh run view <run-id> --repo Folt-CLI/folt --log-failed
```

Trim the output to the failing step (the last 200 lines at most). A job that
failed with no clear cause is re-run once with
`gh run rerun <run-id> --repo Folt-CLI/folt --failed`; a second failure
is a finding. The text goes into the review prompt under the heading
**CI result** because the sandbox has no network. The review agent reports a
failed, cancelled or missing job as a finding (High) with the log excerpt; if
no log exists, state that plainly. The review is posted only after CI has
finished.

For a planned skip, `gh pr checks <n> --repo Folt-CLI/folt --json name,bucket`
should show `Plan` and `CI` as `pass`, and `Verify` and `MSRV` as `skipping`.
Confirm the plan's summary explains the skip; those planned skips are expected,
and must be recorded as not run in the PR verification.

## Verification in the PR

The PR's Verification section lists the local suite command and result, and
each CI job on Ubuntu, macOS and Windows with its result, filled in after CI
finishes. Say plainly what did not run.

## Standard paragraph for every Codex implementation prompt

> Every ticket adds or updates the tests for what it changes, in the same pull
> request. A bug fix adds a regression case that fails before the fix. Write the
> tests and run `cargo test --locked --workspace`. Do not add dependencies
> without the ticket's selected solution. Do not analyse code, reviews or CI;
> implement and fix only. The orchestrator runs the gate before committing or
> pushing and hands you findings and failing output to fix.

## Suites and layout

The local tier is `cargo test --workspace`: unit tests in `folt-core/src/`, the
validation suite, and doctests. It runs in seconds with no network, Docker or
extra tool at test runtime. Dependencies must already be cached for offline
builds; use `export CARGO_NET_OFFLINE=true` and `cargo test --offline --workspace`
when working without network access.

| Tier or suite | Command | Includes |
| --- | --- | --- |
| Local | `cargo test --workspace` | Unit, validation, doctests |
| Regression | `cargo test-regression` | Saved regression cases |
| End to end | `cargo test-e2e` | CLI child processes |
| Performance | `cargo test-performance` | Criterion speed benchmarks (manual or scheduled) |
| QA | `cargo test-qa` | Broader manual or environment-specific checks |
| PR | `cargo test-pr` | Unit, validation, regression, end to end |
| Full | `cargo test-full` | Unit and every declared test suite, including QA |

Use `CARGO_NET_OFFLINE=true` or append `--offline` to a test alias for offline
Cargo resolution. The performance alias passes trailing arguments to Criterion;
use the environment variable for that alias. Explicit test selection does not run doctests, so keep the
local command alongside the PR/full commands. Regression and QA are currently
empty; end to end has a version smoke test and a fixture
path check. All three targets use `test = false`, keeping them out of the plain
workspace run. The performance bench is separate from the test suites.

`tests/` is the `folt-tests` workspace member. Its `validation/`, `regression/`
and `qa/` directories each have one `main.rs` suite entry point. `tests/e2e/`
belongs to the CLI package so Cargo provides the built `folt` binary. Automatic
test and benchmark discovery is disabled in `folt-tests`; add suite modules to
their entry point. Shared helpers live in `tests/common/mod.rs`, and data lives
in `tests/fixtures/`. Cargo sets `FOLT_TESTS_DIR` to the absolute `tests/` path,
so fixture lookup does not depend on the working directory.

To add a unit test, put a small `#[test]` function in a `#[cfg(test)] mod tests`
next to the pure function. Prefer tables of meaningful boundary cases. Add a
validation test to the relevant `tests/validation/<module>.rs`, or declare a new
module in `main.rs`. Each test creates its own `common::TempHome` and passes its
lock to operations that write. The home is private on Unix and removed on drop.
Include helpers in suites that need them with:

```rust
#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
```

The allowance lets different suites use different helpers under Clippy's
`-D warnings`. Validation-specific setup lives in `validation/support.rs`.

To add a regression test, declare a module in `regression/main.rs`, record the
bug it covers, and keep the smallest reproducer. To add a data case, create a
directory under `fixtures/cases/` containing its inputs, expected results and a
short explanation. Use `common::fixture("cases/<name>")` to locate it. Shared
corpus entries go under `fixtures/corpus/` and are only added, never rewritten.
Copy mutable inputs to temporary storage before exercising them.

Tests must not change process environment, working directory or umask. Start
CLI commands as child processes; set any command-specific environment on the
child. Do not use sleeps, clocks or metadata timing to detect edits. Keep
property tests bounded and reproducible. Platform-specific tests carry
`#[cfg(unix)]` or `#[cfg(windows)]`.

Portability limit found while adding this suite: existing history operations
sync directories through `File::open(...).sync_all()` in `store.rs` and
`refs.rs`. Opening directories this way fails on Windows, so the history
validation tests expose an existing Windows blocker. Fixing directory durability
needs a separate implementation decision; these tests do not skip the failure.
The suite was run on Linux with Rust 1.98.1; Windows, macOS and Rust 1.89 have
not been verified.

## Performance

Run `cargo test-performance` for Criterion benchmarks of hashing, object store
writes and reads, drift checks, and registry access with 10 000 placements.
Byte inputs use a fixed-seed xorshift stream at 4 KiB, 1 MiB and 64 MiB; large
cases use ten samples. Files and registry rows are prepared outside measurement.
Writes remove the preceding object outside measurement, so each measured put
includes writing a new object and syncing it. Reads use the OS cache normally;
these are not cold-disk measurements. Drift cases include observing the file and,
when its size matches the base, hashing it before classifying it.

Codex and PR CI run `cargo test-performance --test` to execute each benchmark
once without statistical measurement. With cached dependencies, run it offline
with `CARGO_NET_OFFLINE=true cargo test-performance --test`. The alias forwards
arguments to Criterion after `--`, so put Cargo options before the alias's
separator by using the full command if needed:
`cargo bench --offline -p folt-tests --bench performance -- --test`.
The full suite is excluded from `cargo test --workspace`, `test-pr` and
`test-full`; CI smoke-checks it separately. Clippy's `--all-targets` includes it.

For a local comparison around a change:

```sh
cargo test-performance --save-baseline before
# Make the change, then compare without replacing the saved baseline.
cargo test-performance --baseline before
```

The Performance workflow runs on Ubuntu daily at 03:17 UTC, or manually. It
restores the last successful scheduled run's artifact from the same branch as
`old`, then runs `--baseline-lenient old` to compare and save fresh `new` results
in one run. Criterion 0.8.2 rejects combining `--baseline` and `--save-baseline`;
lenient comparison also allows new benchmarks or a missing/expired artifact.
Baseline artifacts and logs are retained for 30 days. Each benchmark's mean
change appears in the job summary; Criterion regressions produce warnings with
p < 0.05 and a 10% noise threshold (20% for store writes that sync to disk).
The workflow never runs on PRs; regressions do not fail the job. Operational
errors still fail the scheduled run so incomplete results do not become the
next baseline.

Judge a regression over several nights: shared runners vary, and one warning
is only a hint. Comparison with the preceding night can miss slow creep.
Without a previous baseline the run saves fresh results without comparison.
GitHub disables schedules after 60 days without repository activity; re-enable
the workflow if needed.

## Implementation order

Issue [#99](https://github.com/Folt-CLI/folt/issues/99) is stacked above
`maintenance/97-harness`, the branch for
[#97](https://github.com/Folt-CLI/folt/issues/97) in
[PR #101](https://github.com/Folt-CLI/folt/pull/101). #97 supplied the
harness, aliases and this document's suites and layout first; #99 extends this
file with the process rules and the gate using those existing commands.

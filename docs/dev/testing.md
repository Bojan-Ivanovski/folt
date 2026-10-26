# Testing

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
| QA | `cargo test-qa` | Broader manual or environment-specific checks |
| PR | `cargo test-pr` | Unit, validation, regression, end to end |
| Full | `cargo test-full` | Unit and every declared test suite, including QA |

Use `CARGO_NET_OFFLINE=true` or append `--offline` to an alias for offline
Cargo resolution. Explicit test selection does not run doctests, so keep the
local command alongside the PR/full commands. Regression and QA are currently
empty; end to end has a version smoke test and a fixture
path check. All three targets use `test = false`, keeping them out of the plain
workspace run. Performance targets and their alias belong to
[#100](https://github.com/Bojan-Ivanovski/folt/issues/100).

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

Check both default targets and suites excluded from the local run:

```sh
export CARGO_NET_OFFLINE=true
cargo fmt --all
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo clippy --offline --workspace --test '*' -- -D warnings
cargo build --offline --workspace
cargo test --offline --workspace
cargo test-pr --offline
cargo test-full --offline
```

The `*` is unquoted in the Cargo alias strings because Cargo splits arguments
without a shell. Quote it when invoking Cargo directly from a shell.

Portability limit found while adding this suite: existing history operations
sync directories through `File::open(...).sync_all()` in `store.rs` and
`refs.rs`. Opening directories this way fails on Windows, so the history
validation tests expose an existing Windows blocker. Fixing directory durability
needs a separate implementation decision; these tests do not skip the failure.
The suite was run on Linux with Rust 1.98.1; Windows, macOS and Rust 1.89 have
not been verified.

# Contributing to Folt

Follow the [Code of Conduct](CODE_OF_CONDUCT.md) when participating in the
community.

Start with [the development process](docs/dev/process.md) and
[the current stack](docs/dev/stack.md). Use the repository issue forms and
native parents/dependencies. Keep work bounded, attach findings and solution
options when analysis is requested, and let the developer select the solution.

Follow [the workflow](docs/dev/workflow.md), [stacked pull
requests](docs/dev/stacked-prs.md) and [review](docs/dev/review.md) for issue
branches, agent delivery, stacks and reviews. Use issue-type commit subjects,
the **Summary** and **Changes** commit body and the pull request format. Record
only verification actually performed.

Keep secrets and build outputs out of Git. Document configuration using
placeholders only.

Building the workspace requires a C compiler because `folt-core` bundles SQLite
through `rusqlite` and the Zstandard library through `zstd`: use `cc`/GCC or
Clang on Linux and macOS, and the MSVC C++ build tools on Windows.

For defects, include reproduction steps and impact. A private security reporting
channel has not been configured; do not put credentials, private data or exploit
details into public issues. No repository license has been selected yet.

## Local checks

Run the gate in [Testing](docs/dev/testing.md) before opening a pull request.
Its core commands are below; the gate also covers the excluded suites through
the second Clippy command and PR/full aliases defined there:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace
cargo test --locked --workspace
```

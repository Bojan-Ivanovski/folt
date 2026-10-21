# Contributing to Folt

Start with [the development process](docs/dev/process.md) and
[the current stack](docs/dev/stack.md). Use the repository issue forms and native
parents/dependencies. Keep work bounded, attach findings and solution options
when analysis is requested, and let the developer select the solution.

Follow [the workflow](docs/dev/workflow.md), [stacked pull
requests](docs/dev/stacked-prs.md) and [review](docs/dev/review.md) for issue
branches, agent delivery, stacks and reviews. Use exact Type: label commit subjects, the **Summary** and **Changes**
commit body and the pull request format. Record only verification actually
performed.

Keep secrets and build outputs out of Git. Document configuration using
placeholders only.

For defects, include reproduction steps and impact. A private security reporting
channel has not been configured; do not put credentials, private data or exploit
details into public issues. No repository license has been selected yet.

## Local checks

Run these before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace
```

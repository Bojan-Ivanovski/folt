<div align="center">
  <img src="https://avatars.githubusercontent.com/u/351130785?s=200" width="120" alt="Folt logo">
  <h1>Folt</h1>
  <p>Manage documents the way Git manages source code.</p>

  <a href="https://github.com/Folt-CLI/folt/actions/workflows/ci.yml"><img src="https://github.com/Folt-CLI/folt/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="License: MIT OR Apache-2.0"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/Rust-1.89%2B-orange" alt="Rust MSRV: 1.89"></a>
  <a href="#what-exists-today"><img src="https://img.shields.io/badge/status-early%20development-yellow" alt="Status: early development"></a>
</div>

## What is Folt

Folt is a standalone Rust CLI being built to manage documents, called Folios,
the way Git manages source code: locally and with remotes.
[Foltivo](https://github.com/Bojan-Ivanovski/foltivo-backend) is the first intended
remote host, much as GitHub is for Git, but Folt does not depend on it.

## What exists today

Folt is in early development. **The CLI currently only prints its version.**
The following capabilities exist in the `folt-core` library; they are not yet
exposed as CLI commands:

- **Library-level Folio names:** validation and names derived from file stems.
- **Library-level front matter and references:** inspect and edit names, IDs
  and references in a limited YAML subset while preserving unrelated content.
- **Library-level registry, history and refs:** track Folios and placements,
  record commits, and manage branches and tags.
- **Library-level placements:** track internal files, copies and links,
  detect changes, and convert between copies and links.
- **Library-level register:** plan and apply explicit-path registration of
  files and directory Folios. See [registration details](docs/dev/register.md).
- **Library-level rename:** update a Folio's registered name and internal file.
- **Library-level prune:** report or remove absent external placement records
  without deleting files. See [pruning details](docs/dev/prune.md).
- **Library-level checkout:** move a placement to a branch tip;
  internal placements can only check out `main`.

## Build from source

Use the stable Rust toolchain selected by [rust-toolchain.toml](rust-toolchain.toml).
The minimum supported Rust version is **1.89**, declared in [Cargo.toml](Cargo.toml).

Building requires a C compiler because `folt-core` bundles SQLite through
`rusqlite` and the Zstandard library through `zstd`: use `cc`/GCC or Clang on
Linux and macOS, and the MSVC C++ build tools on Windows.
See [CONTRIBUTING.md](CONTRIBUTING.md) for contributor setup and checks.

```sh
cargo build --locked --workspace
cargo run -p folt-cli
```

Running the binary currently prints `folt` followed by its package version.

## Not decided yet

The [pending decisions](docs/dev/stack.md#pending-decisions) include:

- Sync protocol: a new protocol or an adaptation of Git's.
- Local Folio and workspace format.
- Remote configuration and authentication, with authentication defined by each host.
- The CLI command set.

## Contributing

Read the [glossary](GLOSSARY.md) for Folt and process terminology.

Start with the [contributing guide](CONTRIBUTING.md) and
[development process](docs/dev/process.md). Follow the
[Code of Conduct](CODE_OF_CONDUCT.md), consult the
[security policy](SECURITY.md) for vulnerability reporting, and read
[AGENTS.md](AGENTS.md) for repository instructions.

## License

Licensed under either the [MIT License](LICENSE-MIT) or the
[Apache License, Version 2.0](LICENSE-APACHE), at your option.

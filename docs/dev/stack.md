# Stack

## Confirmed

- **Language:** Rust.
- **Repository:** this repository. `folt` is a standalone tool and does not depend
  on Foltivo; Foltivo is one host that can act as a remote.
- **Role:** manage Folios locally and exchange them with remotes, replacing Git
  synchronization for Folio content.
- **Crate structure:** a virtual Cargo workspace with `folt-core` (library) and
  `folt-cli` (the `folt` binary), selected in
  [#7](https://github.com/Bojan-Ivanovski/folt/issues/7).

## Pending decisions

- Sync protocol: an own protocol or an adaptation of Git's.
- Local Folio and workspace format.
- How remotes are configured and authenticated; each host defines its own
  authentication.
- Command set.

## Delivery status

The workspace scaffold exists: a `folt` binary that prints its version. Nothing
else is implemented.

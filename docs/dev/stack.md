# Stack

## Confirmed

- **Language:** Rust.
- **Repository:** this repository. `folt` is a standalone tool and does not depend
  on Foltivo; Foltivo is one host that can act as a remote.
- **Role:** manage Folios locally and exchange them with remotes, replacing Git
  synchronization for Folio content.
- **Crate structure:** a virtual Cargo workspace with `folt-core` (library) and
  `folt-cli` (the `folt` binary), selected in
  [#7](https://github.com/Folt-CLI/folt/issues/7).

The home and every Folio directory are trusted: whoever can change them can
change everything Folt writes. Folt does not defend against a process running as
the same user. On Unix, the home and directories Folt creates within it are
created private (`0700`); files created directly by the home and Folio directory
code use `0600`. Opening a home writable by group or others is refused; opening
one readable by them returns a warning. On other platforms the trust boundary
is documentation only and permission behavior is unchanged. This is the selected
solution for [#82](https://github.com/Folt-CLI/folt/issues/82).

## Pending decisions

- Sync protocol: an own protocol or an adaptation of Git's.
- Local Folio and workspace format.
- How remotes are configured and authenticated; each host defines its own
  authentication.
- Command set.

## Delivery status

The workspace scaffold exists: a `folt` binary that prints its version. Nothing
else is implemented.

# Explicit-path registration in folt-core

`register::plan_register(&home, paths)` is the dry run. It reads metadata and
checks readability by opening regular files without reading content. It does
not create markers, Folios or a registry. The returned plan lists distinct
resolved paths, status, byte sizes, missing markers to create, counts, the total
new bytes and up to ten largest entries. No globs or recursion are performed.

`register::apply_register(&home, &plan, confirm_large)` takes the home lock and
replans before writing. It returns the applied plan, or a blocked plan showing
refusals or required confirmation. An unchanged input returns the same plan as
the dry run. Already registered paths are no-ops. One refused path prevents the
whole batch from being registered. Files accept arbitrary bytes; registration
parses no metadata, including front matter.

A directory is one Folio anchored at `__folio__.md`. A missing marker is created
empty using create-new semantics before the registry transaction. An appearing
marker is checked and used without overwriting it. Duplicate paths, alternate
resolved spellings, and a directory named alongside its marker produce one
entry. Symlinks must be named by their target instead. Non-regular, missing and
unreadable files, paths in the Folt home, and paths inside an already registered
directory Folio are refused.

The optional `large_file_bytes` integer in the home's `config.toml` defaults to
64 MiB (`config::DEFAULT_LARGE_FILE_BYTES`). Files at or above it require
confirmation. A batch total above 256 MiB (`register::BATCH_TOTAL_BYTES`) adds
one batch confirmation note. Both checks must pass, or `confirm_large` must be
true. These are policy defaults, intended to bound unexpected memory and disk
use: `create_folio` still receives a `Vec<u8>`, and registration stores another
copy of the content. Interactive prompts and advice to prefer links belong to
the CLI.

Apply reads each source once, hashes those captured bytes, creates Folio
directories, captures internal and copy bases, and writes every Folio and both
placements in one registry transaction. There is no initial commit or ref;
status is clean with `NoHistory`, and history starts with the first edit.

Ordinary errors remove this call's Folio directories and its unchanged newly
created markers. Existing or replaced markers are preserved. A crash before
the registry transaction may leave orphan Folio directories or markers. No
orphan adoption or sweeping is attempted here: discovery and repair (#51/#15)
own that work, and retrying creates new Folios while the orphans remain. The
home lock coordinates Folt writers; it does not lock out external editors.

`register::apply_register_links(&home, &plan, confirm_large)` holds the home
lock while probing every destination, registering the batch through the same
transaction, then converting each copy independently through the placement
conversion rules. The dry run remains `plan_register`: it neither probes nor
writes. `RegisterLinkOutcome` contains the applied registration plan and a
`Linked` or `LeftAsCopy { reason }` result for every path. A conversion failure
never undoes registration or stops the other conversions. Already registered
copies are converted too, so the same request can finish an interrupted call.
Fresh Folios need no history tip. Edited, staged, off-main, internal and missing
copies are refused. Hard-linked copies are refused on Unix. Missing placements
need repair (#38).

Conversion syncs the parent after preparing the link and after its atomic
rename. If interrupted after the rename, the Copy row reads Unusable; retry
recognizes the intact link and finishes the row update. Before registration
commits, rollback is unchanged. Windows directory sync remains a no-op. With
Rust 1.89 and the current safe APIs, Windows cannot verify hard-link counts;
the hard-link count check is skipped and copy-to-link conversion proceeds.
Windows therefore cannot refuse copies with other hard links before conversion.

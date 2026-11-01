# Explicit-path registration in folt-core

`register::plan_register(&home, paths)` is the dry run. It reads metadata and
checks readability by opening regular files. For Markdown (`md` or `markdown`,
ignoring ASCII case), it reads at most 64 KiB plus 8 bytes to validate front
matter and compute a name edit. It does not create markers, Folios or a registry. The returned plan lists distinct
resolved paths, status, byte sizes, missing markers to create, counts, the total
new bytes and up to ten largest entries. No globs or recursion are performed.

`register::apply_register(&home, &plan, confirm_large)` takes the home lock and
replans before writing. It returns the applied plan, or a blocked plan showing
refusals or required confirmation. An unchanged input returns the same plan as
the dry run. Already registered paths are no-ops. One refused path prevents the
whole batch from being registered. Non-Markdown files accept arbitrary bytes;
their contents are not read during planning.

Paths can also be supplied as `RegisterRequest::new(path, name)`. The name comes
from existing `folio-name` front matter, an explicit option, or the slugged stem
(directory name for directory Folios). An explicit option with existing
`folio-name`, or on an already registered path, is refused; renames belong to
#155. Explicit names must be unique. Explicit options are reserved first, then
front matter names, then stem names in request order. Taken stem names get
`_1`, `_2`, and so on, shortening the base to fit 64 bytes. An unusable stem
requires an explicit name.

The plan exposes each name, its source and the exact proposed front matter
edit, and displays `will write folio-name: "<name>" into <path>` for writes.
Read-only or multiply hard-linked Markdown files needing a write are refused.
Changed sources block apply. Names are stored in the same registry
transaction as their Folios. During stage 2b, apply returns an error if any
entry needs a front matter write; file rewriting is implemented in the next
stage. Existing front matter names and non-Markdown names can be applied.

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
copies are refused. Hard-linked copies are refused on every platform. Missing placements
need repair (#38).

Conversion syncs the parent after preparing the link and after its atomic
rename. If interrupted after the rename, the Copy row reads Unusable; retry
recognizes the intact link and finishes the row update. Before registration
commits, rollback is unchanged. Windows directory sync remains a no-op. The source must have exactly one
hard link before the temporary link is created and again just before the rename.
If the link count cannot be read, conversion is refused with that error.

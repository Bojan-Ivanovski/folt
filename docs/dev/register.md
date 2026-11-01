# Explicit-path registration in folt-core

`register::plan_register(&home, paths)` is the dry run. It reads metadata and
checks readability by opening regular files. For Markdown (`md` or `markdown`,
ignoring ASCII case), it reads at most 64 KiB plus 8 bytes to validate front
matter and compute a name edit. It does not create markers, Folios or a registry.
The returned plan lists distinct resolved paths, status, byte sizes, missing
markers to create, counts, the total
new bytes and up to ten largest entries. No globs or recursion are performed.

`register::apply_register(&home, &plan, confirm_large)` takes the home lock and
replans before writing. It returns the applied plan, or a blocked plan showing
refusals or required confirmation. The applied plan records the captured byte
sizes, including any inserted name. Already registered paths are no-ops. One refused path prevents the
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
Markdown snapshot checks (signature and prefix) block apply when a source changed after the plan; non-Markdown sources are not snapshotted and are replanned from their current bytes. Names are stored in the same registry
transaction as their Folios. Before hashing and ingesting a Markdown file,
apply recomputes the planned edit on the full captured bytes and checks the
signature and prefix. If the key is missing, it writes a quoted `folio-name`,
creating a block when needed. An existing name needs no write, preserving the
file's bytes and metadata. A new block uses LF, including in a CRLF file; an
existing block retains its line endings and a BOM is preserved.

A directory is one Folio anchored at `__folio__.md`. A missing marker is created
empty using create-new semantics before the registry transaction, then receives
its name through the same atomic write as any other Markdown file. An appearing
marker is checked and used, adding a name only if it has none. Duplicate paths,
alternate resolved spellings, and a directory named alongside its marker produce one
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

Apply reads each source once, writes any planned name, hashes the resulting
bytes, creates Folio directories, captures internal and copy bases, and writes
every Folio and both
placements in one registry transaction. There is no initial commit or ref;
status is clean with `NoHistory`, and history starts with the first edit.

A name write creates a temporary file in the same directory, copies permissions,
syncs the file, rechecks the source signature and hard-link count, renames over
the source and syncs the directory. This costs an extra synced file write per
changed Markdown file. Replacement changes the inode and modification time;
permissions are preserved, but ownership, ACLs and extended attributes are not
copied. Files with multiple hard links are refused when a write is needed.
Directory sync is a no-op on Windows, as for placement conversion.

Ordinary errors (including transaction or commit failures) restore the original
bytes through another atomic write, only while the file still has the signature
of our write. External edits made before that check are preserved. A failed or skipped restore reports
the path and says the file keeps the name. Cleanup also removes this call's
Folio directories and unchanged newly created markers; their signatures are
updated after rewriting and restoration. Existing or replaced markers are
preserved. A crash before the registry transaction may leave orphan Folio
directories or markers, and names already written remain in the files. A fresh
plan and retry reads those names and succeeds while they are free. No
orphan adoption or sweeping is attempted here: discovery and repair (#51/#15)
own that work, and retrying creates new Folios while the orphans remain. The
home lock coordinates Folt writers; it does not lock out external editors.

### Known limit: the write window

The write is atomic: a crash never leaves a half-written file. The final change
check and the rename are two operations, because a rename cannot be made
conditional on the file's content. An edit made by another program in the gap
between them, or during the rollback, is replaced by the content register read.
It needs a write to the same file within milliseconds of the rename (editor
autosave, a sync tool or a script). Hard links are refused in the plan. Writing
in place was rejected because a crash could leave a half-written note.

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

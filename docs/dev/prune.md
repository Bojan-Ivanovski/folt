# Pruning placements in folt-core

Terms are defined in [GLOSSARY.md](../../GLOSSARY.md).

`prune::prune_placements` reports absent external placement rows. Its
`PruneOptions::default()` is a dry run; set `apply: true` explicitly to delete
rows. Callers provide the home lock, registry and placement filesystem. The
separate `gather`, pure `judge`, plan `report` and `apply` functions support
planning and rechecking. No schema changes are needed.

An empty scope selects the whole registry. Otherwise resolved placement keys
select rows at or under those paths, with whole path segments: `/old` does not
select `/old-notes`. Use `PlacementPath::resolve_directory` for a directory
scope, including a directory Folio and its nested placements. Overlapping
scopes do not duplicate rows. Internal rows count in scope and are always kept.

Only a `Missing` observation with an existing parent directory is dropped,
regardless of kind, edits, base or stage. A missing parent keeps the row with
"parent directory missing": the volume or share might be offline. Read errors,
including permissions, ENOTDIR and unreachable shares, keep the row and report
the error. Mismatching links are kept with "a different file than expected".
Copies differing from both base and stage are kept with "edited or different":
this is a note because edits and unrelated files cannot be distinguished.
Directories and other unusable entries are kept. Internal rows are always kept,
even when missing; a Gone internal row is a damaged home, owned by rebuild #51.
A Folio left with only its internal row remains registered.

Apply re-reads each row and observation under the home lock in one registry
transaction and rechecks immediately before deletion. A changed row or a file
that reappears is kept and reported. Registry failures roll back every deletion.
External editors do not take the home lock; the filesystem observation is a
point-in-time check, not a filesystem lock. Prune never removes or overwrites a
file. Repeating prune is safe; the second unchanged apply drops nothing. With
unchanged inputs the dry-run and apply reports differ only in `dry_run`.

To guard against an offline mount whose parent still exists, applying a batch
that would drop **at least five rows and strictly more than half of all rows in
scope** requires `confirm_mass_deletion: true`. Without confirmation it returns
`ConfirmationRequired` naming both counts and writes nothing. Dry run reports
these rows without requiring confirmation. Exactly half does not require it.

The plain `PruneReport` includes dropped paths and kinds. Each dropped copy's
`stage` is its optional staged `ObjectId`, even when a surviving twin holds it;
an unstaged copy has `stage: None`. Its `stage_unreachable` flag indicates whether
that stage loses its last surviving row reference. The report's
`unreachable_staged_blobs` lists the distinct `(FolioId, ObjectId)` pairs losing
their references; its length gives the count. The report also includes kept rows
with reasons, warnings, scoped/dropped/kept counts and `dry_run`. References
outside scope and internal rows also preserve stage reachability. Multiple
dropped twins holding one stage list that pair once. A file moved by hand may
still exist elsewhere; the
report warns about this for dropped rows. Interrupted moves need no special
handling: with existing parents, after destination insertion prune drops the
absent destinations; after rename it drops the absent sources. Missing immediate
parents still keep nested rows, including an interrupted directory move. The surviving rows retain identical
history, base and stage state.

Prune touches no files or objects. Unreachable staged blobs belong to ticket
#45's object garbage collection, not prune. Rebuild belongs to #51; rebind,
orphan recovery, Folio deletion, CLI command names and flags (#6) are outside
this operation. No special interrupted-move recovery is performed.

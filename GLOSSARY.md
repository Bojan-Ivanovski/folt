# Folt glossary

## How to use this file

Read this file before analysis or implementation.
Terms are exact: use each only in its defined sense, and add a term in the change that introduces it.
`Status: open: #N` means the named Question has not settled the stated meaning.

## A-Z index

- [alias](#alias)
- [base](#base)
- [behind](#behind)
- [blob](#blob)
- [branch](#branch)
- [checkout](#checkout)
- [child](#child)
- [commit](#commit)
- [copy](#copy)
- [directory Folio](#directory-folio)
- [Disposition](#disposition)
- [drift](#drift)
- [Epic](#epic)
- [Folio](#folio)
- [Folio directory](#folio-directory)
- [Folio id](#folio-id)
- [folio-name](#folio-name)
- [front matter](#front-matter)
- [gate](#gate)
- [home](#home)
- [home lock](#home-lock)
- [incident](#incident)
- [internal row](#internal-row)
- [issue type](#issue-type)
- [link](#link)
- [marker](#marker)
- [milestone](#milestone)
- [object](#object)
- [object store](#object-store)
- [orchestrator](#orchestrator)
- [owned key](#owned-key)
- [parent](#parent)
- [pin (reference field)](#pin-reference-field)
- [pin (retention ref)](#pin-retention-ref)
- [placement](#placement)
- [plan and apply](#plan-and-apply)
- [prune](#prune)
- [Question](#question)
- [reference](#reference)
- [refs table](#refs-table)
- [refusal](#refusal)
- [register](#register)
- [registry](#registry)
- [rename](#rename)
- [size](#size)
- [stage](#stage)
- [tag](#tag)
- [tip](#tip)
- [unresolved reference](#unresolved-reference)
- [unstage](#unstage)
- [work item](#work-item)
- [worktree](#worktree)

## Storage and identity

### home

- **Layer:** Storage and identity
- **Definition:** The Folt home is the directory holding the registry, Folio directories, configuration, lock and temporary files. It defaults to `.folt` in the OS home directory; `FOLT_HOME` overrides that path.
- **Code:** `crates/folt-core/src/home.rs:25` (`Home`); `crates/folt-core/src/home.rs:377` (`resolve_home`)
- **Related:** [registry](#registry), [Folio directory](#folio-directory), [home lock](#home-lock).
- **Not:** The OS home directory or a workspace marker; portability and multi-user rules remain open in [#17](https://github.com/Folt-CLI/folt/issues/17).
- **Status:** decided

### registry

- **Layer:** Storage and identity
- **Definition:** The SQLite database `registry.db` in the home records Folio existence, names, placements and cached references. Its tables are `folios`, `placements`, `refs` and `folt_metadata`.
- **Code:** `crates/folt-core/src/registry.rs:215` (`Registry`)
- **Related:** [home](#home), [Folio](#folio), [placement](#placement), [refs table](#refs-table).
- **Not:** Just the placements table, a tracked-roots list or the on-disk history refs.
- **Status:** decided

### Folio

- **Layer:** Storage and identity
- **Definition:** A document managed by Folt, identified by its own registry row rather than by a path. Explicit registration supports files with arbitrary bytes and directories anchored at a marker.
- **Code:** `crates/folt-core/src/registry.rs:87` (`FolioRow`); `crates/folt-core/src/registry.rs:413` (`folio_exists`)
- **Related:** [Folio id](#folio-id), [folio-name](#folio-name), [placement](#placement), [directory Folio](#directory-folio).
- **Not:** A placement or an automatically discovered Markdown file; missing, moved and deleted Folio lifecycle rules remain open in [#15](https://github.com/Folt-CLI/folt/issues/15).
- **Status:** decided

### Folio id

- **Layer:** Storage and identity
- **Definition:** The identity assigned to a Folio, generated as a random version 4 UUID and displayed in lowercase hyphenated form. It names the Folio directory in the home.
- **Code:** `crates/folt-core/src/folio_dir.rs:18` (`FolioId`); `crates/folt-core/src/placement_fs.rs:17` (`internal_path`)
- **Related:** [Folio](#folio), [folio-name](#folio-name), [owned key](#owned-key).
- **Not:** A content hash or a readable name; `folio-id` is an owned front matter key that Folt does not write today.
- **Status:** decided

### folio-name

- **Layer:** Storage and identity
- **Definition:** The readable, unique Folio label: 1 to 64 ASCII letters, digits, dots, underscores or hyphens, with no leading or trailing dot or hyphen and no 64-digit hexadecimal value. The code accepts upper case, preserves case when slugging and indexes names with binary collation; Markdown content carries the `folio-name` key.
- **Code:** `crates/folt-core/src/folio_name.rs:7` (`FolioName`); `crates/folt-core/src/folio_name.rs:44` (`parse`); `crates/folt-core/src/registry.rs:261` (`folios_name`)
- **Related:** [Folio id](#folio-id), [front matter](#front-matter), [rename](#rename).
- **Not:** A file name, title, branch name or cached child label; the lowercase-only wording in [#47](https://github.com/Folt-CLI/folt/issues/47) is unconfirmed.
- **Status:** open: #47

### Folio directory

- **Layer:** Storage and identity
- **Definition:** The directory `<home>/folios/<Folio id>/` that holds one Folio’s `folio.main`, `blobs/` and `refs/`. Its identity and layout information are read from the registry.
- **Code:** `crates/folt-core/src/folio_dir.rs:69` (`FolioDir`)
- **Related:** [home](#home), [Folio id](#folio-id), [object store](#object-store), [internal row](#internal-row).
- **Not:** A directory Folio in the user’s files.
- **Status:** decided

### directory Folio

- **Layer:** Storage and identity
- **Definition:** A user directory registered as one Folio through its `__folio__.md` marker. Registration creates an empty marker when missing and then gives it a folio-name.
- **Code:** `crates/folt-core/src/register.rs:34` (`DIRECTORY_MARKER`); `crates/folt-core/src/register.rs:91` (`InsideDirectoryFolio`)
- **Related:** [Folio](#folio), [marker](#marker), [register](#register).
- **Not:** The Folio directory in the home, or a recursive registration of every child path.
- **Status:** decided

### object store

- **Layer:** Storage and identity
- **Definition:** The per-Folio store of immutable objects beneath the Folio directory’s `blobs/`. Object files use an object id followed by `.blob`, regardless of object kind.
- **Code:** `crates/folt-core/src/store.rs:21` (`ObjectStore`)
- **Related:** [Folio directory](#folio-directory), [object](#object), [blob](#blob).
- **Not:** One global shared store or a directory containing only content blobs.
- **Status:** decided

## History

### object

- **Layer:** History
- **Definition:** An immutable blob, commit or tag with an id computed from the SHA-256 of its canonical encoding. The object kind is part of that encoding.
- **Code:** `crates/folt-core/src/object.rs:36` (`ObjectId`); `crates/folt-core/src/object.rs:83` (`ObjectKind`)
- **Related:** [object store](#object-store), [blob](#blob), [commit](#commit), [tag](#tag).
- **Not:** A Folio id, mutable working file or ref file; object kind means the object kind enum, not a placement or ref kind.
- **Status:** decided

### blob

- **Layer:** History
- **Definition:** An object whose body is the exact captured content bytes. Staging stores working content as a blob for a later commit.
- **Code:** `crates/folt-core/src/object.rs:84` (`Blob`); `crates/folt-core/src/working_state.rs:91` (`stage`)
- **Related:** [object](#object), [stage](#stage), [commit](#commit).
- **Not:** Every file ending in `.blob`: commit and tag objects use that extension too.
- **Status:** decided

### commit

- **Layer:** History
- **Definition:** An immutable history object recording a required content blob id, parents, author, time, message, optional body and optional group. The commit operation publishes the object, advances the branch ref and updates the affected working state.
- **Code:** `crates/folt-core/src/object.rs:650` (`Commit`); `crates/folt-core/src/history.rs:189` (`apply_commit`)
- **Related:** [blob](#blob), [branch](#branch), [stage](#stage), [tip](#tip).
- **Not:** A Git commit of this repository or a placement’s recorded starting commit id.
- **Status:** decided

### branch

- **Layer:** History
- **Definition:** A named ref under `refs/heads/` pointing to a commit of one Folio. The fixed default branch is `main`, and a placement’s `head` stores the branch name it follows.
- **Code:** `crates/folt-core/src/refs.rs:83` (`Kind`); `crates/folt-core/src/layout.rs:20` (`DEFAULT_BRANCH`)
- **Related:** [commit](#commit), [tip](#tip), [placement](#placement), [checkout](#checkout).
- **Not:** The working file `folio.main` or a Git branch of this repository; ref kind means the ref kind enum, not the object or placement kind enums.
- **Status:** decided

### tag

- **Layer:** History
- **Definition:** A named ref under `refs/tags/` that can point to a commit directly or to a tag object carrying a target, name, tagger, time and message.
- **Code:** `crates/folt-core/src/refs.rs:83` (`Kind`); `crates/folt-core/src/object.rs:736` (`Tag`)
- **Related:** [branch](#branch), [commit](#commit), [object](#object).
- **Not:** A branch that advances with commits or a pin reference field.
- **Status:** decided

### pin (retention ref)

- **Layer:** History
- **Definition:** A ref under `refs/pins/` that retains a commit in the child Folio’s history. Its name combines the referring parent’s Folio id and the commit id.
- **Code:** `crates/folt-core/src/refs.rs:67` (`pin_name`); `crates/folt-core/src/refs.rs:83` (`Kind`)
- **Related:** [commit](#commit), [pin (reference field)](#pin-reference-field), [reference](#reference).
- **Not:** The optional text field of a declared reference.
- **Status:** decided

### tip

- **Layer:** History
- **Definition:** The commit id currently held by a branch ref. A placement records its own starting commit separately, so it can differ from the tip.
- **Code:** `crates/folt-core/src/working_state.rs:32` (`gather`); `crates/folt-core/src/placement_rules.rs:109` (`Behind`)
- **Related:** [branch](#branch), [commit](#commit), [behind](#behind).
- **Not:** The branch name stored in `head`, a content blob id or an ancestry comparison.
- **Status:** decided

## Placements and working state

### placement

- **Layer:** Placements and working state
- **Definition:** A registry row identifying where a Folio appears, with a resolved path, Folio id, kind, branch, recorded commit, optional base and optional stage. The path is the key, and the kind is link, copy or internal.
- **Code:** `crates/folt-core/src/registry.rs:47` (`Placement`); `crates/folt-core/src/placement.rs:9` (`PlacementKind`)
- **Related:** [link](#link), [copy](#copy), [internal row](#internal-row), [base](#base), [stage](#stage).
- **Not:** The file itself or the second in-memory placement type, which has no Folio id or stage; type renames wait for [#178](https://github.com/Folt-CLI/folt/issues/178).
- **Status:** decided

### link

- **Layer:** Placements and working state
- **Definition:** A placement whose path is a symbolic link to `folio.main`, always on `main` and without a base, recorded commit or stage of its own. Working operations through an intact link use the internal row.
- **Code:** `crates/folt-core/src/placement.rs:10` (`Link`); `crates/folt-core/src/working_state.rs:244` (`resolve_working`); `crates/folt-core/src/placement_fs.rs:37` (`link_target`)
- **Related:** [placement](#placement), [internal row](#internal-row), [copy](#copy), [drift](#drift).
- **Not:** A Markdown link, hard link or declared reference.
- **Status:** decided

### copy

- **Layer:** Placements and working state
- **Definition:** A placement whose path holds a regular file with its own bytes and working state. It is the default user placement kind and can follow a branch independently of other copies.
- **Code:** `crates/folt-core/src/placement.rs:11` (`Copy`); `crates/folt-core/src/placement.rs:17` (`DEFAULT`)
- **Related:** [placement](#placement), [base](#base), [stage](#stage), [behind](#behind).
- **Not:** A definition of duplicating a Folio’s identity; that remains open in [#16](https://github.com/Folt-CLI/folt/issues/16).
- **Status:** decided

### internal row

- **Layer:** Placements and working state
- **Definition:** The registry placement of kind internal for `<home>/folios/<Folio id>/folio.main`, constrained to branch `main`. It holds that file’s working state, shared by links, and is never a user-selected placement kind.
- **Code:** `crates/folt-core/src/registry.rs:105` (`internal_placement`); `crates/folt-core/src/placement.rs:12` (`Internal`)
- **Related:** [placement](#placement), [link](#link), [branch](#branch), [Folio directory](#folio-directory).
- **Not:** The file itself: call the file `folio.main` and the row the internal row on `main`.
- **Status:** decided

### base

- **Layer:** Placements and working state
- **Definition:** The content hash and file signature captured when a copy or internal working file is created or refreshed. The signature contains size, modification time, change time and inode, with unavailable metadata represented as absent.
- **Code:** `crates/folt-core/src/placement.rs:94` (`PlacementBase`); `crates/folt-core/src/placement.rs:48` (`Signature`)
- **Related:** [copy](#copy), [internal row](#internal-row), [drift](#drift), [stage](#stage).
- **Not:** A merge base, PR base branch or name prefix before a numeric suffix; links have no base.
- **Status:** decided

### drift

- **Layer:** Placements and working state
- **Definition:** The result of comparing a copy or internal working file with its base, or observing a link. The states are InSync, PlacementEdited, Gone, Detached and Unusable.
- **Code:** `crates/folt-core/src/placement.rs:78` (`Drift`); `crates/folt-core/src/placement.rs:114` (`classify`)
- **Related:** [base](#base), [link](#link), [copy](#copy), [behind](#behind).
- **Not:** Whether a branch tip moved; the working-status edit enum means Clean or Edited, while the front matter edit type represents a byte-span replacement.
- **Status:** decided

### stage

- **Layer:** Placements and working state
- **Definition:** To capture working bytes as a blob, record its hash and size for the next commit, and rebuild that Folio branch’s cached declared references. Bytes equal to the base clear the stage; later changes to staged bytes show StagedThenModified.
- **Code:** `crates/folt-core/src/working_state.rs:91` (`stage`); `crates/folt-core/src/placement.rs:101` (`StagedBlob`); `crates/folt-core/src/working_state.rs:18` (`Staged`)
- **Related:** [blob](#blob), [commit](#commit), [unstage](#unstage), [refs table](#refs-table).
- **Not:** Git staging or a development workflow stage; staging does not change the base.
- **Status:** decided

### unstage

- **Layer:** Placements and working state
- **Definition:** To clear a placement’s recorded stage without reading or changing the working file. This operation does not rebuild the cached references.
- **Code:** `crates/folt-core/src/working_state.rs:135` (`unstage`)
- **Related:** [stage](#stage), [placement](#placement), [refs table](#refs-table).
- **Not:** Discarding file edits, deleting a blob or resetting the branch.
- **Status:** decided

### behind

- **Layer:** Placements and working state
- **Definition:** A comparison between a placement’s recorded commit and its branch tip. Behind means both exist and differ; the other results are UpToDate, NoHistory, Unrecorded and MissingTip.
- **Code:** `crates/folt-core/src/placement_rules.rs:109` (`Behind`); `crates/folt-core/src/working_state.rs:55` (`judge_working`)
- **Related:** [tip](#tip), [branch](#branch), [drift](#drift), [checkout](#checkout).
- **Not:** Proof that one commit is an ancestor of another; the current comparison does not walk history.
- **Status:** decided

### checkout

- **Layer:** Placements and working state
- **Definition:** To move a copy or internal row to a branch tip through the placement move rules, resolving an intact link to its internal row first. Internal rows can only check out `main`; an edited working file requires explicit discard permission only when its content differs from the target commit’s content, and can be checked out without discard when it already matches.
- **Code:** `crates/folt-core/src/checkout.rs:40` (`checkout_placement`); `crates/folt-core/src/checkout.rs:20` (`resolve`); `crates/folt-core/src/placement_rules.rs:214` (`judge_move`)
- **Related:** [branch](#branch), [tip](#tip), [copy](#copy), [internal row](#internal-row).
- **Not:** Checking out a tag or detached commit id, or moving the branch ref itself.
- **Status:** decided

## Front matter and references

### front matter

- **Layer:** Front matter and references
- **Definition:** The optional YAML-subset block at the start of Markdown content, after an optional UTF-8 BOM, opens with a `---` line and closes with a `---` or `...` line. Its closing delimiter must start before byte 65536; Folt edits owned keys while preserving foreign content.
- **Code:** `crates/folt-core/src/front_matter.rs:119` (`locate`); `crates/folt-core/src/front_matter.rs:14` (`SCAN_LIMIT`)
- **Related:** [owned key](#owned-key), [folio-name](#folio-name), [reference](#reference), [marker](#marker).
- **Not:** General YAML support or a separate metadata file.
- **Status:** decided

### marker

- **Layer:** Front matter and references
- **Definition:** The `__folio__.md` file anchoring a directory Folio and carrying its Markdown content and front matter. Registration creates it when missing and can insert a missing folio-name into an existing marker.
- **Code:** `crates/folt-core/src/register.rs:34` (`DIRECTORY_MARKER`); `crates/folt-core/src/register.rs:149` (`creates_marker`)
- **Related:** [directory Folio](#directory-folio), [front matter](#front-matter), [register](#register).
- **Not:** A workspace `.folt/`, home format file, object codec byte, conflict marker or build-skip marker.
- **Status:** decided

### owned key

- **Layer:** Front matter and references
- **Definition:** A front matter key reserved for Folt: `folio-id`, `folio-name` or `folio-references`. Folt validates these keys and rejects spellings or structures that could hide one; it does not write `folio-id` today.
- **Code:** `crates/folt-core/src/front_matter.rs:15` (`OWNED`)
- **Related:** [front matter](#front-matter), [Folio id](#folio-id), [folio-name](#folio-name), [reference](#reference).
- **Not:** A foreign key, or the older `references` spelling of `folio-references`.
- **Status:** decided

### reference

- **Layer:** Front matter and references
- **Definition:** A declared relationship from one Folio to another under `folio-references`, keyed by an alias and naming exactly one target name or id, with an optional pin field. Staging caches declared references per parent Folio and branch.
- **Code:** `crates/folt-core/src/front_matter.rs:440` (`Reference`); `crates/folt-core/src/registry.rs:662` (`rebuild_references`)
- **Related:** [alias](#alias), [pin (reference field)](#pin-reference-field), [unresolved reference](#unresolved-reference), [refs table](#refs-table).
- **Not:** An on-disk history ref or a computed Markdown-body link; local path selection remains open in [#28](https://github.com/Folt-CLI/folt/issues/28).
- **Status:** decided

### alias

- **Layer:** Front matter and references
- **Definition:** The key assigned to a declared reference inside one parent’s `folio-references` block. It is unique within that block and is stored as the reference row’s key.
- **Code:** `crates/folt-core/src/front_matter.rs:441` (`alias`); `crates/folt-core/src/registry.rs:38` (`key`)
- **Related:** [reference](#reference), [refs table](#refs-table), [folio-name](#folio-name).
- **Not:** A Folio name, symbolic link or the older local-path alias concept.
- **Status:** decided

### pin (reference field)

- **Layer:** Front matter and references
- **Definition:** The optional pin text attached to a declared reference. The parser and registry currently preserve it as opaque text; resolving tags and storing validated commit hashes is pending work.
- **Code:** `crates/folt-core/src/front_matter.rs:444` (`pin`); `crates/folt-core/src/registry.rs:42` (`pin`)
- **Related:** [reference](#reference), [pin (retention ref)](#pin-retention-ref), [commit](#commit).
- **Not:** A retention ref or a commit hash already validated by the current stage operation; [#13](https://github.com/Folt-CLI/folt/issues/13) supplies the intended model and [#152](https://github.com/Folt-CLI/folt/issues/152) owns its implementation.
- **Status:** decided

### unresolved reference

- **Layer:** Front matter and references
- **Definition:** A cached reference whose name does not resolve to a registered Folio. Its row keeps the target name in child_label and an empty child id; edge is an informal synonym for reference.
- **Code:** `crates/folt-core/src/registry.rs:37` (`ReferenceRow`); `crates/folt-core/src/registry.rs:662` (`rebuild_references`)
- **Related:** [reference](#reference), [alias](#alias), [refs table](#refs-table).
- **Not:** An undecodable front matter key, which the parser calls an unresolved key.
- **Status:** decided

### refs table

- **Layer:** Front matter and references
- **Definition:** The registry table caching declared references, keyed by parent Folio id, branch and alias key. Staging replaces that branch’s rows with parsed targets, labels and optional pin text.
- **Code:** `crates/folt-core/src/registry.rs:264` (`refs`); `crates/folt-core/src/registry.rs:662` (`rebuild_references`)
- **Related:** [registry](#registry), [reference](#reference), [stage](#stage), [unresolved reference](#unresolved-reference).
- **Not:** The `refs/` directory or the history ref store of branches, tags and retention pins.
- **Status:** decided

## Operations

### register

- **Layer:** Operations
- **Definition:** To start managing explicit paths by creating Folio identities, names, stored content and initial copy and internal rows in one registry transaction. Markdown paths receive a missing folio-name; one refused path blocks the batch, and registration creates no initial commit or ref.
- **Code:** `crates/folt-core/src/register.rs:186` (`plan_register`); `crates/folt-core/src/register.rs:668` (`apply_register`)
- **Related:** [Folio](#folio), [folio-name](#folio-name), [placement](#placement), [marker](#marker), [plan and apply](#plan-and-apply).
- **Not:** Recursive discovery, a Git add or an initial history commit.
- **Status:** decided

### plan and apply

- **Layer:** Operations
- **Definition:** The operation pattern in which facts are read, judged into an allowed plan or refusal, then rechecked before writes. Callers hold the home lock across gathering, judging and applying history and placement changes; registration apply takes the lock and replans.
- **Code:** `crates/folt-core/src/history.rs:40` (`gather_commit`); `crates/folt-core/src/history.rs:114` (`judge_commit`); `crates/folt-core/src/history.rs:189` (`apply_commit`); `crates/folt-core/src/register.rs:668` (`apply_register`)
- **Related:** [refusal](#refusal), [home lock](#home-lock), [register](#register), [prune](#prune).
- **Not:** The CI plan job, a promise that every apply acquires its own lock, or an Implementation plan issue section.
- **Status:** decided

### refusal

- **Layer:** Operations
- **Definition:** A domain decision that a requested operation is not allowed under the observed facts. Placement rules represent it as a path plus a reason; history and registration have their own refusal types.
- **Code:** `crates/folt-core/src/placement_rules.rs:1088` (`Refusal`); `crates/folt-core/src/placement_rules.rs:1063` (`RefusalReason`); `crates/folt-core/src/history.rs:598` (`Refusal`); `crates/folt-core/src/register.rs:84` (`RefusalReason`)
- **Related:** [plan and apply](#plan-and-apply), [placement](#placement), [register](#register).
- **Not:** One shared error type: qualify the module for each refusal or refusal reason; renames wait for [#178](https://github.com/Folt-CLI/folt/issues/178).
- **Status:** decided

### rename

- **Layer:** Operations
- **Definition:** To change a Folio’s registered name and, when its internal content has a name to replace, update that content and commit on `main`. Copies retain their bytes and bases, and apply checks referrers before changing the name.
- **Code:** `crates/folt-core/src/rename.rs:26` (`RenamePlan`); `crates/folt-core/src/rename.rs:127` (`plan_rename`); `crates/folt-core/src/rename.rs:253` (`apply_rename`)
- **Related:** [folio-name](#folio-name), [internal row](#internal-row), [commit](#commit), [reference](#reference).
- **Not:** Moving a path, rewriting parent files or changing the Folio id.
- **Status:** decided

### prune

- **Layer:** Operations
- **Definition:** To report or drop absent external placement rows whose parent directories exist, keeping internal rows and leaving files and objects untouched. It defaults to a dry run, rechecks before deletion and requires confirmation for mass deletion.
- **Code:** `crates/folt-core/src/prune.rs:177` (`judge`); `crates/folt-core/src/prune.rs:352` (`prune_placements`)
- **Related:** [placement](#placement), [internal row](#internal-row), [stage](#stage), [home lock](#home-lock).
- **Not:** Object garbage collection or Folio deletion; a missing path alone does not settle the lifecycle rules in [#15](https://github.com/Folt-CLI/folt/issues/15).
- **Status:** decided

### home lock

- **Layer:** Operations
- **Definition:** The exclusive file-lock guard coordinating Folt writers in one home. It must cover each operation’s required reads, decisions and writes, with exclusive guard access for operations that require it.
- **Code:** `crates/folt-core/src/home.rs:250` (`HomeLock`)
- **Related:** [home](#home), [plan and apply](#plan-and-apply), [register](#register), [prune](#prune).
- **Not:** A filesystem lock against external editors or permission to share one implementation worktree.
- **Status:** decided

## Process

### Question

- **Layer:** Process
- **Definition:** An issue whose outcome is a decision selected by the developer and whose derived work belongs in child issues. L is always a Question; it can be Done with Disposition Answered before its children close, but cannot itself close until they all close.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [work item](#work-item), [child](#child), [Disposition](#disposition), [size](#size).
- **Not:** An unselected agent recommendation or permission to close a parent with open children.
- **Status:** decided

### work item

- **Layer:** Process
- **Definition:** An issue for bounded work, with exactly Summary, Scope, Analysis, Solution and Details. Summary and Scope are filled at creation; the size template determines analysis, review and solution readiness.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [size](#size), [issue type](#issue-type), [Question](#question).
- **Not:** An Epic’s Objective-only body or the incident report format.
- **Status:** decided

### Epic

- **Layer:** Process
- **Definition:** An XL release issue that groups work, titled `<version> -- Folt` with only an Objective in its body. It needs no analysis or review of its own and closes with Disposition Released after all children close.
- **Code:** none (process term; see [working/xl-epic.md](docs/dev/working/xl-epic.md))
- **Related:** [parent](#parent), [child](#child), [milestone](#milestone), [size](#size).
- **Not:** A directly implemented XL work item; its Status path and Component count remain unspecified in the working templates.
- **Status:** decided

### incident

- **Layer:** Process
- **Definition:** A report recording a problem, observed evidence and user impact. Investigation, mitigation and permanent fixes are separate linked work items.
- **Code:** none (process term; see [incidents.md](docs/dev/incidents.md))
- **Related:** [work item](#work-item), [issue type](#issue-type).
- **Not:** The implementation ticket for the fix or an assumed root cause.
- **Status:** decided

### orchestrator

- **Layer:** Process
- **Definition:** The role that analyses work, hands findings to implementation agents, runs the gate and delivers commits and pull requests. The developer selects solutions and merges unless explicitly delegating a particular merge.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [gate](#gate), [worktree](#worktree), [Question](#question).
- **Not:** The implementation agent, whose role is to implement and fix.
- **Status:** decided

### worktree

- **Layer:** Process
- **Definition:** A dedicated Git working directory for one issue branch where implementation takes place. Concurrent issue work uses separate worktrees.
- **Code:** none (process term; see [workflow.md](docs/dev/workflow.md))
- **Related:** [work item](#work-item), [orchestrator](#orchestrator), [gate](#gate).
- **Not:** The primary checkout or the external files Folt manages as placements.
- **Status:** decided

### gate

- **Layer:** Process
- **Definition:** The required local verification the orchestrator runs before a push, with every required command passing. The CI gate separately requires a successful plan and successful or explicitly planned skipped checks.
- **Code:** none (process term; see [testing.md](docs/dev/testing.md))
- **Related:** [orchestrator](#orchestrator), [worktree](#worktree).
- **Not:** A read-only review or an unperformed checklist; the Testing guide defines the commands.
- **Status:** decided

### size

- **Layer:** Process
- **Definition:** The ticket’s scope category: XS is self-descriptive, S has a decided solution, M needs analysis and review, L is a Question split into children, and XL is an Epic. XS needs neither analysis nor review; S needs review but no analysis.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [work item](#work-item), [Question](#question), [Epic](#epic).
- **Not:** A time estimate or permission to implement L or XL as one change; use the working templates’ current statuses, not the older guide values.
- **Status:** decided

### issue type

- **Layer:** Process
- **Definition:** The issue’s kind of work: Task, Bug, Feature, Question, Epic, Maintenance, Documentation, Incident or Test. Native issue types replace Type labels, and the exact type is used in commit subjects.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [work item](#work-item), [Question](#question), [Epic](#epic), [incident](#incident).
- **Not:** A Component field, priority or size.
- **Status:** decided

### Disposition

- **Layer:** Process
- **Definition:** The issue field recording the outcome before closure, replacing the older Disposition label. A parent may have its Disposition and be Done before closure while children remain open.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [Question](#question), [Epic](#epic), [parent](#parent), [child](#child).
- **Not:** The Status field; current Status values are S0: To Do, S1: In Analysis, S3: Ready For Implementation, S4: In Implementation and S5: Done.
- **Status:** decided

### milestone

- **Layer:** Process
- **Definition:** The release assignment required on every ticket and pull request. Use the release milestone for the work, currently `0.1.0`, or `Backlog` otherwise.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [Epic](#epic), [work item](#work-item).
- **Not:** A parent relationship, priority or Status.
- **Status:** decided

### parent

- **Layer:** Process
- **Definition:** The issue that groups another issue through a native parent relationship. Derived work belongs under its Question; otherwise use an existing delivery group or release Epic unless instructed otherwise.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [child](#child), [Question](#question), [Epic](#epic).
- **Not:** A dependency ordering or the parent Folio of a declared reference; qualify that as reference parent.
- **Status:** decided

### child

- **Layer:** Process
- **Definition:** An issue grouped under a parent through the native child relationship. A parent cannot close until all children close, and actual prerequisites use separate dependency links.
- **Code:** none (process term; see [working/README.md](docs/dev/working/README.md))
- **Related:** [parent](#parent), [work item](#work-item), [Question](#question).
- **Not:** A reference target Folio; qualify that as reference child.
- **Status:** decided

## Terms in conflict

These rows distinguish existing meanings without selecting future behavior.
Issue links refer to Folt-CLI/folt, including tickets originally linked under
Bojan-Ivanovski/folt in the findings. Type renames belong to
[#178](https://github.com/Folt-CLI/folt/issues/178).

| Term | Meanings found | Decided meaning, or open Question |
| --- | --- | --- |
| internal row | Registry row; internal placement; internal file; internal main file; `folio.main` | Internal row means the registry row; `folio.main` means the file. Say the internal row on `main`. |
| stale | Failed ref compare-and-swap; differing branch tip; old child label; reference cache older than edits; obsolete path | Say failed ref update, behind, cached child label or references from the last stage. Merge and rebase have a selected solution in [#74](https://github.com/Folt-CLI/folt/issues/74), but are not implemented here; local path resolution is open: [#28](https://github.com/Folt-CLI/folt/issues/28). |
| name | Folio label; path-derived label; reference target name; child_label; ref name; object hash; path holding a reserved label | Use folio-name for the Folio label, ref name for history refs, and object id for the hash. Upper case works today; lowercase-only policy is open: [#47](https://github.com/Folt-CLI/folt/issues/47). |
| link | Symbolic-link placement; link row; Markdown link; hard link | Link means the symbolic-link placement; qualify Markdown links and hard links. The row records the expected kind even if the filesystem entry changes. |
| copy | Regular-file placement; duplicated Folio or directory; ordinary copying action | Copy means the regular-file placement. Identity after duplication is open: [#16](https://github.com/Folt-CLI/folt/issues/16). |
| main | Folio default branch; `folio.main`; repository release branch | Qualify Folio branch `main`, file `folio.main` and Git branch `main`. |
| stage and staged | Capturing a blob; recorded hash and size; Staged enum; refreshing cached references; issue workflow stage; Git staging | Stage means the Folt working-state operation and its recorded blob; qualify workflow stages and Git staging. |
| base | Placement hash and signature; merge base; PR base branch; name prefix before a suffix | Base means placement content and signature; qualify merge base, PR base branch and name prefix. |
| home | Folt home; OS home; former home after a move; retired workspace marker | Home means the Folt storage directory; qualify OS home and former home. Portability, restore and multi-user rules are open: [#17](https://github.com/Folt-CLI/folt/issues/17). |
| Folio directory and directory Folio | Storage directory in the home; registered user directory | Folio directory means storage; directory Folio means the user directory anchored at a marker. |
| placement and same-named types | Registry and in-memory Placement; working-state and front-matter Edit; history Refusal and placement Refusal/RefusalReason and registration RefusalReason; ref Kind, ObjectKind and PlacementKind | Placement means the registry row; qualify the in-memory type. Working-state Edit means Clean/Edited; front-matter Edit means a byte-span replacement. Refusal types and kind enums require module or domain qualification; renames wait for [#178](https://github.com/Folt-CLI/folt/issues/178). |
| ref, refs, reference, edge, pin | History pointers; registry reference rows; declared relationships; informal edge; retention pin; reference pin field | Reference is the declared relationship, with edge an informal synonym. Distinguish the refs table from history refs and retention pins from opaque reference pin fields; [#152](https://github.com/Folt-CLI/folt/issues/152) implements commit-hash pins. |
| unresolved | Unknown reference target; undecodable front matter key | Use unresolved reference for the cached unknown target, and unresolved key for parser ambiguity. |
| marker | `__folio__.md`; workspace `.folt/`; home format file; codec marker; planned conflict markers; build-skip marker | Marker means `__folio__.md`; qualify codec, conflict and build-skip markers. Workspace and home format markers are retired below. |
| plan and apply | Judged operation plan; dry-run report; CI plan job; forbidden Implementation plan issue section | Plan and apply means the operation pattern; qualify a CI plan. Registration and prune expose dry-run reports; do not add an Implementation plan section to work items. |
| commit | Folt history object or operation; placement’s starting commit id; repository Git commit | Commit means Folt history in domain text; qualify Git commits and a placement’s recorded commit. |
| head, tip, branch | Placement branch name; branch’s current commit; removed Folio HEAD file; Git HEAD | Head is the placement’s branch name, tip is the ref’s commit id, branch is the named history ref. Qualify Git HEAD. |
| registry | SQLite database; retired tracked-roots registry; placement registry as the placements table | Registry means the database; say placements table for that subset. Missing, moved and deleted Folio lifecycle rules are open: [#15](https://github.com/Folt-CLI/folt/issues/15). |
| reference form | Older `references` key; implemented `folio-references` key | The owned key is `folio-references`; its alias maps to exactly one of name or id, with an optional pin. |

## Retired words

These replacements apply to Folt terminology; historical quotations retain
their original wording. A Git HEAD or Git index is still qualified as Git.

| Retired word | Current word or representation | Ticket that retired it |
| --- | --- | --- |
| tracked roots | Individually registered Folios and placements | [#27](https://github.com/Folt-CLI/folt/issues/27); absence of tracked roots is explicit in [#15](https://github.com/Folt-CLI/folt/issues/15). |
| `.folt/` marker | Global Folt home, defaulting to the OS home’s `.folt` directory | [#14](https://github.com/Folt-CLI/folt/issues/14). This retires the workspace-marker meaning, not the default directory name. |
| home format marker | format_version in the registry’s folt_metadata table | [#65](https://github.com/Folt-CLI/folt/issues/65). |
| `folio.toml` | Markdown front matter; non-Markdown Folios have no metadata today | [#72](https://github.com/Folt-CLI/folt/issues/72). |
| `record` | Registry Folio format and placement base fields | [#63](https://github.com/Folt-CLI/folt/issues/63) and [#64](https://github.com/Folt-CLI/folt/issues/64). |
| `index` | Placement row’s staged blob hash and size | [#42](https://github.com/Folt-CLI/folt/issues/42). |
| `HEAD` | Placement row’s head branch and recorded commit | [#73](https://github.com/Folt-CLI/folt/issues/73). |
| main side | The internal row on `main` | Wording standardized by [#179](https://github.com/Folt-CLI/folt/issues/179); no earlier code meaning was found. |

# Persistent native source admission

`PersistentRootContext` comes from an actual `Connection` instance owner and
either the default session's `settings::SourceProfile` or an explicitly selected
state directory. `SourceProfile` shares the actual profile lock owner, without
cloning its file descriptor, and repeats named/held identity, regular-file,
single-link and profile-manifest checks. Default source storage uses the held
profile's persistent Data location. Temporary runtime discovery is not a default
source-storage authority.

Constructing the context or `SourceRuntime`, or cloning its resident/disk pools,
creates no source directory or ledger file. Nonzero source I/O explicitly begins
a `SourceSession` outside engine, registry and UI locks. Its operation retains
both instance-request admission and the existing workspace file-operation gate
until the last source read/write finishes. Cached immutable surfaces must retain
their source owners without indefinitely caching an active session.

The first source session admits `native-sources-v1` beneath the actual persistent
base. An exclusive held namespace-owner file serializes inventories. An existing
nonempty directory without a recognized owner fails closed. Startup inspects only
recognized entries in this owned namespace, retains file identities, rejects
aliases and multiple links, and counts all source and incomplete staging files
into the same global `DiskBudget` already exposed to surfaces. It repeats every
observation after its last admission callback. Unknown/partial entries are kept;
they cannot become transparent pixels or an inferred newest checkpoint.

Runtime directory/file/path indexes have a conservative resident reservation.
Source block/cache index accounting remains a separate required admission before
resident eviction or larger-canvas activation. A derived mirror of existing
resident rasters does not establish bounded total application memory.

Stores are resolved by inventoried immutable identity and an explicit selected
`CommitRef`. Serialized recovery data never supplies runtime paths or offset
constructors. A warm registered owner must be the same shared Store allocation.
Sealing a batch preserves that Store's original lineage and advances its checked
checkpoint sequence; receiving document IDs do not replace physical lineage.
Every actual physical reference needed by a scene belongs in a selected table,
including reused tiles. A readonly unconfirmed tail cannot accept a new seal.

No source Store is deleted on Drop. A never-published fragment may use its
explicit unique-owner abort after all provisional Surface/Snapshot owners drop.
The runtime does not retain a duplicate file handle which could keep an aborted
file allocated after its verified quota release. Published/history/clipboard/
task/recovery owners require a complete retirement inventory.

`SourceSession::abort_fragment` combines the fragment's proven file retirement
with its exact registered empty-directory identity. Only successful empty owned
directory retirement releases the directory/location slot. Busy, foreign or
uncertain cleanup keeps conservative inventory; it never recursively deletes.
The session permit remains held through cleanup. Its caller's cleanup check may
ignore an obsolete gesture revision, while actual profile/session checks remain.

Live inventory entries retain filesystem file and directory identities even when
their weak Store owner expires. Resolving a selected checkpoint checks those IDs
before and after reopening, including a final named-file observation. A copied
same-byte/header replacement does not inherit the old live owner's authority.
Ownership uncertainty closes further source admission until a fresh inventory.

## Exclusive marker publication

`file_safety::owned_rename` verifies the expected complete SHA256 through a held
staging file and a charged bounded scratch buffer, syncing that file before the
proof is returned. Publication repeats the caller's source/revision checks and
same-directory identities, refuses an existing destination atomically, and then
checks the exact resulting named/held identity. It never deletes uncertain names
or refunds their disk reservation.

Windows uses `SetFileInformationByHandle(FileRenameInfo)` with
`ReplaceIfExists=false` and a DELETE-capable held staging handle. The absolute
extended target path is resolved while the verified destination directory and
all its non-alias ancestors remain pinned by handles denying deletion. Native
relative names with a non-null RootDirectory returned an invalid-parameter error
and are not used. A foreign replacement at the old path is never the rename
source. The variable aligned native packet uses the same resident budget retained
by its SHA proof. POSIX uses an exclusive
same-directory rename relative to the retained directory descriptor; its source
component is still path based. Caller-held private-directory/profile writer
admission and before/after identity checks remain necessary. This does not claim
an atomic POSIX source-file-descriptor rename against a hostile competing writer.

The helper does not change legacy SaveTemporary/backup limits or establish
Photoshop, editable-source or recovery authority. Windows directory metadata
power-loss durability remains unqualified because the current `sync_parent`
implementation performs no directory flush there. Small interruption/ownership
tests cannot substitute for actual power-loss or installed-package acceptance.

The proposed recovery marker is separately recognized as version 3/type
`native-backing`, preserving version 2 PSD discovery and cleanup. Its recovery
domain binds canonical Document36 metadata, complete ordered raster coordinate
bindings, every owning Store identity and selected 96-byte CommitRef, plus full
metadata/payload hashes. This source-only recovery proof is not a standard PSD36
layer/composite proof. Live dispatch remains gated until capture, admitted typed
restore and the actual engine adopter retain every source and metadata owner.

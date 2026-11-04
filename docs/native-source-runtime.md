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
Source block/cache buffer capacities separately retain conservative charges in
that same pool, including transient replacement buffers and recovered indexes.
A derived mirror of existing resident rasters does not establish bounded total
application memory or authorize resident eviction/larger-canvas activation.

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
An already-admitted session may finish proven provisional retirement after the
closing gate stops new work. Its retirement-only check still validates the actual
instance/profile/root and owned inventory, and retains both request and workspace
file-operation permits until cleanup ends. Connection destruction therefore waits
before releasing its instance lock. This exception cannot admit new allocation,
tile writes, checkpoints or publication; those retain the normal closing check.

`SourceSession::byte_temporary` is the admitted factory for codec operation
spools. It chooses a fresh internal bytes namespace, reserves the directory slot,
and registers the primitive's exact held file/directory identities before
exposing its append-only writer. The temporary and completed-byte wrappers
borrow the active session through the last read and publication check. They
accept no input path. Explicit abort or retirement first proves the primitive's
sole owned file, then retires the exact empty directory and unregisters its slot.
Drop, an escaped byte-owner clone, a failed callback or foreign replacement keeps
conservative inventory and charge. Startup observes recognized orphan spools as
disk debt; it does not infer source or publication authority from their presence.
The codec still combines session checks with its captured source/revision/lease
checks. The wrapper preserves a specific callback failure across the storage API.

Live inventory entries retain filesystem file and directory identities even when
their weak Store owner expires. Resolving a selected checkpoint checks those IDs
before and after reopening, including a final named-file observation. A copied
same-byte/header replacement does not inherit the old live owner's authority.
Ownership uncertainty closes further source admission until a fresh inventory.

External save staging uses a distinct coexistence admission ticket. Persistent
source reservations and growth also count retained external debt; observing only
`DiskBudget::used()` is insufficient for operation admission. `admitted()` counts
both domains. A ticket can be reserved only after the caller durably records its
orphan intent. Drop or a failed receipt keeps the debt. Authenticated startup
journal recovery can restore the exact intent/byte count idempotently; conflicting
counts refuse admission.

The runtime constructs its disk ledger with the same resident pool. Intent index
capacity is admitted before fallible allocation; settled intents retain charged
tombstones until ledger teardown, bounded to 65,536 distinct intents. Reusing a
settled intent refuses admission so an old checked receipt cannot settle a later
operation. A legacy ledger without an admitted resident pool refuses coexistence
tickets. Cross-process replay protection still depends on the authenticated
durable journal; in-memory tombstones cannot replace it.

Settlement requires the destination owner's checked held-identity receipt,
validated outside the meter lock. It removes operation admission after verified
temporary retirement or transfer to the external saved-file domain. It does not
report physical disk space freed and never relaxes persistent source reservations'
unlink-only retirement rule. The publication caller still owns durable intent,
destination/backup inventory, uncertain-result reconciliation and crash recovery.

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
The native filename packet includes an explicit zero WCHAR while its counted
name length excludes that terminator. This avoids relying on padding after the
last character for absolute-name conversion. Native alignment and nested-runtime
marker cases exercise this separately from the existing long-path checks.

Native backing identity/read observations use nonblocking, no-follow opens and
verify the opened regular file. A FIFO or device replacing a checked name cannot
stall source quiescence. Held profile admission also reads its bounded manifest
through this path and repeats file/directory identity and stamp checks. Legacy
preference readers and publication policy retain their existing behavior.

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
The approved distinct binary framing uses `recovery-v3-UUID.pbr`. Its runtime I/O
factory publishes only through an exclusive held pending file, exact SHA readback
and no-clobber rename. Opening requires the actual authenticated inventory and
retains the source session; neither an arbitrary path nor marker presence grants
editable authority. Unknown framing remains held without selection or cleanup.

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


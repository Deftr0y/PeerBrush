# Native backing storage groundwork

This is an internal source-storage primitive. It does not enable 16000-square
documents, change the existing canvas/file limits, change PSD/PSB source formats,
or qualify the editor's large-document memory use. The engine, history, operations,
codec and recovery-profile integration still require coordinated implementation.

`large_document::Store` appends immutable native RGBA blocks to one owned pack
file. A `TileSpec` describes 1..256 pixels per edge at native depth 8 or 16. Native16
payloads use big-endian channel pairs; host-endian casts are not part of this API.
The SHA-256 identity binds the record kind, specification and complete payload.
Repeated identical blocks reuse the same reference. No backing error becomes a
transparent pixel or a disposable-cache miss.

## Pin and allocation contract

Call `Store::put(spec, bytes, check)` while retaining the dirty input bytes. Only a
successful return establishes a durable block reference. `Store::pin` returns a
fallible, immutable `PinnedTile`; the caller must pin an admitted region before
using existing infallible pixel sampling. Pixel getters must not start disk I/O.

`ResidentBudget` is shared across stores and counts the exact boxed byte layouts
of live pins. Admission happens before allocation or reading. A cloned pin shares
one allocation and charge; concurrent duplicate loads reserve both buffers until
they resolve to one canonical shared allocation. Failed, cancelled and refused
loads release their reservations. A weak-cache entry cannot retain a live,
uncharged byte buffer. The buffer is dropped before its charge is released.

`ScratchBytes::new(&budget, length, maximum)` admits an explicitly bounded mutable
byte buffer under the same ledger (at most 64 MiB per scratch). Freezing it into
`PinnedBytes` transfers the allocation and charge without copying the payload.
Candidate edits can keep native16 data in canonical byte order throughout this
stage instead of allocating an uncharged host-word conversion.

`Store::resident_budget()` returns its exact shared pool. `reserve_bytes` returns
an immutable `Reservation` without allocating a placeholder buffer; external COW
paths must retain that handle until the actual buffer is released or transferred
to another separately enforced admission policy. A reservation is not permission
to release accounting while an otherwise unaccounted allocation remains live.
The crate-private frozen-owner finalization hook may reduce only unused peak
reservation after all replaced growth buffers have dropped; the remaining charge
must cover actual surviving capacities. The payload and charge remain inseparable
through clones and history. Pins, byte scratch and disk reservations cannot use
this external-owner hook or resize their accounted payloads.

This budget excludes allocator overhead, indexing structures, document/history
metadata and buffers not admitted through these pin/scratch APIs. The explicit
record, resident-slot and manifest limits bound store structure counts; they are
not a total process RAM bound. A full application admission policy must account
for those other owners and scratch before enabling larger documents.

Readers and writers use nonblocking operation admission. Cache/state mutexes are
not held across I/O or caller checks. Reentrant or competing writes return `Busy`
instead of waiting behind an operation callback. The worker scheduler must retry
busy work explicitly without publishing a partial document or blocking UI locks.
Offset-based I/O is used throughout; the Windows implementation never relies on
the mutable file cursor exposed by [`seek_read`/`seek_write`](https://doc.rust-lang.org/std/os/windows/fs/trait.FileExt.html).

## Checkpoints and recovery

`commit(source, revision, parent, metadata, blocks, check)` binds an observed source
identity, increasing revision, exact prior checkpoint, opaque metadata and ordered
block references. Every referenced block must belong to the store. The metadata
and reference stream have independent hash bindings in the loaded snapshot.
The store does not interpret metadata as an engine document or grant profile
publication authority.

Records are appended, sealed, synced and read back before a reference is returned.
The constructor creates only a fresh directory and exclusive source file. Existing
paths are refused. The caller must supply an already-owned, access-restricted,
canonical session/profile parent and keep its admission lock; these checks do not
create profile authority or configure Windows ACLs. Source file and directory
identities, aliases, hard links and expected bytes/stamps are rechecked. All writes
target the retained file handle. Foreign replacements are neither overwritten nor
removed. Closing the store does not delete source data.

The caller must atomically retain a successful `CommitRef` through the separate
validated profile/recovery writer. `recover` accepts that independently retained
reference, validates its complete prefix and exact chain, and never guesses the
latest checkpoint from a directory. A suffix after the selected checkpoint is
preserved and makes the recovered store read-only. A partial, failed or cancelled
append poisons further writes in that instance. Even a complete seal whose final
check failed is not silently promoted, truncated or replayed.

`Snapshot::visit_blocks` streams a bounded reference batch. Visitor output remains
provisional until the complete call succeeds, including its final integrity and
ownership checks. A loader must build a pending document and commit it only after
all native blocks, metadata and source/revision guards succeed.

`Store::snapshot` selects a retained checkpoint through an existing held owner,
so another tab can recover without acquiring a second exclusive file lock. It
checks the complete selected prefix and record boundaries without advancing the
owner's publication head. Unknown tails remain held; embedded record-like pixel
bytes cannot become checkpoint authority. `Store::identity` is only a lineage
identifier for source keys, not filesystem authority or permission to skip pins.

Source-store compaction/retirement, profile upgrades, global retained-owner
accounting, actual power-loss acceptance and the integration with self-contained
standard PSD/PSB saves are not provided by this primitive. The opt-in multi-layer
dense tile test verifies backing I/O and bounded pins only; it cannot qualify
large-document editing, styled rendering, history or receiving-app fidelity.

## Persistent disk admission and planar staging

`DiskBudget` counts shared logical-file reservations. A reservation remains
charged after its handle is dropped or a write fails. A partial file may retain
its conservatively reserved maximum until verified retirement or a fresh startup
inventory. This is not a measurement of filesystem allocation or free space.
File-count admission separately bounds the ledger's bookkeeping structures.

`from_inventory` receives explicit named files and their held handles, counts
physical identities once, and repeats every observation after its final caller
check. The caller must retain actual instance/profile writer admission and include
all source, history, recovery, backup and incomplete staging files. Inventory
observations grant no source authority. Scanning a cache or starting an empty
ledger over an existing namespace cannot establish safe admission.

`Store::create_with_disk_budget` and `recover_with_disk_budget` join that shared
pool. Every append reserves capacity before writing; deduplication and reopening
the same owned file do not double-charge it. The original constructors remain
for existing bounded callers and cannot qualify shared large-document admission.
Dropping a source Store still never deletes files or refunds disk capacity.

`PlanarScratch` is an exclusive append-only staging file. Its four channel planes
are ordered R, G, B, A, with no row padding and canonical big-endian native16
samples. Appends must match the exact next channel and row and contain at most
256 complete rows. `ImportWhiteMatte` and `SaveCurrentMatte` describe the stored
standard-composite convention; this module performs no alpha or color conversion.

Finishing binds geometry, role, chunk order and the caller's source/snapshot SHA
to a synced, fully reread seal. The 64 KiB chunk SHA index remains charged in the
shared resident pool. Reads verify complete overlapping chunks into a caller's
same-pool `ScratchBytes`; output is provisional until all checks succeed. Missing,
corrupt, truncated or replaced data returns an error, never transparent pixels.
Neither a storage seal nor a scratch file substitutes for codec completion,
source identity, lease, document revision or final publication checks.

A native16 16000-square staging payload is 2,048,000,000 bytes. Its header, SHA
index and seal bring the reserved encoded file size to 2,049,000,208 bytes. These
bytes must coexist with source backing, standard output spool, final candidate,
backups and any retained history. The legacy 256 MiB SaveTemporary limit is
unchanged. A planar component test is not full editor or PSB qualification.

`CompletedPlanar::retire` requires the last shared reader, repeats ownership
checks, removes only its owned file, verifies absence and then releases its disk
reservation. Windows retirement targets the retained DELETE-capable handle using
[`SetFileInformationByHandle`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfileinformationbyhandle),
preserving a foreign replacement at the old name. Unix retirement requires the
retained private directory and serialized profile writer. Failure or ordinary
Drop preserves conservative accounting; directories are not recursively removed.

Application startup/session admission, authoritative surface migration, durable
recovery markers, source-store retirement and compatible profile backup still
need their actual callers and native qualification before larger documents open.

## Exact derived scalar fields

`ScalarScratch` and `CompletedScalar` represent one finite f64 component in
canonical big-endian row order. They are distinct from the strict four-channel
native8/16 planar types. A scalar file has its own header and hash domains and
cannot become a Source36 raster or recovery authority. Geometry is local to the
captured producer frame; placement remains part of the external stage binding.

The producer appends complete sequential rows, then finishes with its stage SHA.
`value_sha256` is the pure SHA256 of the emitted BE f64 values, allowing comparison
with an independent full-emission token. The separate seal binds geometry, type,
role, stage, ordered chunk hashes and the index. Nonfinite or partial samples are
rejected. Exact bits, including negative zero and finite subnormal values, remain
unchanged.

Full-row reads retain the explicit 256-row and 64 MiB allocation limits. Rectangle
reads admit at most 128 KiPixel and 256 rows, verify every intersecting 64 KiB
chunk, and use the same shared resident pool. Field files remain fully charged in
the operation's shared disk quota until verified retirement; the disposable
512 MiB image-cache limit cannot account for a 2.048 GB field. The opt-in scalar
test qualifies storage and readback, not the effects producer, editor or total RAM.

Never-published planar/scalar writers have an explicit `abort` operation. It
rehashes the complete known successful prefix, verifies the exact header and held
identities, and repeats stamps before owned retirement. A cancellation between
successful chunks can therefore release its reservation without adopting a seal.
Unknown partial writes, extra bytes, changed hashes or foreign replacements keep
their bytes and charge. Drop still performs no cleanup. The caller retains its
actual session I/O permit through abort; cleanup checks may ignore the obsolete
gesture revision, but cannot invent filesystem or profile authority.

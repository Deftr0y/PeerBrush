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

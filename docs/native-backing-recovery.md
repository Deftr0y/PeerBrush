# Native backing recovery marker

The internal version 3 recovery marker is separate from version 2 PSD recovery
and from a Photoshop Source36 standard-layer/composite proof. Its capture and
provisional loader are implemented, but live emission remains closed until the
native engine adopter and recovery lifecycle retain all required ownership.
Resident documents keep the existing version 2 worker. This does not raise editor
canvas limits, migrate old recovery, or select an unknown marker automatically.

## Framing

An immutable marker is named `recovery-v3-UUID.pbr`. All integers below use big
endian byte order. The fixed header is 112 bytes:

| Bytes | Field |
| --- | --- |
| 16 | `PBRECOVERY3` followed by five zero bytes |
| 4 + 4 | Version 3 and native-backing type 1 |
| 16 + 16 + 16 | Snapshot, project and document UUID bytes |
| 8 + 8 | Captured document revision and timestamp in milliseconds |
| 8 | Canonical Document36 JSON byte length |
| 4 + 4 + 8 | Store count, raster count and total coordinate binding count |

The header is followed by the exact canonical Document36 JSON bytes, then the
Store table. Each Store entry is its 16-byte identity and selected 96-byte
CommitRef. Store entries are ordered by identity without duplicates. CommitRef
is selection transport only: the loader never reads a deserialized file offset
directly or infers a newer head.

Each raster begins with a 20-byte descriptor: ID, width and height as u32, depth
and zero flags as u16, then coordinate binding count as u32. Raster IDs must be
exactly 1 through N and match the complete typed metadata roster. Each binding is
four u32 values: tile x, tile y, Store table index and selected reference-table
ordinal. Coordinates use strict lexicographic `(x, y)` order with no duplicates.
Full native tiles are 256 by 256 RGBA, with canonical big endian 16-bit pairs.
Absence is the explicitly declared zero source; a present reference that fails
validation never becomes transparent pixels.

The final 32 bytes are SHA256 of the domain
`PeerBrushNativeBackingRecovery3\0` followed by every preceding marker byte.
Every count, length, reserved field and final EOF must agree. The digest detects
corruption; it is not authentication or a signature.

Metadata is bounded to 64 MiB, the complete marker to 128 MiB, Stores to 4,096,
rasters to 65,536 and coordinate bindings to 1,048,576. Per-operation limits may
be smaller. These storage bounds do not promise that an arbitrary document fits
the actual shared resident/disk admission or qualifies for editing.

## Ownership and completion

Capture reads the immutable SceneSnapshot and its selected prefix owners after
engine locks are released. Canonical metadata and every temporary index receive
fallible capacity admission before allocation. Every physical raster reference
must belong to its authenticated selected table; the marker includes no runtime
paths or fabricated BlockRef offsets. This includes retained originals, masks,
selection sources and other locations in the typed scene roster.

Publication uses the actual SourceSession, an exclusive held pending file, full
sync and fresh SHA readback, then a no-clobber owned rename. Source, revision,
profile and immutable-owner checks repeat after durable I/O before publication.
A failed publication capture is latched. Unknown outcomes and partial pending
files retain their byte admission and are not selected or deleted on Drop.

Loading opens only a marker in the actual profile/instance inventory. It validates
the complete framing before typed metadata or source adoption. Each Store is
resolved through that held inventory, checking its filesystem owner, original
CMIT lineage, selected prefix and every reference-table membership. Referenced
native blocks are pinned and SHA checked explicitly. Any missing, corrupt,
foreign, stale or canceled source refuses the whole provisional result.

Source sealing can reuse a completely covering, explicitly known checkpoint.
The held Store re-reads that known CMIT and validates its hash and every reference
against the index established by its original write or full recovery. This helper
accepts no supplied offset or selection. Arbitrary transported CommitRefs still
require complete prefix validation. A readonly unconfirmed tail cannot append;
missing or foreign required references refuse. A writable source appends only
when the known table does not cover the new references, preserving its original
physical lineage, parent and monotonic sequence. Its older opaque CMIT metadata
does not stand in for the marker's independently bound current Document36 data.

The loader returns an admitted PendingScene together with every selected prefix
and its held marker/session guard. This result does not itself authorize a
registry entry. Actual adoption must retain source and metadata owners and repeat
the current project, lease, revision, cancellation and finalization checks before
publication. Runtime document identity is separate from physical Store lineage.

Live history, tabs, clipboard, tasks and recovery markers keep their own strong
source owners. No Store or published marker is retired by Drop, and no orphan
scan grants deletion authority. Complete owner/marker inventory is required for
future compaction or retirement. This marker captures the current scene; it does
not claim to serialize a complete engine undo stack.

Native marker writes keep the normal closing check. The retirement-only cleanup
check cannot authorize a checkpoint after closing starts. The final engine freeze
and pre-close publication permit remain a live integration requirement. Windows
directory-metadata power-loss durability also remains unqualified.

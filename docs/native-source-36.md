# Native source format 36 contract (unreleased)

Format 36 is a new bounded native-source design. Format 35 is unused. Existing
formats 1..34 retain their current read contract. This document does not activate
format 36 saves, raise canvas/file limits or qualify large-document editing.

The existing PBR1 resource marker remains. Its new manifest contains metadata
and portable raster references instead of base64 native tile payloads. Standard
layer/mask channels and a current native merged composite remain mandatory.
Readers without format 36 support must protect or refuse the sources. They must
never interpret missing inline tiles as an editable empty document.

## Semantic metadata and raster roster

Use explicit typed mirrors of Document, Layer, MaskStep, selection Coverage and
retained-original definitions. Every native raster has a deterministic traversal
ID 1..N and a descriptor containing width, height and depth. Retained original
pixels, layer pixels, every native mask step, selection_coverage and
selection_previous are included. Every effect location also maps historical
Liquify `settings.strokes[*].coverage` masks to typed coverage/raster references;
these cannot remain opaque inline pixel JSON. Retained warp stages are geometry,
not historical Liquify coverage. IDs are independent of runtime store UUIDs,
pack offsets and SurfaceKey values. No runtime path belongs in the saved source.

Current Bezier/Text/Shape/Svg source definitions retain their exact values.
The format36 ICC field uses canonical STANDARD base64, capped at 4 MiB decoded
bytes (5,592,408 encoded characters), preserving exact original profile bytes.
Numeric byte arrays are not an alias in this new field. Legacy formats retain
their existing wire behavior; the new typed roundtrip must enforce this choice.
Validated embedded effect image strings retain their existing separate limits.
Runtime-only source_structure and source_patterns inspection views remain
protected views; they do not become editable authority through this format.
Unknown source kinds, missing raster locations or unsupported retained structures
must fail admission rather than disappear. Surface objects do not implement
implicit I/O or default source serialization. Typed metadata is staged from an
immutable snapshot; serialization itself must not perform file I/O.

Metadata JSON must have unique object keys, finite numbers, no trailing JSON and
canonical ordered object keys/serialization. The complete decompressed PBR1
manifest is capped at 64 MiB, retaining room for the current 32 MiB editable
pattern-asset envelope and ICC metadata. Parsed trees, canonical core/manifest
buffers, typed metadata, tile transfer and index scratch need combined RAM
admission in one shared pool. Raster count, reference count, unique tile count,
chunk count and metadata/index allocation require explicit operation admission.
These are additional internal bounds, not a promise of arbitrary document size.

Canonical metadata has separate operation-selected node and depth bounds, with
format implementation ceilings of 8,388,608 nodes and 128 levels. A no-DOM pass
counts empty arrays, values, object fields and decoded strings before allocating
the parsed tree. Its source fingerprint prevents reusing admission for different
bytes. The accounting envelope covers tree/container growth, string/parser and
sorting scratch plus simultaneous raw/core/manifest/typed data and tile/index
scratch. It is a conservative application accounting bound, not total process or
allocator RAM evidence.

The raw input must already have a retained charge before decompression. Shared
reservations are acquired with rollback before parsing. Immutable `Admitted<T>`
owners share both one payload allocation and a separate charge-only Arc. Moved
large fields retain that owner; new deep copies require separate admission.
Temporary accounting can be reduced only after the corresponding buffers drop
and all surviving capacities remain covered. Transitioning legacy document
clones retains its separate bounds and does not establish a total RAM claim.

The typed transition consumes the admitted canonical tree and moves the document
metadata into the exhaustive DTO. Its exact canonical roundtrip rejects ignored,
inserted or narrowed fields. The engine must also validate document ID/revision,
every raster reference and semantic feature admission. Moving a DTO does not
itself establish that a standard Photoshop section faithfully represents it.

## Manifest and hash bindings

The manifest contains format 36, source document ID/revision, typed document
metadata, the ordered raster roster, tile/reference counts and roots, core SHA,
the standard-data binding, and ordered chunk descriptors (ordinal, payload byte
count, payload SHA-256). Integer lengths/counts are checked before allocations.

The core is canonical JSON containing format, source ID/revision, typed metadata,
raster roster, counts and tile/reference roots. It excludes the standard binding
and chunk hash list to avoid a cycle. Its digest is:

`SHA256("PeerBrushSource36Core\0" || core_bytes:u64BE || canonical_core)`.

The standard binding is:

`SHA256("PeerBrushSource36Standard\0" || layer_bytes:u64BE || composite_bytes:u64BE || exact_layer_payload || exact_composite_payload)`.

The layer payload is the original layer-and-mask section contents, including
private chunk tags, tag headers and padding, excluding its outer length field.
The composite payload starts with its compression word and ends at file EOF.
The declared lengths must equal the indexed spans. Legacy u64 FNV checksums have
no format 36 authority. Header geometry, profiles, standard metadata and current
composite checks remain required independently. Source lease/identity and whole
source SHA rechecks still guard publication; these hashes are not signatures.

The ordered tile root hashes a distinct `PeerBrushSource36Tiles\0` domain,
u64BE tile count, and each canonical spec8/digest32/payload_length:u32BE tuple.
The reference root similarly hashes `PeerBrushSource36Refs\0`, u64BE count and
each raster_id:u32BE/tx:u32BE/ty:u32BE/digest32 tuple in reference order.

## Private chunks

The additional-information key is `Pb36`, signature `8BIM`. It uses an ordinary
32-bit payload length even in PSB. Each payload is at most 64 MiB. The layer/mask
container retains its normal PSD/PSB length width. Adobe's documented PSD size
limit is [2 GB](https://helpx.adobe.com/photoshop/desktop/save-and-export/export-files-to-different-formats/photoshop-file-formats-overview.html);
larger admitted outputs require PSB even when individual fields fit u32. Refuse
an oversized PSD request before touching the destination and explain the PSB
choice; do not silently change the requested extension.

Each payload begins with this 56-byte header:

| Field | Encoding |
| --- | --- |
| Magic | Eight bytes `PBRBULK1` |
| Version | u32BE, exactly 36 |
| Ordinal | u32BE, contiguous from zero |
| Total chunks | u32BE, equal to manifest count |
| Record count | u32BE |
| Core binding | 32-byte core SHA-256 |

The header is followed by exactly the declared records with no record padding:

| Record | Encoding |
| --- | --- |
| TILE | kind:u32BE=1, spec8, digest32, payload_length:u32BE, native payload |
| REFS | kind:u32BE=2, raster_id:u32BE, tx:u32BE, ty:u32BE, digest32 |

Spec8 is width:u16BE, height:u16BE, depth:u16BE, channels:u16BE. Application
source tiles always use full 256x256 RGBA storage, including edge padding, at
depth 8 or 16. Native16 samples are big-endian pairs. The tile digest is exactly
the backing store's `SHA256("TILE" || spec8 || payload)`. No runtime pack offsets
or store identity are serialized.

TILE records precede REFS records globally and sort strictly by (spec8, digest).
REFS records sort strictly by (raster_id, ty, tx). The chunk payload SHA covers
the exact header and records. Duplicate physical tiles, duplicate references,
missing/orphan tiles or rasters, conflicting specs, unknown record kinds, wrong
order, excess bytes and count/hash/binding mismatches fail validation. Repeated
`Pb36` keys become recognized only after the complete exact manifest/chunk set
validates; ordinary duplicate/unknown metadata protection remains unchanged.

Decoded blocks and metadata remain provisional until every required chunk and
native stream completes and all source/publication checks pass. A chunk or
scratch completion token alone cannot grant editable or recovery authority.

## Owned planar scratch and disk admission

LargeSourceTemporary is separate from the unchanged legacy SaveTemporary limit.
It requires a shared persistent disk reservation and an already-owned profile or
session directory. Create fresh files exclusively, retain their handles and
identities, check directory/file aliases, links and stamps, and refuse foreign
replacements. No random-write interface is provided.

Planar scratch contains exactly R, G, B, A channel-major rows, without padding,
at native depth. Imports retain standard white-matte RGB/alpha; bounded stripe
consumers explicitly restore straight alpha. A writer accepts only the next
channel/row, with 1..256 complete rows per append. Seal checks exact geometry and
byte count, syncs and reads back SHA, then exposes an opaque completed handle
with bounded stripe reads. Decoder completion and source identity proofs remain
the codec caller's responsibility. All transfer buffers require RAM admission.

The shared disk budget must count persistent backing/history, backups, orphaned
owned files, standard/manifest spools, planar scratch and final candidates while
they coexist. Dropping a failed writer does not release a charge for bytes left
on disk. Startup recovery requires independently retained inventory authority;
an arbitrary directory scan does not grant ownership. Capacity or I/O failure
preserves current sources, existing destinations and foreign replacements.

Container replay is append-only and has an explicit 64 GiB operation ceiling,
separate from unchanged live document limits. Its header and all section/resource
lengths are checked before writing. It requires whole bulk and standard-data
completion proofs, rechecks the compressed manifest and exact standard SHA while
copying bounded read-only ranges, and returns an opaque whole-file SHA/length
observation. That observation binds the complete manifest and header, not merely
the core hash. A sealed owned file, current render/metadata fidelity, source lease
and final publication checks are still required. A failed replay never grants
completion; its partially written owned candidate must be retired explicitly.

Source verification consumes the exact indexed PBR1 compressed range, retaining
its completed encoded SHA observation through typed conversion. It checks that
resource again, the full global Pb36 set, the strong original standard binding,
every native layer/mask channel and the single merged planar stream. Every row
reader must finish its checksum/end validation. Whole-source SHA and length
observations bracket this work. The resulting opaque token is tied to the exact
borrowed source/index and complete manifest proofs; row counts cannot construct
it. A separate whole-source recheck precedes handoff, alongside the caller's file
identity/lease and engine revision checks. Stream validity still does not prove
that a styled layer or merged composite equals the current typed scene render.
A failed source completion stays failed even if the bytes later reappear. The
successful handoff consumes it and moves the typed metadata with its retained
charge owner into PendingScene; it does not return a bare uncharged payload.

## Captured standard save plan

The new-format standard plan borrows the immutable metadata projection of an
admitted pending or native document snapshot and its native physical references.
The projection serializes the exact Document36 wire without cloning its source,
style or selection metadata graph. Physical sources resolve through slots bound
to that captured generation. This does not demote a native document to pending
import data. It preserves full padded tile bytes, native low bits and
hidden RGB. Tile/reference roots and chunk partitions are planned from bounded
immutable metadata; emitting a tile still pins and verifies its actual bytes.
References sort by raster ID, tile Y, tile X, independently of runtime map order.

Standard native callbacks identify the exact captured scene, source layer or
composite, channel, document-space rectangle, first row, row count and depth.
They prepare at most 256 rows into an admitted bounded planar buffer. Sampling
performs no file I/O. Mask serialization requests actual mask values even when
the overall mask is disabled; the disabled flag remains standard metadata.
The current merged composite uses native white-matte RGB plus native alpha.

Ordinary supported sources keep their standard layer ordering. When filters,
adjustments, unsupported clipping or a styled group require a visible baked
appearance, a hidden wrapper also retains every original source layer/mask and
its original group/visibility metadata. The original-source callback writes raw
native source samples rather than effect-rendered replacements. This is an
honest Photoshop fallback, not unrestricted native Photoshop compatibility.
The editable document limit remains 200 layers. New-format standard derived
records admit at most 403 (200 layers, up to 200 group ends, wrapper and bake);
legacy standard/import bounds are unchanged. The strict new-format reader must
select this explicit derived bound only after source-format admission.

Reopened standard metadata and every native layer/mask/composite sample must
equal a newly prepared plan from that same captured scene. Matching stream or
whole-file hashes cannot replace this fidelity comparison. CompletedSave36 is
closed to generic callbacks or digest-only constructors; publication requires
the actual scene driver to combine complete source, native and fidelity proofs
while retaining the exact snapshot and owned PhotoshopContainer bytes.

## Provisional import handoff

The importer receives an already retained source handle, a pre-admitted exact
structural index, and the caller's authenticated fresh backing session. It
streams verified TILE bytes into that session, then builds the complete native
surface roster without exposing editable document state. Manifest, index and
physical-reference allocation owners remain with the provisional candidate.
No saved path or runtime store identity grants ownership of an import session.

The caller validates all supported scene semantics and prepares actual native
rows from this exact provisional snapshot. Every standard layer, raw mask and
current composite sample must match before qualification. The comparison is
bound to this candidate, rather than reusable for another import with similar
metadata. Whole source bytes, held identity and active session must be checked
again during the final guarded adoption; a session revoked during the source
hash pass prevents the adoption callback. The caller retains responsibility for
file identity, lease, profile lock and final engine revision checks throughout
that callback. Failure must preserve unsupported/read-only protection and must
not fall through to an editable ordinary-Photoshop interpretation of broken
recognized private sources.

These closed save/import components do not activate a larger live canvas or
claim complete editor, history, recovery or installed-profile qualification.

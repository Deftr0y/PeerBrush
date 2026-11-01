# Explicit native paint tiles (unreleased)

`brush::native::PreparedBrush` prepares the existing document-frame dabs before
processing individual full-stride 256x256 tiles. It shares the resident brush's
pressure, texture, flow, opacity, polygon and erasure arithmetic. Native16 input,
coverage and output use canonical big-endian pairs; no RGB8 projection replaces
native source samples. Logical frame clipping leaves physical edge padding intact.

The plan retains a conservative 2 MiB reservation covering its bounded dab/tile
vectors, including their construction working set. Each tile has a charged
coverage buffer and a separately charged immutable output. Cloning an output
shares its charge until the final owner drops it. The kernel rejects a foreign
resident pool, a wrong tile length or an undeclared footprint. Cancellation and
allocation refusal return no output and leave the captured source untouched.
These charges exclude unrelated resident sources and allocator overhead; they
are not a total application RAM bound.

Preparation and sampling perform no file I/O. The source worker must prepare
and validate the complete original tile outside engine/UI locks, including its
padding, and retain the document/source/revision/reservation guards. Publishing
requires the engine's independent durable-source and atomic-history transaction;
raw output bytes grant no editable, file or recovery authority.

`changed_tiles` compares exact dab prefixes and includes old dabs removed from a
tail. A preview restores such a tile by removing its override over the immutable
baseline. It must not blend a new result onto the previous preview or create
history for each dab. The final immutable output can be shared with a preview
while its owner stages the corresponding durable tile.

This component does not switch live documents to backed surfaces or raise canvas
limits. Full gesture/engine integration, native desktop acceptance, performance,
soft-selection reconciliation and durable publication remain separate checks.
Smudge must preserve its ordered pigment reservoir; clone/heal require explicit
translated sampling and healing halos rather than independent reordered dabs.

`fill::native::PreparedFill` provides the corresponding foreground-fill kernel.
It clips in the physical source frame while testing polygons in document space,
preserving layer offsets, native blending and untouched edge padding. Its tile
iterator allocates no full-frame buffer. Source preparation and provisional
result delivery are explicit callbacks; a source, sink or cancellation failure
cannot complete the visit. Previously delivered results remain provisional and
cannot authorize partial engine publication. Use the validated current polygon
from `selection::polygon`, preserving the legacy stale-polygon rule.

`PreparedClear` consumes already-prepared selection confidence in document
coordinates. It preserves the existing f32-to-f64 alpha calculation, clears
hidden RGB only when a selected pixel's resulting alpha is zero, and leaves
physical padding untouched. Invalid confidence, an undeclared coverage sample,
cancellation or a foreign pool returns no partial tile. The caller must bind
the prepared coverage and original color/mask slot to the captured scene before
the common engine transaction can adopt the result.

`retouch::native::PreparedRetouch` preserves the resident clone/heal coverage,
bilinear sampling, premultiplied means and native-depth blend arithmetic. Its
explicit read plan declares source sampling queries and the destination healing
halo. Prepared samplers are memory-only and fallible; a missing sample cannot
be replaced with transparent pixels. Known exterior zeros must be established
by the prepared source's geometry. A radius-64 halo spans at most 384x384 pixels,
so its source preparation must be split into bounded strips. The two integral
fields use charged byte storage, including their padded rows and columns.
The output preserves complete physical tile padding and stays provisional until
the common captured-scene transaction adopts it. Merged artwork/mask sampling
must use the shared prepared compositor; this kernel adds no full-frame fallback.

`smudge::native::PreparedSmudge` retains the ordered pigment reservoir. A worker
prepares explicitly stamped native strips of at most 128 KiPixels and completes
each dab's average before staging any of that dab's tiles. Native8 uses the
original f32 reservoir and native16 the original f64 reservoir, including the
legacy f32 write-strength product. A transparent loading color can still move
existing pigment. The working owner must make all completed prior dabs visible
to the next preparation. Strip/tile preparation may perform worker I/O; sampling
already-prepared bytes never does. Input and output buffers share the captured
resident pool, and tile results preserve physical padding.

The run owns its provisional candidate. A preparation, cancellation or staging
failure drops that owner and returns no completed candidate. Successful output
still grants no document authority: the common scene/revision/lease transaction
must adopt it with one history entry. The small resident differential fixture is
an operator oracle; the max-radius strip test is a footprint check, not dense16K
document admission or native desktop/performance qualification.

`edit_bounds::native::FrameExpansion` computes the existing canvas/source union
with checked geometry and supplies an exact old-source read rectangle for each
new tile. Its memory-only remap preserves native samples and hidden RGB inside
the original logical frame; original physical padding never becomes image
content. New space and new padding are explicit zero. Missing source pixels and
cancellation fail the tile. Color, mask steps, retained placement, vector source
placement and effect coordinates must all be staged together before the common
engine transaction changes a layer's frame. This component does not replace
those metadata guards or widen live source admission.

`edit_bounds::input::RasterInput` borrows an admitted `NativeDocument` and binds
layer or mask-step reads to its generation, raster slot, source key and frame.
Full-tile tokens preserve physical padding. Regional and healing-halo preparation
performs explicit worker I/O before returning fallible memory-only samplers.
Healing halos use bounded region strips and a separately charged linear owner.
Selection geometry and contours stay borrowed; matching coverage takes priority
and samples its physical origin/extent using legacy red-channel projection.
The post-edit soft-selection kernel preserves the existing premultiplied native
rounding and must not be applied a second time to selected clear.

`brush::input::LocalStroke` admits its shifted point, pressure and polygon
capacities before allocation. It creates the actual brush, clone/heal and smudge
plans with the captured storage origin; another generation/frame cannot reuse
that geometry accidentally.

`edit_bounds::output::TileOutput` stages one plane through the actual
`SourceSession` and the engine's `OutputDraft`. It compares full original padded
tiles, keeps mutable proposal/index allocations charged, creates a fragment only
for changed nonzero bytes, and poisons failed replacements. Unchanged operations
produce no output draft. Explicit zero and unused-fragment retirement precede
sealing. Refusals retain ownership and admission until proven cleanup; dropping
an owner never silently deletes files or refunds disk. These source adapters do
not replace the coordinator's atomic group, mask, frame, retained-source, actor,
lease, revision, selection or history decisions, and do not raise canvas limits.

Projected frame plans additionally bind a proposed shape/origin to the original
generation and primary layer slot. Every mask plane receives the color frame's
same translation while retaining its own original logical dimensions/depth.
`stage_frame_with` remaps and edits each complete destination tile in one pass;
it does not publish an intermediate expanded document. The brush worker uses
that path for reframed strokes and the changed-tile path otherwise, applying
soft coverage against the correctly reframed baseline. All source reads remain
explicit before memory-only kernels, and a failed tile retires the unpublished
fragment through the held session. Complete frame, mask, retained placement and
source/effect metadata changes still belong to one coordinator batch.

Resolved native fill and selected clear consume the same borrowed plane and
session-owned output path. An opaque fill-baseline proof binds the literal
procedural color to the original generation, slot and logical frame. It does
not expand draw permissions: the coordinator must pair its complete output
with the kind transition and retained-source retirement in one atomic batch.
New frame space and padding remain zero, including when the original fill has
partial alpha or hidden RGB. Selected clear consumes coverage once.

The resident engine now materializes that literal visual baseline before frame
expansion and soft-selection snapshots. Previously those snapshots could see
the fill's unused raw plane, losing unselected visible color or blending at the
wrong document coordinates. The outer transaction retains the original kind
and pixels for one undo/redo record. Prepared native8/16 fixtures reproduce the
former loss and compare the corrected engine with the staged native result;
their runtime qualification remains required. Neither these adapters nor a
metadata-only compile establish large-document or desktop acceptance.

The original-frame smudge adapter retains one session-owned provisional tile
map across all dabs. Explicit strip/tile preparation sees completed prior dabs;
memory-only reservoir sampling never opens storage. A borrowing run entry point
preserves the owner on error for registered fragment retirement. Soft coverage
is applied once against the captured original after the full stroke.

Reframed smudge retains an owned `FrameSeed` before constructing an output draft.
The seed holds its complete provisional surface, registered fragments, index
admission and actual source session across every dab. Working strip/tile reads
see completed prior writes. Replaced append-only blocks remain charged on disk;
unused fragments are retired only after all candidate views drop. Explicit zero
is canonicalized before the single final draft/seal. No intermediate document
or history state is published, and another frame cannot reuse the stroke token.

The clone/heal adapter prepares a generation-matched paint source or raw paint
mask step, including exact floating bilinear support and the complete healing
destination halo. Bilinear support can span 385 samples because it includes the
last adjacent sample; bounded physical strips feed its charged linear owner.
Exterior zeros follow from source geometry, while undeclared queries fail.
Reframed targets remap the destination baseline and halo without changing source
document coordinates. Merged artwork and composed-mask inputs still require the
shared prepared compositor and are not replaced with raw layer samples.

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

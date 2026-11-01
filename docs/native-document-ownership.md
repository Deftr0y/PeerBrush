# Native document ownership

`engine::native::NativeDocument` is an immutable owner of typed Source36 metadata
and an admitted, shared source roster. It does not replace the live engine's
resident document yet. Current canvas admission limits remain active.

Clones share the same metadata allocation and its charge. The roster has a
separate lifetime admission, so sharing it with a new header never depends on a
retired header ticket. Raster slots are opaque and bound to the exact immutable
document generation. Reading a slot returns a Surface; source I/O still requires
explicit fallible preparation outside engine/root/registry locks.

The metadata projection exposes borrowed values instead of Arc payloads. Its
serialization emits metadata only. NativeDocument itself has no serde or Deref
implementation. A consumer cannot sample backed pixels through a legacy getter.

The initial visibility operation prepares a fresh deep metadata copy after
measuring its bounded wire bytes and admitting parser/typed storage. It compares
the copied serialization before changing the requested value. The resulting
header owns its fresh admission and does not inherit old header credits. Its
unchanged source roster remains shared. Final adoption checks the exact base
generation, revision, document identity, read-only state and cancellation without
I/O or callbacks.

SceneSnapshot can retain either a provisional PendingScene or a NativeDocument.
Both return the same borrowed metadata projection. Raster, mask, coverage and
retained-original projections produce opaque generation-bound slots, resolved
against the snapshot's strong roster. Native frames share their header and roster
without a metadata copy; their immutable render key is computed once. Metadata
serialization remains the exact Source36 wire representation. The guarded codec
handoff refuses native authority and any runtime leases, file guards or protected
inspection owners.

The independent-root reorder precursor rebuilds the admitted source roster in
canonical document traversal order. Current pixels, originals, mask steps,
historical Liquify clips and both selection coverage slots are remapped together.
Their physical Surface keys stay unchanged; no pixel payload is rewritten. The
result is revalidated against the strict 1..N reference rule before adoption.
Hierarchical and clipping reorder policy remains part of the pending dispatcher
migration.

Generated raster updates use an owned OutputDraft. Sealing checks shared pools
and the session's registered Store owners, retaining selected checkpoints and
FreshFragment tokens. Empty or unused fragments must be retired before sealing.
Preparation and final publication refusals return those owners to the caller;
cleanup releases candidate/preview/checkpoint owners before requesting registered
fragment retirement outside the engine lock. Closing cleanup remains fail closed
until the concrete service supplies its cleanup-only retained-instance admission.

Prepared edits retain typed scopes. The publication path rechecks current leases,
closed/read-only state, exact generation/revision and cancellation before marking
source fragments. Marking and document adoption perform no source I/O or caller
callbacks. Metadata-only adoption cannot publish a generated raster update.

This is the data preparation boundary. Engine actor/reservation enforcement,
history publication, complete source mutation/renumbering, native operators,
recovery and coordinated live document replacement must be connected and tested
before enabling larger documents. The metadata-copy path is conservative; it is
not a performance or total application RAM qualification.

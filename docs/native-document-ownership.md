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
fragment retirement outside the engine lock. The concrete service supplies its
cleanup-only retained-instance admission for verified retirement during closing;
staging, sealing and publication retain the ordinary closing refusal.

Prepared edits retain typed scopes. The publication path rechecks current leases,
closed/read-only state, exact generation/revision and cancellation before marking
source fragments. Marking and document adoption perform no source I/O or caller
callbacks. Metadata-only adoption cannot publish a generated raster update.

A raster batch prepares linked planes, layer placement, source definitions,
selection geometry and both coverage sources in one candidate and revision.
New retained originals have their own sealed output owners. Duplicate targets,
ambiguous origins and foreign metadata admissions fail with the complete batch
available for cleanup. Changed source definitions/transforms require the rendered
color output; fill materialization is an explicit transition paired with output.
Source replacement currently retains the existing definition validator and caps.
The native source evaluator and complete command policies remain separate gates.

Native history captures a constant-size cursor/owner snapshot. Workers prepare
the history record, coalesced gesture scopes, immutable undo/redo header and
retirement capacity before publication. The final write-fenced step rechecks the
cursor, actor, leases, document generation and cancellation, then adopts the
candidate and history record together without allocation or source I/O. Retired
owners are returned for release after leaving the engine mutex. Undo and redo
advance revision monotonically while sharing the exact native sources. Selective
task compensation and runtime selection replay ownership still require migration.

The editor capture factory binds the observed generation, revision, actor and
targets after checking the current write fence and reservations. It performs no
source I/O. Workers retain that admission with their candidate; native history
refuses a data-only capture without editor admission. Final publication rechecks
the actor, targets and current leases. The production factory accepts an immutable
normalized command descriptor; generic caller-supplied scope capture exists only
in tests. Descriptor construction and typed parameter normalization run outside
the engine mutex. Capture shares its admitted scope owner and binds generation,
revision, method classes and command tokens without copying the parameter tree.

Prepared publication inherits the engine-derived command scopes after checking
its actual targets. A command view exposes those exact scopes and checks a typed
producer's baseline-to-proposed logical write box. Read halos remain independent.
Producers must retain their actual changed-tile/write proof; semantic fill
materialization is a distinct baseline proof.

A native transaction derives each next command from the original immutable
descriptor, recomputes its scopes against the current private working metadata,
and retains every provisional output. Intermediate generations keep the original
source revision and do not publish blocks or enter history. The final candidate
advances once, retains the original document generation guard and the union of
initial and working command scopes, and enters one history record. Foreign,
duplicate, out-of-order or cancelled steps cannot advance the working state.
An intermediate step receipt cannot enter history or publish independently, even
when its first working generation still equals the public document generation.
Superseded intermediate fragments currently remain conservatively retained;
reachability retirement/compaction remains required before full admission.

The normalized metadata dispatcher handles layer creation, updates, deletion,
ordering, clipping and parenting, plus mask removal, enable and link changes.
Canonical roster rebuilding preserves the exact immutable sources and removes
only unreachable locations. Folder foreground fill uses a separate typed
insertion paired with its sealed complete canvas output; it never replaces the
folder's dummy plane. Other structural/source/effect commands and selective task
compensation remain required before live editor migration.

This is the data preparation boundary. Engine actor/reservation enforcement,
history publication, complete source mutation/renumbering, native operators,
recovery and coordinated live document replacement must be connected and tested
before enabling larger documents. The metadata-copy path is conservative; it is
not a performance or total application RAM qualification.

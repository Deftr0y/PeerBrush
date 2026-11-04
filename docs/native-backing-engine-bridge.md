# Explicit engine backing bridge

This component keeps the current resident `Raster` and document admission as
the live authority. It does not qualify or enable 16000-square documents.

`raster::backed::Plane` contains immutable native tile descriptors and an owned
store handle. `pin` reads and verifies every present requested block before
returning a bounded view. Sampling a view performs no I/O. Missing descriptors
mean known transparent tiles; a present foreign, missing or corrupt block is an
error. Edge tiles retain the full 256-pixel stride. Native16 bytes are big endian.
The index constructor alone grants no imported-source or editable authority.

`engine::backed::Source::capture` clones resident source descriptors under the
engine lock. After releasing all engine/root/registry locks, `prepare` stages the
plane, pins a bounded view and prepares one complete aligned tile. Shared-pool
scratch and conservative COW reservations precede allocation. Cancellation and
backing errors return without changing the document. The typed payload is
single-use and bound to its document, exact revision, layer, shape and depth.
An atomic cancellation handle survives preparation and is checked again before
transaction publication; no caller callback or disk I/O runs under that lock.

`commit_backed_pixels` consumes that payload through the existing edit
transaction, including reservations, source checks, native depth, paint/source
protection, soft selection, retained-original reconciliation, budget validation,
rollback and history. The existing JSON pixel protocol remains little endian.
Typed canonical bytes avoid base64, encoded JSON and decoded-pixel copies.

The first bridge intentionally refuses shifted, partial and multiple tiles.
Current resident document/history/clipboard/proposal owners are not registered
in the new pool, so its charges are not a claim about total application RAM.
The temporary COW reservation is released when the transaction finishes;
resulting resident/history tiles remain under existing legacy policies, without
a continuing shared-pool charge. Legacy resident setters still allocate through
infallible Vec/Arc operations; the precursor does not replace their allocator. The store is not yet authoritative recovery data for a
live document, and ordinary document/raster serde never performs backing I/O.
Full ownership admission, fallible render/edit consumers, durable recovery and
profile-backup integration must precede live eviction or larger document caps.

The derived scene bridge captures the current document or gesture preview through
`Engine::capture_scene`. It clones native owners without source I/O. After all
engine/root/registry locks are released, staging uses the admitted persistent
source runtime, reuses unchanged COW tiles only after a weak owner upgrades and
matches the exact Arc, and seals the referenced blocks in every physical store.
The returned operation retains the runtime session and selected checkpoints
through its last source read. A cached scene must not indefinitely retain an
active session. A wholly absent plane needs no source runtime session or files.

Scene identity includes typed metadata and every resolved physical source key;
uncommitted preview pixels cannot alias the committed revision's render cache.
Retained originals and historical Liquify clips occupy explicit source roster
locations. Protected Photoshop structure and raw pattern inspection owners stay
runtime-only. Capture or staging does not promote those views to editable data.
Resolved tile fragments share immutable Store owners, retain full edge padding,
and keep descriptor admission through the final history/clipboard/task owner.
These derived sources coexist with resident authority until all live consumers,
publication, recovery and cleanup paths are migrated and qualified.

# Bounded selection flood traversal

The selection flood core accepts caller-owned workspace buffers and fallible
matching and span-output callbacks. It does no source I/O, creates no coverage
source and cannot publish a document. A source error, cancellation, work refusal
or full frontier prevents completion; that workspace cannot resume afterward.
Output spans remain provisional until the complete selection worker succeeds.

The ordinary resident Wand/Quick caller retains the existing 16-megapixel
selection admission and 64-million-candidate work limit. Its preview, coverage,
combination, soft-edge, contour, source and history paths remain resident and
require their existing admission. The caller's fallible workspace allocation is
not a global application RAM ledger or large-document acceptance.

Traversal expands contiguous horizontal runs. Four per-seed states distinguish
unknown, rejected, pending and processed pixels, so a source predicate runs at
most once per candidate per seed. Tile-local generation stamps avoid clearing
the full workspace for each short seed. Generation reuse clears every stamp
before reusing an old value. A fixed ring contains at most 65,536 pending span
starts. Traversal operations have a separate finite work bound.

At 16000 square, padded state bytes total 65,028,096, generation bytes total
15,876 and the full frontier uses 262,144 bytes. These are workspace payload
sizes only. Source regions, coverage, contour metadata, allocator overhead and
other live/history/clipboard owners require separate admission. The core's
dimension arithmetic does not enable that canvas size in the editor.

Large-document integration must supply charged buffers, read native source or
composite regions outside engine/UI locks, stream the provisional span union
into owned native coverage, and bind the completed result to the exact source,
revision, lease and cancellation state. Native16 matching may use the existing
display projection; original artwork and retained sources must stay native16.
Current and previous coverage plus the Wand replay baselines remain separate
immutable sources. No backing error can become an empty selection or a
transparent sample.

The provisional span collector uses bounded caller-owned records and a row
index. It requires a completed flood of the exact same dimensions and refuses
record/work exhaustion, failed collectors and late source checks. Row union
then streams scalar coverage without repeatedly rewriting immutable tiles.
The soft-edge stage uses a `(2*radius+1)*width` byte ring, four bytes per-column
sum, and two width-byte rows. It preserves the existing two-pass integer box
filter, including zero canvas borders and intermediate truncation. Read,
output or final-check failures return no completion receipt. These stages still
need the worker's owned source storage and guarded document publication.

Component checks compare complete masks with an independent pixel BFS, including
holes, tile edges, several seed colors, Quick radius, generation rollover and
failure after provisional output. The opt-in 256MP traversal uses computed
native samples and proves traversal/workspace behavior only. It does not qualify
an actual dense document, styled composition, Save/recovery or native latency.

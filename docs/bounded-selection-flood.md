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

`CoverageSource` retains the native scalar8 surface, document origin, bounds and
shared contour metadata without a serialization or infallible pixel interface.
Contour payload and its reservation share one private frozen owner; callers
receive a borrowed slice. Surviving Vec capacities remain charged across source
and prepared-region clones. A growth reservation is reduced only after old
allocations have been freed; this does not resize pin, scratch or disk charges.
Its prepared region retains both source ownership and payload pins. Sampling
outside the declared footprint fails; the physical mask exterior is known zero.
The bounds hint does not authorize dropping nonzero raw pixels inside the mask.
The scalar red channel remains authoritative even when independent RGBA alpha
is zero. Staging currently refuses retained legacy mask originals until their
explicit scene collector can preserve them. No source is discarded to stage a
mask. Flood workers explicitly prepare rows/stripes before memory-only matching;
they do not turn a source getter into a backing read.

`SelectionSource` keeps no restriction, rectangle/polygon geometry and native
coverage distinct. Prepared geometry preserves the existing pixel-center and
stale-polygon rules without allocating a full mask. Matching coverage remains
the first choice. Every prepared selection rejects undeclared samples.

The scalar tile writer holds one charged 256-row stripe and writes each native
RGBA8 mask tile once. Zero tiles are omitted only after their rows are known.
It checks the complete scalar-row geometry and every generated block before
returning a candidate source. Bounds and threshold-128 boundary edges are
computed from adjacent scalar rows. The existing 500,000-edge limit remains;
directed contours retain holes and separate diagonally touching regions. Final
source/revision/lease admission remains the engine's responsibility.

The candidate worker connects explicit source-row preparation, flood completion,
span ordering, soft-edge rows, prior-selection combination and the owned tile
writer. It releases traversal buffers before scalar filtering and releases span
buffers before final contour construction. No restriction remains an empty
combination baseline, matching the existing add/subtract/intersect behavior.
The worker checks source/cancellation through every stage and returns only a
candidate coverage source. It grants no larger live-document admission, history
commit or permission to bypass source locks and reservations.

Component checks compare complete masks with an independent pixel BFS, including
holes, tile edges, several seed colors, Quick radius, generation rollover and
failure after provisional output. The opt-in 256MP traversal uses computed
native samples and proves traversal/workspace behavior only. It does not qualify
an actual dense document, styled composition, Save/recovery or native latency.

`regional_refine` prepares native scalar surfaces through explicit source
regions. Shift uses separable bounded monotone queues; smooth, feather, contrast
and guided edge preserve the existing arithmetic and tile/halo boundaries.
The largest local block is 512 square. Local vectors share a 16 MiB scratch
reservation; native source pins, output buffers, source indices and staging disk
remain separately charged. Source reads use declared windows of at most 128 Ki
pixels. An unchanged selection returns no candidate and keeps its history.

`regional_import` samples an already validated image source. It retains the
existing channel, pixel-center bilinear and float-combination order before
rounding final coverage. Luma conversion uses bounded rows through the same
image-library operation as the resident importer. Original grayscale, alpha
presence and color-space facts come from image admission. The existing PNG
loader uses the image library's default sRGB color space; applying a raw PNG
cICP chunk is a separate behavior change. Native image pixels remain intact.
Container decoding, checksums and final source completion belong to the image
decoder, rather than this sampler.

`regional_masks` consumes a fallible prepared evaluator bound to one captured
scene, layer frame, origin, native depth and shared budgets. Raw fractional
factors are rounded at 65535 before native selection combination, target-depth
projection and opaque output alpha, matching the current mask command. The
adapter supplies no alternative mask-stack evaluator and no history authority.

The native tile and decoded-row writers reject incomplete sequences and
nonnative 8-bit samples. Provisional indices retain a separate reservation through
surface construction. Live adoption also requires the permanently charged
surface descriptor owner, concrete captured-source sampler bindings and the
common engine publication checks. These components do not raise resident
selection/import/refinement limits or establish full 16K application acceptance.

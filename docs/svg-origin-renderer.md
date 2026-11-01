# Bounded full-origin SVG renderer prototype

This isolated, unreleased prototype retains the pinned resvg 0.48.1 and
tiny-skia/tiny-skia-path 0.12.0 versions. It does not raise document limits or
qualify full 16000-square SVG editing. The original package licenses and notices
remain in each vendored directory. `vendor/renderer-provenance.json` records the
original registry package checksums, upstream revisions and manifest hashes.

Translating SVG geometry into a cropped pixmap changes scan conversion and
gradient arithmetic. A border halo cannot establish exact compatibility. This
prototype keeps the full logical canvas, original CTM and shader coordinates,
8191-pixel DrawTiler grid, path/shader round trips, scan-converter clip and
horizontal SIMD/tail grouping. Physical buffers contain complete-width rows at
their original logical Y coordinates. Only buffer addressing changes. The
original group bounding boxes and subpixel shifts remain in force; bounded
group/clip buffers retain their own corresponding logical rows. Sampling an
unstored source row latches a refusal which the strict entry checks before
returning output. It never publishes that provisional pixel value.

`resvg::rows::render_rows` accepts at most 128Ki physical pixels. The logical root
is at most 16000 pixels per edge; the existing signed sampler-index range is
checked independently for group buffers. The supported static subset is paths,
linear/radial gradients, strokes/dashes, opacity/blend groups and clips. Image,
text, pattern, filter and mask nodes refuse at this boundary; application import
must continue its existing static SVG validation. A parsed tree must already be
admitted and remain immutable and owned for the entire call. This API does not
admit a tree, open a file, provide source authority or grant Engine publication.

Every physical image/clip allocation acquires its actual shared-pool reservation
before allocating and retains it until its buffer drops. The application
`svg::row_budget::Budget` adapter uses the existing `ResidentBudget` and captured
native cancellation. It reserves bookkeeping separately and splits the128MiB
path-working ticket into existing bounded reservation sizes. These bytes are not
total application RAM: parsed-tree/font/source owners, other renderer consumers,
GPU uploads, tile staging and allocator overhead need their own actual owners.

Path building, cloning, stroking, dashing, edge conversion and gradient vectors
have a scoped monotonic allocation/work ledger. New capacity is charged before
fallible reservation, counting both old and replacement capacities. The current
per-operation limits are 128MiB, 262144 elements per vector and 32 million explicit
work units. Point transformations charge their actual point count as well as
checking captured cancellation; off-canvas paths cannot evade whole-operation
work admission. The real working ticket is owned by the scoped state and kept
alive through returned `Bounded<T>` payloads, including caller-error payloads.
Nested states retain both real owners, enforce and charge every ancestor, and
cannot turn an ignored inner refusal into an outer success. The returned image
exposes borrowed bytes only; it cannot detach a buffer from its owner.

The upstream infallible APIs require a narrow Rust unwind boundary for resource
refusal. An unconstructible private marker is transported with `resume_unwind`,
which neither invokes nor changes the global panic hook. Only that exact marker
is caught; every unrelated Rust panic resumes with its original payload. TLS is
restored on every exit, and no FFI/non-unwinding frame is entered by the scoped
operation. Abort-profile builds refuse at the API entry before callbacks or
rendering. The checked application release profile uses Rust's unwind default;
any future panic-profile change must preserve this entry refusal or replace the
upstream infallible kernels with explicit fallible APIs. A row result is worker
local and cannot be sent across threads while its scoped state is retained.

`scripts/test_svg_rows.py` builds two separate renderer-only fixtures. The oracle
uses the unmodified registry resvg and its registry tiny-skia packages, isolated
from the parent's path patches. The candidate uses the pinned local forks. Both
consume one synthetic corpus, and compare all emitted RGBA bytes without a
tolerance. Fixtures cover fractional geometry, the known affine crop mismatch,
reflection, gradients, dashes, partial alpha, isolated groups, nested clips and
both sides of the8191 tiler boundary. The synthetic fixture budget tests ownership
and refusal; it is not application-wide memory evidence. Executed results belong
to the exact qualified source, never to a later edited prototype.

Open mandatory work: independently executed oracle parity, whole-application
shared-pool/lifetime tests, source-bound streamed tile-band staging, native8/16
projection and full editing/history/save/reopen/recovery acceptance. The legacy stateless band entry
replays scan-conversion prefixes. The ascending prototype below retains fill,
ordinary-stroke and hairline scan state; its integration gates stay open. A production caller must emit
each full-width band once rather than repeat it for horizontal tiles, and needs
qualified bounded scan continuation if prefix replay is too expensive. No full
frame allocation, virtual full-frame slice, guessed halo, tolerance relaxation
or arbitrary-write staging is an accepted substitute. Real16000-square resource
and performance qualification remains a release gate.


## Ascending scan continuation (unreleased)

`resvg::rows::RowPlan` binds one immutable borrowed tree, matrix and full frame.
It accepts only consecutive complete bands, and a refusal or unwinding attempt
permanently closes the plan. Its retained cache has an actual separate allocation
owner; temporary row working tickets do not replace that owner. Exact tagged
traversal ancestry distinguishes reused clips and repeated geometry at distinct
draw sites. Each cached invocation also verifies unchanged geometry bits, full
logical clip and the next expected local band. No lossy key hash or global cache
flag controls admission.

The prepared fill and ordinary stroke route retains sorted mutable edges, the
next subscan and its pending edge insertion. Integer-band cuts flush completed AA
rows with the original arithmetic; they do not restart an earlier scan prefix.
All original tile transforms and restorations remain, including inactive tiles.
Group-pixmap rectangles use the same prepared route where the original renderer
uses path scanning. Physical destination row filtering preserves original shader
coordinates and horizontal SIMD/tail grouping.

The caller explicitly supplies retained byte, invocation and whole-operation
work limits. One shared work/cancellation counter covers validation and every
band; per-band temporary scope resets cannot reset it. The ordinary qualification
fixture admits 64MiB retained state and verifies that it stays charged until plan
drop, independently of row output lifetimes. This is primitive ownership evidence,
not total app RAM qualification. The renderer grants no SourceSession or Engine
publication authority: the caller must preserve its actual source/capture/session
owners and recheck them before/after bands and at final adoption.

The independent corpus now includes identical reused clip sites, legacy rows,
bands and a complete ascending plan. Selected tests also cover repeated/gapped
bands, geometry/matrix/frame changes, resource refusal, cumulative work,
cancellation-control reset and nested ledgers. Qualification receipts must bind
the exact tested sources and platform; earlier primitive receipts never qualify
later edits.

Hairlines retain the original subdivision, clipped fixed-point kernels, cap
coverage and draw order. AA and aliased kernels emit only provably contributing
integer steps for a band. Integer jumps preserve the original repeated clamp
recurrence and endpoint state; no floating-point slope multiplication replaces
the original arithmetic. The exact primitive corpus includes fractional caps,
curves, dashes and both8191 tiler seams; focused tests compare actual legacy
zero-width aliased and subpixel AA pixels, negative-border recurrences, owner
lifetimes and mid-band work refusal. Invalid kernel state refuses the whole plan.

`maximum_rows()` supplies a guaranteed band height under the original logical
isolated-group width bound and128KiPixel physical allocation limit; it does not
translate geometry or guess an interpolation halo. The caller may emit smaller
bands. An operation-owned geometry cache builds dashes, stroke outlines and the
original ordered tile transforms once. It binds original source geometry,
stroke state including exact dash/scalar bits, matrix and logical frame on every
reuse. Retained paths carry the actual reservation owner, including any temporary
clone that survives the cache itself. Shader transforms still follow the
original full tile traversal. Geometry and scan invocations share the retained
byte and invocation limits; output regions do not reset cumulative work.
The independent primitive corpus must remain byte-identical after this reuse.
Cached tile-path comparisons and hairline-program traversal still consume
whole-operation work, so full-document throughput requires measurement.
Legacy uncached rendering remains available at
its existing supported limits. Full 16000-square resource/performance tests,
source-bound tile-band staging, native8/16 editor/history/save/reopen/recovery and
cross-platform qualification remain mandatory before live adoption or any cap
increase.

## Native source staging (unreleased)

`source::stage::render_captured` supplies the actual captured native edit's pure
cancellation token to the row plan. Source/session checks stay outside the
synchronous renderer and repeat before and after bands, during real tile writes
and at output sealing. The earlier staging entry refuses SVG without this token;
analytic and text generation retain their existing regional route.

SVG uses the existing intrinsic scale followed by its source matrix, rendering
directly in full logical output coordinates. This preserves `svg::render`;
rendering an untransformed base and then applying a raster affine sampler would
produce different pixels. Row pixels use the original demultiply operation and
native16 byte-times257 uplift before entering the standard tile writer.

Each full-width band is rendered once. A native tile-row frontier retains at
most16000×256×8 =32,768,000 bytes, under its actual resident reservation, while
the existing writer consumes horizontal tiles in order. Edge padding is cleared,
band geometry and complete consumption are checked, and final output uses the
existing registered `OutputDraft` seal and verified retirement handling. The
frontier grants no source or publication authority and holds no output-file pins.

The staging policy explicitly admits16MiB retained renderer state,16384 total
geometry/scan invocations and one billion cumulative work units; row temporaries
retain their separate128MiB admission. Existing parser/source/index reservations,
the frontier and the tile scratch also remain charged. An exhausted pool or
operation refuses; it never substitutes transparent pixels or bypasses a quota.
These are bounded allocation contracts, not whole-application RAM or throughput
measurements. Dense native8/16 transformed source-session staging, cancellation
and inventory tests require execution on the final integrated candidate. Guarded
runtime caller integration, full16000-square acceptance and every existing
document-limit gate remain open until that evidence qualifies them.

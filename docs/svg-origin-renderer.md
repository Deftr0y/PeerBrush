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
work checkpoints. The real working ticket is owned by the scoped state and kept
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
projection and full editing/history/save/reopen/recovery acceptance. Separate
bands currently replay scan-conversion prefixes. A production caller must emit
each full-width band once rather than repeat it for horizontal tiles, and needs
qualified bounded scan continuation if prefix replay is too expensive. No full
frame allocation, virtual full-frame slice, guessed halo, tolerance relaxation
or arbitrary-write staging is an accepted substitute. Real16000-square resource
and performance qualification remains a release gate.

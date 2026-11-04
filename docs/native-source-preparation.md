# Native editable-source preparation

The native worker borrows `Source36` definitions and an optional replacement
matrix. It does not clone the document's source content, serialize pixels into
metadata, or grant edit authority. Region outputs retain their actual resident
pool reservation. `source::stage::render` writes registered immutable fragments,
seals an unpublished Engine output under the same document/session, and explicitly
retires abandoned fragments. Revision, lease, source slot, selection, readonly
state, history and final publication remain the Engine batch's responsibility.

Analytic shapes and flattened Bezier paths evaluate the original source grid and
the existing native8/16 premultiplied affine sampler. Text keeps full-source
alignment, native glyph positions and complete glyph rasterizer phase; native8
projection occurs after each glyph, matching the existing editor. A transformed
text region declares its actual mapped interpolation footprint before sampling.
A wide footprint splits output regions recursively while preserving global
sample coordinates. The128Ki-pixel output limit bounds this to18 levels; it
does not repeatedly traverse a full source prefix. There is no implicit disk
sampling or transparent replacement for an unprepared pixel.

Each admitted glyph is rasterized once into exact f32 coverage owned by the
prepared operation. Region sampling reads those charged immutable bytes and
does not rerasterize whole glyphs for every stripe. The existing aggregate limit
of32million outline pixels bounds this coverage payload to128MiB; each individual
glyph remains below4Mi pixels. This is glyph-derived data, not a full-canvas
RGBA allocation or editable source authority.

Font snapshots bypass the presentation font cache. Installed bytes are read from
the actual catalog's held regular file, bound to its exact hash/face index, and
retained with their reservation. Callers receive an opaque borrowed `Font`, so
they cannot detach a cloned `FontArc` from its admission. The already pinned
owned parser's repeated Unicode cmap and horizontal kern records are counted
before its cache vectors allocate, with growth/transient capacity covered by the
snapshot metadata ticket. Pinned ttf-parser0.25.1 visits each glyph before
ab_glyph grows its curve vector. It
refuses the first event beyond32768 and checks cancellation before parsing and
at bounded event intervals. Existing font-format restrictions remain intact.

Native shaping additionally counts repeated OpenType lookup/subtable references
and AAT cache records before harfrust constructs caches. The operation holds
192MiB of shared working admission for harfrust's input-dependent buffer growth,
cache payloads and temporary plans. Curve work holds a separate8MiB ticket;
whole-glyph rasterizer scratch is limited to4Mi pixels and charged before
allocation. These tickets are conservative component admission, not a measurement
or guarantee of total application RAM. Hostile font-table cache amplification,
all library allocation paths and shaping CPU cancellation still require the
full integration/resource audit before the native large-document gate opens.

SVG keeps its existing resvg RGBA8 projection, including in16-bit documents.
Definition metadata remains editable. Translating the rasterizer's canvas to a
cropped region changes affine path coverage; a fixed halo does not establish
pixel equivalence. The intermediate native adapter therefore retains a charged
full-origin projection of at most4Mi pixels and crops exact region bytes from
it. Larger SVG output explicitly refuses while retaining the current appearance
and definition. A global-phase streaming SVG renderer is still a full16K release
gate; this fallback does not satisfy it or authorize raising live canvas limits.

Focused native8/16 comparisons cover legacy/shaped text, glyph overlap, partial
alpha, affine transforms, SVG gradients/clips/isolated opacity, and exact source
phase. Parser fixtures include large simple outlines and repeated composite
graphs. These component checks do not replace actual Native Engine publication,
undo/redo, save/reopen, recovery, native workspace or physical16K qualification.

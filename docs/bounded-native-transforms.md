# Bounded native transform preparation

The native sampler consumes one immutable `Surface` and validated affine,
projective or retained Warp placement. Its output is a complete canonical tile
in the source's native8 or native16 depth. It performs no Engine mutation,
history update, source registration or publication.

Actual output sample coordinates determine each required bilinear source
footprint. The sampler explicitly prepares that footprint before reading its
memory-only pixel view. If its area or pin admission is too large, it divides
the output region and retries the smaller footprints. Coordinate scratch is
reused across subdivisions. Every map evaluation, including repeated scans,
consumes the caller's work allowance. Identity copies charge their complete
physical tile, including padding. Cancellation or source failure returns
no tile; partial output cannot become a completed source.

Sampling preserves the existing native8 f32 and native16 f64 premultiplied
arithmetic, integer copies, fill-edge behavior and rounding. An exact identity
with an unchanged frame preserves the old clone path's physical edge padding.
Resampled edge padding is zero. Known geometric exterior is distinct from a
failed backing read; no source error substitutes transparent pixels.

Native support bounds scan one admitted physical tile at a time, including
hidden RGB and native16 low bits while excluding physical padding. The scan
charges logical visited samples before reading a tile. Retained placement
validation is shared with the resident `Original` validator, whose existing
layout and live-size checks remain in place. Native input frame planning uses
the same placement-bounds helper as the resident original. Bounds retain raw
coefficients and snap mapped points; rendering separately snaps coefficients
before producing and retaining its pixels.

Warp preparation retains a separate bounded metadata reservation. Output,
coordinate scratch, source pins and permanent source descriptors use the same
resident pool. The source's disk pool is also checked. These are component
admissions, not a total application RAM or performance claim.

The provisional whole-plane writer streams these tiles into the caller's owned
store, retaining its index admission through permanent descriptor conversion.
It emits explicit known zero when every physical byte is zero. Its caller must
retire any unused fragment, then seal nonzero output through the common session
before requesting a prepared edit; a completed plane alone grants no authority.
New fill originals use bounded canonical tiles with the existing authored-color
conversion and zero physical edge padding. Their staged original and transformed
current plane must both survive until one batch is accepted or retired.

Retained layer planning reads borrowed native metadata and keeps its placement
arrays charged. It includes linked mask support, independent mask reframing,
mask-only paint-step selection and complete editable-source frame placement.
Planning does not publish pixels. The ordinary size and work checks still apply;
the concrete regional editable-source evaluator remains a separate integration
requirement.

The final consumer must use the common captured edit, generation-bound source
slots, sealed source-session output, retained-source action and guarded
`PreparedDelta`. Original sources and placement metadata remain in their owning
native document until that adoption succeeds. The read-only retained input
projection binds both current and original slots to that exact generation and
borrows the complete original placement. It also accepts mask/coverage raster
projections without converting them into resident pixels. A foreign generation
refuses before source preparation. The single-plane precursor uses the actual
registered source session, seals its complete output through `OutputDraft`, and
prepares a generation-bound raster patch with the original source and placement.
Cancelled or refused output uses registered fragment retirement after every
candidate owner drops. The caller retains the session through final adoption.
Closing-safe retirement additionally requires the concrete runtime's cleanup-only
admission binding.

The complete-plan producer stages all planned layer and mask planes in one
`RasterBatch`. Newly authored fill originals use `ReplaceOutput`, so their source
and transformed pixels share the candidate's one revision and refusal cleanup.
It validates source generations and shared pools before starting any output,
retains packet admission through candidate preparation, and retires earlier
completed planes if a later plane fails. It refuses an editable-source plan
before staging until its regional renderer is connected. Small differential
fixtures compare native8/native16 current, original and mask bytes with the
resident operators and test cancellation and duplicate-target refusal. This
producer does not perform final guarded Engine adoption.

The production entry accepts a captured command token, looks up its immutable
normalized parameters in that exact descriptor, and checks every proposed
logical write frame against the Engine's derived command scopes. Caller-built
plan arrays are private to preparation. Folder plans include hidden descendants
and nested groups, reject locked children, and keep linked and independent mask
placement. Their default affine pivot uses current native alpha support; hidden
RGB and padding cannot move that pivot. Hierarchy visits and alpha scans consume
the same work allowance. A shared default-pivot argument preserves the original
normalized command without allocating modified JSON per child.

Selected-pixel sampling evaluates a virtual native cut and remainder directly
from explicitly prepared current-source and coverage footprints. It keeps the
old native8 alpha quantization, native16 channel bits, f32 selected inverse
mapping, premultiplied interpolation and blend order. It splits mapped regions
when pin/area admission is too large. No resident or temporary full-frame cut
and remainder are produced. The writer preserves physical tile presence and
padding where the resident clone path does, and reports retained-source
invalidation independently of the final pixel comparison.

The shared Engine has two geometric-selection routes: an ordinary layer
transforms its matching polygon directly, while folder dispatch rasterizes
geometry into a canvas-clipped scalar mask first. Matching raw coverage remains
a separate source; its physical exterior, rather than its bounds hint, determines
known zero during scalar resampling. The scalar writer retains the existing
f32 expression, ordered-row completion proof and bounded contour extraction.
The selected command producer consumes the same captured descriptor token and
prepares color, every linked or independently selected paint-mask step, expanded
untouched mask planes, selection geometry and scalar coverage in one native
batch. A private working plane retains current and original output tokens until
the whole candidate is prepared or retired. Final linked-mask reframing reads
the retained original, while expansion of untouched planes copies their current
pixels and translates the retained placement. Current and previous raw coverage
keep their complete native source metadata.
The final resident reconciliation rule also applies: an equal original
descriptor is cleared if the completed current plane changed. A retained-mask
reframe compares bounded pairs of prepared native footprints in document
coordinates and checks its actual changed bounds against that same command's
planned write scope, including samples restored outside the selection.

Expansion also translates local Liquify controls and typed clip placement using
admitted metadata copies. Clip pixels and their retained originals keep their
generation-bound source slots. Unknown settings survive. No inline pixel JSON,
separate intermediate adoption or resident cut/remainder is introduced.

The producer still requires final guarded publication through the shared Engine
or working transaction. Concrete profile/source-session lifetime, one-history
undo/redo, native editor acceptance and full dense16K qualification remain
separate gates. Existing live limits remain unchanged.

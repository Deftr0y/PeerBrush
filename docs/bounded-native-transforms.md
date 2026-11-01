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
selected-pixel/folder transactions and the concrete regional editable-source
evaluator are separate integration requirements.

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
admission binding. Linked and unlinked mask/selection command integration,
cancellation/history acceptance and
full dense16K qualification are separate gates. Existing live limits remain
unchanged.

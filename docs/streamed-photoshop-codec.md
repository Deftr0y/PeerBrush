# Streamed Photoshop codec foundation (unreleased)

The structural index and row readers are preparation for bounded source loading.
They do not enable larger documents, change saved source formats, replace the
active decoder or establish release qualification.

`psd::structural::index` accepts a seekable, already leased source, explicit
limits and a cancellation check. It records checked 64-bit spans instead of
materializing complete resource and layer sections. Its default profile retains
the current 8192-edge, 32 MP and 256 MiB file limits. Image resources and the global
mask retain 32-bit lengths in PSB; layer-info and channel lengths widen. Tagged
lengths follow the existing codec's key-specific policy, including when the
signature is 8B64. Unknown ordinary tags remain opaque indexed spans.

Layer descriptors retain raw record, mask, name, blending-range and tag spans,
alongside blend/opacity/clipping/flag fields and channel geometry. The selected
primary or depth-specific layer-info set follows the existing selection policy;
inactive compatibility records remain opaque spans. Duplicate sources, tags,
channels and conflicting primary/global records are classified explicitly.
Neither classification nor indexing grants permission to restore editable data.
The existing unsupported-feature, profile, metadata/source, native-depth and
saved-composite checks remain necessary.

`psd::row_stream::Source` supports separate bounded cursors into the same source.
Semantic metadata consumers can borrow a bounded read/seek range while native
channel readers remain alive. Each reader supports raw, PackBits, ZIP and
ZIP-predicted 8/16-bit rows. Predictions reset at row boundaries; native 16-bit samples
remain big-endian pairs, without an 8-bit projection. ZIP decoding requires an
actual stream-end status, checksum, exact output size and exact consumption of
the selected encoded range. Extra output, concatenated streams, truncated
footers, malformed row tables and trailing bytes fail. A failed or canceled
reader cannot resume and later produce a completion token.

Successful `Rows::finish` returns an opaque token bound to the exact source
instance and complete encoding descriptor. Its check repeats the caller's
cancellation/identity callback and source-length observation. Decoded rows and
new immutable tile blocks are provisional until **all required streams finish**
and the caller independently rechecks its source lease, identity and whole-file
hash. Completion tokens must not be serialized as recovery or publication
authority.

Independent layer-channel ZIP streams can be interleaved by row or stripe.
A ZIP merged composite is one planar stream across all channels; it cannot be
randomly sought by channel. A bounded full native import therefore needs an
owned planar scratch with completed-stream/hash rechecks before stripe reads,
or a separate preview-only pass when authoritative editable sources restore.
No scratch-file admission expansion is implemented by these readers.

Rows contain logical on-disk channel samples. Existing composite white-matte,
alpha and semantic interpretation must run before treating them as engine RGBA.
Current Raster tiles always use a 256-pixel stride and full 256x256 payload,
including raster edges. A tile sink must explicitly stage that layout; it must
not trim retained originals, off-canvas mask samples or private source padding.
Conversion from canonical big-endian storage into native `u16` vectors must
charge any additional resident copy.

Focused coverage includes native 8/16-bit PSD/PSB raw/RLE fixtures spanning multiple
tile rows, ZIP/prediction fidelity, interleaved cursors, stream termination,
completion binding, cancellation, metadata/row bounds and arithmetic above 4 GiB.
The large-offset fixture is a virtual sparse source and proves only span
arithmetic. It does not qualify native filesystem behavior, dense large files
or 16000-square editing. Application integration, actual large-file/disk-failure
acceptance and cross-platform/native receiving-application checks remain open.

The length and compression rules are based on Adobe's
[Photoshop File Formats Specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)
and the existing codec's reviewed compatibility policy. Replacing the active
reader still requires comparison against its supported corpus, including
padding and protected unsupported records.

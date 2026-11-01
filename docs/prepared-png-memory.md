# Prepared PNG worker ownership

The worker path in `image_import/prepared_png.rs` and
`color_profile/prepared.rs` uses the document's `ResidentBudget`. It does not
call the legacy pattern cache or the global CMS transform cache. The original
encoded source remains separately owned; row delivery stays provisional until
the decoder checks all image data and the complete IEND. A returned completion
is source evidence, not document publication permission.

The original container also receives an allocation-free CRC pass, with
cancellation checks between at-most64KiB chunks. This is required because
png0.18.1 skips CRC verification for ignored iCCP chunks. RawSelection may skip
unused ICC decompression, but cannot skip verification of its encoded bytes.
Consequently an invalid IEND CRC is rejected before row delivery; decoder,
compressed-stream checksum and row-sink failures still return no completion.

png0.18.1 also intentionally swallows iCCP parsing/decompression/limit errors.
The container's explicit iCCP declaration is therefore bound to a successfully
decoded profile before Raw/ToSrgb can deliver rows. Missing decoded metadata
cannot silently turn a declared profile into untagged sRGB, including when the
declaration accompanies nonstandard gAMA. RawSelection's intentional unused-ICC
policy remains separate.

The current pattern policy retains the existing 4 MiB encoded/512-square
limits. The selection policy keeps 64 MiB encoded, 8192-edge and exactly
16,000,000 pixels. Raw decoding can preserve ICC metadata; the explicit
RawSelection compatibility policy skips unused ICC decompression while binding
the original encoded bytes and chunk CRC. Neither silently adopts pattern
color conversion. A future caller
can supply explicit component limits up to 16000/256,000,000, but that does not
change live admission or qualify large interlaced imports.

RawSelection retains the existing default-IDAT-image behavior for APNG input,
including files with a separate default image. It does not compose animation
frames. The completion still requires validation through IEND. Pattern decoding
continues to require one static image.

## Pinned dependency audit

These envelopes depend on the versions pinned in Cargo.lock: png 0.18.1,
fdeflate 0.3.7 and moxcms 0.8.1. Changes to their allocation behavior require
rechecking the envelopes. This is an admission bound for these owned buffers,
not a measured process RAM limit. Allocator headers, fragmentation, unrelated
application memory, system libraries and thread stacks are outside this claim.

The PNG decoder has an 8 MiB internal `png::Limits` allowance and a separately
held 32 MiB resident reservation. The larger reservation is necessary because
the library describes its limit as best effort and because temporary old/new
allocations can coexist:

- `decoder/stream.rs` charges raw chunk growth and retained metadata through
  `reserve_bytes`; ICC decompression is bounded by the remaining allowance
  before its final length is charged. Ignored text is not decompressed.
- Raw chunk/decompression vector growth can temporarily retain old and new
  buffers. The reservation covers up to three allowance-sized generations
  (24 MiB), rather than counting only the final vector length.
- `decoder/unfiltering_buffer.rs` initializes at most 128 KiB and uses the
  four-row shift threshold, lookback and 8 KiB growth increments. With at most
  16000 native RGBA16 pixels per row, a row is at most 128001 filtered bytes;
  the power-of-two shift threshold is at most 524288 bytes. The row, scratch,
  lookback and reallocation coexistence fit in the remaining 8 MiB.
- The inflater's fixed state, row transform and reader bookkeeping remain
  inside that remaining allowance. Input transport, output rows, decoded
  interlaced frames and retained ICC copies are charged separately.

`ScratchBytes` admits the complete encoded base64 upper-bound capacity before
decoding. The resulting source length and both exact transport/PNG SHA-256
identities are retained. Chunk geometry is inspected before decoder creation,
so an excessive IHDR cannot cause an image-size allocation. Every interlaced
decoded frame uses checked, admitted bands of at most1MiB, including the charged
index and reconstruction through every Adam7 pass. It is never covered
by the small decoder ticket. Noninterlaced images stream rows. A final canonical
pattern allocation is also separate. Native channel buffers retain low bits;
8-bit output words are exactly byte-times-257.

The uncached CMS owner holds seven allocation-group reservations totalling
128 MiB from before profile parsing until the
last executor owner is destroyed. Its fields never export an executor Arc
independently of that owner. The envelope includes the following conservative
allocation groups, including their construction overlap. Each group is at
most48MiB and all underlying buffers remain below the unchanged64MiB single
buffer guard; these reservations do not authorize one oversized allocation:

| Group | Reserved share | Source bounds |
| --- | ---: | --- |
| Derived/profile input and fixed parser state | 8 MiB | 4 MiB profile, at most128 tags, possible Lab normalization copy |
| Parsed numeric tables | 16 MiB | Six classic RGB A/B LUTs, bounded auxiliary gamut, three native TRCs |
| Profile text and localization construction | 8 MiB | Cumulative preflight charge includes overlapping strings and vector capacity |
| Both retained executors | 16 MiB | Fixed 65536-entry gamma/linear and interpolation tables; bounded selected CLUT |
| Executor construction intermediates | 48 MiB | Source/destination numeric copies, float CLUT and stage construction coexistence |
| Row conversion intermediates | 8 MiB | Both8/16 conversion methods reject more than16000 RGBA pixels |
| Fixed object/dispatch/construction margin | 24 MiB | Retained through the full owner lifetime |

For numeric bounds, `classic_lut` checks all six A/B tables before CMS entry:
three input/output channels, grid2..33, curve entries2..4096, and exact payload
length. One maximum16-bit table is below 265 KiB. Three direct TRCs are limited
to65536 entries by `ParsingOptions`. An auxiliary gamut table's channel counts
are checked at1..16. In `reader.rs`, its three nested curve groups therefore
contain at most48 bounded curves (6 MiB of native words); its CLUT must also fit
inside the at-most4 MiB input tag.
`reader.rs::read_lut_table_f32`, despite its name, stores native u8/u16 LUT
arrays. The mAB reader has its own hardcoded10,000,000-sample check rather than
the supplied classic-CLUT parsing limit; the auxiliary physical tag-length
bound and channel/curve checks are therefore necessary. Float conversion in
the selected executor factories belongs to their separately admitted groups.
The factories in `transform.rs` and
`conversions/{lut_transforms,prelude_lut_xyz_rgb,rgbxyz,interpolator}.rs` build
fixed-entry gamma/linear/interpolation tables and selected classic CLUT stages.
The largest interpolation weight contains two i32 indices plus one scalar;
its65536-entry table is below1 MiB. The profile and descriptive strings are
consulted during construction, not retained in a process-global cache.

`metadata_bound` checks localized strings before moxcms allocation. Repeated
offsets are charged repeatedly: each record adds256 bytes plus eight times
its declared payload, including UTF-16 decoding, UTF-8 loss replacement and
string/vector coexistence. Plain/description payloads add eight times their
tag length. The sum is capped at8 MiB. Thus a small ICC profile cannot amplify
overlapping localization references into unbounded allocation. Metadata
refusal is explicit; there is no dropped-profile or transparent-pixel fallback.

`ChargedWords` admits twice the requested native-word capacity, uses fallible
reservation, checks the actual capacity against that admission, and never
exports its underlying vector. Caller-owned row output and byte buffers use
the same resident pool. Native transformations preserve the alpha channel.
The canonical sRGB tag bypass uses a local encoded target under the CMS owner,
preserving the existing timestamp-insensitive identity comparison without
initializing the global profile cache.

Physical release order matters: decoder internals drop before their reservation;
CMS executors drop before the CMS reservation; byte/word allocations drop before
their corresponding charges. A failed decoder, sink, cancellation check or final
checksum returns no completion owner. Callers must discard every provisional
durable row/block after such a failure and independently validate scene revision,
source, lease, selection and slot identity before engine adoption.

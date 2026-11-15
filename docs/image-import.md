# Image import

Unreleased TGA update, 2026-11-10. Native Open, Import, placement, file paste and image patches accept supported TGA files up to 16000 pixels per edge and 2 GiB per encoded file. Raw and RLE rows feed admitted tiles. Image Info reads the bounded header and checks that the file can hold the declared pixels. The decoder consumes packets from the existing guarded transfer buffer and groups fully zero rows before tile staging. Rows containing hidden RGB are retained.

Unreleased BMP update, 2026-11-10. Native Open, Import, placement, file paste and image patches accept supported BMP files up to 16000 pixels per edge and 2 GiB per encoded file. Raw and RLE rows feed admitted tiles. Image Info reads bounded metadata and checks the file's pixel extent. The decoder uses the held source's guarded buffer for small packet reads. RLE checks cancellation in bounded groups, including packets that make no row progress. Existing precision and color checks still apply.

Unreleased Netpbm update, 2026-11-10. Native Open, Import, placement, file paste and image patches accept PBM, PGM, PPM and PAM up to 16000 pixels per edge and 3 GiB per encoded file. The decoder retains the 64 KiB header bound, reads rows into admitted tiles and preserves 16-bit samples and alpha. Image Info reads the bounded header. Resident decoding retains its existing limits.

Unreleased QOI update, 2026-11-10. Native Open, Import, placement, file paste and image patch accept QOI up to 16000 pixels per edge. The decoder writes RGBA8 rows to admitted tiles and retains the 128 MiB encoded-file cap. Import into a native16 document promotes incoming QOI samples and preserves the existing 16-bit artwork. Image Info reads the header without loading the full file. Resident image decoding uses its existing limits. The native decoder consumes opcodes from the guarded source buffer and checks cancellation per row. It stages exact RGBA8 rows directly and groups fully zero rows. The held-source digest and exact end-marker checks still apply.

Windows 0.4.0 verified Image Info, editable Open and native-depth PNG readback for four 16000 × 3 fixtures. A bottom-right-origin RGBA8 TGA, a bottom-up RGB8 BMP, an RGBA16 PAM and a literal-pixel RGBA8 QOI each matched all 192,000 output channel samples and kept the input file byte-exact. Importing the QOI into an existing RGB16 project preserved the original low-bit sample and promoted the incoming values exactly.

Two sparse 16000 × 16000 fixtures also opened as editable RGBA8 paint layers on the Windows development host. The direct-row QOI build opened its fixture in 71.7 seconds, compared with 836.0 seconds in the earlier replay of the same file. The bottom-right-origin RLE TGA opened in 81.9 seconds. Independent PNG readback matched all 1,024,000,000 channel samples for each fixture, and both inputs remained byte-exact. Dense canvases and other variants remain unqualified.

A Windows 0.4.0 RLE8 BMP replay opened a 16000 × 3 image containing absolute packets and one-pixel runs in 8.3 seconds with packet batching. The earlier build took 418.8 seconds on the same fixture. Both runs matched all 192,000 channel samples in independent PNG readback and kept the input byte-exact.

Unreleased clipboard update, 2026-11-10. External static PNG clipboard images can paste directly into disk-backed documents within the 128 MiB encoded cap, native geometry and source budgets. Bitmap clipboard formats and resident destinations retain their existing decode budgets. Clipboard PNG animations require an explicit file import to choose a frame.

Unreleased precision correction, 2026-11-09: when the incoming clipboard image and retained copy both contain 16-bit samples, those samples must agree before PeerBrush reuses the retained copy and its document origin. Equal 8-bit display bytes alone cannot replace different incoming native samples. Display-only OS images retain the existing internal precision fallback. Regression source covers matching and differing native samples, providers with and without a sequence value, precision promotion and undo/redo; runtime acceptance remains pending.

Unreleased clipboard update, 2026-11-09: OS bitmap transport and native paste accept up to 16000 pixels per edge, subject to retained/decoded byte budgets and the 128 MiB PNG transport cap. Copy/cut checks the paste decoder working-memory budget before publication as well as retained bytes. Windows PNG preserves native16 samples; DIB supplies display bytes. Other OS bitmap writers validate the actual PNG command size before replacing the clipboard. Clipboard PNGs require a complete valid static container and supported color metadata. Resident document commands retain their original canvas policy. Wide transport regression source is prepared; installed-platform and runtime acceptance remain pending.

Unreleased native placement update, 2026-11-09: verified static PNG files use the same 16000-pixel edge and 3 GiB held-file limits for import, placement, file paste and raw image patches. Source tiles remain subject to the configured resident/disk pools. Native placement validates destination and combined layer frames at up to 16000 pixels per edge, including scaling a smaller source. File paste of these PNGs retains native tiles instead of applying the resident clipboard-buffer estimate. Static inline base64 PNG placement/paste also accepts 16000-pixel edges, with a 128 MiB encoded-byte cap; inline paste retains the cumulative 256 MiB import budget. Animation and other codecs retain their separate decode limits. Managed placement/paste still applies source color/orientation handling and promotes destination precision when needed; raw patches keep their existing coordinate/color policy and refuse a 16-bit source on an 8-bit destination. The document canvas does not grow merely because an imported layer is larger. Regression source is prepared; runtime, performance and native acceptance remain pending.

PeerBrush supports the following raster and vector imports; unreleased additions are labelled below. Use File → Import, drag files into the workspace, or issue `image.import` through the shared document engine. A batch is one undo step. Import never replaces an existing project.

| Format | Import behavior |
| --- | --- |
| PNG | Native 8/16-bit gray, RGB and alpha. Palette images become RGBA8. APNG preserves sample depth and requires an explicit frame. |
| JPEG | 8-bit RGB/gray decoded to RGBA8; EXIF orientation is applied. Higher-precision and four-channel CMYK/YCCK source headers are rejected before decoding. Lossy source data cannot recover detail lost during encoding. |
| JPEG XL / JXL (unreleased PB-143) | Static SDR RGB with optional straight alpha at native 8/16-bit depth, including lossless and lossy files. Orientation is applied. Animation, grayscale/CMYK, floating-point/HDR, associated alpha and extra channels reject explicitly. |
| BMP | Basic decoded bitmap/palette images become RGBA8. Bitfields wider than eight bits per channel and calibrated/profiled encodings refuse explicitly. |
| GIF | Palette images become RGBA8. Multiple frames require an explicit composited frame. |
| TIFF / TIF | Native 8/16-bit gray, RGB and alpha. Multiple pages require an explicit page; selecting a page never changes the source file. Floating-point, signed or unsupported color encodings are rejected. |
| WebP | Static and animated RGBA8. Multiple frames require an explicit composited frame. |
| ICO | Choose an explicit icon variant when multiple images are present. Cursor files are unsupported. |
| PNM / PBM / PGM / PPM / PAM | Basic Netpbm images, including native 16-bit samples where present. |
| TGA | Basic decoded RGB/gray images become RGBA8. |
| QOI | Decoded RGB/RGBA8. |
| SVG / SVGZ | Supported self-contained static vector artwork retains its original markup and editable affine placement. Its colors and coverage render through RGBA8, promoted when the project is native16. |

The Open and Import dialogs number choices from one. Use the previous and next buttons or enter the frame, page or icon variant number directly. Use the scrubber to move through longer frame or page lists. For icons, the size menu lists each variant’s width and height in pixels. MCP/CLI commands use zero-based `frame`, `page` or `variant`. `peerbrush_image_info` returns the choice type and count without editing the document. Its dimensions and depth describe the initial decoder header. Another TIFF page or icon variant can have different dimensions or depth. Import checks the selected image again and promotes an 8-bit project when the selected source contains native 16-bit samples.

Unreleased native document import accepts mixed formats in one batch. If a file changes after inspection, choose the batch again. A failed import leaves the document and undo history as they were before the batch.

Unreleased GIF preparation: frame counting and ICC-extension refusal now share
a streaming container inspector with fixed palette/sub-block buffers and
cancellation checkpoints. Resident import uses the same inspector. This is
also used by the pending native GIF producer for import, placement and paste.
The producer decodes one palette row at a time, stages interlaced rows and
composes the selected frame with GIF transparency and disposal rules. Static
GIFs retain hidden palette RGB; animated frames use the existing compositing
behavior. Multiple frames require an explicit zero-based `frame` selection.
Native GIF canvases and decoded frame rectangles can reach 16000 pixels per
edge. Metadata, decoder buffers and staging remain budgeted. Selecting an
animation frame charges every decoded frame and any required canvas composition.
Earlier frames with restore-to-previous or restore-to-background disposal are
validated row by row without staging their pixels. The limit is 128 million pixel
operations or four times the canvas area, whichever is greater. The resident
geometry, encoded file and frame-count limits are unchanged. Existing document
pixels remain at their original precision. Native patches use the raw first frame, including hidden
palette RGB, and intentionally keep the existing patch command's unmanaged
color behavior. GIF import/placement/paste still refuse embedded ICC extensions.
Live authority activation and runtime acceptance remain pending.

```json
{"op":"image.import","path":"/absolute/artwork.tiff","page":1,"x":40,"y":20,"parent":"folder-id"}
```

`peerbrush_place_image` accepts the same formats and choices, or base64 PNG. Placing native16 pixels into an existing layer promotes the project before resampling or compositing. On Windows, Explorer file paste supports the static formats above. Placement and external image-file paste rasterize SVG; use Import to retain an editable vector source. Files requiring a frame/page/variant choice report that requirement; use Import to make the choice. PeerBrush's internal whole-layer clipboard retains native samples and editable sources.

Supported embedded RGB matrix/TRC and classic LUT ICC profiles are converted to sRGB at the source channel depth, preserving alpha. Unsupported ICC profiles, CMYK and floating-point sources require an explicit supported RGB conversion before import. Untagged images are interpreted as sRGB. Imported working RGB carries a standard sRGB profile on PSD/PNG save. HDR/non-sRGB PNG transfer encodings are rejected. This is a bounded raster import path, not preservation of an animation timeline, TIFF editing metadata or Photoshop structures. Open PSD files as projects.

SVG supports paths with curves/arcs, basic shapes, groups, solid colors, linear/radial gradients, local `use`/symbols and clip paths. Outline text before import. Raster images, filters, masks, patterns, markers, stylesheets, scripts/animation, external resources and unknown visual properties are rejected explicitly. Inline styles accept the supported presentation properties. The viewport must be explicit through dimensions or `viewBox`; percentage dimensions require a `viewBox`. Metadata cannot become artwork through a local reference. The source editor previews supported markup through the shared engine; Apply commits one guarded change and Cancel preserves the document.

SVG's RGBA8 projection is an explicit renderer limit. Existing native16 raster channels are never quantized. Editable PSD saves retain the vector definition alongside current standard raster/mask data and a merged composite, using PeerBrush source format 12. Photoshop sees the rendered raster layer. Older PeerBrush readers open the saved composite read-only. At transparent edges, reconstructing straight color from Photoshop's white-matted merged pixels can differ from the original; the tested SVG fixture differed by at most one native16 unit. Alpha and the saved matted appearance remained exact.

PDF, Illustrator AI and EPS remain unsupported after evaluating their page, font, color and source-preservation requirements. Export supported raster artwork or SVG from the source application. Do not rename an unsupported file to a supported extension.

Resident imports limit files to 128 MiB encoded, decoded images to a 256 MiB batch budget, dimensions to 8192 pixels and 32 million pixels, and the document to its existing raster/layer budget. Preview and commit enforce the same cumulative batch budget. Native tiled imports listed above use their file and native store limits. The native picker accepts at most 16 files per batch. Animation decoding also has a bounded frame/work limit. Invalid files, excessive resources, locks, reservations and stale or closed source projects leave the batch unapplied. Canceling the choice dialog changes no history.

Native seek-based codecs decode from an authenticated temporary copy. If Open
or picker inspection already established a SHA-256, the copy pass verifies
that digest directly. Unpinned files establish a digest in a separate scan
and verify it again while copying.

SVG additionally limits markup to 512 KiB, compressed SVGZ to 1 MiB, the source frame to 4 million pixels and XML to 4096 nodes. Reference expansion, nesting and geometry are bounded. Projection checks transformed isolation boxes and conservative nested clip buffers against a 128 MiB working-memory budget and a rendering-work budget before allocating surfaces. DTDs, processing instructions and external resource resolution are disabled.

## Opening images as projects (unreleased PB-120)

Open uses the same bounded decoders and color handling as Import. PNG, JPEG and the other supported image formats create a new project at the selected content’s actual dimensions and native 8/16-bit depth. Alpha stays intact. SVG/SVGZ retains its supported editable vector source. Files with multiple frames, pages or icon variants require an explicit choice; the native Open dialog offers that choice, while `projects.open` and `document.open` accept zero-based `frame`, `page` or `variant`. Use `image_info` to inspect the available choices first.

Image projects start unsaved and have no PSD save destination. Save/Save As writes a new working PSD or PSB. The original image remains owned by its project until Save As or close: duplicate opens and exports over an open source are rejected, including aliases and other instances. Canceling, decoding errors, changed files and changed document sources preserve existing work. Static noninterlaced RGB/RGBA8/16 PNG opening checks cancellation between native rows and through checked reads, including final PNG completion. Other image layouts and codecs retain their existing before/after decoder checks. Opening through the File menu creates another tab; `document.open` retains its explicit discard guard when replacing an existing document. Import continues to add layers to the chosen project.

## JPEG XL boundary (unreleased)

JXL opening, import, file paste and placement use the same supported subset. Both raw codestream and standard container signatures are accepted; renaming another format to .jxl rejects. Header inspection validates dimensions, depth and the supported RGB profile; it does not establish that the complete image stream is valid. Decode must finish successfully before any project change. If color and alpha depths differ, the output uses their maximum supported 8/16-bit depth. Original RGB profiles are validated before decoding. When the default output ICC exactly matches Original, samples and ICC remain untouched. Otherwise the decoder explicitly produces SDR sRGB and returns its actual output ICC; the existing native-depth import conversion uses that profile, retaining alpha. Unsupported profiles reject.

Native JXL execution runs in one source-matched child of the application, with bounded binary input/output, a two-minute total queue/operation deadline and no file publication authority. Windows uses a 1536 MiB child commit limit and kill-on-close job; Linux uses an address-space limit. macOS retains its inherited address-space policy, applies child CPU/core limits and samples physical footprint against that threshold; this is not a hard resident-memory ceiling. Darwin RSS aliases virtual address space, so the physical threshold is not applied as an address-space cap. A codec crash, failed allocation, timeout, truncated reply or invalid result discards the operation. This protects the application from codec faults; it is not an OS security sandbox or a total application-memory guarantee. Existing file-size, raster, profile and import-coexistence limits also apply.

Open/import cancellation is checked around the bounded decoder operation; those callers do not yet interrupt an in-flight decode. Export cancellation can terminate its child between control polls. The shared child transport also checks cancellation before sending the body, when accepting a validated result and after joining both pipes before returning completed output. In either case source/revision checks still precede shared edits or atomic publication. No partial image is returned as editable work. Source review and prepared fixtures are separate from combined compilation, cross-platform codec and native workspace acceptance, which remain pending.

## Native PNG row loading (unreleased, verification pending)

Static, noninterlaced RGB/RGBA PNGs with normal orientation decode directly into native sparse tiles. The shared header, color/profile, explicit-content and import-budget checks still run first. RGB transparency keys expand to native alpha. Supported ICC conversion acquires the existing native-depth CMS once, retains exact alpha and does not use a display preview. One native RGBA row and a fixed 64 KiB input buffer replace the full decoded image/RGBA transfer copy; codec state, CMS tables/output, profiles, encoded input and final native tiles remain separate allocations. The current encoded, decoded-coexistence, dimension, batch and document limits do not increase.

Open supplies its existing cancellation control to the shared decoder and reports row progress. Native row decoding, IDAT completion and final IEND/chunk validation must all succeed before any source replacement. This also refuses a corrupt final chunk after otherwise valid pixel data. Errors or cancellation never retry through the full-image decoder. Palette, grayscale and interlaced PNGs and sources with nonidentity orientation use their selected layout paths. APNG still requires an explicit frame. Source review and prepared fixtures precede combined codec, engine, protocol and native workspace qualification.


## Native source-backed QOI (unreleased, verification pending)

The native command worker now streams QOI files into source-backed tiles for
import, placement and image patches. It retains RGB/RGBA samples, including
transparent RGB, and uses the shared engine for destination precision, clipping,
selection, placement and undo. Its source is read through the same held-file
identity and consumed-byte hash checks as native PNG. A fixed 64 KiB input buffer,
one admitted output row and a fixed pixel index replace complete encoded/decoded
frame allocations. Invalid headers, truncated pixels, overlong runs, missing end
markers and trailing data refuse without adopting partial work. Existing import
and canvas limits still apply. Regression source and compile-only verification
are separate from runtime/native acceptance; live native authority remains
inactive and other codec/animation producers remain unfinished.


## Native source-backed Netpbm (unreleased, verification pending)

The native worker streams PBM, PGM, PPM and PAM files (P1-P7) for import,
placement and image patches. The installed decoder retains its bit packing,
channel/alpha interpretation and MAXVAL normalization. Original 16-bit samples
remain native16, including transparent RGB; import/placement can promote the
destination, while patches refuse a native16 source in an 8-bit document.

Held-file identity and full consumed-byte hashes protect both decoding passes.
A 64 KiB header limit, charged parser/buffer allowance and two sample rows bound
decoder memory. Header overflow, malformed/truncated data, read errors and
cancellation refuse before publication. As with the existing decoder, only the
first image is imported; remaining file bytes are hashed but not interpreted.
Existing import dimensions and budgets still apply. Regression source and
compile-only checks do not qualify runtime behavior or activate native authority.


## Native source-backed TGA (unreleased, verification pending)

The native command worker streams basic raw and RLE TGA files for Open, Import,
placement, file paste and patches. It supports the installed decoder's palette, packed
15/16-bit color, RGB/RGBA and grayscale/alpha interpretations. Packed 15/16-bit
color contains five-bit primaries; decoded channels remain native8 and promote
through shared commands when needed. All four image origins are normalized.

Palette and row buffers are charged before allocation. RLE packets can cross
rows without a whole-frame buffer. Bottom-up images write each tile stripe
directly to its final position. Invalid palette indices,
truncation, packets exceeding the declared image and interleaved layouts refuse.
Remaining file bytes, including optional footer metadata, are hash-verified but
not interpreted, matching the installed decoder's basic sample interpretation.
Native imports use the TGA file limits above and the native memory and disk
budgets. Resident import limits still apply to resident destinations.


## Native source-backed BMP (unreleased, verification pending)

Native import, placement and patches now stream basic BMP files through
bounded DIB/mask/palette metadata and row buffers. Supported core/info/V2-V5
headers, palette samples and bitfield expansion reuse the installed decoder.
RLE4/RLE8 use bounded instruction storage, preserve sparse deltas and early-end
black pixels, and reject runs or deltas outside the image. Top-down and bottom-up
files write each tile stripe directly to its final position under the native
file and store budgets.

Packed 16/32-bit pixel layouts do not imply 16-bit channels. BMP bitfields wider
than eight bits per channel refuse before the installed decoder can truncate
them. This shared guard also covers resident import/placement, image patches,
selected ICO bitmap variants and Windows DIB clipboard input. Calibrated or
profiled BMP/DIB encodings require explicit supported RGB conversion. Basic
BMP import does not preserve arbitrary ancillary metadata.

Held-file identity and full consumed-byte hashes protect native admission.
Failures retire staged sources before publication. Regression source and
compile-only checks are separate from pending runtime/native acceptance and
live native activation.

## Native source-backed SVG/SVGZ (unreleased, verification pending)

Native import, placement and file paste now use the existing bounded SVG row
renderer. Import retains the original markup and editable affine definition;
placement and paste keep their rasterized behavior. SVG coverage and colors
render as RGBA8 and promote explicitly in native16 projects, preserving existing
native16 artwork. Supported SVG structures and current import limits are unchanged.

The held file is hash-bound across admission and decoding. Markup is capped at
512 KiB, compressed input at 1 MiB, and parser/renderer allocations are admitted
before use. Static SVG rejects frame/page/variant selectors. Malformed gzip,
unsupported SVG content, cancellation and budget refusal leave no partial edit.
All native file producers now require absolute local image paths, matching
resident image import. Regression source covers precision, source retention,
undo, invalid input and staged-source retirement; runtime/native acceptance and
live authority activation remain pending.


Unreleased Windows clipboard follow-through: registered PNG remains preferred
so PNG16 retains native samples. Without PNG, PeerBrush reads the first
enumerated DIB/DIBV5 representation and validates it before decoding; unsupported
precision/color data does not fall through to a generic converted image. Windows
enumerates an original format before synthesized alternatives, as documented in
[Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats).
Format selection and byte capture share one clipboard hold, released before
decoding. Explicit bitmap pixel offsets preserve the start of V4/V5 pixel data.
Native receiving-application acceptance remains pending.


## Native source-backed ICO (unreleased, verification pending)

Native import, placement and file paste accept an explicit `variant` when an
icon contains multiple images. Embedded PNG preserves native 8/16-bit channels
and uses the shared color/orientation pipeline. BMP variants retain palette,
bitfield, RLE and alpha samples, then apply the original AND transparency mask.
The installed decoder's bitmap orientation and RLE transparency behavior remain
unchanged. Unsupported BMP precision and color encodings still refuse.

Patches select the same highest-quality variant as the resident decoder and
keep raw source channels. PNG-based ICO patches retain the existing RGBA8
restriction; import, placement and paste preserve PNG16 instead.

Decoding uses held-file identity and whole-container hashes, a bounded directory,
row buffers and private native tiles. Embedded PNG completion additionally binds
the exact selected payload. No complete encoded or decoded icon frame is copied
into resident memory. Partial masks, invalid ranges, cancellation and admission
failures retire provisional sources before adoption. Current import limits remain
in force. Regression source is separate from runtime/native acceptance; live
native authority activation remains pending.


Unreleased TIFF color correction: import, placement, file paste and image
opening validate the selected page's original ICC declaration before decoding
pixels. Missing profiles remain untagged; malformed tag types, out-of-file
profile ranges, duplicate profiles and profiles exceeding 4 MiB are rejected.
An error reading a declared profile can no longer silently import the image
as untagged RGB. Other pages retain their own independent color declarations.
Classic TIFF and BigTIFF share bounded, cancellable directory inspection in
both byte orders. Native16 samples and orientation still use the shared color
pipeline. Native TIFF staging is described below; runtime/release acceptance remains pending.


## Native source-backed TIFF (unreleased, verification pending)

Native import, placement and file paste now decode the explicitly selected TIFF
page through a session-owned encoded snapshot. Random reads authenticate original
container chunks; selecting a page changes only the decoder's first-IFD view.
The user-selected source stays held and is rechecked before completing preparation.

The native path supports uncompressed, LZW, Deflate, PackBits and Group 4 coding
units, strip/tile storage, separate planes, both byte orders and classic/BigTIFF.
Supported samples are bilevel grayscale and integer 8/16-bit grayscale, grayscale
with alpha, RGB and RGBA. Original channel precision, selected-page ICC conversion
and orientation pass through the shared native engine. Extra sample layouts and
unsupported channel/color encodings require explicit conversion.

Patches use the first page's raw channels and ignore orientation/ICC, matching the
resident patch route. Nonplanar CMYK8/16 patches retain its RGB projection; CMYK
import/placement/paste still require explicit conversion. JPEG-compressed TIFF
needs separate decoder allocation admission and remains unavailable on this native
path. Decoded coding units and tile-row bands are limited to 64 MiB each; encoded
units are limited to 16 MiB. Larger coding units require an incremental decoder
before these limits can be lifted. Existing image/import limits also apply.

Provisional image sources and completed encoded snapshots retire on ordinary
refusal, cancellation and completion. Interrupted byte-spool sealing retains its
writer until cleanup verifies the exact header, payload and successfully written
index/seal bytes. Verified cancellation releases staging inventory; unknown
partial writes, changed contents or foreign identities retain quota and report
retirement refusal. Unverified bytes are never adopted or silently deleted. Regression source covers native/resident samples,
page selection, undo, cancellation and retirement. Runtime acceptance and live
native authority activation remain pending.

## Selected PNG animation frames (unreleased, verification pending)

APNG import, placement and paste now share incremental PNG row expansion with
the native staging path. Base64 placement/paste accepts the same explicit
zero-based `frame` choice. Even a one-frame APNG requires a choice; a separate
default image is decoded and validated but excluded from animation composition.
Adam7 passes, palette transparency, grayscale/RGB channels, frame offsets and
Source/Over blending are supported. Background disposal clears the frame region;
Previous disposal retains the canvas from before that frame. Resident imports
also use this corrected behavior, so a Previous frame cannot resurrect pixels
cleared by a preceding Background frame. ICC conversion and EXIF orientation
apply to the composed selected frame.

The shared container scanner validates every original chunk CRC, animation
sequence, frame count and frame bounds before native staging. File sources retain
their whole-file hash proof; base64 sources retain their admitted encoded owner.
Decoder admission is 32 MiB with an 8 MiB internal decoder limit. Native PNG
files, including APNG, can reach 16000 pixels per edge and 3 GiB of encoded
input. Native APNG file work is capped at 128 million decoded/composited pixels
or five times the canvas area, whichever is greater. Frame composition uses
tile-sized windows and private source stores with explicit retirement on
cancellation or failure. Earlier frames with Previous or Background disposal
are validated without staging their pixels. Background still clears the
required part of the accumulated canvas. Inline animation retains its existing
transport and work limits. The resident canvas retains its 64 MiB allocation guard.

Both static PNG16 and selected APNG16 frames retain 16-bit samples. APNG16
composites Source and Over at native precision. Raw image patches
continue to use the default IDAT image with their existing unmanaged color
policy. Regression source covers both document depths, interlacing, disposal,
color/orientation, frame choices, base64, undo, late decoding errors and cleanup.
Tests, live native authority activation and runtime/release acceptance are pending.

## JPEG profile admission (unreleased, verification pending)

JPEG imports validate the original marker stream before constructing the image
decoder. Declared ICC fragments must have one consistent count, unique nonzero
sequence numbers and every fragment present. The original bytes are assembled
in sequence order, including a complete 255-fragment profile. A missing, empty,
duplicate, oversized or late fragment is an error; it cannot become untagged
sRGB because the underlying decoder reports no usable profile.

The inspector follows entropy byte stuffing, restart markers and successive
scans without allocating segment payloads. It bounds JPEG metadata to 8 MiB,
ICC data to 4 MiB, structural markers to 65536 and scans to 100, while retaining
the existing image/file budgets. A complete end marker is required. Supported
imports remain 8-bit RGB/grayscale; CMYK and other channel precision require
explicit conversion. Raw image patch behavior is separate and unchanged.

Regression source covers profile conversion against identical PNG samples,
fragment ordering and rejection, truncated input, cancellation, and unchanged
import/place/paste batches on failure. The native JPEG path is described below; compilation does not establish runtime
acceptance.

## Native JPEG sources (unreleased, verification pending)

Native JPEG import, placement, paste and image patches now use authenticated
session-owned encoded snapshots. The pinned zune-jpeg 0.5.15 decoder reads that
seekable spool directly, avoiding the image wrapper's full encoded-file copy.
Original ICC fragments feed shared native color conversion; orientation applies
before conversion. Native8 samples promote explicitly when inserted into a
native16 destination without narrowing existing document samples. Raw patches
retain RGB/grayscale and CMYK projection with their unmanaged color/orientation
policy. Resident and native patches validate the same JPEG marker structure.

The decoder has a 64 MiB admitted scratch envelope for bounded metadata, tables,
peek buffers and MCU/upsampling rows. Progressive or separate-component scans
also reserve their complete coefficient planes before decoder construction.
Decoded RGB and grayscale output above 64 MiB uses the pinned decoder's row
sink and a single decoded scratch row. This permits single-scan baseline JPEGs
up to 16000 pixels per edge, with a 3 GiB encoded-file limit and the configured
resident and disk budgets. The native picker applies this per-file limit to JPEG.
Progressive and separate-scan input still needs its coefficient planes,
each limited to 64 MiB. Smaller images retain the contiguous output path.

Decoded output enters the shared native row writer. Files without an embedded
profile or orientation transform use those staged tiles directly. Temporary
sources and encoded snapshots retire before adoption. Decoder and cancellation
errors retain their original cause. Uncertain retirement stays inventoried.
Regression source compares both destination depths, progressive/separate scans,
profile/orientation, patches and undo with resident behavior and covers allocation
refusal, malformed input, cancellation and late reservations. Runtime tests and
platform acceptance remain pending.

## WebP decoder admission preparation (unreleased)

The pinned image-webp 0.2.4 dependency now has opt-in lossless Huffman and lossy partition allocation limits. Partition admission includes unknown-length input growth, temporary packed copies and earlier live partitions. Original upstream licenses and a patch description accompany the vendored source.

Shared WebP decoding rejects embedded VP8 dimensions that differ from the enclosing canvas/frame before allocating YUV planes. Animated ALPH data also checks the following VP8 type and padded ANMF boundary before writing pixels. These checks apply to existing image-crate consumers.

The new allocation options remain disabled by default. They do not cover decoded frame/canvas buffers, transforms, macroblocks or metadata; native WebP production still requires separate admission and integration. Added regression source has not been executed during development.

## Native WebP producer (unreleased)

Native import, placement, file paste and raw patches now use the pinned decoder over an authenticated, session-owned source snapshot. Static RGB/RGBA, lossy/lossless, ALPH transparency and explicitly selected composed animation frames feed canonical native rows. Managed commands preserve original ICC conversion and EXIF orientation; patches use the first composed frame without that conversion, matching the resident patch policy. Destination 16-bit channels are preserved.

Original container inspection bounds chunk/frame counts, padded ranges, embedded dimensions and metadata before decoding. Managed imports refuse missing, duplicate, empty, oversized or inconsistent declared ICC data. Both resident and native paths use this inspection. Native preparation reserves each full-frame/canvas/alpha allocation, transform metadata, padded YUV planes and macroblock growth, then enables the separate 32 MiB Huffman and 32 MiB partition guards. Metadata and output allocations have their own reservations. A failure retires verified temporary sources without adopting an edit.

The shared decoder now traverses metadata between animation frames and clears the previous RGBA rectangle for disposal, including when the incoming frame is opaque or covers the entire canvas. With no background override, disposal clears to transparent; the container background remains a hint.

Native Open, Import, placement, file paste and image patches accept static WebP
up to 16000 pixels per edge. Lossy VP8 streams rows into native tiles with
or without alpha. An uncompressed ALPH plane reads directly from the held
source. Compressed alpha decodes into an authenticated temporary plane that
retires before adoption. Both use two scratch rows for alpha prediction.
Static lossy WebP with a separate alpha plane permits up to 512 MiB of
encoded input per file. Native static lossless VP8L and animated WebP use a 3 GiB
encoded limit. Native WebP export uses the same ceiling. Static lossless output
above 64 MiB uses bounded pixel history and stages inverse-transformed RGBA rows.
Opaque static VP8 and resident decoding retain the 128 MiB encoded-file limit. The VP8 partition guard and
native store budgets still apply. Native animation canvases also reach 16000
pixels per edge. Animated VP8, VP8L and ALPH payloads decode row by row into
native tiles. Frame composition uses bounded tile windows. Codec-specific
partition, table and scratch limits still apply.
Animation work and explicit frame selection remain bounded. Image Info reads held
file metadata. Runtime tests and full-size platform acceptance remain pending.

Above 128 MiB, native Open and Import inspect the held WebP headers before
hashing or copying the payload. The frame picker parses metadata while hashing
the same held bytes, without a temporary chooser copy. Decode validates its
authenticated copy before adopting any tiles.

## Native JPEG XL row import (unreleased)

Native Open, Import, placement, file paste and image patches send a held, authenticated source snapshot to the isolated JPEG XL worker. The decoder retains native 8-bit or 16-bit RGBA samples. Managed imports apply codestream orientation and convert from the decoded output profile. Raw patches on resident and native documents use source orientation and the existing unmanaged patch rules. A 16-bit patch requires a 16-bit destination. Static JPEG XL retains its existing refusal of frame/page/variant selectors and unsupported HDR, animation, grayscale and extra-channel layouts.

Inspection and decoding use the same immutable source. The decode request includes the inspected dimensions and precision, which both child and parent validate. The parent stages bounded rows directly into native tiles. Decoded output above 64 MiB uses bounded spans and an authenticated spool. The parent verifies complete, non-overlapping coverage before it adopts the result. Full child memory allowances are reserved before launch, and the supervisor holds ownership until confirmed exit. An uncertain stop retains registered files for verified recovery.

Native input supports files up to 3 GiB in 64 KiB chunks and dimensions up to 16000 pixels per edge, subject to the existing worker, resident and disk budgets. Resident decoding retains its existing geometry and import limits. Runtime tests and full-size platform acceptance remain pending.

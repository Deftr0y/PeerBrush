# Image import

PeerBrush supports the following raster and vector imports; unreleased additions are labelled below. Use File → Import, drag files into the workspace, or issue `image.import` through the shared document engine. A batch is one undo step. Import never replaces an existing project.

| Format | Import behavior |
| --- | --- |
| PNG | Native 8/16-bit gray, RGB and alpha; palette images become RGBA8. APNG requires an explicit frame. PNG16 animation is rejected because the animation interface cannot preserve its precision. |
| JPEG | 8-bit RGB/gray decoded to RGBA8; EXIF orientation is applied. Higher-precision and four-channel CMYK/YCCK source headers are rejected before decoding. Lossy source data cannot recover detail lost during encoding. |
| JPEG XL / JXL (unreleased PB-143) | Static SDR RGB with optional straight alpha at native 8/16-bit depth, including lossless and lossy files. Orientation is applied. Animation, grayscale/CMYK, floating-point/HDR, associated alpha and extra channels reject explicitly. |
| BMP | Basic decoded bitmap/palette images become RGBA8. |
| GIF | Palette images become RGBA8. Multiple frames require an explicit composited frame. |
| TIFF / TIF | Native 8/16-bit gray, RGB and alpha. Multiple pages require an explicit page; selecting a page never changes the source file. Floating-point, signed or unsupported color encodings are rejected. |
| WebP | Static and animated RGBA8. Multiple frames require an explicit composited frame. |
| ICO | Choose an explicit icon variant when multiple images are present. Cursor files are unsupported. |
| PNM / PBM / PGM / PPM / PAM | Basic Netpbm images, including native 16-bit samples where present. |
| TGA | Basic decoded RGB/gray images become RGBA8. |
| QOI | Decoded RGB/RGBA8. |
| SVG / SVGZ | Supported self-contained static vector artwork retains its original markup and editable affine placement. Its colors and coverage render through RGBA8, promoted when the project is native16. |

The import dialog numbers choices from one. MCP/CLI commands use zero-based `frame`, `page` or `variant`. `peerbrush_image_info` returns the choice type and count without editing the document. Dimensions and depth describe the initial decoder header; another TIFF page or icon variant can have different dimensions or depth. Import checks the selected image again and promotes an 8-bit project when the selected source contains native 16-bit samples.

```json
{"op":"image.import","path":"/absolute/artwork.tiff","page":1,"x":40,"y":20,"parent":"folder-id"}
```

`peerbrush_place_image` accepts the same formats and choices, or base64 PNG. Placing native16 pixels into an existing layer promotes the project before resampling or compositing. On Windows, Explorer file paste supports the static formats above. Placement and external image-file paste rasterize SVG; use Import to retain an editable vector source. Files requiring a frame/page/variant choice report that requirement; use Import to make the choice. PeerBrush's internal whole-layer clipboard retains native samples and editable sources.

Supported embedded RGB matrix/TRC and classic LUT ICC profiles are converted to sRGB at the source channel depth, preserving alpha. Unsupported ICC profiles, CMYK and floating-point sources require an explicit supported RGB conversion before import. Untagged images are interpreted as sRGB. Imported working RGB carries a standard sRGB profile on PSD/PNG save. HDR/non-sRGB PNG transfer encodings are rejected. This is a bounded raster import path, not preservation of an animation timeline, TIFF editing metadata or Photoshop structures. Open PSD files as projects.

SVG supports paths with curves/arcs, basic shapes, groups, solid colors, linear/radial gradients, local `use`/symbols and clip paths. Outline text before import. Raster images, filters, masks, patterns, markers, stylesheets, scripts/animation, external resources and unknown visual properties are rejected explicitly. Inline styles accept the supported presentation properties. The viewport must be explicit through dimensions or `viewBox`; percentage dimensions require a `viewBox`. Metadata cannot become artwork through a local reference. The source editor previews supported markup through the shared engine; Apply commits one guarded change and Cancel preserves the document.

SVG's RGBA8 projection is an explicit renderer limit. Existing native16 raster channels are never quantized. Editable PSD saves retain the vector definition alongside current standard raster/mask data and a merged composite, using PeerBrush source format 12. Photoshop sees the rendered raster layer. Older PeerBrush readers open the saved composite read-only. At transparent edges, reconstructing straight color from Photoshop's white-matted merged pixels can differ from the original; the tested SVG fixture differed by at most one native16 unit. Alpha and the saved matted appearance remained exact.

PDF, Illustrator AI and EPS remain unsupported after evaluating their page, font, color and source-preservation requirements. Export supported raster artwork or SVG from the source application. Do not rename an unsupported file to a supported extension.

Imports limit files to 128 MiB encoded, decoded images to a 256 MiB batch budget, dimensions to 8192 pixels and 32 million pixels, and the document to its existing raster/layer budget. Preview and commit enforce the same cumulative batch budget. The native picker accepts at most 16 files per batch. Animation decoding also has a bounded frame/work limit. Invalid files, excessive resources, locks, reservations and stale or closed source projects leave the batch unapplied. Canceling the choice dialog changes no history.

SVG additionally limits markup to 512 KiB, compressed SVGZ to 1 MiB, the source frame to 4 million pixels and XML to 4096 nodes. Reference expansion, nesting and geometry are bounded. Projection checks transformed isolation boxes and conservative nested clip buffers against a 128 MiB working-memory budget and a rendering-work budget before allocating surfaces. DTDs, processing instructions and external resource resolution are disabled.

## Opening images as projects (unreleased PB-120)

Open uses the same bounded decoders and color handling as Import. PNG, JPEG and the other supported image formats create a new project at the selected content’s actual dimensions and native 8/16-bit depth. Alpha stays intact. SVG/SVGZ retains its supported editable vector source. Files with multiple frames, pages or icon variants require an explicit choice; the native Open dialog offers that choice, while `projects.open` and `document.open` accept zero-based `frame`, `page` or `variant`. Use `image_info` to inspect the available choices first.

Image projects start unsaved and have no PSD save destination. Save/Save As writes a new working PSD or PSB. The original image remains owned by its project until Save As or close: duplicate opens and exports over an open source are rejected, including aliases and other instances. Canceling, decoding errors, changed files and changed document sources preserve existing work. Native image decoding checks cancellation before and after each bounded decoder operation; it does not interrupt a codec halfway through decoding. Opening through the File menu creates another tab; `document.open` retains its explicit discard guard when replacing an existing document. Import continues to add layers to the chosen project.

## JPEG XL boundary (unreleased)

JXL opening, import, file paste and placement use the same supported subset. Both raw codestream and standard container signatures are accepted; renaming another format to .jxl rejects. Header inspection validates dimensions, depth and the supported RGB profile; it does not establish that the complete image stream is valid. Decode must finish successfully before any project change. If color and alpha depths differ, the output uses their maximum supported 8/16-bit depth. Original RGB profiles are validated before decoding. When the default output ICC exactly matches Original, samples and ICC remain untouched. Otherwise the decoder explicitly produces SDR sRGB and returns its actual output ICC; the existing native-depth import conversion uses that profile, retaining alpha. Unsupported profiles reject.

Native JXL execution runs in one source-matched child of the application, with bounded binary input/output, a two-minute total queue/operation deadline and no file publication authority. Windows uses a 1536 MiB child commit limit and kill-on-close job; Linux uses an address-space limit. macOS retains its inherited address-space policy, applies child CPU/core limits and samples physical footprint against that threshold; this is not a hard resident-memory ceiling. Darwin RSS aliases virtual address space, so the physical threshold is not applied as an address-space cap. A codec crash, failed allocation, timeout, truncated reply or invalid result discards the operation. This protects the application from codec faults; it is not an OS security sandbox or a total application-memory guarantee. Existing file-size, raster, profile and import-coexistence limits also apply.

Open/import cancellation is checked around the bounded decoder operation; those callers do not yet interrupt an in-flight decode. Export cancellation can terminate its child between control polls. In either case source/revision checks still precede shared edits or atomic publication. No partial image is returned as editable work. Source review and prepared fixtures are separate from combined compilation, cross-platform codec and native workspace acceptance, which remain pending.

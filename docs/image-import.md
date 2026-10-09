# Image import

Development builds support the following raster imports. Use File → Import, drag files into the workspace, or issue `image.import` through the shared document engine. A batch is one undo step. Import never replaces an existing project.

| Format | Import behavior |
| --- | --- |
| PNG | Native 8/16-bit gray, RGB and alpha; palette images become RGBA8. APNG requires an explicit frame. PNG16 animation is rejected because the animation interface cannot preserve its precision. |
| JPEG | 8-bit RGB/gray decoded to RGBA8; EXIF orientation is applied. Higher-precision and four-channel CMYK/YCCK source headers are rejected before decoding. Lossy source data cannot recover detail lost during encoding. |
| BMP | Basic decoded bitmap/palette images become RGBA8. |
| GIF | Palette images become RGBA8. Multiple frames require an explicit composited frame. |
| TIFF / TIF | Native 8/16-bit gray, RGB and alpha. Multiple pages require an explicit page; selecting a page never changes the source file. Floating-point, signed or unsupported color encodings are rejected. |
| WebP | Static and animated RGBA8. Multiple frames require an explicit composited frame. |
| ICO | Choose an explicit icon variant when multiple images are present. Cursor files are unsupported. |
| PNM / PBM / PGM / PPM / PAM | Basic Netpbm images, including native 16-bit samples where present. |
| TGA | Basic decoded RGB/gray images become RGBA8. |
| QOI | Decoded RGB/RGBA8. |

The import dialog numbers choices from one. MCP/CLI commands use zero-based `frame`, `page` or `variant`. `peerbrush_image_info` returns the choice type and count without editing the document. Dimensions and depth describe the initial decoder header; another TIFF page or icon variant can have different dimensions or depth. Import checks the selected image again and promotes an 8-bit project when the selected source contains native 16-bit samples.

```json
{"op":"image.import","path":"/absolute/artwork.tiff","page":1,"x":40,"y":20,"parent":"folder-id"}
```

`peerbrush_place_image` accepts the same raster formats and choices, or base64 PNG. Placing native16 pixels into an existing layer promotes the project before resampling or compositing. On Windows, Explorer file paste supports the static raster formats above. Files requiring a frame/page/variant choice report that requirement; use Import to make the choice. PeerBrush's internal clipboard retains native samples.

Supported embedded RGB matrix/TRC ICC profiles are converted to sRGB at the source channel depth, preserving alpha. Unsupported ICC profiles, CMYK and floating-point sources require an explicit supported RGB conversion before import. Untagged images are interpreted as sRGB. Imported working RGB carries a standard sRGB profile on PSD/PNG save. HDR/non-sRGB PNG transfer encodings are rejected. This is a bounded raster import path, not preservation of an animation timeline, TIFF editing metadata or Photoshop structures. Open PSD files as projects.

SVG import is the next PB-091 checkpoint. PDF, Illustrator AI and EPS import remain unsupported; export supported raster artwork or SVG from the source application. Do not rename an unsupported file to a supported extension.

Imports limit files to 128 MiB encoded, decoded images to a 256 MiB batch budget, dimensions to 8192 pixels and 32 million pixels, and the document to its existing raster/layer budget. Preview and commit enforce the same cumulative batch budget. The native picker accepts at most 16 files per batch. Animation decoding also has a bounded frame/work limit. Invalid files, excessive resources, locks, reservations and stale or closed source projects leave the batch unapplied. Canceling the choice dialog changes no history.

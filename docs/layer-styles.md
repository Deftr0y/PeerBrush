# Layer-style effects (unreleased)

PeerBrush adds Bevel & Emboss, Contour, Texture, Stroke, Inner Shadow, Inner Glow, Satin, Color Overlay, Gradient Overlay, Pattern Overlay, Outer Glow and Drop Shadow to the regular Color effect stack. Find them by name or under Relief, Outlines, Shadows and Overlays. Top effects run last. Each step has the existing independent strength, bypass, reorder and undo controls; adjusting settings previews actual rendered pixels through the shared engine.

These are native PeerBrush style effects with bounded settings. They do not enable Photoshop layer-style descriptor or embedded-pattern import/export. A Photoshop document containing unsupported styles, patterns, global masks or profiles stays protected and displays its saved composite. See [Photoshop fidelity](photoshop-fidelity.md).

Unreleased source diagnostics now inspect bounded `lfx2`/`lmfx` descriptors and identify recognized single/multiple style definitions and enabled/disabled states. This is view-only inspection; imported Photoshop styles and embedded patterns remain protected. See [descriptor limits](photoshop-fidelity.md).

| Effect | Available settings |
| --- | --- |
| Bevel & Emboss | Inner/Outer/Emboss/Pillow, size, soften, depth, light angle/altitude, Up/Down, highlight/shadow colors, contour, opacity and blend |
| Contour | Linear/Cone/Ring/Smooth profile, range, invert, opacity and blend; maps the incoming relief/tone |
| Texture | Checker/Stripes/Dots/Noise procedural pattern, pixel scale, lighting angle, depth, invert, two colors, opacity and blend |
| Stroke | Circular outline size, Inside/Outside/Center, color, opacity and blend |
| Inner Shadow | Size, distance, angle, choke, contour, color, opacity and blend |
| Inner Glow | Edge/Center source, size, spread, contour, color, opacity and blend |
| Satin | Size, distance, angle, contour, invert, color, opacity and blend |
| Color Overlay | Native-normalized RGB color, opacity and blend |
| Gradient Overlay | Linear/Radial/Angle/Reflected/Diamond, two colors, angle, scale, reverse, opacity and blend |
| Pattern Overlay | Embedded bitmap PNG with native depth, scale, clockwise angle, document phase and nearest/bilinear sampling; or Checker/Stripes/Dots/Noise with pixel scale, invert and two colors; opacity and blend |
| Outer Glow | Size, spread, contour, color and opacity |
| Drop Shadow | Size, distance, angle, spread, contour, color and opacity |

Overlay and inner styles preserve input alpha. Exterior styles can grow into transparent canvas space without replacing or expanding editable source pixels. Earlier steps run in their original source frame; the first active style expands its derived input into document coordinates. Gradient and pattern coordinates use the canvas frame, independent of screen pan/zoom. Output is bounded by the canvas; off-canvas source pixels are retained but exterior styling does not render beyond this frame. Sizes/softening are limited to 64 pixels and shadow/Satin distances to 128 pixels. Large derived/temporary images are rejected explicitly under the existing memory budgets. These Color styles apply to regular layers and isolated folders; adjustment layers, whole-image filters and mask stacks do not offer them.

Drop Shadow and Outer Glow composite beneath the incoming layer image. Their interaction with other layers follows the layer's blend mode; an independent Photoshop-style backdrop blend is not implemented. Relief uses alpha-field shading, not Photoshop's bevel techniques or global-light sharing. Contour operates on incoming luminance as an independent step. Texture and Pattern Overlay accept embedded PNG tiles alongside procedural patterns; bitmap Texture awaits combined sprint QA. Gradients support two-color defaults plus bounded custom color/opacity stops; the custom-stop implementation awaits combined sprint QA. Photoshop gradient libraries, jitter and proprietary style presets are not imported. These limits are intentional and no bit-for-bit Photoshop equivalence is claimed.

UI, MCP and CLI share `effect.add/update/delete/reorder` with explicit layer IDs, source revisions, locks and reservations. Settings use native-normalized RGB arrays (`[r,g,b]`, each 0–1); opacity/spread are 0–1. The tool catalog exposes stable effect keys. An example:

```json
{"op":"effect.add","layer":"LAYER_ID","kind":"drop_shadow","settings":{"color":[0,0,0],"opacity":0.6,"size":8,"distance":12,"angle":120,"spread":0.1,"contour":"smooth"}}
```

Effects keep native 8/16-bit source channels and masks. PSD/PSB saves include the current standard rendered raster/mask data and merged composite; PeerBrush source format 16 retains the editable style stack and original source samples; bitmap pattern settings use format 18. Other applications see the baked appearance, and older PeerBrush readers protect newer style sources rather than silently discarding them. Proportional image resizing scales spatial style settings; unsupported non-proportional style resizing rejects atomically.

## Embedded bitmap patterns

Select Pattern Overlay and choose **Choose bitmap PNG…**. A static PNG up to 512 × 512 pixels and 4 MiB becomes part of the editable effect; the project stores image bytes, never a local path. The shared importer normalizes supported embedded RGB ICC to sRGB at the original channel depth. Unsupported profiles and animation reject explicitly. A retained PNG16 pattern, including a disabled effect, promotes the document and its existing raster/mask channels to 16 bit. Replace it with an explicitly converted PNG8 before lowering document precision.

Scale is 1–800% (100% uses original tile pixels); angle is clockwise −180–180°. Phase X/Y offsets the tile origin in document pixels, within −32768–32768. Tiling wraps both axes. Nearest sampling retains pixel edges; bilinear sampling interpolates premultiplied alpha across wrapped edges. Tile alpha controls overlay coverage while original layer alpha stays unchanged. Pattern coordinates use document pixel centers and the canvas origin, independent of layer movement and screen pan/zoom. Proportional image resize scales the tile and phase; crop/canvas origin changes keep the image aligned. Unsupported non-proportional resize rejects atomically.

For MCP/CLI, set `pattern:"bitmap"` and `bitmap_png` to base64 PNG bytes in ordinary `effect.add/update` settings. Optional keys are `bitmap_scale` (0.01–8, default 1), `bitmap_angle`, `phase_x`, `phase_y` (default 0), and `sampling:"bilinear"` or `"nearest"`. Procedural-only color/color2/scale/invert overrides reject for a bitmap source. Opacity, blend and effect strength remain shared controls. Selecting a procedural pattern in the UI removes the embedded bitmap settings in one undoable edit.

```json
{"op":"effect.add","layer":"LAYER_ID","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":"BASE64_PNG_BYTES","bitmap_scale":1,"bitmap_angle":0,"phase_x":0,"phase_y":0,"sampling":"bilinear","opacity":1}}
```

Source validation includes disabled effects, a 32 MiB aggregate bitmap-source budget and existing document/effect work limits. Decoded tiles use a separate bounded 16 MiB derived cache; neither decoded caches nor previews enter history or PSD sources. PNG picker receipts require the captured project, document, revision, layer and effect; source changes, selection changes, human input, modal/file work or cancellation discard them. Actual parameter gestures preview through the engine and commit once on release.

PSD/PSB source format 18 retains embedded PNGs and native original pixels alongside current standard rendered raster/mask channels and the merged composite. The save compatibility report still requires explicit acceptance of standard editability losses. This feature does not import Photoshop `Patt` resources, implement bitmap Texture, or reproduce Photoshop proprietary layer-style semantics. Protected Photoshop originals remain protected.

## Protected Photoshop pattern inspection (unreleased)

The read-only protection report offers **Pattern tiles** for bounded RGB `Patt`/`Pat2`/`Pat3` assets. It displays actual raw tile pixels, names, asset bounds and original 8/16-bit precision. These are unmanaged source samples: Photoshop color handling and layer-style placement are not interpreted. Opening, inspecting and previewing never enables editing or changes the saved composite, source file or history.

The reader accepts version-1 RGB patterns with version-3 virtual memory arrays, matching channel bounds and native depth, and raw or PackBits RGB storage. Written transparency/mask/extra channels, other color modes, ZIP variants, unknown versions or trailers and malformed data remain protected. Each block is bounded to 8 MiB, 16 assets and 512 × 512 per tile; decoded assets across a document are limited to 16 MiB of actual tiled storage. Metadata and decoded assets are runtime inspection data, excluded from editable document serialization and saves.

The structure follows the [Adobe pattern specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/). Compression mode 1 is checked as PackBits with 16-bit row counts, as in the [primary psd-tools pattern reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/patterns.py), and against actual channel data. This inspection does not recover editable Photoshop styles, establish matching Photoshop color or placement, or establish faithful Photoshop Texture.

## Bitmap Texture (unreleased; sprint QA pending)

Texture also offers **Choose bitmap PNG…**, with the same bounded shared PNG importer, native depth promotion, tile transforms, document phase, sampling and source guards as Pattern Overlay. Native RGB luminance defines height; transparent samples blend toward neutral height. Directional differences use Light angle and Depth, with independent shadow/highlight colors. Tile angle rotates the asset independently. Original alpha and raster samples remain intact. Color controls remain meaningful for bitmap Texture; procedural Scale/Invert reject while a bitmap is retained.

Current standard PSD/PSB rasters and merged composites bake the appearance; supplementary source format 19 retains editable Texture and native PNG bytes. Older readers protect unsupported definitions. Geometry, internal clipboard, shared previews, one-step history and reservations use the common bitmap path. Analytic shader, precision, source/round-trip and native gesture checks are queued for final sprint QA. This adds native Texture, without claiming Photoshop bevel/texture equivalence.

## Multiple gradient stops (unreleased; sprint QA pending)

Gradient Overlay offers compact Color stops and Opacity stops editors. Each channel has 2–16 independent stops, strict increasing positions with fixed 0/100% endpoints, and a 1–99% midpoint for each outgoing segment. Midpoints use piecewise linear interpolation: the midpoint receives half the transition. Color uses native normalized RGB; opacity controls overlay coverage while preserving original layer alpha. Transparent stops retain their color data. Add inserts into the largest gap, remove affects interior stops, and Two colors/Opaque restore the existing defaults. Range gestures use the shared engine and commit once on completion.

Shared `effect.add/update` settings accept `color_stops` and `alpha_stops` arrays; each item has `position`, optional `midpoint`, and either `color: [R,G,B]` or `opacity`. Values are normalized within 0–1. Empty arrays use two-color/full-opacity defaults; unknown fields, missing channels, unsorted/equal positions, missing endpoints and excessive counts reject atomically. Existing shape, angle, scale and reverse apply to both channels in document coordinates. Supplementary source format 20 retains definitions, with current appearance in standard PSD/PSB rasters and composites. Native8/16 interpolation, invalid-input atomicity, shared feedback, previews, undo and standard output coverage are queued for combined sprint QA. This is native gradient development, without claiming Photoshop gradient import/equivalence.

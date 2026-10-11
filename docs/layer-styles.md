# Layer-style effects (unreleased)

PeerBrush adds Bevel & Emboss, Contour, Texture, Stroke, Inner Shadow, Inner Glow, Satin, Color Overlay, Gradient Overlay, Pattern Overlay, Outer Glow and Drop Shadow to the regular Color effect stack. Find them by name or under Relief, Outlines, Shadows and Overlays. Top effects run last. Each step has the existing independent strength, bypass, reorder and undo controls; adjusting settings previews actual rendered pixels through the shared engine.

These are native PeerBrush style effects with bounded settings. Bounded Color, Gradient and bitmap Pattern Overlay descriptors can import when the complete native rendered composite matches exactly; broader Photoshop style import remains protected. Saves bake standard channels and retain native editable sources, without exporting Photoshop style descriptors or pattern libraries. A Photoshop document containing unsupported styles, patterns, global masks or profiles stays protected and displays its saved composite. See [Photoshop fidelity](photoshop-fidelity.md).

Unreleased source diagnostics now inspect bounded `lfx2`/`lmfx` descriptors and identify recognized single/multiple style definitions and enabled/disabled states. Inspection alone does not enable editing; unsupported styles and embedded patterns remain protected. See [descriptor limits](photoshop-fidelity.md).

| Effect | Available settings |
| --- | --- |
| Bevel & Emboss | Inner/Outer/Emboss/Pillow, size, soften, depth, light angle/altitude, Up/Down, highlight/shadow colors, independent channel opacity/blend, contour, common opacity and blend |
| Contour | Linear/Cone/Ring/Smooth/Custom profile, range, invert, opacity and blend; maps the incoming relief/tone |
| Texture | Embedded native bitmap PNG with tile transforms and sampling, or Checker/Stripes/Dots/Noise; lighting angle, depth, shadow/highlight colors, opacity and blend |
| Stroke | Circular outline size, Inside/Outside/Center, color, opacity and blend |
| Inner Shadow | Size, distance, angle, choke, contour, color, opacity and blend |
| Inner Glow | Edge/Center source, size, spread, contour, color, opacity and blend |
| Satin | Size, distance, angle, contour, invert, color, opacity and blend |
| Color Overlay | Native-normalized RGB color, opacity and blend |
| Gradient Overlay | Linear/Radial/Angle/Reflected/Diamond, two-color defaults or independent color/opacity stops with midpoints, angle, scale, reverse, opacity and blend |
| Pattern Overlay | Embedded bitmap PNG with native depth, scale, clockwise angle, document phase and nearest/bilinear sampling; or Checker/Stripes/Dots/Noise with pixel scale, invert and two colors; opacity and blend |
| Outer Glow | Size, spread, contour, color and opacity |
| Drop Shadow | Size, distance, angle, spread, contour, color and opacity |

Overlay and inner styles preserve input alpha. Exterior styles can grow into transparent canvas space without replacing or expanding editable source pixels. Earlier steps run in their original source frame; the first active style expands its derived input into document coordinates. Gradient and pattern coordinates use the canvas frame, independent of screen pan/zoom. Output is bounded by the canvas; off-canvas source pixels are retained but exterior styling does not render beyond this frame. Sizes/softening are limited to 64 pixels and shadow/Satin distances to 128 pixels. Large derived/temporary images are rejected explicitly under the existing memory budgets. These Color styles apply to regular layers and isolated folders; adjustment layers, whole-image filters and mask stacks do not offer them.

Drop Shadow and Outer Glow composite beneath the incoming layer image. Their interaction with other layers follows the layer's blend mode; an independent Photoshop-style backdrop blend is not implemented. Relief uses alpha-field shading, not Photoshop's bevel techniques or global-light sharing. Contour operates on incoming luminance as an independent step. Texture and Pattern Overlay accept embedded PNG tiles alongside procedural patterns; bitmap Texture passed combined sprint QA. Gradients support two-color defaults plus bounded custom color/opacity stops; the custom-stop implementation passed combined sprint QA. Photoshop gradient libraries, jitter and proprietary style presets are not imported. These limits are intentional and no bit-for-bit Photoshop equivalence is claimed.

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

PSD/PSB source format 18 retains embedded PNGs and native original pixels alongside current standard rendered raster/mask channels and the merged composite. The save compatibility report still requires explicit acceptance of standard editability losses. The initial PNG import does not by itself interpret Photoshop pattern placement or proprietary layer-style semantics. The bounded Photoshop pattern import below and native bitmap Texture are separate additions. Protected Photoshop originals remain protected.

## Protected Photoshop pattern inspection (unreleased)

The read-only protection report offers **Pattern tiles** for bounded RGB `Patt`/`Pat2`/`Pat3` assets. It displays actual raw tile pixels, names, asset bounds and original 8/16-bit precision. These are unmanaged source samples: Photoshop color handling and layer-style placement are not interpreted. Opening, inspecting and previewing never enables editing or changes the saved composite, source file or history.

The reader accepts version-1 RGB patterns with version-3 virtual memory arrays, matching channel bounds and native depth, and raw, PackBits, ZIP or ZIP-predicted storage. Optional transparency in the final slot preserves native alpha and hidden RGB; combined regression and native inspection QA passed. Written masks/reserved/extra color channels, other color modes, unknown versions or trailers and malformed data remain protected. Each block is bounded to 8 MiB, 16 assets and 512 × 512 per tile; decoded assets across a document are limited to 16 MiB of actual tiled storage. Metadata and decoded assets are runtime inspection data, excluded from editable document serialization and saves.

The structure follows the [Adobe pattern specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/). Compression mode 1 is checked as PackBits with 16-bit row counts, as in the [primary psd-tools pattern reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/patterns.py), and against actual channel data. This inspection does not recover editable Photoshop styles, establish matching Photoshop color or placement, or establish faithful Photoshop Texture.

## Bitmap Texture (unreleased)

Texture also offers **Choose bitmap PNG…**, with the same bounded shared PNG importer, native depth promotion, tile transforms, document phase, sampling and source guards as Pattern Overlay. Native RGB luminance defines height; transparent samples blend toward neutral height. Directional differences use Light angle and Depth, with independent shadow/highlight colors. Tile angle rotates the asset independently. Original alpha and raster samples remain intact. Color controls remain meaningful for bitmap Texture; procedural Scale/Invert reject while a bitmap is retained.

Current standard PSD/PSB rasters and merged composites bake the appearance; supplementary source format 19 retains editable Texture and native PNG bytes. Older readers protect unsupported definitions. Geometry, internal clipboard, shared previews, one-step history and reservations use the common bitmap path. Analytic shader, precision, source/round-trip and native gesture checks passed in the combined sprint QA. This adds native Texture, without claiming Photoshop bevel/texture equivalence.

## Multiple gradient stops (unreleased)

Gradient Overlay offers compact Color stops and Opacity stops editors. Each channel has 2–16 independent stops, strict increasing positions with fixed 0/100% endpoints, and a 1–99% midpoint for each outgoing segment. Midpoints use piecewise linear interpolation: the midpoint receives half the transition. Color uses native normalized RGB; opacity controls overlay coverage while preserving original layer alpha. Transparent stops retain their color data. Add inserts into the largest gap, remove affects interior stops, and Two colors/Opaque restore the existing defaults. Range gestures use the shared engine and commit once on completion.

Shared `effect.add/update` settings accept `color_stops` and `alpha_stops` arrays; each item has `position`, optional `midpoint`, and either `color: [R,G,B]` or `opacity`. Values are normalized within 0–1. Empty arrays use two-color/full-opacity defaults; unknown fields, missing channels, unsorted/equal positions, missing endpoints and excessive counts reject atomically. Existing shape, angle, scale and reverse apply to both channels in document coordinates. Supplementary source format 20 retains definitions, with current appearance in standard PSD/PSB rasters and composites. Native8/16 interpolation, invalid-input atomicity, shared feedback, previews, undo and standard output coverage passed in the combined sprint QA. Midpoint limit drags preserve valid 1/99% values, preview native pixels and commit exactly once. This is native gradient development, without claiming Photoshop gradient import/equivalence.


## Plain Photoshop Pattern Overlay import (unreleased)

A single unscaled, unrotated bitmap Pattern Overlay with a supported blend with document alignment and zero phase can import from an unambiguous embedded RGB tile at matching native depth and zero source origin. Ordinary raster layers with Normal blend are required; supported raster masks and reduced layer opacity are accepted by the extension below. Every resource asset must be retained in a supported style, and the shared renderer must match the saved native composite exactly. Unsupported color handling, placement, unsupported style combinations, unused resources and malformed descriptors remain protected. Imported tiles use the same compact bitmap controls, shared commands and format-18 editable sources as PNG patterns. Combined regression and native effect verification passed; broader Photoshop styles remain unsupported.

## Mixed Photoshop overlays (unreleased)

A supported Color, bitmap Pattern and plain Gradient Overlay can coexist on the same ordinary raster layer. Each family retains fully validated definitions, including disabled entries and embedded tile data. Native execution follows Color, Pattern, Gradient, independent of descriptor property order; the workspace displays the final step at the top. The complete saved native composite must match exactly before editing is enabled. Unsupported families, unknown fields, ambiguous aliases, unsupported multi-effect lists and unused pattern assets still protect the whole source. Native8/16 stack rendering, source retention, undo and PSD/PSB save coverage passed combined QA. This does not recover broader styled sources.

Pattern and Gradient overlays additionally accept Normal/Multiply/Screen/Overlay/Darken/Lighten on import. Every other source/placement limit and exact native composite gate remains in force.

Explicit Photoshop Color/Gradient multi-effect lists retain 1–16 supported entries per family in file order, including disabled definitions, within a 32-effect total stack limit. Multiple Pattern instances and unsupported entries stay protected. Shared reorder, bypass, settings and undo controls operate on each retained definition.

Supported Photoshop overlay imports now retain supported raster masks and layer opacity, including zero opacity. Every mask/source validator and exact composite gate remains active; unsupported masks/clipping, folder styles and non-Normal layer blending remain protected.

Plain Photoshop Gradient Overlay import additionally supports Linear/Reflected at the horizontal/vertical cardinal axes, retaining angle, reverse and all stop definitions. Unscaled document alignment, exact native composite matching and the existing color/interpolation limits remain mandatory.

Supported overlay-only styles also retain validated default raster clipping groups. Original foreground/base samples and clipping relationships remain editable; standard saves bake the overlay appearance while retaining clipping. Missing/incompatible bases and unsupported clipping structures stay protected.

## Custom contours (unreleased)

Contour, Bevel & Emboss, Inner Shadow, Inner Glow, Satin, Outer Glow and Drop Shadow offer a Custom contour with a compact point editor. Each curve has 2–16 points with strictly increasing normalized positions and fixed 0/100% endpoints; values are independently bounded to 0–100% and can rise or fall. Piecewise linear mapping uses native channels. Add inserts a sampled point into the largest gap without changing the curve; remove affects interior points. Endpoint values remain editable. Selecting a preset removes the custom definition in the same shared edit.

MCP/CLI settings use `contour:"custom"` and `contour_points:[{"position":0,"value":0},{"position":1,"value":1}]`. Unknown fields, invalid endpoints, duplicate positions, excessive counts and points attached to presets or unrelated effects reject atomically. Curves are prepared once per effect render; previews and history keep the ordinary shared command path, reservations and original native raster/mask sources. Supplementary PSD/PSB source format 21 retains even disabled definitions alongside current standard rasters and merged composites. Older readers protect newer sources. Native8/16 mapping, gesture, protocol, undo/save and malformed-source coverage passed combined QA. This adds native contour controls; it does not import Photoshop contour descriptors or establish Photoshop relief/shadow equivalence.

## Hard Photoshop shadows (unreleased)

A single plain RGB Drop Shadow or Inner Shadow can import from a fully validated `lfx2`/`lmfx` definition when the complete native saved composite matches exactly. This subset requires zero blur, choke and noise, explicit two-point linear contour data, local light at a cardinal angle, integer distance within 0–128 pixels, and no contour antialiasing. Drop Shadow requires Normal effect blend and layer knockout; Inner Shadow accepts the six bounded overlay blends. Photoshop light angles convert to native displacement angles. Original raster samples and disabled definitions remain intact.

The source must be an ordinary Normal-blended raster with binary alpha, full layer opacity, no mask or clipping, no other style, and no unknown definition fields. Names do not authorize a contour: explicit endpoint samples are required. Lists containing more than one shadow, mixed interior/exterior stacks, softened shadows, partial-alpha sources, global lighting and independent exterior backdrop blends remain protected. Shared native controls and standard baked PSD/PSB outputs apply after import; Photoshop style descriptors are not exported. Analytic native8/16 orientation/opacity, bypass/undo, editable saves and protected-fallback coverage passed combined QA. Descriptor fields follow the [primary psd-tools effect reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/effects.py) and [Krita ASL serializer](https://invent.kde.org/graphics/krita/-/blob/master/libs/image/kis_asl_layer_style_serializer.cpp); the exact composite gate remains mandatory.

## Independent bevel channels (unreleased)

Bevel & Emboss has separate Highlight opacity, Shadow opacity, Highlight blend and Shadow blend controls. Each channel opacity multiplies the common effect opacity; 100% retains the previous strength. Blend controls default to **Inherit effect blend**, retaining legacy documents' common blend choice. Choosing an explicit blend affects only that channel. These compact controls use shared engine commands and actual rendered gesture previews, with one undo on completion. AI changes use the existing blue, interruptible presentation path.

MCP/CLI settings use `highlight_opacity` and `shadow_opacity` within 0–1, and `highlight_blend`/`shadow_blend` with shared blend keys or `"inherit"`. Invalid values and fields on unrelated effects reject atomically. Current standard PSD/PSB rasters and merged composites retain the rendered result; supplementary source format 22 retains explicit independent settings, including disabled effects, and takes precedence over contour format 21 when both occur. Older readers protect newer sources. Analytic native8/16 channel coverage/blends, legacy appearance, gestures, source/undo/save and atomicity coverage passed combined QA. Relief remains the documented native alpha-field technique; Photoshop bevel techniques, global light and backdrop semantics remain unsupported.

## Gradient placement (unreleased)

Gradient Overlay has compact Offset X/Y controls from −100% to 100% of the canvas width/height. Positive X moves the center right; positive Y moves it down. Linear, reflected, radial, angle and diamond gradients keep their existing shape, stops, angle, scale and reverse. Placement uses document coordinates, independent of screen navigation; resizing the canvas changes the relative center. Source raster/mask samples remain intact.

Shared settings use optional finite `offset_x`/`offset_y` within −1–1; missing values preserve the existing centered appearance. Explicit offset definitions, including disabled effects, require supplementary source format 23 alongside current standard PSD/PSB output. Older readers protect these sources. UI gestures use actual engine previews and one undo step; AI edits use the existing blue presentation path. Native8/16 analytic coordinates, atomic validation, shared feedback, gesture limits and source/undo/save coverage passed combined QA. A separate bounded Photoshop placement extension is implemented below; combined QA passed.

## Photoshop gradient placement extension (unreleased)

The cardinal Linear/Reflected Gradient Overlay subset accepts an explicit Scale of 10–1000% and optional Offset X/Y percentages within −100–100%. Offsets use the canvas frame and retain both coordinates, including zero-valued and disabled definitions. Missing offsets keep the previous centered behavior. Root style scaling must remain 100%; layer alignment, arbitrary rotation, dithering, interpolation variants, malformed units and broader structures remain protected. Complete standard native RGB/alpha composite matching is mandatory before editing.

Imported settings use the same native scale and offset controls, shared previews, history and native source preservation. Explicit offsets use supplementary source format 23, with current standard PSD/PSB rasters and merged composites. Analytic native8/16 orientation, scale/offset limits, reverse/reflected behavior, disabled definitions, bypass undo, saves and malformed/mismatched protection passed combined QA. The placement model follows the [primary Krita overlay renderer](https://github.com/KDE/krita/blob/master/libs/image/layerstyles/kis_ls_utils.cpp); exact saved-composite verification remains necessary. Pattern resampling/phase and broader Photoshop style fidelity remain open.

A further unreleased importer extension accepts an explicit `dropShadowMulti` or `innerShadowMulti` list containing exactly one fully supported shadow descriptor. The retained effect has the same settings and enabled state as the corresponding single-key definition. Empty/multiple lists, incorrect entry types/classes, ambiguous aliases and unsupported fields remain protected, including disabled definitions. The original native source and exact saved-composite gate are unchanged. Development is implemented; combined regression and native-workspace acceptance verification is pending.

Unrotated Pattern Overlay definitions may omit the optional angle field. The importer now treats that omission as zero rotation; explicit angle values still require finite zero-valued angle units. Every existing source, placement and exact saved-composite guard remains active, including disabled definitions. This follows the default in the [primary psd-tools effect reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/effects.py) and the omitted field in the [Krita ASL serializer](https://github.com/KDE/krita/blob/master/libs/image/kis_asl_layer_style_serializer.cpp). Development is implemented; combined regression and native-workspace acceptance verification is pending.

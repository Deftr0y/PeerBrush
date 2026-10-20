# Editable text and vector shapes

Use the **Text** toolbar tool (**T** by default) to click a text field for editing, click empty artwork for a new field, or drag its bounds. Placement previews actual shared-engine text pixels. Releasing opens the properties window; **Apply** creates one undo step. **Cancel**, Escape or a newer shared edit discards the preview. Active placement also cancels on focus loss or a tool change. Space-drag, middle-button pan and Alt-secondary zoom retain their navigation behavior before placement begins.

**Layer → New text**, **New vector shape**, **Edit text/vector** and the layer context menu remain available. A rejected Apply retains the properties draft and displays the error, so a temporary reservation need not discard typed text. Source layers remain ordinary layers with an editable Text definition.

Text retains its characters, field dimensions, bundled Ubuntu Sans font weight, pixel size, line spacing, letter spacing, alignment and color. Regular, medium and semibold are available. Letter spacing adds native source pixels between adjacent glyphs, alongside kerning; it adds no trailing spacing or spacing across explicit newlines. Negative values allow tighter text. This renderer supports plain text with available glyphs; it does not perform complex script shaping, bidirectional layout, automatic wrapping, rich text, or system-font substitution. Unsupported glyphs and shaping-dependent scripts are refused with an explanation. Installed document-font selection and missing-font workflows remain unfinished PB-066 work.

Vector shape sources retain rectangle/ellipse bounds or straight path points, an open/closed path flag, fill, stroke and stroke width. Unreleased [Pen sources](pen.md) also retain editable cubic Bézier anchors and handles. Fill uses even-odd coverage and strokes use round segment ends; path boolean operations and advanced stroke joins remain unsupported. Shapes use supersampled edge coverage. Native 16-bit source colors and coverage go directly into 16-bit raster channels. The compact color values use the document's channel range, 0–255 or 0–65535.

Text properties expose editable field width and height. Resizing changes the local text layout/clipping frame while preserving its document placement and affine basis. Its projected raster retains the previous extent and may grow to contain the new field, preserving attached mask sources rather than scaling or discarding them. Masks, effects, hierarchy and blend settings remain editable. Other source kinds retain their fixed local frames.

New layers are placed at the shown document X/Y coordinates. Move, rotate and scale the complete layer with the existing tools; the editable definition and accumulated affine placement survive. Image resizing updates that placement. Crop and canvas resize retain off-canvas source data. Copy/duplicate/paste of whole layers retains the source; copying selected pixels produces raster artwork.

Painting into source color pixels is refused. Paint the mask, edit the source properties, or choose **Layer → Rasterize text/vector** explicitly. Rasterizing retains the current pixels and removes editable properties; one Undo restores them. Selected-pixel and mask transforms require explicit rasterization; whole-layer transforms preserve the source.

PSD/PSB saves contain current standard raster/mask channels and a current merged composite. Basic text/shape definitions use supplementary format 8; nonzero letter spacing requires format 30, taking precedence over the mask format. Zero spacing retains legacy serialization and rendering. Sources remain editable when reopened in PeerBrush; Photoshop receives standard raster layers, not native Photoshop type or vector objects. Older readers cannot restore newer source semantics. Imported unsupported Photoshop type/vector features retain the existing protected read-only policy.

## Shared commands

```json
{"op":"source.add","name":"Title","x":80,"y":60,"source":{"width":400,"height":160,"content":{"kind":"text","text":"PeerBrush","font":"semibold","size":48,"line_height":1.2,"align":"left","color":[59881,21588,8224,65535]}}}
```

```json
{"op":"source.add","x":100,"y":180,"source":{"width":160,"height":120,"content":{"kind":"shape","shape":"ellipse","bounds":[8,8,152,112],"points":[],"closed":true,"fill":[45001,15003,8127,65535],"stroke":[65535,65535,65535,65535],"stroke_width":3}}}
```

`source.update` requires an existing `layer` ID and a complete `source` definition. Read the current source from observation first. Text supports changes to `width`/`height` and optional `content.letter_spacing` (default zero); retain the matrix while editing properties. A resize can reframe the raster and translate its matrix, so observe again before subsequent updates. Other source kinds keep their dimensions during property updates. The optional matrix is `[a,b,c,d,tx,ty]`, mapping local coordinates to the current raster frame, and defaults to identity. `source.rasterize` requires a layer ID. Optional `parent` creates inside an existing unlocked folder. All commands share UI/MCP/CLI validation, expected revisions, `document_id`/`source_revision` guards and reservations.

Source frames are bounded to four million pixels, text to 4096 characters/96 lines, letter spacing to −1024…1024 native pixels, path vertices to 128, and outline/path rendering to explicit work budgets. Text resizing additionally enforces projected raster, coordinate and color/mask work budgets. Failed or unsupported requests are atomic and preserve human work. Derived caches never replace editable definitions or standard saved channels.

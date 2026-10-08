# Editable text and vector shapes

Use **Layer → New text** or **New vector shape** to create a separate editable layer. The properties window previews the actual canvas result. Apply creates one undo step; Cancel or Escape discards the preview. Choose **Layer → Edit text/vector**, or use the layer's context menu, to edit it later. A newer shared edit cancels a stale properties preview.

Text retains its characters, bundled Ubuntu Sans font weight, pixel size, line spacing, alignment and color. Regular, medium and semibold are available. Explicit newlines create multiple lines. This renderer supports plain text with available glyphs and kerning; it does not perform complex script shaping, bidirectional layout, automatic wrapping, rich text, or system-font substitution. Unsupported glyphs and shaping-dependent scripts are refused with an explanation.

Vector sources retain rectangle/ellipse bounds or editable path points, an open/closed path flag, fill, stroke and stroke width. Paths contain straight segments, with even-odd fill and round segment ends; Bézier editing, path boolean operations and advanced stroke joins are not implemented. Shapes use supersampled edge coverage. Native 16-bit source colors and coverage go directly into 16-bit raster channels. The compact color values use the document's channel range, 0–255 or 0–65535.

Each source has a fixed local frame. New layers are placed at the shown document X/Y coordinates. Move, rotate and scale the complete layer with the existing tools; the editable definition and accumulated affine placement survive. Image resizing updates that placement. Crop and canvas resize retain off-canvas source data. Properties regenerate the source within its frame, retaining its masks, effects, hierarchy and blend settings. Copy/duplicate/paste of whole layers retains the source; copying selected pixels produces raster artwork.

Painting into source color pixels is refused. Paint the mask, edit the source properties, or choose **Layer → Rasterize text/vector** explicitly. Rasterizing retains the current pixels and removes editable properties; one Undo restores them. Selected-pixel and mask transforms require explicit rasterization; whole-layer transforms preserve the source.

PSD saves contain current standard raster/mask channels and a current merged composite. PeerBrush source definitions are supplementary format-8 data. They remain editable when reopened in PeerBrush; Photoshop receives standard raster layers, not native Photoshop type or vector objects. Older PeerBrush readers open these documents protected. Imported unsupported Photoshop type/vector features retain the existing protected read-only policy.

## Shared commands

```json
{"op":"source.add","name":"Title","x":80,"y":60,"source":{"width":400,"height":160,"content":{"kind":"text","text":"PeerBrush","font":"semibold","size":48,"line_height":1.2,"align":"left","color":[59881,21588,8224,65535]}}}
```

```json
{"op":"source.add","x":100,"y":180,"source":{"width":160,"height":120,"content":{"kind":"shape","shape":"ellipse","bounds":[8,8,152,112],"points":[],"closed":true,"fill":[45001,15003,8127,65535],"stroke":[65535,65535,65535,65535],"stroke_width":3}}}
```

`source.update` requires an existing `layer` ID and a complete `source` definition. Read the current source from observation first; retain its `width`, `height` and `matrix` while changing content properties. The optional matrix is `[a,b,c,d,tx,ty]`, mapping original local coordinates to the current raster frame, and defaults to identity. `source.rasterize` requires a layer ID. Optional `parent` creates inside an existing unlocked folder. All commands share UI/MCP/CLI validation, expected revisions, `document_id`/`source_revision` guards and reservations.

Source frames are bounded to four million pixels, text to 4096 characters/96 lines, path vertices to 128, and outline/path rendering to explicit work budgets. Failed or unsupported requests are atomic and preserve human work. Derived caches never replace editable definitions or standard saved channels.

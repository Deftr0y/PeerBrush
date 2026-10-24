# Editable text and vector shapes

Use the **Text** toolbar tool (**T** by default) to click a text field for editing, click empty artwork for a new field, or drag its bounds. Placement previews actual shared-engine text pixels. Releasing opens the properties window; **Apply** creates one undo step. **Cancel**, Escape or a newer shared edit discards the preview. Active placement also cancels on focus loss or a tool change. Space-drag, middle-button pan and Alt-secondary zoom retain their navigation behavior before placement begins.

**Layer → New text**, **New vector shape**, **Edit text/vector** and the layer context menu remain available. A rejected Apply retains the properties draft and displays the error, so a temporary reservation need not discard typed text. Source layers remain ordinary layers with an editable Text definition.

Text retains its characters, field dimensions, exact font family/style/weight, pixel size, line spacing, letter spacing, alignment, color and layout choice. The picker includes bundled Ubuntu Sans regular/medium/semibold and automatically discovered installed static TrueType/OpenType outline fonts, including collection faces. Its searchable list uses the native Windows DirectWrite, macOS CoreText or Linux Fontconfig inventory. Unsupported variable, simulated, color/bitmap/SVG or oversized faces remain listed with a reason. No automatic font import, registration, download or substitution occurs.

Existing fields keep their original character layout and pixels. The **Ligatures and joining** option explicitly enables the new layout (`source.content.layout:"shaped"`). It shapes the exact selected font, resolves script runs and bidirectional order, and positions ligatures and combining marks using the font's advances and offsets. It renders outlines directly into native8/16 channels. Default/omitted `layout` remains `"legacy"`; opening an older field never silently reflows it. Legacy installed-font layout retains its plain Latin/Greek/Cyrillic/Han/precomposed Hangul/Hiragana/Katakana/Bopomofo/Common limits and rejects shaping-dependent text.

Letter spacing adds native pixels between adjacent characters in legacy layout, or between shaped clusters in the new layout, without splitting marks or adding a trailing gap. Standard ligatures remain enabled with nonzero tracking. Joining and other complex scripts require zero letter spacing. Explicit newlines determine lines; no automatic paragraph wrapping, rich text, vertical layout, language-specific selection or Photoshop type-layer import is claimed. Size keeps the existing native font-height scale, with line spacing measured from that height. Alignment uses full advances, including spaces, rather than visible ink bounds. Join controls and Unicode default ignorables participate in shaping without visible replacement glyphs; unsupported variation selectors, missing actual glyphs and unsupported actual outlines reject. Explicit bidi embedding/isolate controls and non-newline control characters remain refused. Text outside its field clips normally; over-budget coordinates or outlines reject atomically.

The unmodified official Noto Arabic and Devanagari fonts under `tests/fonts/noto` are licensed acceptance fixtures only. They are not bundled application fallback fonts. Prepared tests cover actual joining/lam-alef, combining marks and Indic pre-base ordering, exact selected bytes/collection face, native preview/history and standard PSD/PSB channels. Compilation, platform installed-font and visible native text acceptance remain pending for this unreleased batch.

Vector shape sources retain rectangle/ellipse bounds or straight path points, an open/closed path flag, fill, stroke and stroke width. Unreleased [Pen sources](pen.md) also retain editable cubic Bézier anchors and handles. Fill uses even-odd coverage and strokes use round segment ends; path boolean operations and advanced stroke joins remain unsupported. Shapes use supersampled edge coverage. Native 16-bit source colors and coverage go directly into 16-bit raster channels. The compact color values use the document's channel range, 0–255 or 0–65535.

Text properties expose editable field width and height. Resizing changes the local text layout/clipping frame while preserving its document placement and affine basis. Its projected raster retains the previous extent and may grow to contain the new field, preserving attached mask sources rather than scaling or discarding them. Masks, effects, hierarchy and blend settings remain editable. Other source kinds retain their fixed local frames.

New layers are placed at the shown document X/Y coordinates. Move, rotate and scale the complete layer with the existing tools; the editable definition and accumulated affine placement survive. Image resizing updates that placement. Crop and canvas resize retain off-canvas source data. Copy/duplicate/paste of whole layers retains the source; copying selected pixels produces raster artwork.

Painting into source color pixels is refused. Paint the mask, edit the source properties, or choose **Layer → Rasterize text/vector** explicitly. Rasterizing retains the current pixels and removes editable properties; one Undo restores them. Selected-pixel and mask transforms require explicit rasterization; whole-layer transforms preserve the source.

PSD/PSB saves contain current standard raster/mask channels and a current merged composite. Basic text/shape definitions use supplementary format 8; nonzero letter spacing requires format 30, installed-font references require format 31, and shaped layout requires format 34, taking precedence over all older source formats. Zero spacing retains legacy serialization and rendering. Sources remain editable when reopened in PeerBrush; Photoshop receives standard raster layers, not native Photoshop type or vector objects. Older readers cannot restore newer source semantics. Imported unsupported Photoshop type/vector features retain the existing protected read-only policy.

## Exact fonts and missing-font projects (unreleased)

Font choices persist family, PostScript name, style, weight, stretch, collection index and SHA256 of the selected file. Paths and font bytes are private to the local derived service and are never embedded in PSD sources, shared metadata or packages. Discovery and bounded font reads happen on a worker; UI/render callers use cached owned snapshots. A render validates and uses one exact snapshot throughout, preserving native8/16-bit coverage even if the catalog refreshes concurrently. Loading/busy states are explicit and can be retried; there is no automatic replacement.

If a saved exact font is missing, changed or unsupported, the layer retains its text/font metadata and saved native raster. Opening, moving, applying effects/mask edits, grouping, copying whole layers, unrelated edits and saving remain available. Editing the text, resizing its field, scaling/rotating it or resizing the image requires an available exact font or an explicit replacement. The properties window shows the reason and keeps Apply disabled until the draft can render. Selecting a replacement previews actual shared-engine pixels before Apply; Cancel preserves the saved appearance. Explicit **Rasterize text/vector** retains those pixels and one Undo restores the source. Unsupported imported Photoshop type structures remain protected read-only.

The service bounds native descriptors to 8192, collection faces to 256, metadata to 8 MiB, font files to 32 MiB, pending work to 8 requests and cached snapshots to 64 MiB. A caller may retain its owned snapshot until its current render finishes after cache eviction. UI fallback fonts are a separate presentation feature. Native OS inventory and installed-font acceptance must pass the platform checks before this unreleased feature is qualified.

## Shared commands

```json
{"op":"source.add","name":"Title","x":80,"y":60,"source":{"width":400,"height":160,"content":{"kind":"text","text":"PeerBrush","font":"semibold","size":48,"line_height":1.2,"align":"left","color":[59881,21588,8224,65535]}}}
```

```json
{"op":"source.add","name":"Joined title","x":80,"y":60,"source":{"width":400,"height":160,"content":{"kind":"text","layout":"shaped","text":"office a\u0301","font":"regular","size":48,"line_height":1.2,"align":"left","color":[59881,21588,8224,65535]}}}
```

```json
{"op":"source.add","x":100,"y":180,"source":{"width":160,"height":120,"content":{"kind":"shape","shape":"ellipse","bounds":[8,8,152,112],"points":[],"closed":true,"fill":[45001,15003,8127,65535],"stroke":[65535,65535,65535,65535],"stroke_width":3}}}
```

`source.update` requires an existing `layer` ID and a complete `source` definition. Read the current source from observation first. Text supports changes to `width`/`height` and optional `content.letter_spacing` (default zero); retain the matrix while editing properties. A resize can reframe the raster and translate its matrix, so observe again before subsequent updates. Other source kinds keep their dimensions during property updates. The optional matrix is `[a,b,c,d,tx,ty]`, mapping local coordinates to the current raster frame, and defaults to identity. `source.rasterize` requires a layer ID. Optional `parent` creates inside an existing unlocked folder. All commands share UI/MCP/CLI validation, expected revisions, `document_id`/`source_revision` guards and reservations.

Source frames are bounded to four million pixels, text to 4096 characters/96 lines and 16384 generated shaped glyphs, shaped coordinates to ±100000 native pixels, letter spacing to −1024…1024 native pixels, path vertices to 128, and outline/path rendering to explicit work budgets. Text resizing additionally enforces projected raster, coordinate and color/mask work budgets. Failed or unsupported requests are atomic and preserve human work. Derived caches never replace editable definitions or standard saved channels.

Font discovery is shared by MCP `peerbrush_fonts` and CLI `fonts`:

```json
{"action":"list","search":"Sans","limit":32,"offset":0}
```

List returns a catalog generation/status, a bounded page of exact font references or unsupported reasons, total and next offset. Search accepts 128 characters; pages contain 1–128 rows. Use an unchanged `font` record from the list with `{"action":"prepare","font":...}`, poll until `ready:true`, then supply that record in `source.content.font`. `refresh` invalidates derived catalog/cache decisions without editing projects. Existing bundled string aliases retain their original serialization. These requests accept metadata only, never an arbitrary font path or file content.

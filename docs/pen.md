# Editable Pen paths

Unreleased: choose **Pen** in the toolbar or press **P**. Click to place corner anchors; drag while placing an anchor to create paired Bézier handles. Move an existing anchor by dragging it, or drag a displayed handle. Smooth points keep the opposite handle aligned; Corner points allow independent handles. Alt-click a point to convert it. The toolbar also offers Corner, Smooth and point deletion. Delete/Backspace removes the selected point while Pen is active, retaining its layer.

Click the first point or choose Close path to close the contour. Finish/Enter ends an open drawing sequence; Continue resumes it. Reopen permits further drawing. New path creates a separate source. Clicking a closed segment, or Ctrl/Cmd-clicking an open segment, inserts an anchor through Bézier subdivision, retaining the curve's geometry. Drag previews use actual rendered pixels through the shared engine and commit once on release. Escape, tool changes or intervening shared edits cancel an unfinished gesture. Completed anchors stay editable and have ordinary Undo/Redo.

Properties controls the native fill, stroke, stroke width and closure. Closed paths offer Selection and Mask. Selection converts the contour into a shared polygon selection; Mask applies its area to the currently selected layer, preserving Color and the existing area selection. The active path can remain on a separate layer. Open or zero-area contours cannot convert to an area. Conversion follows the shared polygon, feathering and mask limits.

Path sources retain up to 128 anchors with absolute source-frame positions and optional incoming/outgoing handles. Cubic curves flatten adaptively within 0.125 source pixels, with a 4096-vertex subdivision limit. Fill uses even-odd coverage and strokes use round segment ends; advanced joins, multiple contours and path boolean operations remain unsupported. Source frames retain the existing four-million-pixel limit and bounded rendering work. New canvas paths use the document frame; larger documents require an explicitly bounded source frame through the shared command. Out-of-frame source geometry remains editable but its raster projection is clipped to that frame.

Native 8/16-bit artwork and RGBA16 source colors stay at their original depth. Screen/protocol previews may project to 8 bit. PSD/PSB files contain current standard raster/mask channels and merged composites, with supplementary format-17 PeerBrush path sources. Photoshop receives raster layers; these are not native Photoshop Pen/vector-mask records. Unsupported imported Photoshop path/vector structures remain protected and read-only.

## Shared commands

Path commands use document pixels for positions and handles; the engine maps them through the layer's origin and editable affine placement. Observe the layer's source, IDs and revision before editing. All commands enforce the same locks, reservations, revision guards and undo behavior as the UI.

```json
{"op":"path.create","frame":[0,0,512,512],"nodes":[{"point":[40,160],"out_handle":[80,40]},{"point":[240,160],"in_handle":[200,40]}],"closed":false,"fill":[0,0,0,0],"stroke":[59881,21588,8224,65535],"stroke_width":2}
```

Frame defaults to the document rectangle. Optional parent/name use ordinary source-layer validation. RGBA source colors use 0–65535 at both depths; 8-bit raster output explicitly projects them.

```json
{"op":"path.node.add","layer":"PATH_ID","point":[280,220],"in_handle":[240,200],"out_handle":[320,240],"smooth":true}
{"op":"path.node.move","layer":"PATH_ID","index":1,"point":[250,170]}
{"op":"path.handle.move","layer":"PATH_ID","index":1,"handle":"out","point":[300,200]}
{"op":"path.node.split","layer":"PATH_ID","index":0,"t":0.5}
{"op":"path.node.convert","layer":"PATH_ID","index":1,"smooth":false,"reset_handles":true}
{"op":"path.node.delete","layer":"PATH_ID","index":1}
{"op":"path.update","layer":"PATH_ID","closed":true,"fill":[12345,23457,34569,65535]}
```

Add accepts an optional insertion index; a closed path requires that index rather than an implicit append. Split indexes a segment and accepts t within 0.001–0.999. Moving an anchor translates both handles. Smooth handle movement retains the opposite handle's length and alignment. Smooth conversion creates balanced tangents from neighboring anchors; Corner can preserve independent handles or explicitly reset them. Deleting down to fewer than two anchors opens the path; an empty editable path remains on its layer.

```json
{"op":"path.to_selection","path":"PATH_ID","mode":"replace","feather":1}
{"op":"path.to_mask","path":"PATH_ID","layer":"DESTINATION_ID","mask_mode":"replace","feather":1}
```

Selection supports the shared replace/add/subtract/intersect modes. Mask conversion preserves area selection state and uses replace/add/subtract mask modes. Conversion reserves the source path and the affected selection or destination layer. Ordinary expected_revision, document_id and source_revision guards apply.

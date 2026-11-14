# Crop and resize

The **Image** menu provides Crop, Canvas size and Image size. Each editor shows the actual shared-engine result and its dimensions while you adjust it. Apply commits one undoable change. Cancel or Escape restores the current document. A newer shared edit cancels the stale preview; protected Photoshop documents cannot be resized.

**Crop** has eight canvas handles for dragging edges and corners. The opposite edge stays fixed during a drag, and Escape restores the bounds from before the drag. For exact placement, enter Left, Top, Right and Bottom in document pixels. Right and bottom are exclusive. An active selection initializes the crop to its bounding rectangle. **Use selection bounds** restores that rectangle. Cropping changes the canvas frame and shifts all layer origins together. It retains artwork beyond the new canvas, including native 16-bit words, masks, hidden layers and editable effect sources. Negative crop coordinates expand the canvas. An irregular selection still produces a rectangular crop.

**Canvas size** changes the visible canvas without resampling source pixels. Choose one of nine anchors to position existing artwork within the new dimensions. Center splits the dimension difference, rounded to the nearest pixel; corner anchors keep the corresponding corner fixed. Transparent space is added around the artwork. Shrinking retains pixels outside the canvas, so expanding it can reveal them again.

**Image size** resamples every layer and paint mask, including hidden layers, at the original channel depth. Keep aspect ratio is enabled initially. Sampling uses premultiplied bilinear interpolation with clamped source edges. Layer frames and origins round to integer pixels. Editable blur, bloom, feather and liquify controls scale with the image; captured liquify selections retain soft confidence values and holes. Spatial effects require proportional dimensions, allowing normal integer rounding. Non-proportional requests with those effects are refused; supported radius limits remain explicit. Resizing clears the active selection and its previous state. Repeated image resizes resample the current source pixels; Undo restores the preceding originals.

The resize diagrams show the original frame in gray and the proposed frame in orange. Canvas Size displays the artwork offset in document pixels, and Image Size displays the percentage change on each axis. The canvas shows the rendered result.

## Shared commands

```json
{"op":"crop","rect":[100,80,900,680]}
```

```json
{"op":"canvas.resize","width":1600,"height":1200,"anchor":"bottom_right"}
```

```json
{"op":"image.resize","width":800,"height":600}
```

Canvas anchors are `top_left`, `top`, `top_right`, `left`, `center`, `right`, `bottom_left`, `bottom` and `bottom_right`. The default is `center`. The legacy `resize` command is an alias for `image.resize`.

Use the same commands through native controls, `peerbrush_edit`, local RPC or CLI. Optional `document_id` and exact `source_revision` guard the sampled project state. Normal expected revisions and task reservations apply. These operations reserve the whole document and refuse movement or resampling of locked layers. Disk-backed documents support 16000 pixels per edge. Their aggregate resize allowance is the greater of 128 million samples and the destination canvas area, shared by artwork, masks and captured effect coverage. Resident documents support 8192 pixels per edge, 32 megapixels and 128 million resize samples. Existing raster, effect, mask, selection-source and storage budgets also apply. Failed operations are atomic.

Normal PSD saves contain current standard raster/mask channels and a merged composite, with PeerBrush's retained off-canvas and editable sources stored additionally. PNG and PSD retain the document's original depth. Geometry does not implicitly convert 16-bit channels to 8 bit.

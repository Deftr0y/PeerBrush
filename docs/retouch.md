# Clone and heal

Press **C** for Clone or **J** for Heal. Alt-click the canvas, or choose **Set source** and click, to choose a source, then paint with the usual brush size, hardness, opacity, flow, tip, pressure and taper controls. The source crosshair shows where the current stroke samples. No stroke is committed until release; Undo restores the entire gesture.

**Aligned** keeps the source-to-destination offset across strokes. Turn it off to restart each stroke from the chosen source. **Merged** samples visible artwork, including effects, masks and the layer stack. Otherwise the source is the chosen paint layer's original pixels. A source may belong to another layer in the same project. Changing projects or channels requires choosing a new source. If the shared image changes during a gesture, the gesture is canceled before it can overwrite newer work.

Clone copies sampled color and alpha through brush coverage. Heal transfers source texture while adapting its local average color to the destination, preserving the destination's alpha. Healing works on existing pixels; use Clone to fill transparent areas. This is local color adaptation, not Photoshop's complete healing algorithm.

Both tools honor soft selections and holes, layer origins, locks and reservations. Mask mode samples the selected layer's evaluated grayscale mask and writes the active paint mask step. Native 16-bit source words, interpolation and mask values stay at their original precision; screen previews alone may be projected to 8 bit.

## Shared commands

Use `peerbrush_edit`, the local RPC `edit` method, or the CLI edit command with explicit document coordinates:

```json
{"op":"clone","layer":"DESTINATION_ID","source_layer":"SOURCE_ID","source":[120,80],"points":[[300,200],[320,205]],"radius":12,"hardness":0.8,"opacity":0.7,"flow":1}
```

```json
{"op":"heal","layer":"LAYER_ID","source":[120,80],"points":[[300,200],[320,205]],"radius":12,"heal_radius":16,"sample_merged":true}
```

`source` is required. Its offset is measured from the first destination point and stays fixed throughout that command. Agents implement aligned continuation by advancing `source` with each new stroke's first destination point. `source_layer` defaults to the destination layer; merged sampling ignores this layer choice. `heal_radius` is an integer from 1 to 64 pixels, defaulting to the brush radius clamped to 1–32. Merged sampling is limited to 16 megapixels. Shared brush work budgets reject excessive requests atomically.

Mask commands add `mask:true` and optionally destination `step` and source `source_step` IDs. Without `source_step`, sampling uses the evaluated mask; an explicit source step must be a paint step. Merged artwork cannot be sampled into a mask.

Include `document_id` and `source_revision` from observation to require the exact sampled document, plus the normal `expected_revision` and reservation `task`. The engine freezes the source before writing destination pixels, including overlapping clone strokes, and returns actual image feedback with explicit document coordinates. Both operations are a single undoable batch and are shown as blue AI work when authored by an agent.

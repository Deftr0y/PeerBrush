# Getting started

Run the portable `peerbrush` executable. Open the File menu for new/open/import/export and Save As. Keyboard undo/redo works on shared edits. Rust is only needed to develop PeerBrush, not to run a packaged build.

The first canvas is transparent. Use the brush, eraser, fill, gradient, rectangle, ellipse, move, rectangle selection, eyedropper, and pan tools. Scroll or **Alt + right-mouse drag** to zoom around the cursor; **middle-mouse drag** or Space-drag to pan. **F** frames an active selection with a small margin, or centers and fits the full canvas when nothing is selected. The frame icon does the same.

Create regular layers or folders in the layer panel. Creating a folder gathers the selected layers into it, preserving their order and children. Click anywhere on a row to select it; use its eye button to toggle visibility. Drag the row or its dotted grip to reorder. The held row follows the pointer, neighboring rows animate into place, and the canvas previews the new order before release. Expand or collapse groups with the arrow. Right-click for duplication, locking, masks, group membership, and deletion. Double-click a layer name to rename it directly in the row. Enter or clicking away finishes the rename; Escape cancels it. No popup or confirmation is needed. Right-click **Merge** or press Ctrl/Cmd+E to merge adjacent layers or flatten a folder in one undo step. Layers with backdrop-dependent blends should be grouped and the folder merged to preserve their internal blend results.

Click the content thumbnail to paint pixels. Click the mask thumbnail to paint grayscale. The two overlapping swatches at the bottom left set foreground and background colors. **X** swaps them; mask colors remain independent from artwork colors. Click a swatch for the large hue/saturation picker, compact RGBA controls, and editable HEX values (`RGB`, `RRGGBB`, or `RRGGBBAA`). Alt-click isolates a mask; Shift-click toggles it. The mask stack supports paint, fill, invert, levels, feathering, curves, Gaussian blur and tonal adjustments. Select a paint step to edit it; use Add effect to extend the stack.

Save working documents as PSD and export PNG. Import PNG/JPEG as layers, or **drop images anywhere**. Drop one PSD to open it; unsaved work must be saved first. Ctrl/Cmd+S saves; Ctrl/Cmd+O opens; Ctrl/Cmd+Z undoes; Ctrl/Cmd+Shift+Z redoes.

Click a layer row to work with whole layers. **Ctrl/Cmd+D** duplicates the selected layers or folders; **Ctrl/Cmd+C**, **X**, and **V** copy, cut and paste them with their editable masks, effects, children and transforms. Cut removes content only after the clipboard write succeeds. A failed or stale copy cannot delete your work.

Click the canvas to work with pixels. **Ctrl/Cmd+C** copies the active layer's selection, including its visible color effects and mask. A rotated selection clips copied pixels to its exact shape. **Ctrl/Cmd+Shift+C** copies the visible composite. **Ctrl/Cmd+V** also accepts images copied in another application. A PeerBrush pixel copy retains its position; an external image is centered. Paste uses the selected folder or the selected layer's folder. At the top level it creates a folder containing the original and pasted content. Paste and any folder creation are one undo step. Text fields keep normal text shortcuts.


## Compatibility

This is a foundation release, not the complete product roadmap. Editable PSD support is 8-bit RGB with basic raster layers, nested isolated groups, opacity, six blend modes, and raster masks. Unsupported layer features or profiles open as a read-only merged preview. PSB, additional color modes, and bit depths are rejected with an explanation.

PeerBrush embeds its editable sources in the PSD and saves normal raster/mask data for other editors. Externally changed standard PSD data invalidates embedded definitions instead of resurrecting stale content.

The renderer displays a cached preview through the GPU; compositing currently runs on a CPU worker. Large-document streaming, disk eviction, full ICC color management, clipping, clone/heal, text, and advanced selections remain follow-up work. Consult `peerbrush_capabilities` for the exact command surface.

## Recovery and shared control

Unsaved changes generate a recovery PSD in PeerBrush's runtime folder. The AI connection settings panel offers recovery when present. Recovery is a safety copy; save explicitly to keep work permanently.

AI can reserve specific layers or rectangular regions while you edit elsewhere. Blue outlines and layer badges identify AI reservations and recent AI edits. AI opacity and transform previews ease into place, and hierarchy rows animate into their new folder positions. These are presentation transitions; the current shared document and undo history update immediately. Take back control releases reservations. Agents using the old task ID cannot keep editing under that task.


## Maya-style transforms

**Q** hides gizmos; **W** shows move; **E** shows rotation; **R** shows scale. Select a layer, then drag the colored X/Y handles or the center handle. Move/scale axis handles constrain that axis. Shift snaps rotation to 15° or makes scaling uniform. Escape cancels a drag. **D** selects the eraser.

Gizmos preview the actual pixels and apply one undoable edit on release. An active selection transforms its pixels while preserving the remainder of the layer. Its outline rotates, scales and moves with the content; subsequent painting, fills and copying respect that shape. Without a selection, the whole layer transforms. Rotation and scaling resample raster content; undo before trying another transform to retain the original pixels. Whole-group transforms remain planned.

The top bar shows green **MCP connected** when a live client is attached, red otherwise. The animated center line shows agent-supplied progress and tool activity. It is a shared activity display, not access to a model's private reasoning.

## Brush and effects

**[ / ]** (also shifted braces) change brush/eraser size. Hold **S** and move the pointer horizontally over the canvas to resize with a live outline and numeric readout, without painting. Open Brush settings from the small adjustment icon beside Size: hardness, opacity, flow, spacing, roundness, angle, smoothing, and three initial presets. Opacity caps each stroke; flow builds with overlapping dabs. The live stroke uses the same engine as the committed result. Hold **Alt** in Brush for a temporary eyedropper; click to sample, then release Alt to resume painting. The black/white tip remains visible on white artwork.

Each layer and group has a compact connected **Color / Mask** selector. Add levels, curves, Gaussian blur, color adjustment, invert or grayscale to the color stack. Group effects process the combined child content. The top effect runs last, with earlier effects below it. Blend mode and opacity remain visible beneath the stack. Parameters remain editable after later work; effects can be disabled, reordered or removed. Hover a blend choice for a temporary canvas preview; click to commit it. Double-click a range to type a value; arrow keys also adjust it. Values sit inside the orange bar.

Add effect stays above the scrollable stack so it remains reachable as the list grows. Tone adjustment belongs in Color effects; the brush toolbar contains brush controls.

**K** selects Smart mask. Click an artwork color, adjust Tolerance, and choose a contiguous region or all similar colors. Replace, Add and Subtract build an editable mask. Refine with a mask brush, curves or feathering. Subject/object segmentation and liquify are follow-ups.

Color effects are baked into standard PSD raster data while PeerBrush retains editable effect sources. A group with active color effects is saved as one baked standard layer, with its full editable group and children retained in PeerBrush metadata. Other editors see the current appearance; they cannot edit PeerBrush's effect stack. Blur uses a fast three-pass Gaussian approximation in the initial 8-bit pipeline. Live rendering runs on a worker; large documents can show preview latency while progressive rendering is developed.


Ctrl/Cmd-click selects multiple layer rows; Shift-click selects a visible range. W/E/R share a pivot and transform the selected raster layers as one gesture. Opacity, blend, new effects and mask controls apply to selected layers together; painting still targets the active layer. Folder-wide transforms remain a later slice. Folders use a folder glyph and heavier name. Drag onto a folder center to move inside, between rows to reorder, or left of nested rows / below the list to move to the top level. The canvas previews the new hierarchy before release.

Press an eye icon and drag vertically across other eyes to show or hide the swept layers together. Release commits one undo step; Escape cancels. Selection boundaries use animated black-and-white marching ants. **Alt+Backspace** floods the active layer or the current selection with the foreground color. **Alt+B** remains available for the selected layers. Mask mode uses the current grayscale foreground; one undo reverses a fill.

MCP starts automatically with the editor. Red **MCP · waiting for AI** means no agent is attached yet. Click **Connect AI** to register with detected Codex, Claude Desktop, Cursor and Gemini CLI clients and publish the local discovery manifest. The neighboring settings control opens advanced/manual configuration. Restart or reload a configured client to load PeerBrush; green requires a real client connection. The adapter can register while the editor is closed and reconnect when you launch PeerBrush. Keep the app open during editing calls. See [MCP and CLI](mcp.md).

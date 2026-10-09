# Getting started

Run the portable `peerbrush` executable. Open the File menu for new/open/import/export and Save As. Keyboard undo/redo works on shared edits. Rust is only needed to develop PeerBrush, not to run a packaged build.

For the Windows 0.1.4 folder, double-click **Start PeerBrush.cmd**. It uses a separate 0.1.4 workspace under Local AppData, so an older running build can keep its current document open.

The first canvas is transparent. Use the brush, eraser, fill, gradient, rectangle, ellipse, move, rectangle selection, eyedropper, and pan tools. Scroll or **Alt + right-mouse drag** to zoom around the cursor; **middle-mouse drag** or Space-drag to pan. **F** frames an active selection with a small margin, or centers and fits the full canvas when nothing is selected. The frame icon does the same.

Create regular layers or folders in the layer panel. Creating a folder gathers the selected layers into it, preserving their order and children. Click anywhere on a row to select it; use its eye button to toggle visibility. Drag the row or its dotted grip to reorder. The held row follows the pointer, neighboring rows animate into place, and the canvas previews the new order before release. Expand or collapse groups with the arrow. Right-click for duplication, locking, masks, group membership, and deletion. Double-click a layer name to rename it directly in the row. Enter or clicking away finishes the rename; Escape cancels it. No popup or confirmation is needed. Right-click **Merge** or press Ctrl/Cmd+E to merge adjacent layers or flatten a folder in one undo step. Layers with backdrop-dependent blends should be grouped and the folder merged to preserve their internal blend results.

Click the content thumbnail to paint pixels. Click the mask thumbnail to paint grayscale. The two overlapping swatches at the bottom left set foreground and background colors. **X** swaps them; mask colors remain independent from artwork colors. Click a swatch for the large hue/saturation picker, compact RGBA controls, and editable HEX values (`RGB`, `RRGGBB`, or `RRGGBBAA`). Alt-click isolates a mask; Shift-click toggles it. The mask stack supports paint, fill, invert, levels, feathering, curves, Gaussian blur and tonal adjustments. Select a paint step to edit it; use Add effect to extend the stack.

Save working documents as PSD and export PNG. Import PNG/JPEG as layers, or **drop images anywhere**. Drop one PSD to open it; unsaved work must be saved first. Ctrl/Cmd+S saves; Ctrl/Cmd+O opens; Ctrl/Cmd+Z undoes; Ctrl/Cmd+Shift+Z redoes.

Click a layer row to work with whole layers. **Ctrl/Cmd+J** duplicates the selected layers or folders; **Ctrl/Cmd+C**, **X**, and **V** copy, cut and paste them with their editable masks, effects, children and transforms. Cut removes content only after the clipboard write succeeds. A failed or stale copy cannot delete your work.

Click the canvas to work with pixels. **Ctrl/Cmd+C** copies the active layer's selection, including its visible color effects and mask. A rotated selection clips copied pixels to its exact shape. **Ctrl/Cmd+Shift+C** copies the visible composite. **Ctrl/Cmd+V** also accepts images copied in another application. A PeerBrush pixel copy retains its position; an external image is centered. Paste uses the selected folder or the selected layer's folder. At the top level it creates a folder containing the original and pasted content. Paste and any folder creation are one undo step. Text fields keep normal text shortcuts.


## Compatibility

This is a foundation release, not the complete product roadmap. Editable PSD support includes 8-bit and 16-bit RGB raster layers, nested isolated and pass-through groups, opacity, 16 pixel blend modes, raster masks and default grouped raster clipping stacks. A folder's **Pass Through** blend lets its children and adjustments affect the backdrop outside it; Normal isolates them. Folder opacity/masks control the combined change. Enabled folder effects, folder clipping and merging need an isolated blend. Native 16-bit channels remain authoritative during edits, effects, transforms, clipboard operations and PSD/PNG saving. New canvas offers **8 bit / 16 bit**. Unsupported Photoshop features and embedded profiles open as a protected saved-composite preview at the original depth. Supported RGB matrix ICC profiles display through sRGB; **Convert to sRGB copy** makes an explicitly converted editable copy at that same depth and tags its PSD/PNG output. Unsupported profiles show an unmanaged-preview warning and cannot be converted. **Edit 16-bit copy** creates a flattened editable copy that keeps channel precision and requires a new filename; the original stays untouched. PSB and unsupported color modes still report their limits. See [Photoshop fidelity and current limits](photoshop-fidelity.md).

PeerBrush embeds its editable sources in the PSD and saves normal raster/mask data for other editors. Externally changed standard PSD data invalidates embedded definitions instead of resurrecting stale content.

The renderer displays cached previews through the GPU. Incremental brush coverage, regional compositing and partial texture uploads keep gestures small; full previews use bounded parallel CPU workers. Common expensive 8-bit effects and native 16-bit HSL/color balance/adjustment use optional GPU compute, while simple operations stay on CPU when faster. Native 16-bit blur and bloom retain their precision-preserving CPU implementation. Native 16-bit samples are projected only for screen/agent previews; PNG exports retain their depth. Large-document streaming, disk eviction, full ICC color management, broader Photoshop clipping fidelity and text remain follow-up work. Consult `peerbrush_capabilities` for the exact command surface.

## Recovery and shared control

Unsaved changes generate a recovery PSD in PeerBrush's runtime folder. The AI connection settings panel offers recovery when present. Recovery is a safety copy; save explicitly to keep work permanently.

AI can reserve specific layers or rectangular regions while you edit elsewhere. Blue outlines and layer badges identify AI reservations and recent AI edits. AI opacity and transform previews ease into place, and hierarchy rows animate into their new folder positions. These are presentation transitions; the current shared document and undo history update immediately. Take back control releases reservations. Agents using the old task ID cannot keep editing under that task.


## Maya-style transforms

**Q** hides gizmos; **W** shows move; **E** shows rotation; **R** shows scale. Select a layer, then drag the colored X/Y handles or the center handle. Move/scale axis handles constrain that axis. Shift snaps rotation to 15° or makes scaling uniform. Escape cancels a drag. **D** selects the eraser.

Gizmos preview the actual pixels and apply one undoable edit on release. An active selection transforms its pixels while preserving the remainder of the layer. Its outline rotates, scales and moves with the content; subsequent painting, fills and copying respect that shape. Without a selection, the whole layer transforms. Rotation and scaling resample raster content; undo before trying another transform to retain the original pixels. Whole-group transforms remain planned.

The top bar shows green **MCP connected** when a live client is attached, red otherwise. The animated center line shows agent-supplied progress and tool activity. It is a shared activity display, not access to a model's private reasoning.

## Brush and effects

The Brush window offers 21 original brushes in **Sketch, Ink, Paint, Airbrush, Texture and Blend**. Search or choose a category, then click a readable stroke preview to load all settings. Blend presets select Smudge. Edit any tip, texture, pressure curve, taper, flow or smoothing control, then use **Save new** with a name/category. **Update** and **Delete** apply only to your custom brushes. Custom presets persist in the running instance's state folder independently of artwork and undo. The pressure curve is linear at 1; lower values respond more to light force and higher values need more force.

**[ / ]** (also shifted braces) change brush/eraser size. Hold **S** and move the pointer horizontally over the canvas to resize with a live outline and numeric readout, without painting. Open Brush settings from the small adjustment icon beside Size: hardness, opacity, flow, spacing, roundness, angle, smoothing, textured tip presets, real pressure when supplied by the platform, and start/end taper. Opacity caps each stroke; flow builds with overlapping dabs. The live stroke uses the same engine as the committed result. Hold **Alt** in Brush for a temporary eyedropper; click to sample, then release Alt to resume painting. The black/white tip remains visible on white artwork.

Each layer and group has a compact connected **Color / Mask** selector. Add levels, curves, Gaussian blur, color balance, hue/saturation, bloom, liquify, invert or grayscale to the color stack. Group effects process the combined child content. The top effect runs last, with earlier effects below it. Blend mode and opacity remain visible beneath the stack. Parameters remain editable after later work; effects can be disabled, reordered or removed. Hover a blend choice for a temporary canvas preview; click to commit it. Double-click a range to type a value; arrow keys also adjust it. Values sit inside the orange bar.

Add effect stays above the scrollable stack so it remains reachable as the list grows. Tone adjustment belongs in Color effects; the brush toolbar contains brush controls.

**K** selects Smart mask. Click an artwork color, adjust Tolerance, and choose a contiguous region or all similar colors. Replace, Add and Subtract build an editable mask. Refine with a mask brush, curves or feathering. Subject/object segmentation remains a follow-up.

Color effects are baked into standard PSD raster data while PeerBrush retains editable effect sources. A group with active color effects is saved as one baked standard layer, with its full editable group and children retained in PeerBrush metadata. Other editors see the current appearance; they cannot edit PeerBrush's effect stack. Blur uses a fast three-pass Gaussian approximation with higher-precision premultiplied working buffers in the 8-bit pipeline; native 16-bit effects have a separate precision-preserving path. Live rendering runs on workers with bounded caches; full tiled GPU compositing and progressive large-document streaming remain planned.


Ctrl/Cmd-click selects multiple layer rows; Shift-click selects a visible range. W/E/R share a pivot and transform the selected raster layers as one gesture. Opacity, blend, new effects and mask controls apply to selected layers together; painting still targets the active layer. Folder-wide transforms remain a later slice. Folders use a folder glyph and heavier name. Drag onto a folder center to move inside, between rows to reorder, or left of nested rows / below the list to move to the top level. The canvas previews the new hierarchy before release.

Press an eye icon and drag vertically across other eyes to show or hide the swept layers together. Release commits one undo step; Escape cancels. Selection boundaries use animated black-and-white marching ants. **Alt+Backspace** floods the active layer or the current selection with the foreground color. **Alt+B** remains available for the selected layers. Mask mode uses the current grayscale foreground; one undo reverses a fill.

MCP starts automatically with the editor. Red **MCP · waiting for AI** means no agent is attached yet. Click **Connect AI** to register with detected Codex, Claude Desktop, Cursor and Gemini CLI clients and publish the local discovery manifest. The neighboring settings control opens advanced/manual configuration. Restart or reload a configured client to load PeerBrush; green requires a real client connection. The adapter can register while the editor is closed and reconnect when you launch PeerBrush. Keep the app open during editing calls. See [MCP and CLI](mcp.md).


## Painting, adjustments and liquify

Brush settings include **Round / Dry / Chalk / Grain / Bristle**, texture density and grain size. **Start / End** taper lengths are measured in document pixels, with separate size and opacity toggles. Mouse strokes can taper without a tablet. Actual pressure is used only when the input platform supplies it; agent strokes can supply `[x,y,pressure]` points or a parallel `pressures` array.

**U** selects wet blending. **Wetness** controls color mixing, **Pickup** controls pigment collected from the layer, and **Load** adds foreground pigment. Load zero smudges existing artwork. Painting and smudging respect selections and masks.

The **◐** layer-header menu adds an adjustment. One selected paint layer gets a clipped adjustment above it; a selected folder gets an adjustment inside it. Multiple selected layers are gathered into a shared color folder. A canvas selection initializes the adjustment mask. Color balance has independent shadow, midtone and highlight controls. Right-click a layer for **Clip to layer below** or **Release clipping**. Keep a base and its clipped layers together when copying, deleting or reordering; incomplete operations are refused before changing the document.

**Bloom** has Threshold, Spread and Strength. **Linear dodge / Add** is available in the blend menu and previews on hover.

Color and mask effects start with settings closed. Select a row to open its settings and management buttons; select it again to close. Every row has an icon and an always-visible **Weight** from 0–100%, independent of its own parameters. Top effects run last. Dragging Weight, settings or layer opacity renders the shared engine result while held and creates one undo step on release. Escape cancels the preview. A concurrent document change also cancels it, preserving the newer work. Existing saved effects open at 100% weight.

**L** selects Liquify. Choose Push, Expand, Pinch or Restore, then brush on the artwork. The rendered warp previews during the drag and commits as one undo step. Its effect remains in the color stack; revisit Amount and individual stroke radius/strength, disable strokes by removal, reorder the effect, or undo. Folder liquify warps its composite. Color liquify is the current scope; this release does not claim Photoshop's full face-aware liquify feature set.

PeerBrush saves supplementary editable effect sources alongside standard PSD pixels. Default grouped raster clipping stacks and raster-only pass-through folders remain separate standard layers with ordinary divider/clipping flags and masks. Documents with adjustment layers or clipping involving folders still contain a standard baked composite layer, while PeerBrush restores the full editable tree when the standard data matches. Other editors cannot edit PeerBrush's private effect sources. Externally modified or unsupported sources open protected rather than silently discarding features. See [Photoshop fidelity](photoshop-fidelity.md).


## Selection tools and refinement

Press **M** or click Select in the toolbar. The selection toolbar offers rectangular and elliptical marquees, single rows/columns, freehand and polygonal lassos, Magnetic Lasso, Magic Wand, Quick Selection and Object region. Choose replace, add, subtract or intersect; hold Shift to add, Alt to subtract, or both to intersect. Feather sets soft edge coverage. Wand can sample the composite or active layer and select contiguous or matching colors. Quick Selection paints color-connected regions within the brush radius. Magnetic Lasso snaps to nearby rendered luminance edges; click to begin, follow the edge, and press Enter or double-click to close. Polygonal Lasso uses clicked anchors. Backspace/Delete removes the last anchor; Escape cancels the unfinished path.

**Ctrl+D / Select > Deselect** clears the pixel selection, even when layer rows have focus. **Ctrl+Shift+D** reselects it; **Ctrl+A** selects the canvas. Clicking with a marquee or freehand lasso without dragging clears a selection in replace mode. Text inputs keep their normal shortcuts. Select also offers Inverse, Expand and Contract. Expand/Contract use square neighborhoods. Empty intersections remain active empty selections and cannot accidentally expose the whole canvas to painting.

Selections retain disconnected regions, holes and feather coverage through painting, copying and selected-region transforms. Color-connected Quick Selection and perimeter-based Object region are deterministic helpers, not Photoshop's learned object segmentation. Raster selection operations currently support canvases up to 16 megapixels; boundary and brush work budgets report an error instead of truncating a selection.

**Select → Refine selection** previews the cutout, confidence mask or overlay while changing Smooth, Feather, Shift edge, Contrast and Follow image edge. Apply commits one edit; Cancel keeps the original selection. **Mask from selection** maps the selection into the selected layer's mask frame; **Refine layer mask** retains previous editable steps and native 16-bit mask values. Optional **Select subject** and **Select subject in region** use a local learned provider configured outside the application. See [provider setup and commands](segmentation.md).

Levels provides a black-to-white gradient with draggable black, midpoint and white handles. Curves uses a smooth, shape-preserving curve for newly created or edited effects: double-click to add points, drag to shape, and right-click an interior point to remove it. Older saved curve sources retain their original linear interpolation until edited.

To edit a Liquify effect, choose **Edit on canvas** in that effect's controls. Choose Push, Expand, Pinch or Restore, radius and strength above the canvas, then paint. Each stroke remains editable in its chosen effect and captures the current selection. **Add Mask** creates a white mask by default; the connected **Color / Mask** buttons select the channel.

AI reservations cover the affected layers or regions. Reserved layers show blue names and an **AI** tag, including children of reserved folders. Unreserved layers remain editable. Eye toggles remain available during reservations, and visibility-only changes do not invalidate an agent's pixel revision. Mixed commands that also alter opacity, names or pixels still enforce reservations.

Right-click the bottom-left dimensions/depth readout and choose **Project dimensions and depth**. Canvas dimensions preserve layer pixels and positions, including pixels outside the canvas. Choosing 8 bit explicitly quantizes 16-bit channels; Undo restores their original precision. Editable effects and native masks remain in the document. Normal PSD saves include native layer/mask channels and a current merged composite. Unsupported Photoshop documents remain read-only unless explicitly flattened at their original precision.

**C** selects Clone and **J** selects Heal. Alt-click to choose a source; paint with the shared brush controls. Aligned keeps the sampling offset across strokes; Merged samples visible artwork. Healing adapts sampled texture to the destination tone and preserves alpha. Both tools support soft selections, masks, live pixels and one undo per gesture. See [clone/heal commands and behavior](retouch.md).

The **Image** menu opens **Crop**, **Canvas size** with nine anchors, and **Image size** with proportional scaling. Each previews actual pixels and dimensions; Apply is one undo step, while Cancel/Escape preserves the document. Crop/canvas resizing retain off-canvas artwork; image resizing resamples native-depth pixels and masks. See [geometry controls and commands](geometry.md).

The **Layer** menu creates **New text** and **New vector shape** layers. Edit their text/font/alignment or shape/path/fill/stroke properties with actual canvas previews, then Apply once. Return through **Edit text/vector** or the layer context menu. Source color painting requires explicit **Rasterize text/vector**; masks and effects remain editable. Whole-layer transforms and layer clipboard preserve properties. PSD saves also contain current standard raster pixels. See [editable sources and limits](editable-sources.md).

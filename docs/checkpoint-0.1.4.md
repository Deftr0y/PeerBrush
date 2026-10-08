# PeerBrush v0.1.4 — Native precision and selections

This checkpoint brings together native 16-bit editing, textured and tapered brushes, wet blending, editable liquify, clipped adjustments, color balance, hue/saturation, bloom, expanded blend modes and regional brush previews. UI, MCP and CLI edits use the same engine and reservation checks.

![Native PeerBrush workspace](images/workspace.png)

## Selection and editing workflows

Rectangular and elliptical marquees, single rows/columns, freehand/polygonal/magnetic lassos, Wand, Quick Selection and Object region share document-coordinate selection coverage. Add, subtract and intersect retain disconnected regions, holes and feathered edges. Deselect/reselect and square-neighborhood expand/contract are undoable. Empty selections remain active and cannot expose the entire canvas to painting.

Quick Selection and Object region are deterministic color/edge helpers. Learned subject/object segmentation and richer edge refinement remain planned. Raster selection work is bounded to 16 megapixels and reports limits explicitly.

Levels has draggable black/midpoint/white handles. Newly created or edited curves use shape-preserving smooth interpolation; older linear curve sources remain linear until edited. Liquify can target a chosen editable effect and snapshots the active selection for each stroke. Ctrl/Cmd+J duplicates layers; Ctrl/Cmd+D deselects and Ctrl/Cmd+Shift+D reselects. Project dimensions and explicit 8/16-bit conversion are one undoable engine operation; converting to 8 bit intentionally quantizes samples.

## Native 16-bit PSD and PNG

Supported RGB PSD layers, masks, internal clipboard, painting, transforms, effects and exports retain native 16-bit samples. Screen and agent previews project to 8 bit at the output boundary. Every PSD save includes ordinary raster/mask channels and a current merged composite alongside editable PeerBrush sources.

Private source format 6 protects soft selection coverage, smooth curves and selection-aware liquify from older readers that cannot interpret them. Compatible readers restore sources only when standard PSD hashes match. Unsupported Photoshop structures remain read-only; an explicit editable copy flattens structure at the original precision and requires a new save path. PSB, full ICC management, editable Photoshop text/smart objects/vectors and pass-through group fidelity remain outside this checkpoint.

## Rendering and verification

Retained brush sessions, regional compositing and partial preview uploads reduce effect-free gesture work. Full CPU previews use bounded workers. Optional GPU compute accelerates supported expensive 8-bit effects and native 16-bit HSL/color balance/adjustment; native 16-bit blur/bloom remain on CPU. The capability catalog reports the actual adapter and fallback status. Full tiled GPU compositing and progressive large-document streaming remain planned.

A release-mode native 16-bit measurement of five successive brush updates in a nine-layer scene, with a 1536-pixel preview, took 361 ms with full replay/render versus 106 ms with retained strokes and regional rendering on a 2048-square canvas. The 4096-square scene took 335 ms versus 88 ms. All five retained updates used partial rendering and matched the shared-engine result. These are local measurements of effect-free previews, not a guarantee for every project; effects-aware incremental work remains queued.

Windows verification passed all 252 engine/codec/protocol/UI regression tests (five opt-in checks are excluded from that count), three explicit actual-device GPU parity/failure checks on the NVIDIA GeForce RTX 3070 Ti Laptop GPU/Vulkan, independent standard PSD/PNG channels, save/reopen, undo and native workspace captures. All eleven original Hippo source hashes remained unchanged: Egg retains three editable native 16-bit layers; the other ten remain protected and support explicit native-precision copies. The formatting check also passed. Native macOS/Linux checks and Clippy depend on CI; Clippy is not installed in this isolated Windows toolchain.

The Windows portable package includes matching project source, GPL v3 and dependency/font notices. Requested tool artwork, wordmark and curated native screenshots accompany the app. Temporary QA files, development tools, credentials and recovery files are excluded. Cross-platform CI is configured; native macOS/Linux verification remains outstanding.

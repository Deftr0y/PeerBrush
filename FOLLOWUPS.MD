# PeerBrush follow-ups

This is the living backlog for ideas raised during development. Add new requests here as they arrive, keep the user's intent, and move them to the appropriate milestone. Update status only after implementation and verification.

## Current release

| ID | Feature | Status | Acceptance |
| --- | --- | --- | --- |
| PB-001 | Green/red MCP connection text in the top bar | Implemented and verified | Green when a live MCP client is connected, red when none is connected; do not confuse a listening server with a connected client. |
| PB-002 | Centered, one-line animated AI activity text in the top bar | Implemented and verified | Agent-authored natural-language progress updates; smooth text transitions; no code or claims of access to hidden reasoning. |
| PB-003 | Drop files into the editor | Implemented and verified | A PSD opens safely; PNG/JPEG drops create undoable layers; multiple image imports are atomic. |
| PB-004 | Maya-style Q/W/E/R transform gizmos | Implemented and verified | Q hides gizmos, W moves, E rotates, R scales; axis constraints and one undo entry per drag. |
| PB-005 | Human and agent undo/redo; artistic iteration | Implemented and verified | Visual feedback after agent undo/redo; agents inspect, undo, and retry; preserve interleaved human edits. |
| PB-006 | Maya-style navigation | Implemented and verified | Wheel and Alt + right drag zoom at the cursor; middle drag pans. |
| PB-007 | Ubuntu/MayaMCP visual styling, modern typography, coordinated icons and P logo | Implemented and visually checked; continue iterating from feedback | Preserve the current UX; Ubuntu Sans typography, readable white tool glyphs and the geometric MayaMCP-style P logo. |
| PB-008 | F frames selection or canvas | Implemented and verified | An active selection is centered with a small margin; otherwise reset pan and fit the canvas. |
| PB-009 | Compact ranges and no unnecessary boxes | Implemented and verified | Keep numeric values unboxed; consolidate range controls and avoid decorative containers, borders, and wasted space. This is a standing UI preference. |
| PB-010 | Mask feathering and effect previews | Implemented and verified | Non-destructive cached softness, visual stack previews, shared UI/MCP commands, and standard PSD mask pixels. |
| PB-011 | One undo step per parameter gesture | Implemented and verified | A slider drag is one edit; coalescing stops across another participant's edits. |
| PB-012 | White tool icons and two-color palette | Implemented and verified | Readable white glyphs; overlapping foreground/background swatches at bottom left; X swaps colors, including mask grayscale colors. |
| PB-013 | Expanded brush controls | Implemented and verified | Size, hardness, opacity, flow, spacing, roundness, angle, smoothing and useful presets; shared UI/MCP behavior. |
| PB-014 | Reliable layer clicks, visibility and basic tools | Implemented and verified | Whole-row layer selection; eye clicks toggle visibility independently; tools honor masks and selections. |
| PB-015 | Useful interaction animation | Implemented and verified | Held layer follows the pointer, visibly snaps into its destination, and surrounding rows move smoothly; subtle tool and range transitions. |
| PB-016 | Blend-mode hover preview | Implemented and verified | Hover modes to preview on the canvas; commit only on click; leaving the menu restores the current blend without history changes. |
| PB-017 | Editable color and mask effect stacks on layers and groups | Implemented and verified | Separate stacks; editable parameters after other edits; levels, curves, blur and color adjustments first; preserve editable sources in PSD and bake current standard pixels. |
| PB-018 | Non-destructive liquify | Planned next effect slice | Editable distortion map and dedicated visual warp controls, shared with agents. |
| PB-019 | Smart masking tools | Color/contiguous tools verified; subject segmentation planned | Color-range and contiguous-region masking first, with add/subtract and refinement; subject/object segmentation later needs a model backend. |
| PB-020 | Actual live feedback everywhere | Implemented and verified; standing rule | Render strokes, W/E/R transforms, blend hover, effects and dragged layer order through the shared engine. Finish gestures as one undoable edit. |
| PB-021 | Interactive tool size shortcuts | Implemented and verified | [ / ] (including shifted braces) change brush/eraser size; S + horizontal drag changes size with a live tip outline and readout, without painting. |
| PB-022 | Better color picker with editable HEX | Implemented and verified | Large saturation/value field, hue rail, original/current swatches, compact RGB/alpha controls and live HEX editing, including fill layers. |

| PB-023 | Reliable W/E/R/Q after clicking controls | Implemented and verified | Tool shortcuts survive ordinary button, layer and slider focus; text fields keep typing; physical keys support non-Latin layouts. |
| PB-024 | Copy selection and paste into a new layer | Implemented and verified | Ctrl/Cmd+C copies the selected layer area; Ctrl/Cmd+Shift+C copies the composite. Pasting stays in the current folder; a root layer and paste get a new folder. Preserve selection coordinates; one undo restores the previous document. |
| PB-025 | Paste OS clipboard images | Implemented and verified | Ctrl/Cmd+V accepts real image clipboard pixels, including image-only clipboards. External images are centered; text fields retain normal text paste. |
| PB-026 | Keep Add effect visible in long stacks | Implemented and verified | Fixed Add effect control and bounded scrolling for color and mask stacks, including small windows. |

| PB-027 | Remove Adjust layer from brush toolbar | Implemented and verified | Keep brush settings focused on brush behavior; tone adjustments live in editable effect stacks. |

| PB-028 | Multiple layer selection and shared editing | Raster multi-edit verified; group transforms planned | Ctrl/Cmd and Shift selection; shared W/E/R pivot for raster layers; atomic opacity, blend, effect and mask edits. |
| PB-029 | Distinct folder rows and drag into/out of folders | Implemented and verified | Folder glyph and stronger name, cycle/lock checks, live hierarchy canvas preview and one undo step. |
| PB-030 | Sweep layer eyes to show/hide | Implemented and verified | One consistent visibility value while dragging vertically across eyes, live feedback, one undo on release and Escape cancellation. |
| PB-031 | Photoshop-style selection boundary | Implemented and verified | Animated alternating black/white dashes, fixed screen thickness and bounded painting at large zoom. |
| PB-032 | Remove Undo/Redo toolbar buttons | Implemented and verified | Keep keyboard and MCP history controls. |
| PB-033 | Alt+B foreground fill | Implemented and verified | Fill selection or selected layers; honor masks, current foreground and shared history. |

| PB-034 | Contrast-safe brush cursor | Implemented and verified | Black and white tip outline and center marker remain readable on light, dark and tiny-zoom canvases. |
| PB-035 | True foreground/background color swap | Implemented and verified | X swaps both stored colors rather than copying; keep artwork and mask palettes independent. |
| PB-036 | Faster brush strokes | Implemented and benchmarked; incremental/GPU follow-up | Tile-based coverage and one destination COW access per tile; preserve byte-identical stroke rendering; measure on medium/large canvases. Full incremental/GPU compositing is a later slice. |
| PB-037 | Alt temporary eyedropper | Implemented and verified | Holding Alt in Brush samples composite artwork or the selected mask without painting; releasing restores Brush; Alt + right zoom remains available. |
| PB-038 | Reliable MCP setup and reconnection | Implemented and verified | Explain listener versus client, provide Codex setup, initialize/catalog while editor is closed, reconnect when it opens, clear presence at EOF. |
| PB-039 | Agent image placement | Implemented and verified | One-call placement from absolute PNG/JPEG path or base64 PNG, explicit pixel rectangle, new/current layer, editable masks/effects retained, undo and cropped PNG feedback. |
| PB-040 | Blue AI activity markers | Implemented and verified | Blue identifies recent AI edits and reservations on affected layers, tools, controls and canvas areas; human interaction keeps Ember orange. |
| PB-041 | Animated AI edits | Initial animation coverage verified; extend with tools | Ease numeric controls and actual canvas opacity/move feedback, lerp hierarchy rows including folder indentation, transition discrete rendered results. Shared state commits immediately; human edits interrupt presentation animation. Extend animation coverage as tools grow. |
| PB-042 | Refine P logo toward MayaMCP | Awaiting actual MayaMCP M reference | Crisp geometric orange/ivory/plum P, genuine transparency, clear at toolbar and app-icon sizes. |

## Next development slices

- Full stroke-aware incremental previews and progressive large-document rendering.
- Editable text and shape source layers, non-destructive transforms, group/selection transforms, crop/resize, clone/heal, and nonrectangular selections.
- Selective task undo that preserves interleaved human changes.
- Tiled GPU compositing, cache budgets, disk eviction, and progressive large-file decoding.
- ICC color management, clipping, pass-through group fidelity, and broader PSD compatibility.
- Signed cross-platform portable packages; remote connections and plugin architecture later.

## Future refinements

- Clear obsolete reservation errors after takeover/release; keep current participant activity readable.
- Extend AI tool highlighting to every new operation and refine transition timing from user feedback.

## Working rule

Keep the initial editing/PSD/MCP slice usable before adding breadth. Prioritize reliability and human–AI cooperation. New feature ideas do not replace the active implementation task unless the user explicitly says so.

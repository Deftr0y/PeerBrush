# PeerBrush follow-ups

This is the living backlog for ideas raised during development. Add new requests here as they arrive, keep the user's intent, and move them to the appropriate milestone. Update status only after implementation and verification.

## Completed checkpoint

| ID | Work | Status | Acceptance |
| --- | --- | --- | --- |
| PB-074 | Reconcile the shared source, verify and publish the Windows checkpoint | Implemented, verified and published — v0.1.4 | Preserve concurrent work. Freeze the final source, rerun engine/codec/protocol/UI and actual GPU checks, verify native 16-bit PSD/PNG and undo/reopen, fix README/release links, package matching source plus GPL/dependency notices, commit and push, publish the portable Windows build and return its download link. |

The reconciled 0.1.4 checkpoint passed 252 regression tests, three actual-device GPU checks, native painting/MCP/CLI and independent 16-bit PSD/PNG checks, and all eleven Hippo PSD checks with original hashes unchanged. [Windows portable download](https://github.com/Deftr0y/PeerBrush/releases/download/v0.1.4/PeerBrush-Windows.zip) and [matching source](https://github.com/Deftr0y/PeerBrush/releases/download/v0.1.4/PeerBrush-source.zip) are published from commit `8adadb8`. Archive integrity, committed source, licensing/artwork, binary hashes and the packaged native16 CLI were verified. See [checkpoint verification](docs/checkpoint-0.1.4.md). The earlier 0.1.3 candidate remains historical validation documentation.

See [development notes](docs/development.md) for the engine and validation workflow. The planned effects-stack/weight/icons, Procreate brush library and transform-toolbar requests remain in the backlog.

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
| PB-018 | Non-destructive liquify | Implemented and verified | Editable distortion map and dedicated visual warp controls, shared with agents. |
| PB-019 | Smart masking tools | Color/contiguous tools verified; subject segmentation planned | Color-range and contiguous-region masking first, with add/subtract and refinement; subject/object segmentation later needs a model backend. |
| PB-020 | Actual live feedback everywhere | Implemented and verified; standing rule | Render strokes, W/E/R transforms, blend hover, effects and dragged layer order through the shared engine. Finish gestures as one undoable edit. |
| PB-021 | Interactive tool size shortcuts | Implemented and verified | [ / ] (including shifted braces) change brush/eraser size; S + horizontal drag changes size with a live tip outline and readout, without painting. |
| PB-022 | Better color picker with editable HEX | Implemented and verified | Large saturation/value field, hue rail, original/current swatches, compact RGB/alpha controls and live HEX editing, for artwork and mask colors. |

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
| PB-036 | Faster brush strokes | Implemented and benchmarked; incremental/GPU follow-up | Tile-based coverage and one destination COW access per tile; preserve byte-identical stroke rendering; measure on medium/large canvases. Retained Session/dirty previews are now implemented; full GPU compositing remains a later slice. |
| PB-037 | Alt temporary eyedropper | Implemented and verified | Holding Alt in Brush samples composite artwork or the selected mask without painting; releasing restores Brush; Alt + right zoom remains available. |
| PB-038 | Reliable MCP setup and reconnection | Implemented and verified | Explain listener versus client, provide Codex setup, initialize/catalog while editor is closed, reconnect when it opens, clear presence at EOF. |
| PB-039 | Agent image placement | Implemented and verified | One-call placement from absolute PNG/JPEG path or base64 PNG, explicit pixel rectangle, new/current layer, editable masks/effects retained, undo and cropped PNG feedback. |
| PB-040 | Blue AI activity markers | Implemented and verified | Blue identifies recent AI edits and reservations on affected layers, tools, controls and canvas areas; human interaction keeps Ember orange. |
| PB-041 | Animated AI edits | Initial animation coverage verified; extend with tools | Ease numeric controls and actual canvas opacity/move feedback, lerp hierarchy rows including folder indentation, transition discrete rendered results. Shared state commits immediately; human edits interrupt presentation animation. Extend animation coverage as tools grow. |
| PB-042 | Refine P logo toward MayaMCP | Interlocking P accepted provisionally; further refinement planned | Crisp geometric orange/ivory/plum P, genuine transparency, clear at toolbar and app-icon sizes. |

| PB-043 | One-click local MCP registration and discovery | Implemented and verified | Register detected Codex/Claude/Cursor/Gemini clients, preserve settings, publish token-free manifest; green only for a real connection. |
| PB-044 | Merge layers and folders | Implemented and verified | One undoable bake, preserve masks/effects/current appearance; guard gaps, locks, stale commits and backdrop-dependent blends. |
| PB-045 | Top effect runs last | Implemented and verified | Reverse display and reorder directions for both stacks without reinterpreting existing saved sources. |
| PB-046 | Whole-layer duplicate/copy/cut/paste | Implemented and verified | Ctrl/Cmd+J/C/X/V for selected trees, editable masks/effects, efficient COW snapshots, OS-image fallback and safe cuts. |
| PB-047 | Inline layer renaming | Implemented and verified | Double-click edits in the row, Enter/blur commits, Escape cancels; no popup or confirmation. |
| PB-048 | Connected Color/Mask and bottom layer controls | Implemented and verified | Compact connected selector; blend and opacity below effects; Add effect remains reachable. |
| PB-049 | Regular layers and folders; Alt+Backspace flood | Implemented and verified | Remove fill-layer creation from the UI, preserve existing files, fill active layer/selection with foreground through the shared engine. |
| PB-050 | Selection follows its transformed content | Implemented and verified | Rotated/scaled/moved outline and exact selection clipping for paint, fill and copy; undo restores geometry and pixels. |
| PB-051 | New folder contains selected layers | Implemented and verified | Gather selected roots and all descendants in order, atomic lock/reservation checks and one undo. |

| PB-052 | Textured brush tips | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-053 | Pressure and taper along strokes | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-054 | Smudge and wet blending | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-055 | Color balance and hue controls | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-056 | Clipped adjustment layers | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-057 | Dedicated bloom | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-058 | Expanded blend modes including Linear dodge / Add | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |
| PB-059 | Editable liquify | Implemented and verified | Shared engine, live rendered previews, reversible edits and UI/MCP/CLI access. |

| PB-060 | Open the Hippo PSD | Implemented and verified | All eleven originals unchanged. Egg has three editable 16-bit layers; other ten open protected with an explicit native-precision editing copy. |
| PB-061 | Smooth tools on medium/large canvases | In development; effects-aware regional previews verified (PB-075) | Profile actual release builds; reduce preview work, measure timings, and progress toward GPU editing. Portable packaging is not a speed constraint. |

| PB-062 | Preserve 16-bit color through editing and saving | Implemented and verified | Native COW samples, brush/transform/effects/masks/clipboard, PSD16 and PNG16; display/agent previews project only at the boundary. |

## Next development slices

The remaining technical slices precede the user's ordered feature list:

| ID | Work | Status | Acceptance |
| --- | --- | --- | --- |
| PB-075 | Effects-aware incremental previews | Implemented and verified | Retain gesture-local color/mask results and re-evaluate padded filter windows for color effects, nested folders and clipped adjustments at 8/16 bits. Handle isolated mask edge clamping; reset on layout/settings/revision changes and errors; keep global caches, sources and history unchanged. CPU/GPU padded windows match full renders. Native filtered painting and one-step undo restore exact PNG16 output. |
| PB-076 | Tiled GPU compositing | Implemented and verified; broader GPU modes use CPU fallback | Downsampled 8-bit normal layers/isolated folders use a 64 MiB derived tile atlas, bounded output/staging/metadata and one batched readback. Shared CPU sampling, COW/undo identities and atomic failure recovery; native16, masks, clipping, adjustments, other blends and full-resolution saves retain CPU rendering. Device parity, eviction, latency and native painting/undo verified. Caches remain outside sources/history. |
| PB-077 | Folder transforms | Planned | Transform selected roots and descendants with a shared pivot, preserve masks/effects and native precision, actual shared-engine previews, atomic lock/reservation checks and one undo per gesture. |
| PB-078 | Selective task undo | Planned | Revert agent tasks while preserving interleaved human work, expose precise scope/conflicts and keep subsequent undo/redo coherent through the shared engine. |

Then develop the following slices in the user's requested priority order:

| Priority | ID | Feature | Status | Acceptance |
| --- | --- | --- | --- | --- |
| 1 | PB-063 | Better selections and masking | Implemented; advanced refinement remains planned | Rectangular/elliptical/row/column marquees, freehand/polygon/magnetic lassos, Wand, Quick Selection, deterministic Object region, add/subtract/intersect, feather, expand/contract, deselect/reselect and coverage-aware edits share the engine. Raster selection work is bounded to 16 megapixels. Learned subject/object segmentation and richer edge refinement still need a backend. |
| 2 | PB-064 | Clone and heal tools | Planned | Cleanup and retouching with shared human/AI controls, explicit source coordinates, selection/mask support, live rendered previews and undoable gestures. |
| 3 | PB-065 | Crop, canvas resize and image resize | Planned | Clear anchors, explicit document dimensions and coordinates, live previews and reversible changes through the shared engine. |
| 4 | PB-066 | Editable text and vector shapes | Planned | Preserve editable source properties alongside current standard PSD raster output and merged composite; shared UI/MCP/CLI editing. |
| 5 | PB-067 | Non-destructive transforms | Planned | Retain original pixels so repeated rotation and scaling do not degrade artwork; render the actual result live, preserve masks/effects and finish each gesture as one undo step. |
| 6 | PB-068 | Large-document handling | Planned | Progressive loading/rendering, bounded tile caches, cache budgets, disk eviction and progressive large-file decoding; derived caches stay outside history and editable sources. |
| 7 | PB-069 | Better AI collaboration | Planned | Visible proposed edits for human acceptance, clearer task scope and graceful recovery from interrupted work; preserve human changes, respect reservations and show AI work in blue. |
| 8 | PB-070 | Color management and stronger Photoshop fidelity | Planned | ICC profiles, pass-through groups, broader clipping support and broader PSD compatibility; precise unsupported-feature explanations and protected read-only fallback. |

All slices preserve native 16-bit precision, shared engine/reservation rules, current standard PSD raster/mask data and merged composites, and the compact Ember UI. Verify behavior changes with engine/codec/protocol tests and the native workspace.

Signed cross-platform portable packages remain planned. Remote connections and plugin architecture follow once the local editor is dependable.

## Additional requested work

These requests are queued for later development; they do not replace the active implementation task or change the priority order above.

| ID | Feature | Status | Acceptance |
| --- | --- | --- | --- |
| PB-071 | Compact, selectable effects stacks with universal weight and icons | Planned | Make color and mask stacks much smaller and easier to read, select and manage. Effect settings/tools stay closed by default and open for the selected effect. Every effect has an always-visible Weight control, defaulting to its full contribution, independent of effect-specific parameters. Give every existing and future effect an intuitive, recognizable icon in stack rows and effect menus. Preserve top-runs-last order, compact unboxed ranges, live shared-engine rendering, reversible edits and native 16-bit precision; weights must survive clipboard and PSD sources while standard saved pixels reflect the current result. |
| PB-072 | General brush improvements and a better preset library | Planned | Improve painting behavior and preset browsing/organization using Procreate's brush workflow as a reference. Provide a richer, curated library with readable stroke previews, useful brush categories, easy preset selection and editable/custom presets. Refine tip/texture behavior, pressure response, taper, smoothing, flow and wet blending as needed. Preserve shared UI/MCP/CLI behavior, live rendered strokes, one undo step per gesture and native 16-bit precision. |
| PB-073 | Compact transform controls at the bottom right | Planned | Remove transform buttons from the left toolbar. Replace the bottom-right "You and your AI" text with small transform buttons/indicators, showing the active Q/W/E/R mode. Keep keyboard shortcuts, tooltips, actual live transform previews and one undo step per gesture. Preserve the compact Ember UI and blue AI activity feedback. |

| PB-079 | Multiple projects in tabs with cross-project editing and independent AI work | Planned | Keep multiple projects open at once in tabs. Copy/paste elements between projects and drag layers between tabs/projects, preserving editable layer trees, masks/effects and native precision. Keep each project's active document state, selection, undo/redo, dirty state and save destination separate. Let AI work in an explicitly targeted project while the user works in another; expose stable project IDs in AI tools and responses, and keep targeting independent of the visible tab. Reject missing, closed or stale targets without changing another project. Cross-project operations must be undoable in each affected document and must not cause unintended edits or save-state changes elsewhere. |
| PB-080 | Clear project lifecycle dialogs and unsaved-work protection | Planned | Provide clear prompts and feedback for opening, loading, importing, saving, closing tabs and exiting. Before replacing or closing a project with unsaved edits, offer Save / Don't Save / Cancel, including "Save project before opening?" and "Save project before exiting?" where applicable. Proceed after Save only when it succeeds; a canceled or failed save keeps the document and edits intact. Canceled open/import flows and missing, unreadable or invalid files preserve current work and show useful feedback. Track dirty state per project; closing a tab checks that project, and app exit accounts for every dirty document with clear save destinations and a safe cancellation path. |

## Future refinements

- Clear obsolete reservation errors after takeover/release; keep current participant activity readable.
- Extend AI tool highlighting to every new operation and refine transition timing from user feedback.

## Working rule

Keep the initial editing/PSD/MCP slice usable before adding breadth. Prioritize reliability and human–AI cooperation. New feature ideas do not replace the active implementation task unless the user explicitly says so.

# PeerBrush v0.1.1 checkpoint

This checkpoint adds a usable shared editing workspace: compact brush/color controls, editable layer/folder color and mask effect stacks, multiple layer selection, live transforms and hierarchy previews, clipboard images, and human/agent history.

AI can place generated/edited pixels from a local file or base64 PNG directly into an exact document rectangle on a new or existing layer. Blue identifies recent edits and reservations. Opacity, position, fill colors, effect/mask values and hierarchy rows animate in the UI; discrete canvas edits transition with premultiplied crossfades. The committed document updates immediately, and presentation values never enter history or PSD sources. Human input interrupts animation.

MCP starts with the application. The stdio adapter can initialize/list tools while the editor is closed and reconnect when it opens. The Connect AI panel offers configuration matching its runtime workspace. Codex registration uses the same executable and state directory as the running editor.

## Validation

- 73 automated tests passed on Windows; the separate native Windows image-clipboard test also passed during this development slice.
- Byte-for-byte brush comparison covers varied strokes, ellipse tips, softness, smoothing, flow/opacity, erasing and clips; pathological work leaves pixels unchanged.
- Native executable checks cover HTTP authentication, shared MCP/CLI state, late adapter reconnection, EOF cleanup, singleton behavior, cropped image payloads, placement/undo and blue opacity frames.
- Independent psd-tools reconstruction matched the standard opaque layer composite within one channel value; merged export matched PNG pixels. Transparent alpha matched exactly, with premultiplied color differences no greater than two.
- The previous 0.1.0 executable opened new format-3 PSDs as read-only merged previews instead of dropping editable effects.
- Actual native workspace, settled brush/color panels and high-contrast tip cursor were visually checked.

## Measured brush performance

Release benchmark on this Windows machine, using the same 128-point round stroke (radius64, hardness0.65, flow0.4, opacity0.7); median of three runs. These are fixture timings, not a frame-rate guarantee.

| Canvas | Previous stroke | Current stroke | Time reduction |
| --- | --- | --- | --- |
| 2048 × 2048 | 67.93 ms | 42.75 ms | 37% |
| 4096 × 4096 | 69.08 ms | 39.72 ms | 43% |

Coverage stores one alpha byte per pixel rather than four RGBA bytes. Tip trigonometry is prepared once, solid/outside areas skip expensive math, and each touched destination tile gets one copy-on-write access. Rendering remains equivalent to the prior implementation.

Five full 1536-edge preview updates improved roughly 5–6%; CPU compositing still dominates. The native UI now chooses preview resolution from the visible canvas in device pixels, capped at1536. Full incremental brush previews, cropped viewport rendering and GPU compositing remain follow-ups. Reproduce with `cargo run --release --example brush_performance`.

## Remaining scope

Liquify, model-based subject/object segmentation, full group transforms, selective task undo, text/vector layers, broader PSD fidelity and full color management remain on the roadmap. Native macOS/Linux behavior awaits CI and platform testing. The next logo revision is awaiting the actual MayaMCP M reference; rejected candidates have not replaced the previously selected app mark.

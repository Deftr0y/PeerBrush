# PeerBrush 0.2

Version 0.2 uses internal version 0.2.0. This is the release candidate summary; published downloads and their exact source commit are listed on [GitHub Releases](https://github.com/Deftr0y/PeerBrush/releases).

## Changes since 0.1.4

- Independent project tabs, guarded cross-project layer transfers and unsaved-work review for closing projects or exiting.
- Native-depth retained raster transforms, folder transforms, clone/heal, crop/canvas/image resize, text and editable vector sources.
- Expanded selections and refinement, an optional external learned selection provider, and selections that remain independent of masks. Ctrl+D duplicates the current layer selection; DEL deletes selected layers; Ctrl+G groups and Ctrl+Shift+G merges.
- Common raster image imports and supported static SVG/SVGZ, explicit animation/page/icon choices, Windows image-file paste and native-depth clipboard operations.
- Searchable categorized effects, compact effect weights, Posterize and channel clamp, a brush library, whole-image filters and a searchable preset library. Double-click sliders to type exact values.
- Clear folder hierarchy guides, full-row layer context menus and the connected Add mask/Color/Mask selector. Transformed content remains editable throughout the canvas.
- Broader GPU previews for 8/16-bit blend, mask, clipping, folder and prepared-adjustment stacks. Bounded caches and shared-renderer fallback protect source precision; larger workloads can remain faster on CPU.
- Standard RGB PSD clipping, pass-through folders and full layer/folder locks. Matrix/TRC and bounded classic RGB LUT ICC previews, native-depth import normalization and explicit sRGB copies preserve protected originals.
- Guarded live AI code editing, proposals, reservations, visible blue feedback, cancellation/takeover and selective task undo that preserve interleaved human work.

## Limits and verification

PSD v1 RGB at 8/16 bits is the supported editable subset. Unsupported Photoshop settings remain read-only; this is not full Photoshop compatibility. Partial layer locks, Blend If, fill opacity, knockout, newer ICC LUT structures, CMYK, HDR/32-bit and PSB remain unsupported. Editable vector projection uses RGBA8 coverage without quantizing existing native16 raster sources. See [Photoshop fidelity](photoshop-fidelity.md) and [image imports](image-import.md).

Engine, codec, HTTP/stdio protocol and UI regression tests passed locally. Native Windows captures and independent standard PSD16/PNG16 decoding verified current rendered results, low-bit preservation, undo and editable reopening. Actual GPU parity checks passed on the available Windows device. The local Windows clipboard transport check was excluded because its protection gate refused existing clipboard contents; fresh-runner CI keeps that check enabled. Platform build results and package checksums must be verified for the exact published commit. macOS/Linux manual desktop interaction remains unverified.

Signing configuration and package verification are described in [release packages](release-packages.md). Sigstore archive signatures, when supplied, do not remove native Windows/macOS publisher warnings.

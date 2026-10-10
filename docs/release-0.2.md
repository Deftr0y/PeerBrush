# PeerBrush 0.2

Version 0.2 uses internal version 0.2.0. The [published experimental prerelease](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.2) was released on 2026-10-10 from `2e1791ffc02584103f7a8f1824ef7668a1b19589`. Windows x64, Linux x64 and macOS arm64 packages contain identical corresponding source; their archive checksums are recorded in the release.

## Packaging correction — 2026-10-10

Corrected archives remove duplicated website-only trailers, demo artwork and showcase images. The released 0.2.0 executables and release tag are unchanged. All application/build inputs in the corrected corresponding source match the original build byte-for-byte. Required GPL source, application artwork/fonts, user guides and third-party notices remain included. All packages remain unsigned.

Archive/source policy commit: [`0eb25310d37976dc6fd655bf960b89686204a569`](https://github.com/Deftr0y/PeerBrush/commit/0eb25310d37976dc6fd655bf960b89686204a569). `release.json` distinguishes this packaging/source revision from the original `binary_source_commit`. The same source ZIP is embedded in all three packages.

| Download | Bytes | SHA-256 |
| --- | ---: | --- |
| [PeerBrush-0.2.0-Linux-x64-repacked-20261010.zip](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/PeerBrush-0.2.0-Linux-x64-repacked-20261010.zip) | 29010659 | `f31dd45b5f96ce15ae10a5bd1221ff81a6fa3f9ca8d08cdc6b2744934c203156` |
| [PeerBrush-0.2.0-macOS-arm64-repacked-20261010.zip](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/PeerBrush-0.2.0-macOS-arm64-repacked-20261010.zip) | 25473214 | `35472e98dbbf5a27f3fd261f837162accf125a0c8503799f00d69a7e6002f8ef` |
| [PeerBrush-0.2.0-Windows-x64-repacked-20261010.zip](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/PeerBrush-0.2.0-Windows-x64-repacked-20261010.zip) | 26478146 | `3db047293fa1e307ec72dd24963ce14df551feb46a66b1b7e098126a6dadfec4` |
| [PeerBrush-0.2.0-source-repacked-20261010.zip](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/PeerBrush-0.2.0-source-repacked-20261010.zip) | 11728683 | `52a914ec993ca0deacd15dedf65805577201d5dfc240c32cb07c7a21e7266aa0` |

[Correction checksums](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/SHA256SUMS-repacked-20261010) and [release inventory](https://github.com/Deftr0y/PeerBrush/releases/download/v0.2/release-inventory-repacked-20261010.json) describe the actual public downloads. Superseded oversized uploads were removed after public download and website-link verification. Older releases are unchanged and require a separate audit.

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

Engine, codec, HTTP/stdio protocol and UI regression tests passed locally. Native Windows captures and independent standard PSD16/PNG16 decoding verified current rendered results, low-bit preservation, undo and editable reopening. Actual GPU parity checks passed on the available Windows device. The local Windows clipboard transport check was excluded because its protection gate refused existing clipboard contents; fresh-runner CI keeps that check enabled. [Exact-source release CI](https://github.com/Deftr0y/PeerBrush/actions/runs/38013631911) and regular CI passed on all three platforms. Actual public packages were checked against the inspected archives for source, architecture, licenses, executable permissions and SHA-256 checksums. macOS/Linux manual desktop interaction remains unverified.

All 0.2 packages are unsigned. Native publisher signing, notarization and archive signatures are deferred; Windows/macOS may show publisher or security warnings. Checksums verify integrity and are not publisher signatures. Package verification is described in [release packages](release-packages.md).

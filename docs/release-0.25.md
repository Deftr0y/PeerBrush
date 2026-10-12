# PeerBrush 0.25

Version 0.25 uses application version 0.25.0. The [published experimental prerelease](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.25) was released on 2026-10-12 from `f5083d2e33a0158c9976db5de89549bd34ee7bc3`. Windows x64, Linux x64 and macOS arm64 packages share that source commit. All packages are unsigned.

## Changes since 0.2

- Native-depth PSD/PSB opening and saving, guarded save-loss review, previous-file versions and stronger autosave/recovery handling.
- Production export presets and guarded layer batches; canvas navigation and tablet pressure controls; editable Pen paths; exact effect baking, mirror/quarter-turn transforms and Shift snapping.
- Per-tool brush settings, document-relative brush textures, Shift line/axis gestures, image project opening, tab reordering and clearer folder/mask/selection controls.
- Independent compact effect expansion for each project, with native-precision HSV/HEX color pickers and provisional gestures that commit once.
- Twelve categorized native layer styles, including editable patterns, gradient color/transparency stops, contour points, independent Bevel channels and placement controls. Shared engine commands retain original native 8/16-bit sources, undo and current standard PSD/PSB raster output.
- Selection and transform drafts reject stale document, revision, target and focus changes. Held shortcuts and parameter drags require an explicit retry after cancellation.
- Selection clicks use the actual mouse-release position when later pointer movement shares the frame, preserving existing coverage while starting lasso paths.
- Selected-content Cut/Paste preserves feathered coverage, holes, document placement and native samples, with atomic guards against clipboard failure and concurrent edits.
- Bounded Photoshop overlay, descriptor, pattern and hard-shadow imports retain strict source validation and exact current standard composite matching.

## Deferred work and verification

Broader Photoshop layer-style fidelity, including Classic gradients, soft shadows/glows, unsupported metadata and editable reopening of representative styled originals, is deferred. Unsupported documents remain protected; this release does not claim full Photoshop compatibility. Native gradient smoothing is independent of Photoshop Classic import.

Release validation covers engine, codec, protocol, input-adapter and packaging checks and isolated Windows workspace verification. Local OS clipboard tests are excluded; fresh-runner transport checks are recorded separately in release CI. Interactive Cut/Paste transport acceptance and a separately reported selection recurrence remain outstanding. macOS/Linux manual desktop interaction remains unverified. Publication requires matching Windows x64, Linux x64 and macOS arm64 archives from one exact commit, with verified source and checksums.

Packages retain the existing unsigned distribution policy. See [release packages](release-packages.md) and [Photoshop fidelity](photoshop-fidelity.md).

The combined local run passed 874 Rust checks across 84 targets and 18 packaging checks, with eight optional checks ignored and two local clipboard checks excluded. Formatting and Clippy passed with existing warnings. Fresh isolated Windows 8/16-bit gradient workspaces preserved original source words, zero editing history, editable PSD/PSB reopening and independently decoded exact standard channels and current merged composites. The protected Photoshop reference source and embedded pattern remained unchanged. Exact-source platform CI and public archive verification are separate publication gates.

[Exact-source platform CI](https://github.com/Deftr0y/PeerBrush/actions/runs/38252533281) passed on all three platforms. Actual public assets, their corresponding source, architecture, licenses and SHA-256 checksums were verified. The official website home, Downloads and dated version history were read back at version 0.25.

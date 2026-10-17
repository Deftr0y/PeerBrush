# PeerBrush 0.26

Application version 0.26.0 is prepared for stable release. Publication remains pending final reviewed promotion, exact-source builds, package inspection and the platform acceptance checks below. Published 0.25 artifacts and their provenance remain unchanged.

## Changes since 0.25

- Linked layer masks move with their layers; unlinking preserves independent mask placement. The shared engine retains native 8/16-bit samples, current PSD raster output, and undo.
- Brush previews reuse an exact displayed baseline for the first dab while uploading a complete new gesture frame. Pan, zoom and toolbar selection render in the input frame.
- Windows packaging adds an unsigned MSIX candidate with the reserved PeerBrush identity, PSD/PSB Open with handlers, queued file activation and a stable family-scoped MCP alias. Opening files preserves existing project tabs and unsaved work. Registration does not change default file handlers.
- Installed Windows state lives outside AppData virtualization. Connect AI detects hidden client configuration destinations and presents manual configuration when automatic registration would be invisible.
- macOS packaging can sign, notarize, staple and verify a newly built app from an explicitly reviewed source. Interrupted notarization resumes the original submission rather than uploading it again.

Backlog entries for additional image formats, shortcuts, sliders, large documents and performance record intended work; their presence does not establish implementation or release acceptance. Store-submission tooling remains inactive and untested against Partner Center. Manual submission of a verified candidate is supported.

## Release checks and limits

Windows x64, Linux x64 and macOS arm64 portable packages must share one final immutable production source commit and version. Inspect the actual packages, corresponding GPL source archives, third-party notices, architecture and checksums before publication. Actions candidates are not public release downloads.

The proposed Store version is 1.26.0.0. Read the actual current Partner Center package version before submission and require a strictly greater version. Windows SDK validation does not establish installation, clean-machine runtime dependency availability, app certification, cross-package file ownership, upgrade or uninstall acceptance. Complete the installed checks in [MSIX packaging](msix.md) through an approved installation route.

A new macOS version requires fresh binaries and a new reviewed signing source pin. An older version's signature or notarization receipt does not certify 0.26. Preserve the protected Apple signing approval and reconcile any earlier submission independently. Complete Gatekeeper and native launch checks described in [macOS signing](macos-signing.md).

Broader Photoshop fidelity remains bounded by [the compatibility policy](photoshop-fidelity.md). Unsupported documents remain protected; this release does not claim full Photoshop compatibility. Native macOS/Linux desktop interaction and real receiving-application acceptance require separate evidence.

# PeerBrush 0.30.0 - draft release notes

The v0.3 milestone is under development as application version 0.30.0. No 0.30.0 release is published. These notes are a review draft and do not establish download availability, installation acceptance or update readiness.

- Working PSD/PSB documents retain the existing limits of 8192 pixels per edge, 32 megapixels and 256 MiB per file. The 16000 × 16000 editor is planned for the next version.
- Editor work preserves native 8/16-bit samples, PSD/PSB raster data and current merged composites. Unsupported Photoshop structures retain their existing read-only protection.
- Responsiveness work includes shared source snapshots, incremental stroke previews, bounded inspector drafts and repeated color sampling. Save performance remains under investigation; current development measurements do not establish a general improvement.
- Installation preparation includes a Microsoft Store MSIX, a macOS DMG with an Applications link, Linux DEB packaging, per-user settings migration and PSD/PSB Open With integration. Default-app changes require the user's choice. Windows distribution and updates are Store-only. Store submission, installed upgrade and full platform acceptance remain incomplete.
- Update preparation displays progress and release notes, retains restart-later choices and protects unsaved work. macOS update installation remains disabled while native recovery, installed acceptance and approved signing/feed configuration are incomplete.
- Release preparation verifies platform source and version consistency, immutable files and checksums, bounded native evidence, public download readback and Store certification state. Publication and Store submission remain disabled pending completion, review and explicit approval.

See the [development plan](development-0.3.md) for implemented work, test evidence and remaining acceptance. Remote control and third-party add-ons are outside this milestone. Broader Photoshop fidelity remains bounded by the [compatibility policy](photoshop-fidelity.md).

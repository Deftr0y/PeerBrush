# PeerBrush 0.30.0 - draft release notes

The v0.3 milestone is under development as application version 0.30.0. No 0.30.0 release is published. These notes are a review draft and do not establish download availability, installation acceptance or update readiness.

- Working PSD/PSB documents retain the existing limits of 8192 pixels per edge, 32 megapixels and 256 MiB per file. The 16000 × 16000 editor is planned for the next version.
- Text layers can use installed fonts. If a saved font changes, PeerBrush asks you to choose a replacement before editing the text with it.
- Link a painted mask to its layer to transform them together, or unlink it to move either independently. Free Transform previews multiple selected layers on the canvas, including a nine-point Grid Warp. Apply commits the transform in one undo step.
- New Canvas can suggest the dimensions of an image on the clipboard. Typing your own dimensions takes precedence. The export menu offers JPEG XL at the document's native 8- or 16-bit depth and lossless WebP at 8 bits per channel.
- Info shows the application version and source revision. The MCP status distinguishes an available local server from a connected client.
- Editor work preserves native 8/16-bit samples, PSD/PSB raster data and current merged composites. Unsupported Photoshop structures retain their existing read-only protection.
- Fixed a 16-bit file-writing error that prevented Photoshop from opening layered documents with merged transparency. Photoshop 2026 opened the corrected PSD and PSB with both test layers intact.
- PeerBrush now opens Photoshop files with valid zero padding at the end of their layer data. Three Photoshop-authored style samples opened read-only and matched Photoshop's saved pixels. Unsupported styles remain protected.
- Responsiveness work includes shared source snapshots, incremental stroke previews, bounded inspector drafts and repeated color sampling. Save performance remains under investigation; current development measurements do not establish a general improvement.
- Installation preparation includes a Microsoft Store MSIX, a macOS DMG with an Applications link, Linux DEB packaging, per-user settings migration and PSD/PSB Open With integration. Default-app changes require the user's choice. Windows distribution and updates are Store-only. Store submission, installed upgrade and full platform acceptance remain incomplete.
- The Mac editor checks the signed stable feed before starting Sparkle. A no-update result or cancelled check leaves the editor and its settings alone. After an interrupted install, the editor can offer the verified previous app with separate settings. Signed two-version acceptance and publication of the feed are still pending.
- Release preparation verifies platform source and version consistency, immutable files and checksums, bounded native evidence, public download readback and Store certification state. Protected Store submission requires authenticated native checks; final publication remains gated on certification and installed acceptance.

See the [development plan](development-0.3.md) for implemented work, test evidence and remaining acceptance. Remote control and third-party add-ons are outside this milestone. Broader Photoshop fidelity remains bounded by the [compatibility policy](photoshop-fidelity.md).

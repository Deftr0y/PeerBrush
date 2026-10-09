# PeerBrush development

- Preserve the GPL v3 license and Ember palette from MayaMCP.
- Use compact, unboxed range values. Avoid decorative containers and borders that add clutter or waste space.
- Show the actual rendered result while the user works: strokes, transforms, blend choices, effect changes and layer reordering. Preview through the shared engine, without committing history until the gesture finishes. Outline-only feedback is insufficient.
- Show AI work in blue. Animate meaningful AI changes in presentation only: numeric controls, canvas results and hierarchy movement. Commit the actual shared state immediately, keep animations out of history/PSD sources, isolate derived caches, and let human input interrupt them.
- Editing logic belongs in the document engine. UI, MCP, and CLI must share commands and enforce the same reservations.
- Never silently drop unsupported Photoshop features or claim full PSD compatibility. Keep unsupported documents read-only.
- Every save must contain a current merged composite. PeerBrush-only source data must not replace standard PSD raster/mask data.
- Preserve human work when resolving conflicts or undoing agent tasks.
- Keep model choice outside the application; return actual image content and explicit document coordinates to clients.
- Run engine/codec/protocol tests and visually verify the native workspace after behavior changes.
- Do not commit generated QA artifacts, credentials, connection tokens, development tools, recovery files, or build outputs. Requested bundled app artwork, fonts and curated documentation screenshots are source assets and must accompany the application.

- Layer controls use a connected Color/Mask selector, with blend and opacity beneath the effects. Top effects run last. Rename inline, without a dialog. Create regular layers, folders and explicitly requested adjustment layers; new folders collect selected roots.

- Preserve native 16-bit channels through edits, internal clipboard, effects, transforms and PSD/PNG saves. Screen/agent previews may project to 8 bit. Never silently make an 8-bit editable copy of a 16-bit original. Unsupported Photoshop structures can be explicitly flattened at their original precision.

## Release and public-information maintenance

- For authorized releases, patches and material project developments, follow [the release-maintenance playbook](docs/release-maintenance.md). Keep GitHub release artifacts/notes, the official website's home and Downloads information, confirmed official Discord announcements and social accounts consistent with the actual published version and verified changes. Keep Windows, macOS and Linux packages on the same version and commit; verify real assets and checksums and report any missing platform. Maintain website version history with dated patch notes and versioned downloads, alongside consistent home/latest information. Respect current holds, destination-specific permissions and platform rules; unreleased work must be labelled as such.
- Read back published state, avoid duplicate posts, and keep necessary receipts/blockers privately outside this public repository. Never claim every destination is updated when any result is failed, inaccessible or unverified. Missing access is a blocker, not permission to guess a destination.
- Apply the playbook's privacy/security gate before commits, packages or public updates. Keep credentials, private configuration, personal working practices and internal coordination out of public files, history, artifacts, logs and screenshots. Use redacted examples and report suspected exposure privately.

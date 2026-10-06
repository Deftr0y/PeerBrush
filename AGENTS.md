# PeerBrush development

- Preserve the GPL v3 license and Ember palette from MayaMCP.
- Use compact, unboxed range values. Avoid decorative containers and borders that add clutter or waste space; this is an explicit user preference.
- Show the actual rendered result while the user works: strokes, transforms, blend choices, effect changes and layer reordering. Preview through the shared engine, without committing history until the gesture finishes. Outline-only feedback is insufficient.
- Show AI work in blue. Animate meaningful AI changes in presentation only: numeric controls, canvas results and hierarchy movement. Commit the actual shared state immediately, keep animations out of history/PSD sources, isolate derived caches, and let human input interrupt them.
- Editing logic belongs in the document engine. UI, MCP, and CLI must share commands and enforce the same reservations.
- Never silently drop unsupported Photoshop features or claim full PSD compatibility. Keep unsupported documents read-only.
- Every save must contain a current merged composite. PeerBrush-only source data must not replace standard PSD raster/mask data.
- Preserve human work when resolving conflicts or undoing agent tasks.
- Keep model choice outside the application; return actual image content and explicit document coordinates to clients.
- Run engine/codec/protocol tests and visually verify the native workspace after behavior changes.
- Do not commit generated QA artifacts, credentials, connection tokens, development tools, recovery files, or build outputs. Requested bundled app artwork, fonts and curated documentation screenshots are source assets and must accompany the application.

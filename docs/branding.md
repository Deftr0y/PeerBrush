# PeerBrush branding

The bundled mark is the interlocking P accepted by the user as a provisional logo on 2026-10-07. It uses MayaMCP's ember orange, ivory and plum palette. Tool icons are simple white glyphs; blue is reserved for AI work indicators.

MayaMCP's interlocking orange/ivory mark is the intended M reference: its subtle letter shape is formed by the linked ribbons. Use maya_plugin/maya_mcp_icons/ember-link.png from MayaMCP as the visual reference. The PeerBrush P carries the same broad rounded ribbons, diagonal connections and plum overlap faces into a longer left stem and upper bowl.

The user considers the current P acceptable for now, with further refinement still wanted. Preserve this direction when revisiting proportions, edge quality and readability at toolbar/app-icon sizes. Keep actual transparency and avoid a decorative badge or surrounding box.


The Windows 0.1.4 artwork included an ImageGen-created transparent tool atlas and transparent PeerBrush wordmark (`assets/peerbrush-wordmark.png`). The current development source replaces the tool atlas with the approved Bold Solid family: `assets/tool-icons.png` (6 × 3 glyph coverage cells) and `assets/selection-icons.png` (4 × 2 selection cells). The original ImageGen outputs are bundled intact; runtime decoding converts coverage into alpha for state tinting. See `assets/tool-icons-prompts.txt` for the generation prompts. This redesign is unreleased. The existing P logo, wordmark and Ember palette remain. Runtime display crops transparent wordmark padding and scales the texture for supported texture limits.

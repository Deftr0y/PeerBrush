# PeerBrush interface

The current workflow is retained while the visual system follows the user's Ubuntu and MayaMCP references: charcoal surfaces, ivory text, orange accents, plum selection fills, compact controls, and simple white tool glyphs.

Ubuntu Sans Regular is bundled for interface text, with Medium and Semibold for headings and branding. Fonts require no local installation. The unmodified fonts and Ubuntu Font Licence come from [Canonical's official repository](https://github.com/canonical/Ubuntu-Sans-fonts/tree/9554af00fb9d438a12c916df8451c10dcedc9b7e).

Tool icons use the approved **Bold Solid** ImageGen family: filled silhouettes, open cutouts and consistent optical weight. The 18 primary tools use `assets/tool-icons.png` (6 × 3 cells); eight additional selection glyphs use `assets/selection-icons.png` (4 × 2 cells). The white-on-black source images are glyph coverage maps. `src/icons.rs` decodes them once to neutral alpha textures, preserves square proportions and applies state color during rendering: ivory by default, Ember orange when selected, blue during AI activity and muted gray when disabled. Utility/action icons retain native vector artwork. Icons do not depend on font glyphs. This redesign is an unreleased development change.

The app header and window use the user's selected folded-canvas P logo (the third option). It was created with the built-in image-generation tool, using MayaMCP artwork as a visual reference. Original prompt: “A P formed by a folded ivory canvas sheet and one broad orange brush stroke, with plum side faces; a minimal dimensional icon in MayaMCP's orange and ivory visual family, on a transparent background.” The source PNG is bundled in `assets/peerbrush-logo.png`.

The interface remains an early version and will continue to evolve from specific user feedback.

The user's range reference is the design contract: a dark rounded track, orange fill, and the value inside the fill, with the label outside. Avoid separate numeric boxes and decorative containers. Idle layer rows have no enclosing box. Foreground/background colors sit at the bottom left. Small hover transitions, parameter fill transitions, floating held rows and animated row positions communicate interaction.

Zero-decimal range controls with whole-number bounds use one-unit steps for keyboard and pointer edits. This prevents small keyboard increments from rounding back to the same displayed integer. Native focus, repeats and endpoint clamping remain in the slider. Exact half-step ties follow its lower-bound step grid, including negative ranges. Controls with fractional bounds (such as a 0.5-pixel Liquify radius) retain their existing mapping; idle fractional animation is not quantized.

Always show the actual rendered result during interaction. Stroke, transform, effect, blend and layer-order feedback uses shared editing commands applied to a transient document; previews do not create undo history.

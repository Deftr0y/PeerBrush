# PeerBrush interface

The current workflow is retained while the visual system follows the user's Ubuntu and MayaMCP references: charcoal surfaces, ivory text, orange accents, plum selection fills, compact controls, and simple white tool glyphs.

Ubuntu Sans Regular is bundled for interface text, with Medium and Semibold for headings and branding. Fonts require no local installation. The unmodified fonts and Ubuntu Font Licence come from [Canonical's official repository](https://github.com/canonical/Ubuntu-Sans-fonts/tree/9554af00fb9d438a12c916df8451c10dcedc9b7e).

All toolbar and action icons are native vector artwork in `src/icons.rs`, drawn on a consistent grid and scaled for display density. Tool and utility icons use readable white strokes; selection and connection states carry color. They do not depend on font glyphs.

The app header and window use the user's selected folded-canvas P logo (the third option). It was created with the built-in image-generation tool, using MayaMCP artwork as a visual reference. Original prompt: “A P formed by a folded ivory canvas sheet and one broad orange brush stroke, with plum side faces; a minimal dimensional icon in MayaMCP's orange and ivory visual family, on a transparent background.” The source PNG is bundled in `assets/peerbrush-logo.png`.

The interface remains an early version and will continue to evolve from specific user feedback.

The user's range reference is the design contract: a dark rounded track, orange fill, and the value inside the fill, with the label outside. Avoid separate numeric boxes and decorative containers. Idle layer rows have no enclosing box. Foreground/background colors sit at the bottom left. Small hover transitions, parameter fill transitions, floating held rows and animated row positions communicate interaction.

Always show the actual rendered result during interaction. Stroke, transform, effect, blend and layer-order feedback uses shared editing commands applied to a transient document; previews do not create undo history.

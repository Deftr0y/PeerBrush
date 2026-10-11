# Layer-style effects (unreleased)

PeerBrush adds Bevel & Emboss, Contour, Texture, Stroke, Inner Shadow, Inner Glow, Satin, Color Overlay, Gradient Overlay, Pattern Overlay, Outer Glow and Drop Shadow to the regular Color effect stack. Find them by name or under Relief, Outlines, Shadows and Overlays. Top effects run last. Each step has the existing independent strength, bypass, reorder and undo controls; adjusting settings previews actual rendered pixels through the shared engine.

These are native PeerBrush style effects with bounded settings. They do not enable Photoshop layer-style descriptor or embedded-pattern import/export. A Photoshop document containing unsupported styles, patterns, global masks or profiles stays protected and displays its saved composite. See [Photoshop fidelity](photoshop-fidelity.md).

| Effect | Available settings |
| --- | --- |
| Bevel & Emboss | Inner/Outer/Emboss/Pillow, size, soften, depth, light angle/altitude, Up/Down, highlight/shadow colors, contour, opacity and blend |
| Contour | Linear/Cone/Ring/Smooth profile, range, invert, opacity and blend; maps the incoming relief/tone |
| Texture | Checker/Stripes/Dots/Noise procedural pattern, pixel scale, lighting angle, depth, invert, two colors, opacity and blend |
| Stroke | Circular outline size, Inside/Outside/Center, color, opacity and blend |
| Inner Shadow | Size, distance, angle, choke, contour, color, opacity and blend |
| Inner Glow | Edge/Center source, size, spread, contour, color, opacity and blend |
| Satin | Size, distance, angle, contour, invert, color, opacity and blend |
| Color Overlay | Native-normalized RGB color, opacity and blend |
| Gradient Overlay | Linear/Radial/Angle/Reflected/Diamond, two colors, angle, scale, reverse, opacity and blend |
| Pattern Overlay | Checker/Stripes/Dots/Noise procedural pattern, pixel scale, invert, two colors, opacity and blend |
| Outer Glow | Size, spread, contour, color and opacity |
| Drop Shadow | Size, distance, angle, spread, contour, color and opacity |

Overlay and inner styles preserve input alpha. Exterior styles can grow into transparent canvas space without replacing or expanding editable source pixels. Earlier steps run in their original source frame; the first active style expands its derived input into document coordinates. Gradient and pattern coordinates use the canvas frame, independent of screen pan/zoom. Output is bounded by the canvas; off-canvas source pixels are retained but exterior styling does not render beyond this frame. Sizes/softening are limited to 64 pixels and shadow/Satin distances to 128 pixels. Large derived/temporary images are rejected explicitly under the existing memory budgets. These Color styles apply to regular layers and isolated folders; adjustment layers, whole-image filters and mask stacks do not offer them.

Drop Shadow and Outer Glow composite beneath the incoming layer image. Their interaction with other layers follows the layer's blend mode; an independent Photoshop-style backdrop blend is not implemented. Relief uses alpha-field shading, not Photoshop's bevel techniques or global-light sharing. Contour operates on incoming luminance as an independent step. Patterns are procedural and gradients use two color stops; Photoshop pattern assets, gradient libraries, jitter and proprietary style presets are not imported. These limits are intentional and no bit-for-bit Photoshop equivalence is claimed.

UI, MCP and CLI share `effect.add/update/delete/reorder` with explicit layer IDs, source revisions, locks and reservations. Settings use native-normalized RGB arrays (`[r,g,b]`, each 0–1); opacity/spread are 0–1. The tool catalog exposes stable effect keys. An example:

```json
{"op":"effect.add","layer":"LAYER_ID","kind":"drop_shadow","settings":{"color":[0,0,0],"opacity":0.6,"size":8,"distance":12,"angle":120,"spread":0.1,"contour":"smooth"}}
```

Effects keep native 8/16-bit source channels and masks. PSD/PSB saves include the current standard rendered raster/mask data and merged composite; PeerBrush source format 16 also retains the editable style stack and original source samples. Other applications see the baked appearance, and older PeerBrush readers protect newer style sources rather than silently discarding them. Proportional image resizing scales spatial style settings; unsupported non-proportional style resizing rejects atomically.

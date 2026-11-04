# v0.3 Photoshop and selection acceptance

Unreleased, bounded PB-122/PB-110 audit. Existing implementations and completed history remain in place. This note does not establish receiving-application, hardware or native desktop acceptance.

## PB-122: retain conservative import

The shared codec retains original native8/16 raster sources and validates supported style definitions, including disabled entries. Editable style import requires the complete stored white-matte RGB and alpha composite to match at the original channel depth. A failed render, unsupported descriptor or one-sample mismatch preserves the protected saved appearance. Integer-grid gradient fallback regenerates derived effect identities before its one bounded alternate render; it does not try arbitrary combinations or relax comparison.

The preserved feature branch and v0.3 base have the same Photoshop/style and selection/transform implementation. Existing [codec tests](../tests/psd_clipping.rs) cover integer-grid/aligned gradients, destination-stop midpoint ownership, retained original sources, malformed disabled definitions, exact mismatch protection and editable PSD/PSB saves. Current family order is Pattern, Gradient, Color; the current fidelity guide now agrees with the [calibration correction](layer-styles.md#calibrated-photoshop-overlay-family-order-unreleased), while earlier evidence remains in the development history.

No additional importer was justified by this audit. Native gradient smoothness is an authoring foundation, tested against independent cubic expectations in [layer-style tests](../tests/layer_styles.rs). It is not an exact Photoshop Classic implementation: wider saved samples retain one-byte differences, so nonzero Photoshop `Intr` remains protected. Further PB-122 acceptance requires independently saved Photoshop fixtures covering the proposed interpolation/placement subset and the representative styled source, including its profile and unsupported structures. Recognizing another descriptor alone does not satisfy this requirement.

## PB-110: scalar coverage interpolation

Selected scale/rotation previously sampled a selection mask as premultiplied RGBA and used the resulting red channel. Canonical selection masks store the coverage value in all four channels. Interpolating between full coverage and zero therefore unpremultiplied the red channel back to 255 instead of retaining the fractional edge. For example, a three-quarter sample should retain 191 coverage, not 255. This could distort selection geometry used by later edits or copying even when the transformed artwork alpha was already fractional.

The shared transform path now bilinearly interpolates scalar red-channel coverage directly, matching `Coverage::value`. It continues honoring masks whose stored alpha differs from their coverage channel. Paint, selected mask and selected folder transforms share this path. Original source cutting, native raster interpolation, reservations, transaction guards and history remain unchanged.

Two focused regressions were added to [selection-system tests](../tests/selection_system.rs): one distinguishes red coverage from independent RGBA alpha; the other uses independently calculated quarter-position samples across native8/16 paint, mask and folder transforms. The latter checks all output coverage, holes/soft values, unselected native/hidden samples, unchanged mask-target color, preview parity, one-step undo/redo and PSD/PSB reopening.

## Verification and remaining acceptance

The analytic regressions passed with the combined engine/codec/protocol/UI suite: 979 tests at `3dc56e6`, then 981 at `9b3eace` after adding unrelated idle-dialog capture regressions. All-target Clippy passed with warnings. Native Windows checks at `f7f715d` verified actual native8/16 artwork/coverage, frozen proposal previews without source/history changes, exact commit/undo/redo, independent standard PSD/PSB white-matte RGB/alpha samples and editable reopening. The transform implementation and these analytic tests are unchanged between that native checkpoint and `9b3eace`. Generated inputs, captures and receipts remain outside public source.

This specific sampling defect does not establish the cause or resolution of the separately reported wand/copy/transform recurrence. Wider follow-on selection workflows, actual desktop clipboard transport, receiving-application round trips, macOS/Linux desktop and physical tablet/display acceptance remain separate checks. No tolerance was added to Photoshop composite matching, and protected unsupported originals remain protected.

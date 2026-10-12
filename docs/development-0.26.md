# PeerBrush 0.26 development

Unreleased development on `codex/0.26`, starting from the published 0.25 source and its documentation updates. Published downloads remain 0.25. This queue supplements the acceptance criteria in [FOLLOWUPS.MD](../FOLLOWUPS.MD); an existing implementation is not evidence that outstanding acceptance work passed.

Work proceeds in the backlog's established priority order:

1. **PB-122 / PB-136:** expand conservative Photoshop style import, preserving native channels and requiring an exact match with the standard saved composite. Keep unsupported documents protected. First slice: connect the existing integer pixel-grid gradient renderer to bounded gradient import without changing previously accepted source placement.
2. **PB-110:** investigate the reported selection recurrence and preserve selection coverage through copy/paste, transforms and cancellation.
3. **PB-080 / PB-137 / PB-138:** complete remaining lifecycle and cross-platform native acceptance checks. The original reported incident's cause remains unverified.
4. **PB-072 / PB-108:** continue canvas/tablet and performance work; physical tablet, eraser-end and driver measurements require actual hardware.
5. **PB-118 / PB-070:** receiving-application round trips and remaining Photoshop/color-profile fidelity, with actual application/version evidence.
6. **PB-106 / PB-133:** close outstanding effects UI and native clipboard acceptance checks without replacing a populated human clipboard.
7. **PB-007 / PB-042 / PB-036 / PB-061 / PB-076 / PB-081:** remaining visual feedback, performance paths and optional refinements after the higher-priority reliability work.
8. **PB-141:** use installed system fonts for multilingual UI while preserving Ubuntu Sans, Unicode paths, shaping and document Text sources; verify missing-font cases and actual platform package reductions before removing bundled coverage.
9. **PB-103 / PB-105:** signing and larger-document architecture remain dependent on signing access and bounded resource measurements; current size limits stay in force until the engine and codec support an increase.
10. **PB-139:** historical-reference audit may proceed read-only. History rewrites, tag moves and deletion remain subject to the explicit owner-approval requirement recorded in the backlog.

The existing GitHub Actions workflow runs engine, codec, protocol and UI regression tests, formatting, Clippy and package checks on every branch push, across Windows, macOS and Linux. Development checkpoints use that workflow for automated testing. A queued run is not a passed run: record the exact tested commit and outcome before closing acceptance work. Native workspace inspection remains separate from CI, as do physical hardware and receiving-application checks.

No release, website deployment, announcement or historical rewrite is part of starting this development branch.

## PB-122 integer-grid gradient import

Implemented, acceptance pending. Existing supported gradient geometry is tried first. If its complete native composite differs, the decoder tries one alternate document with enabled Gradient Overlays using the existing integer-center pixel grid. Only an exact match of every standard white-matte RGB and alpha sample permits editable opening. Original native rasters and all existing descriptor/structure guards remain in force; rejected candidates use the protected saved composite. Mixed per-effect geometry combinations, smooth Photoshop gradients and broader unsupported style structures remain protected.

The regression fixture supplies independent 8/16-bit integer-center ramp samples, reversed placement, editable PSD/PSB reopen, standard-only appearance, effect bypass/undo and one-word mismatch protection. The first checkpoint failed platform regression tests. Review found that the alternate document reused the first render's derived effect keys; the correction regenerates those keys through shared engine invalidation before rendering the candidate. GitHub Actions and fresh native workspace acceptance of the correction are pending; this slice does not close PB-122. CI now publishes bounded test names and counts in its job summary and failure annotations, retaining the original test exit status and excluding arbitrary log contents. Compilation failures can report bounded Rust diagnostic codes without publishing their free-form messages.

## PB-122 layer-aligned gradients

Implemented, acceptance pending. Bounded cardinal Linear/Reflected Gradient Overlays can retain an explicit `Algn=true` definition using the existing native integer grid and alpha-bound layer alignment. Document-aligned gradients keep their earlier first-choice placement. Dither, unsupported interpolation, malformed fields and other unsupported structures remain protected; every accepted import still requires an exact complete native composite match.

The additional native8/16 fixture checks transparent hidden source RGB, enabled/disabled definitions, independent integer-center samples, editable PSD/PSB saves, effect bypass/undo and one-word mismatch protection. No new source format or rendering formula is introduced. Automated and native acceptance remain pending.

## PB-122 additional gradient shapes

Implemented, acceptance pending. Radial, Angle and Diamond descriptors now map to the existing integer-grid native renderer alongside Linear and Reflected. Their PSD IDs follow the [primary ag-psd descriptor codec](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts). Cardinal angle, native RGB stops, scale/offset, alpha-stop, descriptor and complete exact-composite guards continue to apply. Unknown shapes and unsupported rotation remain protected even when disabled. This is a bounded import extension, not full Photoshop gradient compatibility.

Independent native8/16 fixtures cover radius/Manhattan coordinates, angle-axis and singular-center samples, reverse, enabled/disabled definitions, original sources, editable PSD/PSB reopening, effect bypass/undo and exact mismatch protection. Automated acceptance is pending.

Sixteen independent offline 256×160 fixtures cover native8/16 Linear, Radial, Angle and Diamond imports, with document or alpha-bound layer alignment. Every original channel (including transparent hidden RGB), editable PSD/PSB definition and rendered export survived save/reopen and exact bypass/undo. The rendered gradient, recovered source and additional shape images were inspected. The layer-alignment regression fixture's standard-composite expectation was corrected to distinguish white-matted PSD RGB from straight-alpha rendered RGB; no renderer or exact-match tolerance changed. Native workspace interaction and final combined CI acceptance remain pending.

## PB-122 standalone solid Stroke

Implemented, acceptance pending. One unscaled Normal solid-color `FrFX` definition, or one explicit `frameFXMulti` entry, can map to the existing native Inside/Outside/Center Stroke renderer. Whole-pixel sizes are bounded to 0–128 pixels. Original native8/16 raster channels remain intact; editable opening requires an exact complete standard composite match. Mixed stacks, multiple strokes, gradient/pattern paint, overprint, masks, clipping, partial source alpha and unsupported fields remain protected, including disabled definitions. Descriptor IDs follow the [primary ag-psd codec](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts).

Regression coverage specifies independent half-coverage edge samples, master/present/enabled states, single/list aliases, native source retention, current standard PSD/PSB composites, editable reopening, bypass/undo and one-sample mismatch protection. Formatting and compile checking passed; automated and fresh native workspace acceptance remain pending. This does not establish general Photoshop Stroke equivalence or recover the broader representative styled document.

## CI checkpoints

Commit `0b91a4c` passed all three native jobs and matching platform package inspection in [GitHub Actions](https://github.com/Deftr0y/PeerBrush/actions/runs/38275647900). Commit `197eac5` also has a [successful complete run](https://github.com/Deftr0y/PeerBrush/actions/runs/38285419164), while its other run failed an intermittent file-ownership regression. Later runs must be verified separately; a successful older checkpoint does not close the current development queue.

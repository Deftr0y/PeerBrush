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

The regression fixture supplies independent 8/16-bit integer-center ramp samples, reversed placement, editable PSD/PSB reopen, standard-only appearance, effect bypass/undo and one-word mismatch protection. The first checkpoint failed platform regression tests. Review found that the alternate document reused the first render's derived effect keys; the correction regenerates those keys through shared engine invalidation before rendering the candidate. GitHub Actions and fresh native workspace acceptance of the correction are pending; this slice does not close PB-122. CI now publishes bounded test names and counts in its job summary, retaining the original test exit status and excluding arbitrary log contents.

## PB-122 layer-aligned gradients

Implemented, acceptance pending. Bounded cardinal Linear/Reflected Gradient Overlays can retain an explicit `Algn=true` definition using the existing native integer grid and alpha-bound layer alignment. Document-aligned gradients keep their earlier first-choice placement. Dither, unsupported interpolation, malformed fields and other unsupported structures remain protected; every accepted import still requires an exact complete native composite match.

The additional native8/16 fixture checks transparent hidden source RGB, enabled/disabled definitions, independent integer-center samples, editable PSD/PSB saves, effect bypass/undo and one-word mismatch protection. No new source format or rendering formula is introduced. Automated and native acceptance remain pending.

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

CI caches Rust registry data and build outputs with the [official cache action](https://github.com/actions/cache), pinned to its verified Node.js 24 v5 commit. Keys separate runner image/architecture, stable compiler version, dependency manifests and release compiler flags. Tests and package verification still execute on every run; cached builds are not acceptance evidence. Runtime QA state, source-control metadata and credentials are outside the cached paths. Turnaround improvements remain to be measured after the first successful cache population.

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

Six independent 256×160 Stroke fixtures opened in a fresh isolated native workspace as editable native8/16 at revision zero. Captures were inspected for Inside, Outside and Center placement and show the rendered outlines and native Stroke control over the retained checker source. Interaction and final CI acceptance remain separately pending.

## CI checkpoints

Commit `0b91a4c` passed all three native jobs and matching platform package inspection in [GitHub Actions](https://github.com/Deftr0y/PeerBrush/actions/runs/38275647900). Commit `197eac5` also has a [successful complete run](https://github.com/Deftr0y/PeerBrush/actions/runs/38285419164), while its other run failed an intermittent file-ownership regression. Later runs must be verified separately; a successful older checkpoint does not close the current development queue.

The standalone Stroke checkpoint `4d09cc1` passed all three native jobs and package inspection in [this complete run](https://github.com/Deftr0y/PeerBrush/actions/runs/38289758916). Its [other run](https://github.com/Deftr0y/PeerBrush/actions/runs/38289766620) failed Linux save-safety retention at `tests/file_safety.rs:332`; that result remains separate from the style acceptance.

## PB-137 ownership release hardening

The [639a369 Linux run](https://github.com/Deftr0y/PeerBrush/actions/runs/38287773856) identified an ownership-busy failure at the first open after cancellation (`tests/file_ownership.rs:105`), while another run's Linux job passed. Final native ownership now explicitly unlocks its OS handles before closing them. [Rust's lock contract](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock) allows duplicated or inherited descriptors to keep a lock alive after the original closes; a deterministic regression retains those descriptors across reacquisition and verifies that the new owner still blocks hard-link aliases. This addresses that lock-lifetime condition; the intermittent CI cause and broader original data-loss report are not yet proven resolved. Formatting and compile checking passed; cross-platform CI acceptance is pending.

Save leases and previous-save quota guards now use the same explicit OS unlock on drop, including fallible lease preparation after acquisition. Additional deterministic coverage retains duplicate descriptors across reacquisition, verifies that the new lease still excludes competitors, and preserves original file bytes. CI adds bounded `SAVE_BUSY` and `BACKUP_STORAGE_FULL` failure categories without publishing arbitrary panic text. These changes address the lock-lifetime condition; the intermittent save-safety failure still requires CI verification.

The save-lease checkpoint `d454414` passed all native jobs and package inspection in [this complete run](https://github.com/Deftr0y/PeerBrush/actions/runs/38292827304). A later Linux run still found ownership-busy during a repeated save in the previous-save chooser. Physical-identity locks now retain the source file handle as well as the OS lock, so an unlinked/replaced source inode cannot be reused by an unrelated file while its identity remains claimed. Staged identities are pinned through publication; retiring/releasing them drops those handles. The deterministic regression checks original content after unlink, independent files, staged publication and release. Post-acquisition owner-record errors also use the explicit unlock guard. Formatting and compile checking passed; CI and native save acceptance of source pinning are pending.

## PB-080 workspace cache identity

The registry cache now retains and checks the weak identity of its root engine, alongside the weak workspace. Closing the root can remove its registry handle while an engine snapshot keeps the registry alive. Previously its address-only key could then collide with a new root after allocation reuse. The weak root pins that allocation until cache retirement; identity validation also rejects a stale cache entry. A deterministic regression simulates the collision after close, preserves the old snapshot, and verifies independent project lookup and active identity. Formatting and compile checking passed; CI and native lifecycle acceptance remain pending. This is a separate isolation fix, not an established explanation for the original data-loss report.

## PSD supplementary canvas identity

Supplementary PeerBrush sources now require exact agreement with the standard header's width and height, in addition to existing depth, standard hashes, metadata and source validation. An external dimension swap can retain the same planar sample count and unchanged layer records; those hashes alone cannot establish the canvas identity. A mismatch keeps the saved standard appearance protected at its actual header dimensions and original precision. Native8/16 PSD/PSB regression coverage preserves the unchanged editable case and rejects conflicting sources/saves. Formatting and compile checking passed; CI and native acceptance are pending.

## PB-141 installed-font discovery

Implemented discovery and presentation selection, acceptance pending. The existing fontdb codec scans standard Windows, macOS and Linux font locations. Regular installed faces are selected only after checking representative complete Hebrew, Arabic, Devanagari, Han, kana and Hangul samples. Collection indices are retained. Ubuntu Sans stays primary; the existing bundled language fonts remain a temporary fallback while replacement acceptance and archive reductions are pending.

Selected font bytes are snapshotted and shared by the filename shaper, with a 32 MiB per-file and 64 MiB retained-data bound. The presentation catalog neither retains font paths nor downloads missing fonts. Existing bidi/shaping and 64-entry/8 MiB texture bounds remain in force. Runtime font-family names are XML-escaped. Document Text definitions, logical filenames, original paths, history and pixels are unchanged. Additional coverage checks installed selection, absent/invalid/oversized fonts, complete script samples, collection indices and primary-font order. Formatting and compile checking passed; CI, native multilingual acceptance and bundled-font removal remain pending.

A fresh isolated Windows workspace opened a synthetic mixed Hebrew/Arabic/Hindi/Han/kana/Hangul filename as editable native16 at revision zero. Its native capture was inspected for shaped status text and clipped RTL tab presentation. The source artwork and logical path were retained. A deterministic Ubuntu-only shaping database checks visible missing-glyph rendering for an absent Hindi font without rewriting the logical name. CI now reports measured source/platform archive sizes through bounded notices and job summaries. System-only package reductions and native macOS/Linux acceptance remain pending; bundled assets are retained until replacement verification passes.

Complex-script layer-row names also use the shared presentation shaper, including AI/hidden-layer color tinting. Existing click targets and inline rename keep the original logical name. A native8/16 UI regression double-clicks the shaped name and checks exact selection, source document and history retention before editing. Latin row labels keep their existing font weights. Formatting and compile checking passed; CI and fresh native row acceptance are pending.

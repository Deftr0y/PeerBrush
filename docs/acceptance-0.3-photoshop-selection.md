# v0.3 Photoshop and selection acceptance

Unreleased, bounded PB-122/PB-110 audit. Existing implementations and completed history remain in place. This note does not establish receiving-application, hardware or native desktop acceptance.

November 6 continuation: the bounded normal/unclipped Invert import and standard
save implementation is described in [Photoshop fidelity](photoshop-fidelity.md).
Before acceptance, independently saved Photoshop native8/16 PSD/PSB cases must
cover zero-size/canvas-size Invert records, white-default and fractional masks,
visibility, opacity, folder placement and unchanged base pixels. Edit the base
and mask, verify one-step undo/redo, and reopen the standard layers in Photoshop
after saving. Inspect the current merged composite independently. Include older
PeerBrush source restoration, malformed definitions and one-sample mismatch
protection. The inactive/advisory global forms found in the supplied regression
now have a standard preservation path. Confirm exact original rendering and
metadata retention on that file; tag recognition alone does not qualify it. New regression source
has compile-only coverage; execution and receiving-editor acceptance are pending.

## PB-122: retain conservative import

The supplied RGB8 Invert and Multiply document provides an independent blend calibration. Reading its standard channels and saved composite directly, all 2,963,145 RGB samples match when Multiply rounds the channel product and the product of layer opacity and mask coverage to bytes before interpolating over the opaque backdrop. A continuous reference calculation differs by one byte in 130,029 samples. The reader preserves the disabled version 3 Content Credentials record, including its eight-byte zero trailer. Resident and native opening can adopt the calibrated rule after matching the entire saved composite. On Windows, the resident codec opened the supplied document for editing with four layers and retained all six admitted global records and the exact render through PSD and PSB round trips.

The Windows desktop build opened an isolated copy for editing with two Invert adjustments, the masked Multiply layer and the original backdrop. Opening created no edit history. All 3,950,860 full-resolution RGBA preview samples matched the independently decoded saved composite. Native Save initially requested consent for the imported masks. The shared save report now recognizes the implicit white default followed by one full-weight paint step when the standard layer plan includes the mask. Custom fill values, disabled steps and additional mask effects still receive the loss warning. The Windows library and regression sources passed compile-only checking. The rebuilt native diagnostic reported no editability losses and reached Save. Its synchronous request expired after four minutes before an output file was published. Native Save completion and Photoshop receiving-editor verification remain pending.

Native PSD and PSB Save now renders a small merged image once for the four encoded channels and again for independent readback. Each pass releases its frame before continuing. The buffer is capped at 16 MiB and one-eighth of the remaining resident budget. Larger frames or insufficient admission use the existing stripe renderer. Save reports completed channel rows during layer and merged encoding, with separate stages for preparation, sealing, verification, assembly, readback and publication. Regression source covers RGB8 and RGB16 white-matte samples, budget refusal, release of the buffer and exact progress across stripe boundaries. Controlled timing for the latest source remains pending.

A Windows headless save of a 3072 × 1025 RGB16 PSB reported intermediate progress against 4,100 channel rows in each encoding stage. The same run completed and published the file.

The async diagnostic reproduced source inventory contention while the Save worker started and the server prepared its reply. The server now prepares the reply before starting the worker. Project listing provides Save status without a source read session. The repaired path still needs a full native round trip.

The repaired async run reached candidate readback and refused publication at the old two-resource Source36 index limit. The limit now accounts for the ten supported fixed document resources alongside the manifest and profile choice. Duplicate, conflicting and changed records remain subject to the existing checks, and unrelated resources retain the previous allowance. Readback and native-stream completion transfer contiguous unpadded raw rows in bounded batches. Stream completion and per-sample comparison still run independently.

A Windows development build with the corrected resource limit and batched sample comparison saved and reopened both formats with all four layers editable. Independent parsing matched all 3,950,860 standard merged RGBA samples in each file, four standard layers with their native raster samples and mask coverage, ten fixed image resources and six retained global records. The isolated input remained byte-for-byte unchanged. The diagnostic took about 17 minutes to save PSD and 14 minutes to save PSB, with each reopen taking about 4 minutes 35 seconds. A second Windows development build with batched stream completion passed the same PSD and PSB checks and reported encoding, readback verification and publication separately. Its observed saves took about 11 minutes for PSD and 7 minutes 17 seconds for PSB. Controlled production timing and Photoshop receiving-editor acceptance remain pending.

One headless diagnostic Open took about 81 seconds on a local Windows development build. Native decoding and the exact Multiply comparison both contributed to the elapsed time. Native layer and mask decoding checks cancellation while unpacking cached bytes and validates ownership before reading the held file, writing tiles and completing each channel. The stored composite reader uses the same cancellation and ownership boundaries while checking expected samples. Regression source covers cached RLE rows, exact planar samples, cancellation and ownership checks at file reads and completion. Windows compile-only checking passed. The earlier rebuilt headless diagnostic opened the same file in about 157 seconds on a separate run. Both observations exceed the synchronous CLI request deadline. Controlled production-build profiling remains pending.

For [Pattern Fill](photoshop-fidelity.md#pattern-fill-in-0310-unreleased), use
independently saved RGB8 and RGB16 PSD and PSB files with an asymmetric
non-square tile, mixed tile alpha and a separate raster mask. Test both signs
of a quarter turn with signed X/Y offsets at 100% scale. Compare the whole
saved composite, change the angle and offset, then verify undo and redo.
Reopen each writer's standard `PtFl` and `Patt` output in Photoshop and check
editability, placement, mask samples and the merged image. Include an
older layout and files with linked placement, fractional offsets, unsupported
angles and mismatched composites to verify the corresponding protection paths.

For [Threshold](photoshop-fidelity.md), use independently saved RGB8 and RGB16
PSD and PSB fixtures with points at the cutoff and on both sides of it. Include
the RGB8 halfway tie and native16 samples that share a display byte. Verify
alpha, native raster and mask samples, clipping, level edits, undo and redo,
then reopen the standard adjustment and merged image in Photoshop. Keep
malformed or mismatched records protected.

For [Posterize](photoshop-fidelity.md), use independently saved RGB8 and RGB16
Photoshop PSD and PSB fixtures at several level counts. Include adjacent samples
on both sides of bucket boundaries and masked or clipped adjustments. Edit the
count, verify undo and redo, and reopen the saved standard adjustment in
Photoshop. Compare the original source channels and current merged image at
the original depth, including 16-bit palette values between byte-grid samples.
Reopen older Nearest and Legacy bins effects and verify their original rendering.

For [shifted two-point Curves](photoshop-fidelity.md), use independently saved
native8/16 Photoshop PSD/PSB fixtures with master and individual RGB inputs.
Include samples below, between and above the input points, as well as a masked
or clipped layer. Verify complete composite equality, drag both inputs, undo
and redo, then reopen the standard adjustment in Photoshop. Compare its
editable point coordinates, mask and current merged image at the original
depth. Earlier full-range and baked PeerBrush layouts also need readback.

For Photoshop splines, include RGB8 master and independent channel curves,
including a Red/Blue-only file with an omitted master and Green record.
Cover asymmetric and tightly spaced points and identical or conflicting extensions.
Include a combined RGB8 adjustment that exercises channel-first byte rounding
and even halfway ties before and after the master curve.
Verify point drags, addition and removal on the Photoshop coordinate grid,
then reopen the saved adjustment and merged composite in Photoshop. Use RGB16
fixtures to calibrate the remaining native rounding differences before
enabling editable multi-point Photoshop import at that depth.

The shared codec retains original native8/16 raster sources and validates supported style definitions, including disabled entries. Editable style import requires the complete stored white-matte RGB and alpha composite to match at the original channel depth. A failed render, unsupported descriptor or one-sample mismatch preserves the protected saved appearance. The RGB8 Multiply and integer-grid gradient fallbacks are independent, document-wide candidates. Each regenerates derived effect identities and requires the same exact comparison.

The preserved feature branch and v0.3 base have the same Photoshop/style and selection/transform implementation. Existing [codec tests](../tests/psd_clipping.rs) cover integer-grid/aligned gradients, destination-stop midpoint ownership, retained original sources, malformed disabled definitions, exact mismatch protection and editable PSD/PSB saves. Current family order is Pattern, Gradient, Color; the current fidelity guide now agrees with the [calibration correction](layer-styles.md#calibrated-photoshop-overlay-family-order-unreleased), while earlier evidence remains in the development history.

No additional importer was justified by this audit. Native gradient smoothness is an authoring foundation, tested against independent cubic expectations in [layer-style tests](../tests/layer_styles.rs). It is not an exact Photoshop Classic implementation: wider saved samples retain one-byte differences, so nonzero Photoshop `Intr` remains protected. Further PB-122 acceptance requires independently saved Photoshop fixtures covering the proposed interpolation/placement subset and the representative styled source, including its profile and unsupported structures. Recognizing another descriptor alone does not satisfy this requirement.

## PB-110: scalar coverage interpolation

Selected scale/rotation previously sampled a selection mask as premultiplied RGBA and used the resulting red channel. Canonical selection masks store the coverage value in all four channels. Interpolating between full coverage and zero therefore unpremultiplied the red channel back to 255 instead of retaining the fractional edge. For example, a three-quarter sample should retain 191 coverage, not 255. This could distort selection geometry used by later edits or copying even when the transformed artwork alpha was already fractional.

The shared transform path now bilinearly interpolates scalar red-channel coverage directly, matching `Coverage::value`. It continues honoring masks whose stored alpha differs from their coverage channel. Paint, selected mask and selected folder transforms share this path. Original source cutting, native raster interpolation, reservations, transaction guards and history remain unchanged.

Two focused regressions were added to [selection-system tests](../tests/selection_system.rs): one distinguishes red coverage from independent RGBA alpha; the other uses independently calculated quarter-position samples across native8/16 paint, mask and folder transforms. The latter checks all output coverage, holes/soft values, unselected native/hidden samples, unchanged mask-target color, preview parity, one-step undo/redo and PSD/PSB reopening.

## Verification and remaining acceptance

An RGB8 wand request on a 16000-square Windows headless document exposed an earlier refusal while admitting its runtime replay command. The normalizer now emits its keys in the order used by the exact JSON round-trip check. Replay still retains only supported options and remains outside the saved document. Windows compile-only verification passed. A subsequent full-size request exceeded the diagnostic's three-minute RPC window. A 512-square check reached native source preparation and reported inventory contention. Runtime selection and transform verification remain pending.

Wand guide staging now uses the compositor's tile map to skip stripes proven transparent. Occupied stripes still use the shared renderer. An unfeathered contiguous replacement stages coverage within the completed flood bounds and carries its document origin, including the contour coordinates. Feathered and combined selections keep their existing row path. Windows compile-only verification passed. Runtime acceptance for this increment remains pending.

The guide-only Windows build completed the sparse 16000-square RGB8 workflow. Independent cropped PNG checks verified selection coverage, duplication, move, rotation and scale, including fractional coverage and history. The Wand request took 761 seconds. With cropped coverage, the 16000-square Wand request took 134 seconds at RGB8 and 141 seconds at RGB16 in separate runs. Both passed the full workflow, with exact selection and artwork samples and undo/redo. The builds used different optimization settings, and background activity was uncontrolled. A two-project replay hit source-inventory contention while editing the second project.

Native edits, history moves, Open, Save and native source reads now share foreground admission across tabs. Recovery waits for idle native sources, yields to an arriving operation before its next guarded step, and finishes any required publication cleanup before allowing that operation to stage. Windows headless builds passed both tabs at 512 and 16000 square. The full-size replay independently checked RGB8 and RGB16 coverage, duplicated and transformed artwork, and undo/redo. The recovery catalog contained both final revisions after editing ended. Regression source covers foreground arrival and a checkpoint deferred by an edit in another tab. Desktop input and OS clipboard acceptance remain pending.

The same admission covers native clipboard staging, transfers between tabs and the inspection of native image imports. Windows headless verification covered exact RGB16 layer copy, destination undo and an atomic move with one history step per tab. Independent exports matched the transferred and unrelated native samples. Autosave recorded both move revisions after editing. Regression source covers a checkpoint deferred during transfer staging and a late reservation refusal.

Full-resolution Wand guides advance through whole transparent tile bands in one pass. The shared compositor renders occupied bands. Regression source covers a far-edge sample, skipped bands and the crop’s half-open boundaries. Windows compile-only checking passed.

Two alternating pairs on the same Windows host used the same sparse 16000-square RGB8 fixture and release optimization setting. The per-stripe build took 132.33 and 130.62 seconds. The build that skips whole transparent bands took 57.25 and 67.48 seconds. Independent cropped PNG checks verified exact selection coverage in all four runs. Background load was uncontrolled. These observations cover the headless Wand request.

Native row staging also advances over runs of zero tile columns in a populated band. Any nonzero channel, including RGB beneath zero alpha, still receives a native tile. Local Windows replays verified exact Wand coverage on sparse 16,000 × 16,000 RGB8 and RGB16 documents. The compile-checked regression source also covers hidden RGB and both row orders. The replays did not measure a speed change from column skipping.

Wand and refinement now stage the renderer’s RGBA8 guide samples directly. This removes the per-row expansion to 16-bit samples and the subsequent conversion back to 8-bit. The row writer keeps the same tile order, cancellation and sparse-channel checks. Windows headless replays verified exact Wand coverage on 16,000 × 16,000 RGB8 and RGB16 documents. Timing varied with concurrent work. RGB8 and RGB16 comparisons also verified exact edge-guided selection refinement across tile boundaries, undo/redo and preserved artwork. Native desktop verification remains pending.

Selection refinement derives bounds and contours from the validated scalar tiles in bounded bands. The selection retains those tiles for publication. A paired Windows headless replay on 257 × 259 documents matched the previous scalar output exactly at both native depths, including fractional coverage, undo, redo and unchanged native artwork. Compile-checked regression source also compares bounds, origin and contours with the existing scalar writer. Full-size refinement timing remains pending.

A second canvas press in the same input frame now keeps selection-sensitive
shortcuts from following the first release. A canvas release also cannot
attach those shortcuts to an earlier selection worker. Regression source
covers Copy and W with two gestures and with a blocked predecessor. Native
desktop reproduction of the reported recurrence remains pending.

The deferred shortcut guard also covers selected transforms and other
edits that replace or clear selection coverage. The native Move release
fixture queues Copy, verifies the moved coverage, and pastes the saved
pixels at the new document coordinates at both native depths.

When a keyboard selection change and a dependent shortcut share an input
batch, the later shortcut requires a fresh press. The native fixture
checks layer Copy before Select All, holds Cut and Move after it, and
checks that a fresh Copy uses the committed pixel selection.

A selection worker rejection now also stops document shortcuts in the frame that receives the rejection. A Copy or transform key arriving in that frame requires a fresh press before it can use the current selection or layer. The original error stays visible. Native8/16 regression source uses a real reservation refusal delivered through the asynchronous edit worker and checks artwork, clipboard dispatch, tool state, history and explicit retry. Runtime execution and native desktop reproduction remain pending.

Release-frame shortcuts use the selection commit receipt. If the gesture ends without a result or the receipt no longer matches the current revision, Copy, Cut, Paste and transform shortcuts require a fresh press. A cancelled contact retains that protection through button-up. Regression source covers a Wand release outside the workspace with prior coverage, a valid retry, and Copy and Move arriving after a cancelled marquee. Desktop verification remains pending.

The shared canvas input now takes a marquee's final sample from the primary-button release, including a subpixel move that crosses a selection boundary. W/E/R and Free Transform use the recorded press and release positions and the Shift state at release. Motion and pointer departure delivered later in the same input frame cannot change those endpoints. Regression source checks an exact marquee commit and undo, a selection-only W/E/R preview against its commit, and Free Transform's pending values after release. Native desktop input and regression execution remain pending for this change.

The analytic regressions passed with the combined engine/codec/protocol/UI suite: 979 tests at `3dc56e6`, then 981 at `9b3eace` after adding unrelated idle-dialog capture regressions. All-target Clippy passed with warnings. Native Windows checks at `f7f715d` verified actual native8/16 artwork/coverage, frozen proposal previews without source/history changes, exact commit/undo/redo, independent standard PSD/PSB white-matte RGB/alpha samples and editable reopening. The transform implementation and these analytic tests are unchanged between that native checkpoint and `9b3eace`. Generated inputs, captures and receipts remain outside public source.

This specific sampling defect does not establish the cause or resolution of the separately reported wand/copy/transform recurrence. Wider follow-on selection workflows, actual desktop clipboard transport, receiving-application round trips, macOS/Linux desktop and physical tablet/display acceptance remain separate checks. No tolerance was added to Photoshop composite matching, and protected unsupported originals remain protected.

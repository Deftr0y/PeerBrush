# Photoshop fidelity

PeerBrush 0.4 preserves the two-byte Photoshop print dot range in PSD and PSB resource 1019. Resident source format 139 and native Source36 check the retained bytes against the standard resource. Malformed or duplicate records, Duotone setup, alternate spot colors and EPS export options keep the saved composite read-only. Photoshop round-trip verification remains pending. [Adobe image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains validated legacy halftone screens and printer transfer curves in PSD and PSB resources 1012, 1013, 1015 and 1016. Working saves preserve the resource bytes and native image channels. Resident source format 138 and native Source36 bind the saved resources to the editable source. Malformed or duplicate records, custom halftone spot functions and unsupported document resources such as HDR Toning and ImageReady data keep the saved composite read-only. Photoshop round-trip verification remains pending. [Adobe Photoshop file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains Photoshop’s JPEG quality resource (1030) in editable PSD and PSB working files. It preserves the original bytes for supported Standard, Optimized and Progressive settings, including the optional seventh byte. This metadata is carried through working saves. PeerBrush exports use their own settings. Resident source format 136 and native Source36 bind the retained record to the standard resource. Malformed and duplicate records keep the saved composite protected at its original depth. Receiving-editor verification is pending. [Adobe image resource IDs](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [ExifTool JPEG quality fields](https://github.com/exiftool/exiftool/blob/master/lib/Image/ExifTool/Photoshop.pm#L386-L418)

PeerBrush 0.4 preserves Photoshop’s one-byte Watermark resource and empty URL List records in editable PSD and PSB files. Resident source format 135 and native Source36 bind the retained records to the standard resources. Files with nonempty URL lists, malformed records, Slices, Count Tool data or Path Selection State open read-only with the saved composite. Receiving-editor verification is pending. [Adobe Photoshop file format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains the legacy Macintosh Print Manager and Photoshop CS2 Print Info records as opaque bytes within the combined 1 MiB resource limit. It also retains empty Color Samplers records in both documented resource slots. Files with sampler points or malformed sampler records open read-only so canvas edits cannot discard or misplace those points. Resident source format 134 and native Source36 bind the retained bytes to the standard PSD and PSB resources. [Adobe Photoshop file format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains Photoshop's open and closed folder markers when saving editable PSD and PSB layer trees. The Layers panel uses those markers when the project first opens. Expanding or collapsing a folder in the panel changes the current tab view. The markers travel with their folders independently of layer visibility and compositing. Resident source format 131 binds the markers to the standard layer records. Animation scene groups remain protected. Receiving-editor verification is pending. [Adobe's section divider specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PSD and PSB import decode Unicode layer names with or without a trailing UTF-16 NUL. Protected-file diagnostics use the same decoder and shorten long names at complete character boundaries. Both writers preserve the full Unicode name and shorten the legacy 255-byte name field at a complete character boundary. Malformed Unicode names keep the saved composite read-only. [Adobe's Unicode string and layer-name specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Unreleased PSD and PSB saves carry through Photoshop's standard print border and background color records. PeerBrush validates the border units and the size of the background color record, then preserves the complete resource bytes, including native color words and names. Malformed or duplicate records keep the original protected. These settings travel with the document for Photoshop to use. [Adobe's image resource and color structure specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Unreleased PSD and PSB working files retain the standard IPTC, EXIF, XMP and caption digest records that fit a combined 1 MiB limit. PeerBrush preserves resource names, payloads and padding byte for byte. The payloads remain opaque. A working save binds those bytes to its editable source, and a conflicting or duplicate record keeps the document protected. A compatible flattened copy can retain valid File Info at the original channel depth. [Adobe's image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 also retains the legacy Pascal caption, File Info URL and Unicode Workflow URL within the same limit. It checks caption length and Workflow URL encoding before retaining the original bytes. URL records remain document data and are never opened by the codec. Resident source format 129 binds the additional records, and native Source36 compares them against the standard resource bytes. Receiving-editor verification is pending. [Adobe's image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Unreleased PSD and PSB saves also retain Print Information and Print Style records up to 16 KiB each within the same combined limit. PeerBrush validates the version 16 descriptor structure and preserves the bytes for Photoshop to read. Malformed and duplicate records keep the file protected. Resident source format 130 and native Source36 bind the records to the standard file. [Adobe's image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains macOS NSPrintInfo and Windows DEVMODE in PSD and PSB within the same combined 1 MiB limit. These records stay opaque and travel byte for byte, including their names and padding. Empty, duplicate or conflicting records keep the file protected. Resident source format 132 and native Source36 bind the opaque data to the standard resources. Photoshop receiving-editor verification is pending. [Adobe's image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

PeerBrush 0.4 retains Photoshop’s Measurement Scale descriptor in PSD and PSB. Descriptors up to 16 KiB retain the original resource bytes and are bound to the editable source in resident format 133 and native Source36. PeerBrush preserves the scale for Photoshop without applying it to document pixel coordinates. Invalid or duplicate records keep the document protected. [Adobe’s image resource specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Windows headless checks used synthetic RGB8 and RGB16 documents in both formats. After a layer rename and save, independent parsing found exact File Info records and native raster and merged samples. Each unmodified save reopened for editing. Changing an XMP byte made the saved composite open read-only. Receiving-editor verification remains pending.

A second Windows replay verified all five supported resource IDs together, including IPTC and both EXIF IDs, with identical bytes and record order across all four depth and format combinations.

The supplied RGB8 Photoshop document saved to PSD and PSB. Independent parsing verified all three File Info records, ten other image resources, six global records, standard layer and mask samples, and the full-resolution merged composite in both. Each saved file reopened with four editable layers in PeerBrush. Adobe Photoshop receiving-editor verification remains pending.

Unreleased PSD and PSB import and save preserve ordered lists of up to 16 solid Normal inside Strokes on an unmasked, unclipped Normal paint layer, within the shared 32-effect limit. The list keeps disabled entries and each stroke's width, color and opacity. Inside Strokes retain the layer alpha for later styles. Lists containing outside or center placements remain protected. Both writers retain standard native raster samples and a current merged composite. Resident source format 125 and native readback bind the descriptor order. Editable import requires binary source alpha and an exact match to the entire original-depth saved composite. [Adobe's layer-style reference](https://helpx.adobe.com/archive/en/photoshop/cc/2015/photoshop_reference.pdf)

Windows headless checks round-tripped synthetic 17 × 11 RGB8 and RGB16 fixtures in both formats through the resident codec and native Open, Save and reopen. The three Stroke entries retained their order, disabled middle entry and exact settings. Independent parsing matched every standard raster and merged sample in the native saves. Resident checks kept partial-alpha input and lists with an outside placement protected. Adobe Photoshop receiving-editor verification remains pending.

Unreleased PSD and PSB import and save preserve ordered lists of up to 16 supported hard Inner Shadows within the 32-effect layer limit. The list retains disabled entries and supported Global Light links. Each shadow casts from the layer's original alpha. The reader requires the entire original-depth saved composite to match, and both writers retain the native raster samples and a current merged composite. Resident source format 123 and native readback bind the ordered definitions. Multiple Drop Shadows remain protected. Receiving-editor acceptance is pending. [Object-based layer effects format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Inner Shadow list descriptor mapping](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts)

Unreleased PSD and PSB import recognizes legacy `lrFX` common state, version 2 RGB Color Overlay and version 0 or 2 hard Inner Shadow. Editable legacy shadows use whole-degree casts and whole-pixel distances on unmasked, unclipped layers. A hard legacy Drop Shadow requires a matching object-based Drop Shadow with an explicit knockout setting. Companion effects must match the first modern effect of their kind at legacy precision. The modern definition retains its full settings. Standalone supported effects preserve legacy RGB words, byte opacity, blend and enabled state. Invalid, ambiguous or conflicting records keep the original protected. Editable import requires a complete original-depth match to the saved composite. Supported saves use the current object-based style record with native raster and mask channels and a current merged composite. Photoshop acceptance remains pending. [Adobe legacy effects format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Legacy effect field mapping](https://github.com/Agamnentzar/ag-psd/blob/master/src/effectsHelpers.ts)

Unreleased PSD and PSB saves keep a supported bitmap Pattern Overlay as a standard layer style. It can accompany supported Gradient and Color Overlays. Unlinked patterns at 100% scale retain right-angle rotation, whole-pixel offsets within ±32,768 pixels, opacity, blend and enabled state. Pattern Fill and Pattern Overlay share an embedded asset when they reference the same bitmap source. The writers preserve native layer and raster mask samples and include a current merged composite. Resident source format 121 and native readback bind nonzero placement to the descriptor and asset bytes. Earlier zero-placement and baked saves retain their restoration paths. Editable import requires an exact original-depth composite match. Photoshop acceptance remains pending. [Adobe's embedded pattern format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Pattern Overlay descriptor mapping](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts)

Unreleased PSD and PSB writers retain up to 16 supported Gradient Overlays on one paint or Solid Color layer, within the shared 32-effect limit. Multiple gradients use Photoshop's `gradientFillMulti` list and preserve order and enabled state. Supported Pattern and Color Overlays, hard shadows and solid Strokes can share the stack in Photoshop's order. Standard gradients retain exact stop positions, midpoints, opacity, blend and supported placement. Saves carry the original native-depth raster and mask channels and a current merged composite. Resident source format 120 and native readback bind the ordered list. Earlier single-gradient and baked saves retain their restoration paths. Editable imports still require an exact original-depth match to the saved composite. Receiving-editor acceptance remains pending. [Gradient Overlay descriptor mapping](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts)

Unreleased PSD/PSB support retains Photoshop Print Flags and version 1 Print Flags Information through edits, compatible copies and saves. Both writers emit the original standard records, including the required padding for the nine-byte flags record. Duplicate, malformed or conflicting embedded-source records keep the original protected. These values describe Photoshop print settings and travel with the document. Runtime and receiving-application verification remain pending. [Adobe's format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Unreleased PSD/PSB support also retains the standard Copyright Flag through editable opens, edits and saves. An explicit false flag stays distinct from an absent record. Duplicate, malformed or source-conflicting flags keep the original protected. [Adobe's format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

The standard Global Angle resource is preserved from 0 to 359 degrees through PSD/PSB edits and saves. Unreleased hard Inner and Drop Shadows can use a valid explicit Global Angle when the original-depth composite matches exactly. Both writers preserve the link and the effect's stored local angle in a standard style. The shadow inspector’s **Use Global Light** control links an effect to the project angle. The first link creates the project angle from the shadow’s direction, rounded to a whole degree. Turning off the link keeps the current rendered direction. Right-click the canvas dimensions in the status bar and choose Global Light to preview and change the document angle in one undoable edit. Editing an individual shadow's direction switches that effect to local light. Pasting into a project with a different or absent Global Angle keeps the copied shadow's direction as local light. Files with linked effects and missing, malformed or conflicting Global Angle records remain protected. Receiving-editor acceptance remains pending. [Adobe's format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Global Light behavior](https://helpx.adobe.com/photoshop/desktop/create-manage-layers/apply-layer-effects/set-a-global-lighting-angle-for-all-layers.html)

Unreleased Linear Light support reads and writes the standard `lLit` layer
blend in PSD and PSB. RGB8 uses the calibrated Photoshop midpoint of 128.
RGB16 uses a native midpoint of 32768 and retains the original channel words.
Editable import requires an exact match to the saved composite. [Adobe's format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop RGB8 calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/blend-modes.md#vividlinear-light-hard-mix-darkerlighter-color)

Unreleased Fill support keeps standard `iOpa` separate from overall Opacity
for paint with no enabled effects and Solid Color layers without effects. The inspector's
Fill control and `layer.update` edit the same standard byte. PSD and PSB keep
the original layer samples and write a current merged composite. Editable import
requires exact saved-composite agreement. Color Burn, Linear Burn, Color Dodge,
Linear Dodge, Difference, Vivid Light, Linear Light and Hard Mix apply their
own Fill-adjusted overlap colors. Fill separately scales the source-only alpha.
Both native depths can use the preview shader. Color Burn and Color Dodge
correct their wide quotients with exact integer comparisons. In a clipping
group, each clipped layer's Fill
changes the group's color within the base's shape. The base's Fill applies
when the combined group blends into the document. PeerBrush retains supported disabled paint effects in its editable sources. Restore full Fill before enabling those effects. Imports with partial Fill and enabled effects remain protected. [Adobe's opacity controls](https://helpx.adobe.com/photoshop/desktop/create-manage-layers/apply-layer-effects/set-layer-opacity-and-blending-modes.html) · [PSD Fill key](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/constants.py) · [Photoshop clipping calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/ps-compat.md#clipping-masks)

PSD and PSB preserve a disabled **Blend Clipped Layers As Group** flag on
plain Normal paint and Solid Color clipping bases at full Fill. In this
configuration the measured Photoshop composite follows the existing clipping
order. The layer context menu and `layer.update` can restore grouping before
changing the base blend, adding effects, or reducing Fill. Non-Normal and
styled disabled bases remain protected. Editable import still requires an
exact saved-composite match. [Photoshop clipping calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/ps-compat.md#clipping-masks)

Vivid Light, Pin Light and Hard Mix use the standard `vLit`, `pLit` and
`hMix` layer records. Vivid Light rounds the darkening and lightening ramps
separately. Hard Mix selects the threshold at the native midpoint. All three
preserve native layer samples and share the same exact-composite import policy.
Photoshop receiving-editor acceptance remains pending. [Adobe's blend-mode records](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Vivid Light and Hard Mix RGB8 calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/blend-modes.md#vividlinear-light-hard-mix-darkerlighter-color)

Darker Color and Lighter Color choose an entire RGB color using rounded
weighted luma. Ties retain the backdrop. Hue, Saturation, Color and Luminosity
use non-separable blending in the document's native sample units. PSD and PSB
retain the standard blend key and original layer channels. The saved composite
must match exactly for editable import, including Photoshop rounding near
half-luma boundaries. [Adobe's blend-mode records](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [RGB8 Photoshop calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/blend-modes.md#vividlinear-light-hard-mix-darkerlighter-color) · [W3C non-separable blending](https://www.w3.org/TR/compositing-1/#blendingnonseparable)

Unreleased Threshold support keeps standard `thrs` adjustments editable in
RGB8 and RGB16 PSD and PSB. The Level control accepts integers from 1 through
255. Threshold compares weighted RGB luminosity using the document's native
depth. RGB8 halfway samples fall on the darker side. RGB16 compares native
samples on the Photoshop tonal grid. Direct inspection of the saved fixture
channels matched 38,636 RGB8 pixels and 4,436 RGB16 pixels in opaque regions
where one mask was fully active. [Adobe adjustment tags](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [RGB8 fixture](https://github.com/psd-tools/psd-tools/blob/main/tests/psd_files/adjustments/threshold_rgb.psd) · [RGB16 fixture](https://github.com/psd-tools/psd-tools/blob/main/tests/psd_files/adjustments/threshold_16bits_rgb.psd)

Threshold is available as an effect, adjustment layer and whole-image filter.
The renderer preserves alpha and underlying source pixels. Both save paths
write a standard Threshold record for a supported full-strength adjustment
with its raster and mask channels and the current merged composite. Resident
source format 57 protects editable Threshold settings, including in stacks
that require the disclosed raster fallback. Editable import requires an exact
match to the complete saved composite. Unsupported Threshold record variants
remain protected. Regression execution and receiving-editor acceptance are
pending.

Unreleased Posterize support keeps standard `post` adjustments editable in
RGB8 and RGB16 PSD and PSB. It accepts a level count from 2 through 255 with a
plain record and optional zero padding. Photoshop bins selects from the
depth-specific tonal grid and preserves alpha and underlying layer pixels.
At 16 bits, the bucket and its palette step use the Photoshop 0–32768 scale,
then the palette step is rounded to the full native word range. This matched
all 13,308 native channel samples in four opaque, fully masked fixture panels
covering 3, 7, 13 and 21 levels. Editable import still requires the entire
saved composite to match exactly. [Adobe adjustment tags](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [RGB8 calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustments-calibration.md#levels-stage-order-and-posterize) · [RGB16 fixture](https://github.com/psd-tools/psd-tools/blob/main/tests/psd_files/adjustments/posterize_16bits_rgb.psd)

The Posterize control offers Nearest and Photoshop bins. Earlier Photoshop
settings retain their rendering and appear as Legacy bins in the inspector.
Both PSD/PSB writers emit a standard record for a supported, full-strength
Photoshop bins adjustment at either depth. Legacy bins retain the earlier
standard RGB8 path and the disclosed RGB16 raster fallback. The writers include
the original raster and mask samples and a current merged composite. Resident
source formats 55 and 56 and native Source36 mapping markers distinguish the
earlier and current mappings, including disabled effects. Regression execution
and receiving-editor acceptance remain pending.

Unreleased Curves support preserves two-point adjustments as editable linear
Curves at RGB8 and RGB16, including shifted black and white input points. RGB8
also supports three through sixteen points using Photoshop's natural cubic
spline with flat output beyond the input endpoints. The spline matched all
27,648 saved channel samples in each of two Photoshop RGB8 fixtures. The graph keeps
standard points on their coordinate grid when they are dragged. Added points
on a spline also use that grid. The codec supports a master RGB curve by
itself or independent Red, Green and Blue curves with a neutral master. An
omitted master or color record is neutral. Any duplicate Curves extension
must agree with the primary records.
RGB8 also supports a nonneutral master together with individual color
curves. Each color curve rounds to the nearest byte before the master runs.
Both stages round exact halfway values to even. The model matched all 86,931
eligible channel samples across three combined adjustments in the
[Photoshop RGB Curves fixture](https://github.com/psd-tools/psd-tools/blob/main/tests/psd_files/adjustments/curves_rgb.psd).
The complete original-depth saved composite must match before the import is
editable. Both writers emit standard `curv` for canvas-sized, full-strength
adjustments with exactly representable points. Supported blends, clipping
and raster masks use the existing adjustment path. Resident formats 52 and 53
bind full-range standard records. Format 54 binds shifted input points,
format 58 retains the Photoshop spline settings, and format 59 binds combined
Curves. Native Source36 marks the combined channel mapping separately.
Native Source36 compares the actual standard records and native channels and
continues to recognize earlier baked layouts.
RGB16 multi-point or combined Photoshop curves, pencil maps and conflicting
extensions stay protected. PeerBrush can retain its native16 spline settings
through the disclosed raster fallback. The record layout follows the
[ag-psd Curves implementation](https://github.com/Agamnentzar/ag-psd/blob/master/src/additionalInfo.ts)
and the [psd-tools Curves implementation](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/adjustments.py).
The input controls follow [Adobe's Curves documentation](https://helpx.adobe.com/photoshop/desktop/adjust-color/color-corrections/adjust-color-and-tone-with-curves.html). The [RGB8 master fixture](https://github.com/storytold/photocraft-corpus/blob/f5b1178cab15309e05b7b504545df3c701f6d8fd/photoshop/adjustments/rgb8/curves.psd), [RGB8 sparse channel fixture](https://github.com/storytold/photocraft-corpus/blob/f5b1178cab15309e05b7b504545df3c701f6d8fd/photoshop/adjustments/rgb8/curves-per-channel.psd), and [RGB16 master fixture](https://github.com/storytold/photocraft-corpus/blob/f5b1178cab15309e05b7b504545df3c701f6d8fd/photoshop/adjustments/rgb16/curves.psd) document the calibration boundary.
Receiving-editor acceptance is pending.

Unreleased Levels import/save now recognizes the master RGB `levl` record
when individual channel records are neutral, or independent Red/Green/Blue
records when the master is neutral. It preserves input/output endpoints,
gamma, supported blending, clipping and raster masks at the original depth.
Editable import still requires an exact complete saved-composite match.
Master adjustments accept gamma 0.1–9.99, valid one- and two-step input
intervals and the full standard input endpoint range. Imported master records
use explicit exact input mapping, as do individual channel records. Combined
RGB8 master and channel records with all gamma values at 1 can also open
editably. They apply the channel and then the master, rounding each stage to
the nearest byte with ties upward. A saved Photoshop fixture matched all 16,000
opaque Red and Green samples in its linear region, including a halfway sample
that distinguishes the rounding rule. Combined records with nonlinear gamma or
at RGB16, and unknown extensions, remain protected. Neutral `Lvls` extensions
accept the declared records with optional zero padding to four-byte alignment.
Four Photoshop-authored 62-record tags include two such bytes in the tag
length. Comparing 95,850 opaque channel samples from the same fixture found
differences with the tested combined mappings. [Adobe's format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop Levels fixture](https://github.com/psd-tools/psd-tools/blob/main/tests/psd_files/adjustments/levels_rgb.psd)

Both save paths emit standard Levels for a canvas-sized adjustment with one
enabled full-strength Levels effect whose endpoints and gamma map exactly to
the standard integer fields. Other stacks keep the disclosed raster fallback.
Resident source format 48 and later bind the Levels payload and blend to the
editable source. Format 49 protects exact input mapping and gamma above 5;
format 50 protects channel settings, including disabled or neutral channel arrays.
Native Source36 uses Levels semantic marker version 3 for channel settings.
Resident source format 61 and native marker version 4 bind the RGB8 two-stage
mapping. The combined Levels inspector edits endpoints in byte units. Gamma
changes outside the calibrated mapping use the disclosed raster fallback on
save. Applying these settings to a native16 document uses the native-precision
renderer and the same save fallback.
Older baked layouts and legacy minimum-span mappings remain restorable.
Native Source36 compares its standard records and original native channels. Save review reports the supported
record as an adjustment. Standard raster/mask channels and the current composite
remain mandatory. Photoshop/native acceptance and regression execution are pending.

Unreleased PSD/PSB import preserves bounded raster mask samples outside the
layer frame for linked and unlinked masks. It retains the original mask at
native 8/16-bit depth and checks the complete saved composite before making
the document editable. Masks that exceed retained-source limits keep their
protected saved appearance.

Native Open decodes the original mask and its current projection into
provisional native tiles in one decoding pass. The retained translation and
both planes reach the document only after its native render matches the saved
composite. The encoded-file and off-frame mask retention limits still apply.

Both writers keep the current mask inside the layer frame and write off-frame
samples from a retained, integer-translated paint step with an optional
constant fill. Resident source format 43 binds the expanded linked mask
record. Source36 verifies the standard channel against the retained source
and can reopen older files that wrote only the current mask frame.

Native Open accepts supported conventional RGB8/16 PSD and PSB geometry
up to 16,000 pixels per edge. Supported native files use PeerBrush's
64 GiB held-file ceiling and must fit the separate source budgets.
Compatibility decoding still uses the 256 MiB encoded-file limit. Native Open
admits supported layer styles and Pattern Fills by their descriptor, asset and
effect sizes. It also accepts the exact PeerBrush sRGB working profile. Raster
channels can stream beyond the compatibility decoder's file limit. Editable
adoption still requires an exact saved-composite match at the original depth.
If the original gradient placement differs, Native Open can check the same
single document-wide integer-grid placement as the compatibility decoder.
It compares the alternate using the already staged raster and mask tiles.
Beyond the resident limits, Open can display a verified saved composite
as a native read-only preview. The canvas and agent previews render that
composite through the shared engine, with supported ICC conversion at the
original bit depth before display projection. Preview requests for original
layers, masks and selection imagery are refused. The source remains read-only. The
explicit editable-copy action creates a new unsaved native document in the
current tab. Profiled copies require explicit sRGB conversion consent.
Supported profiles convert at the original depth, including native 16-bit
channels and alpha. Unsupported profiles remain protected.
See [large document limits](large-documents.md).

Unreleased Invert adjustment import covers empty `nvrt` definitions with supported
layer blend modes, including zero-size Photoshop raster records and document-space raster
masks. An Invert layer can also import with a standard clipping relationship to a
raster sibling below it. The complete native 8/16-bit rendering must exactly
match the saved composite before editing is allowed. Styled, pass-through
or non-canvas adjustment records, malformed clipping or adjustment definitions,
and unrelated unsupported resources remain protected.

Saving an enabled, full-weight Invert-only adjustment with a supported blend mode and its original canvas
frame writes standard `nvrt` data, original native raster channels, the current
mask and a current merged composite. A supported clipping relationship to a
raster sibling remains in the standard layer tree. Save reports identify these
as editable adjustments. Other adjustment stacks retain the explicit baked-save
report and supplementary editable PeerBrush sources. Resident format 40 checks
the standard clipping flags against the editable sources. Older baked saves
retain their previous restoration rules, including in native Source36. Resident
format 44 binds non-Normal Invert blend modes to the standard records. Native
Source36 verifies that layout and replays earlier baked adjustments separately. Test
execution and receiving-editor acceptance are pending.
This implements the empty adjustment definition in [Adobe's Invert specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/), not general Photoshop adjustment compatibility.

Unreleased Solid Color import recognizes a canvas-sized `SoCo` fill with plain
RGB values that fit the editable fill model exactly. Empty raster bounds are
supported. Opacity, standard raster masks and supported clipping stay editable
when the complete render matches the saved PSD/PSB composite at its original
8- or 16-bit depth. Normal Solid Color layers can retain styles admitted by
the existing layer-style parser, including Color Overlay with a raster mask.
Unsupported style properties and combinations retain protection.
Fractional channel values, unsupported descriptors and
render mismatches retain the protected saved composite. Both writers emit
standard `SoCo` for canvas-sized, effect-free RGB fills with opaque color. They
also write the native-depth raster and mask channels and current merged
composite. The layer's separate opacity remains editable. Fill layers outside
this shape retain their raster appearance and editable PeerBrush sources, with
an explicit save-review notice. Resident format 41 binds the standard color to
the editable definition. Older raster-only saves retain their restoration path.
Styled fills save their rendered standard raster and editable PeerBrush color,
effects and mask sources. Native execution and Photoshop acceptance are pending.
See [Adobe's Solid Color block specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/).

Unreleased Gradient Fill import recognizes bounded `GdFl` descriptors
using the existing gradient parser's cardinal-axis geometry and explicit RGB
stops. The editable representation is an opaque fill with a Gradient Overlay
effect with **Apply overlay coverage** selected. Color and opacity stops
remain independent, including transparent ramps. Raster masks and original
8/16-bit depth are retained, subject to the complete saved-composite comparison.
Dithering, unknown
properties, additional styles and unsupported definitions remain protected.
Both writers emit standard `GdFl` definitions for canvas-sized opaque base fills
with one enabled, full-strength Normal gradient effect, independent gradient
coverage and integer-grid geometry. The color/opacity stop positions must map
exactly to Photoshop's 0–4096 positions, and midpoint ratios to integer
percentages. Cardinal angles, supported shapes, scale and offsets retain their
validated definitions. Other combinations retain the disclosed raster fallback.
Every save also carries rendered standard raster/mask channels and the current
merged composite. Save review identifies standard Gradient Fill records.
Resident source format 46 and later bind the emitted descriptor to the editable source;
format 45 retains the earlier raster-only coverage layout. Native Source36
verifies the corresponding standard layout and preserves older restoration.
The coverage setting is shared by UI, MCP, CLI and native regional
rendering. Native execution and Photoshop acceptance remain pending. Descriptor mapping
follows [ag-psd's Gradient Fill reader](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts).

Unreleased vector-mask handling preserves version 3 `vmsk` records with no
paths and an explicit full effective fill. This includes an inverted empty
initial fill and unlinked masks. Disabled pathless masks can retain either
initial fill or omit the record. Resident format 140 binds the omitted-fill
case to the standard mask bytes. The default transparency-shape flag (`tsly`)
is also retained. The global vector-mask flag (`vmgm`) can be enabled when the layer
has a preserved no-op vector mask. Conventional import checks the complete
original-depth composite before allowing edits. Both writers preserve these
standard records byte for byte. Resident format 42 binds the original full
unflagged vector mask, and format 51 binds the expanded metadata. Source36
checks the exact standard records. Active paths, active empty coverage, unknown
flags and `vsms` records remain protected. The read-only report identifies the vector
mask and summarizes its path records or explains which record could not be
interpreted. See [Adobe's path and vector-mask formats](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/).

When a preserved no-op vector mask accompanies a raster mask, the importer can
read the raster mask from the real user-mask channel (`-3`). The rendered
mask (`-2`) must have the same bounds, background, flags and native samples,
including off-layer pixels. The two planes may use different compression.
PSD and PSB saves regenerate both standard planes from the current raster
mask, retain the vector-mask record and write the current merged composite.
Resident source format 60 binds the paired layout. Native readback also checks
both planes and retains support for earlier single-plane saves.

Unreleased PSD and PSB import supports Photoshop Density on one raster user
mask, including retained samples outside the layer frame and an explicit
zero feather. A single raster mask with only an explicit zero Feather also
opens for editing when its complete saved composite matches. Saves normalize
that zero setting to a plain raster mask and keep its original samples.
Density is editable as a percentage in the mask stack. A final, full-strength
Density step on a single raster mask saves as standard Photoshop metadata.
The standard mask channel carries the native 8- or 16-bit samples before
Density, and the current merged composite includes Density. Save Review
reports mask steps that still need baking into the standard channel. Nonzero
feather, vector-mask parameters and paired density masks remain protected.
[Adobe Photoshop file format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Adobe mask density controls](https://helpx.adobe.com/in_hi/photoshop/how-to/fine-tune-layer-masks.html)

Unreleased document metadata preservation covers empty `Patt` resources, RGB
filter-mask display settings (`FMsk`), disabled credentials with an empty identity
(`CAI `), OCIO explicitly set to none with empty display/view settings, unused
generator records (`GenI`) and the recognized `cinf` compositor-information form.
Every field, version and payload is validated. Active or unfamiliar variants,
duplicates and unknown tags remain protected. These records are retained as
standard tagged blocks, including the specified PSB length widths; private
sources must agree with their exact bytes. Metadata-bearing imports also require
an exact native composite match. Format 37 protects these resident sources;
Source36 retains them as admitted metadata through history and standard saves.
No source artwork or credential identity is included in the regression fixtures.

The disabled version 3 `CAI ` form also accepts the eight zero bytes Photoshop can include at the end of the descriptor. Both writers preserve the complete block. Nonzero trailer bytes, active credentials and a populated identity keep the original protected. [Observed version 3 descriptor layout](https://github.com/Agamnentzar/ag-psd/blob/master/src/additionalInfo.ts#L4024-L4040)

The `FMsk` and `cinf` block structures follow [Adobe's specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/).
Descriptor versions for the additional tags are cross-checked against
[psd-tools' tagged-block reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/tagged_blocks.py).
Admission of inactive forms is limited to the explicit fields described above;
these references do not establish support for active OCIO or credentials.

Unreleased source-resource protection: multiple PeerBrush `PBR1` resources are
ambiguous and retain a read-only standard preview, including identical duplicates.
A supported resource cannot hide an unsupported source definition by appearing
after it. Native 8/16-bit samples and the original file remain protected. This
guard does not introduce a new source format or expand the canvas limits.

PeerBrush 0.2 implements the documented RGB subset: standard raster clipping, pass-through folders, full layer locks, supported RGB ICC previews and explicit native-depth sRGB copies. Unsupported Photoshop settings remain protected. The v0.1.4 checkpoint predates these changes; [v0.2 packages](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.2) contain the documented support.

## Pattern Fill in 0.4.0 (unreleased)

An RGB `PtFl` layer can open as an editable fill with a bitmap pattern
when it references one embedded tile, explicitly uses unlinked placement at
100% scale with right-angle rotation and whole-pixel offsets within ±32,768
pixels, and matches the saved composite at the original channel depth. The
bitmap's alpha controls fill coverage independently of its color, and the
raster mask remains editable. Fractional offsets, other angles, unknown
descriptor properties, and ambiguous or unused assets remain protected.
Both PSD/PSB writers use a standard `PtFl` record and an embedded `Patt` bitmap
for this subset. The layer also carries current raster and mask channels.
Save Review identifies these editable Pattern Fills. Reopen checks the standard
definition and asset against the editable source. Definitions outside the save
subset use standard raster channels and retain their editable PeerBrush data.
Photoshop receiving-editor verification remains pending. [Adobe file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Pattern descriptor reader](https://github.com/Agamnentzar/ag-psd/blob/master/src/descriptor.ts#L1833)

## PSB support — unreleased PB-104

Open or drop `.psb` files into independent project tabs; Save/Save As selects PSD v1 or PSB v2 from the destination extension. UI, MCP and CLI share the codec, current source/file guards and protected-copy behavior. PSB keeps native 8/16-bit RGB source, mask and merged channels. Standard layers/groups, clipping, baked effects, ICC handling and supplementary editable sources follow the PSD behavior below; PSB does not make unsupported Photoshop objects editable. Unsupported global mask settings, unknown global features and conflicting global layer blocks remain protected in both formats. In RGB16 files with `Lr16`, a primary layer table containing records or extra data keeps the document read-only. Native PeerBrush saves also verify the signed layer count and the RGB16 transparency marker before restoring editable sources.

### Preferred per-layer global mask settings (unreleased)

The codec retains the documented preferred global-mask kind `128`, which uses mask values stored per layer, without applying its overlay settings to rendered pixels. Support is limited to overlay color-space value `0`, opacity 0–100 and 13–16-byte payloads with zero filler. These bytes survive editing, undo and standard PSD/PSB saves alongside actual raster/mask channels and a current merged composite. Supplementary sources must agree with the standard settings; they cannot silently drop or replace them. Other kinds, color spaces, extensions and malformed settings remain protected. Explicit flattened copies clear the original document settings.

This follows [Adobe’s global layer mask structure](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/). It removes one false protection case in otherwise supported raster documents. Photoshop styles, embedded patterns, unmanaged profiles and other unsupported structures still require protection; this change does not make the supplied styled document editable.

PSB structural layer-section, layer-info and channel lengths are 64-bit; its RLE row counts are 32-bit. Only the specified wide tagged blocks use 64-bit payload lengths. Resources, layer extra data and global-mask lengths remain 32-bit. Parsing rejects truncated/overflowing lengths and bounds RLE row scratch before allocating it. The writer always includes current standard raster/mask data and a merged composite. The implementation follows [Adobe’s file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) and recognizes additional wide keys used by [psd-tools](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/tagged_blocks.py), while protecting their unsupported contents.

The existing **8192 pixels per edge, 32 MP canvas, 100 editable layers, 200 standard layer records, 512 MiB source tiles and 256 MiB encoded file limits still apply**. PSB files beyond those limits, non-RGB modes and unsupported channel depths report errors. The [PB-105 investigation](large-document-investigation.md) explains why valid canvases can exceed save limits and proposes streaming/out-of-core work before raising them. Independent raw/RLE/ZIP/prediction fixtures exercise both depths, masks and protected sources; no broader Photoshop or receiving-application compatibility is claimed.

## Protection diagnostics — unreleased PB-122

Unreleased PB-122 corrects false read-only fallback for default blending ranges that include transparency or mask channel pairs. Those pairs must be neutral and fit the layer's bounded channel count. The native **Why read only?** action explains why a protected file is displaying its saved appearance, with the codec's specific reasons and original bit depth. The explicit flattened-copy action copies that appearance at the original precision.

Large PSDs and PSBs that use the protected streamed composite can also show the original indexed layer records in **Why read only?** and in MCP/CLI `source_structure`. The report includes bounded names, folder relationships, document bounds, visibility, mask presence and source tags. Known unsupported text, Smart Object, placed-layer, artboard and filter-effect records carry a layer-specific reason. The desktop panel can find a source layer by name, reason or tag. The report uses the same held file as the composite. Malformed folder boundaries suppress the hierarchy, and a refused diagnostics allocation leaves the saved preview available with source inspection unavailable. These records are structural observations. They carry no editable layer IDs or decoded source rasters. [Adobe's tagged block specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

The bounded channel-pair layout follows [Adobe's layer blending ranges specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/). Default transparency pairs must not be mistaken for a non-default Blend If setting. Color profiles, layer-style sources and unknown Photoshop features retain their existing protection independently.

## RGB8 Multiply rounding in 0.4.0 (unreleased)

PSD and PSB import can qualify an alternate RGB8 Multiply rule for unclipped paint layers over opaque pixels. The renderer rounds the layer opacity times mask coverage and the channel product to bytes before interpolation. The regular interpretation is tried first. The alternate is retained for edits and saves only when it reproduces the entire standard saved composite. The standard files still carry the Multiply mode, raster and mask channels, and the current merged image. Translucent pixels and 16-bit rendering use their existing blend calculations.

## Blend If in 0.4.0 (unreleased)

**Blend If** in the layer footer edits Gray, Red, Green and Blue ranges on unclipped paint and Solid Color layers without effects. These ranges work with the supported layer blend modes and all Fill values. **This Layer** uses the incoming color. **Underlying Layer** uses the rendered backdrop and passes through fully transparent areas. Drag the black and white handles along the tonal ramps. Move the two handles at either end apart to create a transition. Expand **Exact values** to enter thresholds directly. The canvas previews each gesture and undo records the completed edit. MCP and CLI expose the same values as `blend_if` and accept them through `layer.update`.

Blend If scales pixel coverage while the eight special Fill modes keep their separate Fill-dependent color calculation. [Adobe's blending options](https://helpx.adobe.com/uk/photoshop/using/layer-opacity-blending.html) and [Patchy's rendering calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/layer-effects-render.md#dialog-blend-if-and-satin).

Unclipped paint layers may retain disabled effects alongside active ranges. An imported style still has to pass descriptor validation and the exact native-depth composite check. PeerBrush saves retain the disabled definitions and original raster data, and the standard layer keeps its raster channels and ranges. Clear Blend If before enabling an effect on that layer. Photoshop can render active effects independently of the pixel gate, so those imported combinations remain protected. [Photoshop render calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/layer-effects-render.md#dialog-blend-if-and-satin)

Folders without effects can also use Blend If. **This Layer** samples the composited children, and **Underlying Layer** samples the backdrop outside the folder. Active ranges isolate the children even when the folder is set to Pass Through. Clearing the ranges restores ordinary Pass Through behavior. The opening folder record carries the ranges and the selected blend mode even when the Photoshop file has no color channels for the folder itself. Non-neutral ranges on closing dividers remain protected. [Patchy's folder record notes](https://github.com/SethRobinson/Patchy/blob/main/docs/file-formats.md) and [folder rendering implementation](https://github.com/SethRobinson/Patchy/blob/main/src/render/layer_compositor.hpp#L3792).

Unclipped adjustments use the adjusted output for **This Layer** and the pre-adjustment backdrop for **Underlying Layer**. Supported Photoshop adjustment records retain their ranges even when they carry no raster color channels. Other PeerBrush adjustments keep their existing save compatibility rules. [Patchy's adjustment rendering calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/layer-effects-render.md#dialog-blend-if-and-satin).

Clipped paint, Solid Color and adjustment layers can use Blend If over grouped paint or Solid Color bases without effects. The underlying range sees the stack's current color and visible coverage. A supported base can also use Blend If. Its source range sees the base's own color and its underlying range sees the backdrop below the stack. The base can hide its own pixels while a paint member still draws inside the original clipping shape. Clipped adjustments preserve the stack's visible alpha. Ranged bases with effects, disabled grouping or a special blend at partial Fill remain protected. Eligible GPU previews track the original shape and the visible coverage separately. [Patchy's clipping calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/ps-compat.md#clipping-masks).

PSD and PSB save these ranges in the standard layer record along with the original raster and a current merged composite. The compositor preserves native 8- and 16-bit channels. Imports with active ranges require an exact saved-composite match before editing. Active mask and transparency ranges, malformed values, and combinations outside the supported layer subset remain protected. Eligible previews can use GPU compositing, with shared CPU repair near rounding boundaries. Native GPU parity and Photoshop receiving-application verification are pending.

## RGB blend channels in 0.4.0 (unreleased)

**Blend channels** in the layer context menu shows a swatch for each RGB channel. A slash marks an excluded channel. Hover or focus a swatch to preview toggling that channel in the canvas, then activate it to apply. Excluding a channel preserves its premultiplied backdrop value, including over transparent pixels. Excluding all three removes the layer's contribution to the composite. MCP and CLI accept three booleans in RGB order through `layer.update` and expose them as `blend_channels`.

Adjustments and unstyled Solid Color layers and folders support these controls. Paint layers can also carry Color, Gradient and Pattern Overlay stacks. Pass Through folders apply exclusions to each child write. Nested Pass Through exclusions combine, and later children see the result. Isolated folders apply exclusions at their merge.

In a clipping stack with a grouped Solid Color or paint base, a restricted member preserves the current stack's channels. A restricted base preserves the outside backdrop's channels at the stack merge. Turning off all three channels on the base removes the stack's composite contribution. Grouped paint bases and clipped paint members can use Color, Gradient and Pattern Overlays because these effects preserve the clipping alpha. Photoshop import requires Normal blending on styled bases and members and an exact saved-composite match at the original depth. Unsupported style or clipping combinations and malformed or non-RGB blocks remain protected. [Adobe's layer blending guide](https://helpx.adobe.com/photoshop/using/layer-opacity-blending.html) · [Patchy's channel-restriction calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/ps-compat.md)

PSD and PSB retain the standard `brst` block and current merged composite. The standard raster includes rendered overlays. PeerBrush retains the original raster and style stack in editable sources, using resident source format 77 for the newly supported clipping combinations.

## Brightness/Contrast in 0.4.0 (unreleased)

Brightness/Contrast and Legacy Brightness/Contrast are editable adjustments with integer controls. For RGB8 imports, `CgEd` supplies the settings and selects the mode. A standalone `brit` selects Legacy. Either form retains the stored mean and requires an exact match to the saved composite before editing. Automatic and Lab-only modes, conflicting records, Photoshop RGB16 records, and files with rounding differences remain protected. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Descriptor layout](https://github.com/Agamnentzar/ag-psd/blob/master/src/additionalInfo.ts) · [Photoshop adjustment calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustments-calibration.md)

Both PSD/PSB writers save supported RGB8 adjustments as standard records with original raster layers and a current merged composite. Modern mode writes `CgEd` with a zeroed `brit` companion. PeerBrush-authored RGB16 adjustments process native channel words and save a standard baked raster plus editable PeerBrush sources, using resident source format 78 for Legacy or 79 for modern mode. Photoshop receiving-editor verification is pending.

## Exposure in 0.4.0 (unreleased)

Exposure layers provide stops, Offset and Gamma controls. RGB8 Photoshop `expA` version 1 records retain their three binary32 settings, original raster layers and masks. Import requires an exact saved-composite match. Malformed records, Photoshop RGB16 Exposure and files with rounding differences remain protected. Both PSD/PSB writers save the supported RGB8 layer with a current merged composite. PeerBrush-authored RGB16 Exposure operates on native channel words and saves a standard baked raster plus editable PeerBrush sources, using resident source format 80. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop adjustment calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustments-calibration.md)

## Tone preset metadata in 0.4.0 (unreleased)

Supported Levels, Curves and Exposure adjustment layers retain the preset kind and Unicode filename from their `CgEd` companion in both PSD/PSB writers. This applies to Levels and Curves at RGB8 and RGB16 and Exposure at RGB8. Rendering uses the parameters embedded in the adjustment. The filename is stored as metadata and never opened as a path. Import still requires an exact saved-composite match, and malformed, duplicate or mismatched companion records keep the document protected. Editable resident saves with the new companion support use source format 89. [Preset descriptor layout](https://github.com/Agamnentzar/ag-psd/blob/master/src/additionalInfo.ts)

## Channel Mixer in 0.4.0 (unreleased)

Choose the Red, Green, Blue or Monochrome output swatch. Each output has signed Red, Green, Blue and Constant percentages. Preview and rendering use the same fixed-point weights. RGB8 Photoshop `mixr` records with the calibrated layout can open as editable layers after an exact saved-composite match. The standard PSD/PSB save retains active coefficients, original rasters, masks, the recognized `CgEd` preset name and kind when present, and a current merged composite. Unsupported record layouts, non-RGB coefficients and Photoshop RGB16 Channel Mixer remain protected. [Adobe Channel Mixer controls](https://helpx.adobe.com/photoshop/desktop/adjust-color/selective-color-adjustments/mix-color-channels-with-channel-mixer.html) · [Photoshop calibration and record layout](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#channel-mixer)

PeerBrush sources preserve both the active and inactive mode settings. Authored RGB16 adjustments process native channel words, with a standard baked raster and an editable PeerBrush source in resident format 81. Photoshop receiving-editor verification is pending.

## Black & White in 0.4.0 (unreleased)

Black & White has six independent color weights and an optional RGB tint. RGB8 Photoshop `blwh` records with the supported descriptor layout open for editing when the render matches the saved composite exactly. Both PSD/PSB writers retain the six weights, tint switch, fractional RGB tint values, preset metadata, original layers and masks, and a current merged composite. Unknown descriptor layouts, non-RGB tints, RGB16 Photoshop imports and composite mismatches remain protected. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop Black & White calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#black--white)

PeerBrush-authored RGB16 Black & White evaluates the conversion and tint at native precision. Saves pair a standard baked raster with the editable PeerBrush source in resident format 82. Photoshop receiving-editor verification is pending.

## Selective Color in 0.4.0 (unreleased)

Selective Color exposes Relative and Absolute modes with Cyan, Magenta, Yellow and Black corrections for each of nine RGB color and tone ranges. Hover over either mode for its scaling rule. Supported RGB8 `selc` records become editable when the rendered result exactly matches the saved composite. Both PSD/PSB writers retain the method, all nine corrections, original layers and masks, and a current merged composite. Unknown methods, nonzero reserved records and mismatched composites remain protected. [Adobe Selective Color controls](https://helpx.adobe.com/photoshop/desktop/adjust-color/selective-color-adjustments/make-selective-color-adjustments.html) · [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop adjustment calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#selective-color)

PeerBrush-authored RGB16 Selective Color applies all corrections at native precision. Saves pair a standard baked raster with editable PeerBrush source format 83. Photoshop RGB16 records remain protected pending calibration.

## Legacy Vibrance in 0.4.0 (unreleased)

Vibrance (Legacy) provides live Vibrance and Saturation controls. Supported RGB8 `vibA` records open for editing when the rendered result matches the saved composite exactly. Both PSD/PSB writers retain the legacy settings, original layers and masks, and a current merged composite. Save Review flags live records with nonzero Vibrance because Photoshop can recompose them differently. Newer Color and Vibrance records, including Temperature and Tint, and Photoshop RGB16 Vibrance remain protected. [Adobe Vibrance controls](https://helpx.adobe.com/photoshop/desktop/adjust-color/color-corrections/adjust-color-saturation-using-vibrance-adjustment.html) · [Adobe Color and Vibrance controls](https://helpx.adobe.com/photoshop/desktop/create-manage-layers/color-adjustment-fill-layers/correct-color-balance-with-color-and-vibrance.html) · [Patchy calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#vibrance)

PeerBrush-authored RGB16 Vibrance applies the transfer to native channel words. Saves pair a standard baked raster with an editable PeerBrush source in resident format 84.

## Photo Filter in 0.4.0 (unreleased)

Photo Filter provides a color picker, Density and Preserve Luminosity. Supported RGB8 `phfl` imports retain version 2 Lab or RGB components and version 3 XYZ components when the rendered result matches the saved composite exactly. Changing Density or Preserve Luminosity keeps the original color values. The picker supports exact 12-digit RGB HEX when editing an existing RGB color record. Other RGB8 color edits use version 2 Lab. Both PSD/PSB writers retain the supported settings, original layers and masks, and a current merged composite. Save Review flags possible clipping differences with Preserve Luminosity. [Adobe Photo Filter controls](https://helpx.adobe.com/nz/photoshop/using/applying-color-balance-adjustment.html) · [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop adjustment calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#photo-filter)

PeerBrush-authored RGB16 Photo Filter accepts exact 12-digit RGB HEX, runs at native precision, and saves a standard baked raster alongside an editable PeerBrush source in resident format 85. Photoshop RGB16 Photo Filter records and unsupported color spaces remain protected.

## Gradient Map in 0.4.0 (unreleased)

Gradient Map provides Classic, Linear and Perceptual interpolation, Reverse, Smoothness, and editable RGB stops. The position grid uses 4096 intervals and each midpoint controls the segment leading into its stop. Supported RGB8 Photoshop `grdm` versions 1 and 3 become editable after an exact saved-composite match. Both PSD/PSB writers preserve the gradient name, RGB stops, opacity stop data, original layers and masks, and a current merged composite. The adjustment preserves source alpha. Noise gradients, dither, unsupported stop layouts and Photoshop RGB16 Gradient Map records remain protected. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop Gradient Map calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustment-kinds.md#gradient-map) · [OKLab](https://bottosson.github.io/posts/oklab/)

PeerBrush-authored RGB16 Gradient Map uses continuous luma and native channel words. Saves pair a standard baked raster with editable PeerBrush source format 86. Photoshop receiving-editor verification is pending.

## Photoshop Color Balance in 0.4.0 (unreleased)

Color Balance (Photoshop) provides three signed axes in each of Shadows, Midtones and Highlights, with Preserve Luminosity. The shared renderer uses calibrated channel black points, white points and gamma, including the near-black curve. Existing PeerBrush Color Balance layers retain their original rendering. Supported RGB8 Photoshop `blnc` records become editable after an exact saved-composite match. Both PSD/PSB writers retain the adjustment settings, original layers and masks, and a current merged composite. [Adobe Color Balance controls](https://helpx.adobe.com/photoshop/desktop/adjust-color/color-corrections/apply-color-balance-and-photo-filter-adjustments.html) · [Photoshop Color Balance calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustments-calibration.md#color-balance)

PeerBrush-authored RGB16 Color Balance (Photoshop) operates on native words. Saves pair a standard baked raster with editable PeerBrush source format 87. Malformed records, composite mismatches and Photoshop RGB16 Color Balance remain protected.

## Photoshop Hue/Saturation in 0.4.0 (unreleased)

Hue/Saturation (Photoshop) has separate Master and Colorize controls and six color ranges. The Hue slider shows its degree value on a color ramp. Each range has Hue, Saturation and Lightness controls, plus a start angle and widths for the core and both fades. A graph shows which hues receive the full adjustment and which fall within the fades, including ranges that wrap around red. The canvas previews the adjustment through the shared compositor. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Photoshop Hue/Saturation calibration](https://github.com/SethRobinson/Patchy/blob/main/docs/adjustments-calibration.md#huesaturation-calibration)

Supported RGB8 `hue2` records become editable when the reconstructed result exactly matches the saved composite. PSD/PSB saves preserve both mode settings, the six range records, the optional 36-byte trailer, and supported preset metadata. Save Review flags possible Photoshop re-rendering differences, especially at feathered range edges. Older `hue ` records, unsupported definitions, RGB16 Photoshop Hue/Saturation and composite mismatches remain protected.

PeerBrush-authored RGB16 Hue/Saturation works directly on native words. PSD/PSB saves include a current merged composite and standard baked raster plus editable PeerBrush source format 88. Receiving-editor verification is pending.

## Color Lookup in 0.4.0 (unreleased)

Choose a 3D `.cube`, integer `.3dl` or baked IRIDAS/SpeedGrade `.look` table up to 2 MiB, or an RGB-to-RGB ICC v2/v4 DeviceLink up to 512 KiB. The shared renderer handles per-channel CUBE domains, Flame and Lustre 3DL input shapers, baked LOOK float grids, classic ICC LUT8/LUT16 tables, and v4 A-to-B curve, CLUT and matrix stages. It uses tetrahedral interpolation with up to 33 grid points per axis and preserves RGB8/RGB16 channel precision and alpha. LOOK accepts UTF-8 and BOM-marked UTF-16. Its shader and metadata XML stay embedded in the original bytes. Active LOOK masks, unknown outer sections and malformed XML remain protected. [ICC profile specification](https://www.color.org/specifications/ICC.1-2022-05.pdf) · [OpenColorIO CUBE format](https://github.com/AcademySoftwareFoundation/OpenColorIO/blob/main/src/OpenColorIO/fileformats/FileFormatIridasCube.cpp) · [OpenColorIO 3DL format](https://github.com/AcademySoftwareFoundation/OpenColorIO/blob/main/src/OpenColorIO/fileformats/FileFormat3DL.cpp) · [OpenColorIO LOOK format](https://github.com/AcademySoftwareFoundation/OpenColorIO/blob/main/src/OpenColorIO/fileformats/FileFormatIridasLook.cpp)

Supported undithered Photoshop `clrL` CUBE, 3DL, LOOK and DeviceLink records become editable when the result exactly matches the saved composite. For standalone tables, use the RGB/BGR swatches under **Output channel order** (Photoshop Data Order) and **Table axis order** (Photoshop Table Order). Supported external ICC profiles take precedence when the file embeds both sources. PSD/PSB saves preserve original tables, external profiles, source names, orders, layers and masks, and include a current merged composite. Resident LOOK saves use source format 94. Earlier PeerBrush CUBE sources retain their previous appearance and save metadata. Dither, external references, unsupported LUT/profile types and composite mismatches remain protected. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Adobe Color Lookup formats](https://helpx.adobe.com/photoshop/desktop/save-and-export/export-files-to-different-formats/export-color-lookup-tables.html) · [Photoshop order measurements](https://github.com/storytold/photocraft/issues/9126)

New standalone tables can save an ICC v4.3 DeviceLink with a float `D2B0` CLUT and its input domain or shaper, plus the original LUT bytes. On reopen, a byte-exact regeneration check identifies a derived profile and keeps the table as the editable source. Supported external float CLUT, matrix and sampled shaper stages can also be read. An unrepresentable generated profile uses the standard baked fallback with editable PeerBrush sources. Save Review flags older standalone tables without a compiled profile and earlier CUBE table-order metadata that Photoshop may interpret differently. Receiving-editor verification for PeerBrush’s generated profiles is pending. [ICC profile specification](https://www.color.org/specifications/ICC.1-2022-05.pdf) · [Native reopen evidence](https://github.com/storytold/photocraft/issues/8839)

## Supported clipping

PSD v1 RGB documents at 8 or 16 bits can contain consecutive clipped raster layers directly above a raster base. Stacks can live at the document root or inside an isolated or pass-through folder. The importer resolves each follower to the nearest non-clipped sibling below it, including hidden bases, and never uses a base outside its folder. Raster alpha, opacity, supported blend modes and raster masks use the shared document compositor. The base's blend applies to the complete clipping unit; hiding the base hides its followers.

Saving these stacks writes separate standard raster layers, clipping bytes, mask channels and an explicit enabled `clbl` setting on each base. Native 16-bit source and mask words stay native, including hidden RGB. Raster-layer color effects are baked into each affected standard raster; supplementary PeerBrush sources retain editable originals/effects. A folder with active color effects retains the existing behavior of baking that folder and its children into one standard raster. Every save includes a current merged composite. Reopening without private resources retains clipping relationships wherever the standard layers remain separate.

Documents containing adjustments beyond the supported Invert form above, or clipping involving folders, still use a standard baked composite layer plus supplementary editable PeerBrush sources. Imported Photoshop clipping involving folders is protected until its Photoshop behavior is supported.

## Pass-through folders

Choose **Pass Through** in a folder's blend menu, or send `layer.update` with `blend:"pass_through"`. Existing/new folders retain their current Normal default. Children blend individually against the backdrop below the folder, including through nested pass-through folders; an isolated folder stops that traversal. Folder opacity and raster masks attenuate the change to the combined backdrop. Human gestures preview the actual shared-engine pixels and commit once. Reservations, undo/redo and native-depth layer clipboard use the same sources.

Adjustment layers inside pass-through folders can change layers below and outside the folder. Effect preparation follows composition order across folder boundaries, including regional stroke previews. Color effects on ordinary children remain editable and native-depth. Enabled effects directly on a pass-through folder, clipping to/from that folder and merging that folder are rejected with an explicit instruction to choose an isolated blend or release clipping. Disabled folder effects can be retained. A containing isolated folder can still be merged normally.

Standard PSD folder keys and section-divider overrides (`lsct`/`lsdk`) recognize `pass`. Raster-only trees save ordinary pass-through dividers, raster layers and masks, plus the current composite. Reopening without PeerBrush resources preserves those layers and their outside blending. Adjustment documents beyond the supported Invert form above continue to save a standard baked composite; supplementary source format 10 restores their editable pass-through topology in compatible readers and protects it in earlier readers. The unreleased Invert layout uses format 35.

## Protected grayscale previews

8-bit and 16-bit grayscale PSD and PSB files open read-only from their saved composite. PeerBrush restores declared merged transparency before display conversion. Untagged gray samples map to equal RGB channels. Supported v2/v4 monochrome ICC tone curves with an XYZ connection space produce an sRGB preview at the original channel depth. The source grayscale samples, profile and extra selection data stay in the original file. An explicit editable copy uses the flattened preview at the same bit depth. Grayscale layer editing, profile LUTs and other unsupported ICC layouts remain unavailable. [Adobe Photoshop file format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [ICC monochrome profile specification](https://www.color.org/ICC1-V41.pdf) · [Grayscale white-matte calibration](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/pil_io.py)

## RGB ICC previews and copies

Profiled PSDs retain the saved composite and exact original ICC resource at 8 or 16 bits. Supported ICC v2/v4 RGB matrix/TRC profiles with XYZ connection space, classic `lut16` RGB-to-XYZ/Lab tables and `lut8` RGB-to-Lab tables display through an sRGB conversion, including loading feedback, isolated previews, thumbnails and agent observations. Common sRGB, Adobe RGB, Display P3 and ProPhoto RGB matrix profiles are covered. Conversion uses the relative colorimetric `A2B1` table when available, otherwise the perceptual `A2B0` table, without black-point compensation. Matrix profiles use relative colorimetric intent. Out-of-gamut channels clip to the sRGB range. LUT conversion interpolates at native input/output depth; it does not promise a lossless color-space round trip. The screen is treated as sRGB; monitor calibration and HDR display mapping are pending.

Profiled Photoshop documents remain **READ ONLY**. **Convert to sRGB copy** explicitly flattens their saved appearance into an editable project at the original bit depth. Native16 conversion operates on all 65,536 channel values before display projection; alpha and the source file stay intact. Save requires a new filename. Converted PSD and PNG copies carry a standard sRGB ICC tag; standard PSD raster channels and the merged composite contain the converted result. PeerBrush recognizes its exact built-in working profile on reopen without converting twice. Other imported profiles continue through the protected path.

Agents call `document` with `action:"compatible_copy", convert_to_srgb:true` and the observed revision. Omitting explicit conversion is refused for a profiled source. `document.color_profile` reports source/preview spaces and conversion availability without returning raw profile bytes. The ordinary protected-document copy remains available for unprofiled sources. Reservations and source revision/identity checks are rechecked before replacing the workspace.

Exporting a protected original to PNG retains its original native samples and ICC bytes. Invalid, `mAB`/`mBA`, multi-process, non-RGB, device-link, abstract, named-color and HDR/CICP-transfer profiles remain protected with explicit unmanaged-preview reasons; their editable conversion is unavailable. `lut8` XYZ is protected because ICC does not define its PCS encoding. Duplicate PSD profiles and profiles above 4 MiB are rejected. Parsing checks tag bounds, duplicate/required tags, singular matrix colorants and table dimensions before allocating LUTs. Classic LUTs require three input/output channels, 2–33 grid points per axis and 2–4096 input/output curve entries. Lab normalization follows the table's encoding, including legacy `lut16` Lab in ICC v4. Four disposable transform caches stay outside history and recovery. Encodings follow the [ICC specification](https://www.color.org/specification/ICC.1-2022-05.pdf).

Raster image import normalizes these supported RGB profiles to sRGB at the original channel depth, preserving alpha. Profile assignment, editable profiled layer stacks, newer LUT structures, soft proofing, CMYK, monitor profiles and HDR remain future work.

## Standard layer locks

Photoshop's full protection records (`lspf=7` for transparency, composite and position, and `lspf=0x80000000` for Lock All) import as PeerBrush's layer lock at either depth, including folder locks that protect descendants. Saves preserve the Lock All value while it is active. Unlocking uses the shared engine and clears the preserved record. Undo restores it. Editable sources must agree with the standard lock when the file carries one. Partial and unknown protection combinations still open read-only. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Observed Lock All value](https://github.com/storytold/photocraft/issues/395)

## Layer color labels (unreleased)

PSD and PSB retain Photoshop's standard `lclr` labels, including Seafoam, Indigo, Magenta and Fuchsia. Use **Color label** in the layer context menu to choose a named color or clear the label. The menu colors match the markers in the layer rows. The command observes ordinary layer locks and reservations. The label changes organization only and leaves the artwork and composite unchanged. Unknown label values and malformed reserved words still protect the source. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Adobe Layers panel](https://helpx.adobe.com/photoshop/desktop/create-manage-layers/get-started-layers/organize-layers-in-the-layers-panel.html) · [Observed label values](https://github.com/Agamnentzar/ag-psd/blob/master/src/helpers.ts)

## Save compatibility report (unreleased)

Choose **File → PSD compatibility…** before saving to inspect the codec's standard layer plan at the current document revision. The report identifies separate editable raster layers and folders, folders baked into one raster, composite-only output, and original roots hidden beneath a filtered composite. It lists standard bounds, mask channels, effects and mask steps baked to pixels, text/vector definitions and retained transform originals that require supplementary PeerBrush sources, and opacity values quantized to the standard 256 levels. Native raster/mask channels and the current merged composite retain the document's 8/16-bit depth. Saving does not convert profiles: editable output is either tagged with the built-in sRGB profile or uses the untagged sRGB working space; protected profile sources cannot be saved.

The report is view-only, shared with MCP and CLI through `document` action `save_report`. Supply the observed `document_id` and `expected_revision`, plus `project_id` for an explicit project. A stale request is refused; the native report closes when its source changes. `has_editability_losses` describes limitations of standard PSD structures; it does not claim that supplementary PeerBrush definitions are editable in another application. Inspecting the report neither writes a file nor changes pixels/history.

Native Save, Save As and the save step of a close/exit review require **Save with these changes** whenever the report lists losses, including standard opacity quantization. Cancel keeps the project open and leaves the destination untouched. Acceptance applies to the reviewed document revision; a later edit or project switch cancels that review. Plain raster/folder documents with no reported losses save directly. Protected sources still require an explicit compatible copy.

MCP/CLI callers must review `save_report`, then send `action:"save"` with `accept_editability_loss:true` and that exact `document_id` and `expected_revision`. Existing Save As destinations independently require `overwrite:true`; neither flag overrides a loaded-file conflict, protected source or source change while encoding. A previous save does not grant acceptance for later revisions. Private recovery snapshots continue retaining editable sources without an interactive prompt and do not replace the working PSD/PSB. Practical receiving-application round trips remain pending; these checks do not establish full Photoshop compatibility.

## Protected settings

Unsupported documents retain their saved composite at the original channel depth and cannot be edited or saved over their source. The native **READ ONLY** label exposes warnings on hover; agents receive the same reasons in `document.warnings`. **Edit 16-bit copy** / `compatible_copy` explicitly flattens unsupported structure into a new editable project at the original precision and requires a new save path.

The **Why read only?** report also lists original source layer records, including folder membership, original document coordinates, visibility, mask presence, tag names and unsupported settings found on each record. MCP and CLI observations expose the same view-only records in `document.source_structure`. `source_index` is the original file record index; `parent_index` points to its source folder. Closing folder dividers are omitted. `kind` distinguishes folder (`group`) from ordinary layer records (`layer`); unsupported source object types are not inferred. These records have no editable layer IDs and do not represent recovered layer pixels. Document-level warnings still apply to records without individual warnings.

Source diagnostics stay outside saved PeerBrush sources and history. Each report has at most 200 records, 64 tag names and 64 reasons per record; diagnostic text is limited to 1,024 Unicode characters, with `details_truncated` explicitly indicating shortened details. The protected composite and source file retain their precision. Creating an explicit editable flattened copy clears these diagnostics. This reporting change is unreleased and does not add Photoshop style or pattern import support.

The codec now checks:

- Duplicate or invalid **Version Info** declarations. PSD and PSB readers require an unambiguous version and merged-image flag when this resource is present, including before preview or restoration from supplementary sources.
- Document-wide hidden effects, Layer Comps and Timeline data. These remain available through the read-only saved composite at the original precision. A single valid effects-visible flag leaves the supported editable path available. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)
- Quick Mask, animation Sheet Disclosure and Onion Skins resources also keep PSD and PSB files read-only. The saved composite remains viewable at the original channel depth. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)
- Non-square or ambiguous Pixel Aspect Ratio records. PeerBrush shows the saved raster samples without stretching them. Editable square-pixel documents retain the exact version 1 or 2 aspect record in standard PSD and PSB saves. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)
- Conflicting, duplicate or invalid ICC Untagged Profile declarations. An editable RGB document explicitly marked as intentionally untagged keeps that flag in standard PSD and PSB saves without adding an ICC profile. [Adobe PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)
- Standard ICC resources that disagree with supplementary working-profile metadata. Adding or removing the built-in sRGB profile protects a file when its supplementary sources still declare the previous color state. Other embedded profiles follow the protected color-preview path.
- Disabled **Blend Clipped Layers As Group** outside the supported Normal base subset, knockout (`knko`) and partial Fill outside the supported paint and Solid Color layer subset.
- Active **Blend If** source or destination ranges outside the supported Gray/RGB layer subset, and invalid range blocks.
- Partial/unknown Photoshop protection flags (`lspf` and standalone transparency protection), invalid clipping/flag payloads, duplicate tags and malformed folder boundaries.
- Section-divider blend overrides (`lsct`/`lsdk`), including supported pass-through folders when the ordinary layer-record blend says Normal. Unknown divider modes and animation scene groups remain protected; `pass` on a raster layer is invalid.
- Advanced mask payloads and flags, and layers whose raster pixels are explicitly irrelevant to their appearance without a supported adjustment definition.

Valid no-op raster flags remain readable. Interior-effect/vector flags only pass for ordinary raster sources; the corresponding unsupported effect/vector tags still protect the document. Embedded ICC profiles stay protected until explicitly converted, even when otherwise valid private PeerBrush sources are present and standard layer/composite hashes match.

The format checks follow Adobe's [PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/); grouped clipping follows Adobe's [clipping-mask documentation](https://helpx.adobe.com/photoshop/using/revealing-layers-clipping-masks.html), and outside-group blending follows Adobe's [group blend documentation](https://helpx.adobe.com/uk/photoshop/using/layer-opacity-blending.html). The implementation covers the subset above, not full Photoshop compatibility.

In the read-only source-layer report, vector mask diagnostics identify malformed records and describe unsupported masks by their path and knot counts, initial fill and flags. The report reads bounded metadata while the canvas shows the saved composite. [Adobe Photoshop file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

## Verification

Independent handwritten 8/16-bit PSD fixtures cover clipped blend stacks, masks, hidden/transparent bases, folder boundaries, pass-through divider overrides, standard save/reopen without private resources, effect baking, engine reservations/history and unsupported feature protection. Nested-folder tests cover translucent backdrop interpolation, adjustment dependencies across pass-through/isolated boundaries, scattered hierarchy storage, regional previews, clipboard, atomic rejection and undo/redo. All 354 engine, codec, HTTP/stdio protocol and UI regression tests pass; seven opt-in device/clipboard/benchmark checks were not run.

The native Windows workspace displayed a five-layer native16 clipping scene and rendered an opacity gesture on its clipped stripes. One keyboard undo restored PNG16 byte-for-byte. An independent decoder confirmed all standard raster/mask channel planes, layer bounds and clipping flags exactly against the original fixture, and every merged PSD16 composite word against the exported PNG16. Reopening after removing private resources restored the same tree and exact PNG16 output. A knockout fixture opened read-only at 16 bits with the specific `knko` explanation. Original fixture files were retained unchanged; generated QA files are ignored development data.

The pass-through slice also loaded a native16 standard fixture through the running app's shared protocol. Independent decoding confirmed every standard raster and group/child mask word, bounds and clipping flags, supplementary format 10, and a current merged composite matching every PNG16 word. Removing private resources retained the five-layer tree and byte-identical PNG16 output. A shared-engine Normal blend edit changed the rendered image; one protocol undo restored the baseline PNG16 exactly. Both rendered exports were visually inspected. Native control/gesture verification remains pending for this slice: the Windows capture tool returned unrelated foreground images for the PeerBrush window and activation failed, including after a fresh launch and capture-session reset. No interaction with the unrelated apps was attempted.


The RGB ICC checkpoint also passed six dedicated tests with independently assembled standard ICC/PSD records and an analytic sRGB transfer reference. Native app-rendered frames verified the protected managed appearance, 16-bit editable copy and pass-through folder controls. Independent decoding matched every protected-original PNG16 sample and ICC byte, every converted standard raster/composite word, the sRGB tag across PSD/PNG and exact metadata-free reopening. A shared pass-through blend edit and one undo restored PNG16 exactly. Desktop click/drag verification remains unverified after the Computer Use approval timeout; app-rendered screenshots verify the native result without making that interaction claim.

The original [layer-style authoring work](layer-styles.md) introduced native 8/16-bit rendering, editable sources in format 16 and a baked standard appearance. Later import and save support for legacy effects, object descriptors and embedded patterns is documented above. [Adobe layer-effect records](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)


## Photoshop object-style diagnostics (unreleased)

Protected `lfx2` and `lmfx` layers expose bounded descriptor diagnostics through the native source report, observation and compatibility report. The codec lists recognized shadow, glow, overlay, Stroke, Satin and Bevel definitions, including multiple instances, their local enabled/disabled states, present flags and the separate master switch. Unknown properties and malformed or unsupported value types remain explicit. Names and opaque descriptor content are validated but are not dumped into these diagnostics.

Inspection is limited to 1 MiB per effects block, 32 nesting levels, 4096 descriptor/value nodes, 256 properties or list items, 128-byte printable identifiers and 4096 UTF-16 code units per string. Duplicate keys, malformed booleans/Unicode, non-finite numbers, unsupported versions and nonzero or oversized trailers reject inspection without making the source editable. Diagnostics do not create native effect stacks, decoded editable layers or a substitute composite. Styled sources still use their preserved merged image at original channel depth; broader style/pattern rendering and editable import remain pending. Binary layout follows the [Adobe file-format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/); reading a descriptor does not establish Photoshop rendering equivalence.

## Standalone solid Stroke import (0.26 development, unreleased)

Native Open checks Stroke source alpha in the bounded raster row decoder. An otherwise supported document can open for layered editing beyond the resident canvas limit when the original alpha is binary and the complete native composite matches. Runtime and Photoshop round-trip acceptance remain pending.

One unscaled Normal solid-color Stroke can retain its native Inside, Outside or Center definition when its complete standard composite matches exactly. It can run above supported Pattern, Gradient and Color Overlays and a supported hard Inner Shadow. Inside Stroke also works with a supported hard Drop Shadow because it preserves the casting alpha. Outside and Center Stroke can retain a supported hard Drop Shadow when one of the effects is disabled, either opacity is zero, or the Stroke size is zero. Whole-pixel sizes are bounded to 0–128 pixels and source channels retain their original precision. Lists with outside or center Strokes, simultaneously contributing Outside or Center Stroke and Drop Shadow, partial source alpha, masks, clipping, gradient or pattern stroke paint, and overprint remain protected. PSD/PSB saves retain the editable PeerBrush effects, standard raster layers and current composite. The mixed order follows Krita's projection sequence and still requires Photoshop round-trip verification. [Krita layer style projection](https://raw.githubusercontent.com/KDE/krita/master/libs/image/layerstyles/kis_layer_style_projection_plane.cpp)

## Plain Color Overlay import (unreleased)

A bounded first editable style subset accepts a single RGB Color Overlay in `lfx2` or `lmfx`: Normal/Multiply/Screen/Overlay/Darken/Lighten effect blend, percentage opacity, unscaled styles and explicit enabled/present/master flags. The source must be an ordinary raster layer with Normal layer blend. Supported raster masks and reduced layer opacity are accepted by the extension below. Unknown properties, non-RGB colors, unsupported multi-effect lists, competing object/legacy style blocks, unsupported units and broader Photoshop structures keep the document protected. Disabled overlays retain their native editable definition.

Before enabling editing, the loader renders all layers through the shared engine and requires every saved native composite sample, including alpha, to match exactly. A mismatch or render-budget failure preserves the protected original composite. Imported original raster samples remain unchanged; future PSD/PSB working saves bake the current appearance into standard channels and retain editable native sources through the existing supplementary format and explicit save-loss report. These saves do not export editable Photoshop style descriptors. Native8/16 codec and protected-fallback regression coverage passed in the combined sprint QA; the representative styled source remains unsupported.

Color Overlay blend identifiers are validated against the [primary Photoshop terminology mapping](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/terminology.py). The loader keeps exact composite gating for every supported mode; recognizing an enum does not waive native rendering fidelity. Analytic native8/16 blend coverage passed in the combined sprint QA.

## Plain Gradient Overlay import (unreleased)

A single `GrFl`/`gradientFillMulti` descriptor can become a native Gradient Overlay when it uses a supported blend, is on a supported cardinal axis, unscaled, aligned to the document, undithered and unoffset. The custom RGB gradient must have zero smoothing, explicit 50% stop midpoints, 2–16 strictly ordered color and opacity stops with 0/4096 endpoints, and supported RGB/user-color definitions. Enabled/present/master/reverse flags remain explicit. Noise gradients, newer interpolation methods, alternate color encodings, smoothness, layer alignment, unsupported multi-effect definitions and unknown fields stay protected. The same raster-only layer restrictions and exact native saved-composite gate used for Color Overlay apply. Missing fields produce bounded protection reasons.

Native source pixels are retained; editable stops use source format 20 on later PSD/PSB saves, with current standard rasters/composites and explicit standard-output editability loss. These saves do not export Photoshop gradient style descriptors. Native8/16 synthetic import, reverse, standard saves and malformed/missing-field protection coverage passed in the combined sprint QA. This does not recover the broader representative styled document.


## Plain bitmap Pattern Overlay import (unreleased)

A `patternFill` descriptor can become a native bitmap Pattern Overlay when it uses a supported blend and placement. The embedded RGB tile must resolve to one unambiguous ID. Its storage origin is independent of placement. The reader retains 8- or 16-bit tile samples, including 8-bit tiles in 16-bit documents. Every decoded pattern asset must be retained by a supported style or fill. Unused assets, duplicate blocks and unsupported color handling or style combinations keep the source protected. The exact saved native-composite gate used for the other overlay imports applies.

Imported tile samples and original raster pixels retain their precision. The earlier source format 18 stored native bitmap effects alongside the baked standard appearance. Native8/16 source retention, reversible phase edits, working saves, malformed identities, resource ambiguity, unused assets and composite mismatches passed the earlier combined sprint QA. The broader representative styled document remains protected. [Pattern effect definitions](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/effects.py)

## Mixed overlay import (unreleased)

The supported Color, Pattern and Gradient subsets may coexist, with unambiguous definitions per family. All fields and disabled definitions remain subject to the individual import limits. Imports execute Pattern, Gradient, Color, with Color uppermost, following the [saved Photoshop calibration](layer-styles.md#calibrated-photoshop-overlay-family-order-unreleased); descriptor property order is ignored. Previously saved native stacks keep their explicit order. The loader requires an exact complete native composite match and never searches for an alternative order. Any unsupported sibling keeps the entire document protected. Earlier combined regression evidence and the later calibration correction are retained in the development record; broader styled-source acceptance remains pending.

## Pattern and Gradient overlay blends (unreleased)

Pattern and Gradient Overlay import now accepts the same Normal, Multiply, Screen, Overlay, Darken and Lighten effect blends as Color Overlay. Native rendering, exact composite gating and every previous placement/color/structure limit apply. Unknown or unsupported blend enumerations remain protected, including disabled definitions; importing a recognized mode never bypasses fidelity checks. Native8/16 analytic composite and PSD/PSB source-retention coverage passed combined QA.

## Multiple overlay definitions (unreleased)

Explicit `solidFillMulti` and `gradientFillMulti` lists now retain 1–16 fully supported definitions per family, in file order. Pattern runs first, then Gradient entries, then Color entries; the workspace shows final operations at the top. Disabled definitions remain editable native effects. Lists under a single-effect key cannot imply multiple effects; competing aliases, empty/oversized lists, unknown fields or unsupported siblings protect the whole document. The total native stack is limited to 32 effects, matching shared editing/save constraints; one bitmap Pattern may coexist within that limit. File order within a family follows the [primary effect-list reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/effects.py), with family order determined by the calibration above. Exact complete native composite matching is still mandatory. Added coverage includes noncommuting opacity stacks, every enabled/master combination, reorder undo, native8/16 PSD/PSB saves and boundary/alias protection; earlier combined QA and later calibrated-order checks are recorded separately.

## Raster masks and layer opacity (unreleased)

Supported overlay-only styles now import on ordinary raster layers with supported native raster masks and reduced layer opacity. Overlays preserve the source alpha; the shared engine applies the mask and layer opacity to their combined result. Every mask field/channel still passes the existing codec validation, and the complete saved native composite must match exactly. Unsupported masks, vector/global mask structures, unsupported clipping/folder styles, non-Normal layer blends, fill-opacity changes and unknown features remain protected. Native8/16 analytic masked composites, zero/partial/full layer opacity, all three overlay families, bypass undo and PSD/PSB source/mask retention coverage passed combined QA.

In 0.4.0 development, supported Color, Gradient and Pattern Overlays can also open on an unclipped raster layer with a supported non-Normal blend when the layer explicitly enables Blend Interior Effects As Group (`infx`). The shared compositor applies the layer blend after rendering its interior overlays. The full saved composite must match exactly at the original channel depth. For this styled subset, missing, disabled, duplicate or invalid grouping flags keep the document protected. Non-Normal clipping groups and other style families remain protected. Both writers retain supported explicit flag values in the standard layer record. PeerBrush saves retain the original raster and editable overlays, with their rendered appearance in the standard raster channels and current merged composite. [Adobe's blending guide](https://helpx.adobe.com/uk/photoshop/using/layer-opacity-blending.html) · [PSD flag format](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

## Cardinal and reflected gradients (unreleased)

The plain document-aligned gradient subset now includes Linear and Reflected shapes at −180°, −90°, 0°, 90° or 180°, with the existing reverse flag and six supported blends. Scale remains exactly 100%; offsets, layer alignment, dithering, smoothing, noncentral stop midpoints, arbitrary angles and other shapes remain protected. Native source settings retain the explicit angle and shape. The [primary gradient terminology](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/terminology.py) identifies `Lnr ` and `Rflc`; the complete native composite must still match exactly. Independent horizontal/vertical native8/16 orientation, reflection, reverse, editable PSD/PSB sources and unsupported-shape/rotation coverage passed combined QA.

## Default raster clipping with overlays (unreleased)

Supported overlay-only styles may import on ordinary Normal raster layers in the existing default raster clipping subset. Overlay rendering preserves the original source alpha before clipping to the validated raster base. Every clipping flag, grouping/hierarchy restriction, mask/source check and exact native composite gate remains active; missing or incompatible bases and unsupported clipping behavior protect the whole source. Added native8/16 coverage checks all three overlay families against independently specified clipped composites, original base/foreground samples, bypass undo, editable PSD/PSB saves and standard-only PSD reopening. Combined QA passed.

## Hard shadow import extension (unreleased)

The bounded editable style subset additionally accepts a single plain hard RGB Drop Shadow or Inner Shadow with zero size/choke/noise, explicit linear contour endpoints and cardinal integer displacement. Full-opacity, unmasked, unclipped Normal raster layers with binary alpha are required. Drop Shadow accepts only Normal effect blend with layer knockout; Inner Shadow accepts the six bounded effect blends. Every definition is validated even when disabled, and the complete native composite must match exactly before editing. Unsupported shadow settings, partial alpha, mixed/multiple styles and broader source structures preserve read-only protection and the saved native composite. Original sources, standard current output and editable saves use the shared engine. See [settings and precise limits](layer-styles.md#hard-photoshop-shadows-unreleased). Native8/16 analytic orientation, opacity, source/history/save and malformed/mismatched protection coverage passed combined QA. This does not recover general Photoshop shadow stacks or the representative styled source.

## Exact standard composite gate (unreleased)

Editable style import compares every standard white-matte RGB and alpha sample at the original channel depth. Restoring straight RGB from a saved translucent composite can quantize a channel; this does not introduce a tolerance or authorize a different encoded composite. Invalid white-matte samples and any differing standard sample preserve read-only protection. Cardinal gradient and shadow directions use exact axes to avoid floating-point residuals at channel rounding boundaries.

Combined QA passed all 792 engine/codec/protocol/UI checks across 85 test targets, with eight opt-in checks ignored and one Windows clipboard station check excluded. See [combined verification](development.md#combined-contour-bevel-and-photoshop-import-verification-unreleased). Broader Photoshop structures, styled sources and receiving-application compatibility remain open.

## Gradient placement extension (unreleased)

The supported cardinal Linear/Reflected overlay subset additionally retains 10–1000% scale and optional signed canvas-relative offsets within −100–100%. Version 0.26 development also retains bounded alpha-bound layer alignment and Radial/Angle/Diamond definitions using the existing integer grid. Strict percentage units, explicit two-coordinate offset objects, current native source validation and complete exact standard composite matching remain mandatory. Unsupported rotation/interpolation and unknown fields preserve protected fallback. See [placement settings and limits](layer-styles.md#photoshop-gradient-placement-extension-unreleased) and [0.26 acceptance status](development-0.26.md). The earlier document-placement native8/16 source, bypass/undo, save and protected-fallback coverage passed combined QA; newer import acceptance remains tracked separately.

## ZIP pattern assets (unreleased)

Bounded RGB Patt/Pat2/Pat3 asset inspection additionally accepts ZIP channels at native 8/16-bit precision. Each channel must contain one complete zlib stream with exactly the declared native sample count; incomplete streams, bad checksums, extra output, concatenated streams and trailing compressed data reject. A fixed scratch buffer bounds validation work and respects loading cancellation. Native row decoding, asset coordinates, source budgets and editable overlay composite guards remain shared. Analytic samples and malformed ZIP coverage passed combined QA. This extends asset decoding without authorizing unsupported Photoshop placement, profiles or style structures. Compression fields follow the [primary pattern reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/patterns.py).

## ZIP-predicted pattern assets (unreleased)

The bounded RGB pattern reader also accepts ZIP prediction, using the shared native channel decoder. Each row restores byte deltas at 8 bits or big-endian word deltas at 16 bits, with wrapping at the original channel precision and a fresh predictor per row. The same exact compressed-frame/output bounds and cancellation checks apply. Raw original asset coordinates and native samples remain available for inspection; supported Pattern Overlay imports still require complete exact standard composite matching. Combined coverage includes all four compression modes, predictor wrapping/row reset, view-only PSD/PSB protection, source-bound protocol PNGs and editable bounded overlay saves. Combined QA passed all 799 engine/codec/protocol/UI checks, with native asset inspection at both depths and compression modes; see [verification](development.md#combined-compressed-pattern-verification-unreleased). Layout and predictors follow the [primary pattern reader](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/psd/patterns.py) and [compression implementation](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/compression/__init__.py).

## Pattern transparency (unreleased)

Bounded RGB pattern assets additionally retain an optional trailing native transparency plane. RGB and alpha must share precision, dimensions and supported compression framing; reserved or extra color planes remain protected. Hidden RGB samples remain intact, including fully transparent pixels. Empty written channels reject rather than silently become opaque. Alpha uses the shared native channel decoder and existing allocation/cancellation bounds. Native PNG inspection and protected PSD/PSB protocol regression coverage passed combined QA. The slot layout follows the [primary pattern channel implementation](https://github.com/psd-tools/psd-tools/blob/main/src/psd_tools/api/numpy_io.py). Asset decoding alone does not authorize unsupported Photoshop editing.

The existing bounded Pattern Overlay subset can consume these transparent tiles through its retained native PNG sources. Coverage additionally checks transparent/partial/opaque samples, Normal and Multiply opacity, enabled and disabled definitions, exact mismatch protection, phase undo, original source preservation and editable PSD/PSB saves at both depths and all four compression modes. This coverage passed combined QA with all 802 engine/codec/protocol/UI checks successful; broader pattern placement and color-management limits remain active. See [native verification](development.md#combined-pattern-transparency-verification-unreleased).

## Retained layer reader versions (unreleased)

PSD and PSB round trips retain validated advisory metadata on closing folder records, including reader version, label color, reference point, layer name source, and static timestamp. The folder’s opening and closing records retain their own values. Unsupported rendering and protection settings on closing records keep the document read-only. [Adobe Photoshop file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

The readers interpret ordinary and nested section divider tags consistently when opening layers, restoring PeerBrush sources, and inspecting protected files. A valid `lsdk` divider opens with the same folder state as `lsct`. Saves use `lsct` for both. Unsupported divider types, malformed records and animation scene dividers keep the saved composite protected. [Adobe Photoshop file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/) · [Nested divider alias in ag-psd](https://github.com/Agamnentzar/ag-psd/blob/master/src/additionalInfo.ts)

When restoring supplementary PeerBrush data, the resident and indexed readers check all standard layer tags and channel IDs. Unsupported tags, active knockout, and duplicate layer tags keep the saved composite protected even when the supplementary checksum matches.

Supported editable layers retain Photoshop's aligned-rendering flag (`sn2P`) as a four-byte standard record with value 0 or 1. Both writers preserve its bytes, and supplementary sources must agree with the standard record. Malformed values, flags on closing folder records and unsupported vector features stay protected. [Adobe Photoshop file format specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/)

Validated `lyvr` compatibility markers from 70 through 160 in steps of ten are retained as standard PSD/PSB layer metadata. The marker alone does not change raster interpretation or override unsupported siblings, profiles, masks or styles. Unknown/future values, malformed lengths, invalid signatures and duplicate tags remain protected. Standard metadata and native pixels survive edits, duplication, undo and working saves; supplementary source format 24 makes the retained marker explicit to older readers. Native8/16 and PSD/PSB regression coverage passed combined QA. The marker semantics follow [Adobe's layer-version specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/). This does not claim support for every feature of those Photoshop versions or recover the representative styled source.

Supplementary metadata definitions are also checked against each actual standard layer record, including folder closing records and the hidden original layers written alongside document filters. Missing, substituted or moved metadata cannot authorize editable sources solely because the standard-pixel checksum still matches. A bounded record walk compares exact allowlisted payloads without decoding channels again; field ordering does not change their interpretation. Native8/16 PSD/PSB coverage for protected mismatches and valid filtered sources passed combined QA, with all 805 engine/codec/protocol/UI checks successful; see [native verification](development.md#combined-layer-metadata-verification-unreleased).

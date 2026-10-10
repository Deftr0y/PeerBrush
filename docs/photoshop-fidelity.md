# Photoshop fidelity

PeerBrush 0.2 implements the documented RGB subset: standard raster clipping, pass-through folders, full layer locks, supported RGB ICC previews and explicit native-depth sRGB copies. Unsupported Photoshop settings remain protected. The v0.1.4 checkpoint predates these changes; [v0.2 packages](https://github.com/Deftr0y/PeerBrush/releases/tag/v0.2) contain the documented support.

## Supported clipping

PSD v1 RGB documents at 8 or 16 bits can contain consecutive clipped raster layers directly above a raster base. Stacks can live at the document root or inside an isolated or pass-through folder. The importer resolves each follower to the nearest non-clipped sibling below it, including hidden bases, and never uses a base outside its folder. Raster alpha, opacity, supported blend modes and raster masks use the shared document compositor. The base's blend applies to the complete clipping unit; hiding the base hides its followers.

Saving these stacks writes separate standard raster layers, clipping bytes, mask channels and an explicit enabled `clbl` setting on each base. Native 16-bit source and mask words stay native, including hidden RGB. Raster-layer color effects are baked into each affected standard raster; supplementary PeerBrush sources retain editable originals/effects. A folder with active color effects retains the existing behavior of baking that folder and its children into one standard raster. Every save includes a current merged composite. Reopening without private resources retains clipping relationships wherever the standard layers remain separate.

Documents containing adjustment layers or clipping involving folders still use a standard baked composite layer plus supplementary editable PeerBrush sources. Imported Photoshop clipping involving folders is protected until its Photoshop behavior is supported.

## Pass-through folders

Choose **Pass Through** in a folder's blend menu, or send `layer.update` with `blend:"pass_through"`. Existing/new folders retain their current Normal default. Children blend individually against the backdrop below the folder, including through nested pass-through folders; an isolated folder stops that traversal. Folder opacity and raster masks attenuate the change to the combined backdrop. Human gestures preview the actual shared-engine pixels and commit once. Reservations, undo/redo and native-depth layer clipboard use the same sources.

Adjustment layers inside pass-through folders can change layers below and outside the folder. Effect preparation follows composition order across folder boundaries, including regional stroke previews. Color effects on ordinary children remain editable and native-depth. Enabled effects directly on a pass-through folder, clipping to/from that folder and merging that folder are rejected with an explicit instruction to choose an isolated blend or release clipping. Disabled folder effects can be retained. A containing isolated folder can still be merged normally.

Standard PSD folder keys and section-divider overrides (`lsct`/`lsdk`) recognize `pass`. Raster-only trees save ordinary pass-through dividers, raster layers and masks, plus the current composite. Reopening without PeerBrush resources preserves those layers and their outside blending. PeerBrush adjustment documents continue to save a standard baked composite; supplementary source format 10 restores their editable pass-through topology in compatible readers and protects it in earlier readers. This does not add editable Photoshop adjustment-layer descriptors or layer styles.

## RGB ICC previews and copies

Profiled PSDs retain the saved composite and exact original ICC resource at 8 or 16 bits. Supported ICC v2/v4 RGB matrix/TRC profiles with XYZ connection space, classic `lut16` RGB-to-XYZ/Lab tables and `lut8` RGB-to-Lab tables display through an sRGB conversion, including loading feedback, isolated previews, thumbnails and agent observations. Common sRGB, Adobe RGB, Display P3 and ProPhoto RGB matrix profiles are covered. Conversion uses the relative colorimetric `A2B1` table when available, otherwise the perceptual `A2B0` table, without black-point compensation. Matrix profiles use relative colorimetric intent. Out-of-gamut channels clip to the sRGB range. LUT conversion interpolates at native input/output depth; it does not promise a lossless color-space round trip. The screen is treated as sRGB; monitor calibration and HDR display mapping are pending.

Profiled Photoshop documents remain **READ ONLY**. **Convert to sRGB copy** explicitly flattens their saved appearance into an editable project at the original bit depth. Native16 conversion operates on all 65,536 channel values before display projection; alpha and the source file stay intact. Save requires a new filename. Converted PSD and PNG copies carry a standard sRGB ICC tag; standard PSD raster channels and the merged composite contain the converted result. PeerBrush recognizes its exact built-in working profile on reopen without converting twice. Other imported profiles continue through the protected path.

Agents call `document` with `action:"compatible_copy", convert_to_srgb:true` and the observed revision. Omitting explicit conversion is refused for a profiled source. `document.color_profile` reports source/preview spaces and conversion availability without returning raw profile bytes. The ordinary protected-document copy remains available for unprofiled sources. Reservations and source revision/identity checks are rechecked before replacing the workspace.

Exporting a protected original to PNG retains its original native samples and ICC bytes. Invalid, `mAB`/`mBA`, multi-process, non-RGB, device-link, abstract, named-color and HDR/CICP-transfer profiles remain protected with explicit unmanaged-preview reasons; their editable conversion is unavailable. `lut8` XYZ is protected because ICC does not define its PCS encoding. Duplicate PSD profiles and profiles above 4 MiB are rejected. Parsing checks tag bounds, duplicate/required tags, singular matrix colorants and table dimensions before allocating LUTs. Classic LUTs require three input/output channels, 2–33 grid points per axis and 2–4096 input/output curve entries. Lab normalization follows the table's encoding, including legacy `lut16` Lab in ICC v4. Four disposable transform caches stay outside history and recovery. Encodings follow the [ICC specification](https://www.color.org/specification/ICC.1-2022-05.pdf).

Raster image import normalizes these supported RGB profiles to sRGB at the original channel depth, preserving alpha. Profile assignment, editable profiled layer stacks, newer LUT structures, soft proofing, CMYK, monitor profiles and HDR remain future work.

## Standard layer locks

Photoshop's full protection tag (`lspf=7`, transparency/composite/position locked) imports as PeerBrush's layer lock at either depth, including folder locks that protect descendants. Unlock the layer explicitly before editing. Human, UI, MCP and CLI commands use the same engine guard and undo restores the lock. Saves emit the standard protection tag and transparency-protection flag; reopening without supplementary PeerBrush resources retains the lock and native raster channels. Partial and unknown protection combinations remain read-only because their more selective behavior is not implemented.

## Protected settings

Unsupported documents retain their saved composite at the original channel depth and cannot be edited or saved over their source. The native **READ ONLY** label exposes warnings on hover; agents receive the same reasons in `document.warnings`. **Edit 16-bit copy** / `compatible_copy` explicitly flattens unsupported structure into a new editable project at the original precision and requires a new save path.

The codec now checks:

- Disabled **Blend Clipped Layers As Group** (`clbl`), knockout (`knko`) and non-default fill opacity (`iOpa`).
- Non-default **Blend If** source/destination ranges. Empty or unsplit default black/white ranges are accepted.
- Partial/unknown Photoshop protection flags (`lspf` and standalone transparency protection), invalid clipping/flag payloads, duplicate tags and malformed folder boundaries.
- Section-divider blend overrides (`lsct`/`lsdk`), including supported pass-through folders when the ordinary layer-record blend says Normal. Unknown divider modes and animation scene groups remain protected; `pass` on a raster layer is invalid.
- Advanced mask payloads and flags, and layers whose raster pixels are explicitly irrelevant to their appearance.

Valid no-op raster flags remain readable. Interior-effect/vector flags only pass for ordinary raster sources; the corresponding unsupported effect/vector tags still protect the document. Embedded ICC profiles stay protected until explicitly converted, even when otherwise valid private PeerBrush sources are present and standard layer/composite hashes match.

The format checks follow Adobe's [PSD specification](https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/); grouped clipping follows Adobe's [clipping-mask documentation](https://helpx.adobe.com/photoshop/using/revealing-layers-clipping-masks.html), and outside-group blending follows Adobe's [group blend documentation](https://helpx.adobe.com/uk/photoshop/using/layer-opacity-blending.html). The implementation covers the subset above, not full Photoshop compatibility.

## Verification

Independent handwritten 8/16-bit PSD fixtures cover clipped blend stacks, masks, hidden/transparent bases, folder boundaries, pass-through divider overrides, standard save/reopen without private resources, effect baking, engine reservations/history and unsupported feature protection. Nested-folder tests cover translucent backdrop interpolation, adjustment dependencies across pass-through/isolated boundaries, scattered hierarchy storage, regional previews, clipboard, atomic rejection and undo/redo. All 354 engine, codec, HTTP/stdio protocol and UI regression tests pass; seven opt-in device/clipboard/benchmark checks were not run.

The native Windows workspace displayed a five-layer native16 clipping scene and rendered an opacity gesture on its clipped stripes. One keyboard undo restored PNG16 byte-for-byte. An independent decoder confirmed all standard raster/mask channel planes, layer bounds and clipping flags exactly against the original fixture, and every merged PSD16 composite word against the exported PNG16. Reopening after removing private resources restored the same tree and exact PNG16 output. A knockout fixture opened read-only at 16 bits with the specific `knko` explanation. Original fixture files were retained unchanged; generated QA files are ignored development data.

The pass-through slice also loaded a native16 standard fixture through the running app's shared protocol. Independent decoding confirmed every standard raster and group/child mask word, bounds and clipping flags, supplementary format 10, and a current merged composite matching every PNG16 word. Removing private resources retained the five-layer tree and byte-identical PNG16 output. A shared-engine Normal blend edit changed the rendered image; one protocol undo restored the baseline PNG16 exactly. Both rendered exports were visually inspected. Native control/gesture verification remains pending for this slice: the Windows capture tool returned unrelated foreground images for the PeerBrush window and activation failed, including after a fresh launch and capture-session reset. No interaction with the unrelated apps was attempted.


The RGB ICC checkpoint also passed six dedicated tests with independently assembled standard ICC/PSD records and an analytic sRGB transfer reference. Native app-rendered frames verified the protected managed appearance, 16-bit editable copy and pass-through folder controls. Independent decoding matched every protected-original PNG16 sample and ICC byte, every converted standard raster/composite word, the sRGB tag across PSD/PNG and exact metadata-free reopening. A shared pass-through blend edit and one undo restored PNG16 exactly. Desktop click/drag verification remains unverified after the Computer Use approval timeout; app-rendered screenshots verify the native result without making that interaction claim.

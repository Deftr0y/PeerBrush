# Captured native Photoshop channel rows

`psd::native_rows36::Rows36` supplies `scene_save36::CapturedRows` and
`standard36::NativeRows` from the actual immutable scene. Its lifetime retains
that exact scene; the existing save operation must also retain and recheck its
authenticated source/session, revision, cancellation and destination guards.
This provider does not create a destination or authorize publication.

Each request validates the scene provenance, native precision, channel, complete
record frame, row range and exact output size. One request contains at most 256
rows and 128 KiPixels. Source regions are pinned through fallible preparation;
subsequent sampling uses only prepared memory. A source, admission or guard
failure returns an error, never transparent substitute samples. Output remains
provisional until the entire request and its final guard succeed.

Raw paint channels use the current physical source plane, including hidden RGB
and native16 low bits. Retained transform originals are separately preserved
source data and do not replace the current standard layer channels. Fill
channels use their procedural colour, expanded by 257 at native16, including
colour beneath zero alpha. Layer visibility, opacity, clipping and masks remain
separate standard metadata/channels and do not premultiply raw layer colours.
An original group/adjustment frame uses the canvas and exact zero padding outside
its physical source. Current ordinary paint/fill frames use their placement and
physical dimensions.

The baked current composite and merged compatibility composite use the actual
`production_export::native_rows::Rows` with the same scene, complete topology,
full canvas, unit scale and nearest native samples. Its bounded RGBA scratch is
charged to the scene's actual resident pool. Baked layer channels remain straight
RGBA. Merged RGB uses the legacy integer white-matte expression
`(colour * alpha + maximum * (maximum - alpha) + maximum / 2) / maximum`;
the alpha channel remains unchanged. Channel encoding is native8 or big-endian
native16 for both PSD and PSB.

The current base provider deliberately refuses raw mask requests, including
disabled masks, until the concrete prepared raw-mask path is connected. A
disabled flag must never turn editable mask samples into unit values. Current
effectful/procedural-source layers and editable-source composites also require
their concrete regional provider. No generic callback can fabricate their
authority, and no full resident or frozen effects implementation is substituted.
The existing composite provider independently refuses unsupported masks, effects,
filters or topology. These refusals keep feature activation closed; they do not
complete the full styled/masked save path.

`OriginalLayer` explicitly means current raw physical source channels without
effect rendering or adjustment replacement. This agrees with ordinary unstyled
legacy raw-channel emission. Legacy hidden-source saves can still select prepared
per-layer effect colours by layer ID; this implementation does not claim parity
with that effectful legacy branch. Its source-envelope role remains explicit.

Component coverage checks tiny native8/16 sources, retained/current distinction,
procedural fills, every colour/alpha channel, exact current composite/white matte,
off-canvas placement and source padding, scene/frame mismatches, disabled-mask and
derived-source refusal, cancellation, actual foreign-block failure and resident
admission refusal. Full PSD/PSB saving, prepared masks/effects, installed editor
acceptance and physical large-document qualification remain separate gates.
